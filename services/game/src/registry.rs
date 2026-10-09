//! The online-session registry: who is connected, where they stand, and how a
//! broadcast reaches them — the shared half of T-018 slice 5.
//!
//! Until now every handler in this binary answered the connection that asked.
//! The chat family (117/118 general, 119/120 whisper, 121/122 head chat, 112
//! notice) is the first code here that has to *address other players*, so the
//! sessions have to be enumerable. The legacy keeps them in `cltCharManager`
//! (`CR[id]`, `GetIDFromName`, `GetIDFromCharUnique`, `SendNeighbourMsg`,
//! `SendAllPersonHomeMsg`); this registry is that table narrowed to what chat
//! actually reads. Party, guild, rank and battle-royal membership are not here —
//! they are the modes the legacy itself drops (`gz_proto::chat` lists them).
//!
//! # The key is a per-connection serial, not `char_unique`
//!
//! `char_unique` is the id the wire carries and the id the legacy resolves
//! against (`GetIDFromCharUnique`, `CharManager.cpp:203`), and it would be the
//! natural map key — except that in capture prologue mode the server pins every
//! connection's `char_unique` to the captured character's id (`config.rs`,
//! `CAPTURED_CHAR_UNIQUE`), because each replayed per-character frame names its
//! subject and the client discards frames that do not match. Two clients
//! therefore *share* one id, and a `char_unique`-keyed map would let the second
//! connection overwrite the first: the first player's whisper would reach
//! nobody and its head-chat store would vanish on every disconnect.
//!
//! So the key is a serial handed out per connection ([`Registry::next_key`]),
//! and `char_unique` travels in the frames and in [`SessionInfo`] — which is
//! where the 119/121 lookups resolve it, exactly as the legacy does.
//!
//! The consequence worth naming: in capture mode two sessions really do share a
//! `char_unique`, so [`Registry::find_by_charunique`] returns the lower key —
//! deterministic but arbitrary. Live mode allocates one id per session
//! (`allocate_char_unique`), so the ambiguity cannot arise there; until T-017,
//! the capture prologue is the default, which is why `/t name` (the 117
//! name-based whisper) is the path that is unambiguous with two clients.
//!
//! # What "near" means
//!
//! The legacy fans a neighbour message out over a map's *cell* neighbourhood:
//! every cell in the 3×3 block around the sender's cell (`ITZMap.cpp:71-87`),
//! which is itself a screen-sized block — `CreateWorld(MAX_MAP_NUMBER,
//! MAX_CHAR_NUMBER, 800, 600, MAP_TILE_XSIZE, MAP_TILE_YSIZE)`
//! (`MapManager.cpp:82-87`) leaves `cellXtile`/`cellYtile` at their defaults,
//! the screen in tiles: `(800 + 80 - 1) / 80 = 10` and `(600 + 40 - 1) / 40 =
//! 15` (`ITZWorld.h:91-104`, `map.h:33-35`).
//!
//! The port approximates it: [`cell_of`] is plain tile division by
//! [`CELL_TILES_X`]/[`CELL_TILES_Y`] with the same 3×3 rule, where the legacy's
//! `GetCellIndex` (`ITZMap.cpp:116-122`) maps a tile into an isometric-skewed
//! cell — `sx = (mapXtile - y - 1 + x) / (cellXtile << 1)`, `sy = (x + y) /
//! (cellYtile << 1)`. Reproducing that needs `m_mapXtile`, the *per-map* tile
//! width, which this port does not model (no map geometry is loaded yet), and
//! the skewed cell boundary would be wrong anyway as soon as a map's size
//! differs from the assumption. The two agree on "same map, roughly the same
//! screen's worth of tiles", which is all a head-chat fan needs today;
//! slice 6's observer/visibility work is where real map geometry has to land.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use gz_proto::chat::{MAX_PLAYER_NAME, cut};
use tokio::sync::mpsc::UnboundedSender;

