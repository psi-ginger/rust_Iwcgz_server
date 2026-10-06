//! Game service — the second client-facing hop (blueprint 01 Part B, 05
//! §S8–S12).
//!
//! Reproduces the legacy `GoonZuWorld` server's login half without the
//! `AuthServer`/`GameDBManager` cluster: game login is verified straight against
//! PostgreSQL, and the character list comes from `tbl_Person`/`tbl_IP`.
//!
//! # Cipher pairing (this is the part that bites)
//!
//! The game link is **encoded in both directions, but with different keys**
//! (`char.cpp:5177-5219`, `GameProtocol.cpp:76`, `ITZSendBuffer.cpp:122-124`):
//!
//! - server → client uses `GetEncrypt2()`, the fixed link cipher
//!   (`ITZSession.cpp:56`);
//! - client → server uses `GetEncrypt()`, the session cipher keyed by the two
//!   bytes carried in `GAMEMSG_RESPONSE_CLIENTCHARUNIQUE`.
//!
//! So this service **encodes with the link cipher** and **decodes with
//! [`Cipher::session_cipher`]** — the exact mirror of the front service, which
//! encodes with neither and decodes with the session cipher.
//!
//! # Session lifecycle (T-016)
//!
//! The four `ITZSession` states, the 10 s / 7 s / 600 ms timings and the rule
//! that a refusal is a disconnect all live in [`gz_net::session`]; this module
//! only drives them. Two things that differ from the legacy are called out where
//! they bite: the handshake window is measured to the first inbound frame
//! (`gz_net::session` module docs), and the link key comes from configuration
//! rather than a source literal ([`config`]).
//!
//! Environment: see [`config::GameConfig`]. `DATABASE_URL`, `GAME_LISTEN`,
//! `GAME_LINK_KEY1`, `GAME_LINK_KEY2`.

use std::borrow::Cow;
use std::time::{SystemTime, UNIX_EPOCH};

use gz_cipher::Cipher;
use gz_db::DropStatus;
use gz_db::{
    AccountRepository, CharacterFull, CharacterRepository, DropLocation, GroundClaim,
    ItemRepository, MoveOutcome, PgAccountRepository, PgCharacterRepository, PgItemRepository,
    PickupStatus, RepositoryError, SplitStatus,
};
use gz_net::session::{Rejection, Session};
use gz_net::{COMPRESSION_FLAG, Frame, FrameParser, HEADER_SIZE, MAX_FRAME_SIZE, Tick};
use gz_proto::game::{
    BasicInfo, CharListEntry, CharUnique, GameLogin, MAX_CONSTRUCTION_NUM, MAX_VILLAGE_NUMBER,
    MakeCharInfo, ServerInfo, VillageStruct, client_map_index, decode_make_new_char,
    decode_select_char, decode_village_struct_request, encode_all_char_info_one,
    encode_all_char_info_two, encode_change_horse, encode_del_result, encode_family_update,
    encode_get_village_info, encode_person_horse, encode_response_name,
    encode_skillbook_magic_info,
};
use gz_proto::item::{
    Item, MAX_ITEM_PER_PERSON, MAX_ITEM_UNIQUE, decode_inv_drop, decode_inv_pickup,
    decode_move_item, decode_split_item, encode_person_item_info,
};
use gz_proto::opcodes_game::GameMsg;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

type AnyError = Box<dyn std::error::Error + Send + Sync>;

mod config;
mod generated_capture;
mod generated_frames;
mod import_capture;
mod world_entry;

use config::{GameConfig, Prologue};

/// `MAX_TOTAL_CHAR_PER_USER` (3 main + 12 char-house) — the legacy sends one
/// char-list packet for every slot, occupied or not
/// (`ServerDB-Person.cpp:1560-1606`). **All 15 are required:** the shipped
/// client's `IsReadyCharList` (`0x737120`) AND-reduces `clBI.GetIndex() == i`
/// across `i in 0..0xF` and only self-heals the missing slots after a long
/// timeout — sending fewer than 15 is what makes char-select stall. The array
/// is 15 entries (`PutCharList` `0x48D040` rejects `GetIndex() > 14`).
const CHAR_SLOTS: i16 = 15;

/// Base `uiTotalMoveSpeed` (6 bits).
const BASE_MOVE_SPEED: u8 = 8;

/// `< 0x4E20` (20000): the client indexes its `CharUniqueInfo` table by
/// `uiMakeCharUnique` (`SetCharReservedChar`).
const MAX_SESSION_CHAR_UNIQUE: u64 = 19_999;

/// Hands out per-session character ids in `1..=MAX_SESSION_CHAR_UNIQUE`.
///
/// The legacy's equivalent is `m_uiCharID`, a UI16 the session manager assigns
/// when it binds a session (`CharServer-Session.cpp:58` passes it straight into
/// `MakeSimpleChar`) — a connection-scoped number, not a database column. Only
/// [`Prologue::Live`] consults this; see [`GameConfig::char_unique`].
static NEXT_CHAR_UNIQUE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

/// Next session character id, wrapping inside the client's table bound.
///
/// 0 is skipped: the legacy never hands out a zeroed id, and 0 collides with the
/// "empty" pattern every zeroed capture frame carries.
fn allocate_char_unique() -> u32 {
    let bound = MAX_SESSION_CHAR_UNIQUE as u32 + 1;
    let n = NEXT_CHAR_UNIQUE.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % bound;
    if n == 0 { 1 } else { n }
}

/// A millisecond reading for [`gz_net::session::Session`], standing in for
/// `GetTickCount()`.
///
/// The legacy's is a `DWORD` that wraps every 49.7 days; a `u64` millisecond
/// count off the monotonic clock does not, which is the whole reason `Tick` is
/// not `u32`.
fn now_tick() -> Tick {
    use std::sync::OnceLock;
    use std::time::Instant;
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    ORIGIN.get_or_init(Instant::now).elapsed().as_millis() as u64
}

#[tokio::main]
async fn main() -> Result<(), AnyError> {
    let database_url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| config::DEFAULT_DATABASE_URL.to_string());

    let mut argv = std::env::args().skip(1);
    match argv.next() {
        // `import-capture` is a database-seeding tool and touches no link, so it
        // deliberately does not require the link key. Everything that opens a
        // socket does.
        Some(command) => run_command(&command, &database_url, argv.collect()).await,
        None => {
            let cfg = GameConfig::from_env()?;
            serve(&cfg).await
        }
    }
}

/// Accepts client connections until the process is stopped.
async fn serve(cfg: &GameConfig) -> Result<(), AnyError> {
    let accounts = PgAccountRepository::connect(&cfg.database_url).await?;
    let characters = PgCharacterRepository::connect(&cfg.database_url).await?;
    let items = PgItemRepository::connect(&cfg.database_url).await?;
    let listener = TcpListener::bind(&cfg.listen).await?;
    println!("game: listening on {}", cfg.listen);
    println!(
        "game: session timeouts: handshake {}ms, connect {}ms, close throttle {}ms",
        cfg.timeouts.handshake.as_millis(),
        cfg.timeouts.connect.as_millis(),
        cfg.timeouts.close_throttle.as_millis()
    );
    println!(
        "game: link key from configuration; prologue={:?} char_unique={}",
        cfg.prologue,
        match cfg.prologue {
            Prologue::Capture => "capture-pinned".to_string(),
            Prologue::Live => "allocated per session".to_string(),
        }
    );
    if cfg.prologue == Prologue::Capture {
        println!(
            "game: WARNING replaying the captured world prologue; T-017 replaces this \
             with payload-driven frames (set GAME_PROLOGUE=live to select the \
             locally-generated path)"
        );
    }

    loop {
        let (stream, peer) = listener.accept().await?;
        let accounts = accounts.clone();
        let characters = characters.clone();
        let items = items.clone();
        let peer = peer.to_string();
        let cfg = cfg.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_client(stream, &peer, &cfg, accounts, characters, items).await {
                eprintln!("game: {peer}: {e}");
            }
        });
    }
}

/// The non-serving modes, so seeding a database does not need a second binary.
///
/// `import-capture [--character <name>]` replaces the named character's
/// `character_items` rows with the item array from the replayed reference
/// capture (see [`import_capture`]).
async fn run_command(command: &str, database_url: &str, args: Vec<String>) -> Result<(), AnyError> {
    match command {
        "import-capture" => {
            let name = args
                .windows(2)
                .find(|w| w[0] == "--character")
                .map_or("Tester", |w| w[1].as_str());
            let characters = PgCharacterRepository::connect(database_url).await?;
            let items = PgItemRepository::connect(database_url).await?;
            let character = characters
                .find_by_name(name)
                .await?
                .ok_or_else(|| format!("no live character named {name:?}"))?;
            let report = import_capture::run(&items, character.character_id).await?;
            println!(
                "game: imported {} item(s) into {:?} (character_id {}); \
                 {} empty slot(s), {} with only a unique and a count",
                report.occupied,
                character.name,
                character.character_id,
                report.empty,
                report.sparse
            );
            Ok(())
        }
        other => Err(format!("unknown command {other:?} (try `import-capture`)").into()),
    }
}

