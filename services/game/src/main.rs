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
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gz_cipher::Cipher;
use gz_db::{
    FriendRepository, PgAccountRepository, PgCharacterRepository, PgFriendRepository,
    PgItemRepository, PgQuestRepository, QuestRepository,
};
use gz_net::session::{Rejection, Session};
use gz_net::{COMPRESSION_FLAG, Frame, FrameParser, HEADER_SIZE, MAX_FRAME_SIZE, Tick};
use gz_proto::chat::{
    decode_chat_request, decode_head_chat_request, decode_makesecret_chat_channel_request,
    decode_secret_chat_request,
};
use gz_proto::combat::{
    DIE_MODE_NORMAL, DeathPenaltyResponse, HIT_TARGET_PLAYER, HitResponse, RequestHit,
    ReviveResponse,
};
use gz_proto::game::{
    CharUnique, GameLogin, decode_select_char, decode_village_struct_request, encode_person_ip,
    encode_person_private_info, encode_response_name,
};
use gz_proto::map_geometry::cell_of;
use gz_proto::opcodes_game::GameMsg;
use gz_proto::order::{ORDER_ATTACK, ORDER_MOVE, OrderData};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;

type AnyError = Box<dyn std::error::Error + Send + Sync>;

mod auth;
mod chat;
mod combat;
mod config;
mod friends;
mod geography;
mod items;
mod monsters;
mod movement;
mod quest;
mod registry;
mod stats;
mod world;
mod world_entry;

use combat::{HIT_THROTTLE_MS, PlayerAttacks, Rng, Swing, player_swing, start_approach};
use config::GameConfig;
use registry::{MAX_CHAR_NUMBER, OUTBOUND_CAP, Outbound, Registered, Registry, SessionInfo};

// The auth/character-lifecycle family lives in `auth.rs`. The dispatcher calls
// the login/selectchar/make-new-char/delete handlers; `send_char_list`,
// `admit_select_char` and `admit_game_login` are reached from the `mod tests`
// below through `super::*`, hence the allow.
#[allow(unused_imports)]
pub(crate) use auth::{
    admit_game_login, admit_make_new_char_slot, admit_select_char, handle_game_login,
    handle_make_new_char, handle_select_char, handle_userchar_deletion, send_char_list,
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
    MakeSecretChatChannelRequest, SECRETCHAT_FAIL_INVALIDNAME, SECRETCHAT_FAIL_NOTUSER,
    SECRETCHAT_SUCCESS, SecretChatRequest, handle_chat, handle_head_chat_request,
    handle_makesecret_chat_channel, handle_secret_chat, route_chat, whisper_parts,
};

// The friend family lives in `friends.rs`; the crate root re-exports the
// handlers the 6200/6202/6209 dispatch arms call so the `mod tests` below can
// reach them through `super::*`.
#[allow(unused_imports)]
pub(crate) use friends::{
    friend_all_list_body, handle_friend_add, handle_friend_add_accept, handle_friend_list,
};

#[allow(unused_imports)]
pub(crate) use quest::{
    BEGINNER2_CHAIN, ChainQuest, QuestActor, QuestOutcome, SlotOccupancy, apply_cancel_quest,
    apply_quest_going, apply_quest_status, handle_cancel_quest, handle_quest_status,
    handle_set_quest_going, push_quest_state,
};

// The movement family lives in `movement.rs`. The dispatcher calls the request
// handlers and `next_move_deadline`; the `mod tests` below reach `Movement` and
// the frame builders through `super::*`, hence the allow.
#[allow(unused_imports)]
pub(crate) use movement::{
    Movement, char_update_body, fan_char_update, handle_leave_request, handle_revive,
    handle_sit_down, handle_userorder, logout_departed, next_move_deadline, send_char_update,
    step_movement,
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
    handle_inv_cancellation_sealing_up, handle_inv_drop, handle_inv_pickup, handle_item_move,
    handle_item_split, handle_personal_shop_data,
};