/// `MAX_CHAR_NUMBER` (`CommonHeader.h:556`) — `GetIDFromCharUnique` returns 0
/// for `charunique <= 0` and for `charunique >=` this (`CharManager.cpp:205`),
/// so neither can name a session.
pub const MAX_CHAR_NUMBER: i16 = 20_000;

/// Tiles per near-cell on the x axis. See the module docs for the derivation
/// (`ITZWorld.h:91-92` with `map.h:33`).
pub const CELL_TILES_X: i32 = 10;

/// Tiles per near-cell on the y axis (`map.h:35`).
pub const CELL_TILES_Y: i32 = 15;

/// The registry's key: one per connection, from [`Registry::next_key`].
///
/// Never a `char_unique` — see the module docs.
pub type SessionKey = u64;

/// Next session key. Starts at 1 so 0 can never name a real session, which is
/// what the legacy's `IsValidID` guard also guarantees for its ids.
static NEXT_SESSION_KEY: AtomicU64 = AtomicU64::new(1);

/// One frame queued for one connection.
///
/// Nothing but a connection's own task writes to its socket, so a broadcast
/// hands each recipient's channel a frame and the recipient's read loop sends
/// it. Frames queued to the same connection go out in the order they were
/// queued; a chat frame queued while the same dispatch is also writing a reply
/// directly may be written after it, which costs ordering between two frame
/// types the client handles independently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outbound {
    /// The `GAMEMSG_*` opcode the body goes out under.
    pub cmd: u16,
    /// The headerless body — `send_raw` frames it.
    pub payload: Vec<u8>,
}

/// What the registry holds about one world-entered character.
///
/// Seeded at `REQUEST_SELECTCHAR` from the character row and kept current by
/// `update_pos` as the avatar steps. `level` and `school_id` are what the chat
/// paths read out of it: the 119 delivery echoes the sender's `siLevel`
/// (`Server-System.cpp:591`), and the HOME branch matches `siHomeVillage`
/// (`:1061`, `:1066`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    /// `siCharUnique` — what 118/120/121/122 put on the wire and what the
    /// 119/121 lookups resolve.
    pub char_unique: i16,
    /// The character's name, and the target `find_by_name` matches exactly.
    pub name: String,
    /// `clIP.siLevel` as `i32`, because 118's `siLevel` is `SI32`.
    pub level: i32,
    /// The map the character stands on, for the near fan.
    pub map_id: i32,
    /// Tile x, updated as the avatar steps.
    pub x: i32,
    /// Tile y.
    pub y: i32,
    /// `clBI.siHomeVillage` — `CharacterFull.school_id`.
    pub school_id: i16,
}

/// One registered connection.
#[derive(Debug)]
pub struct OnlineSession {
    /// The character row the chat paths read.
    pub info: SessionInfo,
    /// `bHeadChatSwitch` + `szHeadChatText` folded into one option:
    /// `Some(text)` is the switch being on with its stored text
    /// (`SetHeadChat`, `char.cpp:6623-6630`), `None` is the switch being off —
    /// which is what 121 tests before answering (`Server-System.cpp:712`).
    pub head_chat: Option<String>,
    tx: UnboundedSender<Outbound>,
}

/// The table of world-entered connections, shared by every connection task.
///
/// One `Mutex` around a `HashMap`: chat traffic is a handful of lock
/// acquisitions per message, all of them map lookups and channel sends, so
/// there is nothing here worth sharding. A poisoned lock is recovered from
/// rather than propagated — a panic in another connection's task must not stop
/// this one from delivering a message.
#[derive(Debug)]
pub struct Registry {
    sessions: Mutex<HashMap<SessionKey, OnlineSession>>,
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

impl Registry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// The next connection's key, from 1 upward.
    ///
    /// Relaxed ordering is enough: the keys only have to differ within the
    /// process, and [`SessionKey`]'s value carries no ordering meaning.
    #[must_use]
    pub fn next_key() -> SessionKey {
        NEXT_SESSION_KEY.fetch_add(1, Ordering::Relaxed)
    }

