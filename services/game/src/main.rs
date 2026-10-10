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
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use gz_cipher::Cipher;
use gz_db::{DropLocation, PgAccountRepository, PgCharacterRepository, PgItemRepository};
use gz_net::session::{Rejection, Session};
use gz_net::{COMPRESSION_FLAG, Frame, FrameParser, HEADER_SIZE, MAX_FRAME_SIZE, Tick};
use gz_proto::chat::{decode_chat_request, decode_head_chat_request, decode_secret_chat_request};
use gz_proto::game::{CharUnique, GameLogin, decode_select_char, decode_village_struct_request};
use gz_proto::opcodes_game::GameMsg;
use gz_proto::order::OrderData;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

type AnyError = Box<dyn std::error::Error + Send + Sync>;

mod auth;
mod chat;
mod config;
mod items;
mod movement;
mod registry;
mod stats;
mod world;
mod world_entry;

use config::GameConfig;
use registry::{Outbound, Registered, Registry, SessionInfo};

// The auth/character-lifecycle family lives in `auth.rs`. The dispatcher calls
// the login/selectchar/make-new-char/delete handlers; `send_char_list` and
// `admit_select_char` are reached from the `mod tests` below through
// `super::*`, hence the allow.
#[allow(unused_imports)]
pub(crate) use auth::{
    admit_select_char, handle_game_login, handle_make_new_char, handle_select_char,
    handle_userchar_deletion, send_char_list,
};

// The chat family lives in `chat.rs`; the crate root re-exports the handlers
// the 117/119/121 dispatch arms call and the pure functions the `mod tests`
// below reach through `super::*`. Some of these are test-only from the root's
// point of view, hence the allow — they travel with `chat` when its tests move
// into the module.
#[allow(unused_imports)]
pub(crate) use chat::{
    CHATMODE_GUILD, CHATMODE_HEAD, CHATMODE_HOME, CHATMODE_KING, CHATMODE_NEWSITEM, CHATMODE_PARTY,
    CHATMODE_SECRET, CHATMODE_TOWN, CHATMODE_VILLAGER, ChatRequest, ChatRoute,
    SECRETCHAT_FAIL_INVALIDNAME, SECRETCHAT_FAIL_NOTUSER, SECRETCHAT_SUCCESS, SecretChatRequest,
    handle_chat, handle_head_chat_request, handle_secret_chat, route_chat, whisper_parts,
};

// The movement family lives in `movement.rs`. The dispatcher calls the request
// handlers and `next_move_deadline`; the `mod tests` below reach `Movement` and
// the frame builders through `super::*`, hence the allow.
#[allow(unused_imports)]
pub(crate) use movement::{
    Movement, char_update_body, fan_char_update, handle_leave_request, handle_userorder,
    logout_departed, next_move_deadline, send_char_update, step_movement,
};

// The stats family lives in `stats.rs`. The dispatcher calls the 230 and 250
// handlers and world entry calls `basic_info_for`; the `mod tests` below reach
// `Allocation`, `allocate_stat` and the derived-maxima formulas through
// `super::*`, hence the allow.
#[allow(unused_imports)]
pub(crate) use stats::{
    Allocation, BasicInfo, UseBonus, allocate_stat, basic_info_for, derived_max_life,
    derived_max_mana, handle_change_status_info, handle_use_bonus,
};

// The item family lives in `items.rs`. The dispatcher calls the 303/305/306/307/
// 308 and 7002 handlers; the `mod tests` below reach the personal-shop helpers
// through `super::*`, hence the allow.
#[allow(unused_imports)]
pub(crate) use items::{
    handle_inv_drop, handle_inv_pickup, handle_item_move, handle_item_split,
    handle_personal_shop_data,
};

// The world family lives in `world.rs`. The dispatcher calls 1101 and world
// entry; `LiveFrames`, `announce_nearby_characters` and the char-info builders
// are reached from the `mod tests` below and from the movement module through
// `super::*`/`crate::*`, hence the allow.
#[allow(unused_imports)]
pub(crate) use world::{
    LIVE_CHARACTER_STATE, LiveFrames, announce_nearby_characters, handle_village_struct,
    make_char_info_for, make_char_info_from_session, send_character_state, send_world_entry,
    server_info,
};

/// `MAX_TOTAL_CHAR_PER_USER` (3 main + 12 char-house) — the legacy sends one
/// char-list packet for every slot, occupied or not
/// (`ServerDB-Person.cpp:1560-1606`). **All 15 are required:** the shipped
/// client's `IsReadyCharList` (`0x737120`) AND-reduces `clBI.GetIndex() == i`
/// across `i in 0..0xF` and only self-heals the missing slots after a long
/// timeout — sending fewer than 15 is what makes char-select stall. The array
/// is 15 entries (`PutCharList` `0x48D040` rejects `GetIndex() > 14`).
const CHAR_SLOTS: i16 = 15;

/// `< 0x4E20` (20000): the client indexes its `CharUniqueInfo` table by
/// `uiMakeCharUnique` (`SetCharReservedChar`).
const MAX_SESSION_CHAR_UNIQUE: u64 = 19_999;

/// Hands out per-session character ids in `1..=MAX_SESSION_CHAR_UNIQUE`.
///
/// The legacy's equivalent is `m_uiCharID`, a UI16 the session manager assigns
/// when it binds a session (`CharServer-Session.cpp:58` passes it straight into
/// `MakeSimpleChar`) — a connection-scoped number, not a database column. Only
/// [`GameConfig::char_unique`] is the only source; it always returns the allocation.
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
#[macro_export]
macro_rules! logln {
    ($($arg:tt)*) => {
        std::println!("[{}] {}", $crate::log_stamp(), std::format_args!($($arg)*))
    };
}

/// [`logln`]'s stderr twin, in place of the bare `eprintln!` the error paths used.
#[macro_export]
macro_rules! logln_err {
    ($($arg:tt)*) => {
        std::eprintln!("[{}] {}", $crate::log_stamp(), std::format_args!($($arg)*))
    };
}

