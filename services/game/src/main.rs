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
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gz_cipher::Cipher;
use gz_db::DropStatus;
use gz_db::{
    AccountRepository, CharacterFull, CharacterRepository, DropLocation, GroundClaim,
    ItemRepository, MoveOutcome, PgAccountRepository, PgCharacterRepository, PgItemRepository,
    PickupStatus, RepositoryError, SplitStatus,
};
use gz_net::session::{Rejection, Session};
use gz_net::{COMPRESSION_FLAG, Frame, FrameParser, HEADER_SIZE, MAX_FRAME_SIZE, Tick};
use gz_proto::chat::{
    CHATMODE_GUILD, CHATMODE_HEAD, CHATMODE_HOME, CHATMODE_KING, CHATMODE_NEWSITEM, CHATMODE_PARTY,
    CHATMODE_SECRET, CHATMODE_TOWN, CHATMODE_VILLAGER, ChatRequest, MAX_HEAD_CHAT_TEXT_SIZE,
    MAX_PLAYER_NAME, SECRETCHAT_FAIL_INVALIDNAME, SECRETCHAT_FAIL_NOTUSER, SECRETCHAT_SUCCESS,
    SecretChatRequest, cut, decode_chat_request, decode_head_chat_request,
    decode_secret_chat_request, encode_chat_response, encode_head_chat_response, encode_notice,
    encode_secret_chat_result,
};
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
use gz_proto::order::{BasicCharUpdate, ORDER_MOVE, OrderData};
use gz_proto::personalshop::{ShopTradeData, decode_personal_shop_request, shop_mode_for_request};
use gz_proto::statusinfo::{CHANGETYPE_AUTOFOOD, ChangeStatusInfo, decode_change_status_info};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

type AnyError = Box<dyn std::error::Error + Send + Sync>;

mod config;
mod generated_capture;
mod generated_frames;
mod import_capture;
mod registry;
mod world_entry;

use config::{GameConfig, Prologue};
use registry::{Outbound, Registered, Registry, SessionInfo, SessionKey};

/// `MAX_TOTAL_CHAR_PER_USER` (3 main + 12 char-house) — the legacy sends one
/// char-list packet for every slot, occupied or not
/// (`ServerDB-Person.cpp:1560-1606`). **All 15 are required:** the shipped
/// client's `IsReadyCharList` (`0x737120`) AND-reduces `clBI.GetIndex() == i`
/// across `i in 0..0xF` and only self-heals the missing slots after a long
/// timeout — sending fewer than 15 is what makes char-select stall. The array
/// is 15 entries (`PutCharList` `0x48D040` rejects `GetIndex() > 14`).
const CHAR_SLOTS: i16 = 15;

/// `MOVE_PERCENT_STEP` — the accumulator level at which the legacy commits a
/// tile (`clMoveInfo.IncMovePercent(movedlength) >= 50`, `char-move.cpp:159`).
const MOVE_PERCENT_STEP: i32 = 50;

/// `MOVE_PERCENT_COST` — what a committed tile subtracts from the accumulator
/// (`siMovePercent - 100`, `char-move.cpp:164`).
const MOVE_PERCENT_COST: i32 = 100;

/// `(CHAR_ACTION_INTERVAL_FRAME + 1) * 50` — the cap on one advance's elapsed
/// time, so a long stall cannot bank more than one tile (`char-move.cpp:145`;
/// `CHAR_ACTION_INTERVAL_FRAME` is 6 at `Char-Common.h`).
const MOVE_DELAY_CAP_MS: u32 = 350;

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

/// Elapsed-since-process-start stamp, `+mm:ss.mmm`, prefixed to every log line.
///
/// Added for the live run's one open question: where the ~20 s between
/// `REQUEST_SELECTCHAR` and the world appearing goes (handoff 2026-10-08).
/// Two adjacent stamps 20 s apart are time the *server* spent, while a stamp
/// followed 20 s later by the next *inbound* frame is time the *client*
/// spent — the distinction the unstamped log could not make. It reads
/// [`now_tick`]'s clock, so stamps also agree with the session ticks.
fn log_stamp() -> String {
    let ms = now_tick();
    let secs = ms / 1000;
    format!("+{:>3}:{:02}.{:03}", secs / 60, secs % 60, ms % 1000)
}

/// Wraps one log call in [`log_stamp`], in place of a bare `println!`.
///
/// The bodies name the `std::` forms directly, and this block was inserted
/// after the mechanical `println!` → `logln!` rewrite — so the rewrite can
/// never reach inside these macros and recurse.
macro_rules! logln {
    ($($arg:tt)*) => {
        std::println!("[{}] {}", log_stamp(), std::format_args!($($arg)*))
    };
}