    /// Recovers a lock, poison and all: the guard outlives the panic that
    /// poisoned it, and a half-updated map is still better than a dead server.
    fn lock(&self) -> MutexGuard<'_, HashMap<SessionKey, OnlineSession>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Enters a connection into the table, replacing anything already under
    /// `key`.
    ///
    /// Replacing is the character-switch path: a second `REQUEST_SELECTCHAR`
    /// tears the old avatar down (`ServerDB-Person.cpp:1770`), so the new
    /// character starts with no stored head chat — hence `head_chat: None`
    /// rather than carrying the old one over.
    pub fn register(&self, key: SessionKey, tx: UnboundedSender<Outbound>, info: SessionInfo) {
        self.lock().insert(
            key,
            OnlineSession {
                info,
                head_chat: None,
                tx,
            },
        );
    }

    /// Drops a connection's entry. Called by [`Registered`] on every exit path;
    /// a `key` that was never registered (a drop before `REQUEST_SELECTCHAR`)
    /// is a no-op.
    pub fn remove(&self, key: SessionKey) {
        self.lock().remove(&key);
    }

    /// A copy of one session's character row, or `None` if it is not
    /// registered — which is every frame a connection sends before it has
    /// selected a character. The legacy's `IsValidID(id)` guards play the same
    /// part (`Server-System.cpp:730`).
    #[must_use]
    pub fn info(&self, key: SessionKey) -> Option<SessionInfo> {
        self.lock().get(&key).map(|s| s.info.clone())
    }

    /// Moves a registered character, or does nothing if it is gone.
    pub fn update_pos(&self, key: SessionKey, x: i32, y: i32) {
        if let Some(session) = self.lock().get_mut(&key) {
            session.info.x = x;
            session.info.y = y;
        }
    }

    /// Stores (or, with `None`, clears) a character's head chat, the pair the
    /// legacy's `SetHeadChat(bool, TCHAR*)` implements (`char.cpp:6623`).
    pub fn set_head_chat(&self, key: SessionKey, text: Option<String>) {
        if let Some(session) = self.lock().get_mut(&key) {
            session.head_chat = text;
        }
    }

    /// The stored head chat, i.e. the switch being on.
    #[must_use]
    pub fn head_chat(&self, key: SessionKey) -> Option<String> {
        self.lock().get(&key).and_then(|s| s.head_chat.clone())
    }

    /// The session whose character is named `name` — `GetIDFromName`
    /// (`CharManager.cpp:185`).
    ///
    /// Exact and case-sensitive, like the legacy's `_tcscmp` key equality
    /// (`HashTable_StringInt.h:45`), and the query is capped at 19 bytes the
    /// way `StringCchCopy(..., MAX_PLAYER_NAME, ...)` caps it before hashing.
    /// With the same name registered twice the lower key wins, so a lookup is
    /// deterministic; names are unique per database anyway, and only capture
    /// mode's pinned `char_unique` can make two sessions collide.
    #[must_use]
    pub fn find_by_name(&self, name: &str) -> Option<SessionKey> {
        let mut want = name.to_owned();
        cut(&mut want, MAX_PLAYER_NAME - 1);
        self.lock()
            .iter()
            .filter(|(_, s)| s.info.name == want)
            .map(|(key, _)| *key)
            .min()
    }

    /// The session whose character carries `char_unique` —
    /// `GetIDFromCharUnique` (`CharManager.cpp:203`), bounds and all.
    #[must_use]
    pub fn find_by_charunique(&self, char_unique: i16) -> Option<SessionKey> {
        if char_unique <= 0 || char_unique >= MAX_CHAR_NUMBER {
            return None;
        }
        self.lock()
            .iter()
            .filter(|(_, s)| s.info.char_unique == char_unique)
            .map(|(key, _)| *key)
            .min()
    }

    /// Queues one frame for one connection; `false` when it is gone.
    ///
    /// A send to a connection that just disconnected is not an error: the frame
    /// is dropped, exactly as the legacy's `SendNetMsg` on a sessionless
    /// character would.
    pub fn send_to(&self, key: SessionKey, cmd: u16, payload: Vec<u8>) -> bool {
        match self.lock().get(&key) {
            Some(session) => session.tx.send(Outbound { cmd, payload }).is_ok(),
            None => false,
        }
    }