// The world family lives in `world.rs`. The dispatcher calls 1101 and world
// entry; `LiveFrames`, `announce_nearby_characters` and the char-info builders
// are reached from the `mod tests` below and from the movement module through
// `super::*`/`crate::*`, hence the allow.
#[allow(unused_imports)]
pub(crate) use world::{
    LIVE_CHARACTER_STATE, LiveFrames, announce_map_npcs, announce_nearby_characters,
    character_cell, handle_village_struct, make_char_info_for, make_char_info_from_session,
    send_character_state, send_world_entry, server_info,
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

/// Upper bound on one connection's blocked `write_all`/`flush`
/// (`send_raw`). A peer that stops reading fills the kernel send buffer and
/// the write never returns; the timeout turns that stall into an error so the
/// read loop exits and `Registered`'s drop releases the registry entry and
/// login claim. Ten seconds is generous for a healthy client — one near-block
/// fan is a few kilobytes — and matches the handshake budget.
const WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Concurrent connections the accept loop will spawn a handler for.
///
/// Once the semaphore is exhausted, `serve` drops the accepted socket
/// immediately instead of spawning: a flood of connects can no longer grow an
/// unbounded pile of tasks. Legitimate capacity is far above any real shard's
/// population; the bound only matters under abuse.
const MAX_CONCURRENT_CONNS: usize = 512;

/// TCP keepalive schedule, applied to every accepted socket.
///
/// An established session is never idle-kicked (the legacy wasn't), so a peer
/// that vanishes behind a NAT without a FIN would otherwise stay registered
/// forever. Keepalive probes surface the death as a socket error, which tears
/// the read loop down and releases the registry entry. The idle time is the
/// time before the first probe; the interval spaces the follow-up probes.
const KEEPALIVE_IDLE: std::time::Duration = std::time::Duration::from_secs(60);
const KEEPALIVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

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
    let friends = PgFriendRepository::connect(&cfg.database_url).await?;
    let quests = PgQuestRepository::connect(&cfg.database_url).await?;
    // One table for every connection: the chat family is the first code here
    // that addresses players other than the one asking.
    let registry = Arc::new(Registry::new());
    // The map-0 field monsters. A failed load (a missing or unreadable data
    // tree) leaves the world empty rather than refusing to start, the same
    // posture the quest load takes.
    // A mutex, because Phase 2 mutates them: HP, position, death and respawn all
    // change on every tick. Critical sections are synchronous and no guard is
    // ever held across an await.
    let monsters = match monsters::MonsterWorld::load(&cfg.data_root) {
        Ok(mut world) => {
            world.spawn_all(&registry);
            logln!(
                "game: spawned {} field monster(s) on map {} from {}",
                world.monster_count(),
                monsters::FIELD_MAP,
                cfg.data_root.display()
            );
            Arc::new(Mutex::new(world))
        }
        Err(e) => {
            logln_err!("game: {e}; no field monsters spawned");
            Arc::new(Mutex::new(monsters::MonsterWorld::empty()))
        }
    };
    // A player's own attack numbers, joined once at boot (see
    // [`PlayerAttacks`]). The same degradation: no data tree, no combat, and a
    // server that still starts.
    let player_attacks = Arc::new(match PlayerAttacks::load(&cfg.data_root) {
        Ok(attacks) => attacks,
        Err(e) => {
            logln_err!("game: {e}; players attack with the level term only");
            PlayerAttacks::default()
        }
    });
    // The field-monster AI. One pass every `AI_TICK_MS`, entirely under the
    // world lock and never across an await.
    {
        let registry = registry.clone();
        let monsters = monsters.clone();
        tokio::spawn(async move {
            let mut rng = Rng::new(now_tick());
            loop {
                tokio::time::sleep(Duration::from_millis(monsters::AI_TICK_MS)).await;
                let now = now_tick();
                let hits = {
                    let mut world = monsters
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    world.tick(&registry, now, &mut rng)
                };
                for hit in hits {
                    // The 268 answers the *target*, so everyone near the struck
                    // player sees the same frame (`Server-Attack.cpp:2775`).
                    registry.broadcast_near_point(
                        monsters::FIELD_MAP,
                        hit.x,
                        hit.y,
                        GameMsg::ResponseHit as u16,
                        HitResponse {
                            char_unique: hit.char_unique,
                            damage: hit.damage,
                            double_damage: 0,
                            life: hit.life as i32,
                            mode: 0,
                            special_attack_switch: 0,
                            b_indure: false,
                            b_miss_hit: false,
                            b_dodge_hit: false,
                            ui_type: HIT_TARGET_PLAYER,
                        }
                        .encode()
                        .to_vec(),
                    );
                    // A player's death keeps its body, and it gets a prompt:
                    // the legacy's kill branch runs `DeathPenalty()` and then
                    // `RequestRevive(id, NORMAL)` back to back
                    // (`CharManager.cpp:362-363`), and those two frames are the
                    // only things that open the client's death dialog and arm the
                    // revive button inside it. A server that sends neither leaves
                    // the body on the ground with no way back — the live report
                    // from Test12.
                    if hit.life == 0 {
                        let dead = hit.char_unique;
                        registry.send_to(
                            hit.key,
                            GameMsg::ResponseDealthpenaltyinfo as u16,
                            DeathPenaltyResponse {
                                si_money: 0,
                                si_exp: 0,
                                si_killed_mode: 0,
                                sz_kill_me_char_name: String::new(),
                            }
                            .encode()
                            .to_vec(),
                        );
                        registry.send_to(
                            hit.key,
                            GameMsg::ResponseRevive as u16,
                            ReviveResponse {
                                si_char_unique: hit.char_unique,
                                si_param: 1,
                                si_die_mode: DIE_MODE_NORMAL,
                            }
                            .encode()
                            .to_vec(),
                        );
                        logln!(
                            "game: character {dead} died; sent the death dialog \
                             and armed the revive prompt"
                        );
                    }
                }
            }
        });
    }
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

    // Bounded concurrency: one permit per live handler, taken before the
    // spawn. [`admit`] is a seam so the cap logic is testable without a
    // listener.
    let conn_slots = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_CONNS));

    loop {
        let (stream, peer) = listener.accept().await?;
        // Keepalive so a peer that vanishes without a FIN surfaces as a socket
        // error instead of a forever-registered session (established sessions
        // are never idle-kicked).
        if let Err(e) = socket2::SockRef::from(&stream).set_tcp_keepalive(
            &socket2::TcpKeepalive::new()
                .with_time(KEEPALIVE_IDLE)
                .with_interval(KEEPALIVE_INTERVAL),
        ) {
            logln_err!("game: {peer}: set keepalive: {e}");
        }
        let Some(permit) = admit(&conn_slots) else {
            // At capacity: drop the socket now, before anything is cloned or
            // spawned. The client sees a refused connection rather than a
            // silently queued one.
            logln!("game: {peer}: {MAX_CONCURRENT_CONNS} connections in flight, dropping");
            continue;
        };
        let accounts = accounts.clone();
        let characters = characters.clone();
        let items = items.clone();
        let friends = friends.clone();
        let quests = quests.clone();
        let registry = registry.clone();
        let monsters = monsters.clone();
        let player_attacks = player_attacks.clone();
        let peer = peer.to_string();
        let cfg = cfg.clone();
        tokio::spawn(async move {
            // Held for the whole session; the slot frees when the handler ends.
            let _permit = permit;
            if let Err(e) = handle_client(
                stream,
                &peer,
                &cfg,
                accounts,
                characters,
                items,
                friends,
                quests,
                registry,
                monsters,
                player_attacks,
            )
            .await
            {
                logln_err!("game: {peer}: {e}");
            }
        });
    }
}