/// [`logln`]'s stderr twin, in place of the bare `eprintln!` the error paths used.
macro_rules! logln_err {
    ($($arg:tt)*) => {
        std::eprintln!("[{}] {}", log_stamp(), std::format_args!($($arg)*))
    };
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
    // One table for every connection: the chat family is the first code here
    // that addresses players other than the one asking.
    let registry = Arc::new(Registry::new());
    let listener = TcpListener::bind(&cfg.listen).await?;
    logln!("game: listening on {}", cfg.listen);
    logln!(
        "game: session timeouts: handshake {}ms, connect {}ms, close throttle {}ms",
        cfg.timeouts.handshake.as_millis(),
        cfg.timeouts.connect.as_millis(),
        cfg.timeouts.close_throttle.as_millis()
    );
    logln!(
        "game: link key from configuration; prologue={:?} char_unique={}",
        cfg.prologue,
        match cfg.prologue {
            Prologue::Capture => "capture-pinned".to_string(),
            Prologue::Live => "allocated per session".to_string(),
        }
    );
    if cfg.prologue == Prologue::Capture {
        logln!(
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
        let registry = registry.clone();
        let peer = peer.to_string();
        let cfg = cfg.clone();
        tokio::spawn(async move {
            if let Err(e) =
                handle_client(stream, &peer, &cfg, accounts, characters, items, registry).await
            {
                logln_err!("game: {peer}: {e}");
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
            logln!(
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

/// One turn of the read loop's `select!`.
///
/// Three things can end the wait: a frame from the peer, a frame queued for
/// this connection by a broadcast ([`Outbound`]), or the window that bounds the
/// wait (the handshake deadline before the first frame, the next movement step
/// after it). Every branch is cancel-safe — `TcpStream::read` consumes nothing
/// while pending, an `mpsc` `recv` that loses the race has taken nothing, and a
/// sleep holds no state — so a frame cannot be lost by the switch.
enum LoopTick {
    /// Bytes arrived (or the socket reported EOF, or the read failed).
    Read(std::io::Result<usize>),
    /// A queued outbound frame is due. `None` would mean every sender is gone,
    /// which cannot happen while this task holds `out_tx`.
    Send(Option<Outbound>),
    /// The wait's window elapsed.
    TimedOut,
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
    registry: Arc<Registry>,
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
    logln!("game: {peer}: charunique={char_unique} -> ({key1:#04x}, {key2:#04x})");

    // Inbound frames use the negotiated session cipher, keyed by the bytes just
    // sent -- the exact pairing at char.cpp:5177-5219.
    let mut inbound = Cipher::session_cipher(key1, key2);
    let mut parser = FrameParser::new();
    let mut buf = [0u8; 8192];
    // Set once the game login succeeds; select-char and later opcodes need it.
    let mut login: Option<GameSession> = None;
    // Cursor into `generated_capture::REPLY_PLAN`. The plan is the one-shot
    // world-entry bulk: step 0 is answered on the first `REQUEST_CHANGESTATUSINFO`
    // (230) and `plan_step` stays past it, so a later 230 is dispatched as the
    // legacy status message instead of replaying the bulk again (T-018).
    let mut plan_step = 0usize;
    // The move being stepped, if any. Driven by the read loop's timer arm; see
    // [`Movement`].
    let mut movement: Option<Movement> = None;
    // Which captured frames get rebuilt from PostgreSQL on the way out.
    let mut live = LiveFrames {
        items: &items,
        account_id: 0,
        account_name: String::new(),
        character: None,
        char_unique,
    };

    // The registry key this connection answers under, plus the removal guard.
    // The guard is armed here — before the first frame — so that every exit
    // (EOF, framing rejection, cipher desync, handshake timeout) cleans up after
    // a character that *did* register, while a drop before `REQUEST_SELECTCHAR`
    // is the no-op it should be. See `Registered`.
    let session_key = Registry::next_key();
    let _registered = Registered::new(registry.clone(), session_key);
    // Everything aimed at this connection but produced elsewhere — a whisper,
    // a head-chat fan, the general-chat broadcast this speaker is also part of
    // — is queued here and written by this loop, so only this task ever writes
    // to the socket (`Outbound`).
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<Outbound>();

    loop {
        // The handshake window runs from accept to the first inbound frame. Past
        // it, a peer that has said nothing is a silent half-open connection and
        // gets reclaimed rather than parked forever -- the fix the legacy's
        // 10 s check does not deliver. See gz_net::session.
        //
        // Once a move is in flight the same read is bounded by the next step
        // instead, so an idle client still has its avatar stepped (`Movement`).
        let read_window = if !session_state.state().is_active() {
            Some(cfg.timeouts.handshake)
        } else {
            movement.as_ref().map(Movement::next_delay)
        };
        let tick = tokio::select! {
            read = stream.read(&mut buf) => LoopTick::Read(read),
            queued = out_rx.recv() => LoopTick::Send(queued),
            _ = async {
                match read_window {
                    Some(window) => tokio::time::sleep(window).await,
                    // No window: nothing but a read or a queued frame can end
                    // the wait, exactly as the blocking read did before.
                    None => std::future::pending::<()>().await,
                }
            } => LoopTick::TimedOut,
        };
        let n = match tick {
            LoopTick::Send(Some(frame)) => {
                send_raw(&mut stream, &mut outbound, frame.cmd, &frame.payload).await?;
                continue;
            }
            LoopTick::Send(None) => {
                // Unreachable: `out_tx` is held for the whole connection.
                return Err("outbound channel closed while the session was live".into());
            }
            LoopTick::TimedOut => {
                if session_state.state().is_active() {
                    // A move's next tile is due: step it and keep reading.
                    step_movement(
                        &mut stream,
                        &mut outbound,
                        &mut live,
                        &mut login,
                        &mut movement,
                        &registry,
                        session_key,
                    )
                    .await?;
                    continue;
                }
                let now = now_tick();
                if let Some(reason) = session_state.handshake_expired(now) {
                    logln_err!(
                        "game: {peer}: {} ms with no inbound frame; disconnecting ({reason:?})",
                        cfg.timeouts.handshake.as_millis()
                    );
                }
                session_state.reject(Rejection::HandshakeTimeout, now);
                return Ok(());
            }
            LoopTick::Read(read) => read?,
        };
        if n == 0 {
            logln!(
                "game: {peer}: disconnected after {} ms",
                now_tick().saturating_sub(session_state.since())
            );
            session_state.close(now_tick());
            return Ok(());
        }
        parser.feed(&buf[..n]).map_err(|e| {
            let reason = session_state.reject(Rejection::Framing(e), now_tick());
            logln_err!("game: {peer}: framing rejection {reason:?}; disconnecting");
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
                    logln_err!("game: {peer}: framing rejection {reason:?}; disconnecting");
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
                    logln_err!("game: {peer}: cipher error {e} ({reason:?}); disconnecting");
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
                logln_err!(
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
                    logln_err!(
                        "game: {peer}: LZF failure at opcode {:#06x} ({reason:?}); \
                         disconnecting: {e}",
                        frame.cmd()
                    );
                    return Ok(());
                }
            };
            logln!("game: {peer}: recv opcode {cmd} ({} bytes)", payload.len());

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
                    // The bound character is the currently entered one. A request
                    // for the *same* slot is the legacy's silent no-op
                    // (`ServerDB-Person.cpp:1763`); a request for a different slot
                    // is a real switch (`:1770` `CloseAction`, `:1795`
                    // `DeleteChar`, then the new character is loaded and announced
                    // through `SendServerInfo`). World entry is therefore not
                    // one-shot per connection.
                    let bound_slot = live.character.as_ref().map(|c| c.slot);
                    if !admit_select_char(bound_slot, slot) {
                        logln_err!(
                            "game: {peer}: selectchar slot {slot} ignored: it is already the \
                             active character (ServerDB-Person.cpp:1763 returns for the same \
                             PersonID)"
                        );
                        continue;
                    }
                    // Switching avatars tears the old one down before the new
                    // world entry, exactly like the legacy `CloseAction`. Stop any
                    // in-flight step so the old character cannot walk on, and
                    // rewind the capture reply plan so the new entry's first 230
                    // resends the world bulk (`ServerDB-Person.cpp:1795`).
                    movement = None;
                    plan_step = 0;
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
                    // Enter the character into the shared table. From here on
                    // this player is visible to the chat family, and a second
                    // REQUEST_SELECTCHAR re-enters the same key with a fresh
                    // row and no head chat (`Registry::register`). The key is
                    // the connection's serial, never `char_unique` — see the
                    // registry module docs for why capture mode makes those
                    // differ.
                    registry.register(
                        session_key,
                        out_tx.clone(),
                        SessionInfo {
                            char_unique: i16::try_from(char_unique).map_err(|_| {
                                format!(
                                    "session char_unique {char_unique} does not fit the \
                                     SI16 siCharUnique"
                                )
                            })?,
                            name: character_id.name.clone(),
                            level: i32::from(character_id.level),
                            map_id: character_id.map_id,
                            x: character_id.pos_x,
                            y: character_id.pos_y,
                            school_id: character_id.school_id,
                        },
                    );
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
                    // 230 has two roles in this port. The *first* one after
                    // char-select is the reference server's one-shot world
                    // bulk -- map/object data, then ALLCHARINFO one/two, carried
                    // by `REPLY_PLAN`'s step 0. Every later 230 is the legacy
                    // `CHANGESTATUSINFO` message, which the legacy answers with
                    // at most a 24-byte 231 (`Server-Person.cpp:2831-2860`).
                    //
                    // Before T-018 both roles went through the same plan cursor,
                    // so a repeated 230 hit the cursor's re-sync branch and
                    // replayed the whole ~65 KB bulk -- the live "movement
                    // micro-stutter". The plan is now stepped exactly once; see
                    // [`replay_plan_step`].
                    let world_bulk_pending = cfg.prologue == Prologue::Capture && plan_step == 0;
                    if world_bulk_pending {
                        replay_plan_step(&mut stream, &mut outbound, cmd, &mut plan_step, &live)
                            .await?;
                    } else {
                        handle_change_status_info(
                            &mut stream,
                            &mut outbound,
                            char_unique,
                            payload.as_ref(),
                        )
                        .await?;
                    }
                }
                c if c == GameMsg::RequestUserorder as u16 => {
                    // If a move is already being stepped, the authoritative cell
                    // is the tile it has reached, not the destination the cached
                    // row was set to when that move started. Re-seat the row
                    // before the handler reads it as its starting cell.
                    if let Some(mv) = movement.as_ref()
                        && let Some(character) = live.character.as_mut()
                    {
                        character.pos_x = mv.pos.0;
                        character.pos_y = mv.pos.1;
                    }
                    let order = OrderData::decode(payload.as_ref())?;
                    match handle_userorder(
                        &mut stream,
                        &mut outbound,
                        &characters,
                        &mut live,
                        order,
                    )
                    .await?
                    {
                        Some(mv) => {
                            if let Some(session) = login.as_mut() {
                                session.entry_cell = Some(DropLocation {
                                    map_id: mv.map_id,
                                    x: mv.pos.0,
                                    y: mv.pos.1,
                                });
                            }
                            movement = Some(mv);
                        }
                        // A modelled stop, or any order this port does not act
                        // on: stop stepping so the authoritative cell freezes.
                        None => movement = None,
                    }
                }
                c if c == GameMsg::RequestHopetoquit as u16
                    || c == GameMsg::RequestHopetofront as u16 =>
                {
                    // Both are client leave requests, answered with an empty
                    // 108/110 frame. The legacy only acks them
                    // (`DoMsg_GAMEMSG_REQUEST_HOPETOQUIT`/`_HOPETOFRONT` set a
                    // `bHopeToQuit`/`bHopeToFront` flag and `SendNetMsg`) and
                    // lets the client close the socket itself; it never calls
                    // `CloseSocket` from the handler.
                    //
                    // Closing here races the client. The first thing
                    // `DoMsg_GAMEMSG_RESPONSE_HOPETOFRONT` does is
                    // `if(pGameSession->CloseSocket())` (`Client-System.cpp:3027`),
                    // and `ITZSession::CloseSocket` returns FALSE once the socket
                    // is already invalid. A FIN that lands with -- or just ahead
                    // of -- the 110 dispatch therefore makes the client skip its
                    // entire reset: it neither returns to the front nor clears
                    // its game state. Keeping the socket open lets the ack land
                    // on a live session and the client tear itself down cleanly.
                    let ack = if c == GameMsg::RequestHopetoquit as u16 {
                        GameMsg::ResponseHopetoquit
                    } else {
                        GameMsg::ResponseHopetofront
                    };
                    handle_leave_request(
                        &mut stream,
                        &mut outbound,
                        &characters,
                        login.as_ref(),
                        ack,
                    )
                    .await?;
                    logln!(
                        "game: {peer}: {cmd} ack sent; leaving the socket open for the client \
                         to close ({})",
                        if ack == GameMsg::ResponseHopetoquit {
                            "quit"
                        } else {
                            "return to character select"
                        }
                    );
                }
                c if c == GameMsg::RequestHeartbeat as u16 => {
                    // `DoMsg_GAMEMSG_REQUEST_HEARTBEAT` is an empty no-op
                    // (`Server-System.cpp:520-524`): the client says "still
                    // here" and the server answers nothing.
                    logln!("game: {peer}: heartbeat -> no reply (legacy no-op)");
                }
                // The reference server sends nothing for 246; the data the client
                // wants arrives after 230.
                c if c == GameMsg::RequestAllcharinfo as u16 => {
                    logln!("game: {peer}: allcharinfo request -> no reply (matches capture)");
                }
                c if c == GameMsg::RequestPersonalshopdata as u16 => {
                    let session = login
                        .as_ref()
                        .ok_or("personal shop request before a successful game login")?;
                    handle_personal_shop_data(
                        &mut stream,
                        &mut outbound,
                        session,
                        char_unique,
                        payload.as_ref(),
                    )
                    .await?;
                }
                // ---- the chat family: 117, 119, 121 (T-018 slice 5) ----
                //
                // All three are handled synchronously: they read the registry,
                // encode, and queue. Nothing but this task writes to the
                // socket, so a reply to *this* connection goes out through the
                // same queue as a broadcast to everybody else, in order.
                c if c == GameMsg::RequestChat as u16 => {
                    let request = decode_chat_request(payload.as_ref())?;
                    handle_chat(&registry, session_key, request);
                }
                c if c == GameMsg::RequestSecretchat as u16 => {
                    let request = decode_secret_chat_request(payload.as_ref())?;
                    handle_secret_chat(&registry, session_key, request);
                }
                c if c == GameMsg::RequestHeadchat as u16 => {
                    let subject = decode_head_chat_request(payload.as_ref())?;
                    handle_head_chat_request(&registry, session_key, subject);
                }
                other => logln!("game: {peer}: unhandled opcode {other}"),
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
    /// The character's current cell, used as the cell for
    /// `GAMEMSG_REQUEST_INVDROP`.
    ///
    /// Seeded at world entry and updated on every `ORDER_MOVE`
    /// ([`handle_userorder`]), so a dropped item lands where the character is
    /// standing rather than where it spawned.
    entry_cell: Option<DropLocation>,
}

/// Whether a `GAMEMSG_REQUEST_SELECTCHAR` should be honoured on this connection.
///
/// The old reading here ("world entry is one-shot, a repeat is a silent no-op in
/// every case the legacy can reach") was **wrong** and is corrected here. The
/// legacy's guard is the *bound character*, not the act of having entered:
///
/// 1. `Server-Login.cpp:184` reads `idnum` from `pclCI->clBI.siIDNum`, which is
///    the **account** idnum, not the character's (`LiveFrames` relies on the same
///    fact for 248). `uiIndex` is the requested slot.
/// 2. `rsp_PersonIDGet` (`Goonzu_DB\04. GameDB___________.txt:1073`) selects
///    `WHERE [idnum] = @idnum and [index] = @index`. With `idnum` the account, a
///    request for *any* slot of the account matches a row.
/// 3. `ServerDB-Person.cpp:1757` returns only when the DB result is empty (no
///    such slot). For an occupied slot, `:1763` returns only when the requested
///    character is the one already bound (`GetPersonID()` equals the request's).
///    Otherwise `:1770` runs `CloseAction`, `:1795` `DeleteChar`s the old avatar,
///    and the new character is announced through `SendServerInfo`.
///
/// So a second selectchar naming a **different** character is legal and must
/// tear down and reload; only re-selecting the *same* character is a no-op. The
/// client relies on this: the exit dialog's character-select button sets
/// `GS_INITCHARLIST` with no server message (`NExitDlg.cpp:288`), reusing the
/// same connection, and `CNSelectCharDlg::SelectChar()` then sends the request
/// (`NSelectChar.cpp:1287`).
fn admit_select_char(bound_slot: Option<i16>, slot: i32) -> bool {
    match bound_slot {
        // No character entered yet: the first selectchar always proceeds.
        None => true,
        // Same slot: the legacy detects the same PersonID and returns.
        Some(bound) => i32::from(bound) != slot,
    }
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

    logln!(
        "game: login ok for {:?} (account {}, auth_key {}): {occupied} character(s)",
        login.userid,
        account.account_id,
        login.auth_key
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
        logln!(
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
        // The character's effective `GetTotalMoveSpeed()` (base class speed plus
        // gear; `character_stats.move_speed`), not a fixed reference value.
        total_move_speed: u8::try_from(character.move_speed.clamp(0, 63)).unwrap_or(0),
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

    logln!(
        "game: selectchar slot {slot} char {:?} -> map {} at ({}, {})",
        character.name,
        character.map_id,
        character.pos_x,
        character.pos_y
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
    logln!("game: itemmove {from} -> {to} for character {character_id}: {detail}",);

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
    logln!("game: itemsplit {from} -> {to} x{count} for character {character_id}: {detail}");

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
    logln!("game: created char {name:?} id={character_id} slot={slot} for account {account_id}");
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
        logln!("game: char-del slot {slot} out of range for account {account_id}");
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
            logln!("game: char-del slot {slot} id {id} for account {account_id}: done");
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
            logln!("game: char-del slot {slot} for account {account_id}: slot already empty");
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
        logln!(
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
    logln!("game: invdrop slot {slot} for character {character_id}: {detail}");

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
        logln!(
            "game: sent itemdrop 309: map {} ({}, {}) unique {} handle {}",
            grounded.map_index,
            grounded.x,
            grounded.y,
            grounded.item_unique,
            grounded.list_index
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
        logln!(
            "game: ignoring invpickup of unique {} at handle {} (outside the legacy's range)",
            request.item_unique,
            request.item_index
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
                logln!("game: invpickup reported success without a slot or item");
                return Ok(());
            }
        },
        PickupStatus::NoSuchItem { handle } => {
            logln!("game: invpickup: no ground item has handle {handle}");
            return Ok(());
        }
        PickupStatus::ItemMismatch { stored, claimed } => {
            logln!("game: invpickup: handle names unique {stored}, not {claimed}");
            return Ok(());
        }
        PickupStatus::CellMismatch { stored, claimed } => {
            logln!(
                "game: invpickup: item is at ({}, {}, {}), client said ({}, {}, {})",
                stored.0,
                stored.1,
                stored.2,
                claimed.0,
                claimed.1,
                claimed.2
            );
            return Ok(());
        }
        PickupStatus::NoFreeSlot => {
            logln!("game: invpickup: every carry slot is occupied");
            return Ok(());
        }
    };

    logln!(
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
        logln!(
            "game: sent itemdrop 309 removal for map {} ({}, {})",
            cleared.map_index,
            cleared.x,
            cleared.y
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
        logln!("game: ignoring village-struct request for ref {village_ref}");
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
    logln!("game: sent village {village_ref} structure");
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
    if cmd == GameMsg::ResponseLetter as u16 {
        return Some("it is somebody else's letter: 116 is per-event, not world entry");
    }
    // The client arms `m_bEvent=true` on any monster-attack body
    // (`Client-MofuMofuEvent.cpp:91-96`); with no event running the honest
    // thing is to send nothing, exactly as the legacy's `IsEvent()` gate
    // (`Server-Person.cpp:2380`) does.
    if cmd == GameMsg::ResponseMonsterAttackStart as u16 {
        return Some("no monster-attack event is running; a body would arm one at village 0");
    }
    // Answered per-request by `handle_village_struct`; the captured burst copy
    // carries village 35's live structure table and would overwrite the fresh
    // answer.
    if cmd == GameMsg::ResponseVillagestruct as u16 {
        return Some(
            "generated on request (1101); the captured burst copy carries the reference village's structures",
        );
    }
    // The client walks the payload record-by-record with no length guard
    // (`Client-Login.cpp:60-82`); with nobody else on the map there are no
    // records, and there is no valid "empty list" body. The legacy omits the
    // frame entirely (`ServerMap.cpp:140`).
    if cmd == GameMsg::ResponseLoginmany as u16 {
        return Some("no other characters on the map; an empty body would be walked as garbage");
    }
    None
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
    logln!(
        "game: replaying {what}: {} frames, {total} payload bytes",
        frames.len()
    );
    for (cmd, body) in frames {
        if let Some(why) = withhold_always(cmd) {
            logln!("game: withholding {what} frame {cmd}: {why}");
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
            logln!("game: withholding {what} frame {cmd}: {why}");
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
    logln!(
        "game: generating world-entry prologue: {} frames, {total} payload bytes",
        sequence.len()
    );
    let _ = character_id; // 248/249 in the 230 burst; see `LiveFrames::regenerate`

    for (cmd, body) in sequence {
        send_raw(stream, cipher, cmd, &body).await?;
    }
    Ok(())
}

/// Replays `REPLY_PLAN`'s next step and advances the cursor, or does nothing if
/// `cmd` is not that step.
///
/// The plan is walked exactly once. It used to also re-replay an earlier step
/// when the client repeated its opcode, on the theory that
/// `REQUEST_CHANGESTATUSINFO` (230) was a "send me current state" re-sync — but
/// 230 is the horse auto-food message (`Server-Person.cpp:2831-2860`), and the
/// only step it ever drove is the ~65 KB world bulk. Re-sending that on every
/// 230 was the live micro-stutter T-018 fixes; later 230s are now dispatched by
/// the `230` arm as the legacy message. What remains in the plan is only the
/// one-shot world bulk.
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

    logln!("game: replay plan waiting for opcode {expect}, client sent {cmd}");
    Ok(())
}

/// Answers `GAMEMSG_REQUEST_CHANGESTATUSINFO` (230) with
/// `GAMEMSG_RESPONSE_CHANGESTATUSINFO` (231) for the one change type the legacy
/// handles, and stays silent for every other (`Server-Person.cpp:2831-2860`).
///
/// The client sends 230 for the horse auto-food toggle (`NCharHorse.cpp:471`,
/// `NCharStatus-Horse.cpp:232`) and applies the reply in
/// `Client-Person.cpp:1655-1680`. The legacy branches on exactly
/// `CHANGETYPE_AUTOFOOD` (`:2841`); its `default:` is a bare `return`
/// (`:2857-2858`), so an unknown change type is answered with nothing rather
/// than with a guess.
///
/// The response's `siCharUnique` is the session's character — the only one this
/// server has. The client drops a reply whose unique it cannot resolve
/// (`Client-Person.cpp:1659-1660`), so echoing the session's own unique is what
/// lets the toggle take effect.
///
/// # Errors
///
/// A malformed body ([`gz_proto::wire::WireError`]), a `char_unique` that does
/// not fit the `SI16` field, or a socket/cipher failure.
async fn handle_change_status_info(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    char_unique: i32,
    payload: &[u8],
) -> Result<(), AnyError> {
    let change = decode_change_status_info(payload)?;
    if change.change_type != CHANGETYPE_AUTOFOOD {
        logln!(
            "game: changestatusinfo type {} -> no reply (legacy answers only AUTOFOOD; \
             Server-Person.cpp:2841-2858)",
            change.change_type
        );
        return Ok(());
    }

    let own_unique = i16::try_from(char_unique).map_err(|_| {
        format!("session char_unique {char_unique} does not fit the SI16 siCharUnique")
    })?;
    // The legacy builds a fresh change block, so the amount slots go back out as
    // zero rather than echoing whatever the request carried (`:2851-2852`).
    let response = ChangeStatusInfo {
        change_type: CHANGETYPE_AUTOFOOD,
        change_switch: change.change_switch,
        amount1: 0,
        amount2: 0,
        amount3: 0,
    };
    let body = response.encode_response(own_unique);
    send_raw(
        stream,
        outbound,
        GameMsg::ResponseChangestatusinfo as u16,
        &body,
    )
    .await?;
    logln!(
        "game: changestatusinfo AUTOFOOD switch={} for char_unique {own_unique} ({} bytes)",
        change.change_switch,
        body.len()
    );
    Ok(())
}

/// Answers `GAMEMSG_REQUEST_PERSONALSHOPDATA` (7002) with
/// `GAMEMSG_RESPONSE_PERSONALSHOPDATA` (7003), which is what makes the client
/// create the personal-shop or manufacture dialog (`Client-PersonalShop.cpp:87-88`
/// and `:127-128`).
///
/// The legacy handler is `Server-PersonalShop.cpp:41-101`: it resolves the
/// target by `siCharUnique` (`:45`), and for a shop that is not open fills
/// `clShopData` from the character — mode from the request's flags (`:54-68`),
/// person id (`:70`) and account name (`:71`) — then sends the whole
/// `cltShopTradeData` back (`:97-99`).
///
/// This server knows exactly one character: the session's own. A request naming
/// any other `siCharUnique` has no data behind it, so it is logged rather than
/// answered with someone else's identity. In a world with no other avatars the
/// client can only ever name its own character anyway — the send site passes the
/// right-clicked character's id (`NRightMouseMenuDlg.cpp:349`), and the only
/// avatar it has is itself.
///
/// # Errors
///
/// A malformed body ([`gz_proto::wire::WireError`]) or a socket/cipher failure.
async fn handle_personal_shop_data(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    session: &GameSession,
    char_unique: i32,
    payload: &[u8],
) -> Result<(), AnyError> {
    let request = decode_personal_shop_request(payload)?;

    let Some(character_id) = session.character_id else {
        logln!(
            "game: personal shop request for char_unique {} before a character was selected; \
             not answering",
            request.char_unique
        );
        return Ok(());
    };
    let own_unique = i16::try_from(char_unique).map_err(|_| {
        format!("session char_unique {char_unique} does not fit the SI16 siCharUnique")
    })?;
    if request.char_unique != own_unique {
        logln!(
            "game: personal shop request for char_unique {} but this session is {own_unique}; \
             another character's shop is not modelled yet, not answering",
            request.char_unique
        );
        return Ok(());
    }

    let shop = ShopTradeData {
        shop_mode: shop_mode_for_request(request.manufacture, request.maker),
        person_id: character_id,
        account_id: session.username.clone(),
    };
    let body = shop.encode_response(request.char_unique);
    send_raw(
        stream,
        outbound,
        GameMsg::ResponsePersonalshopdata as u16,
        &body,
    )
    .await?;
    logln!(
        "game: personal shop reply for char_unique {}: mode {}, person_id {}, account {:?} \
         ({} bytes)",
        request.char_unique,
        shop.shop_mode,
        shop.person_id,
        shop.account_id,
        body.len()
    );
    Ok(())
}

/// A move in progress, stepped the way `cltCharCommon::Move` steps it
/// (`char-move.cpp:38-227`): every tick the accumulator grows by
/// `movespeed * elapsed / 50`, a tile is committed once it reaches 50, and each
/// committed tile is broadcast as a 200 `CHARUPDATE`. The legacy server keeps
/// one of these per character and calls it once per frame; the read loop's timer
/// arm is the frame here.
///
/// Modelling the step rather than teleporting matters for authority: an item
/// dropped mid-walk has to land on the tile the character is actually on, and
/// the captured reference session shows the server broadcasting the start cell
/// and then each stepped cell (`generated_frames.rs`'s 241 + 200 + 200 run).
struct Movement {
    /// Destination from the 240's `siPara1` (`x | (y << 16)`).
    dest: (i32, i32),
    /// The authoritative tile, advanced one step at a time.
    pos: (i32, i32),
    /// Map the move is on, for the drop cell.
    map_id: i32,
    /// The mover's effective `GetTotalMoveSpeed()` for the whole walk. Captured
    /// once when the order is answered, so gear changes mid-walk do not rescale
    /// an in-flight step.
    move_speed: i32,
    /// `clMoveInfo.siMovePercent`.
    percent: i32,
    /// When [`Movement::advance`] last ran, i.e. `clMoveInfo.dwLastMoveClock`.
    last: Instant,
}

impl Movement {
    fn new(dest: (i32, i32), pos: (i32, i32), map_id: i32, move_speed: i32) -> Self {
        Self {
            dest,
            pos,
            map_id,
            move_speed,
            percent: 0,
            last: Instant::now(),
        }
    }

    /// Advances the accumulator and commits at most one tile
    /// (`char-move.cpp:145-171`). The step is limited by
    /// [`MOVE_DELAY_CAP_MS`], so a stalled tick banks no more than one tile.
    /// Returns the new cell when a tile was committed.
    fn advance(&mut self, now: Instant) -> Option<(i32, i32)> {
        let cap = i64::from(MOVE_DELAY_CAP_MS);
        let delay = (now - self.last).as_millis().min(cap as u128) as i64;
        self.last = now;
        self.percent += (i64::from(self.move_speed) * delay / 50) as i32;
        if self.percent < MOVE_PERCENT_STEP {
            return None;
        }
        self.percent -= MOVE_PERCENT_COST;
        // One tile toward the destination; moving both axes at once is the
        // diagonal the legacy's `siNX++`/`siNY++` pair produces
        // (`char-move.cpp:107-126`).
        self.pos = (
            self.pos.0 + (self.dest.0 - self.pos.0).signum(),
            self.pos.1 + (self.dest.1 - self.pos.1).signum(),
        );
        Some(self.pos)
    }

    /// Whether the character has reached the destination.
    fn arrived(&self) -> bool {
        self.pos == self.dest
    }

    /// How long until the accumulator should next reach [`MOVE_PERCENT_STEP`],
    /// capped at [`MOVE_DELAY_CAP_MS`] exactly as one legacy tick is capped.
    fn next_delay(&self) -> Duration {
        let needed = i64::from((MOVE_PERCENT_STEP - self.percent).max(1));
        let speed = i64::from(self.move_speed).max(1);
        let ms = ((needed * 50 + speed - 1) / speed)
            .min(i64::from(MOVE_DELAY_CAP_MS))
            .max(1);
        Duration::from_millis(ms as u64)
    }
}

/// Sends one 200 `CHARUPDATE` for the given cell (`cltServer::SendCharUpdate`,
/// `Server.cpp:1654-1689`). Life, mana and move speed come from the cached row,
/// because stats are not modelled yet.
async fn send_char_update(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    live: &LiveFrames<'_>,
    x: i32,
    y: i32,
) -> Result<(), AnyError> {
    let character = live
        .character
        .as_ref()
        .ok_or("char update before the character row is loaded")?;
    let update = BasicCharUpdate {
        si_char_unique: i16::try_from(live.char_unique).map_err(|_| {
            format!(
                "session char_unique {} does not fit the SI16 siCharUnique",
                live.char_unique
            )
        })?,
        // The character's effective `GetTotalMoveSpeed()`; the client uses it for
        // the avatar's step animation rate.
        si_move_speed: i8::try_from(character.move_speed.clamp(0, i16::from(i8::MAX))).unwrap_or(0),
        b_urgent_mode: false,
        si_life: character.max_life,
        si_reserved_life: 0,
        si_mana: character.max_mana,
        si_x: i16::try_from(x).map_err(|_| format!("x {x} does not fit SI16"))?,
        si_y: i16::try_from(y).map_err(|_| format!("y {y} does not fit SI16"))?,
        ui_gm_mode: 0,
        b_pc_room: false,
        si_make_ani_type: 0,
    };
    send_encoded(
        stream,
        outbound,
        GameMsg::ResponseCharupdate,
        &update.encode(),
    )
    .await
}

/// Advances an in-flight move by one tick. When the tick commits a tile it sends
/// the 200 `CHARUPDATE` for the new cell, updates the session's drop cell and
/// the registry's copy of the position (so the near fan sees where the avatar
/// actually stands), and ends the move on arrival. A no-op when no move is
/// active or the tile is not due yet.
///
/// # Errors
///
/// A socket or cipher failure while sending the update.
async fn step_movement(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    live: &mut LiveFrames<'_>,
    login: &mut Option<GameSession>,
    movement: &mut Option<Movement>,
    registry: &Registry,
    session_key: SessionKey,
) -> Result<(), AnyError> {
    let Some(mv) = movement.as_mut() else {
        return Ok(());
    };
    let Some((x, y)) = mv.advance(Instant::now()) else {
        return Ok(());
    };
    let map_id = mv.map_id;
    if mv.arrived() {
        *movement = None;
    }
    send_char_update(stream, outbound, live, x, y).await?;
    // The stepped tile is now the authoritative cell: a 248 resync mid-walk and
    // a re-issued 240 both have to see it, not the destination the row holds.
    if let Some(character) = live.character.as_mut() {
        character.pos_x = x;
        character.pos_y = y;
    }
    if let Some(session) = login.as_mut() {
        session.entry_cell = Some(DropLocation { map_id, x, y });
    }
    // The registry tracks the *stepped* tile, not the destination the cached
    // row was set to when the order was answered: a head-chat fan has to reach
    // the cells the avatar is standing in, not the cell it is walking to.
    registry.update_pos(session_key, x, y);
    Ok(())
}

/// Applies `GAMEMSG_REQUEST_USERORDER` (240). The only order acted on is
/// `ORDER_MOVE`; every other order type is logged and dropped.
///
/// `siPara1` carries the destination as `x | (y << 16)`: the client packs it that
/// way (`cltOrder::MakeOrder`, `order.cpp:148`) and the `ORDER_MOVE` handler
/// unpacks it with `LOWORD`/`HIWORD` (`char-order.cpp:139-140`).
///
/// The reply mirrors the legacy order handler: a 241 order ack built the way
/// `MakeOrder_MOVE` builds it (`order.cpp:257-262`), then a 200 `CHARUPDATE` of
/// the cell the move starts from. The legacy then steps the character a tile at a
/// time and broadcasts the new cell on each step (`char-move.cpp:159-182`); the
/// returned [`Movement`] drives the same behaviour from the read loop, so the
/// avatar lands where it was sent and a mid-walk drop lands underfoot.
///
/// The destination is persisted to `characters.pos_x`/`pos_y` and mirrored into
/// the cached row up front, so a later 248/258 re-generates at the new cell and
/// a re-login resumes there even if the connection drops mid-walk.
///
/// Returns the move to drive; `None` for an order that is not modelled or a
/// click on the current cell.
///
/// # Errors
///
/// A malformed body, a database failure, or a socket/cipher failure.
async fn handle_userorder(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    characters: &PgCharacterRepository,
    live: &mut LiveFrames<'_>,
    order: OrderData,
) -> Result<Option<Movement>, AnyError> {
    if order.si_order != ORDER_MOVE {
        logln!(
            "game: ignoring USERORDER opcode {} (only ORDER_MOVE={ORDER_MOVE} is modelled)",
            order.si_order
        );
        return Ok(None);
    }

    let Some(character) = live.character.as_ref() else {
        return Err("userorder before the character row is loaded".into());
    };
    let character_id = character.character_id;
    let map_id = character.map_id;
    let start = (character.pos_x, character.pos_y);
    let move_speed = i32::from(character.move_speed);

    let (x16, y16) = OrderData::move_target(order.si_para1);
    let dest = (i32::from(x16), i32::from(y16));

    // The legacy answers the order in the handler itself (`char-order.cpp:169`);
    // the position reaches the database on a separate flush, so the client's
    // local walk is not held up by a round trip here.
    send_encoded(
        stream,
        outbound,
        GameMsg::ResponseUserorder,
        &OrderData::move_order(live.char_unique, x16, y16).encode(),
    )
    .await?;
    // The reference server broadcasts the cell the move starts from, then one
    // 200 per committed tile (`generated_frames.rs`: 241, 200, 200).
    send_char_update(stream, outbound, live, start.0, start.1).await?;

    // Persist the destination, then mirror: a later 248 (resync) or a re-login
    // reads the row. The stepped cell is tracked in memory for drops.
    characters
        .update_position(character_id, dest.0, dest.1)
        .await?;
    if let Some(character) = live.character.as_mut() {
        character.pos_x = dest.0;
        character.pos_y = dest.1;
    }

    logln!("game: move char {character_id} from {start:?} to {dest:?} on map {map_id}");

    if start == dest {
        return Ok(None);
    }
    Ok(Some(Movement::new(dest, start, map_id, move_speed)))
}

/// Persists the character and acknowledges a client leave request (107 quit or
/// 109 return-to-front).
///
/// The legacy handles both leave requests the same way:
/// `DoMsg_GAMEMSG_REQUEST_HOPETOQUIT` (`Server-System.cpp:1195`) and
/// `DoMsg_GAMEMSG_REQUEST_HOPETOFRONT` (`:1239`) guard against auto-hunt/PVP and
/// death, set `bHopeToQuitSwitch`, and answer with the matching empty 108/110
/// frame. The port has no auto-hunt, no PVP flag, and `life` never reaches zero,
/// so those guards cannot fire; what remains is the save and the ack.
///
/// The client does the rest: 108 makes it quit
/// (`Client-System.cpp:3002`) and 110 makes it close the game socket and return
/// to front (`:3025`). The handler deliberately does **not** close the session:
/// the client's own close is gated on the socket still being open (see the
/// caller), so the server has to leave it for the peer.
///
/// The character cell is written before the ack. Every `ORDER_MOVE` persists its
/// destination already, so this only matters when the character did not move
/// this session; it mirrors `SendDB_PosLifeMana` on the legacy
/// `cltCharServer::Close` path (`CharServer-Session.cpp:687`). No character is
/// selected only if the client leaves before `REQUEST_SELECTCHAR`, in which case
/// there is nothing to save and the ack is still sent.
///
/// # Errors
///
/// A database failure while saving, or a socket/cipher failure while sending.
async fn handle_leave_request(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    characters: &PgCharacterRepository,
    session: Option<&GameSession>,
    ack: GameMsg,
) -> Result<(), AnyError> {
    if let Some(session) = session
        && let Some(character_id) = session.character_id
        && let Some(cell) = session.entry_cell
    {
        characters
            .update_position(character_id, cell.x, cell.y)
            .await?;
    }
    send_encoded(stream, outbound, ack, &[]).await
}

// ---------------------------------------------------------------------------
// The chat family (T-018 slice 5): 117, 119, 121 and the 112 notice
// ---------------------------------------------------------------------------
//
// `gz_proto::chat` is the wire half; this is the routing half. It re-expresses
// `DoMsg_GAMEMSG_REQUEST_CHAT` (`Server-System.cpp:722-1192`),
// `DoMsg_GAMEMSG_REQUEST_SECRETCHAT` (`:527-600`) and
// `DoMsg_GAMEMSG_REQUEST_HEADCHAT` (`:693-720`) as functions over the
// registry: no I/O, no `await`, just lookups, encodes and queued frames. The
// read loop writes them, so a reply to this connection and a broadcast to
// everybody else travel the same queue in order.

/// Where one 117 request goes.
///
/// The order of the tests is the legacy's and is not interchangeable: the text
/// is checked for a leading `/` *before* the mode is looked at
/// (`pChatMsg[0] == TEXT('/')` at `:859` runs ahead of the mode chain at
/// `:1018-1184`), so `/t name` typed with some other mode selected is still a
/// whisper.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ChatRoute {
    /// `chatbuffer[0] == NULL` (`:762`): nothing to say, nothing sent.
    Ignore,
    /// `/t ` or `/T ` — the 117 whisper, answered with an 118 plus a 120.
    Whisper,
    /// `/p ` or `/P ` — a party invite. The legacy answers with a
    /// `SRVAL_PARTYINVITE_*` server response (`:912-969`) this port does not
    /// carry, so the request is dropped silently.
    Party,
    /// Any other leading `/`: a GM order. `OperateGameMasterOrder` (`:978`) has
    /// no orders modelled, so it always fails and the client always gets the
    /// 112 notice.
    Command,
    /// `CHATMODE_HEAD` — store the text and fan the 122 out.
    Head,
    /// `CHATMODE_HOME` — fan the 118 out over the home village.
    Home,
    /// A mode the legacy gates behind something this port does not carry
    /// (city-hall rank, party, guild, king, news ticket): it returns without
    /// sending anything (`:1018-1042`, `:1069-1138`), and so does this port.
    /// The mode rides along for the log line.
    Dropped(i16),
    /// The default branch (`:1139`) — and the one place this port deliberately
    /// differs from it; see [`handle_chat`].
    Broadcast,
}

/// The legacy's decision tree for one 117 request, with no side effects.
fn route_chat(text: &str, mode: i16) -> ChatRoute {
    let bytes = text.as_bytes();
    // The empty test comes first and is unconditional (`:762`): a request with
    // no text at all is not a command, not a mode, just nothing.
    if bytes.is_empty() {
        return ChatRoute::Ignore;
    }
    if bytes[0] == b'/' {
        // The legacy reads `pChatMsg[1]` and `pChatMsg[2]` as whole characters,
        // and a text too short to hold them lands on the zero-filled tail of
        // `chatbuffer` (`:726`) — so "/t" without a space is *not* a whisper.
        // The length guard reproduces that instead of indexing absent bytes.
        if bytes.len() >= 3 {
            match (bytes[1], bytes[2]) {
                (b't', b' ') | (b'T', b' ') => return ChatRoute::Whisper,
                (b'p', b' ') | (b'P', b' ') => return ChatRoute::Party,
                _ => {}
            }
        }
        return ChatRoute::Command;
    }
    match mode {
        CHATMODE_HEAD => ChatRoute::Head,
        CHATMODE_HOME => ChatRoute::Home,
        CHATMODE_TOWN | CHATMODE_VILLAGER | CHATMODE_PARTY | CHATMODE_GUILD | CHATMODE_KING
        | CHATMODE_NEWSITEM => ChatRoute::Dropped(mode),
        // Anything else is the default branch — including `CHATMODE_SECRET`
        // sent *without* a slash, which the legacy's mode chain at `:1018-1138`
        // does not catch either, so plain text under the whisper mode is
        // broadcast as ordinary chat there and here.
        _ => ChatRoute::Broadcast,
    }
}

/// The `/t` line split the way the legacy splits it.
///
/// `_stscanf(&pChatMsg[1], TEXT("%s %s"), code, szname)` (`:865`) takes the
/// second whitespace-separated token as the target, then forces
/// `szname[MAX_PLAYER_NAME - 1] = NULL` (`:867`) — at most 19 bytes. The
/// message is *not* tokenised: it is recomputed as
/// `&pChatMsg[1 + 1 + 1 + (_tcslen(szname) + 1)]` (`:999`), a fixed
/// `4 + target.len()` bytes from the start of the line. So extra whitespace, a
/// target over the cap, or no message at all all shift or miss exactly as that
/// arithmetic shifts and misses — which is what [`tail_at`] reproduces.
///
/// Returns `(target, message)`.
fn whisper_parts(text: &str) -> (String, String) {
    let mut tokens = text.split_whitespace();
    let _command = tokens.next();
    let mut target = tokens.next().unwrap_or("").to_owned();
    cut(&mut target, MAX_PLAYER_NAME - 1);
    let message = tail_at(text, 4 + target.len()).to_owned();
    (target, message)
}

/// `&text[index..]`, rounded down to a character boundary; `""` past the end.
///
/// The legacy indexes a `TCHAR` buffer that was zero-filled and only ever
/// written by `StringCchCopy` (`:726-728`), so a position past the terminator
/// reads zeros and yields an empty string rather than faulting. Rounding down
/// instead of panicking is this port's half of the same "it just reads nothing"
/// behaviour; the legacy would read a truncated multi-byte character here.
fn tail_at(text: &str, index: usize) -> &str {
    if index >= text.len() {
        return "";
    }
    let mut start = index;
    while start > 0 && !text.is_char_boundary(start) {
        start -= 1;
    }
    &text[start..]
}

/// Handles 117 `REQUEST_CHAT`.
///
/// The mode chain, the two slash commands that have a wire representation and
/// the GM-order fall-through all live in [`route_chat`]; what is decided here
/// is what each route *sends*.
///
/// **Deviation, the owner's instruction:** the default branch fans out to the
/// whole server instead of the near cells. The legacy sends to the 3×3 cell
/// neighbourhood (`SendNeighbourMsg(&clMsg, true)`, `:1149`); general chat is
/// deliberately global here, modern-server style, with every connected player —
/// the speaker included — receiving it. Whispers, head chat and the home-village
/// branch keep their legacy reach.
fn handle_chat(registry: &Registry, session_key: SessionKey, request: ChatRequest) {
    let Some(sender) = registry.info(session_key) else {
        // The legacy's `IsValidID(id)` guard (`:730`): a character that has not
        // been selected is not in the world and cannot speak.
        return;
    };
    match route_chat(&request.text, request.mode) {
        ChatRoute::Ignore => {}
        ChatRoute::Whisper => {
            let (target, message) = whisper_parts(&request.text);
            match registry.find_by_name(&target) {
                // `GetIDFromName` returned 0 (`:871`): the name resolves to
                // nobody, and the client is told so with
                // `SECRETCHAT_FAIL_INVALIDNAME` — the 120 ctor's `siCharUnique`
                // defaults to 0 (`MsgType-System.h:1011`).
                None => {
                    registry.send_to(
                        session_key,
                        GameMsg::ResponseSecretchatresult as u16,
                        encode_secret_chat_result(SECRETCHAT_FAIL_INVALIDNAME, &target, 0),
                    );
                }
                // The 118 goes to the target with `CHATMODE_SECRET`, level 0
                // and system text 0 — the ctor's defaults at `:1001` — then a
                // `SECRETCHAT_SUCCESS` ack carrying the *target's* name and
                // `siCharUnique` 0 goes to the speaker (`:1008`).
                Some(target_key) => {
                    registry.send_to(
                        target_key,
                        GameMsg::ResponseChat as u16,
                        encode_chat_response(
                            sender.char_unique,
                            &sender.name,
                            0,
                            CHATMODE_SECRET,
                            0,
                            &message,
                        ),
                    );
                    registry.send_to(
                        session_key,
                        GameMsg::ResponseSecretchatresult as u16,
                        encode_secret_chat_result(SECRETCHAT_SUCCESS, &target, 0),
                    );
                }
            }
        }
        ChatRoute::Party => {}
        ChatRoute::Command => {
            // `OperateGameMasterOrder` failed (`:978-984`) — it always does
            // here, no order is modelled — so the speaker gets the 34-byte
            // FAIL notice, and only the speaker.
            registry.send_to(
                session_key,
                GameMsg::ResponseNotice as u16,
                encode_notice("FAIL", "Master Order Error", true),
            );
        }
        ChatRoute::Head => {
            // `SetHeadChat(true, pChatMsg)` (`:1050`), whose `MStrCpy` caps the
            // stored text at 127 bytes (`char.cpp:6628`), then the 122 fan to
            // the near cells *including* the speaker
            // (`SendNeighbourMsg(&clMsg, true)`, `:1055`).
            //
            // The legacy's gate at `:1047` is "not in a personal shop"; that
            // switch is unmodelled and treated as always off, on both this side
            // and 121's.
            let mut text = request.text.clone();
            cut(&mut text, MAX_HEAD_CHAT_TEXT_SIZE - 1);
            registry.set_head_chat(session_key, Some(text.clone()));
            registry.broadcast_near(
                &sender,
                GameMsg::ResponseHeadchat as u16,
                encode_head_chat_response(sender.char_unique, &text),
            );
        }
        ChatRoute::Home => {
            // `if (pclchar->pclCI->clBI.siHomeVillage)` (`:1061`) then
            // `SendAllPersonHomeMsg(..., siHomeVillage)` (`:1066`), which
            // reaches every character of that home village *including* the
            // speaker (`CharManager.cpp:751`).
            if sender.school_id != 0 {
                registry.broadcast_school(
                    sender.school_id,
                    GameMsg::ResponseChat as u16,
                    encode_chat_response(
                        sender.char_unique,
                        &sender.name,
                        0,
                        request.mode,
                        0,
                        &request.text,
                    ),
                );
            }
        }
        ChatRoute::Dropped(mode) => {
            logln!(
                "game: {}: chat mode {mode} dropped (the legacy gates it behind a rank, \
                 party, guild, king or ticket this port does not model)",
                sender.name
            );
        }
        ChatRoute::Broadcast => {
            // The default branch: the head chat is cleared
            // (`SetHeadChat(false, NULL)`, `:1142`) and the message goes out
            // with the request's `siSystemTxt` echoed and level 0 (`:1144`).
            // The summon-emote side effect at `:1151-1181` is a documented
            // skip; summons are not modelled.
            registry.set_head_chat(session_key, None);
            registry.broadcast_all(
                GameMsg::ResponseChat as u16,
                encode_chat_response(
                    sender.char_unique,
                    &sender.name,
                    0,
                    request.mode,
                    request.system_txt,
                    &request.text,
                ),
            );
        }
    }
}

/// Handles 119 `REQUEST_SECRETCHAT` (`Server-System.cpp:527-600`).
///
/// Two lookups, deliberately independent: the target is resolved *by
/// `siCharUnique`* (`:551`) and *again by `szCharName`* (`:561`), and the
/// legacy never compares them — a message carrying Alice's id and Bob's name is
/// delivered to **Alice**, as long as both names exist. That quirk is locked
/// down by a test; nothing here "fixes" it.
///
/// A miss on either lookup answers `SECRETCHAT_FAIL_NOTUSER` with the literal
/// name `"NULL"` and the request's `siCharUnique` echoed (`:554`, `:564`). And
/// there is **no** success ack on this path: the `SECRETCHAT_SUCCESS` 120 is
/// sent only by the 117 `/t` branch (`:1008`).
fn handle_secret_chat(registry: &Registry, session_key: SessionKey, request: SecretChatRequest) {
    let Some(sender) = registry.info(session_key) else {
        return;
    };
    let not_user = encode_secret_chat_result(
        SECRETCHAT_FAIL_NOTUSER,
        "NULL",
        i32::from(request.char_unique),
    );
    let target = match registry.find_by_charunique(request.char_unique) {
        Some(target) => target,
        None => {
            registry.send_to(
                session_key,
                GameMsg::ResponseSecretchatresult as u16,
                not_user,
            );
            return;
        }
    };
    if registry.find_by_name(&request.name).is_none() {
        registry.send_to(
            session_key,
            GameMsg::ResponseSecretchatresult as u16,
            not_user,
        );
        return;
    }
    // The delivery goes to the *charunique* target, and it is the only path
    // that fills in 118's `siLevel`: `level = pclCM->CR[id]->pclCI->clIP.
    // GetLevel()` (`:591`), the sender's level.
    registry.send_to(
        target,
        GameMsg::ResponseChat as u16,
        encode_chat_response(
            sender.char_unique,
            &sender.name,
            sender.level,
            CHATMODE_SECRET,
            0,
            &request.text,
        ),
    );
}

/// Handles 121 `REQUEST_HEADCHAT` (`Server-System.cpp:693-720`).
///
/// The request names the **subject**, not the asker, and the reply goes to
/// whoever asked: it carries the subject's stored text and the subject's
/// `siCharUnique`. Nothing is sent when the subject is not online
/// (`GetIDFromCharUnique` fails, `:708`) or when its head-chat switch is off
/// (`:712`), which is `None` in the registry. The personal-shop gate at `:713`
/// is treated as always open, exactly as 117's is.
fn handle_head_chat_request(registry: &Registry, session_key: SessionKey, subject: i16) {
    let Some(subject_key) = registry.find_by_charunique(subject) else {
        return;
    };
    let Some(text) = registry.head_chat(subject_key) else {
        return;
    };
    registry.send_to(
        session_key,
        GameMsg::ResponseHeadchat as u16,
        encode_head_chat_response(subject, &text),
    );
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
        let is_guild_status = cmd == GameMsg::ResponseGuildSetguildstatus as u16;
        // Event/secondary state frames that ride the same preamble. Each is
        // either generated here with an honest "empty" body, or withheld via
        // `withhold_always`. 8451 is the exception: its two captured copies
        // stay verbatim (documented static blob, no session id in body).
        let is_space_box = cmd == GameMsg::ResponseSpaceBoxList as u16;
        let is_person_quest = cmd == GameMsg::ResponsePersonQuest as u16;
        let is_soulguard_get = cmd == GameMsg::ResponseSoulguardGet as u16;
        let is_soulguard_house_get = cmd == GameMsg::ResponseSoulguardHouseGet as u16;
        let is_marry_info = cmd == GameMsg::ResponseGetMarryInfo as u16;
        let is_elixir_set = cmd == GameMsg::ResponseElixirSet as u16;
        let is_enemy_capture_all = cmd == GameMsg::ResponseEnemycaptureall as u16;
        let is_item_mall_existence = cmd == GameMsg::ResponseItemmallexistence as u16;
        let is_servertime = cmd == GameMsg::ResponseServertime as u16;
        let is_zero_op_8464 = cmd == world_entry::OP_8464;
        let is_zero_op_8470 = cmd == world_entry::OP_8470;
        if !is_one
            && !is_two
            && !is_name
            && !is_horse_list
            && !is_change_horse
            && !is_skillbook
            && !is_family
            && !is_village
            && !is_guild_status
            && !is_space_box
            && !is_person_quest
            && !is_soulguard_get
            && !is_soulguard_house_get
            && !is_marry_info
            && !is_elixir_set
            && !is_enemy_capture_all
            && !is_item_mall_existence
            && !is_servertime
            && !is_zero_op_8464
            && !is_zero_op_8470
        {
            return Ok(None);
        }
        // 2926 is handled before the character lookup: the legacy sends it
        // unconditionally at login and its body for a character with no guild is
        // the same 76 bytes whatever the row says (`ServerDB-Person.cpp:3247-3258`).
        //
        // Replaying the captured copy here is what made VIT read 54 — see
        // `world_entry::guild_status`, which documents the record's `siVit` = 49
        // and the client branch that adds it. It also must not fall back to the
        // captured body if `character` were ever unset, because the fallback is
        // the bug.
        if is_guild_status {
            let char_unique = u16::try_from(self.char_unique).map_err(|_| {
                format!(
                    "session char_unique {} does not fit the u16 guild-status stamp",
                    self.char_unique
                )
            })?;
            return Ok(Some(world_entry::guild_status(char_unique)));
        }
        // The frames with no session-specific content: capture-length bodies,
        // honest zeros (all-empty) or the legacy's defaults (02 00 capacity).
        if is_space_box {
            return Ok(Some(world_entry::space_box_list_empty()));
        }
        if is_person_quest {
            return Ok(Some(world_entry::person_quest_empty()));
        }
        if is_soulguard_get {
            return Ok(Some(world_entry::soulguard_get_empty()));
        }
        if is_soulguard_house_get {
            return Ok(Some(world_entry::soulguard_house_get_default()));
        }
        if is_marry_info {
            return Ok(Some(world_entry::marry_info_empty()));
        }
        if is_enemy_capture_all {
            return Ok(Some(world_entry::enemy_capture_all_clear()));
        }
        if is_item_mall_existence {
            return Ok(Some(world_entry::item_mall_existence_absent()));
        }
        if is_zero_op_8464 {
            return Ok(Some(world_entry::zero_frame(
                world_entry::OP_8464,
                world_entry::OP_8464_LEN,
            )));
        }
        if is_zero_op_8470 {
            return Ok(Some(world_entry::zero_frame(
                world_entry::OP_8470,
                world_entry::OP_8470_LEN,
            )));
        }
        if is_elixir_set {
            let char_unique = u16::try_from(self.char_unique).map_err(|_| {
                format!(
                    "session char_unique {} does not fit the u32 elixir-id stamp",
                    self.char_unique
                )
            })?;
            return Ok(Some(world_entry::elixir_set_empty(char_unique)));
        }
        if is_servertime {
            // 177 rides the preamble twice (idx 62 in the prologue tail, idx 84
            // in the state sync). A frozen stamp is the one thing the client
            // already folds into its minute/day display; a fresh one costs it
            // nothing.
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            return Ok(Some(world_entry::encode_server_time(
                &world_entry::ServerTime::from_unix_seconds(now),
            )));
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
        ALL_CHAR_INFO_ONE_LEN, ALL_CHAR_INFO_TWO_LEN, BASIC_INFO_LEN, CHANGE_HORSE_LEN,
        CLT_HEALTH_INFO_LEN, CLT_HEALTH_INFO_WIRE_LEN, FAMILY_UPDATE_LEN, GET_VILLAGE_INFO_LEN,
        HEALTH_INFO_OFFSET, HEALTH_POINT_OFFSET, HEALTH_POINT_SEED, PERSON_HORSE_HEADER_LEN,
        PERSON_HORSE_LEN, RESPONSE_NAME_LEN, SKILLBOOK_MAGIC_INFO_LEN,
    };

    /// The regression this locks down is the 2026-10-07 live failure: the client
    /// reopened the character roster on the **same** connection (the exit
    /// dialog's character-select button sets `GS_INITCHARLIST` with no server
    /// message, so no reconnect happens) and sent `REQUEST_SELECTCHAR` again.
    /// The port silently dropped it, so picking another character never entered
    /// the world.
    ///
    /// The legacy refuses only the *same* character (`ServerDB-Person.cpp:1763`
    /// matches the PersonID and returns); a different slot is a real
    /// tear-down-and-reload (`:1770` `CloseAction`, `:1795` `DeleteChar`).
    #[test]
    fn selectchar_no_ops_the_same_slot_and_switches_a_different_one() {
        // First entry: nothing is bound, so any slot proceeds.
        assert!(admit_select_char(None, 0));
        assert!(admit_select_char(None, 2));
        // Same slot as the already-bound character: silently ignored.
        assert!(!admit_select_char(Some(1), 1));
        assert!(!admit_select_char(Some(0), 0));
        // A different slot is a real switch and must be honoured.
        assert!(admit_select_char(Some(1), 0));
        assert!(admit_select_char(Some(1), 2));
        // Out-of-range slots never reach the predicate; `handle_select_char`
        // rejects them.
    }

    /// The seven hundred range personal-shop opcodes are the legacy pair, and the
    /// reply body opens the dialog at the offsets the client reads
    /// (`Client-PersonalShop.cpp:94,117,131`).
    #[test]
    fn personal_shop_reply_opens_the_dialog() {
        use gz_proto::personalshop::{PSHOP_MODE_SELL, RESPONSE_PERSONALSHOPDATA_SIZE};

        assert_eq!(GameMsg::RequestPersonalshopdata as u16, 7002);
        assert_eq!(GameMsg::ResponsePersonalshopdata as u16, 7003);

        let shop = ShopTradeData {
            shop_mode: shop_mode_for_request(false, false),
            person_id: 7,
            account_id: "Tester".to_owned(),
        };
        let body = shop.encode_response(0x05BF);
        assert_eq!(body.len(), RESPONSE_PERSONALSHOPDATA_SIZE);
        assert_eq!(&body[0..2], &0x05BFu16.to_le_bytes());
        assert_eq!(body[8], PSHOP_MODE_SELL as u8);
        assert_eq!(&body[12..16], &7i32.to_le_bytes());
        assert_eq!(&body[16..22], b"Tester");
    }

    /// The request flags select the legacy's shop mode
    /// (`Server-PersonalShop.cpp:54-68`).
    #[test]
    fn personal_shop_request_flags_pick_the_legacy_mode() {
        use gz_proto::personalshop::{
            PSHOP_MODE_MAKEITEM, PSHOP_MODE_MAKEITEM_WANT, PSHOP_MODE_SELL,
        };
        // An ordinary "open the shop" click (both flags false) is a seller shop.
        assert_eq!(shop_mode_for_request(false, false), PSHOP_MODE_SELL);
        assert_eq!(shop_mode_for_request(true, false), PSHOP_MODE_MAKEITEM);
        assert_eq!(shop_mode_for_request(true, true), PSHOP_MODE_MAKEITEM_WANT);
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
        // byte the sender calls `uiHealthPoint` is written -- see
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
    /// [`PERSON_HORSE_HEADER_LEN`] the 226 frame independently pins.
    ///
    /// What runs *back* from `clHorseInfo` is [`CLT_HEALTH_INFO_WIRE_LEN`], not
    /// [`CLT_HEALTH_INFO_LEN`]. Those differ by four, and getting it wrong is what
    /// broke the status window: 7596 instead of 7592 put the seed at 7606, four
    /// bytes into the slot's tail where `New Era.exe` never looks, so writing 7606
    /// and then 7607 both left the live client reading `-100%`. The wire width is
    /// pinned by the client's own `cltHealthInfo::Set` at `0x0085B2A0`, which is a
    /// single `movups`/`movups` pair — sixteen bytes, one SSE store.
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
        let health = horse_info - CLT_HEALTH_INFO_WIRE_LEN;
        assert_eq!(health, HEALTH_INFO_OFFSET, "clHealthInfo starts here");
        assert_eq!(HEALTH_INFO_OFFSET, 7_592);
        assert_eq!(HEALTH_POINT_OFFSET, 7_602);

        // `uiHealthPoint` is the tenth byte of `clHealthInfo` (`CommonHeader.h:2898`),
        // which the client's `SetHealthAddAbility` at `0x0084C380` reads as
        // `movzx eax,byte ptr [ecx+0Ah]`.
        assert_eq!(HEALTH_POINT_OFFSET - HEALTH_INFO_OFFSET, 10);
        // And the capture states the reference character's own value, non-zero. That
        // is a second, independent vote: if the base were really 7596 this byte
        // would be 0 and the reference session would have shown `-100%` too.
        assert_eq!(
            captured[HEALTH_POINT_OFFSET], 110,
            "the reference character's wellness rate is +10"
        );

        // The captured character's wellness state, so the sweep below has something
        // to be zero against. `siHungry` 14 is a played character running low;
        // every organ and disease byte is clean.
        assert_eq!(
            i16::from_le_bytes([captured[health], captured[health + 1]]),
            14,
            "siHungry"
        );
        assert_eq!(
            &captured[health + 2..health + 6],
            &[0, 0, 0, 0],
            "siInnerOrg1..4"
        );
        assert_eq!(
            &captured[health + 6..health + 10],
            &[0, 0, 0, 0],
            "uiOrg1..4Disease"
        );
        // The four bytes the client's 16-byte `Set` copies but this source does not
        // declare. The capture leaves them zero, so we do too.
        assert_eq!(
            &captured[HEALTH_INFO_OFFSET + CLT_HEALTH_INFO_LEN..horse_info],
            &[0u8; 4],
            "the client's four extra clHealthInfo bytes"
        );

        // And what we now send. `cltHealthInfo::Init()` seeds `uiHealthPoint` at 100
        // (`Common.cpp:3253`), and 100 is the neutral value: it makes every
        // `siHealth*Rate` zero (`Parametabox.cpp:7727`), so `clHealthAdd` adds
        // nothing and the rest of `clTotalBA` supplies the real numbers.
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
        assert_eq!(HEALTH_POINT_SEED, 100);
        assert_eq!(generated[HEALTH_POINT_OFFSET], HEALTH_POINT_SEED);
        assert_eq!(
            &generated[HEALTH_INFO_OFFSET..HEALTH_POINT_OFFSET],
            &[0u8; 10],
            "siHungry, siInnerOrg1..4 and uiOrg1..4Disease stay zero, as the sender \
             writes them"
        );
    }

    /// Regression guard for the 2026-10-06 client crash.
    ///
    /// A probe ramp was written over body offsets 7,424..7,680 to make the client
    /// report which byte it reads as `clHealthInfo.uiHealthPoint`. It killed
    /// `New Era.exe` 1.1.1.3 on login: Application Error event 1000,
    /// `0xc0000005` at `0x003caf30`.
    ///
    /// That offset is an RVA, so the fault site is `0x00400000 + 0x3CAF30 =
    /// 0x007CAF30`, and the client's own code there is a **null dereference from an
    /// out-of-range table index**: it reads a word at `[ecx+0x37FA]`, bounds it
    /// against 4, and only dereferences when the lookup succeeds *and* returns a
    /// type-`0x11` entry. The ramp planted a value above 4.
    ///
    /// This does not re-run the probe. It asserts the *shape* of what we send, so
    /// that no future attempt to make the unmodelled band observable does it by
    /// writing a run of distinct values into it.
    #[test]
    fn the_unmodelled_band_carries_no_probe_ramp() {
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

        // `clHealthInfo` plus `clHorseInfo`'s 28-byte header is where the ramp
        // killed the client. Everything from the byte after `uiHealthPoint` to the
        // end of the horse array must stay zero: the client's fault site is a
        // bounded table index, and this band holds the values it reads.
        let horse_band = &generated[HEALTH_POINT_OFFSET + 1..HORSE_ARRAY_END];
        assert!(
            horse_band.iter().all(|b| *b == 0),
            "the horse band must stay zero: `New Era.exe` indexes a table by the word \
             at +0x37FA and null-derefs on anything above 4, and cltHorse::szName is a \
             fixed 12 bytes with no terminator slot. First non-zero at body offset {:?}",
            horse_band
                .iter()
                .position(|b| *b != 0)
                .map(|p| p + HEALTH_POINT_OFFSET + 1)
        );

        // And nothing anywhere in the band may form a long ascending run, which is
        // the ramp's fingerprint. `window(3)` over the whole unmodelled region
        // catches any future attempt to encode an index into it.
        let unmodelled = &generated[BASIC_INFO_LEN..HORSE_ARRAY_END];
        assert!(
            !unmodelled
                .windows(8)
                .any(|w| { w.windows(2).all(|p| p[1] == p[0].wrapping_add(1)) }),
            "an 8-byte ascending run in the unmodelled band is a probe ramp; it \
             crashed the client on 2026-10-06"
        );
    }

    /// `clHorse[0]` ends the region the ramp must never reach. Derived from the
    /// capture rather than hardcoded: `clHorse[0]` starts 28 bytes after
    /// `clHorseInfo`, holds `MAX_HORSE_NAME` 12 and is 108 bytes, and there are 5
    /// of them (frame 226 is exactly `28 + 5 * 108`).
    ///
    /// This must come out at **8176** (`0x1FF0`), which is the next source offset
    /// in the client's own `cltPersonInfo::SetOne` copy table. If it ever stops
    /// agreeing with that literal, the offsets have drifted apart again.
    const HORSE_ARRAY_END: usize =
        HEALTH_INFO_OFFSET + CLT_HEALTH_INFO_WIRE_LEN + PERSON_HORSE_HEADER_LEN + 5 * 108;

    /// The offset chain the status-window fix rests on, asserted as arithmetic so a
    /// change to any link is a test failure rather than a live regression.
    ///
    /// Every number here is independently observable: `0x1DA8` and `0x1DB8` are
    /// literals in the client's `SetOne` (`New Era.exe` 1.1.1.3, VA `0x00461AC1`
    /// and `0x00461AD9`), `0x1FF0` is the next one, and 28 + 5 * 108 is the horse
    /// array the capture and frame 226 agree on.
    #[test]
    fn the_client_layout_chain_agrees_with_the_horse_array() {
        assert_eq!(HEALTH_INFO_OFFSET, 0x1DA8);
        assert_eq!(HEALTH_POINT_OFFSET, 0x1DA8 + 10);
        assert_eq!(HEALTH_INFO_OFFSET + CLT_HEALTH_INFO_WIRE_LEN, 0x1DB8);
        assert_eq!(HORSE_ARRAY_END, 0x1FF0);
        assert_eq!(PERSON_HORSE_HEADER_LEN, 28);
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

    /// T-018: the plan's first entry is the one-shot world bulk on 230.
    ///
    /// The `230` arm only calls [`replay_plan_step`] while `plan_step == 0`, so
    /// that entry has to be the bulk. If the plan ever grew a leading entry for
    /// another opcode, the world would silently stop loading.
    #[test]
    fn the_first_plan_entry_is_the_230_world_bulk() {
        let (cmd, _) = generated_capture::REPLY_PLAN
            .first()
            .expect("the plan is not empty");
        assert_eq!(*cmd, GameMsg::RequestChangestatusinfo as u16);
    }

    /// T-018: a repeated 230 must not become a second world bulk.
    ///
    /// The dispatcher answers the first `REQUEST_CHANGESTATUSINFO` (230) from
    /// the plan and every later one as the legacy status message. That is only
    /// complete while 230 appears exactly once in the plan -- a second entry
    /// would make the 65 KB burst reachable again, which is the live
    /// micro-stutter this change removed.
    #[test]
    fn opcode_230_appears_once_and_is_the_twenty_five_frame_bulk() {
        let entries: Vec<&(u16, &[(u32, u32)])> = generated_capture::REPLY_PLAN
            .iter()
            .filter(|(cmd, _)| *cmd == GameMsg::RequestChangestatusinfo as u16)
            .collect();
        assert_eq!(entries.len(), 1, "230 must have exactly one plan entry");
        let frames: usize = entries[0]
            .1
            .iter()
            .map(|&(start, end)| generated_capture::frames_in(start, end).count())
            .sum();
        assert_eq!(frames, 25, "the one 230 entry is the 25-frame world bulk");
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
                GameMsg::ResponseMonsterAttackStart as u16,
                GameMsg::ResponseLoginmany as u16,
                GameMsg::ResponseVillagestruct as u16,
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
        assert_eq!(kept.len(), sent.len() - 5);
        // Only the two event-specific frames and the letter had to move; the
        // rest are now generated, so being sent is unchanged.
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

    /// Pins the new zero/empty bodies to the exact capture sizes. These
    /// frames replaced a reference-character payload with a passive "nothing
    /// here" body; a size mismatch would desync the client's fixed-stride
    /// walk like the quest/spacebox decoders do.
    #[test]
    fn the_zero_state_frames_match_the_capture_lengths() {
        let cases: &[(u16, Vec<u8>)] = &[
            (
                GameMsg::ResponseSpaceBoxList as u16,
                world_entry::space_box_list_empty(),
            ),
            (
                GameMsg::ResponsePersonQuest as u16,
                world_entry::person_quest_empty(),
            ),
            (
                GameMsg::ResponseSoulguardGet as u16,
                world_entry::soulguard_get_empty(),
            ),
            (
                GameMsg::ResponseSoulguardHouseGet as u16,
                world_entry::soulguard_house_get_default(),
            ),
            (
                GameMsg::ResponseGetMarryInfo as u16,
                world_entry::marry_info_empty(),
            ),
            (
                GameMsg::ResponseEnemycaptureall as u16,
                world_entry::enemy_capture_all_clear(),
            ),
            (
                GameMsg::ResponseItemmallexistence as u16,
                world_entry::item_mall_existence_absent(),
            ),
            (
                world_entry::OP_8464,
                world_entry::zero_frame(world_entry::OP_8464, world_entry::OP_8464_LEN),
            ),
            (
                world_entry::OP_8470,
                world_entry::zero_frame(world_entry::OP_8470, world_entry::OP_8470_LEN),
            ),
        ];
        for (cmd, body) in cases {
            let captured = generated_capture::frame(*cmd)
                .unwrap_or_else(|| panic!("capture must contain opcode {cmd}"));
            assert_eq!(body.len(), captured.len(), "opcode {cmd}");
        }
        // 8286's only non-zero is the capacity default.
        let house = world_entry::soulguard_house_get_default();
        assert_eq!(&house[0..2], &[0x02, 0x00], "default capacity 2");
        assert!(house[2..].iter().all(|&b| b == 0));
        // 8178 needs the session id to start at offset 0.
        let elixir = world_entry::elixir_set_empty(0x05BF);
        assert_eq!(&elixir[0..4], &[0xBF, 0x05, 0x00, 0x00]);
        assert!(elixir[4..].iter().all(|&b| b == 0));
        let cap8178 = generated_capture::frame(GameMsg::ResponseElixirSet as u16).unwrap();
        assert_eq!(elixir.len(), cap8178.len());
    }

    /// The 230 burst must not fall through to a verbatim replay of someone
    /// else's payload. Every opcode in range `66..91` is either generated
    /// (via `regenerate`), withheld, or one of the two documented static
    /// blobs (8451 twice, 1208).
    #[test]
    fn every_state_sync_frame_is_generated_withheld_or_a_static_blob() {
        // Already regenerated or withheld by the earlier slices.
        let regenerated: &[u16] = &[
            GameMsg::ResponseAllcharinfoone as u16,
            GameMsg::ResponseAllcharinfotwo as u16,
            GameMsg::ResponseName as u16,
            GameMsg::ResponsePersonHorse as u16,
            GameMsg::ResponseSkillbookMagicinfoGet as u16,
            GameMsg::ResponseFamilyUpdate as u16,
            GameMsg::ResponseGetvillageinfo as u16,
            GameMsg::ResponseSpaceBoxList as u16,
            GameMsg::ResponsePersonQuest as u16,
            GameMsg::ResponseSoulguardGet as u16,
            GameMsg::ResponseSoulguardHouseGet as u16,
            GameMsg::ResponseGetMarryInfo as u16,
            GameMsg::ResponseElixirSet as u16,
            GameMsg::ResponseEnemycaptureall as u16,
            GameMsg::ResponseItemmallexistence as u16,
            GameMsg::ResponseServertime as u16,
            GameMsg::ResponseGuildSetguildstatus as u16,
            world_entry::OP_8464,
            world_entry::OP_8470,
        ];
        // Kept verbatim as documented static data: 8451 appears twice in the
        // burst and carries aSeason-1 UI blob only; 1208's 4-byte packed
        // `cltDate` is frozen but harmless (the client renders it in the
        // upper bar; the server's game clock is not wired to a real timebase).
        let static_blobs: &[u16] = &[8451, GameMsg::ResponseDate as u16];
        for (cmd, _, _) in crate::generated_frames::CAP_FRAMES[66..=90].iter() {
            if withhold_always(*cmd).is_some() {
                continue;
            }
            let ok = regenerated.contains(cmd) || static_blobs.contains(cmd);
            assert!(
                ok,
                "opcode {cmd} in the state-sync burst is neither generated, withheld, nor a documented static blob"
            );
        }
    }

    /// A move commits one tile at a time along the direction of the destination
    /// and stops there, at the cadence `char-move.cpp` gives: 100 accumulator
    /// units per tile at `movespeed` units per 50 ms, i.e. `5000 / movespeed`
    /// ms per tile (≈357 ms for the captured speed 14).
    #[test]
    fn a_move_steps_one_tile_at_a_time_on_the_legacy_cadence() {
        let mut movement = Movement::new((3, 0), (0, 0), 7, 14);
        let start = movement.last;
        let mut now = start;

        let mut steps = Vec::new();
        let mut intervals = Vec::new();
        let mut previous = start;
        while !movement.arrived() {
            now += movement.next_delay();
            if let Some(pos) = movement.advance(now) {
                steps.push(pos);
                intervals.push(now - previous);
                previous = now;
            }
            assert!(now - start < Duration::from_secs(5), "move never arrived");
        }

        assert_eq!(steps, vec![(1, 0), (2, 0), (3, 0)]);
        // After the first (cold-start) tile the cadence settles at one tile per
        // ~357 ms; the cap only reshapes a delayed tick, never the average.
        for interval in &intervals[1..] {
            assert!(
                *interval >= Duration::from_millis(340) && *interval <= Duration::from_millis(360),
                "tile interval {interval:?} is off the legacy cadence"
            );
        }
    }

    /// A diagonal click steps both axes at once, the `siNX++/siNY++` pair at
    /// `char-move.cpp:107-126`.
    #[test]
    fn a_diagonal_move_steps_both_axes() {
        let mut movement = Movement::new((2, 2), (0, 0), 1, 14);
        let mut now = movement.last;
        let mut steps = Vec::new();
        while !movement.arrived() {
            now += movement.next_delay();
            if let Some(pos) = movement.advance(now) {
                steps.push(pos);
            }
        }
        assert_eq!(steps, vec![(1, 1), (2, 2)]);
    }

    /// A stalled tick must not bank more than one tile: `advance` caps its
    /// elapsed time at `(CHAR_ACTION_INTERVAL_FRAME + 1) * 50` ms
    /// (`char-move.cpp:145`).
    #[test]
    fn a_stalled_tick_commits_at_most_one_tile() {
        let mut movement = Movement::new((10, 0), (0, 0), 1, 14);
        let stalled = movement.last + Duration::from_secs(4);
        assert_eq!(movement.advance(stalled), Some((1, 0)));
        assert_eq!(movement.pos, (1, 0));
    }

    /// `next_delay` never exceeds the legacy cap, so the read loop's timer arm
    /// wakes at the same bound the legacy's `min((CHAR_ACTION_INTERVAL_FRAME+1)*50, …)`
    /// applies.
    #[test]
    fn next_delay_is_capped_at_the_legacy_tick_bound() {
        let mut movement = Movement::new((10, 0), (0, 0), 1, 14);
        // Commit one tile so the accumulator sits below zero and would ask for
        // more than the cap without it.
        let now = movement.last + Duration::from_millis(179);
        assert!(movement.advance(now).is_some());
        assert_eq!(movement.next_delay(), Duration::from_millis(350));
    }

    /// The cadence follows the character, not a fixed reference: `advance` banks
    /// `move_speed * elapsed / 50`, so a slower mover takes proportionally longer
    /// per tile. At speed 7 a tile needs `5000 / 7 ≈ 715` ms against 357 ms at
    /// the captured 14.
    #[test]
    fn the_step_cadence_scales_with_the_characters_move_speed() {
        let mut slow = Movement::new((1, 0), (0, 0), 1, 7);
        let start = slow.last;
        // Too early for a speed-7 tile: half the accumulator still falls short.
        assert!(slow.advance(start + Duration::from_millis(357)).is_none());
        // A full 715 ms commits exactly one.
        assert_eq!(
            slow.advance(start + Duration::from_millis(715)),
            Some((1, 0))
        );

        let mut fast = Movement::new((1, 0), (0, 0), 1, 28);
        let start = fast.last;
        // The same 179 ms that just missed at speed 7 commits a tile at 28
        // (28 * 179 / 50 ≈ 100), and the next is due well under the cap.
        assert_eq!(
            fast.advance(start + Duration::from_millis(179)),
            Some((1, 0))
        );
        assert!(fast.next_delay() < Duration::from_millis(350));
    }

    /// The quit menu's two requests are the legacy pair, and each is answered by
    /// the next opcode up with an empty body: 107 `REQUEST_HOPETOQUIT` → 108
    /// `RESPONSE_HOPETOQUIT`, 109 `REQUEST_HOPETOFRONT` → 110
    /// `RESPONSE_HOPETOFRONT` (`Server-System.cpp:1195`, `:1239`). Pinning the
    /// numbers keeps the dispatcher's `c == … as u16` guards honest even if the
    /// codegen ever renumbers the enum.
    #[test]
    fn leave_requests_are_the_legacy_107_109_pair() {
        assert_eq!(GameMsg::RequestHopetoquit as u16, 107);
        assert_eq!(GameMsg::ResponseHopetoquit as u16, 108);
        assert_eq!(GameMsg::RequestHopetofront as u16, 109);
        assert_eq!(GameMsg::ResponseHopetofront as u16, 110);
    }

    // ------------------------------------------------------------------ chat
    //
    // T-018 slice 5: the routing half of the chat family. The wire half is
    // `gz_proto::chat`'s own tests; these pin the opcodes, the route order and
    // the frames each handler actually queues.

    /// A character with the defaults a chat test cares about.
    fn chat_info(char_unique: i16, name: &str, x: i32, y: i32) -> SessionInfo {
        SessionInfo {
            char_unique,
            name: name.to_owned(),
            level: 34,
            map_id: 7,
            x,
            y,
            school_id: 3,
        }
    }

    /// Registers a connection and hands back its key and inbox.
    fn chat_join(
        registry: &Registry,
        info: SessionInfo,
    ) -> (SessionKey, tokio::sync::mpsc::UnboundedReceiver<Outbound>) {
        let key = Registry::next_key();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        registry.register(key, tx, info);
        (key, rx)
    }

    /// Nothing queued, as distinct from "the sender is gone".
    fn assert_chat_silent(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Outbound>, what: &str) {
        assert!(
            matches!(
                rx.try_recv(),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            ),
            "{what}: expected no frame"
        );
    }

    /// The chat family's opcodes are the legacy's, and the dispatcher's
    /// `c == … as u16` guards depend on them: 117 `REQUEST_CHAT`, 118
    /// `RESPONSE_CHAT`, 119 `REQUEST_SECRETCHAT`, 120
    /// `RESPONSE_SECRETCHATRESULT`, 121 `REQUEST_HEADCHAT`, 122
    /// `RESPONSE_HEADCHAT`, 112 `RESPONSE_NOTICE`
    /// (`MsgType-System.h:961-1160`, `MsgType.h:3256`).
    #[test]
    fn chat_opcodes_are_the_legacy_pair_plus_the_notice() {
        assert_eq!(GameMsg::RequestChat as u16, 117);
        assert_eq!(GameMsg::ResponseChat as u16, 118);
        assert_eq!(GameMsg::RequestSecretchat as u16, 119);
        assert_eq!(GameMsg::ResponseSecretchatresult as u16, 120);
        assert_eq!(GameMsg::RequestHeadchat as u16, 121);
        assert_eq!(GameMsg::ResponseHeadchat as u16, 122);
        assert_eq!(GameMsg::ResponseNotice as u16, 112);
    }

    /// The route decision reproduces the legacy's order, which is not
    /// interchangeable: the leading `/` is tested *before* the mode
    /// (`pChatMsg[0] == TEXT('/')` at `Server-System.cpp:859` runs ahead of the
    /// mode chain at `:1018-1184`), a text too short to hold the command's two
    /// characters is not a command at all, and the modes the legacy gates are
    /// dropped rather than broadcast.
    #[test]
    fn route_chat_tests_the_slash_before_the_mode_and_drops_the_gated_modes() {
        use gz_proto::chat::{
            CHATMODE_ASK, CHATMODE_GUILD, CHATMODE_HEAD, CHATMODE_HOME, CHATMODE_KING,
            CHATMODE_NEWSITEM, CHATMODE_NORMAL, CHATMODE_PARTY, CHATMODE_SECRET, CHATMODE_TOWN,
            CHATMODE_VILLAGER,
        };

        // Empty text is nothing, whatever the mode.
        assert_eq!(route_chat("", CHATMODE_NORMAL), ChatRoute::Ignore);
        assert_eq!(route_chat("", CHATMODE_HEAD), ChatRoute::Ignore);

        // The slash wins over the mode: `/t ` under the head-chat mode is still
        // a whisper.
        assert_eq!(route_chat("/t Bob hi", CHATMODE_HEAD), ChatRoute::Whisper);
        assert_eq!(route_chat("/T Bob hi", CHATMODE_NORMAL), ChatRoute::Whisper);
        assert_eq!(route_chat("/p Bob", CHATMODE_NORMAL), ChatRoute::Party);
        assert_eq!(route_chat("/P Bob", CHATMODE_NORMAL), ChatRoute::Party);

        // "/t" without the space reads the zero-filled tail of the legacy's
        // `chatbuffer` (`:726`), so it is not a whisper — and neither is a
        // single "/".
        assert_eq!(route_chat("/t", CHATMODE_NORMAL), ChatRoute::Command);
        assert_eq!(route_chat("/", CHATMODE_NORMAL), ChatRoute::Command);
        assert_eq!(route_chat("/x", CHATMODE_NORMAL), ChatRoute::Command);

        // The gated modes drop, whatever the text.
        for mode in [
            CHATMODE_TOWN,
            CHATMODE_VILLAGER,
            CHATMODE_PARTY,
            CHATMODE_GUILD,
            CHATMODE_KING,
            CHATMODE_NEWSITEM,
        ] {
            assert_eq!(route_chat("hello", mode), ChatRoute::Dropped(mode));
        }

        // Head and home route by mode.
        assert_eq!(route_chat("hello", CHATMODE_HEAD), ChatRoute::Head);
        assert_eq!(route_chat("hello", CHATMODE_HOME), ChatRoute::Home);

        // `CHATMODE_SECRET` *without* a slash falls through the legacy's mode
        // chain (`:1018-1138`) exactly as it falls through here: plain text
        // under the whisper mode is ordinary chat.
        assert_eq!(route_chat("hello", CHATMODE_SECRET), ChatRoute::Broadcast);
        assert_eq!(route_chat("hello", CHATMODE_ASK), ChatRoute::Broadcast);
        assert_eq!(route_chat("hello", CHATMODE_NORMAL), ChatRoute::Broadcast);
    }

    /// The `/t` split reproduces the legacy's `_stscanf` + fixed-offset
    /// arithmetic (`:865`, `:999`): the target is the second whitespace token
    /// capped at 19 bytes, and the message starts `4 + target.len()` bytes into
    /// the line — so extra whitespace, an over-cap target and no message at all
    /// all shift or miss exactly as that arithmetic does.
    #[test]
    fn whisper_parts_reproduces_the_legacy_offset_arithmetic() {
        // "/t Target" — the message starts past the end of the line.
        let (target, message) = whisper_parts("/t Target");
        assert_eq!(target, "Target");
        assert_eq!(message, "", "the fixed offset runs off the end");

        // The message begins right after the target and one space, wherever the
        // whitespace actually was.
        let (target, message) = whisper_parts("/t  Bob   hello there");
        assert_eq!(target, "Bob");
        assert_eq!(message, "   hello there", "offset 4 + len, not a tokeniser");

        // A target over the cap is cut, and the offset follows the *cut* name.
        let long = "abcdefghijklmnopqrs"; // 19 bytes, the cap
        let (target, message) = whisper_parts(&format!("/t {long} hi"));
        assert_eq!(target, long);
        assert_eq!(message, "hi");

        let over = format!("/t {}X hi", "a".repeat(30));
        let (target, message) = whisper_parts(&over);
        assert_eq!(target.len(), 19, "the cap");
        assert_eq!(message, "aaaaaaaaaaX hi", "the offset follows the cut name");

        // No target at all: the legacy's `szname` stays empty and the message
        // starts at byte 4.
        let (target, message) = whisper_parts("/t ");
        assert_eq!(target, "");
        assert_eq!(message, "", "nothing after the command");

        // A multi-byte target is cut on a character boundary, and the offset
        // still follows the cut length.
        let (target, message) = whisper_parts("/t éééééééééé hi");
        assert_eq!(target, "é".repeat(9), "19 bytes of whole characters");
        assert_eq!(message, "é hi");
    }

    /// General chat is server-wide (the owner's instruction): every connected
    /// player — the speaker included — gets the 118, with the request's
    /// `siSystemTxt` echoed and level 0 (`:1144`).
    #[test]
    fn general_chat_reaches_the_whole_server_including_the_speaker() {
        use gz_proto::chat::CHATMODE_NORMAL;

        let registry = Registry::new();
        let (alice, mut ra) = chat_join(&registry, chat_info(11, "Alice", 0, 0));
        let (_bob, mut rb) = chat_join(&registry, chat_info(12, "Bob", 500, 500));
        let (_carol, mut rc) = chat_join(&registry, chat_info(13, "Carol", 0, 0));

        handle_chat(
            &registry,
            alice,
            ChatRequest {
                mode: CHATMODE_NORMAL,
                system_txt: 7,
                text: "hello world".to_owned(),
            },
        );

        for (rx, who) in [
            (&mut ra, "the speaker"),
            (&mut rb, "Bob"),
            (&mut rc, "Carol"),
        ] {
            let frame = rx
                .try_recv()
                .unwrap_or_else(|_| panic!("{who} gets the frame"));
            assert_eq!(frame.cmd, GameMsg::ResponseChat as u16);
            let body = &frame.payload;
            assert_eq!(i16::from_le_bytes([body[2], body[3]]), 11, "the sender");
            assert_eq!(i16::from_le_bytes([body[6], body[7]]), CHATMODE_NORMAL);
            assert_eq!(
                i32::from_le_bytes(body[12..16].try_into().unwrap()),
                7,
                "siSystemTxt echoed"
            );
            assert_eq!(
                i32::from_le_bytes(body[16..20].try_into().unwrap()),
                0,
                "level 0"
            );
            assert_eq!(&body[20..25], b"Alice");
            assert_eq!(&body[25..], b"hello world\0");
        }
    }

    /// A `/t` whisper delivers the 118 to the target with `CHATMODE_SECRET`,
    /// level 0 and system text 0, then a `SECRETCHAT_SUCCESS` 120 carrying the
    /// target's name to the speaker (`:1001-1008`).
    #[test]
    fn a_whisper_reaches_the_target_and_acks_the_speaker() {
        let registry = Registry::new();
        let (alice, mut ra) = chat_join(&registry, chat_info(11, "Alice", 0, 0));
        let (_bob, mut rb) = chat_join(&registry, chat_info(12, "Bob", 0, 0));

        handle_chat(
            &registry,
            alice,
            ChatRequest {
                mode: CHATMODE_SECRET,
                system_txt: 0,
                text: "/t Bob secret message".to_owned(),
            },
        );

        // The target gets the 118.
        let frame = rb.try_recv().expect("Bob gets the whisper");
        assert_eq!(frame.cmd, GameMsg::ResponseChat as u16);
        let body = &frame.payload;
        assert_eq!(i16::from_le_bytes([body[2], body[3]]), 11, "the sender");
        assert_eq!(i16::from_le_bytes([body[6], body[7]]), CHATMODE_SECRET);
        assert_eq!(
            i32::from_le_bytes(body[16..20].try_into().unwrap()),
            0,
            "level 0"
        );
        assert_eq!(&body[20..25], b"Alice");
        assert_eq!(&body[25..], b"secret message\0");

        // The speaker gets the 120 ack.
        let frame = ra.try_recv().expect("Alice gets the ack");
        assert_eq!(frame.cmd, GameMsg::ResponseSecretchatresult as u16);
        assert_eq!(
            i16::from_le_bytes([frame.payload[0], frame.payload[1]]),
            SECRETCHAT_SUCCESS
        );
        assert_eq!(&frame.payload[2..5], b"Bob");
        assert_eq!(
            i32::from_le_bytes(frame.payload[24..28].try_into().unwrap()),
            0
        );
    }

    /// A `/t` to a name that is not online answers `SECRETCHAT_FAIL_INVALIDNAME`
    /// with the requested name and `siCharUnique` 0 (`:871`).
    #[test]
    fn a_whisper_to_an_unknown_name_fails_with_invalidname() {
        let registry = Registry::new();
        let (alice, mut ra) = chat_join(&registry, chat_info(11, "Alice", 0, 0));

        handle_chat(
            &registry,
            alice,
            ChatRequest {
                mode: CHATMODE_SECRET,
                system_txt: 0,
                text: "/t Nobody hello".to_owned(),
            },
        );

        let frame = ra.try_recv().expect("Alice gets the failure");
        assert_eq!(frame.cmd, GameMsg::ResponseSecretchatresult as u16);
        assert_eq!(
            i16::from_le_bytes([frame.payload[0], frame.payload[1]]),
            SECRETCHAT_FAIL_INVALIDNAME
        );
        assert_eq!(&frame.payload[2..8], b"Nobody");
        assert_eq!(
            i32::from_le_bytes(frame.payload[24..28].try_into().unwrap()),
            0
        );
    }

    /// `/p` is a party invite, and the legacy's `SRVAL_PARTYINVITE_*` responses
    /// are not modelled — so the request is dropped silently (`:912-969`).
    #[test]
    fn a_party_invite_is_dropped_silently() {
        let registry = Registry::new();
        let (alice, mut ra) = chat_join(&registry, chat_info(11, "Alice", 0, 0));
        let (_bob, mut rb) = chat_join(&registry, chat_info(12, "Bob", 0, 0));

        handle_chat(
            &registry,
            alice,
            ChatRequest {
                mode: CHATMODE_SECRET,
                system_txt: 0,
                text: "/p Bob".to_owned(),
            },
        );

        assert_chat_silent(&mut ra, "the speaker");
        assert_chat_silent(&mut rb, "Bob");
    }

    /// Any other leading `/` is a GM order, and `OperateGameMasterOrder` always
    /// fails here — so the speaker gets the 34-byte FAIL notice (`:978-984`).
    #[test]
    fn a_gm_order_fails_with_the_34_byte_notice() {
        let registry = Registry::new();
        let (alice, mut ra) = chat_join(&registry, chat_info(11, "Alice", 0, 0));
        let (_bob, mut rb) = chat_join(&registry, chat_info(12, "Bob", 0, 0));

        handle_chat(
            &registry,
            alice,
            ChatRequest {
                mode: CHATMODE_SECRET,
                system_txt: 0,
                text: "/w something".to_owned(),
            },
        );

        let frame = ra.try_recv().expect("Alice gets the notice");
        assert_eq!(frame.cmd, GameMsg::ResponseNotice as u16);
        assert_eq!(frame.payload.len(), 34, "the FAIL notice is 34 bytes");
        assert_eq!(&frame.payload[10..14], b"FAIL");
        assert_eq!(&frame.payload[14..32], b"Master Order Error");
        assert_chat_silent(&mut rb, "Bob");
    }

    /// Head chat stores the text (capped at 127) and fans the 122 out over the
    /// near cells including the speaker (`:1050-1055`).
    #[test]
    fn head_chat_stores_and_fans_to_the_near_cells() {
        let registry = Registry::new();
        let (alice, mut ra) = chat_join(&registry, chat_info(11, "Alice", 0, 0));
        let (_bob, mut rb) = chat_join(&registry, chat_info(12, "Bob", 5, 5));
        let (_carol, mut rc) = chat_join(&registry, chat_info(13, "Carol", 500, 500));

        handle_chat(
            &registry,
            alice,
            ChatRequest {
                mode: CHATMODE_HEAD,
                system_txt: 0,
                text: "top of the world".to_owned(),
            },
        );

        // The text is stored for 121.
        assert_eq!(
            registry.head_chat(alice).as_deref(),
            Some("top of the world")
        );

        // Alice and Bob are in the same cell; Carol is not.
        for (rx, who) in [(&mut ra, "Alice"), (&mut rb, "Bob")] {
            let frame = rx
                .try_recv()
                .unwrap_or_else(|_| panic!("{who} gets the 122"));
            assert_eq!(frame.cmd, GameMsg::ResponseHeadchat as u16);
            assert_eq!(i16::from_le_bytes([frame.payload[0], frame.payload[1]]), 11);
            assert_eq!(&frame.payload[4..], b"top of the world\0\0");
        }
        assert_chat_silent(&mut rc, "Carol");
    }

    /// 121 asks for a *subject's* stored head chat and replies to the asker with
    /// the subject's text — or nothing if the subject is offline or the switch
    /// is off (`:708-712`).
    #[test]
    fn head_chat_request_replies_with_the_subjects_stored_text() {
        let registry = Registry::new();
        let (alice, mut ra) = chat_join(&registry, chat_info(11, "Alice", 0, 0));
        let (bob, mut rb) = chat_join(&registry, chat_info(12, "Bob", 0, 0));

        // Alice sets her head chat; both get the 122 fan.
        handle_chat(
            &registry,
            alice,
            ChatRequest {
                mode: CHATMODE_HEAD,
                system_txt: 0,
                text: "look at me".to_owned(),
            },
        );
        let _ = ra.try_recv().expect("Alice gets her own 122");
        let _ = rb.try_recv().expect("Bob gets Alice's 122");

        // Bob asks for Alice's head chat.
        handle_head_chat_request(&registry, bob, 11);

        let frame = rb.try_recv().expect("Bob gets the 121 reply");
        assert_eq!(frame.cmd, GameMsg::ResponseHeadchat as u16);
        assert_eq!(
            i16::from_le_bytes([frame.payload[0], frame.payload[1]]),
            11,
            "the subject"
        );
        assert_eq!(&frame.payload[4..], b"look at me\0\0");
        assert_chat_silent(&mut ra, "Alice");
    }

    /// 121 is silent when the subject's switch is off or the subject is not
    /// online (`:708-712`).
    #[test]
    fn head_chat_request_is_silent_when_the_subject_is_offline_or_off() {
        use gz_proto::chat::CHATMODE_NORMAL;

        let registry = Registry::new();
        let (alice, mut ra) = chat_join(&registry, chat_info(11, "Alice", 0, 0));
        let (bob, mut rb) = chat_join(&registry, chat_info(12, "Bob", 0, 0));

        // Bob asks for Alice's head chat before she sets it: the switch is off.
        handle_head_chat_request(&registry, bob, 11);
        assert_chat_silent(&mut rb, "switch off");

        // Alice sets it, then clears it with a normal chat.
        handle_chat(
            &registry,
            alice,
            ChatRequest {
                mode: CHATMODE_HEAD,
                system_txt: 0,
                text: "on".to_owned(),
            },
        );
        let _ = ra.try_recv();
        let _ = rb.try_recv();
        handle_chat(
            &registry,
            alice,
            ChatRequest {
                mode: CHATMODE_NORMAL,
                system_txt: 0,
                text: "off".to_owned(),
            },
        );
        let _ = ra.try_recv();
        let _ = rb.try_recv();

        // The switch is off again.
        handle_head_chat_request(&registry, bob, 11);
        assert_chat_silent(&mut rb, "switch off again");

        // An unknown subject is silent too.
        handle_head_chat_request(&registry, bob, 999);
        assert_chat_silent(&mut rb, "unknown subject");
    }

    /// The HOME branch fans the 118 out over the speaker's home village,
    /// speaker included (`:1061-1066`).
    #[test]
    fn home_chat_reaches_the_speaker_school() {
        let registry = Registry::new();
        let (alice, mut ra) = chat_join(&registry, chat_info(11, "Alice", 0, 0));
        let (_bob, mut rb) = {
            let mut info = chat_info(12, "Bob", 0, 0);
            info.school_id = 4;
            chat_join(&registry, info)
        };
        let (_carol, mut rc) = chat_join(&registry, chat_info(13, "Carol", 0, 0));

        handle_chat(
            &registry,
            alice,
            ChatRequest {
                mode: CHATMODE_HOME,
                system_txt: 0,
                text: "home sweet home".to_owned(),
            },
        );

        for (rx, who) in [(&mut ra, "Alice"), (&mut rc, "Carol")] {
            let frame = rx
                .try_recv()
                .unwrap_or_else(|_| panic!("{who} gets the 118"));
            assert_eq!(frame.cmd, GameMsg::ResponseChat as u16);
            assert_eq!(
                i16::from_le_bytes([frame.payload[6], frame.payload[7]]),
                CHATMODE_HOME
            );
        }
        assert_chat_silent(&mut rb, "Bob is in another school");
    }

    /// Empty text is nothing to say, nothing sent (`:762`).
    #[test]
    fn empty_chat_text_is_silent() {
        use gz_proto::chat::CHATMODE_NORMAL;

        let registry = Registry::new();
        let (alice, mut ra) = chat_join(&registry, chat_info(11, "Alice", 0, 0));
        let (_bob, mut rb) = chat_join(&registry, chat_info(12, "Bob", 0, 0));

        handle_chat(
            &registry,
            alice,
            ChatRequest {
                mode: CHATMODE_NORMAL,
                system_txt: 0,
                text: String::new(),
            },
        );

        assert_chat_silent(&mut ra, "the speaker");
        assert_chat_silent(&mut rb, "Bob");
    }

    /// 119 resolves the target by `siCharUnique` *and* by name, independently,
    /// and delivers to the charunique target with the sender's level (`:551-593`)
    /// — the quirk where a message carrying Alice's id and Bob's name reaches
    /// Alice is locked down here.
    #[test]
    fn secret_chat_delivers_to_the_charunique_target_with_the_senders_level() {
        let registry = Registry::new();
        let (alice, mut ra) = chat_join(&registry, chat_info(11, "Alice", 0, 0));
        let (_bob, mut rb) = chat_join(&registry, chat_info(12, "Bob", 0, 0));

        // Alice sends to char_unique 12 (Bob) but names "Alice" — the legacy
        // delivers to the charunique target (Bob) because it never compares.
        handle_secret_chat(
            &registry,
            alice,
            SecretChatRequest {
                char_unique: 12,
                name: "Alice".to_owned(),
                text: "for Bob".to_owned(),
            },
        );

        // Bob gets the 118 with Alice's level.
        let frame = rb.try_recv().expect("Bob gets the 119 delivery");
        assert_eq!(frame.cmd, GameMsg::ResponseChat as u16);
        let body = &frame.payload;
        assert_eq!(i16::from_le_bytes([body[2], body[3]]), 11, "the sender");
        assert_eq!(i16::from_le_bytes([body[6], body[7]]), CHATMODE_SECRET);
        assert_eq!(
            i32::from_le_bytes(body[16..20].try_into().unwrap()),
            34,
            "the sender's level"
        );
        assert_eq!(&body[20..25], b"Alice");
        assert_eq!(&body[25..], b"for Bob\0");

        // No success ack on this path.
        assert_chat_silent(&mut ra, "Alice");
    }

    /// A 119 with an unknown char_unique or name answers `SECRETCHAT_FAIL_NOTUSER`
    /// with the literal name `"NULL"` and the request's `siCharUnique` echoed
    /// (`:554`, `:564`).
    #[test]
    fn secret_chat_failure_names_null_and_echoes_the_request_id() {
        let registry = Registry::new();
        let (alice, mut ra) = chat_join(&registry, chat_info(11, "Alice", 0, 0));

        // Unknown char_unique.
        handle_secret_chat(
            &registry,
            alice,
            SecretChatRequest {
                char_unique: 999,
                name: "Bob".to_owned(),
                text: "hello".to_owned(),
            },
        );
        let frame = ra.try_recv().expect("Alice gets the failure");
        assert_eq!(frame.cmd, GameMsg::ResponseSecretchatresult as u16);
        assert_eq!(
            i16::from_le_bytes([frame.payload[0], frame.payload[1]]),
            SECRETCHAT_FAIL_NOTUSER
        );
        assert_eq!(&frame.payload[2..6], b"NULL");
        assert_eq!(
            i32::from_le_bytes(frame.payload[24..28].try_into().unwrap()),
            999
        );

        // Known char_unique but unknown name.
        handle_secret_chat(
            &registry,
            alice,
            SecretChatRequest {
                char_unique: 11,
                name: "Nobody".to_owned(),
                text: "hello".to_owned(),
            },
        );
        let frame = ra.try_recv().expect("Alice gets the second failure");
        assert_eq!(
            i16::from_le_bytes([frame.payload[0], frame.payload[1]]),
            SECRETCHAT_FAIL_NOTUSER
        );
        assert_eq!(&frame.payload[2..6], b"NULL");
        assert_eq!(
            i32::from_le_bytes(frame.payload[24..28].try_into().unwrap()),
            11
        );
    }
}