    /// Queues one frame for **every** connected character, the speaker
    /// included.
    ///
    /// This is the general-chat fan. The legacy sends to the 3×3 cell
    /// neighbourhood instead (`SendNeighbourMsg(&clMsg, true)`,
    /// `Server-System.cpp:1149`) — see the owner's instruction in the handoff:
    /// general chat is deliberately server-wide here, modern-server style, and
    /// only head chat keeps the near fan.
    ///
    /// Returns how many connections took the frame.
    pub fn broadcast_all(&self, cmd: u16, payload: Vec<u8>) -> usize {
        let sessions = self.lock();
        let mut sent = 0;
        for session in sessions.values() {
            if session
                .tx
                .send(Outbound {
                    cmd,
                    payload: payload.clone(),
                })
                .is_ok()
            {
                sent += 1;
            }
        }
        sent
    }

    /// Queues one frame for every character in `origin`'s cell neighbourhood,
    /// `origin` included — `SendNeighbourMsg(..., true)`, whose `true` is the
    /// self-inclusion flag (`Char-Neighbour.cpp:29-78`).
    ///
    /// Returns how many connections took the frame.
    pub fn broadcast_near(&self, origin: &SessionInfo, cmd: u16, payload: Vec<u8>) -> usize {
        self.fan(origin, None, cmd, payload)
    }

    /// [`Self::broadcast_near`] minus the sender's own connection.
    ///
    /// The legacy has no equivalent because it has no split between "write to my
    /// own socket now" and "hand a copy to the session manager": one
    /// `SendNeighbourMsg` covers the whole 3×3 block including self, and that is
    /// correct there. A port that owns the sender's cipher has to choose, and for
    /// a frame the sender also writes **directly** the right choice is to exclude
    /// self here rather than have two paths deliver the same bytes.
    ///
    /// The reason is ordering, not bandwidth. A frame queued here is drained by the
    /// recipient's read loop on a later tick, while the sender's own copy is written
    /// synchronously inside the dispatch. So a self-copy of step *N* can be written
    /// to the socket *after* the direct copy of step *N+1*, and the client would
    /// then `MoveTo` the older tile — an avatar that walks backwards. Excluding self
    /// keeps exactly one writer per connection and per frame type, in order.
    ///
    /// Use this when the caller also sends the frame to itself; use
    /// [`Self::broadcast_near`] when the fan is the only delivery (the chat paths,
    /// where the legacy likewise sends one message to the whole block and nothing
    /// goes out separately).
    ///
    /// Returns how many connections took the frame.
    pub fn broadcast_near_others(
        &self,
        origin: &SessionInfo,
        origin_key: SessionKey,
        cmd: u16,
        payload: Vec<u8>,
    ) -> usize {
        self.fan(origin, Some(origin_key), cmd, payload)
    }

    /// The shared fan. `skip` is the one key to leave out.
    fn fan(
        &self,
        origin: &SessionInfo,
        skip: Option<SessionKey>,
        cmd: u16,
        payload: Vec<u8>,
    ) -> usize {
        let sessions = self.lock();
        let mut sent = 0;
        for (key, session) in sessions.iter() {
            if skip == Some(*key) || !is_near(origin, &session.info) {
                continue;
            }
            if session
                .tx
                .send(Outbound {
                    cmd,
                    payload: payload.clone(),
                })
                .is_ok()
            {
                sent += 1;
            }
        }
        sent
    }

    /// Queues one frame for every character of the given home village —
    /// `SendAllPersonHomeMsg` (`CharManager.cpp:751`), which matches
    /// `siHomeVillage == villageunique` and does not exclude the speaker.
    ///
    /// Callers gate on the value being non-zero, as the legacy's HOME branch
    /// does (`Server-System.cpp:1061`): with no home village of its own the
    /// speaker has nothing to say, and `school_id == 0` would otherwise match
    /// every character that has none either.
    ///
    /// Returns how many connections took the frame.
    pub fn broadcast_school(&self, school_id: i16, cmd: u16, payload: Vec<u8>) -> usize {
        let sessions = self.lock();
        let mut sent = 0;
        for session in sessions.values() {
            if session.info.school_id != school_id {
                continue;
            }
            if session
                .tx
                .send(Outbound {
                    cmd,
                    payload: payload.clone(),
                })
                .is_ok()
            {
                sent += 1;
            }
        }
        sent
    }
}