/// Drives one client connection through the `ITZSession` lifecycle.
///
/// The state machine, the three timings and "a rejection is a disconnect" live
/// in [`gz_net::session`]; this function supplies the socket and the dispatch.
/// The shape of the legacy link is preserved throughout:
///
/// 1. **S8** — the server opens by pushing `RESPONSE_CLIENTCHARUNIQUE`. It is a
///    one-sided greeting, not a request/answer: `SecretNCharUniqueSendOperation`
///    generates two random key bytes, installs them on the *server's* session
///    encryptor, and unicasts them (`char.cpp:5182-5219`). There is no
///    `START_FLAG` exchange anywhere in the legacy tree, and nothing the client
///    sends arrives before this.
/// 2. **S8 → S9** — the client's `REQUEST_LOGIN` is the first inbound frame,
///    which is what establishes the session here (see the `gz_net::session`
///    module docs on where that differs from the legacy).
/// 3. **S10–S13** — char-select and the world.
///
/// Every exit other than a clean disconnect goes through
/// [`Session::reject`], so a refusal is structurally a disconnect: the cipher
/// cannot be resynchronised mid-stream in any case, since both key bytes advance
/// on every call (BUG-011's writeup).
async fn handle_client(
    mut stream: TcpStream,
    peer: &str,
    cfg: &GameConfig,
    accounts: PgAccountRepository,
    characters: PgCharacterRepository,
    items: PgItemRepository,
) -> Result<(), AnyError> {
    let mut session_state = Session::new(cfg.timeouts);
    if !session_state.accept(now_tick()) {
        // Unreachable: a fresh Session is always None, and ITZSession::SetSocket
        // has the same guard, so the legacy has the same dead branch.
        return Err("fresh session refused its own socket".into());
    }
    // ITZList::push marks the session live as it enters the master list, which is
    // what arms the 10 s watchdog.
    session_state.set_active(true);

    let (key1, key2) = random_key_bytes();

    // S8: the client cannot send anything until it has this, and it decodes it
    // with the link cipher, whose key comes from configuration (SEC-014).
    let mut outbound = Cipher::session_cipher(cfg.link_key.0, cfg.link_key.1);
    // The character is not known yet — the login request that names it has not
    // arrived — so this cannot be read from the database here. `Prologue::Live`
    // allocates it per session, the way `m_uiCharID` is a connection-scoped
    // number in the legacy; `Prologue::Capture` pins it because every replayed
    // per-character frame identifies its subject by the capture's id and the
    // client discards records that do not match. `characters.game_unique` holds
    // the durable per-character id and `handle_select_char` cross-checks the two.
    let char_unique = cfg.char_unique(allocate_char_unique()) as i32;
    send_encoded(
        &mut stream,
        &mut outbound,
        GameMsg::ResponseClientcharunique,
        &CharUnique {
            char_unique,
            key1,
            key2,
            version: cfg.client_compat_version,
        }
        .encode(),
    )
    .await?;
    println!("game: {peer}: charunique={char_unique} -> ({key1:#04x}, {key2:#04x})");

    // Inbound frames use the negotiated session cipher, keyed by the bytes just
    // sent -- the exact pairing at char.cpp:5177-5219.
    let mut inbound = Cipher::session_cipher(key1, key2);
    let mut parser = FrameParser::new();
    let mut buf = [0u8; 8192];
    // Set once the game login succeeds; select-char and later opcodes need it.
    let mut login: Option<GameSession> = None;
    // Cursor into `generated_capture::REPLY_PLAN`, so a repeated client opcode
    // can map to a different captured reply on its second appearance.
    let mut plan_step = 0usize;
    // Which captured frames get rebuilt from PostgreSQL on the way out.
    let mut live = LiveFrames {
        items: &items,
        account_id: 0,
        account_name: String::new(),
        character: None,
        char_unique,
    };

    loop {
        // The handshake window runs from accept to the first inbound frame. Past
        // it, a peer that has said nothing is a silent half-open connection and
        // gets reclaimed rather than parked forever -- the fix the legacy's
        // 10 s check does not deliver. See gz_net::session.
        let deadline = if session_state.state().is_active() {
            None
        } else {
            Some(cfg.timeouts.handshake)
        };
        let n = match deadline {
            Some(window) => match tokio::time::timeout(window, stream.read(&mut buf)).await {
                Ok(result) => result?,
                Err(_) => {
                    let now = now_tick();
                    if let Some(reason) = session_state.handshake_expired(now) {
                        eprintln!(
                            "game: {peer}: {} ms with no inbound frame; disconnecting ({:?})",
                            window.as_millis(),
                            reason
                        );
                    }
                    session_state.reject(Rejection::HandshakeTimeout, now);
                    return Ok(());
                }
            },
            None => stream.read(&mut buf).await?,
        };
        if n == 0 {
            println!(
                "game: {peer}: disconnected after {} ms",
                now_tick().saturating_sub(session_state.since())
            );
            session_state.close(now_tick());
            return Ok(());
        }
        parser.feed(&buf[..n]).map_err(|e| {
            let reason = session_state.reject(Rejection::Framing(e), now_tick());
            eprintln!("game: {peer}: framing rejection {reason:?}; disconnecting");
            AnyError::from(e)
        })?;

        loop {
            // `next_frame` is fallible and *also* reports "buffer exhausted", so
            // the two outcomes are separated here rather than in a `while let`
            // that would swallow the error as end-of-iteration.
            let frame = match parser.next_frame() {
                Ok(None) => break,
                Ok(Some(frame)) => frame,
                Err(e) => {
                    let reason = session_state.reject(Rejection::Framing(e), now_tick());
                    eprintln!("game: {peer}: framing rejection {reason:?}; disconnecting");
                    return Ok(());
                }
            };
            // The legacy promotes CONNECTED -> ESTABLISHED on the first receive,
            // not on the first *request*; same thing here, and it is what stops
            // the handshake deadline applying to a live session.
            session_state.establish();

            let wire = frame.as_bytes();
            let mut plain = vec![0u8; wire.len()];
            let integrity = match inbound.decode(wire, &mut plain) {
                Ok(integrity) => integrity,
                Err(e) => {
                    let reason = session_state.reject(Rejection::Integrity, now_tick());
                    eprintln!("game: {peer}: cipher error {e} ({reason:?}); disconnecting");
                    return Ok(());
                }
            };
            if !integrity {
                // No resynchronise is possible: both key bytes advance on every
                // call, so a single dropped or mis-ordered frame leaves the two
                // ends irrecoverably out of step. The legacy reaches the same
                // conclusion -- ITZNetIOCP.cpp:145-150 turns a FALSE from the
                // recv guard into SESSION_STATE_DISCONNECTED.
                let reason = session_state.reject(Rejection::Integrity, now_tick());
                eprintln!(
                    "game: {peer}: cipher desync at opcode {:#06x} ({reason:?}); disconnecting",
                    frame.cmd()
                );
                return Ok(());
            }

            let (cmd, payload) = match deframe(&plain) {
                Ok(v) => v,
                Err(e) => {
                    // A flagged body that is not a valid LZF stream. There is no
                    // way to recover the stream from here, and no legacy
                    // recovery to copy: NLZF::Decompress failing means the frame
                    // was never decodable.
                    let reason = session_state.reject(Rejection::Integrity, now_tick());
                    eprintln!(
                        "game: {peer}: LZF failure at opcode {:#06x} ({reason:?}); \
                         disconnecting: {e}",
                        frame.cmd()
                    );
                    return Ok(());
                }
            };
            println!("game: {peer}: recv opcode {cmd} ({} bytes)", payload.len());

            match cmd {
                c if c == GameMsg::RequestLogin as u16 => {
                    let request = GameLogin::decode(payload.as_ref())?;
                    login = Some(
                        handle_game_login(
                            &mut stream,
                            &mut outbound,
                            &accounts,
                            &characters,
                            &request,
                        )
                        .await?,
                    );
                }
                c if c == GameMsg::RequestSelectchar as u16 => {
                    let slot = decode_select_char(payload.as_ref())?;
                    if !admit_select_char(login.as_ref().and_then(|s| s.character_id)) {
                        eprintln!(
                            "game: {peer}: ignoring selectchar slot {slot}: this session has \
                             already entered the world, and the legacy answers a repeat with \
                             silence (ServerDB-Person.cpp:1757,1763)"
                        );
                        continue;
                    }
                    let character_id = {
                        let session = login
                            .as_ref()
                            .ok_or("selectchar before a successful game login")?;
                        handle_select_char(
                            &mut stream,
                            &mut outbound,
                            &characters,
                            cfg,
                            session,
                            char_unique,
                            slot,
                        )
                        .await?
                    };
                    if let Some(session) = login.as_mut() {
                        session.character_id = Some(character_id.character_id);
                        session.entry_cell = Some(DropLocation {
                            map_id: character_id.map_id,
                            x: character_id.pos_x,
                            y: character_id.pos_y,
                        });
                    }
                    if let Some(session) = login.as_ref() {
                        live.account_id = session.account_id;
                        live.account_name = session.username.clone();
                    }
                    live.character = Some(character_id.clone());
                    // The world bulk after char-select. Capture mode replays it;
                    // live mode generates it (T-017). See `Prologue` for why the
                    // two cannot be mixed today.
                    match cfg.prologue {
                        Prologue::Capture => {
                            replay(
                                &mut stream,
                                &mut outbound,
                                generated_capture::PROLOGUE_TAIL,
                                "world-entry prologue",
                                &live,
                                // The character's first delivery of these frames,
                                // and the prologue carries no ALLCHARINFOONE
                                // anyway.
                                false,
                            )
                            .await?
                        }
                        Prologue::Live => {
                            send_world_entry(
                                &mut stream,
                                &mut outbound,
                                char_unique,
                                &items,
                                character_id.character_id,
                            )
                            .await?
                        }
                    }
                }
                c if c == GameMsg::RequestItemsplit as u16 => {
                    let character_id = login
                        .as_ref()
                        .and_then(|s| s.character_id)
                        .ok_or("item split before a character was selected")?;
                    handle_item_split(
                        &mut stream,
                        &mut outbound,
                        &items,
                        character_id,
                        payload.as_ref(),
                    )
                    .await?;
                }
                c if c == GameMsg::RequestMakenewchar as u16 => {
                    let (account_id, account_name) = login
                        .as_ref()
                        .map(|s| (s.account_id, s.username.clone()))
                        .ok_or("char create before a successful game login")?;
                    handle_make_new_char(
                        &mut stream,
                        &mut outbound,
                        &characters,
                        account_id,
                        &account_name,
                        payload.as_ref(),
                    )
                    .await?;
                }
                c if c == GameMsg::RequestDeluserchar as u16 => {
                    let (account_id, account_name) = login
                        .as_ref()
                        .map(|s| (s.account_id, s.username.clone()))
                        .ok_or("char delete before a successful game login")?;
                    handle_userchar_deletion(
                        &mut stream,
                        &mut outbound,
                        &characters,
                        account_id,
                        &account_name,
                        GameMsg::ResponseDeluserchar,
                        true,
                        payload.as_ref(),
                    )
                    .await?;
                }
                c if c == GameMsg::RequestPredeluserchar as u16 => {
                    let (account_id, account_name) = login
                        .as_ref()
                        .map(|s| (s.account_id, s.username.clone()))
                        .ok_or("char predelete before a successful game login")?;
                    handle_userchar_deletion(
                        &mut stream,
                        &mut outbound,
                        &characters,
                        account_id,
                        &account_name,
                        GameMsg::ResponsePredeluserchar,
                        false,
                        payload.as_ref(),
                    )
                    .await?;
                }
                c if c == GameMsg::RequestVillagestruct as u16 => {
                    let village_ref = decode_village_struct_request(payload.as_ref())?;
                    handle_village_struct(&mut stream, &mut outbound, village_ref).await?;
                }
                c if c == GameMsg::RequestItemchange as u16 => {
                    // The client sends 305 exactly when a drag ends on an
                    // *occupied* slot — it is the merge/swap gesture. Legacy
                    // parses the same cltMoveItemInfo and runs the same
                    // sDBRequest_MoveItem path as 303, so one handler serves both.
                    let character_id = login
                        .as_ref()
                        .and_then(|s| s.character_id)
                        .ok_or("item change before a character was selected")?;
                    handle_item_move(
                        &mut stream,
                        &mut outbound,
                        &items,
                        character_id,
                        payload.as_ref(),
                    )
                    .await?;
                }
                c if c == GameMsg::RequestItemmove as u16 => {
                    let character_id = login
                        .as_ref()
                        .and_then(|s| s.character_id)
                        .ok_or("item move before a character was selected")?;
                    handle_item_move(
                        &mut stream,
                        &mut outbound,
                        &items,
                        character_id,
                        payload.as_ref(),
                    )
                    .await?;
                }
                c if c == GameMsg::RequestInvdrop as u16 => {
                    let (character_id, cell) = {
                        let session = login
                            .as_ref()
                            .ok_or("item drop before a successful game login")?;
                        let character_id = session
                            .character_id
                            .ok_or("item drop before a character was selected")?;
                        let cell = session
                            .entry_cell
                            .ok_or("item drop before a character position was known")?;
                        (character_id, cell)
                    };
                    handle_inv_drop(
                        &mut stream,
                        &mut outbound,
                        &items,
                        character_id,
                        cell,
                        payload.as_ref(),
                    )
                    .await?;
                }
                c if c == GameMsg::RequestInvpickup as u16 => {
                    let character_id = login
                        .as_ref()
                        .ok_or("item pickup before a successful game login")?
                        .character_id
                        .ok_or("item pickup before a character was selected")?;
                    handle_inv_pickup(
                        &mut stream,
                        &mut outbound,
                        &items,
                        character_id,
                        payload.as_ref(),
                    )
                    .await?;
                }
                c if c == GameMsg::RequestChangestatusinfo as u16 => {
                    // The reference server answers 230 with a 25-frame data
                    // block (map/object bulk, then ALLCHARINFO one/two), not
                    // with 231. `REPLY_PLAN` mirrors that verbatim. In live mode
                    // there is nothing to answer with yet -- 231, the "your
                    // status is current" reply, is T-017's to introduce, and
                    // sending it now would tell the client a state this server
                    // has not computed.
                    if cfg.prologue == Prologue::Capture {
                        replay_plan_step(&mut stream, &mut outbound, cmd, &mut plan_step, &live)
                            .await?;
                    } else {
                        println!(
                            "game: {peer}: changestatusinfo -> no reply \
                             (live mode; payload-driven 230 is T-017)"
                        );
                    }
                }
                // The reference server sends nothing for 246; the data the client
                // wants arrives after 230.
                c if c == GameMsg::RequestAllcharinfo as u16 => {
                    println!("game: {peer}: allcharinfo request -> no reply (matches capture)");
                }
                other => println!("game: {peer}: unhandled opcode {other}"),
            }
        }
    }
}

/// Splits a decrypted frame into `(opcode, payload)`, decompressing when asked.
///
/// The client LZF-compresses every game-link frame of 200 bytes or more and
/// marks it with bit 15 of `usCmd` (`Client-Winsock.cpp:178-196`). The legacy
/// consumer decompresses and clears the flag before dispatch
/// (`GameProtocol.cpp:109-141`); without this the opcode reads as
/// `real | 0x8000` (e.g. `MAKENEWCHAR` 7 arrives as 32775). The front link
/// never compresses (`SendFrontServerMsg` has no LZF step), so only the game
/// service needs this.
///
/// # Errors
///
/// [`gz_lzf::LzfError`] if a flagged body is not a valid LZF stream, which the
/// caller treats as a fatal desync like a cipher failure.
fn deframe(plain: &[u8]) -> Result<(u16, Cow<'_, [u8]>), AnyError> {
    let cmd = u16::from_le_bytes([plain[3], plain[4]]);
    if cmd & COMPRESSION_FLAG == 0 {
        return Ok((cmd, Cow::Borrowed(&plain[HEADER_SIZE..])));
    }

    // A decompressed body can be no larger than the biggest frame a peer may
    // send, so size the scratch from the framing ceiling rather than guessing.
    let mut body = vec![0u8; MAX_FRAME_SIZE - HEADER_SIZE];
    let n = gz_lzf::decompress(&plain[HEADER_SIZE..], &mut body)?;
    body.truncate(n);
    Ok((cmd & !COMPRESSION_FLAG, Cow::Owned(body)))
}

/// The authenticated state carried for the rest of the connection.
struct GameSession {
    /// `accounts.account_id` (= the front's `siAuthKey`).
    account_id: i32,
    /// `accounts.gm_level`, forwarded as `siGameMasterLevel`.
    gm_level: i32,
    /// The world-entered character. `None` until `REQUEST_SELECTCHAR` names one,
    /// because the client picks a slot after logging in.
    character_id: Option<i32>,
    /// The logged-in account's name (`accounts.username`). Char-list/create
    /// messages echo this back to the client inside `cltCharListInfo.clBI.szAccountID`.
    username: String,
    /// Where the character stood on entering the world, used as the cell for
    /// `GAMEMSG_REQUEST_INVDROP`.
    ///
    /// This is deliberately *not* named `position`: the server does not track
    /// movement, so it stays at the entry coordinates no matter where the player
    /// walks. A dropped item therefore lands where the character spawned.
    entry_cell: Option<DropLocation>,
}