/// Takes a connection slot, or `None` when the server is at
/// [`MAX_CONCURRENT_CONNS`].
///
/// A separate function so the cap is testable: exhaust the semaphore, call
/// `admit`, and assert `None` — no listener, no handler needed. The permit is
/// owned so it can be moved into the handler task.
fn admit(
    slots: &std::sync::Arc<tokio::sync::Semaphore>,
) -> Option<tokio::sync::OwnedSemaphorePermit> {
    slots.clone().try_acquire_owned().ok()
}

/// One turn of the read loop's `select!`.
///
/// Four things can end the wait: a frame from the peer, a frame queued for
/// this connection by a broadcast ([`Outbound`]), the connection being closed
/// from elsewhere (a duplicate login kicking the holder), or the window that
/// bounds the wait (the handshake deadline before the first frame, the next
/// movement step after it). Every branch is cancel-safe — `TcpStream::read`
/// consumes nothing while pending, an `mpsc` `recv` that loses the race has
/// taken nothing, `Notify::notified` leaves its permit intact, and a sleep
/// holds no state — so a frame cannot be lost by the switch.
enum LoopTick {
    /// Bytes arrived (or the socket reported EOF, or the read failed).
    Read(std::io::Result<usize>),
    /// A queued outbound frame is due. `None` would mean every sender is gone,
    /// which cannot happen while this task holds `out_tx`.
    Send(Option<Outbound>),
    /// The connection was closed from elsewhere, with no frame to lose.
    Closed,
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
#[allow(clippy::too_many_arguments)]
async fn handle_client(
    mut stream: TcpStream,
    peer: &str,
    cfg: &GameConfig,
    accounts: PgAccountRepository,
    characters: PgCharacterRepository,
    items: PgItemRepository,
    friends: PgFriendRepository,
    quests: PgQuestRepository,
    registry: Arc<Registry>,
    monsters: Arc<Mutex<monsters::MonsterWorld>>,
    player_attacks: Arc<PlayerAttacks>,
) -> Result<(), AnyError> {
    let mut session_state = Session::new(cfg.timeouts);
    if !session_state.accept(now_tick()) {
        // Unreachable: a fresh Session is always None, and ITZSession::SetSocket
        // has the same guard, so the legacy has the same dead branch.
        return Err("fresh session refused its own socket".into());
    }
    // The handshake budget is an absolute deadline pinned at accept, not a
    // fresh window per read-loop turn: re-arming it from "now" every turn is
    // what let a peer that dribbled one frame per window stay unhandshaken
    // indefinitely. One clock, set once. On tokio's clock, because that is
    // what arms the sleep (and what a paused-clock test can control).
    let handshake_deadline = tokio::time::Instant::now() + cfg.timeouts.handshake;
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
    // The auto-attack running on this connection, if any: the monster being
    // swung at and when the next swing falls due.
    //
    // This has to live on the server because the client never repeats it —
    // `cltOrder::MakeOrder` pushes one buffer entry per click and the mouse
    // handler then clears `siSelectedOrder` (`order.cpp:182`,
    // `MouseOperation.cpp:771`), so one `ORDER_ATTACK` has to buy the whole
    // fight. That is also why the legacy sets a persistent order rather than
    // answering a hit per request (`char-order.cpp:234-238`).
    let mut attack: Option<(u16, Instant)> = None;
    // The monster this connection is *walking toward*, when that walk is an
    // approach rather than a player's own `ORDER_MOVE`. The swing arm plans one
    // approach per tick; without this the chase only advanced once per attack
    // interval, so a monster that kept moving was never caught (live log,
    // 2026-10-17: the player followed a raccoon for four seconds at one tile a
    // second and every swing came back `OutOfRange`). Cleared the moment the
    // swing lands, the target drops, or the player walks off.
    let mut approach: Option<u16> = None;
    // Which frames get built from PostgreSQL on the way out.
    let mut live = LiveFrames {
        items: &items,
        account_id: 0,
        account_name: String::new(),
        character: None,
        char_unique,
        friend_list: Vec::new(),
        quest: gz_proto::quest::PersonQuest::new(),
    };

    // The registry key this connection answers under, plus the removal guard.
    // The guard is armed here — before the first frame — so that every exit
    // (EOF, framing rejection, cipher desync, handshake timeout) cleans up after
    // a character that *did* register, while a drop before `REQUEST_SELECTCHAR`
    // is the no-op it should be. See `Registered`.
    // The combat throttle and this connection's own roll stream. The legacy's
    // clock is per-character (`CheckLastMsgClock`, `Server-Person.cpp:2760`);
    // per-connection is the same thing here, because one connection is one
    // character.
    let mut last_attack_ms: u64 = 0;
    let mut combat_rng = Rng::new(now_tick() ^ u64::from(char_unique as u32));
    let session_key = Registry::next_key();
    let _registered = Registered::new(registry.clone(), session_key);
    // Woken by [`Registry::claim_login`] when a duplicate login for this account
    // arrives. The legacy closes *both* connections on a duplicate
    // (`Server-Auth.cpp:51-120`); this is how the holder's read loop learns it
    // must disconnect.
    let close = Arc::new(Notify::new());
    // Everything aimed at this connection but produced elsewhere — a whisper,
    // a head-chat fan, the general-chat broadcast this speaker is also part of
    // — is queued here and written by this loop, so only this task ever writes
    // to the socket (`Outbound`).
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<Outbound>(OUTBOUND_CAP);

    // The `(map, cell)` the session's monsters were last announced for, so the
    // loop below can tell a walk into a new cell (or a warp) from standing
    // still. Set at world entry, where the first announce happens.
    let mut last_seen: Option<(i32, (i32, i32))> = None;

    loop {
        // Monsters the session has newly stepped or warped into view. The design
        // puts monsters on the same neighbour mechanism as characters (§10.2 of
        // `DESIGN_MONSTER_COMBAT_AND_AUTOHUNT.md`); with a separate store that is
        // this one check, and it covers a walk cell crossing, a gate and a map
        // change alike. A map change is a fresh entry — the old map's cells mean
        // nothing on the new one — so the whole block is announced.
        if session_state.state().is_active()
            && let Some(info) = registry.info(session_key)
        {
            let cell = cell_of(info.x, info.y, info.map_id);
            match last_seen {
                Some((map_id, was)) if map_id == info.map_id => {
                    if was != cell {
                        monsters
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .announce_new_cells(&registry, session_key, info.map_id, was, cell);
                    }
                }
                _ => monsters
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .announce_near(&registry, session_key, info.map_id, info.x, info.y),
            }
            last_seen = Some((info.map_id, cell));
        }
        // The handshake window runs from accept — `handshake_deadline`, pinned
        // once when the connection arrived — to the first inbound frame, and
        // each turn arms only the time *left* on that clock. Past it, a peer
        // that has said nothing (or dribbled just enough to look busy) is a
        // silent half-open connection and gets reclaimed rather than parked
        // forever -- the fix the legacy's 10 s check does not deliver. See
        // gz_net::session.
        //
        // On a live session the wait instead runs until the next thing this
        // connection owes the world: a step landing, or a swing falling due,
        // whichever comes first. An established session has no idle window at
        // all; liveness is TCP keepalive's job.
        let read_window = if !session_state.state().is_active() {
            Some(handshake_deadline.saturating_duration_since(tokio::time::Instant::now()))
        } else {
            let now = Instant::now();
            movement
                .as_ref()
                .and_then(|mv| mv.deadline)
                .into_iter()
                .chain(attack.as_ref().map(|&(_, due)| due))
                .min()
                .map(|due| due.saturating_duration_since(now))
        };
        let tick = tokio::select! {
            read = stream.read(&mut buf) => LoopTick::Read(read),
            queued = out_rx.recv() => LoopTick::Send(queued),
            _ = close.notified() => LoopTick::Closed,
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
            LoopTick::Closed => {
                // A duplicate login for this account woke us. Its notice was
                // queued to `out_rx` before the wake, so drain and write it —
                // the legacy sends `GAMEMSG_RESPONSE_ANOTHERAUTH` and only then
                // closes (`Server-Auth.cpp:100-106`). Draining also flushes any
                // broadcast that raced the kick.
                while let Ok(frame) = out_rx.try_recv() {
                    send_raw(&mut stream, &mut outbound, frame.cmd, &frame.payload).await?;
                }
                logln!("game: {peer}: closed by a duplicate login for this account");
                session_state.close(now_tick());
                return Ok(());
            }
            LoopTick::TimedOut => {
                if session_state.state().is_active() {
                    // Movement deadline fired: step if a tile is due.
                    if movement.is_some() {
                        step_movement(
                            &mut stream,
                            &mut outbound,
                            &mut live,
                            &mut movement,
                            &registry,
                            session_key,
                        )
                        .await?;
                        // Re-arm the deadline from the (potentially updated) movement.
                        if let Some(mv) = movement.as_mut() {
                            mv.deadline = Some(next_move_deadline(mv, Instant::now()));
                        }
                    }
                    // A finished approach must not sit idle until the next swing
                    // tick: against a monster that keeps moving that stalls the
                    // chase at one tile per attack interval. Keep walking at move
                    // speed and the already-armed swing lands as soon as the gap
                    // closes, instead of the player trailing the target forever.
                    if movement.is_none()
                        && let Some(target) = approach
                        && let Some(character) = live.character.as_ref()
                    {
                        start_approach(
                            monsters.as_ref(),
                            player_attacks.as_ref(),
                            &mut movement,
                            target,
                            (character.pos_x, character.pos_y),
                            character,
                        );
                    }
                    // An auto-attack swing that has fallen due. The client
                    // queued `ORDER_ATTACK` once and never repeats it — one
                    // buffer entry per click, then `siSelectedOrder` is cleared
                    // (`order.cpp:182`, `MouseOperation.cpp:771`) — so keeping
                    // the cadence is this loop's job.
                    if let Some((target, due)) = attack
                        && due <= Instant::now()
                    {
                        attack = match live.character.as_ref() {
                            Some(character) => {
                                let (ax, ay) = match movement.as_ref() {
                                    Some(mv) => mv.pos,
                                    None => (character.pos_x, character.pos_y),
                                };
                                let class_kind = u16::try_from(character.class_kind).unwrap_or(1);
                                let outcome = player_swing(
                                    &registry,
                                    monsters.as_ref(),
                                    player_attacks.as_ref(),
                                    &mut combat_rng,
                                    &characters,
                                    &items,
                                    session_key,
                                    &login,
                                    character,
                                    (ax, ay),
                                    target,
                                )
                                .await?;
                                // A landing swing is self-evident — the client
                                // prints the number — so only the two ways it
                                // fails are worth a line.
                                if outcome != Swing::Landed {
                                    logln!(
                                        "game: {peer}: swing at {target} did not land: \
                                         {outcome:?}"
                                    );
                                }
                                // The gap gets closed instead of refused:
                                // `ORDER_ATTACK` sets `CHARWORKSTEP_DECIDELENGTH`
                                // and keeps the fight in the legacy
                                // (`char-order.cpp:232-238`), and the client
                                // walks its own copy of that step *before* it
                                // sends the order (`Client.cpp:3297` `LetOrder`)
                                // — which is why the avatar closed the gap while
                                // this server's own tile stayed put and every
                                // swing came back `OutOfRange`. A target that
                                // merely stepped aside stays a target as well; a
                                // player who walks off issues `ORDER_MOVE`, and
                                // that is still what ends the attack.
                                //
                                // One walk at a time: a movement already running
                                // is either this approach or the player's own
                                // ordered walk, and only the player's replaces it.
                                if outcome == Swing::OutOfRange && movement.is_none() {
                                    let range = player_attacks.range(class_kind);
                                    match start_approach(
                                        monsters.as_ref(),
                                        player_attacks.as_ref(),
                                        &mut movement,
                                        target,
                                        (ax, ay),
                                        character,
                                    ) {
                                        Some(dest) => {
                                            approach = Some(target);
                                            logln!(
                                                "game: {peer}: out of range of {target} (range \
                                                 {range}): walking ({ax}, {ay}) -> ({}, {})",
                                                dest.0,
                                                dest.1
                                            );
                                        }
                                        None => logln!(
                                            "game: {peer}: out of range of {target} (range \
                                             {range}) from ({ax}, {ay}) and no approachable tile"
                                        ),
                                    }
                                }
                                // Whatever the swing did, only `OutOfRange` still
                                // has a walk to keep planning.
                                if outcome != Swing::OutOfRange {
                                    approach = None;
                                }
                                (outcome != Swing::NoTarget).then(|| {
                                    (
                                        target,
                                        Instant::now()
                                            + Duration::from_millis(
                                                player_attacks.interval(class_kind),
                                            ),
                                    )
                                })
                            }
                            None => None,
                        };
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
            // The AFK clock reads here, before dispatch, so no arm's
            // `continue` can skip it; which frames reset it is
            // `Registry::note_activity`'s table.
            registry.note_activity(session_key, cmd, now_tick());

            match cmd {
                c if c == GameMsg::RequestLogin as u16 => {
                    let request = GameLogin::decode(payload.as_ref())?;
                    login = Some(
                        handle_game_login(
                            &mut stream,
                            &mut outbound,
                            &accounts,
                            &characters,
                            &registry,
                            session_key,
                            close.clone(),
                            out_tx.clone(),
                            peer,
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
                        close.clone(),
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
                    // Capture the full stat block for the 214 inspect: another
                    // player's pull reads it out of the registry rather than the
                    // database (`Server-Person.cpp:83`).
                    if let Some(session) = login.as_ref()
                        && let Ok(basic) =
                            basic_info_for(session.account_id, &session.username, &character_id)
                    {
                        registry.set_basic_info(session_key, basic);
                    }
                    // Hydrate the friend list from PostgreSQL, then tell any
                    // online friend that this character logged in (6207) — the
                    // legacy does both when a character's DB friend list lands
                    // (`ServerDB-Friend.cpp:23-52`). The 6219 world-entry frame
                    // below is built from the same list. A failed load degrades
                    // to an empty tab for this session rather than blocking
                    // world entry.
                    let friend_all = match friends.load_owned(character_id.character_id).await {
                        Ok(loaded) => {
                            let count = loaded.len();
                            registry.set_friends(
                                session_key,
                                loaded
                                    .into_iter()
                                    .map(|f| crate::registry::FriendEntry {
                                        person_id: f.person_id,
                                        group: f.group,
                                        name: f.name,
                                        account_id: f.account_id,
                                    })
                                    .collect(),
                            );
                            logln!(
                                "game: {} loaded {count} friend(s) from the database",
                                character_id.name
                            );
                            let notified = registry.friend_login_push(session_key);
                            if notified > 0 {
                                logln!(
                                    "game: {}: notified {notified} online friend(s) of login",
                                    character_id.name
                                );
                            }
                            friend_all_list_body(&registry, session_key)
                        }
                        Err(e) => {
                            logln_err!(
                                "game: friend load for {} failed: {e}; friend tab empty for this \
                                 session",
                                character_id.name
                            );
                            vec![0u8; gz_proto::friends::RESPONSE_FRIENDALLLIST_SIZE]
                        }
                    };
                    live.friend_list = friend_all;
                    // Hydrate the quest block the same way: the client's own copy of
                    // it is what decides whether it will walk onto a gate at all
                    // (`MouseOperation.cpp:5500-5511`), so it has to be real before
                    // the player can move. A failed load degrades to the zero block
                    // (nothing cleared, nothing running) rather than blocking world
                    // entry.
                    live.quest = match quests.load(character_id.character_id).await {
                        Ok(rows) => {
                            let mut state = gz_proto::quest::PersonQuest::new();
                            for row in rows {
                                if let Some(unit) = state.unit_mut(row.quest_type) {
                                    *unit = row.unit;
                                } else {
                                    logln_err!(
                                        "game: {}: stored quest type {} is outside the \
                                         client's block — ignored",
                                        character_id.name,
                                        row.quest_type
                                    );
                                }
                            }
                            let cleared = (0..gz_proto::quest::QUEST_TYPES)
                                .filter(|t| {
                                    state
                                        .unit(*t as i16)
                                        .is_some_and(|u| u.state != 0 || u.cur_unique != 0)
                                })
                                .count();
                            logln!(
                                "game: {} loaded quest state ({cleared} type(s) non-empty)",
                                character_id.name
                            );
                            state
                        }
                        Err(e) => {
                            logln_err!(
                                "game: quest load for {} failed: {e}; quest block empty for this \
                                 session",
                                character_id.name
                            );
                            gz_proto::quest::PersonQuest::new()
                        }
                    };
                    // Make the newcomer visible, and make it visible to everyone
                    // already here — both halves of
                    // `cltServerMap::SendNewOthersInfoToPerson` (`ServerMap.cpp:73-150`).
                    announce_nearby_characters(
                        &registry,
                        session_key,
                        &character_id,
                        char_unique,
                        None,
                    )?;
                    // ...and the map's static NPCs, which are what make the town
                    // exist at all: the client creates a character only from this
                    // frame (`Client-Login.cpp:53-85`).
                    announce_map_npcs(&registry, session_key, character_id.map_id);
                    // ...and the field monsters, which are what populate the
                    // map outside the towns. The loop republishes the rest as
                    // the player walks; this is the entry cell's first announce.
                    if let Some(info) = registry.info(session_key) {
                        monsters
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .announce_near(&registry, session_key, info.map_id, info.x, info.y);
                        last_seen = Some((info.map_id, cell_of(info.x, info.y, info.map_id)));
                    }
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
                    // The cell comes from the character row, the **one** owner of
                    // "where this character stands": world entry seeds it, every
                    // committed step re-seats it (`step_movement`) and a gate warp
                    // moves it together with the map. A second copy on the session
                    // is what let a drop land one warp behind.
                    let character = live
                        .character
                        .as_ref()
                        .ok_or("item drop before a character was selected")?;
                    let (character_id, cell) = (character.character_id, character_cell(character));
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
                c if c == GameMsg::RequestRevive as u16 => {
                    handle_revive(
                        &mut stream,
                        &mut outbound,
                        &characters,
                        &registry,
                        session_key,
                        &mut live,
                        payload.as_ref(),
                    )
                    .await?;
                }
                c if c == GameMsg::RequestInvCancellationsealingup as u16 => {
                    let character_id = login
                        .as_ref()
                        .ok_or("unseal before a successful game login")?
                        .character_id
                        .ok_or("unseal before a character was selected")?;
                    // The legacy stamps its own local `SYSTEMTIME`
                    // (`Server-Item.cpp:3793-3795`); this port's clock is UTC, which
                    // is the only documented difference.
                    let now = crate::world_entry::ServerTime::from_unix_seconds(now_seconds());
                    let today = (
                        u8::try_from(now.year.saturating_sub(2000)).unwrap_or(0),
                        u8::try_from(now.month).unwrap_or(0),
                        u8::try_from(now.day).unwrap_or(0),
                    );
                    handle_inv_cancellation_sealing_up(
                        &mut stream,
                        &mut outbound,
                        &items,
                        character_id,
                        today,
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
                        cfg.debug_log,
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
                        &registry,
                        session_key,
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
                    if order.si_order == ORDER_ATTACK {
                        // This is the attack the client actually sends: one
                        // order per click, never repeated (`MouseOperation.cpp:5631`,
                        // `Client.cpp:3299`). The legacy answers it by setting a
                        // persistent order rather than by resolving the hit
                        // here (`char-order.cpp:226-263`), so this arm starts
                        // the fight and the read loop's timer arm lands the
                        // swings on the weapon's interval.
                        //
                        // The same 200 ms gate as 267 — `CheckLastMsgClock(200)`
                        // covers the whole request side (`Server-Person.cpp:2760`).
                        if now_tick().saturating_sub(last_attack_ms) < HIT_THROTTLE_MS {
                            continue;
                        }
                        last_attack_ms = now_tick();
                        // `siPara2` is the target's char unique
                        // (`cltOrder::MakeOrder`, `order.cpp:155`). Anything
                        // outside the monster block is a player and PVP is out
                        // of scope, so a target that does not parse is skipped
                        // here and the world's own lookup rejects it again
                        // on the swing.
                        let Ok(target_unique) = u16::try_from(order.si_para2) else {
                            continue;
                        };
                        let Some(me) = registry.info(session_key) else {
                            continue;
                        };
                        if me.life == 0 {
                            continue;
                        }
                        // A PC's swing goes to its neighbours and not to itself
                        // (`SendNeighbourExceptMeMsg`, `char-order.cpp:254`):
                        // the attacker's own client already started the
                        // animation when the click landed (`OrderOperation` →
                        // `LetOrder`, `Client.cpp:3297`).
                        registry.broadcast_near_others(
                            &me,
                            session_key,
                            GameMsg::ResponseUserorder as u16,
                            OrderData::attack_order(i32::from(me.char_unique), order.si_para2)
                                .encode()
                                .to_vec(),
                        );
                        // Armed for "now": the next pass of the read loop lands
                        // the first swing, then re-arms on the interval.
                        logln!(
                            "game: {peer}: ORDER_ATTACK -> monster {target_unique}, \
                             attacker at ({}, {}); swing armed",
                            me.x,
                            me.y
                        );
                        attack = Some((target_unique, Instant::now()));
                        continue;
                    }
                    // Every other order takes over from the fight. In this port
                    // `handle_userorder` accepts `ORDER_MOVE` alone, so a walk
                    // is also the only thing that cancels it — which mirrors the
                    // legacy's `clCurOrder.Set(&ReserveOrder)` replacing the
                    // attack order outright.
                    if order.si_order == ORDER_MOVE {
                        attack = None;
                        approach = None;
                    } else {
                        // Anything that is neither a walk nor a swing lands here
                        // and is dropped by `handle_userorder`. Without this line
                        // the client's real order is invisible — which is exactly
                        // the ambiguity a live combat test trips over.
                        logln!(
                            "game: {peer}: order {} ignored (para1={}, para2={}, char={})",
                            order.si_order,
                            order.si_para1,
                            order.si_para2,
                            order.char_unique
                        );
                    }
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
                            // Arm the deadline for the first step.

                            mv.deadline = Some(next_move_deadline(&mv, Instant::now()));
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
                c if c == GameMsg::RequestHit as u16 => {
                    // 267: the swing. The legacy throttles the whole handler on
                    // the attacker's own 200 ms clock
                    // (`CheckLastMsgClock(200)`, `Server-Person.cpp:2760`) —
                    // the anti-macro half of the combat path, and worth keeping:
                    // without it a fast client out-damages its own interval.
                    if now_tick().saturating_sub(last_attack_ms) < HIT_THROTTLE_MS {
                        continue;
                    }
                    last_attack_ms = now_tick();

                    let Some(character) = live.character.as_ref() else {
                        continue; // no character in a world: nothing can swing
                    };
                    let request = RequestHit::decode(payload.as_ref())?;
                    // The target is named by its `CR` id, which for a field
                    // monster *is* its char unique (`NPC.cpp:972`). Anything
                    // outside that block is a player, and PVP is out of scope.
                    let Ok(target_unique) = u16::try_from(request.cl_attack_target.si_attack_id)
                    else {
                        continue;
                    };
                    // Re-seat on the authoritative cell if a step is running, so
                    // the range check uses where the attacker *is*, not where it
                    // started (the same re-seat the 240 arm does).
                    let (ax, ay) = match movement.as_ref() {
                        Some(mv) => mv.pos,
                        None => (character.pos_x, character.pos_y),
                    };
                    player_swing(
                        &registry,
                        monsters.as_ref(),
                        player_attacks.as_ref(),
                        &mut combat_rng,
                        &characters,
                        &items,
                        session_key,
                        &login,
                        character,
                        (ax, ay),
                        target_unique,
                    )
                    .await?;
                }
                c if c == GameMsg::RequestSitdown as u16 => {
                    // 7231: the sit/stand toggle (T-018 slice 6 B4, sit/stand
                    // leg). The client sends this, not a 240 ORDER_SITDOWN; the
                    // 241 that observers see is this handler's output.
                    handle_sit_down(
                        &mut stream,
                        &mut outbound,
                        &registry,
                        session_key,
                        &live,
                        payload.as_ref(),
                    )
                    .await?;
                }
                c if c == GameMsg::RequestPersonIp as u16 => {
                    // 214 -> 215: the on-demand inspect of another character's
                    // stat block (T-018 slice 6 B3).
                    handle_person_ip(&mut stream, &mut outbound, &registry, payload.as_ref())
                        .await?;
                }
                c if c == GameMsg::RequestPersonprivateinfo as u16 => {
                    // 272 -> 274: the right-click "person info" dialog. The menu
                    // item sends the target's `SI16 siCharUnique`
                    // (`RightMouseMenu.cpp:481-487`).
                    handle_person_private_info(
                        &mut stream,
                        &mut outbound,
                        &registry,
                        payload.as_ref(),
                    )
                    .await?;
                }
                c if c == GameMsg::RequestRightmouseUserlistName as u16 => {
                    // 297 -> 298: the right-click menu's header banner name. The
                    // 6 record carries no name, so the client asks on demand.
                    handle_rightmouse_userlist_name(
                        &mut stream,
                        &mut outbound,
                        &registry,
                        payload.as_ref(),
                    )
                    .await?;
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
                        live.character.as_ref(),
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
                // ---- the whisper channel: 123 -> 124 (right-click Whisper) ----
                c if c == GameMsg::RequestMakesecretchatchannel as u16 => {
                    let request = decode_makesecret_chat_channel_request(payload.as_ref())?;
                    handle_makesecret_chat_channel(&registry, session_key, request);
                }
                // ---- the friend family: 6200 -> 6201, 6202 -> 6203 ----
                c if c == GameMsg::RequestFriendadd as u16 => {
                    handle_friend_add(&registry, session_key, payload.as_ref())?;
                }
                c if c == GameMsg::RequestFriendaddaccept as u16 => {
                    handle_friend_add_accept(&friends, &registry, session_key, payload.as_ref())
                        .await?;
                }
                c if c == GameMsg::RequestFriendlist as u16 => {
                    handle_friend_list(&registry, session_key);
                }
                // ---- the quest family: 5700 / 5701 / 5702, 225 back ----
                //
                // The acting character's three read-only values are copied out
                // first, so the handlers can borrow the quest block mutably in
                // the same call.
                c if c == GameMsg::RequestSetquestgoing as u16 => {
                    let actor = QuestActor::from_character(
                        live.character
                            .as_ref()
                            .ok_or("quest going before a character was selected")?,
                    );
                    handle_set_quest_going(
                        &mut stream,
                        &mut outbound,
                        &quests,
                        &items,
                        &items,
                        &characters,
                        crate::world::NPC_PRICE_RATE,
                        &actor,
                        &mut live.quest,
                        payload.as_ref(),
                    )
                    .await?;
                }
                c if c == GameMsg::RequestCancelquest as u16 => {
                    let actor = QuestActor::from_character(
                        live.character
                            .as_ref()
                            .ok_or("quest cancel before a character was selected")?,
                    );
                    handle_cancel_quest(
                        &mut stream,
                        &mut outbound,
                        &quests,
                        &actor,
                        &mut live.quest,
                        payload.as_ref(),
                    )
                    .await?;
                }
                c if c == GameMsg::RequestQueststatus as u16 => {
                    let actor = QuestActor::from_character(
                        live.character
                            .as_ref()
                            .ok_or("quest status before a character was selected")?,
                    );
                    handle_quest_status(
                        &mut stream,
                        &mut outbound,
                        &quests,
                        &items,
                        &items,
                        &characters,
                        crate::world::NPC_PRICE_RATE,
                        &actor,
                        &mut live.quest,
                        payload.as_ref(),
                    )
                    .await?;
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
///
/// The write is bounded by [`WRITE_TIMEOUT`]: a peer that stops reading fills
/// the kernel buffer and this `write_all` never returns, so without the bound
/// one stalled connection would leak a task forever. On timeout the error
/// propagates, the read loop exits, and `Registered`'s drop releases the
/// registry entry and the login claim.
async fn send_raw(
    stream: &mut TcpStream,
    cipher: &mut Cipher,
    cmd: u16,
    payload: &[u8],
) -> Result<(), AnyError> {
    let frame = Frame::new(cmd, payload)?;
    let wire = frame.into_bytes();
    let enciphered = cipher.encode(&wire)?;
    tokio::time::timeout(WRITE_TIMEOUT, async {
        stream.write_all(&enciphered).await?;
        stream.flush().await
    })
    .await
    .map_err(|_| -> AnyError {
        format!("write to peer timed out after {WRITE_TIMEOUT:?} ({cmd:#x})").into()
    })??;
    Ok(())
}

/// Answers `GAMEMSG_REQUEST_PERSON_IP` (214) with `GAMEMSG_RESPONSE_PERSON_IP`
/// (215): the on-demand inspect of another character's stat block (T-018 slice 6
/// B3).
///
/// The client sends the target's `char_unique` as one `SI32`
/// (`cltMsg(GAMEMSG_REQUEST_PERSON_IP, GetCharUnique())`, `Client-SetChar.cpp:304`);
/// the legacy resolves it to a session and replies with that session's `clIP`
/// (`Server-Person.cpp:76-92`). The port reads the target's block out of the
/// registry — captured at world entry, refreshed on a stat spend — so the pull
/// needs no database round trip. An unknown or out-of-range id is answered with
/// nothing, as the legacy's `IsValidID` guard does (`:81`).
///
/// # Errors
///
/// A body shorter than the 4-byte id, or a socket/cipher failure.
async fn handle_person_ip(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    registry: &Registry,
    payload: &[u8],
) -> Result<(), AnyError> {
    if payload.len() < 4 {
        return Err(format!(
            "person ip request is {} bytes, need the 4-byte char_unique",
            payload.len()
        )
        .into());
    }
    let target = i32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
    let Some(char_unique) = i16::try_from(target).ok() else {
        logln!("game: person ip target {target} does not fit the SI16 char_unique -> no reply");
        return Ok(());
    };
    let Some(basic) = registry
        .find_by_charunique(char_unique)
        .and_then(|key| registry.basic_info(key))
    else {
        logln!("game: person ip for unseen char_unique {target} -> no reply");
        return Ok(());
    };
    let body = encode_person_ip(char_unique, &basic);
    send_raw(stream, outbound, GameMsg::ResponsePersonIp as u16, &body).await?;
    logln!(
        "game: person ip for char_unique {target} -> 215 ({} bytes)",
        body.len()
    );
    Ok(())
}

/// Answers `GAMEMSG_REQUEST_PERSONPRIVATEINFO` (272) with
/// `GAMEMSG_RESPONSE_PERSONPRIVATEINFO` (274): the right-click "person info"
/// dialog.
///
/// The client sends the target's character id as one `SI16`
/// (`cltGameMsgRequest_PersonPrivateInfo`, `MsgType-Person.h:196-204`, sent from
/// `RightMouseMenu.cpp:484-486`); the legacy resolves it to a session and fills
/// `cltGameMsgResponse_PersonPrivateInfo` from that character
/// (`Server-Person.cpp:213-265`). The port reads the target's block out of the
/// registry — captured at world entry — so no database round trip is needed.
/// An unseen id is answered with nothing, as the legacy's `IsValidID` guard
/// does (`:225`).
///
/// # Errors
///
/// A body shorter than the 2-byte id, or a socket/cipher failure.
async fn handle_person_private_info(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    registry: &Registry,
    payload: &[u8],
) -> Result<(), AnyError> {
    if payload.len() < 2 {
        return Err(format!(
            "person info request is {} bytes, need the 2-byte char_unique",
            payload.len()
        )
        .into());
    }
    let char_unique = i16::from_le_bytes([payload[0], payload[1]]);
    let Some(basic) = registry
        .find_by_charunique(char_unique)
        .and_then(|key| registry.basic_info(key))
    else {
        logln!("game: person info for unseen char_unique {char_unique} -> no reply");
        return Ok(());
    };
    let body = encode_person_private_info(char_unique, &basic);
    send_raw(
        stream,
        outbound,
        GameMsg::ResponsePersonprivateinfo as u16,
        &body,
    )
    .await?;
    logln!(
        "game: person info for char_unique {char_unique} -> 274 ({} bytes)",
        body.len()
    );
    Ok(())
}

/// Answers `GAMEMSG_REQUEST_RIGHTMOUSE_USERLIST_NAME` (297) with
/// `GAMEMSG_RESPONSE_RIGHTMOUSE_USERLIST_NAME` (298): the name for the right-click
/// menu's header banner.
///
/// The legacy `cltMakeCharInfo` record (LOGINMANY, 6) does not carry a character
/// name — `cltCommonLogic::SetChar` never calls `SetName`
/// (`CommonLogic-SetChar.cpp:44-127`) — so the client asks on demand with
/// `cltGameMsgRequest_GetName` (one `SI32 siCharUnique`, `MsgType.h:3188-3195`, sent
/// from `NRightMouseMenuDlg.cpp:463`) and the server answers with
/// `cltGameMsgResponse_GetName` (`MsgType.h:3197-3208`), whose 24-byte body is the
/// same one [`encode_response_name`] builds for opcode 244. The client handler
/// `SetName`s the slot by char id (`Client-Person.cpp:3032-3046`);
/// unanswered, the banner stays blank.
///
/// Mirror the legacy lookup (`Server-Person.cpp:2673-2696`): an out-of-range or
/// unseen id gets nothing, like the `IsValidID` guard.
///
/// # Errors
///
/// A body shorter than the 4-byte id, or a socket/cipher failure.
async fn handle_rightmouse_userlist_name(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    registry: &Registry,
    payload: &[u8],
) -> Result<(), AnyError> {
    if payload.len() < 4 {
        return Err(format!(
            "right-mouse name request is {} bytes, need the 4-byte char_unique",
            payload.len()
        )
        .into());
    }
    let char_unique = i32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
    // The legacy rejects ids outside the CharUnique array (`Server-Person.cpp:2677`);
    // that bound also guarantees the cast to the registry's SI16 key is lossless.
    if !(0..i32::from(MAX_CHAR_NUMBER)).contains(&char_unique) {
        logln!("game: right-mouse name for out-of-range char_unique {char_unique} -> no reply");
        return Ok(());
    }
    let Some(basic) = registry
        .find_by_charunique(char_unique as i16)
        .and_then(|key| registry.basic_info(key))
    else {
        logln!("game: right-mouse name for unseen char_unique {char_unique} -> no reply");
        return Ok(());
    };
    let body = encode_response_name(char_unique, &basic.name);
    send_raw(
        stream,
        outbound,
        GameMsg::ResponseRightmouseUserlistName as u16,
        &body,
    )
    .await?;
    logln!(
        "game: right-mouse name for char_unique {char_unique} -> 298 ({} bytes)",
        body.len()
    );
    Ok(())
}

/// Two key bytes for this connection's session cipher. Unpredictability only
/// needs to defeat accidental reuse in development, not an attacker.
fn random_key_bytes() -> (u8, u8) {
    let v = hash_now();
    (v as u8, (v >> 8) as u8)
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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
mod tests;