#[tokio::main]
async fn main() -> Result<(), AnyError> {
    if let Some(command) = std::env::args().nth(1) {
        return Err(format!("unknown argument {command:?}: game.exe takes no arguments").into());
    }
    let cfg = GameConfig::from_env()?;
    serve(&cfg).await
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
        "game: link key from configuration; char_unique=allocated per session, \
         world entry generated from PostgreSQL"
    );

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
    // arrived — so this cannot be read from the database here. It is allocated per
    // session, the way `m_uiCharID` is a connection-scoped number in the legacy.
    // `characters.game_unique` holds the durable per-character id and
    // `handle_select_char` cross-checks the two.
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
    // The move being stepped, if any. Driven by the read loop's timer arm; see
    // [`Movement`].
    let mut movement: Option<Movement> = None;
    // Which frames get built from PostgreSQL on the way out.
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
        let read_window = if !session_state.state().is_active() {
            Some(cfg.timeouts.handshake)
        } else if let Some(mv) = movement.as_ref() {
            mv.deadline
                .map(|dl| dl.saturating_duration_since(Instant::now()))
        } else {
            None
        };
        let tick = tokio::select! {
            read = stream.read(&mut buf) => LoopTick::Read(read),
            queued = out_rx.recv() => LoopTick::Send(queued),
            _ = async {
                match read_window {
                    Some(window) => tokio::time::sleep(window).await,
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
                return Err("outbound channel closed while the session was live".into());
            }
            LoopTick::TimedOut => {
                if session_state.state().is_active() {
                    // Movement deadline fired: step if a tile is due.
                    if movement.is_some() {
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
                        // Re-arm the deadline from the (potentially updated) movement.
                        if let Some(mv) = movement.as_mut() {
                            mv.deadline = Some(next_move_deadline(mv));
                        }
                    }
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
                    // in-flight step so the old character cannot walk on
                    // (`ServerDB-Person.cpp:1795`).
                    if let Some(mv) = movement.as_mut() {
                        mv.deadline = None;
                    }
                    movement = None;

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
                            class_kind: u16::try_from(character_id.class_kind.max(0)).unwrap_or(0),
                            life: u32::try_from(character_id.max_life.max(0)).unwrap_or(0),
                            move_speed: u8::try_from(character_id.move_speed.clamp(0, 63))
                                .unwrap_or(0),
                            sex: u8::try_from(character_id.gender.clamp(0, 1)).unwrap_or(0),
                            // `gm_level` lives on the authenticated account, not the
                            // character row (`GameSession`), so it is read from the
                            // login rather than from `character_id`.
                            gm_level: login
                                .as_ref()
                                .map(|s| u8::try_from(s.gm_level.clamp(0, 255)).unwrap_or(0))
                                .unwrap_or(0),
                        },
                    );
                    // Make the newcomer visible, and make it visible to everyone
                    // already here — both halves of
                    // `cltServerMap::SendNewOthersInfoToPerson` (`ServerMap.cpp:73-150`).
                    announce_nearby_characters(&registry, session_key, &character_id, char_unique)?;
                    // The whole of world entry, generated from PostgreSQL: the
                    // context frames, then the character's own state block.
                    //
                    // It ships *with* char-select rather than waiting for the
                    // client's first `REQUEST_CHANGESTATUSINFO` (230), because that
                    // 230 rides a process-global 30 s timer anchored at *first*
                    // world entry and never restarts across a logout — so on a
                    // re-entry a deferred bulk lands up to 30 s late, after the
                    // client has already built a world without it. That was the
                    // measured ~20 s re-entry stall (2026-10-08).
                    send_world_entry(&mut stream, &mut outbound, &live).await?;
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
                    // 230 is the horse auto-food message, and the legacy answers it
                    // with at most a 24-byte 231 (`Server-Person.cpp:2831-2860`).
                    //
                    // It used to have a second role here: the reference server's
                    // first post-char-select 230 answered the world bulk. That is
                    // gone — `send_world_entry` ships the whole world with
                    // char-select — so every 230 now takes the same path.
                    handle_change_status_info(
                        &mut stream,
                        &mut outbound,
                        char_unique,
                        payload.as_ref(),
                    )
                    .await?;
                }
                c if c == GameMsg::RequestUsebonus as u16 => {
                    // 250: stat allocation. Handled server-authoritatively: the
                    // request is a wish, the server reads its own row, validates,
                    // writes, recomputes the derived maxima and answers with 215.
                    handle_use_bonus(
                        &mut stream,
                        &mut outbound,
                        &characters,
                        &mut live,
                        payload.as_ref(),
                    )
                    .await?;
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
                        &registry,
                        session_key,
                        &mut live,
                        order,
                    )
                    .await?
                    {
                        Some(mut mv) => {
                            if let Some(session) = login.as_mut() {
                                session.entry_cell = Some(DropLocation {
                                    map_id: mv.map_id,
                                    x: mv.pos.0,
                                    y: mv.pos.1,
                                });
                            }
                            // Arm the deadline for the first step.

                            mv.deadline = Some(next_move_deadline(&mv));
                            movement = Some(mv);
                        }
                        // A modelled stop, or any order this port does not act
                        // on: stop stepping so the authoritative cell freezes.
                        None => {
                            if let Some(mv) = movement.as_mut() {
                                mv.deadline = None;
                            }
                            movement = None;
                        }
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

/// Two key bytes for this connection's session cipher. Unpredictability only
/// needs to defeat accidental reuse in development, not an attacker.
fn random_key_bytes() -> (u8, u8) {
    let v = hash_now();
    (v as u8, (v >> 8) as u8)
}

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
    use crate::registry::SessionKey;
    use gz_db::CharacterFull;
    use gz_proto::game::{
        MakeCharInfo, encode_all_char_info_one, encode_all_char_info_two, encode_change_horse,
        encode_family_update, encode_get_village_info, encode_login_many_batches,
        encode_person_horse, encode_response_name, encode_skillbook_magic_info,
    };
    use gz_proto::item::{Item, encode_person_item_info};
    use gz_proto::order::BasicCharUpdate;
    use gz_proto::personalshop::{ShopTradeData, shop_mode_for_request};
    use std::time::Duration;
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

    /// A fully-populated row for the allocation tests. Level 13 with abilities
    /// 20/15/20/10/10/10/10 and bonus 5 is exactly the *repaired* dev seed: the
    /// total is 100, which is `40 + 12*5`, so the starting 5 points are
    /// spendable (at the old level 10 the same row totals 100 against an
    /// allowance of 85 and every spend is refused).
    fn seeded_tester() -> CharacterFull {
        CharacterFull {
            character_id: 1,
            name: "Tester".into(),
            class_kind: 1,
            gender: 1,
            level: 13,
            max_life: 161,
            max_mana: 40,
            move_speed: 8,
            exp: 0,
            money: 1000,
            map_id: 158,
            pos_x: 27,
            pos_y: 87,
            slot: 0,
            school_id: 0,
            game_unique: Some(1471),
            str: 20,
            dex: 15,
            vit: 20,
            mag: 10,
            hnd: 10,
            luk: 10,
            wis: 10,
            bonus_points: 5,
        }
    }

    /// B1: the fan carries the same bytes the mover's own socket gets, so the two
    /// writers of one frame cannot drift. `fan_char_update` and `send_char_update`
    /// both go through `char_update_body`, so this pins that they are one encoder.
    ///
    /// It also pins what the frame actually carries for an observer: the cell, and
    /// the move speed. `si_life`/`si_mana` ride along (the struct has no smaller
    /// variant) but the client renders no bystander bar from them — the floating
    /// gauge is `CR[1]`-only (`NCharPerson.cpp:3920`). The observer's animation
    /// comes from `si_x`/`si_y` via the `bselfswitch == false` branch
    /// (`Client-Person-BigInfo.cpp:1187-1222`), so those are the fields under test.
    #[test]
    fn the_observer_fan_carries_the_same_cell_and_speed_as_the_movers_own_frame() {
        let body = char_update_body(1471, &seeded_tester(), 28, 90, false).unwrap();
        let update = BasicCharUpdate::decode(&body).expect("the fan body decodes as a 200");

        assert_eq!(update.si_char_unique, 1471);
        assert_eq!((update.si_x, update.si_y), (28, 90));
        assert_eq!(
            update.si_move_speed, 8,
            "the character's own effective move speed, not a constant"
        );
        // The captured frame's length, so a field added to `BasicCharUpdate`
        // cannot silently change the wire size of every observer's frame.
        assert_eq!(body.len(), gz_proto::order::BASIC_CHAR_UPDATE_LEN);
    }

    /// The fan reaches observers in the 3×3 near block and never the sender, who
    /// already wrote the frame directly (`registry.rs` tests the cell arithmetic;
    /// this pins the wiring, including that a walker's own copy is not duplicated).
    /// Registers one session in a registry for a fan test and hands back its inbox.
    fn registry_join(
        registry: &Registry,
        char_unique: i16,
        name: &str,
        x: i32,
        y: i32,
    ) -> (SessionKey, tokio::sync::mpsc::UnboundedReceiver<Outbound>) {
        let key = Registry::next_key();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        registry.register(
            key,
            tx,
            SessionInfo {
                char_unique,
                name: name.to_owned(),
                level: 13,
                map_id: 158,
                x,
                y,
                school_id: 0,
                class_kind: 1,
                life: 161,
                move_speed: 12,
                sex: 1,
                gm_level: 0,
            },
        );
        (key, rx)
    }

    #[test]
    fn the_fan_reaches_a_nearby_session_and_skips_the_mover() {
        let registry = Registry::new();
        // Both characters spawn on the palace tile (0009's deliberate choice), so
        // they are in one another's near cell on login.
        let (mover_key, mut rmover) = registry_join(&registry, 1471, "Tester", 27, 87);
        let (_observer_key, mut robeserver) = registry_join(&registry, 1472, "Tester2", 27, 87);
        let (_far_key, mut rfar) = registry_join(&registry, 1473, "Far", 400, 400);

        // `step_movement` updates the registry before fanning, so the observer set
        // is the cell just reached.
        registry.update_pos(mover_key, 28, 90);
        let observers =
            fan_char_update(&registry, mover_key, 1471, &seeded_tester(), 28, 90, false).unwrap();
        assert_eq!(
            observers, 1,
            "Tester2 shares the cell; the far one does not"
        );

        let frame = robeserver.try_recv().expect("the observer gets the 200");
        assert_eq!(frame.cmd, GameMsg::ResponseCharupdate as u16);
        let update = BasicCharUpdate::decode(&frame.payload).expect("observer body decodes");
        assert_eq!((update.si_x, update.si_y), (28, 90));
        assert!(
            rmover.try_recv().is_err(),
            "the mover writes its own copy directly; a queued one could land after a later direct one"
        );
        assert!(
            rfar.try_recv().is_err(),
            "cell (40, 26) is not in the 3x3 block around (2, 6)"
        );
    }

    /// World entry must send all three per-character state frames. This is the
    /// regression test for the 2026-10-09 fault: under `live` no 248/249 was ever
    /// sent, because both the char-select push and the 230 arm were gated on
    /// the retired replay mode, so the client entered the world with a null
    /// per-character record — no name, black HP/MP bars, unable to move.
    ///
    /// Asserted against the *shape* (which opcodes, and that they are the ones that
    /// carry `clBI`/`clIP`) rather than against a socket, because the original bug
    /// was an absent call, not a wrong body.
    #[test]
    fn live_world_entry_sends_the_per_character_state_block() {
        // 244 names the avatar, 248 is clBI+clIP, 249 is the item array. Anything
        // missing here is a field the client reads out of a null record.
        for cmd in [
            GameMsg::ResponseName as u16,
            GameMsg::ResponseAllcharinfoone as u16,
            GameMsg::ResponseAllcharinfotwo as u16,
        ] {
            assert!(
                LIVE_CHARACTER_STATE.contains(&cmd),
                "opcode {cmd} carries the character state and must be sent at world entry"
            );
        }
        // Nothing else belongs here: these are the only three the burst frames
        // regenerate from the row, and each is one `regenerate` arm.
        assert_eq!(LIVE_CHARACTER_STATE.len(), 3);
        // The nameplate must precede the block that names the same character, or
        // the client can resolve an id it has not been told the name of yet.
        assert_eq!(
            LIVE_CHARACTER_STATE[0],
            GameMsg::ResponseName as u16,
            "244 names the avatar before 248/249 describe it"
        );
    }

    /// Two sessions must never share a character id — the property the retired
    /// capture mode broke.
    ///
    /// Pinning every session to one id made two clients the same character, so each
    /// treated the other's 200 as its own (`bselfswitch`,
    /// `Client-Person-BigInfo.cpp:1160-1176`) and wrote the other's life/mana into
    /// its own avatar (`:1228-1238`, unconditional). That is a client-side identity
    /// collision, which no server-side test can observe — hence it is pinned as a
    /// property of the allocator rather than over a socket.
    #[test]
    fn two_sessions_get_distinct_character_ids() {
        assert_ne!(
            allocate_char_unique(),
            allocate_char_unique(),
            "two sessions must never share a character id"
        );
        // And the allocator stays inside the client's `CharUniqueInfo` bound, which
        // is what the 592-byte `uiMakeCharUnique` field and the client's own table
        // both require.
        let bound = MAX_SESSION_CHAR_UNIQUE;
        let first = u64::from(NEXT_CHAR_UNIQUE.load(std::sync::atomic::Ordering::Relaxed));
        assert!(
            first <= bound + 1,
            "the allocator must not exceed the client's CharUniqueInfo bound"
        );
        assert_ne!(
            gz_proto::captured_lengths::CAPTURED_CHAR_UNIQUE as u64,
            bound,
            "the retired capture's id is a different character, not the bound"
        );
    }

    /// LOGINMANY is the frame that makes another player exist, and it must be built
    /// from the **registry** (other players) rather than from this session's row.
    ///
    /// Regression test for "nearby avatars are invisible", reported live 2026-10-09.
    /// The 200 fan was verified byte-correct on a real socket and still rendered
    /// nothing, because the observer had no `CR[id]` for the mover: 6 is the only
    /// frame that creates one (`Client-Login.cpp:53-85` -> `PushChar` ->
    /// `SetCharReservedChar` -> `SetChar`, `Client-SetChar.cpp:112-138`), and every
    /// per-character frame is discarded at `Client-Person-BigInfo.cpp:1166-1169`
    /// when `CharUniqueInfo[charunique]` is 0.
    #[test]
    fn login_many_carries_the_other_players_own_identity() {
        let registry = Registry::new();
        // The observer, already in the world.
        let (observer_key, mut robeserver) = registry_join(&registry, 1472, "Tester2", 27, 87);
        // A newcomer joins the same cell.
        let (newcomer_key, mut rnewcomer) = registry_join(&registry, 1473, "Tester", 27, 87);

        announce_nearby_characters(&registry, newcomer_key, &seeded_tester(), 1473).unwrap();

        // The newcomer is told about the observer. Read it off the wire the server
        // actually queued — not off a locally rebuilt body, which would only prove
        // the encoder agrees with itself.
        let frame = rnewcomer
            .try_recv()
            .expect("the newcomer learns who is here");
        assert_eq!(frame.cmd, GameMsg::ResponseLoginmany as u16);
        assert_eq!(
            frame.payload.len(),
            gz_proto::game::MAKE_CHAR_INFO_PAYLOAD_LEN,
            "one packed record, no length prefix"
        );
        let announced = MakeCharInfo::decode(&frame.payload).expect("the record decodes");
        assert_eq!(
            announced.make_char_unique, 1472,
            "the newcomer must be told about the observer's id, not itself"
        );

        // ...and the observer is told about the newcomer. This second direction is
        // what the legacy also does (`ServerMap.cpp:73-150`), and without it the
        // observer would never see the newcomer appear.
        let frame = robeserver
            .try_recv()
            .expect("the observer learns of the newcomer");
        assert_eq!(frame.cmd, GameMsg::ResponseLoginmany as u16);
        let announced = MakeCharInfo::decode(&frame.payload).expect("the record decodes");
        assert_eq!(
            announced.make_char_unique, 1473,
            "the observer must be told about the newcomer's id"
        );
        assert_eq!(
            announced.make_char_kind, 1,
            "uiMakeCharKind is what the client draws the avatar from, not the name"
        );
        assert_eq!((announced.x, announced.y), (27, 87));
        assert_eq!(
            announced.map_index, 158,
            "the map the client must already hold"
        );
        // Never a record of itself: a client re-SetChar-ing its own avatar would
        // reset the character it is already standing on.
        let _ = observer_key;
    }

    /// A character that leaves the newcomer set gets no record — the near block is
    /// the whole point of the frame, and an over-wide fan would put avatars on screen
    /// that are not there.
    #[test]
    fn login_many_excludes_characters_outside_the_near_block() {
        let registry = Registry::new();
        let (newcomer_key, mut rnewcomer) = registry_join(&registry, 1473, "Tester", 27, 87);
        let (_near, mut rnear) = registry_join(&registry, 1472, "Near", 28, 88);
        let (_far, mut rfar) = registry_join(&registry, 1474, "Far", 400, 400);

        announce_nearby_characters(&registry, newcomer_key, &seeded_tester(), 1473).unwrap();

        let body = rnewcomer.try_recv().expect("one frame for the near cell");
        let announced = MakeCharInfo::decode(&body.payload).expect("the record decodes");
        assert_eq!(
            announced.make_char_unique, 1472,
            "only the character sharing the near cell"
        );
        assert!(
            rfar.try_recv().is_err(),
            "cell (40, 26) is not in the 3x3 block around (2, 5)"
        );
        // The bystander is told about the newcomer; the distant one is not told at all.
        let frame = rnear.try_recv().expect("the near bystander is told too");
        assert_eq!(frame.cmd, GameMsg::ResponseLoginmany as u16);
    }

    /// An empty near block sends **nothing**, never a zero-length frame.
    ///
    /// `DoMsg_GAMEMSG_RESPONSE_LOGINMANY` casts the payload and reads `uiType`
    /// before it knows it got any bytes (`Client-Login.cpp:61-63`), so an empty body
    /// is walked as garbage. This is the legacy's own `if(index)` guard
    /// (`ServerMap.cpp:140`), and the reason 6 used to be withheld wholesale.
    #[test]
    fn an_empty_near_block_sends_no_frame_at_all() {
        assert!(
            encode_login_many_batches(&[]).is_empty(),
            "no records must mean no frame, not a zero-length one"
        );
        let registry = Registry::new();
        let (lonely_key, mut rlonely) = registry_join(&registry, 1471, "Tester", 27, 87);
        announce_nearby_characters(&registry, lonely_key, &seeded_tester(), 1471).unwrap();
        assert!(
            rlonely.try_recv().is_err(),
            "alone in the world, nothing is announced"
        );
    }

    /// A generated record round-trips through the client's own stride rules.
    ///
    /// This is the assertion that would have caught the bug in `MakeCharInfo::decode`
    /// that the first version of the test above walked into: reading the header with
    /// a sequential `Reader` skips the 0x07 and 0x11 **pad bytes** and shifts every
    /// field after them, so a record decoded as `(768, 769)` instead of `(27, 87)`.
    /// A fan built from mis-decoded coordinates would put avatars in the sea.
    #[test]
    fn a_generated_login_many_record_round_trips_through_its_own_layout() {
        for (unique, kind, level, life, x, y, map, speed) in [
            (1471u16, 1u16, 13u8, 161u32, 27u16, 87u16, 158u16, 12u8),
            (1, 0, 1, 40, 0, 0, 1, 10),
            // 10-bit fields at their maxima, which is where a masking bug shows.
            (19_999, 65_535, 255, 0xF_FFFF, 0x3FF, 0x3FF, 0x3FF, 63),
        ] {
            let record = MakeCharInfo {
                make_char_unique: unique,
                make_char_kind: kind,
                level,
                life,
                x,
                y,
                map_index: map,
                total_move_speed: speed,
                home_village_unique: 0,
                game_master_level: 0,
                map_unique: 0,
                make_ani_type: 0,
            };
            let decoded = MakeCharInfo::decode(&record.encode()).expect("decodes");
            assert_eq!(
                decoded, record,
                "record {unique} did not survive the round trip"
            );
        }
    }

    /// A record that did not round-trip would be worse than no record at all, so the
    /// encode/decode pair is pinned against the *client's* bit layout rather than
    /// against itself. The pack sites are the two dwords at 0x08 and 0x0C; a swapped
    /// bit order there is silent and would scatter every avatar.
    #[test]
    fn the_login_many_header_uses_the_clients_bit_order() {
        use gz_proto::game::MAKECHARTYPE_PC;
        // A record with every header field distinct, so a bit-order mix-up in either
        // packed dword shows up as the wrong field rather than a coincidence. The
        // speed is 17, not `seeded_tester`'s 8, precisely to keep the two dwords
        // from agreeing on anything.
        let record = MakeCharInfo {
            make_char_unique: 1471,
            make_char_kind: 9,
            level: 42,
            life: 123_456,
            x: 333,
            y: 777,
            map_index: 158,
            total_move_speed: 17,
            home_village_unique: 3,
            game_master_level: 0,
            map_unique: 0,
            make_ani_type: 0,
        };
        let bytes = record.encode();
        // 0x00 uiType:1 | uiMakeCharKind:16 (low bits first).
        assert_eq!(
            u32::from_le_bytes([bytes[0x00], bytes[0x01], bytes[0x02], bytes[0x03]]),
            (MAKECHARTYPE_PC & 1) | (9 << 1),
            "uiType must be MAKECHARTYPE_PC or the client strides as a Common record"
        );
        // uiMakeCharUnique:16 @ 0x04, uiLevel:8 @ 0x06, pad @ 0x07.
        assert_eq!(u16::from_le_bytes([bytes[0x04], bytes[0x05]]), 1471);
        assert_eq!(bytes[0x06], 42);
        assert_eq!(bytes[0x07], 0, "the pad byte must be zero");
        // 0x08 uiLife:20 | uiX:10.
        let life_x = u32::from_le_bytes([bytes[0x08], bytes[0x09], bytes[0x0A], bytes[0x0B]]);
        assert_eq!(life_x & 0xF_FFFF, 123_456);
        assert_eq!((life_x >> 20) & 0x3FF, 333);
        // 0x0C uiY:10 | uiMapIndex:10 | uiTotalMoveSpeed:6.
        let y_map_speed = u32::from_le_bytes([bytes[0x0C], bytes[0x0D], bytes[0x0E], bytes[0x0F]]);
        assert_eq!(y_map_speed & 0x3FF, 777);
        assert_eq!((y_map_speed >> 10) & 0x3FF, 158);
        assert_eq!((y_map_speed >> 20) & 0x3F, 17);
        // 0x10 siHomeVillageUnique.
        assert_eq!(bytes[0x10], 3);
    }

    /// A busy cell arrives as a *series* of frames, never one oversized one, and no
    /// record is dropped when the batch splits.
    #[test]
    fn a_login_many_batch_never_exceeds_the_frame_ceiling() {
        let record = seeded_tester();
        let many: Vec<MakeCharInfo> = (0..40)
            .map(|i| {
                let mut m = make_char_info_for(&record, 1473).unwrap();
                m.make_char_unique = u16::try_from(100 + i).unwrap();
                m
            })
            .collect();

        let batches = encode_login_many_batches(&many);
        assert!(batches.len() > 1, "40 records must not fit in one frame");
        let mut total = 0usize;
        for body in batches {
            assert!(
                body.len() <= gz_proto::game::MAX_CLTMSG_DATA_SIZE,
                "batch of {} bytes exceeds MAX_CLTMSG_DATA_SIZE",
                body.len()
            );
            assert!(!body.is_empty(), "a batch is never empty");
            // Records are packed, not length-prefixed: the body is a whole number of
            // them, which is how the client computes its stride.
            assert_eq!(body.len() % gz_proto::game::MAKE_CHAR_INFO_PAYLOAD_LEN, 0);
            total += body.len() / gz_proto::game::MAKE_CHAR_INFO_PAYLOAD_LEN;
        }
        assert_eq!(total, 40, "no record may be dropped when splitting");
    }

    /// The recalc formulas are the ones the dev seed's stored maxima must match:
    /// both reproduce the `0001` DDL defaults exactly at level 1 (vit 5 -> 40,
    /// mag 5 -> 20) and the repaired row's stored values at level 13.
    #[test]
    fn the_derived_maxima_reproduce_the_ddl_defaults_and_the_repaired_seed() {
        assert_eq!(derived_max_life(5, 1), 40);
        assert_eq!(derived_max_mana(5, 1), 20);
        assert_eq!(derived_max_life(20, 13), 161);
        assert_eq!(derived_max_mana(10, 13), 40);
    }

    #[test]
    fn allocating_spends_bonus_and_raises_the_stat() {
        let out = allocate_stat(&seeded_tester(), &UseBonus { pos: 0, amount: 5 }).unwrap();
        assert_eq!(out.bonus, 0);
        assert_eq!(out.str_, 25);
        // Vit/mag are untouched, so the derived maxima do not move.
        assert_eq!((out.max_life, out.max_mana), (161, 40));
        // The total is invariant under an allocation: 95 + 5 both ways.
    }

    #[test]
    fn allocating_vit_recomputes_max_life_from_the_formula() {
        let out = allocate_stat(&seeded_tester(), &UseBonus { pos: 3, amount: 5 }).unwrap();
        assert_eq!(out.vit, 25);
        assert_eq!(out.max_life, 25 * (8000 + 13 * 5) / 1000); // 201
        assert_eq!(out.bonus, 0);
    }

    #[test]
    fn allocate_rejects_zero_and_negative_amounts() {
        // `siAmount <= 0` is refused outright: the reset path (item 24035, the
        // `IDC_BUTTON_*2` space) is deferred, and zero is a no-op that must not
        // cheerfully "succeed".
        assert!(allocate_stat(&seeded_tester(), &UseBonus { pos: 0, amount: 0 }).is_err());
        assert!(allocate_stat(&seeded_tester(), &UseBonus { pos: 0, amount: -1 }).is_err());
        // The reset pos space never reaches the amount rule benefit-of-the-doubt.
        assert!(
            allocate_stat(
                &seeded_tester(),
                &UseBonus {
                    pos: 1115,
                    amount: -1
                }
            )
            .is_err()
        );
    }

    #[test]
    fn allocate_rejects_pos_outside_the_zero_to_six_ability_space() {
        for pos in [-1, 7, 1115] {
            assert!(
                allocate_stat(&seeded_tester(), &UseBonus { pos, amount: 1 }).is_err(),
                "siPos {pos} must be refused"
            );
        }
    }

    #[test]
    fn allocate_rejects_spending_more_than_the_unspent_vault() {
        // The client can only accumulate what the status window shows, but a
        // tampered `siAmount` must not conjure points from the server's vault.
        assert!(allocate_stat(&seeded_tester(), &UseBonus { pos: 0, amount: 6 }).is_err());
    }

    #[test]
    fn allocate_caps_hnd_at_twice_the_level() {
        // Hnd at exactly the level-13 cap (26) with the row's total still 100:
        // 16 points moved out of Str keep the invariant while the cap is at its
        // edge. A spend that would land on 27 must be refused.
        let mut at_cap = seeded_tester();
        at_cap.hnd = 26;
        at_cap.str = 4;
        assert!(allocate_stat(&at_cap, &UseBonus { pos: 4, amount: 1 }).is_err());
        // The remaining headroom is spendable.
        let mut headroom = seeded_tester();
        headroom.hnd = 25;
        headroom.str = 5;
        let out = allocate_stat(&headroom, &UseBonus { pos: 4, amount: 1 }).unwrap();
        assert_eq!(out.hnd, 26);
    }

    #[test]
    fn allocate_refuses_a_row_already_over_the_level_allowance() {
        // The unrepaired seed: the same abilities and bonus at level 10 total
        // 100 against an allowance of 85. The server refuses to let that row
        // spend at all — the soft-fail shape of the legacy's kick.
        let mut character = seeded_tester();
        character.level = 10;
        assert!(allocate_stat(&character, &UseBonus { pos: 0, amount: 1 }).is_err());
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

    /// The 249 body is exactly the item array plus a zeroed `clSummonInfo`.
    ///
    /// The client's willingness to accept the world-entry burst rests on this
    /// frame having the size the reference server sent, so the length is pinned to
    /// the capture's measurement. `clSummonInfo` is a documented deviation: the
    /// reference character owned summons (115 non-zero bytes in the capture) and we
    /// hold no table for them, so ours is honestly zero.
    #[test]
    fn the_item_array_splice_fills_exactly_the_captured_frame_length() {
        // Every slot empty: the encoder rejects a short slice and expects exactly
        // `MAX_ITEM_PER_PERSON` records, so the "empty bag" case is the full-width
        // one.
        let empty: Vec<gz_proto::item::Item> =
            vec![Item::empty(); gz_proto::item::MAX_ITEM_PER_PERSON];
        let body = encode_all_char_info_two(&empty).unwrap();
        assert_eq!(
            body.len(),
            gz_proto::captured_lengths::CAPTURED_ALL_CHAR_INFO_TWO_LEN,
            "249 must keep the size the reference server sent"
        );
        // The array itself is 80 whole records; anything left over is `clSummonInfo`
        // and must be zero, or the client walks a summon list that is not ours.
        assert_eq!(
            body.len() - gz_proto::item::ITEM_ARRAY_LEN,
            4_240,
            "the tail is clSummonInfo"
        );
        assert!(
            body[gz_proto::item::ITEM_ARRAY_LEN..]
                .iter()
                .all(|&b| b == 0),
            "clSummonInfo must be zero: we model no summons"
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
    /// `clHealthInfo` starts here, and `uiHealthPoint` is the tenth byte of it.
    ///
    /// Both offsets were measured against the capture (moved to
    /// `archive/capture/` on 2026-10-09) and are pinned here as literals. The
    /// arithmetic that produced them is preserved in the comments above
    /// [`HEALTH_INFO_OFFSET`] and in the handoff entry for 2026-10-06.
    ///
    /// What was wrong is worth keeping: `cltHealthInfo`'s *wire* width is
    /// [`CLT_HEALTH_INFO_WIRE_LEN`], four bytes narrower than the source-declared
    /// [`CLT_HEALTH_INFO_LEN`], because the client's own `Set` at `0x0085B2A0` is a
    /// single 16-byte `movups` pair. Measuring back from the capture's horse marker
    /// gave 7,592; the wrong 7,596 put the seed at 7,606, four bytes into a tail
    /// `New Era.exe` never reads — which is why writing 7,606 and then 7,607 both
    /// left the live status window showing `-100%`.
    #[test]
    fn the_health_point_offset_is_the_measured_one() {
        assert_eq!(HEALTH_INFO_OFFSET, 7_592);
        assert_eq!(HEALTH_POINT_OFFSET, 7_602);
        // `uiHealthPoint` is `CommonHeader.h:2898`'s tenth byte, which the client's
        // `SetHealthAddAbility` at `0x0084C380` reads as
        // `movzx eax, byte ptr [ecx+0Ah]`.
        assert_eq!(HEALTH_POINT_OFFSET - HEALTH_INFO_OFFSET, 10);
        // The *wire* width is the wider one: the client's `Set` copies sixteen bytes
        // in one `movups` pair, four past what this source declares.
        assert_eq!(CLT_HEALTH_INFO_WIRE_LEN - CLT_HEALTH_INFO_LEN, 4);

        // And what we now send. `cltHealthInfo::Init()` seeds `uiHealthPoint` at 100
        // (`Common.cpp:3253`), and 100 is the neutral value: it makes every
        // `siHealth*Rate` zero (`Parametabox.cpp:7727`), so `clHealthAdd` adds
        // nothing and the rest of `clTotalBA` supplies the real numbers.
        assert_eq!(HEALTH_POINT_SEED, 100);
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
            generated.len(),
            gz_proto::captured_lengths::CAPTURED_ALL_CHAR_INFO_ONE_LEN,
            "248 keeps the size the reference server sent"
        );
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

    /// The generated `PERSONITEMINFO` is 104 bytes, the size the reference server
    /// sent, and its 6..8 padding is ours (zero) where the legacy's was
    /// uninitialised stack.
    ///
    /// 104 is a *single-slot* notification, not an inventory dump — `siItemPos` = 8
    /// with all 84 `cltItem` bytes zero, i.e. "slot 8 is empty". That measurement
    /// is what disproved the old "item-array stride" premise.
    #[test]
    fn generated_person_item_info_keeps_the_captured_length() {
        let generated = encode_person_item_info(world_entry::PERSON_ITEM_INFO_SLOT, &Item::empty());
        assert_eq!(
            generated.len(),
            gz_proto::captured_lengths::CAPTURED_PERSON_ITEM_INFO_LEN,
            "104 bytes: a single-slot notification"
        );
        assert_eq!(
            i16::from_le_bytes([generated[0], generated[1]]),
            world_entry::PERSON_ITEM_INFO_SLOT as i16,
            "the slot is the one the reference server announced"
        );
        // 6..8 is the `/Zp8` pad between `siChangedNum` and `siExp`. The capture
        // held uninitialised stack there; we write zero, which is the whole of the
        // documented deviation from the captured bytes.
        assert_eq!(&generated[6..8], &[0, 0], "the pad is ours and is zero");
    }

    /// Every frame the world-entry burst generates must have the size the reference
    /// server sent for the same opcode.
    ///
    /// Length is the one property of a generated frame that cannot be argued about
    /// from the struct declarations alone — `sizeof` depends on the shipped build's
    /// packing — so it is pinned to the capture's measurements rather than to a
    /// reviewer's arithmetic. This is the check that would have caught a
    /// mis-derived `cltHorse`.
    ///
    /// The right-hand side is [`gz_proto::captured_lengths`], which carries each
    /// number's origin; the left is what this port's encoders produce. If they ever
    /// diverge, the client walks the frame at the wrong stride and faults silently.
    #[test]
    fn every_generated_burst_frame_matches_its_captured_length() {
        use gz_proto::captured_lengths as captured;

        let generated: &[(GameMsg, usize, usize)] = &[
            (
                GameMsg::ResponseAllcharinfoone,
                ALL_CHAR_INFO_ONE_LEN,
                captured::CAPTURED_ALL_CHAR_INFO_ONE_LEN,
            ),
            (
                GameMsg::ResponseAllcharinfotwo,
                ALL_CHAR_INFO_TWO_LEN,
                captured::CAPTURED_ALL_CHAR_INFO_TWO_LEN,
            ),
            (
                GameMsg::ResponseName,
                RESPONSE_NAME_LEN,
                captured::CAPTURED_RESPONSE_NAME_LEN,
            ),
            (
                GameMsg::ResponsePersonHorse,
                PERSON_HORSE_LEN,
                captured::CAPTURED_PERSON_HORSE_LEN,
            ),
            (
                GameMsg::ResponseChangehorse,
                CHANGE_HORSE_LEN,
                captured::CAPTURED_CHANGE_HORSE_LEN,
            ),
            (
                GameMsg::ResponseSkillbookMagicinfoGet,
                SKILLBOOK_MAGIC_INFO_LEN,
                captured::CAPTURED_8371_LEN,
            ),
            (
                GameMsg::ResponseFamilyUpdate,
                FAMILY_UPDATE_LEN,
                captured::CAPTURED_8271_LEN,
            ),
            (
                GameMsg::ResponseGetvillageinfo,
                GET_VILLAGE_INFO_LEN,
                captured::CAPTURED_7203_LEN,
            ),
        ];

        for &(msg, declared, was) in generated {
            assert_eq!(
                declared, was,
                "declared length for opcode {} disagrees with the captured measurement",
                msg as u16
            );
        }
    }

    /// 244 is the nameplate, and it is keyed by a character id taken from the
    /// payload rather than from the frame's `id`.
    ///
    /// `Client-SystemNPC.cpp:977` indexes `CharUniqueInfo` with
    /// `pclinfo->siCharUnique`, so a wrong value here silently renames somebody
    /// else — or nobody. The retired replay mode did exactly that: the captured
    /// frame carried 1471 *and* `Meirin`, and 1471 resolved to our own slot.
    #[test]
    fn generated_name_is_keyed_by_the_session_id_and_the_rows_name() {
        let body = encode_response_name(1471, "Tester");

        assert_eq!(
            body.len(),
            gz_proto::captured_lengths::CAPTURED_RESPONSE_NAME_LEN
        );
        // `siCharUnique` @ 0, `szName[20]` @ 4 (`MsgType.h:3199-3200`).
        assert_eq!(i32::from_le_bytes(body[0..4].try_into().unwrap()), 1471);
        assert_eq!(&body[4..10], b"Tester");
        assert!(body[10..].iter().all(|&b| b == 0), "NUL padded");
        // The name must come from the row, never from a constant.
        assert_ne!(
            &body[4..10],
            b"Meirin",
            "the capture's name must never leak"
        );
    }

    /// 258 is the unmount, and it only unmounts if its `siCharUnique` resolves to
    /// the local player.
    ///
    /// `Client-Person.cpp:1065-1071` does
    /// `CR[CharUniqueInfo[siCharUnique]]->DeleteSlave()` and returns early when the
    /// index does not resolve, so a stale mount survives unless this frame is
    /// stamped with the session's id. The capture put 1471 at offset 112 — the last
    /// real field before the tail padding — and so must we.
    #[test]
    fn generated_change_horse_dismounts_our_own_character() {
        let body = encode_change_horse(1471);

        assert_eq!(
            body.len(),
            gz_proto::captured_lengths::CAPTURED_CHANGE_HORSE_LEN
        );
        // `siChangeHorseIndex` @ 0 is the legacy's "show horse 0" value, so index 0
        // resolves to the empty `cltChangeHorse` rather than a named mount.
        assert_eq!(i16::from_le_bytes([body[0], body[1]]), 0);
        // `cltChangeHorse` @ 4..112 zeroed; `siHorseUnique` and `szName` included.
        assert!(body[4..112].iter().all(|&b| b == 0));
        // `siCharUnique` @ 112 -- the capture's offset.
        assert_eq!(i16::from_le_bytes([body[112], body[113]]), 1471);
    }

    /// 226 lists the five horse slots; empty means unmounted.
    ///
    /// The header is `bool bAutoFoodSwitch`, `bool siCurrentHorseRIDEHORSE`,
    /// `SI16 siCurrentShowHorse` (`Horse.h:223-227`). The horse array starts at 28,
    /// which is 24 bytes past what those three members account for; see
    /// [`PERSON_HORSE_HEADER_LEN`]. The capture was riding and held exactly one
    /// horse at offset 28 — `Foal744`, preceded by `HORSEUNIQUE_FOAL` = 12 as an
    /// `SI16`, a nine-byte marker.
    #[test]
    fn generated_person_horse_is_empty_and_unmounted() {
        let body = encode_person_horse();

        assert_eq!(
            body.len(),
            gz_proto::captured_lengths::CAPTURED_PERSON_HORSE_LEN
        );
        assert_eq!(PERSON_HORSE_LEN, 568, "28 header + 5 * 108");
        assert!(body.iter().all(|&b| b == 0), "no horse at all");
        // The stride is pinned by the capture independently: 568 = 28 + 5 * 108,
        // where 108 is `sizeof(cltHorse)` as it travels.
        assert_eq!(
            PERSON_HORSE_LEN,
            PERSON_HORSE_HEADER_LEN + 5 * gz_proto::game::CLT_HORSE_LEN
        );
    }

    /// The empty world frames are zero-filled and keep the capture's sizes.
    ///
    /// All three legacy constructors are `ZeroMemory(this, sizeof(*this))`
    /// (`MsgType-Item.h:1874`, `MsgType-Person.h:2997`, `MsgType.h:3119`), so zero
    /// is the encoding the legacy itself uses for "none" rather than a stand-in for
    /// data we failed to model.
    #[test]
    fn the_empty_world_frames_are_zero_and_capture_sized() {
        use gz_proto::captured_lengths as captured;

        for (msg, body, was) in [
            (
                GameMsg::ResponseSkillbookMagicinfoGet as u16,
                encode_skillbook_magic_info(),
                captured::CAPTURED_8371_LEN,
            ),
            (
                GameMsg::ResponseFamilyUpdate as u16,
                encode_family_update(),
                captured::CAPTURED_8271_LEN,
            ),
            (
                GameMsg::ResponseGetvillageinfo as u16,
                encode_get_village_info(),
                captured::CAPTURED_7203_LEN,
            ),
        ] {
            assert_eq!(body.len(), was, "opcode {msg}");
            assert!(
                body.iter().all(|&b| b == 0),
                "opcode {msg} must be all zero"
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

    /// The generated world-entry frames keep the sizes the reference server sent.
    ///
    /// The zero-filled ones are the honest encoding: their legacy constructors are
    /// `ZeroMemory(this, sizeof(*this))`, so zero is what the legacy itself means
    /// by "none" rather than a stand-in for data we failed to model.
    #[test]
    fn the_zero_state_frames_match_the_capture_lengths() {
        use gz_proto::captured_lengths as captured;

        let cases: &[(u16, Vec<u8>, usize)] = &[
            (
                GameMsg::ResponsePersonQuest as u16,
                world_entry::person_quest_empty(),
                captured::CAPTURED_225_LEN,
            ),
            (
                GameMsg::ResponseSoulguardGet as u16,
                world_entry::soulguard_get_empty(),
                captured::CAPTURED_8285_LEN,
            ),
            (
                GameMsg::ResponseSoulguardHouseGet as u16,
                world_entry::soulguard_house_get_default(),
                captured::CAPTURED_8286_LEN,
            ),
            (
                GameMsg::ResponseGetMarryInfo as u16,
                world_entry::marry_info_empty(),
                captured::CAPTURED_6504_LEN,
            ),
            (
                GameMsg::ResponseEnemycaptureall as u16,
                world_entry::enemy_capture_all_clear(),
                captured::CAPTURED_ENEMY_CAPTURE_ALL_LEN,
            ),
            (
                GameMsg::ResponseItemmallexistence as u16,
                world_entry::item_mall_existence_absent(),
                captured::CAPTURED_6801_LEN,
            ),
            (
                world_entry::OP_8464,
                world_entry::zero_frame(world_entry::OP_8464, world_entry::OP_8464_LEN),
                captured::CAPTURED_ITEM_TYPE_INFO_LEN,
            ),
            (
                world_entry::OP_8470,
                world_entry::zero_frame(world_entry::OP_8470, world_entry::OP_8470_LEN),
                captured::CAPTURED_8470_LEN,
            ),
        ];
        for &(cmd, ref body, was) in cases {
            assert_eq!(body.len(), was, "opcode {cmd}");
        }
        // 8286's only non-zero is the capacity default.
        let house = world_entry::soulguard_house_get_default();
        assert_eq!(&house[0..2], &[0x02, 0x00], "default capacity 2");
        assert!(house[2..].iter().all(|&b| b == 0));
        // 8178 needs the session id to start at offset 0. The id is a parameter, not
        // the retired capture's — the whole point of generating it.
        let elixir = world_entry::elixir_set_empty(0x05BF);
        assert_eq!(&elixir[0..4], &[0xBF, 0x05, 0x00, 0x00]);
        assert!(elixir[4..].iter().all(|&b| b == 0));
        assert_eq!(
            world_entry::elixir_set_empty(7)[0..4],
            [7, 0, 0, 0],
            "the id comes from the session, not a constant"
        );
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
    /// (`char-move.cpp:145`). With `percent=-50` (steady-state start), a single
    /// capped tick at speed 14 yields 98 points (< 100 needed), so the step is
    /// deferred to the next tick — still at most one tile per cap window.
    #[test]
    fn a_stalled_tick_commits_at_most_one_tile() {
        let mut movement = Movement::new((10, 0), (0, 0), 1, 14);
        let stalled = movement.last + Duration::from_secs(4);
        // At speed 14, a 350 ms capped tick gives 98 points; percent goes -50 -> 48.
        // No step yet, but crucially: it does NOT jump multiple tiles.
        assert!(movement.advance(stalled).is_none());
        assert_eq!(movement.pos, (0, 0));
        // A subsequent normal tick would commit the step.
        let nudge = movement.last + Duration::from_millis(10);
        assert_eq!(movement.advance(nudge), Some((1, 0)));
        assert_eq!(movement.pos, (1, 0));
    }

    /// `next_delay` never exceeds the legacy cap, so the read loop's timer arm
    /// wakes at the same bound the legacy's `min((CHAR_ACTION_INTERVAL_FRAME+1)*50, ...)`
    /// applies.
    #[test]
    fn next_delay_is_capped_at_the_legacy_tick_bound() {
        let mut movement = Movement::new((10, 0), (0, 0), 1, 14);
        // With percent=-50, the first step needs 100 points.
        // 350 ms capped tick gives 14*350/50 = 98 points, not enough.
        let now = movement.last + Duration::from_millis(350);
        assert!(movement.advance(now).is_none()); // 98 points, percent = 48
        // next_delay() needs 2 more points: ceil(2*50/14) = 8 ms (under cap).
        // The cap only applies when needed delay > 350 ms.
        // So after a partial tick, next_delay() is small.
        // To hit the cap, we need a speed where steady-state delay > 350 ms.
        // Speed 13: 5000/13 = 385 ms > 350 ms cap.
        let mut capped = Movement::new((10, 0), (0, 0), 1, 13);
        let start = capped.last;
        // One capped tick (350 ms) gives 13*350/50 = 91 points.
        assert!(capped.advance(start + Duration::from_millis(350)).is_none());
        // Still need 9 more points: ceil(9*50/13) = 35 ms. Not capped yet.
        // Need a case where needed > 350.
        // Speed 10: 5000/10 = 500 ms > 350 ms.
        let mut slow = Movement::new((10, 0), (0, 0), 1, 10);
        let start = slow.last;
        // Full steady-state delay is 500 ms, but next_delay() caps at 350.
        assert_eq!(slow.next_delay(), Duration::from_millis(350));
        // After 350 ms: 10*350/50 = 70 points. Need 30 more = 150 ms.
        assert!(slow.advance(start + Duration::from_millis(350)).is_none());
        assert_eq!(slow.next_delay(), Duration::from_millis(150)); // under cap
    }

    /// The cadence follows the character, not a fixed reference: `advance` banks
    /// `move_speed * elapsed / 50`, so a slower mover takes proportionally longer
    /// per tile. At speed 7 a tile needs `5000 / 7 ≈ 715` ms against 357 ms at
    /// the captured 14. With `percent=-50`, the first tile already uses the
    /// steady-state delay (no cold-start half-interval).
    ///
    /// Note: the legacy cap of 350 ms per tick means a single `advance()` call
    /// with a large elapsed time is capped. These tests use speeds where the
    /// steady-state delay fits under the cap, or pass the exact steady-state
    /// delay in one call.
    #[test]
    fn the_step_cadence_scales_with_the_characters_move_speed() {
        // Speed 16: 5000/16 = 312.5 -> 313 ms < 350 ms cap, fits in one tick.
        let mut slow = Movement::new((1, 0), (0, 0), 1, 16);
        let start = slow.last;
        // Full 313 ms commits exactly one (16*313/50 = 100).
        assert_eq!(
            slow.advance(start + Duration::from_millis(313)),
            Some((1, 0))
        );

        // Speed 28: 5000/28 = 178.6 -> 179 ms, well under cap.
        let mut fast = Movement::new((1, 0), (0, 0), 1, 28);
        let start = fast.last;
        // 179 ms commits a tile at 28 (28*179/50 = 100).
        assert_eq!(
            fast.advance(start + Duration::from_millis(179)),
            Some((1, 0))
        );
        assert_eq!(
            fast.next_delay(),
            Duration::from_millis(179),
            "steady-state delay at speed 28"
        );
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
            class_kind: 1,
            life: 161,
            move_speed: 12,
            sex: 1,
            gm_level: 0,
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