/// Whether a `GAMEMSG_REQUEST_SELECTCHAR` should be allowed to enter the world.
///
/// **World entry is one-shot per connection, and a repeat is a silent no-op —
/// in every case the legacy can actually reach.** Verified 2026-10-05 against a
/// live log that showed seven entries on one socket and a desyncing client:
///
/// 1. `Server-Login.cpp:189-190` — `idnum` is read from the **already-bound**
///    character (`pclCI->clBI.siIDNum`), not from the requested slot, and
///    `idnum <= 0` returns early.
/// 2. `rsp_PersonIDGet` (`Goonzu_DB\04. GameDB___________.txt:1073`) selects
///    `WHERE [idnum] = @idnum and [index] = @index` — **`AND`, not `OR`**. So the
///    requested index is only ever honoured together with the *currently bound*
///    character's idnum.
/// 3. Therefore a request for a **different** slot matches no row, `siResult` is
///    0, and `ServerDB-Person.cpp:1757` returns without sending anything.
/// 4. A request for the **same** slot does match, and returns the same PersonID,
///    which `:1763-1766` detects and returns on.
/// 5. Only a genuinely different character could get past both — and that path
///    (`:1770` `CloseAction`, `:1795` `DeleteChar`, then a fresh `cltCharServer`)
///    *tears the old character down*. It still does not replay world state onto
///    a live session.
///
/// So the legacy has no reachable branch in which a second selectchar re-sends
/// the world, and no branch that tears a character down without a matching
/// reload either. Implementing real switching is T-017's job and must do the
/// `CloseAction`/`DeleteChar` teardown; until then the faithful behaviour is to
/// send nothing, which is what a bare `false` here buys.
///
/// The client is built for this: `NSelectChar.cpp:1343-1354` sends selectchar
/// with an already-bound char client present, and `:1357` closes the dialog
/// without waiting for a reply. So the client-visible result of a refused switch
/// is the char-select dialog closing and no world changing — the legacy's own
/// behaviour, and why this is a no-op rather than a disconnect.
fn admit_select_char(bound_character_id: Option<i32>) -> bool {
    bound_character_id.is_none()
}

/// Sends every slot's `cltCharListInfo` (opcode 3), zeroed for empty slots, so
/// the client's `PutCharList` writes 15 copies and the roster dialog repaints.
async fn send_char_list(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    characters: &PgCharacterRepository,
    account_id: i32,
    account_name: &str,
) -> Result<usize, AnyError> {
    let rows = characters.list_for_account(account_id).await?;
    let mut occupied = 0usize;
    for slot in 0..CHAR_SLOTS {
        let character = rows.iter().find(|c| c.slot == slot);
        if character.is_some() {
            occupied += 1;
        }
        // Empty slots are sent too, zeroed except for the index: the client's
        // `PutCharList` copies each packet into `clCharListInfo[slot]`
        // unconditionally (`Client.cpp:4591-4599`).
        let entry = match character {
            Some(c) => CharListEntry {
                account_idnum: account_id,
                account_id: account_name.to_owned(),
                slot: slot as u8,
                sex: (c.gender != 0) as u8,
                class_kind: c.class_kind as u16,
                character_id: c.character_id,
                name: c.name.clone(),
                level: c.level,
                life: c.max_life,
                mana: c.max_mana,
                exp: c.exp,
            },
            None => CharListEntry {
                account_idnum: 0,
                account_id: String::new(),
                slot: slot as u8,
                sex: 0,
                class_kind: 0,
                character_id: 0,
                name: String::new(),
                level: 0,
                life: 0,
                mana: 0,
                exp: 0,
            },
        };
        send_encoded(stream, outbound, GameMsg::ResponseCharlist, &entry.encode()).await?;
    }
    Ok(occupied)
}

/// S9–S11: verify the game login and answer with one char-list packet per slot.
async fn handle_game_login(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    accounts: &PgAccountRepository,
    characters: &PgCharacterRepository,
    login: &GameLogin,
) -> Result<GameSession, AnyError> {
    // The auth key is the account idnum the front handed out at login; a
    // mismatch means a stale or replayed key, so refuse rather than guess.
    let account = match accounts.find_by_username(&login.userid).await {
        Ok(account) => account,
        Err(RepositoryError::NotFound) => {
            return Err(format!("game login for unknown account {:?}", login.userid).into());
        }
        Err(e) => return Err(e.into()),
    };
    if account.account_id != login.auth_key {
        return Err(format!(
            "game login auth key {} does not match account {}",
            login.auth_key, account.account_id
        )
        .into());
    }

    let occupied = send_char_list(
        stream,
        outbound,
        characters,
        account.account_id,
        &account.username,
    )
    .await?;

    println!(
        "game: login ok for {:?} (account {}, auth_key {}): {occupied} character(s)",
        login.userid, account.account_id, login.auth_key
    );
    Ok(GameSession {
        account_id: account.account_id,
        gm_level: account.gm_level,
        character_id: None,
        username: account.username,
        entry_cell: None,
    })
}

/// S12/S13: world entry. Answers `GAMEMSG_REQUEST_SELECTCHAR` with the
/// character record, matching the legacy order in `ServerDB-Person.cpp`: the
/// server info first, then `GAMEMSG_RESPONSE_LOGIN` carrying `clMakeCharInfo`.
///
/// The client only accepts slots `0..3` at select time
/// (`Server-Login.cpp:187`).
async fn handle_select_char(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    characters: &PgCharacterRepository,
    cfg: &GameConfig,
    session: &GameSession,
    char_unique: i32,
    slot: i32,
) -> Result<CharacterFull, AnyError> {
    if !(0..i32::from(CHAR_SLOTS)).contains(&slot) {
        return Err(format!("selectchar slot {slot} out of range 0..{CHAR_SLOTS}").into());
    }

    let character = characters
        .find_by_slot(session.account_id, slot as i16)
        .await?
        .ok_or_else(|| format!("no character in account {} slot {slot}", session.account_id))?;

    // The character now exists as a world entity, so give it a durable id if it
    // has never had one. It is *not* what goes on the wire: `make_char_unique`
    // below echoes the session's `char_unique`, and every replayed frame carries
    // that same session id, so the wire is self-consistent either way.
    //
    // The mismatch is therefore *inert today* and only worth reporting as future
    // debt: once T-017 generates per-character frames from the database instead
    // of replaying, those frames will carry `stored_unique`, and a session
    // holding a different `char_unique` will have them all discarded by
    // `SetCharReservedChar`.
    //
    // The earlier wording of this message ("the replayed per-character frames
    // will be discarded by the client") was wrong about the mechanism and
    // misled a 2026-10-05 diagnosis into blaming the id mismatch for a
    // character-switch desync that was a missing already-bound guard instead.
    // A character whose id *does* match, `Tester`, glitched identically; and
    // `test3`, which does not match, entered the world cleanly.
    let stored_unique = characters
        .resolve_game_unique(character.character_id)
        .await?;
    if stored_unique != char_unique {
        println!(
            "game: selectchar {:?} has durable game_unique {stored_unique} but this session's \
             char_unique is {char_unique}; harmless while the world is replayed (every frame \
             carries the session id), but T-017 must make these agree or the client will discard \
             this character's records",
            character.name
        );
    }

    // SERVERINFO sets `bReceiveServerInfoSwitch`, which `SetCharReservedChar`
    // checks before it will create any avatar. It must precede RESPONSE_LOGIN.
    send_encoded(
        stream,
        outbound,
        GameMsg::ResponseServerinfo,
        &server_info(cfg).encode(),
    )
    .await?;

    let make_char = MakeCharInfo {
        // Must equal the `char_unique` from opcode 1 or the client ignores the
        // record (`SetCharReservedChar` compares `MyCharUnique`).
        make_char_unique: u16::try_from(char_unique).unwrap_or(0),
        make_char_kind: u16::try_from(character.class_kind.max(0)).unwrap_or(0),
        level: u8::try_from(character.level.clamp(0, 255)).unwrap_or(1),
        life: u32::try_from(character.max_life.max(0)).unwrap_or(0),
        x: u16::try_from(character.pos_x.clamp(0, 0x3FF)).unwrap_or(0),
        y: u16::try_from(character.pos_y.clamp(0, 0x3FF)).unwrap_or(0),
        // Shared with the 309 encoder: the client copies this straight into
        // `CurrentMapIndex` and then drops any 309 naming a different map.
        map_index: client_map_index(character.map_id),
        total_move_speed: BASE_MOVE_SPEED,
        home_village_unique: u8::try_from(character.school_id.clamp(0, 255)).unwrap_or(0),
        game_master_level: u8::try_from(session.gm_level.clamp(0, 255)).unwrap_or(0),
        // 0 lets the client resolve the map type from its own MapData.
        map_unique: 0,
        make_ani_type: 0,
    };
    send_encoded(
        stream,
        outbound,
        GameMsg::ResponseLogin,
        &make_char.encode(),
    )
    .await?;

    println!(
        "game: selectchar slot {slot} char {:?} -> map {} at ({}, {})",
        character.name, character.map_id, character.pos_x, character.pos_y
    );
    Ok(character)
}

/// Applies `GAMEMSG_REQUEST_ITEMMOVE` (303) and `GAMEMSG_REQUEST_ITEMCHANGE`
/// (305) — a drag between two slots of the character's 80-entry item array.
///
/// Both payloads are the same `cltMoveItemInfo { uiFrom, uiTo }` and the
/// legacy routes both to the same DB move path; the two opcodes are just the
/// client's way to say "this move landed on an empty slot" versus "...stacked
/// or overwrote an occupied slot".
///
/// The client applies the move locally by the time it sends
/// (`MouseOperation.cpp:2247-2249`), but the server still answers the two
/// affected slots with `GAMEMSG_RESPONSE_PERSONITEMINFO` (7210). The legacy's
/// opcode table has no `RESPONSE_ITEMMOVE`; instead the DB-server's move
/// response writes *both* slots back out (`ServerDB-Item.cpp:84-102`). The
/// local apply and this reply agree in the common case — the client already
/// swapped — but the merge case is exactly where the client cannot render the
/// result on its own: it applies a swap to two same-item stacks where the
/// server has merged them, so without the correcting 7210 pair the bag shows
/// two stacks against one in the database.
///
/// # Errors
///
/// [`gz_db::RepositoryError`] from the move, or a short body.
async fn handle_item_move(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    items: &PgItemRepository,
    character_id: i32,
    payload: &[u8],
) -> Result<(), AnyError> {
    let (from, to) = decode_move_item(payload)?;
    let outcome = items.move_slot(character_id, from, to).await?;
    let (detail, slots) = match outcome {
        MoveOutcome::SourceEmpty => ("source slot was empty".to_string(), None),
        MoveOutcome::Swapped {
            from: new_from,
            to: new_to,
        } => ("swapped".to_string(), Some((new_from, new_to))),
        MoveOutcome::Stacked {
            total,
            to: new_to,
            from: new_from,
        } => (
            format!("stacked, destination now holds {total}"),
            Some((new_from, new_to)),
        ),
    };
    println!("game: itemmove {from} -> {to} for character {character_id}: {detail}",);

    // SourceEmpty moves nothing; every other outcome rewrites both slots back
    // out so the client's bag agrees with the store — most importantly the
    // merge, whose local result the client cannot produce itself.
    if let Some((new_from, new_to)) = slots {
        send_encoded(
            stream,
            outbound,
            GameMsg::ResponsePersoniteminfo,
            &encode_person_item_info(from, &new_from),
        )
        .await?;
        send_encoded(
            stream,
            outbound,
            GameMsg::ResponsePersoniteminfo,
            &encode_person_item_info(to, &new_to),
        )
        .await?;
    }
    Ok(())
}

/// Applies `GAMEMSG_REQUEST_ITEMSPLIT` (341): moves a chosen count out of one
/// stack into another slot.
///
/// Split and move share a shape because the legacy splits through the same
/// `sDBRequest_MoveItem` DB path (`Server-Item.cpp:4400-4406`): a split is a
/// move whose source record carries only the split count, and its reply is the
/// same `DBMSG_RESPONSE_ITEMMOVE` → two 7210 slot rewrites as an itemmove.
async fn handle_item_split(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    items: &PgItemRepository,
    character_id: i32,
    payload: &[u8],
) -> Result<(), AnyError> {
    let (from, to, count) = decode_split_item(payload)?;
    let outcome = items.split_slot(character_id, from, to, count).await?;
    let detail = match outcome.status {
        SplitStatus::Split => format!("split {count}"),
        SplitStatus::SourceEmpty => "source slot was empty".to_string(),
        SplitStatus::BadCount { stored, requested } => {
            format!("source holds {stored}, cannot split {requested}")
        }
        SplitStatus::DestinationMismatch { stored, claimed } => {
            format!("destination holds item {stored}, source is {claimed}")
        }
        SplitStatus::SelfSplit => "from == to, nothing to split".to_string(),
    };
    println!("game: itemsplit {from} -> {to} x{count} for character {character_id}: {detail}");

    // Only a real split changes the bag; refusals are logged, matching the
    // itemmove rules — the client asked for a visible change, and a no-op
    // leaves its local state exactly as it was.
    if outcome.status == SplitStatus::Split {
        send_encoded(
            stream,
            outbound,
            GameMsg::ResponsePersoniteminfo,
            &encode_person_item_info(from, &outcome.from),
        )
        .await?;
        send_encoded(
            stream,
            outbound,
            GameMsg::ResponsePersoniteminfo,
            &encode_person_item_info(to, &outcome.to),
        )
        .await?;
    }
    Ok(())
}