/// Removes a connection's registry entry when its task ends.
///
/// Built *before* the read loop rather than at registration, so every exit —
/// framing rejection, cipher desync, EOF, handshake timeout, panic — leaves the
/// table clean. A drop before `REQUEST_SELECTCHAR` finds nothing to remove,
/// which is why the guard and the registration are separate steps: the legacy
/// equivalent is a character that never entered the world never being in
/// `cltCharManager` in the first place.
#[derive(Debug)]
pub struct Registered {
    registry: Arc<Registry>,
    key: SessionKey,
}

impl Registered {
    /// Arms a guard for `key`; the actual entry appears at `register`.
    #[must_use]
    pub fn new(registry: Arc<Registry>, key: SessionKey) -> Self {
        Self { registry, key }
    }
}

impl Drop for Registered {
    fn drop(&mut self) {
        self.registry.remove(self.key);
    }
}

/// The near-cell a tile falls in — see the module docs for how this differs
/// from the legacy's `GetCellIndex` (`ITZMap.cpp:116-122`).
fn cell_of(x: i32, y: i32) -> (i32, i32) {
    (x / CELL_TILES_X, y / CELL_TILES_Y)
}

/// Whether `other` is inside `origin`'s 3×3 cell block on the same map — the
/// near set `ITZMap.cpp:71-87` walks.
fn is_near(origin: &SessionInfo, other: &SessionInfo) -> bool {
    if origin.map_id != other.map_id {
        return false;
    }
    let (ox, oy) = cell_of(origin.x, origin.y);
    let (ax, ay) = cell_of(other.x, other.y);
    (ox - ax).abs() <= 1 && (oy - ay).abs() <= 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::{UnboundedReceiver, error::TryRecvError};

    /// A character with the defaults a chat test cares about: one map, one
    /// cell, one home village.
    fn info(char_unique: i16, name: &str, x: i32, y: i32) -> SessionInfo {
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

    /// Registers a connection and hands back its key and its inbox.
    fn join(registry: &Registry, info: SessionInfo) -> (SessionKey, UnboundedReceiver<Outbound>) {
        let key = Registry::next_key();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        registry.register(key, tx, info);
        (key, rx)
    }

    /// Nothing queued, as distinct from "the sender is gone".
    fn assert_silent(rx: &mut UnboundedReceiver<Outbound>, what: &str) {
        assert!(
            matches!(rx.try_recv(), Err(TryRecvError::Empty)),
            "{what}: expected no frame"
        );
    }

    #[test]
    fn keys_are_handed_out_one_per_connection() {
        // The counter is process-wide and the tests share it, so only the
        // relationship can be pinned.
        let first = Registry::next_key();
        let second = Registry::next_key();
        assert!(first > 0, "0 never names a session");
        assert_eq!(second, first + 1, "one key per connection, in order");
    }

    /// The guard is armed before a character is ever registered, so it has to
    /// survive that and still clean up after a real registration.
    #[test]
    fn dropping_the_guard_removes_the_entry_it_never_had_to_create() {
        let registry = Arc::new(Registry::new());
        let key = Registry::next_key();
        let guard = Registered::new(registry.clone(), key);
        drop(guard);
        assert_eq!(registry.find_by_name("Nobody"), None);

        let (key, _rx) = join(&registry, info(11, "Tester", 0, 0));
        let guard = Registered::new(registry.clone(), key);
        assert_eq!(registry.find_by_name("Tester"), Some(key));
        drop(guard);
        assert_eq!(registry.find_by_name("Tester"), None);
        assert_eq!(registry.info(key), None);
    }

    /// A second `REQUEST_SELECTCHAR` re-registers the same connection: the new
    /// character's row replaces the old one and the head-chat store starts
    /// empty (`ServerDB-Person.cpp:1770` tears the old avatar down).
    #[test]
    fn re_registering_a_connection_replaces_the_row_and_clears_head_chat() {
        let registry = Registry::new();
        let key = Registry::next_key();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        registry.register(key, tx.clone(), info(11, "First", 1, 1));
        registry.set_head_chat(key, Some("look at me".into()));

        registry.register(key, tx, info(12, "Second", 2, 2));
        assert_eq!(registry.info(key).unwrap().name, "Second");
        assert_eq!(registry.head_chat(key), None, "the switch was reset");
    }

    #[test]
    fn update_pos_is_what_the_near_fan_reads() {
        let registry = Registry::new();
        let (key, _rx) = join(&registry, info(11, "Walker", 0, 0));
        registry.update_pos(key, 40, 45);
        let session = registry.info(key).unwrap();
        assert_eq!((session.x, session.y), (40, 45));
        assert_eq!(cell_of(session.x, session.y), (4, 3));
    }

    /// General chat is server-wide: the speaker hears their own message too,
    /// which is what the legacy's `SendNeighbourMsg(..., true)` does inside the
    /// neighbourhood and what the owner asked for globally.
    #[test]
    fn broadcast_all_reaches_everyone_including_the_speaker() {
        let registry = Registry::new();
        let (_a, mut ra) = join(&registry, info(11, "A", 0, 0));
        let (_b, mut rb) = join(&registry, info(12, "B", 500, 500));
        let (_c, mut rc) = join(&registry, info(13, "C", 0, 0));

        assert_eq!(registry.broadcast_all(118, b"hi".to_vec()), 3);

        for rx in [&mut ra, &mut rb, &mut rc] {
            let frame = rx.try_recv().expect("every session gets the frame");
            assert_eq!(frame.cmd, 118);
            assert_eq!(frame.payload, b"hi");
        }
    }

    /// The 3×3 cell block on the same map, and nothing else: the neighbour
    /// walk of `ITZMap.cpp:71-87` clamped at the map's edge, with the cell size
    /// `ITZWorld.h:91` derives.
    #[test]
    fn broadcast_near_is_the_three_by_three_cell_block_on_one_map() {
        let registry = Registry::new();
        // Cell (1, 1) of map 7 — tiles 10..29 by 15..29.
        let (origin_key, _rself) = join(&registry, info(11, "Origin", 15, 20));
        // Inside the block: the same cell, the far corner cell (2, 2), the near
        // corner (0, 0).
        let (_same, mut rsame) = join(&registry, info(12, "SameCell", 29, 29));
        let (_corner, mut rcorner) = join(&registry, info(13, "Corner", 0, 0));
        // Two cells away on x: cell (3, 1), outside the block.
        let (_far, mut rfar) = join(&registry, info(14, "Far", 35, 20));
        // Same coordinates, different map.
        let (_other, mut rother) = {
            let mut elsewhere = info(15, "Elsewhere", 15, 20);
            elsewhere.map_id = 8;
            join(&registry, elsewhere)
        };

        let origin = registry.info(origin_key).unwrap();
        assert_eq!(origin.map_id, 7);

        let sent = registry.broadcast_near(&origin, 122, b"head".to_vec());
        assert_eq!(sent, 3, "the far cell and the other map are out");

        for rx in [&mut rsame, &mut rcorner] {
            let frame = rx.try_recv().unwrap();
            assert_eq!(frame.cmd, 122);
            assert_eq!(frame.payload, b"head");
        }
        assert_silent(&mut rfar, "three cells away");
        assert_silent(&mut rother, "another map");
    }

    /// The 200 fan (slice 6 B1) is `broadcast_near` minus the sender: the sender
    /// writes the frame to its own socket directly, and a second queued copy could
    /// be drained *after* a later direct one, walking the avatar backwards.
    ///
    /// The observer set is otherwise the same 3×3 block the chat fan uses.
    #[test]
    fn broadcast_near_others_leaves_the_sender_to_its_own_socket() {
        let registry = Registry::new();
        let (origin_key, mut rself) = join(&registry, info(11, "Mover", 15, 20));
        let (_same, mut rsame) = join(&registry, info(12, "Observer", 15, 20));
        let (_corner, mut rcorner) = join(&registry, info(13, "Corner", 0, 0));
        let (_far, mut rfar) = join(&registry, info(14, "Far", 35, 20));

        let origin = registry.info(origin_key).unwrap();
        assert_eq!(
            registry.broadcast_near_others(&origin, origin_key, 200, b"cell".to_vec()),
            2,
            "the two near observers, not the sender"
        );

        for rx in [&mut rsame, &mut rcorner] {
            let frame = rx.try_recv().expect("a near observer gets the frame");
            assert_eq!(frame.cmd, 200);
            assert_eq!(frame.payload, b"cell");
        }
        assert_silent(&mut rself, "the sender writes its own copy directly");
        assert_silent(&mut rfar, "three cells away");

        // The self-inclusive fan still reaches the sender, for the chat paths.
        assert_eq!(registry.broadcast_near(&origin, 122, b"head".to_vec()), 3);
        let frame = rself.try_recv().expect("the chat fan includes the speaker");
        assert_eq!(frame.cmd, 122);
    }

    /// The fan follows the *current* tile, which is what makes a walking
    /// character's observer set move with it: `step_movement` updates the registry
    /// before fanning, so a character that has walked into a new cell reaches the
    /// observers there and stops reaching the ones it left.
    #[test]
    fn the_fan_follows_the_walker_into_its_new_cell() {
        let registry = Registry::new();
        // (5, 5): cells (0, 0) — not near cell (2, 5).
        let (walker_key, _rwalker) = join(&registry, info(11, "Walker", 5, 5));
        let (_ahead, mut rahead) = join(&registry, info(12, "Ahead", 27, 87));
        let (_behind, mut rbehind) = join(&registry, info(13, "Behind", 5, 5));

        let mut origin = registry.info(walker_key).unwrap();
        assert_eq!(
            registry.broadcast_near_others(&origin, walker_key, 200, b"a".to_vec()),
            1,
            "only the character sharing its cell"
        );
        assert_silent(&mut rahead, "cell (2, 5) is three cells away on x");
        // Drain the first fan's copy so the assertions below read only the second.
        let first = rbehind.try_recv().expect("the sharing character");
        assert_eq!(first.payload, b"a");

        // Step onto the palace spawn, as `step_movement` does before fanning.
        registry.update_pos(walker_key, 27, 87);
        origin = registry.info(walker_key).unwrap();
        assert_eq!(
            registry.broadcast_near_others(&origin, walker_key, 200, b"b".to_vec()),
            1,
            "the observer set follows the walker: palace in, the old cell out"
        );
        let frame = rahead
            .try_recv()
            .expect("the cell it walked into is near now");
        assert_eq!(frame.payload, b"b");
        // Cell (0, 0) to cell (2, 5) is two cells out on x, so the character left
        // behind drops out of the fan — the observer set is not a fixed list.
        assert_silent(&mut rbehind, "two cells away once the walker moved on");
    }

    #[test]
    fn broadcast_school_is_the_home_village_match() {
        let registry = Registry::new();
        let (_a, mut ra) = join(&registry, info(11, "A", 0, 0));
        let (_b, mut rb) = {
            let mut other = info(12, "B", 0, 0);
            other.school_id = 4;
            join(&registry, other)
        };
        let (_c, mut rc) = join(&registry, info(13, "C", 0, 0));

        // The speaker's own school_id is 3, so the speaker is in the fan.
        assert_eq!(registry.broadcast_school(3, 118, b"home".to_vec()), 2);
        let frame = ra.try_recv().unwrap();
        assert_eq!(frame.payload, b"home");
        assert_silent(&mut rb, "a different home village");
        let frame = rc.try_recv().unwrap();
        assert_eq!(frame.payload, b"home");
    }

    /// `GetIDFromName` copies through `StringCchCopy(..., MAX_PLAYER_NAME, ...)`
    /// and the hash key compares with `_tcscmp`
    /// (`HashTable_StringInt.h:45-48`) — so the match is exact and
    /// case-sensitive, and a longer query matches on its first 19 bytes.
    #[test]
    fn find_by_name_is_exact_case_sensitive_and_capped() {
        let registry = Registry::new();
        let (short, _rx) = join(&registry, info(11, "Tester", 0, 0));
        let long_name = "abcdefghijklmnopqrs"; // exactly MAX_PLAYER_NAME - 1
        let (long, _rx) = join(&registry, info(12, long_name, 0, 0));

        assert_eq!(registry.find_by_name("Tester"), Some(short));
        assert_eq!(registry.find_by_name("tester"), None, "no folding");
        assert_eq!(registry.find_by_name("Test"), None, "no prefix match");
        assert_eq!(registry.find_by_name("Nope"), None);
        assert_eq!(
            registry.find_by_name(&format!("{long_name}EXTRA")),
            Some(long),
            "the query is capped at 19 bytes before the lookup"
        );
        assert_eq!(
            registry.find_by_name("Tester and more"),
            None,
            "and a short name stays short: the tail is not stripped"
        );
    }

    /// `GetIDFromCharUnique` returns 0 for `<= 0` and for `>= MAX_CHAR_NUMBER`
    /// (`CharManager.cpp:205`), so neither can resolve — and the out-of-range
    /// test has to happen on an id an `i16` can actually hold.
    #[test]
    fn find_by_charunique_applies_the_legacy_id_bound() {
        let registry = Registry::new();
        let (key, _rx) = join(&registry, info(1471, "Tester", 0, 0));

        assert_eq!(registry.find_by_charunique(1471), Some(key));
        assert_eq!(registry.find_by_charunique(0), None);
        assert_eq!(registry.find_by_charunique(-1), None);
        assert_eq!(registry.find_by_charunique(MAX_CHAR_NUMBER), None);
        assert_eq!(registry.find_by_charunique(1470), None);
    }

    /// Capture mode pins every connection to the captured `char_unique`, so two
    /// sessions share it. The lookup still has to answer, deterministically:
    /// the lower key wins rather than whichever the hash map walked first.
    #[test]
    fn a_shared_char_unique_resolves_to_the_lower_key() {
        let registry = Registry::new();
        let (first, _rx) = join(&registry, info(1471, "First", 0, 0));
        let (second, _rx) = join(&registry, info(1471, "Second", 0, 0));

        assert_eq!(registry.find_by_charunique(1471), Some(first));
        assert!(first < second);
        assert_eq!(registry.find_by_name("Second"), Some(second));
        assert_eq!(registry.find_by_name("First"), Some(first));
    }

    #[test]
    fn sending_to_a_session_that_left_is_not_an_error() {
        let registry = Registry::new();
        let (key, mut rx) = join(&registry, info(11, "Gone", 0, 0));
        assert!(registry.send_to(key, 118, b"one".to_vec()));
        registry.remove(key);
        assert!(!registry.send_to(key, 118, b"two".to_vec()));
        assert_eq!(rx.try_recv().unwrap().payload, b"one");
        // The sender is gone with the registry entry, so the channel reports a
        // disconnect rather than emptiness — either way, no second frame.
        assert!(rx.try_recv().is_err(), "the queued frame was the last");
    }

    #[test]
    fn head_chat_stores_and_clears_per_the_legacy_switch() {
        let registry = Registry::new();
        let (key, _rx) = join(&registry, info(11, "Hats", 0, 0));
        assert_eq!(registry.head_chat(key), None, "the switch starts off");

        registry.set_head_chat(key, Some("top of the world".into()));
        assert_eq!(registry.head_chat(key).as_deref(), Some("top of the world"));

        // `SetHeadChat(false, NULL)` clears the switch; the text is left
        // behind, which is invisible because 121 tests the switch.
        registry.set_head_chat(key, None);
        assert_eq!(registry.head_chat(key), None);
    }
}