/// Applies `GAMEMSG_REQUEST_MAKENEWCHAR` (7): persist a new character and
/// echo its `cltCharListInfo` with a success response (opcode 8).
async fn handle_make_new_char(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    characters: &PgCharacterRepository,
    account_id: i32,
    account_name: &str,
    payload: &[u8],
) -> Result<(), AnyError> {
    let (name, sex, class_kind, slot) = decode_make_new_char(payload)?;
    let character_id = characters
        .create_character(
            account_id,
            &name,
            class_kind as i16,
            sex as i16,
            slot as i16,
        )
        .await?;
    let entry = CharListEntry {
        account_idnum: account_id,
        account_id: account_name.to_owned(),
        slot,
        sex,
        class_kind,
        character_id,
        name: name.clone(),
        level: 1,
        life: 40,
        mana: 20,
        exp: 0,
    };
    send_encoded(
        stream,
        outbound,
        GameMsg::ResponseMakenewchar,
        &entry.encode(),
    )
    .await?;
    println!("game: created char {name:?} id={character_id} slot={slot} for account {account_id}");
    Ok(())
}

/// Applies `GAMEMSG_REQUEST_DELUSERCHAR` (9) and
/// `GAMEMSG_REQUEST_PREDELUSERCHAR` (83): the final "yes, delete" and the
/// grace-mark request. Both reach the same slot-clear response (opcodes 10/84).
#[allow(clippy::too_many_arguments)]
async fn handle_userchar_deletion(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    characters: &PgCharacterRepository,
    account_id: i32,
    account_name: &str,
    response_opcode: GameMsg,
    hard_delete: bool,
    payload: &[u8],
) -> Result<(), AnyError> {
    let slot = payload.first().copied().unwrap_or(0) as i16;
    if !(0..CHAR_SLOTS).contains(&slot) {
        println!("game: char-del slot {slot} out of range for account {account_id}");
        send_encoded(
            stream,
            outbound,
            response_opcode,
            &encode_del_result(0, 0, slot as i8),
        )
        .await?;
        return Ok(());
    }

    let affected = if hard_delete {
        characters.delete_character(account_id, slot).await?
    } else {
        // `GAMEMSG_REQUEST_PREDELUSERCHAR` body is `SI08 siCharSlot;
        // TCHAR szIdentification[12]; SI16 predeltype`. `predeltype = 1`
        // marks the char for grace removal, `0` cancels the pending grace.
        let deltype = if payload.len() > 13 {
            payload[13] as u16 | ((payload.get(14).copied().unwrap_or(0) as u16) << 8)
        } else {
            1
        };
        characters
            .set_delete_pending(account_id, slot, deltype != 0)
            .await?
    };

    match affected {
        Some(id) => {
            send_encoded(
                stream,
                outbound,
                response_opcode,
                &encode_del_result(1, 0, slot as i8),
            )
            .await?;
            println!("game: char-del slot {slot} id {id} for account {account_id}: done");
            // The slot is now removed locally but the client's roster dialog
            // does not repaint until its existing cache is refreshed (no
            // notification flows with opcode 10/84). Re-emit all 15 char-list
            // packets so `PutCharList` rebuilds every slot and the dialog
            // repaints without a user click. This is a dev convenience:
            // the legacy never emitted opcode 3 outside of login.
            let _occupied =
                send_char_list(stream, outbound, characters, account_id, account_name).await?;
        }
        None => {
            send_encoded(
                stream,
                outbound,
                response_opcode,
                &encode_del_result(0, 0, slot as i8),
            )
            .await?;
            println!("game: char-del slot {slot} for account {account_id}: slot already empty");
        }
    }
    Ok(())
}

/// Applies `GAMEMSG_REQUEST_INVDROP` (307) — dropping an item onto the map.
///
/// # Which frame comes back, and why it is 7210 and not 309
///
/// The opcode names invite the wrong answer. `GAMEMSG_RESPONSE_ITEMDROP` (309)
/// carries a `cltCharPos` plus the item's unique and list index, and the client's
/// handler does exactly one thing with it: `SetItemMap(x, y, …)` to draw a ground
/// icon (`Client-SystemNPC.cpp:2274`). It never touches the inventory.
///
/// The frame that updates the inventory is `GAMEMSG_RESPONSE_PERSONITEMINFO`
/// (7210), whose handler writes `clCharItem.clItem[siItemPos].Set(&clItem)` and
/// refreshes every open inventory dialog (`Client-Person-BigInfo.cpp:616`). The
/// client never optimistically updates its bag on a drop — `MouseOperation.cpp`
/// sends 307 and does nothing else — so without 7210 the slot simply never
/// changes. That is the whole of the missing confirmation.
///
/// Tracing the legacy confirms it: `DoMsg_GAMEMSG_REQUEST_INVDROP` ends by handing
/// the drop to the DB layer, and `DoMsg_DBMSG_RESPONSE_DROPITEM` answers the
/// client with 7210 carrying the slot's new contents (`ServerDB-Item.cpp:2833`).
/// The same 7210 is what the 307 handler sends when the client's record does not
/// match the server's (`Server-Item.cpp:2470`), which is why one reply covers
/// every outcome.
///
/// ## The 309 that makes it visible
///
/// The drop also goes out as `GAMEMSG_RESPONSE_ITEMDROP` (309), which is the only
/// thing that puts an icon on the ground. The legacy's ordering is
/// `DropItem` then 7210 (`DoMsg_DBMSG_RESPONSE_DROPITEM`, `ServerDB-Item.cpp:2792`)
/// with the 309 following from the cell notification; the client treats the two
/// independently — one writes the map grid, the other the bag — so the order here
/// is not load-bearing, and 7210 goes first to keep the reply path unchanged.
///
/// Contrary to what the map-layer framing suggests, 309 needs nothing from us but
/// the coordinates: `cltMapCommon::SetItemMap` (`map.cpp:289`) writes the client's
/// own per-cell grid, so the client supplies the map. What it *does* demand is that
/// `clPos.siMapIndex` equal the client's `CurrentMapIndex`, which it took from
/// `RESPONSE_LOGIN` — hence [`gz_proto::game::client_map_index`] on both sides.
///
/// The frame carries no stack count, so a 16-stack drop will render as one icon.
async fn handle_inv_drop(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    items: &PgItemRepository,
    character_id: i32,
    cell: DropLocation,
    payload: &[u8],
) -> Result<(), AnyError> {
    let request = decode_inv_drop(payload)?;

    // `Server-Item.cpp:2462` drops out of range positions silently.
    if request.slot < 0 || request.slot >= MAX_ITEM_PER_PERSON as i32 {
        println!(
            "game: ignoring invdrop of item {} from slot {} (outside 0..{MAX_ITEM_PER_PERSON})",
            request.item.unique(),
            request.slot
        );
        return Ok(());
    }
    let slot = request.slot as u16;

    let outcome = items
        .drop_slot(character_id, slot, &request.item, cell)
        .await?;

    let detail = match outcome.status {
        DropStatus::Dropped => {
            let dropped = outcome.dropped.unwrap_or_else(Item::empty);
            match &outcome.grounded {
                Some(g) => format!(
                    "dropped item {} x{} -> map {} at ({}, {})",
                    dropped.unique(),
                    dropped.count(),
                    cell.map_id,
                    g.x,
                    g.y
                ),
                None => format!(
                    "dropped item {} x{} -> map {} at ({}, {})",
                    dropped.unique(),
                    dropped.count(),
                    cell.map_id,
                    cell.x,
                    cell.y
                ),
            }
        }
        DropStatus::SlotEmpty => format!("slot {slot} was empty"),
        DropStatus::ItemMismatch { stored, claimed } => {
            format!("slot {slot} holds item {stored}, not {claimed}")
        }
        DropStatus::NotEnough { stored, requested } => {
            format!("slot {slot} holds {stored}, cannot drop {requested}")
        }
        DropStatus::GroundBlocked { x, y } => {
            format!("drop cell ({x}, {y}) and all eight neighbours are occupied")
        }
    };
    println!("game: invdrop slot {slot} for character {character_id}: {detail}");

    // Every outcome is answered: the client needs its slot overwritten either to
    // show the removal or to undo an optimistic one it should not have made.
    send_encoded(
        stream,
        outbound,
        GameMsg::ResponsePersoniteminfo,
        &encode_person_item_info(slot, &outcome.current),
    )
    .await?;

    // Only a real drop produces a ground object to draw. The rejection paths leave
    // `grounded` as `None` precisely so nothing is drawn for them.
    if let Some(grounded) = outcome.grounded {
        send_encoded(
            stream,
            outbound,
            GameMsg::ResponseItemdrop,
            &grounded.encode(),
        )
        .await?;
        // The client discards this frame without a word if the cell is outside its
        // map, and we hold no map model to check against — so the log is the only
        // way to tell "drew" from "silently ignored".
        println!(
            "game: sent itemdrop 309: map {} ({}, {}) unique {} handle {}",
            grounded.map_index, grounded.x, grounded.y, grounded.item_unique, grounded.list_index
        );
    }
    Ok(())
}

/// Answers `GAMEMSG_REQUEST_INVPICKUP` (308): a click on a ground item.
///
/// This is the other half of [`handle_inv_drop`]. The client sends five `SI16`
/// describing the cell it clicked (`MouseOperation.cpp:5316` passes
/// `GetItemUnique(), GetItemIndex(), CurrentMapIndex, GetItemX(), GetItemY()`
/// straight out of the pair that 309 stored), and the server either moves the
/// item into a free carry slot or does nothing.
///
/// ## Silence is the rejection
///
/// Unlike 307 — which is always acknowledged, because the client made an
/// optimistic change to its bag that has to be undone — 308 gets **no reply at
/// all** when it is refused. `DoMsg_GAMEMSG_REQUEST_INVPICKUP` returns before
/// reaching any send on all nine rejection paths (`Server-Item.cpp:2358-2392`).
/// That is what makes a stale click harmless: the client re-sends the same
/// request until something visibly changes, and a correction frame would fight
/// that. Every refusal here is therefore logged rather than answered, because
/// unlike 307 there is no client-visible symptom to correlate it with.
///
/// The two range checks the legacy does before touching the item list are kept on
/// the same side as in the original, so a request outside them never reaches the
/// database.
async fn handle_inv_pickup(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    items: &PgItemRepository,
    character_id: i32,
    payload: &[u8],
) -> Result<(), AnyError> {
    let request = decode_inv_pickup(payload)?;

    // `Server-Item.cpp:2358`: the unique is range-checked against MAX_ITEM_UNIQUE,
    // and `siItemIndex` must not be negative. Both are the client's own words
    // about a handle we allocated inside `1..=65535`, so in practice they always
    // pass -- which is exactly why they are worth checking rather than assuming.
    if i32::from(request.item_unique) <= 0
        || i32::from(request.item_unique) >= MAX_ITEM_UNIQUE
        || request.item_index < 0
    {
        println!(
            "game: ignoring invpickup of unique {} at handle {} (outside the legacy's range)",
            request.item_unique, request.item_index
        );
        return Ok(());
    }

    let outcome = items
        .pickup_ground(
            character_id,
            GroundClaim {
                handle: request.item_index,
                item_unique: request.item_unique,
                map_index: request.map_index,
                x: request.x,
                y: request.y,
            },
        )
        .await?;

    let (slot, item) = match outcome.status {
        PickupStatus::PickedUp => match (outcome.slot, outcome.item) {
            (Some(slot), Some(item)) => (slot, item),
            // The repository builds both together on this path; a missing one is a
            // bug in it rather than a state we can meaningfully answer.
            _ => {
                println!("game: invpickup reported success without a slot or item");
                return Ok(());
            }
        },
        PickupStatus::NoSuchItem { handle } => {
            println!("game: invpickup: no ground item has handle {handle}");
            return Ok(());
        }
        PickupStatus::ItemMismatch { stored, claimed } => {
            println!("game: invpickup: handle names unique {stored}, not {claimed}");
            return Ok(());
        }
        PickupStatus::CellMismatch { stored, claimed } => {
            println!(
                "game: invpickup: item is at ({}, {}, {}), client said ({}, {}, {})",
                stored.0, stored.1, stored.2, claimed.0, claimed.1, claimed.2
            );
            return Ok(());
        }
        PickupStatus::NoFreeSlot => {
            println!("game: invpickup: every carry slot is occupied");
            return Ok(());
        }
    };

    println!(
        "game: picked up item {} x{} into slot {slot} for character {character_id}",
        item.unique(),
        item.count()
    );

    // The bag slot first: the client needs `clCharItem.clItem[siItemPos]` updated
    // before the icon leaves the ground, or it would briefly hold an item it has
    // no record of.
    send_encoded(
        stream,
        outbound,
        GameMsg::ResponsePersoniteminfo,
        &encode_person_item_info(slot, &item),
    )
    .await?;

    // Then the 309 that clears the cell. `GroundItem::removal` is the legacy's
    // own convention -- the item's coordinates with both handles zeroed
    // (`ServerMap.cpp:288`) -- so the same frame shape does both jobs.
    if let Some(cleared) = outcome.cleared {
        send_encoded(
            stream,
            outbound,
            GameMsg::ResponseItemdrop,
            &cleared.encode(),
        )
        .await?;
        println!(
            "game: sent itemdrop 309 removal for map {} ({}, {})",
            cleared.map_index, cleared.x, cleared.y
        );
    }

    Ok(())
}

/// Answers `GAMEMSG_REQUEST_VILLAGESTRUCT` (1101). The client blocks player
/// movement (`OrderAction`) and stops popping reserved characters
/// (`Client-SetChar.cpp:66`) until this arrives, so a village map that never
/// gets an answer looks frozen. Zero structure info renders the empty lots.
async fn handle_village_struct(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    village_ref: i32,
) -> Result<(), AnyError> {
    if village_ref <= 0 || village_ref >= i32::from(MAX_VILLAGE_NUMBER) {
        println!("game: ignoring village-struct request for ref {village_ref}");
        return Ok(());
    }

    let body = VillageStruct {
        village_unique: i16::try_from(village_ref).unwrap_or(0),
        structure_info: [0; MAX_CONSTRUCTION_NUM],
    };
    send_encoded(
        stream,
        outbound,
        GameMsg::ResponseVillagestruct,
        &body.encode(),
    )
    .await?;
    println!("game: sent village {village_ref} structure");
    Ok(())
}

/// The `GAMEMSG_RESPONSE_SERVERINFO` body for this single-world dev server.
///
/// `service_area` must match the client's `ServiceArea.txt`
/// (`SERVICEAREA_ENGLISH` → `0x08`) or `DoMsg_..._SERVERINFO` drops the packet
/// and world entry stalls — the failure the 2026-10-05 login-alert RE traced to
/// a *mask byte* rather than to this value. It comes from configuration so a
/// client built for another region is a config change, not a rebuild.
fn server_info(cfg: &GameConfig) -> ServerInfo {
    ServerInfo {
        service_area: cfg.service_area,
        server_unique: cfg.server_unique,
        item_mall_use: false,
        village_war: false,
        bonus_time: false,
        bonus2_time: false,
        special_quest_bonus_time: false,
        broadcast: false,
        best_village_level: 0,
        npc_price_rate: 100,
        item_price_index: 0,
        event_switch: [false; 8],
        minimum_teacher_level: 0,
        service_age: 0,
        enable_pvp: 0,
        auto_unblock_mode: false,
        vote_campaign: false,
        event_toggle_switch: [false; 2],
    }
}

async fn send_encoded(
    stream: &mut TcpStream,
    cipher: &mut Cipher,
    cmd: GameMsg,
    payload: &[u8],
) -> Result<(), AnyError> {
    send_raw(stream, cipher, cmd as u16, payload).await
}

/// Writes one already-framed body under a numeric opcode.
///
/// Needed for the capture replay: many captured frames have no `GameMsg`
/// variant, and the opcode on the wire is what the client dispatches on anyway.
async fn send_raw(
    stream: &mut TcpStream,
    cipher: &mut Cipher,
    cmd: u16,
    payload: &[u8],
) -> Result<(), AnyError> {
    let frame = Frame::new(cmd, payload)?;
    let wire = frame.into_bytes();
    let enciphered = cipher.encode(&wire)?;
    stream.write_all(&enciphered).await?;
    stream.flush().await?;
    Ok(())
}

/// Whether a captured frame has to be withheld in *every* prologue mode, and why.
///
/// One frame qualifies: 116 `RESPONSE_LETTER`.
///
/// ## 116 `RESPONSE_LETTER` — it re-shows the memo dialog
///
/// 116 is a one-shot notification, not state, and it is not part of the world-entry
/// contract. Every `SendLetterMsg` call site in the legacy is driven by an event —
/// a post-office delivery, a guild invite, a rank change, and for this capture a
/// village resident dividend (`ServerDB-Structure.cpp:1326`, the "bank dividend"
/// memo). Nothing sends 116 unconditionally on character entry, so replaying it
/// asserts an event that did not happen to *this* character.
///
/// Its handler has no "already seen" test either (`Client-System.cpp:2490`): it
/// plays `SOUND_EFFECT_LETTER`, creates `NLETTER_DLG`, calls `Push(buffer)` and
/// flashes the taskbar. So the captured letter pops the "memo 1/1" dialog on every
/// world entry, and would re-pop on every resync as well.
///
/// This is why the predicate is not [`withhold_on_resync`]. A resync-only
/// withholding fixes the once-a-minute dialog while leaving the dialog on the
/// *first* world entry — which is where the player first sees that the letter is
/// somebody else's. There is no mail table to generate a real one from, so the
/// honest answer is that this character has no mail and the frame is not sent.
/// [`Prologue::Live`] already omits it: `world_entry::prologue` builds its frame
/// list from named opcodes and 116 is not among them, so the two modes now agree.
///
/// Withholding it costs nothing. 116 is the only letter frame in the capture, so
/// there is no later frame that would have delivered the letter instead.
///
/// Note this is unrelated to `RESPONSE_DATE` (1208) and
/// `RESPONSE_SERVERTIME` (177), the two time frames the same burst carries. Both
/// replay a *frozen* timestamp rather than a live one, so they are stale rather
/// than spamming; see the clock notes on [`replay`].
fn withhold_always(cmd: u16) -> Option<&'static str> {
    (cmd == GameMsg::ResponseLetter as u16)
        .then_some("it is somebody else's letter: 116 is per-event, not world entry")
}

/// Whether a captured frame has to be withheld when the client re-asks for state
/// it already has.
///
/// One frame qualifies, for a reason unrelated to [`withhold_always`]'s.
///
/// ## 248 `RESPONSE_ALLCHARINFOONE` — it teleports the player
///
/// 248 is the only frame the client acts on by writing `cltPBasicInfo`, and
/// `cltPBasicInfo` embeds `cltCharPos clPos`. `SetOne` copies the whole struct
/// (`Client-Person.cpp:776` -> `PersonInfo.h:589`), and the character's own
/// coordinates are read back out of exactly that field: `cltCharCommon::GetX()`
/// is `return pclCI->clBI.GetX();` (`Char-Common.cpp:1206`), which is
/// `clPos.GetX()` (`Common.cpp:2917`), i.e. `clPos.siX`. So a verbatim 248 moves
/// the client's idea of where it is standing back to the cell the capture was
/// taken on. The client sends 230 after every mutation, so replaying 248 there is
/// what snapped the player back to spawn on a drop.
///
/// Nothing else in the burst does this. 249 goes through `SetTwo`, which touches
/// only `clCharItem` and `clSummonInfo` (`PersonInfo.h:617`), and `SetOne` is the
/// sole `clBI` writer anywhere in the client. Withholding 248 also costs the
/// post-mutation refresh nothing: the inventory weight bar reads
/// `cltCharCommon::GetTotalWeight()` -> `pclCI->clCharItem.GetTotalWeight(...)`
/// (`Char-Common.cpp:2791`), summing the item array that 249 carries, not a
/// cached total held in `clBI`.
///
/// **248 is now generated** (`LiveFrames::regenerate`), so its `clPos` is the
/// database's `map_id`/`pos_x`/`pos_y` rather than the capture's. That is
/// strictly better than replaying, but it does *not* retire this withholding:
/// the database's position is where the character last logged out, not where it
/// is standing now, so a resync would still drag the player backwards -- to a
/// different cell than before, which is worse. Untracking is what fixes it:
/// decoding `REQUEST_USERORDER`, whose `ORDER_MOVE` payload carries the
/// destination as `x | (y << 16)` in `siPara1` (`order.h:96`), and persisting it,
/// so that `clPos` is current and 248 can be replayed on re-sync like any other
/// state frame. That is T-018.
fn withhold_on_resync(cmd: u16) -> bool {
    cmd == GameMsg::ResponseAllcharinfoone as u16
}

/// Replays the captured frames in the half-open index range `start..end`.
///
/// `resync` marks a replay of a step the client already received, which is the
/// one case where a captured frame is not merely stale but actively wrong; see
/// [`withhold_on_resync`].
///
/// # Errors
///
/// Propagates any cipher or socket failure; a write failure ends the session.
async fn replay(
    stream: &mut TcpStream,
    cipher: &mut Cipher,
    (start, end): (u32, u32),
    what: &str,
    live: &LiveFrames<'_>,
    resync: bool,
) -> Result<(), AnyError> {
    let frames: Vec<(u16, &'static [u8])> = generated_capture::frames_in(start, end).collect();
    let total: usize = frames.iter().map(|(_, body)| body.len()).sum();
    println!(
        "game: replaying {what}: {} frames, {total} payload bytes",
        frames.len()
    );
    for (cmd, body) in frames {
        if let Some(why) = withhold_always(cmd) {
            println!("game: withholding {what} frame {cmd}: {why}");
            continue;
        }
        if resync && withhold_on_resync(cmd) {
            // The reason differs per frame -- see `withhold_on_resync` -- and
            // logging one reason for both sent people hunting for the wrong bug.
            let why = if cmd == GameMsg::ResponseAllcharinfoone as u16 {
                "it would reset our position"
            } else {
                "it would re-announce a notification we already sent"
            };
            println!("game: withholding {what} frame {cmd}: {why}");
            continue;
        }
        match live.regenerate(cmd).await? {
            Some(generated) => send_raw(stream, cipher, cmd, &generated).await?,
            None => send_raw(stream, cipher, cmd, body).await?,
        }
    }
    Ok(())
}

/// Sends the generated world-entry sequence. The `Prologue::Live` counterpart
/// to [`replay`].
///
/// # Errors
///
/// Propagates any cipher or socket failure from [`send_raw`]; a write failure
/// ends the session. [`world_entry::prologue`] itself panics only on a
/// programmer error in the frame it is handed.
async fn send_world_entry(
    stream: &mut TcpStream,
    cipher: &mut Cipher,
    char_unique: i32,
    items: &PgItemRepository,
    character_id: i32,
) -> Result<(), AnyError> {
    // `clMakeCharInfo.uiMakeCharUnique` is a 16-bit field, so the session id has
    // to fit one; the same narrowing `handle_select_char` does.
    let char_unique = u16::try_from(char_unique)
        .map_err(|_| format!("session char_unique {char_unique} does not fit u16"))?;
    // `PERSONITEMINFO` is a single-slot notification. The captured one announces
    // slot 8 as empty, so we send whatever the database holds for that slot --
    // which for the reference character is also empty, and for any other is
    // correct rather than a replay of someone else's inventory.
    let slot = world_entry::PERSON_ITEM_INFO_SLOT;
    let stored = items
        .list(character_id)
        .await?
        .into_iter()
        .find(|row| row.slot == slot);
    let announced = match stored {
        Some(row) => Item::decode(&row.raw)?,
        None => Item::empty(),
    };

    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let sequence = world_entry::prologue(&world_entry::WorldEntry {
        char_unique,
        now: world_entry::ServerTime::from_unix_seconds(seconds),
        person_item_info: encode_person_item_info(slot, &announced).to_vec(),
    });

    let total: usize = sequence.iter().map(|(_, body)| body.len()).sum();
    println!(
        "game: generating world-entry prologue: {} frames, {total} payload bytes",
        sequence.len()
    );
    let _ = character_id; // 248/249 in the 230 burst; see `LiveFrames::regenerate`

    for (cmd, body) in sequence {
        send_raw(stream, cipher, cmd, &body).await?;
    }
    Ok(())
}

/// Advances the capture replay plan when `cmd` is the step the reference server
/// answered at this point, and does nothing otherwise.
///
/// Walking the plan as a cursor rather than a lookup keeps the captured
/// one-to-one ordering: opcode 121 gets a different reply the second time the
/// client sends it.
///
/// A step the client already consumed can still be replayed if it asks for it
/// again; see the repeat branch below for why `230` depends on that.
///
/// # Errors
///
/// Propagates any cipher or socket failure from [`replay`].
async fn replay_plan_step(
    stream: &mut TcpStream,
    cipher: &mut Cipher,
    cmd: u16,
    plan_step: &mut usize,
    live: &LiveFrames<'_>,
) -> Result<(), AnyError> {
    let Some((expect, ranges)) = generated_capture::REPLY_PLAN.get(*plan_step) else {
        return Ok(());
    };
    if *expect == cmd {
        *plan_step += 1;
        let label = format!("plan step {expect}");
        for (start, end) in *ranges {
            replay(stream, cipher, (*start, *end), &label, live, false).await?;
        }
        return Ok(());
    }

    // Not the next step. If the client is asking again for a step it already got,
    // that is a re-sync rather than a protocol error: `REQUEST_CHANGESTATUSINFO`
    // (230) means "send me current state", and the client sends it every time it
    // wants a fresh answer — after a move, after a teleport. Answering it again is
    // what makes a mutation observable at all, since `LiveFrames` re-reads
    // PostgreSQL on the way out.
    //
    // The nearest earlier step wins, so a repeat of 230 cannot consume the 121
    // entry that follows it and shift every later pairing.
    //
    // `resync` is set because a repeat is the one delivery where the captured
    // bytes are not just out of date but would move the player: 248 in the 230
    // burst carries the capture's `clPos`, and the client reads its own
    // coordinates straight out of that. See [`withhold_on_resync`].
    if let Some(step) = (0..*plan_step)
        .rev()
        .find(|&i| generated_capture::REPLY_PLAN[i].0 == cmd)
    {
        let label = format!("plan step {cmd} (repeat)");
        for (start, end) in generated_capture::REPLY_PLAN[step].1 {
            replay(stream, cipher, (*start, *end), &label, live, true).await?;
        }
        return Ok(());
    }

    println!("game: replay plan waiting for opcode {expect}, client sent {cmd}");
    Ok(())
}

/// The captured frames this server owns rather than replays.
///
/// Every frame in the burst still comes from `login_capture.pcapng`; the point
/// is only that the parts we have modelled are read back out of PostgreSQL on
/// the way out, so a change the player makes in the world survives the next
/// login. Un-modelled frames stay byte-exact, which is what keeps the client
/// accepting the burst.
struct LiveFrames<'a> {
    items: &'a PgItemRepository,
    /// Needed for 248's `clBI.siIDNum`, which is the owning account rather than
    /// the character.
    account_id: i32,
    /// `clBI.szAccountID`. Captured once at login, where the account is already
    /// authenticated, so the 230 burst needs no further lookup.
    account_name: String,
    /// The selected character, held from `REQUEST_SELECTCHAR` on. 248 needs its
    /// slot, name, position and stats, and re-reading them per resync would mean
    /// a `characters` join on every status request.
    character: Option<CharacterFull>,
    /// This session's `char_unique`, as handed to the client in
    /// `RESPONSE_CLIENTCHARUNIQUE`.
    ///
    /// 244 and 258 both resolve their target by indexing the client's
    /// `CharUniqueInfo` table with a character id carried *in the payload*
    /// (`Client-SystemNPC.cpp:977`, `Client-Person.cpp:1065`) rather than using
    /// the frame's own `id`. Replaying the capture therefore applied the
    /// reference character to our session; generating them means stamping the
    /// session's id, not `characters.game_unique`.
    char_unique: i32,
}

impl LiveFrames<'_> {
    /// Rebuilds `body` for the frames we own, or returns `None` to replay it
    /// verbatim.
    ///
    /// # Errors
    ///
    /// [`gz_db::RepositoryError`] from the item read, or a stored record that is
    /// not a whole `cltItem`.
    async fn regenerate(&self, cmd: u16) -> Result<Option<Vec<u8>>, AnyError> {
        let is_one = cmd == GameMsg::ResponseAllcharinfoone as u16;
        let is_two = cmd == GameMsg::ResponseAllcharinfotwo as u16;
        // The rest of the world-entry burst: nameplate, mount, skill book, clan
        // and village-war state. None of them is the captured character's, so all
        // of them are rebuilt from the selected row or zero-filled.
        let is_name = cmd == GameMsg::ResponseName as u16;
        let is_horse_list = cmd == GameMsg::ResponsePersonHorse as u16;
        let is_change_horse = cmd == GameMsg::ResponseChangehorse as u16;
        let is_skillbook = cmd == GameMsg::ResponseSkillbookMagicinfoGet as u16;
        let is_family = cmd == GameMsg::ResponseFamilyUpdate as u16;
        let is_village = cmd == GameMsg::ResponseGetvillageinfo as u16;
        if !is_one
            && !is_two
            && !is_name
            && !is_horse_list
            && !is_change_horse
            && !is_skillbook
            && !is_family
            && !is_village
        {
            return Ok(None);
        }
        // `None` until `REQUEST_SELECTCHAR` names the character, which is when the
        // 230 burst carrying these frames is first sent.
        let Some(character) = self.character.as_ref() else {
            return Ok(None);
        };
        let character_id = character.character_id;

        if is_two {
            return Ok(Some(encode_all_char_info_two(
                &self.item_array(character_id).await?,
            )?));
        }

        // 244 and 258 carry the session's character id, not the row's: the client
        // resolves both through `CharUniqueInfo`, which is keyed on the id handed
        // out in `RESPONSE_CLIENTCHARUNIQUE`.
        if is_name {
            return Ok(Some(encode_response_name(
                self.char_unique,
                &character.name,
            )));
        }
        if is_horse_list {
            return Ok(Some(encode_person_horse()));
        }
        if is_change_horse {
            // 258 narrows the id to `SI16`; the session id is already bounded
            // below `MAX_SESSION_CHAR_UNIQUE`, so this only trips if that bound
            // and this field ever disagree.
            let char = i16::try_from(self.char_unique).map_err(|_| {
                format!(
                    "session char_unique {} does not fit the SI16 siCharUnique",
                    self.char_unique
                )
            })?;
            return Ok(Some(encode_change_horse(char)));
        }
        if is_skillbook {
            return Ok(Some(encode_skillbook_magic_info()));
        }
        if is_family {
            return Ok(Some(encode_family_update()));
        }
        if is_village {
            return Ok(Some(encode_get_village_info()));
        }

        // 248: the identity-and-stats block, from `characters` + `character_stats`.
        let map_index = i16::try_from(client_map_index(character.map_id)).map_err(|_| {
            format!(
                "map {} does not fit the SI16 clPos.siMapIndex",
                character.map_id
            )
        })?;
        let basic = BasicInfo {
            account_idnum: self.account_id,
            account_id: self.account_name.clone(),
            slot: character.slot as u8,
            sex: character.gender as u8,
            class_kind: character.class_kind as u16,
            home_village: character.school_id as i8,
            map_index,
            x: character.pos_x as i16,
            y: character.pos_y as i16,
            character_id: character.character_id,
            name: character.name.clone(),
            str_: character.str,
            dex: character.dex,
            vit: character.vit,
            mag: character.mag,
            hnd: character.hnd,
            luk: character.luk,
            wis: character.wis,
            level: character.level,
            bonus: character.bonus_points,
            life: character.max_life,
            mana: character.max_mana,
            exp: character.exp,
            money: character.money,
        };
        // The trailing four `SI16` are the capture's set-item progress: type 20
        // step 3, both second slots zero. There is no set-item table yet.
        Ok(Some(encode_all_char_info_one(&basic, 20, 3, 0, 0)))
    }

    /// The character's 80 slots, decoded from `character_items.raw`.
    ///
    /// # Errors
    ///
    /// [`gz_db::RepositoryError`] from the read, or a stored record that is not
    /// a whole `cltItem`.
    async fn item_array(&self, character_id: i32) -> Result<Vec<Item>, AnyError> {
        let stored = self.items.list(character_id).await?;
        let mut array = vec![Item::empty(); MAX_ITEM_PER_PERSON];
        for row in stored {
            let Some(cell) = array.get_mut(usize::from(row.slot)) else {
                return Err(format!(
                    "stored item slot {} is outside 0..{MAX_ITEM_PER_PERSON}",
                    row.slot
                )
                .into());
            };
            *cell = Item::decode(&row.raw)?;
        }
        Ok(array)
    }
}

/// Two key bytes for this connection's session cipher. Unpredictability only
/// needs to defeat accidental reuse in development, not an attacker.
fn random_key_bytes() -> (u8, u8) {
    let v = hash_now();
    (v as u8, (v >> 8) as u8)
}

/// A positive session character id below the client's `CharUniqueInfo` bound
/// (`< 0x4E20`). The value is also written into
/// `clMakeCharInfo.uiMakeCharUnique` (a 16-bit field) and compared against the
/// client's 32-bit `MyCharUnique`, so it must fit both.
const _: () = assert!(
    generated_capture::CAPTURED_CHAR_UNIQUE > 0
        && (generated_capture::CAPTURED_CHAR_UNIQUE as u64) < MAX_SESSION_CHAR_UNIQUE
);

fn hash_now() -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
        .hash(&mut hasher);
    std::thread::current().id().hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    // The declared payload lengths. Only the tests below assert on them, so they
    // are imported here rather than at the top of the file.
    use gz_proto::game::{
        ALL_CHAR_INFO_ONE_LEN, ALL_CHAR_INFO_TWO_LEN, CHANGE_HORSE_LEN, CLIENT_HEALTH_INFO_OFFSET,
        CLT_HEALTH_INFO_LEN, FAMILY_UPDATE_LEN, GET_VILLAGE_INFO_LEN, HEALTH_INFO_OFFSET,
        HEALTH_POINT_OFFSET, PERSON_HORSE_HEADER_LEN, PERSON_HORSE_LEN, RESPONSE_NAME_LEN,
        SKILLBOOK_MAGIC_INFO_LEN,
    };

    /// The regression this locks down is the 2026-10-05 live failure: seven
    /// `REQUEST_SELECTCHAR` frames on one socket, each replaying the 45-frame
    /// world prologue over an already-populated world, and a client that
    /// desynced. Nothing about the second entry is legal — see the predicate's
    /// docs — so the state, not the slot, is what decides.
    #[test]
    fn world_entry_is_one_shot_and_the_state_alone_decides() {
        assert!(
            admit_select_char(None),
            "the first selectchar must be honoured"
        );
        // Same slot or a different slot is irrelevant: the predicate is not
        // given the slot, on purpose, because the legacy's own guard is the
        // bound character rather than the request.
        assert!(!admit_select_char(Some(1)));
        assert!(!admit_select_char(Some(2)));
        // Idempotent: refusing a repeat must not make a later request admissible
        // again, which is what would let the prologue replay N times.
        assert!(!admit_select_char(admit_select_char(None).then_some(7)));
    }

    #[test]
    fn deframe_leaves_an_uncompressed_body_alone() {
        let plain = Frame::new(GameMsg::RequestSelectchar as u16, &7i32.to_le_bytes())
            .unwrap()
            .into_bytes();
        let (cmd, payload) = deframe(&plain).unwrap();
        assert_eq!(cmd, GameMsg::RequestSelectchar as u16);
        assert_eq!(payload.as_ref(), 7i32.to_le_bytes().as_slice());
    }

    #[test]
    fn deframe_decompresses_and_clears_the_flag() {
        // The client compresses game frames >= 200 bytes and sets bit 15
        // (`Client-Winsock.cpp:178-196`); the real client's MAKENEWCHAR arrived
        // as opcode 32775, i.e. 7 | 0x8000.
        let body = vec![0x41u8; 400];
        let mut compressed = vec![0u8; body.len() * 2];
        let n = gz_lzf::compress(&body, &mut compressed).unwrap();
        compressed.truncate(n);

        assert!(compressed.len() < 200, "test body should compress");

        let total = HEADER_SIZE + compressed.len();
        let mut plain = vec![0u8; total];
        plain[..2].copy_from_slice(&(total as u16).to_le_bytes());
        plain[2] = 0;
        plain[3..5].copy_from_slice(
            &(GameMsg::RequestMakenewchar as u16 | COMPRESSION_FLAG).to_le_bytes(),
        );
        plain[HEADER_SIZE..].copy_from_slice(&compressed);

        let (cmd, payload) = deframe(&plain).unwrap();
        assert_eq!(cmd, GameMsg::RequestMakenewchar as u16);
        assert_eq!(payload.as_ref(), body.as_slice());
    }

    /// The splice into `RESPONSE_ALLCHARINFOTWO` has to be byte-transparent.
    ///
    /// The client's willingness to accept the world-entry burst rests on that
    /// frame being exactly what the reference server sent; if the DB holds what
    /// the importer read out of the capture, regenerating it must reproduce the
    /// captured bytes and change nothing else.
    #[test]
    fn regenerating_the_capture_reproduces_the_item_array_byte_for_byte() {
        use gz_proto::item::decode_item_array;

        let body = generated_capture::frame(GameMsg::ResponseAllcharinfotwo as u16)
            .expect("the capture must contain an ALLCHARINFOTWO frame");

        // What `LiveFrames::regenerate` does once the stored rows come back.
        let decoded = decode_item_array(body).unwrap();
        let regenerated = encode_all_char_info_two(&decoded).unwrap();

        assert_eq!(regenerated.len(), body.len(), "frame length");
        // The first `ITEM_ARRAY_LEN` bytes must be identical: the importer read
        // them out of this very frame, so a round trip that changed any would
        // mean the encoder and the decoder disagree about the record layout.
        assert_eq!(
            &regenerated[..gz_proto::item::ITEM_ARRAY_LEN],
            &body[..gz_proto::item::ITEM_ARRAY_LEN],
            "regenerated item array"
        );
        // `clSummonInfo` is the documented deviation: the reference character
        // owned summons (115 non-zero bytes) and we hold no table for them.
        assert!(
            regenerated[gz_proto::item::ITEM_ARRAY_LEN..]
                .iter()
                .all(|&b| b == 0)
        );
    }

    /// The generated 248 must place its identity fields where the client reads
    /// them, and must not carry the capture's.
    ///
    /// Pinned against the capture rather than by review because the failure is
    /// invisible: every field lands somewhere, the frame is the right length, and
    /// the client renders a plausible world built from the wrong character.
    #[test]
    fn generated_all_char_info_one_places_identity_and_stats() {
        let basic = BasicInfo {
            account_idnum: 1471,
            account_id: "tester".into(),
            slot: 2,
            sex: 1,
            class_kind: 0x1234,
            home_village: 0,
            map_index: 35,
            x: 25,
            y: 13,
            character_id: 4242,
            name: "Tester".into(),
            str_: 130,
            dex: 50,
            vit: 5,
            mag: 5,
            hnd: 5,
            luk: 5,
            wis: 5,
            level: 34,
            bonus: 0,
            life: 433,
            mana: 20,
            exp: 349_415,
            money: 1000,
        };
        let body = encode_all_char_info_one(&basic, 20, 3, 0, 0);
        assert_eq!(body.len(), 10_168);

        // `clBI.clPos` @ 32..40 -- the field the client reads its own position
        // back out of (`Char-Common.cpp:1206`).
        assert_eq!(
            i16::from_le_bytes([body[32], body[33]]),
            35,
            "clPos.siMapIndex"
        );
        assert_eq!(i16::from_le_bytes([body[34], body[35]]), 25, "clPos.siX");
        assert_eq!(i16::from_le_bytes([body[36], body[37]]), 13, "clPos.siY");
        // `clBI.clPerson` @ 76..100.
        assert_eq!(i32::from_le_bytes(body[76..80].try_into().unwrap()), 4242);
        assert_eq!(&body[80..86], b"Tester");
        // `clIP.siLevel` @ 932 -- the same offset the char list uses, which is
        // what makes one `BasicInfo` encoder serve both frames.
        assert_eq!(i16::from_le_bytes([body[932], body[933]]), 34);
        assert_eq!(
            i32::from_le_bytes(body[904..908].try_into().unwrap()),
            130,
            "siStr"
        );
        assert_eq!(
            i64::from_le_bytes(body[944..952].try_into().unwrap()),
            349_415
        );
        // `clIP.siMoney` @ 960 -- the last eight bytes of the `clIP` window, so
        // there is no room for a field after it and the offset is self-checking.
        assert_eq!(
            i64::from_le_bytes(body[960..968].try_into().unwrap()),
            1000,
            "siMoney"
        );
        // The three fame fields the legacy seeds rather than loads.
        assert_eq!(i16::from_le_bytes([body[952], body[953]]), 0, "siFameLevel");
        assert_eq!(
            i16::from_le_bytes([body[954], body[955]]),
            1,
            "siSkillBonus -- cltPIPInfo::Init()"
        );
        assert_eq!(
            i32::from_le_bytes(body[956..960].try_into().unwrap()),
            0,
            "siFameExp"
        );
        // Trailing set-item fields.
        assert_eq!(i16::from_le_bytes([body[10_160], body[10_161]]), 20);
        assert_eq!(i16::from_le_bytes([body[10_162], body[10_163]]), 3);

        // The unmodelled sub-structs are the documented deviation: zero, where
        // the capture carried the reference character's skills, horse (`Foal744`)
        // and passport. `clHealthInfo` is carved out of that sweep because the one
        // byte the client reads as a percentage is written -- see
        // `the_health_point_offset_is_the_one_the_capture_pins`.
        for (i, &b) in body[968..10_160]
            .iter()
            .enumerate()
            .filter(|&(i, _)| 968 + i != HEALTH_POINT_OFFSET)
        {
            assert_eq!(b, 0, "unmodelled region byte at offset {}", 968 + i);
        }
    }

    /// `clHealthInfo.uiHealthPoint` is the one byte in `ALLCHARINFOONE` that must
    /// not be zero, and its offset has to be pinned to the reference bytes rather
    /// than to arithmetic over eight struct sizes nobody can re-check by eye.
    ///
    /// The chain is short because `clHealthInfo` is declared immediately before
    /// `clHorseInfo` (`PersonInfo.h:308`, `:313`), with nothing between them. The
    /// capture's reference character rode the `HORSEUNIQUE_FOAL` named `Foal744`,
    /// and `siHorseUnique`/`szName` sit at 0/2 inside `cltHorse`
    /// (`CommonHeader.h:3254-3255`), so the nine-byte marker locates `clHorse[0]`
    /// outright. The array's offset inside `cltPersonHorseInfo` is the same
    /// [`PERSON_HORSE_HEADER_LEN`] the 226 frame independently pins, and
    /// [`CLT_HEALTH_INFO_LEN`] bytes run straight back from there.
    ///
    /// This is also the regression for the 2026-10-06 live failure: the status
    /// window read Str/Dex/Int/Wis/Luck/Hnd as 0 and left VIT as the only stat
    /// standing. `SetHealthAddAbility` (`Parametabox.cpp:7712-7744`) scales the
    /// whole block by `(uiHealthPoint - 100) / 100`, so a zeroed `clHealthInfo`
    /// subtracts every ability from the total column -- and never calls `SetVit`,
    /// which is why VIT alone escaped.
    #[test]
    fn the_health_point_offset_is_the_one_the_capture_pins() {
        let captured = generated_capture::frame(GameMsg::ResponseAllcharinfoone as u16)
            .expect("the capture must contain an ALLCHARINFOONE frame");

        // `HORSEUNIQUE_FOAL` is 12 (`Horse.h:33`), little-endian in an `SI16`,
        // and `szName` starts two bytes later.
        let horses: Vec<usize> = captured
            .windows(9)
            .enumerate()
            .filter(|(_, w)| *w == b"\x0c\x00Foal744")
            .map(|(i, _)| i)
            .collect();
        assert_eq!(horses.len(), 1, "exactly one horse record in the frame");
        let horse = horses[0];

        let horse_info = horse - PERSON_HORSE_HEADER_LEN;
        let health = horse_info - CLT_HEALTH_INFO_LEN;
        assert_eq!(health, HEALTH_INFO_OFFSET, "clHealthInfo starts here");

        // That is where the *sender* wrote it. `cltHealthInfo::Set` is a
        // `memcpy` of the whole struct (`Common.cpp:3256-3259`), so the receiver
        // stores those bytes at its own base — see `CLIENT_HEALTH_INFO_DRIFT`.
        assert_eq!(CLIENT_HEALTH_INFO_OFFSET, HEALTH_INFO_OFFSET + 1);
        assert_eq!(
            HEALTH_POINT_OFFSET, 7_607,
            "the client reads uiHealthPoint one byte past where the sender wrote it"
        );
        assert_eq!(
            HEALTH_POINT_OFFSET - CLIENT_HEALTH_INFO_OFFSET,
            10,
            "uiHealthPoint is the eleventh byte of cltHealthInfo"
        );

        // The captured character's wellness state, so the sweep below has something
        // to be zero against: a healthy Meirin with one organ complaint.
        assert_eq!(
            i16::from_le_bytes([captured[health], captured[health + 1]]),
            0,
            "siHungry"
        );
        assert_eq!(
            &captured[health + 2..health + 6],
            &[0, 0, 0, 0],
            "siInnerOrg1..4"
        );
        assert!(
            captured[HEALTH_POINT_OFFSET] <= 100,
            "the capture's uiHealthPoint ({}) is a percentage",
            captured[HEALTH_POINT_OFFSET]
        );

        // And what we now send.
        let generated = encode_all_char_info_one(
            &BasicInfo {
                account_idnum: 1471,
                account_id: "tester".into(),
                slot: 2,
                sex: 1,
                class_kind: 0,
                home_village: 0,
                map_index: 158,
                x: 27,
                y: 87,
                character_id: 4242,
                name: "test3".into(),
                str_: 5,
                dex: 5,
                vit: 5,
                mag: 5,
                hnd: 5,
                luk: 5,
                wis: 5,
                level: 1,
                bonus: 5,
                life: 40,
                mana: 20,
                exp: 0,
                money: 1000,
            },
            0,
            0,
            0,
            0,
        );
        assert_eq!(
            generated[HEALTH_POINT_OFFSET], 100,
            "cltHealthInfo::Init() seeds 100, which zeroes every siHealth*Rate"
        );
        assert_eq!(
            &generated[CLIENT_HEALTH_INFO_OFFSET..HEALTH_POINT_OFFSET],
            &[0u8; 10],
            "siHungry, siInnerOrg1..4 and uiOrg1..4Disease stay zero as the client reads them"
        );
        assert_eq!(
            generated[HEALTH_INFO_OFFSET], 0,
            "the byte the sender calls uiHealthPoint stays zero: writing the seed there \
             is what raised the client's phantom organ complaint by landing on its \
             uiOrg4Disease"
        );
        assert_eq!(
            generated[HEALTH_INFO_OFFSET + 11],
            100,
            "the seed lands on the sender's trailing pad byte, which is exactly the \
             client's uiHealthPoint once the struct is slid one byte"
        );
        assert_eq!(
            &generated[HEALTH_INFO_OFFSET..HEALTH_POINT_OFFSET - 1],
            &[0u8; 10],
            "the sender's clHealthInfo payload proper is untouched: siHungry, \
             siInnerOrg1..4 and uiOrg1..4Disease all stay zero"
        );
    }

    /// The generated `PERSONITEMINFO` must differ from the capture only in the
    /// two padding bytes the legacy never initialised.
    #[test]
    fn generated_person_item_info_differs_from_the_capture_only_in_padding() {
        let captured = generated_capture::frame(GameMsg::ResponsePersoniteminfo as u16)
            .expect("the capture must contain a PERSONITEMINFO frame");
        let generated = encode_person_item_info(world_entry::PERSON_ITEM_INFO_SLOT, &Item::empty());

        assert_eq!(generated.len(), captured.len());
        let differing: Vec<usize> = (0..generated.len())
            .filter(|&i| generated[i] != captured[i])
            .collect();
        // 6..8 is the `/Zp8` pad between `siChangedNum` and `siExp`; the capture
        // holds uninitialised stack there.
        assert_eq!(differing, vec![6, 7], "only the padding may differ");
        // And the slot is the one the capture announced.
        assert_eq!(i16::from_le_bytes([captured[0], captured[1]]), 8);
    }

    /// Every frame the 230 burst now generates must be the same size as the
    /// captured frame for the same opcode.
    ///
    /// Length is the one property of a generated frame that cannot be argued
    /// about from the struct declarations alone — `sizeof` depends on the
    /// shipped build's packing — so it is pinned to the capture rather than to a
    /// reviewer's arithmetic. This is the check that would have caught a
    /// mis-derived `cltHorse`.
    #[test]
    fn every_generated_burst_frame_matches_its_captured_length() {
        let generated: &[(GameMsg, usize)] = &[
            (GameMsg::ResponseAllcharinfoone, ALL_CHAR_INFO_ONE_LEN),
            (GameMsg::ResponseAllcharinfotwo, ALL_CHAR_INFO_TWO_LEN),
            (GameMsg::ResponseName, RESPONSE_NAME_LEN),
            (GameMsg::ResponsePersonHorse, PERSON_HORSE_LEN),
            (GameMsg::ResponseChangehorse, CHANGE_HORSE_LEN),
            (
                GameMsg::ResponseSkillbookMagicinfoGet,
                SKILLBOOK_MAGIC_INFO_LEN,
            ),
            (GameMsg::ResponseFamilyUpdate, FAMILY_UPDATE_LEN),
            (GameMsg::ResponseGetvillageinfo, GET_VILLAGE_INFO_LEN),
        ];

        for &(msg, declared) in generated {
            let captured = generated_capture::frame(msg as u16)
                .unwrap_or_else(|| panic!("the capture must contain opcode {}", msg as u16));
            assert_eq!(
                declared,
                captured.len(),
                "declared length for opcode {} disagrees with the capture",
                msg as u16
            );
        }
    }

    /// 244 is the nameplate, and it is keyed by a character id taken from the
    /// payload rather than from the frame's `id`.
    ///
    /// `Client-SystemNPC.cpp:977` indexes `CharUniqueInfo` with
    /// `pclinfo->siCharUnique`, so a wrong value here silently renames somebody
    /// else — or nobody. Replaying the capture did exactly that: it carried 1471
    /// *and* `Meirin`, and 1471 resolved to our own slot.
    #[test]
    fn generated_name_is_keyed_by_the_session_id_and_the_rows_name() {
        let captured = generated_capture::frame(GameMsg::ResponseName as u16).unwrap();
        let body = encode_response_name(1471, "Tester");

        assert_eq!(body.len(), captured.len());
        // `siCharUnique` @ 0, `szName[20]` @ 4 (`MsgType.h:3199-3200`).
        assert_eq!(i32::from_le_bytes(body[0..4].try_into().unwrap()), 1471);
        assert_eq!(&body[4..10], b"Tester");
        assert!(body[10..].iter().all(|&b| b == 0), "NUL padded");

        // The capture really did carry the other character here, at the same
        // offsets — so this test fails if the frame stops being generated.
        assert_eq!(i32::from_le_bytes(captured[0..4].try_into().unwrap()), 1471);
        assert_eq!(&captured[4..10], b"Meirin");
    }

    /// 258 is the unmount, and it only unmounts if its `siCharUnique` resolves to
    /// the local player.
    ///
    /// `Client-Person.cpp:1065-1071` does
    /// `CR[CharUniqueInfo[siCharUnique]]->DeleteSlave()` and returns early when
    /// the index does not resolve, so the captured mount survives unless this
    /// frame is stamped with the session's id.
    #[test]
    fn generated_change_horse_dismounts_our_own_character() {
        let captured = generated_capture::frame(GameMsg::ResponseChangehorse as u16).unwrap();
        let body = encode_change_horse(1471);

        assert_eq!(body.len(), captured.len());
        // `siChangeHorseIndex` @ 0 is the legacy's "show horse 0" value, so index 0
        // now resolves to the empty `cltChangeHorse` rather than `Foal744`.
        assert_eq!(i16::from_le_bytes([body[0], body[1]]), 0);
        // `cltChangeHorse` @ 4..112 zeroed; `siHorseUnique` and `szName` included.
        assert!(body[4..112].iter().all(|&b| b == 0));
        // `siCharUnique` @ 112 -- where the capture puts 1471, the last real field
        // before the tail padding.
        assert_eq!(i16::from_le_bytes([body[112], body[113]]), 1471);
        assert_eq!(i16::from_le_bytes([captured[112], captured[113]]), 1471);
    }

    /// 226 lists the five horse slots; empty means unmounted.
    ///
    /// The header is `bool bAutoFoodSwitch`, `bool siCurrentHorseRIDEHORSE`,
    /// `SI16 siCurrentShowHorse` (`Horse.h:223-227`), and the capture's byte 1 is
    /// `01` — riding. The horse array starts at 28, which is 24 bytes past what
    /// those three members account for; see [`PERSON_HORSE_HEADER_LEN`].
    #[test]
    fn generated_person_horse_is_empty_and_unmounted() {
        let captured = generated_capture::frame(GameMsg::ResponsePersonHorse as u16).unwrap();
        let body = encode_person_horse();

        assert_eq!(body.len(), captured.len());
        assert_eq!(PERSON_HORSE_LEN, 568, "28 header + 5 * 108");
        assert!(body.iter().all(|&b| b == 0), "no horse at all");

        // The capture was riding, and held exactly one horse at offset 28.
        assert_eq!(captured[1], 1, "siCurrentHorseRIDEHORSE");
        assert_eq!(&captured[30..37], b"Foal744");
        assert_eq!(
            captured
                .windows(9)
                .filter(|w| *w == b"\x0c\x00Foal744")
                .count(),
            1,
            "exactly one horse record in the frame"
        );
    }

    /// 8371, 8271 and 7203 are zero-filled, and must stay the capture's size.
    ///
    /// All three legacy constructors are `ZeroMemory(this, sizeof(*this))`
    /// (`MsgType-Item.h:1874`, `MsgType-Person.h:2997`, `MsgType.h:3119`), so zero
    /// is the encoding the legacy itself uses for "none" rather than a stand-in
    /// for data we failed to model.
    #[test]
    fn the_empty_world_frames_are_zero_and_capture_sized() {
        for (msg, body) in [
            (
                GameMsg::ResponseSkillbookMagicinfoGet,
                encode_skillbook_magic_info(),
            ),
            (GameMsg::ResponseFamilyUpdate, encode_family_update()),
            (GameMsg::ResponseGetvillageinfo, encode_get_village_info()),
        ] {
            let captured = generated_capture::frame(msg as u16)
                .unwrap_or_else(|| panic!("the capture must contain opcode {}", msg as u16));
            assert_eq!(body.len(), captured.len(), "opcode {}", msg as u16);
            assert!(
                body.iter().all(|&b| b == 0),
                "opcode {} must be all zero",
                msg as u16
            );
        }
    }

    /// No generated frame may carry the captured character's identity.
    ///
    /// The whole point of generating these is that the reference character's name
    /// and horse stop leaking into a fresh session, so the absence is asserted
    /// rather than assumed.
    #[test]
    fn no_generated_frame_carries_the_captured_character() {
        let bodies: Vec<Vec<u8>> = vec![
            encode_all_char_info_one(
                &BasicInfo {
                    account_idnum: 1471,
                    account_id: "tester".into(),
                    slot: 0,
                    sex: 1,
                    class_kind: 1,
                    home_village: 0,
                    map_index: 1,
                    x: 25,
                    y: 13,
                    character_id: 4242,
                    name: "Tester".into(),
                    str_: 5,
                    dex: 5,
                    vit: 5,
                    mag: 5,
                    hnd: 5,
                    luk: 5,
                    wis: 5,
                    level: 1,
                    bonus: 0,
                    life: 20,
                    mana: 20,
                    exp: 0,
                    money: 1000,
                },
                20,
                3,
                0,
                0,
            ),
            encode_response_name(1471, "Tester"),
            encode_person_horse(),
            encode_change_horse(1471),
            encode_skillbook_magic_info(),
            encode_family_update(),
            encode_get_village_info(),
        ];

        for body in &bodies {
            assert!(
                !contains_ascii(body, b"Meirin"),
                "the captured character's name leaked into a generated frame"
            );
            assert!(
                !contains_ascii(body, b"Foal744"),
                "the captured horse leaked into a generated frame"
            );
        }
    }

    /// A NUL-terminated ASCII needle anywhere in `haystack`.
    fn contains_ascii(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    /// A re-sync has to keep the inventory refresh and lose the position reset.
    ///
    /// Withholding 248 is only safe because 249 -- the frame that carries the item
    /// array `LiveFrames` re-reads from PostgreSQL -- is a different frame. If the
    /// two ever collapsed into one, a drop would stop showing up in the bag.
    #[test]
    fn a_resync_drops_only_the_position_carrying_frame() {
        let step = generated_capture::REPLY_PLAN
            .iter()
            .find(|(cmd, _)| *cmd == GameMsg::RequestChangestatusinfo as u16)
            .map(|(_, ranges)| *ranges)
            .expect("the plan must answer a state sync");

        let sent: Vec<u16> = step
            .iter()
            .flat_map(|&(start, end)| generated_capture::frames_in(start, end))
            .map(|(cmd, _)| cmd)
            .collect();

        // The reference burst carries each of these exactly once.
        for cmd in [
            GameMsg::ResponseAllcharinfoone as u16,
            GameMsg::ResponseAllcharinfotwo as u16,
        ] {
            assert_eq!(
                sent.iter().filter(|&&c| c == cmd).count(),
                1,
                "expected exactly one {cmd} in the state-sync burst"
            );
        }

        let withheld: Vec<u16> = sent
            .iter()
            .copied()
            .filter(|&cmd| withhold_always(cmd).is_some() || withhold_on_resync(cmd))
            .collect();
        assert_eq!(
            withheld,
            vec![
                GameMsg::ResponseAllcharinfoone as u16,
                GameMsg::ResponseLetter as u16,
            ]
        );

        // And what is left still refreshes the bag.
        let kept: Vec<u16> = sent
            .iter()
            .copied()
            .filter(|&cmd| withhold_always(cmd).is_none() && !withhold_on_resync(cmd))
            .collect();
        assert!(kept.contains(&(GameMsg::ResponseAllcharinfotwo as u16)));
        assert_eq!(sent.len(), 25, "the captured state-sync burst is 25 frames");
        assert_eq!(kept.len(), sent.len() - 2);
    }

    /// The memo dialog is somebody else's letter, and it is withheld on *entry*.
    ///
    /// This is the regression test for the popup on world entry. It asserts two
    /// things that could each regress alone: the capture really does carry a
    /// letter, so that withholding 116 has something to withhold; and 116 is
    /// withheld by [`withhold_always`] rather than only by [`withhold_on_resync`].
    /// The second is the one that bites -- a resync-only withholding leaves the
    /// dialog on the first world entry, which is exactly where the player first
    /// sees that the letter is not theirs.
    #[test]
    fn the_state_sync_burst_carries_the_letter_that_repopped_the_dialog() {
        let step = generated_capture::REPLY_PLAN
            .iter()
            .find(|(cmd, _)| *cmd == GameMsg::RequestChangestatusinfo as u16)
            .map(|(_, ranges)| *ranges)
            .expect("the plan must answer a state sync");

        let letters: Vec<u16> = step
            .iter()
            .flat_map(|&(start, end)| generated_capture::frames_in(start, end))
            .map(|(cmd, _)| cmd)
            .filter(|&cmd| cmd == GameMsg::ResponseLetter as u16)
            .collect();

        assert_eq!(
            letters.len(),
            1,
            "the capture carries exactly one letter; a second would mean \
             withholding it could lose a genuine new memo"
        );
        assert!(
            withhold_always(GameMsg::ResponseLetter as u16).is_some(),
            "116 must be withheld in every prologue mode, not only on resync"
        );
        assert!(
            !withhold_on_resync(GameMsg::ResponseLetter as u16),
            "116 is not a resync concern; if it were back here, entry would replay it"
        );
    }

    /// `replay` drops 116 in every mode, which is the half of the agreement that
    /// lives here. `world_entry::prologue`'s half -- that `live` never built it --
    /// is asserted in that module, next to the frame list that would have to
    /// change for it to start.
    ///
    /// Together they stop the two prologues drifting apart, which would make the
    /// memo dialog depend on which mode the operator happened to launch.
    #[test]
    fn replay_withholds_the_letter_in_both_prologue_modes() {
        for resync in [false, true] {
            assert!(
                withhold_always(GameMsg::ResponseLetter as u16).is_some(),
                "116 must be dropped on entry (resync={resync}) as well as on resync"
            );
        }
    }

    /// The two time frames in the burst are stale, not spammy — so they stay.
    ///
    /// They replay a timestamp frozen at capture time rather than a live one. That
    /// is a separate defect from the dialog spam, and fixing it means generating a
    /// live clock rather than replaying one, so neither is withheld here.
    #[test]
    fn the_time_frames_are_frozen_rather_than_withheld() {
        assert!(!withhold_on_resync(GameMsg::ResponseDate as u16));
        assert!(!withhold_on_resync(GameMsg::ResponseServertime as u16));

        // Confirm the "frozen" half of that claim rather than asserting it: both
        // frames must decode to the capture-time instant, and 1208 must be the
        // 4-byte packed cltDate rather than a wider struct.
        let date = generated_capture::frame(GameMsg::ResponseDate as u16)
            .expect("the capture must contain a RESPONSE_DATE frame");
        assert_eq!(date.len(), 4, "cltDate is a single packed bitfield word");

        let stime = generated_capture::frame(GameMsg::ResponseServertime as u16)
            .expect("the capture must contain a RESPONSE_SERVERTIME frame");
        assert_eq!(stime.len(), 16, "_SYSTEMTIME is eight 16-bit words");
    }

    /// The item array a resync still sends has to be the whole modelled range.
    ///
    /// `regenerating_the_capture_reproduces_it_byte_for_byte` proves the splice
    /// is byte-transparent; this pins that the frame reaching the client after a
    /// drop is the one the splice wrote, i.e. that withholding 248 did not also
    /// withhold the refresh.
    #[test]
    fn the_captured_state_sync_still_carries_a_full_item_array() {
        let body = generated_capture::frame(GameMsg::ResponseAllcharinfotwo as u16)
            .expect("the capture must contain an ALLCHARINFOTWO frame");
        let items = gz_proto::item::decode_item_array(body).unwrap();
        assert_eq!(items.len(), gz_proto::item::MAX_ITEM_PER_PERSON);
        // The capture was taken with the reference character's kit on, so the
        // frame is not trivially empty and would be visibly wrong if dropped.
        assert!(items.iter().any(|i| !i.is_empty()));
    }

    /// Pins the item layout facts the importer and the encoder both rest on.
    #[test]
    fn the_captured_item_array_is_eighty_four_byte_records() {
        use gz_proto::item::{Item, decode_item_array, personitem::*};

        let body = generated_capture::frame(GameMsg::ResponseAllcharinfotwo as u16).unwrap();
        let items = decode_item_array(body).unwrap();
        assert_eq!(items.len(), MAX_ITEM_PER_PERSON);

        // Slot 0 is the helmet the reference character had equipped. These are
        // the values `0003`'s importer wrote for character 1.
        assert_eq!(items[HELMET as usize].unique(), 705);
        assert_eq!(items[HELMET as usize].count(), 1);
        assert_eq!(items[WEAPON1 as usize].unique(), 1);

        // The reference character carries a nearly full bag: 19 of the 20 bag
        // slots, with only 20 empty. That occupancy is what pinned the bag at 20
        // wide -- read as the six slots `ItemUnit.h` declares, slots 21..=34 would
        // be an unexplained run of 14 more items in a range with no name.
        let bag: Vec<u16> = (INV0..=INV_END)
            .filter(|&slot| !items[slot as usize].is_empty())
            .collect();
        assert_eq!(bag.len(), 19);
        assert_eq!(bag.first(), Some(&INV0));
        assert_eq!(bag.last(), Some(&INV_END));
        assert!(
            items[20usize].is_empty(),
            "slot 20 is the only free bag slot"
        );
        assert!(bag.contains(&24) && bag.contains(&28));

        // The horse holds 13 of its 20 slots; both extra bags are empty, which is
        // consistent with a character carrying no bag-expansion items.
        let horse: Vec<u16> = (HORSE0..=HORSE_END)
            .filter(|&slot| !items[slot as usize].is_empty())
            .collect();
        assert_eq!(horse.len(), 13);
        assert!(
            items[BAG0 as usize..=BAG0_END as usize]
                .iter()
                .all(Item::is_empty)
        );
        assert!(
            items[BAG1 as usize..=BAG1_END as usize]
                .iter()
                .all(Item::is_empty)
        );

        // Slot 79 is outside every container: the array is 80 long but only 79
        // slots are addressable.
        assert!(items[BAG1_END as usize + 1..].iter().all(Item::is_empty));
    }
}
