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
//! natural map key — except that it is **session-scoped**, not a database column:
//! the legacy's `m_uiCharID` is handed out by the session manager when it binds a
//! connection (`CharServer-Session.cpp:58`). Nothing guarantees two connections
//! cannot end up holding the same one, and the retired capture mode made that
//! certain by pinning every session to a single id — two clients then *shared* one
//! character, and a `char_unique`-keyed map would have let the second overwrite
//! the first: the first player's whisper would reach nobody and its head-chat store
//! would vanish on every disconnect.
//!
//! So the key is a serial handed out per connection ([`Registry::next_key`]),
//! and `char_unique` travels in the frames and in [`SessionInfo`] — which is
//! where the 119/121 lookups resolve it, exactly as the legacy does.
//!
//! [`Registry::find_by_charunique`] therefore has a defined but arbitrary answer
//! if two sessions ever report the same id: the **lowest** key. That cannot happen
//! while `allocate_char_unique` hands out distinct ids, but the ordering is pinned
//! by test rather than left to `HashMap` iteration, so the answer is deterministic
//! either way.
//!
//! # What "near" means
//!
//! The legacy fans a neighbour message out over a map's *cell* neighbourhood:
//! every cell in the 3×3 block around the sender's cell (`ITZMap.cpp:71-87`),
//! which is itself a screen-sized block — `CreateWorld(MAX_MAP_NUMBER,
//! MAX_CHAR_NUMBER, 800, 600, MAP_TILE_XSIZE, MAP_TILE_YSIZE)`
//! (`MapManager.cpp:82-87`) passes a cell size of 0, and 0 selects the
//! screen-in-tiles default (`ITZWorld.h:95-104`): `(800 + 80 - 1) / 80 = 10` and
//! `(600 + 40 - 1) / 40 = 15` (`map.h:33-36`).
//!
//! The cells themselves are *skewed*, not rectangular. `ITZMap::GetCellIndex`
//! (`ITZMap.cpp:116-122`) maps a tile into an isometric cell, and the skew is
//! what the viewport is shaped like — so an avatar walking toward a screen
//! corner stays fed exactly as long as it is on screen. That is
//! [`map_geometry`]'s job; this module just asks [`is_near`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use gz_proto::chat::{MAX_PLAYER_NAME, cut};
use gz_proto::friends::{encode_friend_login_response, encode_friend_logout_response};
use gz_proto::game::BasicInfo;
use gz_proto::map_geometry::{cell_of, cells_adjacent};
use gz_proto::opcodes_game::GameMsg;
use tokio::sync::Notify;
use tokio::sync::mpsc::UnboundedSender;

/// `MAX_CHAR_NUMBER` (`CommonHeader.h:556`) — `GetIDFromCharUnique` returns 0
/// for `charunique <= 0` and for `charunique >=` this (`CharManager.cpp:205`),
/// so neither can name a session.
pub const MAX_CHAR_NUMBER: i16 = 20_000;

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
    /// `clBI.uiKind` — the avatar/class id. Needed because `MakeCharInfo` is what
    /// gives a client an avatar to draw (`ServerMap.cpp:103`, and the appearance is
    /// read off `uiMakeCharKind`), not off the name or the id.
    pub class_kind: u16,
    /// `clIP.Life` — current life, in the 20 bits `uiLife` packs.
    pub life: u32,
    /// The character's effective `GetTotalMoveSpeed()` (`character_stats.move_speed`),
    /// in the 6 bits `uiTotalMoveSpeed` packs.
    pub move_speed: u8,
    /// `clBI.uiSex` — 1 male, 0 female.
    pub sex: u8,
    /// `clBI.uiGMMode` / `siGameMasterLevel`, from the authenticated account.
    pub gm_level: u8,
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
    /// The character's full stat block, captured at world entry so the 214
    /// `REQUEST_PERSON_IP` inspect answers with the target's `clIP` without a
    /// database round trip (`Server-Person.cpp:83`). Refreshed on every 250
    /// stat spend; `None` until `REQUEST_SELECTCHAR`.
    basic_info: Option<BasicInfo>,
    /// Accepted friendships, hydrated from `character_friends` at world entry
    /// and appended by a 6202 accept; the 6200 duplicate guard's
    /// `clFriend[i]` scan (`Server-Friend.cpp:52-64`) and the 6210/6219
    /// builders all read it.
    friends: Vec<FriendEntry>,
    /// The AFK clock: the `now_tick()` stamp of the last inbound frame that
    /// counted as player activity ([`Registry::note_activity`]), `None` until
    /// any frame is observed after registration. Sitting, vending and the
    /// client's automated keepalives are *not* activity, so they leave this
    /// stamp where it is — which is exactly what keeps an idle shopkeeper's
    /// clock running.
    last_activity_ms: Option<u64>,
    tx: UnboundedSender<Outbound>,
}

/// One accepted friendship, stored per owner session. The field set the 6210
/// friend-list and 6219 all-list need: the client renders a slot only when its
/// person id is > 0 (`Client-Friend.cpp:136`) and shows the group the 6203
/// already reported. Hydrated from `character_friends` at world entry and
/// appended by a 6202 accept, so a re-login keeps the list the database holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FriendEntry {
    /// The friend's person id — `cltSimplePerson.siPersonID`.
    pub person_id: i32,
    /// The group the friendship is stored under (`siGroupID`).
    pub group: u8,
    /// The friend's character name — the duplicate-check key.
    pub name: String,
    /// The friend's account id — `szAccountID[i]` in the 6219 all-list and the
    /// account 6207/6203 carry.
    pub account_id: String,
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
    /// Accounts with a live game-login connection, `account_id -> `[`LoginClaim`].
    /// Claimed at `GAMEMSG_REQUEST_LOGIN` and released when the connection ends,
    /// so a second login for the same account is refused while the first is still
    /// logged in — the legacy's one-login-per-account rule
    /// (`Server-Auth.cpp:51-120`, which scans `cltCharManager` for a character
    /// whose `siIDNum` matches the logging-in account and, on a match, closes
    /// *both* connections). The stored [`Notify`] is how the holder is closed:
    /// [`Self::claim_login`] queues the holder its duplicate-login notice on the
    /// stored `tx` and then wakes the `Notify`, the same order as the legacy,
    /// which sends `GAMEMSG_RESPONSE_ANOTHERAUTH` to the old character and only
    /// then sets its `bCloseSocketSwitch` (`Server-Auth.cpp:100-106`).
    logins: Mutex<HashMap<i32, LoginClaim>>,
}

/// One account's live login claim: the holder's session key, the signal that
/// closes it when a duplicate arrives, and its outbound queue, so the kick can
/// hand the holder its `GAMEMSG_RESPONSE_ANOTHERAUTH` notice before it closes.
type LoginClaim = (SessionKey, Arc<Notify>, UnboundedSender<Outbound>);

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
            logins: Mutex::new(HashMap::new()),
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

    /// Claims `account_id` for `key`'s connection. Returns the session key that
    /// already holds the account if another one does, in which case this
    /// connection must be refused; `None` means the claim succeeded (or was
    /// already this connection's).
    ///
    /// This is the legacy's duplicate-login guard (`Server-Auth.cpp:51-120`):
    /// the legacy scans `cltCharManager` for an online character whose `siIDNum`
    /// matches the account and aborts the second login, then closes *both*
    /// connections. Here the claim is taken at `GAMEMSG_REQUEST_LOGIN`, before
    /// the char list is served, so the second connection never selects a
    /// character; the caller turns the `Some(owner)` into a refusal, and the
    /// holder is kicked here — the holder-kick half of the legacy's two-way
    /// close. The kick is two steps in the legacy's order: `notice` (the
    /// `bNewUser = FALSE` frame the caller built) is queued on the holder's
    /// outbound channel, and only then is its `close` woken, so the holder's
    /// read loop writes the notice before it disconnects.
    ///
    /// `tx` is this connection's outbound queue, kept so a *future* duplicate can
    /// reach it the same way. `notice` is dropped unused when there is no
    /// conflict.
    pub fn claim_login(
        &self,
        account_id: i32,
        key: SessionKey,
        close: Arc<Notify>,
        tx: UnboundedSender<Outbound>,
        notice: Outbound,
    ) -> Option<SessionKey> {
        let mut logins = self.logins.lock().unwrap_or_else(PoisonError::into_inner);
        // Copy out the holder's key, close handle and queue before any
        // `send`/`notify`/re-lock, so the immutable borrow of `logins` ends here.
        let existing = logins
            .get(&account_id)
            .map(|(owner, close, tx)| (*owner, close.clone(), tx.clone()));
        if let Some((owner, holder_close, holder_tx)) = existing
            && owner != key
        {
            // Queued before the wake, so the frame is already in the channel when
            // the holder's loop sees `close` and drains it below. `send` fails
            // only once the holder's loop is gone, where there is nobody to tell.
            let _ = holder_tx.send(notice);
            drop(logins);
            holder_close.notify_one();
            return Some(owner);
        }
        logins.insert(account_id, (key, close, tx));
        None
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
                basic_info: None,
                friends: Vec::new(),
                last_activity_ms: None,
                tx,
            },
        );
    }

    /// Drops a connection's entry. Called by [`Registered`] on every exit path;
    /// a `key` that was never registered (a drop before `REQUEST_SELECTCHAR`)
    /// is a no-op.
    ///
    /// Before removing, broadcasts `GAMEMSG_RESPONSE_LOGOUT` (11) to every
    /// character in the departing session's 3×3 near block — the legacy's
    /// `SendPersonLeaveMsgToOthers` (`ServerMap.cpp:51-63`). This is distinct
    /// from [`Self::logout_departed`] (walking out of range): this is the
    /// *session ending* (logout, disconnect, crash), and must be sent
    /// synchronously because the `Drop` guard cannot await.
    ///
    /// Once the table lock is released it also pushes a 6208
    /// `RESPONSE_FRIENDLOGOUT` to every still-online friend of the departing
    /// character, so their client clears its connect status
    /// (`NNewFriendDlg.cpp:889-905`). The fan runs after the lock because it
    /// looks friends up through [`Self::find_by_character_id`], which takes
    /// the same lock.
    pub fn remove(&self, key: SessionKey) {
        let friend_notices: Vec<(i32, i32)> = {
            let mut sessions = self.lock();
            let mut notices = Vec::new();
            if let Some(departing) = sessions.get(&key) {
                let info = departing.info.clone();
                // Encode LOGOUT (11) = 4-byte char_unique (little-endian)
                let mut payload = Vec::with_capacity(4);
                payload.extend_from_slice(&info.char_unique.to_le_bytes());
                // Broadcast to near block (excluding self, which is already going away)
                for (other_key, other) in sessions.iter() {
                    if *other_key != key && is_near(&info, &other.info) {
                        let _ = other.tx.send(Outbound {
                            cmd: GameMsg::ResponseLogout as u16,
                            payload: payload.clone(),
                        });
                    }
                }
                // The 6208 recipients are read now, while the entry is still
                // under the lock; the sends happen after it drops.
                if let Some(my_person_id) = departing.basic_info.as_ref().map(|b| b.character_id) {
                    for friend in &departing.friends {
                        notices.push((my_person_id, friend.person_id));
                    }
                }
            }
            sessions.remove(&key);
            notices
        };
        // Drop any account-login claim this connection held, so a disconnect —
        // clean or not — frees the account for the next login (`Registered`'s
        // drop is the single exit path).
        self.logins
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|_, (owner, _, _)| *owner != key);
        for (my_person_id, friend_person_id) in friend_notices {
            if let Some(other) = self.find_by_character_id(friend_person_id)
                && other != key
            {
                let _ = self.send_to(
                    other,
                    GameMsg::ResponseFriendlogout as u16,
                    encode_friend_logout_response(my_person_id),
                );
            }
        }
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

    /// Moves a registered character to another map, or does nothing if it is gone.
    ///
    /// [`Self::update_pos`] cannot express a warp: the cell a character is near is
    /// a function of the map *and* the tile (`is_near` returns false across maps),
    /// so a session that kept its old map would keep fans aimed at the map it
    /// left.
    pub fn update_map_pos(&self, key: SessionKey, map_id: i32, x: i32, y: i32) {
        if let Some(session) = self.lock().get_mut(&key) {
            session.info.map_id = map_id;
            session.info.x = x;
            session.info.y = y;
        }
    }

    /// Clocks one inbound frame for `key`: returns the idle time (ms) the
    /// session had accumulated *before* this frame, and advances the AFK
    /// clock only when `cmd` is a frame that proves a player is at the
    /// keyboard ([`counts_as_activity`]). `None` when the session is not
    /// registered — every frame a connection sends before
    /// `REQUEST_SELECTCHAR`.
    ///
    /// The clock starts at the first frame observed after registration, so a
    /// freshly entered character reports 0: the client's own automated
    /// traffic (the 230 on its 30 s timer, the heartbeat) is guaranteed to
    /// arrive within 30 s of world entry, which bounds how far the start can
    /// lag the entry itself.
    ///
    /// Read and conditional write happen under one lock acquisition — this
    /// sits on the per-frame path of every connection. The returned reading is
    /// what the AI gate consumes (`gz_sim::ai::force_aggro`).
    pub fn note_activity(&self, key: SessionKey, cmd: u16, now_ms: u64) -> Option<u64> {
        let mut sessions = self.lock();
        let session = sessions.get_mut(&key)?;
        let idle_ms = session
            .last_activity_ms
            .map_or(0, |last| now_ms.saturating_sub(last));
        if session.last_activity_ms.is_none() || counts_as_activity(cmd) {
            session.last_activity_ms = Some(now_ms);
        }
        Some(idle_ms)
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

    /// Records a character's full stat block, captured at world entry and
    /// refreshed on every stat spend, so the 214 inspect answers with the
    /// target's current `clIP`.
    pub fn set_basic_info(&self, key: SessionKey, basic: BasicInfo) {
        if let Some(session) = self.lock().get_mut(&key) {
            session.basic_info = Some(basic);
        }
    }

    /// A copy of one session's stored stat block, or `None` if it is not
    /// registered or none was captured yet. The 214 `REQUEST_PERSON_IP` pull
    /// reads this (via [`Self::find_by_charunique`]) instead of the database.
    #[must_use]
    pub fn basic_info(&self, key: SessionKey) -> Option<BasicInfo> {
        self.lock().get(&key).and_then(|s| s.basic_info.clone())
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

    /// The session whose captured stat block carries `person_id` — the 123
    /// whisper-channel fallback when the request names no `char_unique`
    /// (`GetIDFromPersonID`, `Server-System.cpp:621`). A session that never
    /// finished world entry has no basic info and cannot be named by person id.
    #[must_use]
    pub fn find_by_character_id(&self, person_id: i32) -> Option<SessionKey> {
        self.lock()
            .iter()
            .filter(|(_, s)| {
                s.basic_info
                    .as_ref()
                    .is_some_and(|b| b.character_id == person_id)
            })
            .map(|(key, _)| *key)
            .min()
    }

    /// Records an accepted friendship for `key`; `Some(i)` is the 0-based slot
    /// the entry landed in, `None` means nothing was added — the friend's name
    /// was already listed (the 6200 duplicate guard mirroring the legacy's
    /// `clFriend[i]` name scan, `Server-Friend.cpp:52-64`) or the session is gone.
    pub fn friend_add(&self, key: SessionKey, entry: FriendEntry) -> Option<usize> {
        let mut sessions = self.lock();
        let session = sessions.get_mut(&key)?;
        if session.friends.iter().any(|f| f.name == entry.name) {
            return None;
        }
        session.friends.push(entry);
        Some(session.friends.len() - 1)
    }

    /// Whether `key` already lists a friend named `name`.
    #[must_use]
    pub fn is_friend(&self, key: SessionKey, name: &str) -> bool {
        self.lock()
            .get(&key)
            .is_some_and(|s| s.friends.iter().any(|f| f.name == name))
    }

    /// The session's accepted friendships, vec index = list slot — the order
    /// 6203 reported, which the 6210 answer must echo back.
    #[must_use]
    pub fn friends_of(&self, key: SessionKey) -> Vec<FriendEntry> {
        self.lock()
            .get(&key)
            .map(|s| s.friends.clone())
            .unwrap_or_default()
    }

    /// Replaces a session's friend list wholesale — the hydrate-on-world-entry
    /// path, which fills the vec from `character_friends` so a re-login keeps
    /// the stored friendships. `friends_of` answers 6209/6219 from this.
    pub fn set_friends(&self, key: SessionKey, friends: Vec<FriendEntry>) {
        if let Some(session) = self.lock().get_mut(&key) {
            session.friends = friends;
        }
    }

    /// Tells every friend of `key`'s character who is online that it just
    /// logged in — the legacy's `siLogin == 1` loop
    /// (`ServerDB-Friend.cpp:45-52`), which runs when a character's DB friend
    /// list is loaded. The group carried is the one *this* list stores the
    /// friend under (`ServerDB-Friend.cpp:49` passes its own `siGroupID[i]`);
    /// person, charunique and account are the logging-in character's.
    ///
    /// Returns how many friends took the frame.
    pub fn friend_login_push(&self, key: SessionKey) -> usize {
        let Some(info) = self.info(key) else {
            return 0;
        };
        let Some(basic) = self.basic_info(key) else {
            return 0;
        };
        let friends = self.friends_of(key);
        let mut sent = 0;
        for friend in friends {
            if let Some(other) = self.find_by_character_id(friend.person_id)
                && other != key
                && self.send_to(
                    other,
                    GameMsg::ResponseFriendlogin as u16,
                    encode_friend_login_response(
                        friend.group,
                        basic.character_id,
                        i32::from(info.char_unique),
                        &basic.account_id,
                    ),
                )
            {
                sent += 1;
            }
        }
        sent
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

    /// Every *other* session in `origin`'s 3×3 near block, with its key, ordered
    /// by key so a caller sees a deterministic list.
    ///
    /// This is the newcomer-side query of [`cltServerMap::SendNewOthersInfoToPerson`]
    /// (`ServerMap.cpp:73-150`): the set of characters a just-arrived player has to
    /// be told about. `skip` is the newcomer's own key — the legacy never lists the
    /// receiver among "others", and a client that receives its own record in a 6
    /// would re-`SetChar` itself.
    #[must_use]
    pub fn near_others(
        &self,
        origin: &SessionInfo,
        skip: SessionKey,
    ) -> Vec<(SessionKey, SessionInfo)> {
        let sessions = self.lock();
        let mut out: Vec<(SessionKey, SessionInfo)> = sessions
            .iter()
            .filter(|(key, session)| **key != skip && is_near(origin, &session.info))
            .map(|(key, session)| (*key, session.info.clone()))
            .collect();
        out.sort_by_key(|(key, _)| *key);
        out
    }

    /// The sessions that were watching `was` and are *not* watching `now` — the
    /// symmetric counterpart of [`Self::near_others`], and the recipient set for
    /// `GAMEMSG_RESPONSE_LOGOUT`.
    ///
    /// The legacy keeps two cell sets per character, `m_pNewCell` and
    /// `m_pRemoveCell`, and the walk that fills them is the same 3×3 block
    /// (`ITZMap.cpp:73-150`): `m_pRemoveCell` is what it entered minus what it is
    /// still in. `SendPersonLeaveMsgToOthers` (`ServerMap.cpp:51-63`) sends the
    /// logout to exactly that set. Recomputing it as `near(was) - near(now)`
    /// rather than carrying a persistent set is the same answer, because the
    /// neighbourhood is a pure function of the tile.
    ///
    /// `mover_key` is excluded: the mover is inside `now` and was inside `was`, so
    /// it is not in the difference anyway, but naming it keeps that true by
    /// construction rather than by arithmetic.
    #[must_use]
    pub fn no_longer_near(
        &self,
        was: &SessionInfo,
        now: &SessionInfo,
        mover_key: SessionKey,
    ) -> Vec<(SessionKey, SessionInfo)> {
        let sessions = self.lock();
        let mut out: Vec<(SessionKey, SessionInfo)> = sessions
            .iter()
            .filter(|(key, session)| {
                **key != mover_key && is_near(was, &session.info) && !is_near(now, &session.info)
            })
            .map(|(key, session)| (*key, session.info.clone()))
            .collect();
        out.sort_by_key(|(key, _)| *key);
        out
    }

    /// The sessions that could **not** see the mover at `was` and can now — the
    /// legacy's `m_pNewCell` (`ITZMap.cpp:73-150`), and the recipient set for
    /// the appearance half of a cell crossing.
    ///
    /// This is the mirror of [`Self::no_longer_near`] and it matters for the
    /// opposite reason: `GAMEMSG_RESPONSE_LOGINMANY` (6) is **not**
    /// idempotent on the client. `SetCharReservedChar`
    /// (`Client-SetChar.cpp:111-119`) sees a record for a `char_unique` it
    /// already holds, **deletes that avatar** (`DeleteInCell` + `DeleteChar`)
    /// and re-`SetChar`s it from the record — so a 6 aimed at somebody who is
    /// already watching does not refresh them, it teleports the avatar to
    /// whatever cell the record names and discards the in-flight walk order.
    /// That is "walks, then warps back" seen from the other client.
    ///
    /// The legacy cannot emit that frame: `SendNewOthersInfoToPerson` and the
    /// matching `GAMEMSG_RESPONSE_LOGIN` only ever walk `m_pNewCell`, the cells
    /// the mover just stepped into, whose occupants by definition could not see
    /// it a moment ago. Recomputing `near(now) - near(was)` is the same set,
    /// because adjacency is symmetric: `p` sits in a cell the mover just
    /// entered exactly when the mover just became adjacent to `p`.
    ///
    /// At world entry there is no `was`, so the whole block is new — that is the
    /// `None` the callers of [`crate::announce_nearby_characters`] pass.
    #[must_use]
    pub fn newly_near(
        &self,
        was: &SessionInfo,
        now: &SessionInfo,
        mover_key: SessionKey,
    ) -> Vec<(SessionKey, SessionInfo)> {
        let sessions = self.lock();
        let mut out: Vec<(SessionKey, SessionInfo)> = sessions
            .iter()
            .filter(|(key, session)| {
                **key != mover_key && !is_near(was, &session.info) && is_near(now, &session.info)
            })
            .map(|(key, session)| (*key, session.info.clone()))
            .collect();
        out.sort_by_key(|(key, _)| *key);
        out
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

/// Whether `other` is inside `origin`'s 3×3 cell block on the same map — the
/// near set `ITZMap.cpp:71-87` walks.
fn is_near(origin: &SessionInfo, other: &SessionInfo) -> bool {
    if origin.map_id != other.map_id {
        return false;
    }
    cells_adjacent(
        cell_of(origin.x, origin.y, origin.map_id),
        cell_of(other.x, other.y, other.map_id),
    )
}

/// Whether an inbound frame counts as the player *doing* something — i.e.
/// resets the AFK clock ([`Registry::note_activity`]).
///
/// The default is **yes**: a frame resets the clock unless it is listed
/// below. Anything unlisted — including opcodes this binary does not handle
/// yet — is treated as player input, so a real action never fails to reset.
/// What is listed is either traffic the client sends entirely on its own
/// (it proves nothing about the human) or a posture the AFK rule deliberately
/// does not excuse (sitting and vending are the states that must keep
/// accumulating).
///
/// | opcode | why it is not the player |
/// |---|---|
/// | `100` heartbeat | automated keepalive; the legacy handler is an empty no-op (`Server-System.cpp:520-524`) |
/// | `230` changestatusinfo | rides the client's process-global 30 s timer (the 230 arm in `main.rs`) |
/// | `121` headchat, `275` addinfo, `6229` electionstump, `7000` personalshopbanner, `8345` newitemrepairrequestinfo | the client fires these **because another character appeared on screen** with the matching flag (`CommonLogic-SetChar.cpp:196-251`) — a passer-by's `SetChar`, not the sender's input |
/// | `7231` sitdown | the sitting posture itself (sit and stand are one toggle) |
/// | `7006`/`7008` start/stop personalshop | entering and leaving the vending posture; running a shop is AFK time |
///
/// shortcut: any *other* client-auto frame resets the clock until it is added
/// here — the per-frame `recv opcode` log line in `main.rs` is how such a
/// frame shows up in triage.
fn counts_as_activity(cmd: u16) -> bool {
    const NOT_PLAYER: &[u16] = &[
        GameMsg::RequestHeartbeat as u16,
        GameMsg::RequestChangestatusinfo as u16,
        GameMsg::RequestHeadchat as u16,
        GameMsg::RequestAddinfo as u16,
        GameMsg::RequestElectionstump as u16,
        GameMsg::RequestPersonalshopbanner as u16,
        GameMsg::RequestNewitemrepairrequestinfo as u16,
        GameMsg::RequestSitdown as u16,
        GameMsg::RequestStartpersonalshop as u16,
        GameMsg::RequestStoppersonalshop as u16,
    ];
    !NOT_PLAYER.contains(&cmd)
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
            class_kind: 1,
            life: 161,
            move_speed: 12,
            sex: 1,
            gm_level: 0,
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

    /// One live login per account: the first claim wins, a second connection is
    /// told who holds it, queues that holder its notice and wakes the holder's
    /// close signal (the holder-kick), the holder may re-claim, and the holder's
    /// teardown frees the account.
    #[tokio::test]
    async fn only_one_login_per_account_at_a_time() {
        let registry = Arc::new(Registry::new());
        let first = Registry::next_key();
        let second = Registry::next_key();
        let first_close = Arc::new(Notify::new());
        let notice = |cmd: u16| Outbound {
            cmd,
            payload: vec![cmd as u8],
        };
        let (first_tx, mut first_rx) = tokio::sync::mpsc::unbounded_channel();

        assert_eq!(
            registry.claim_login(7, first, first_close.clone(), first_tx.clone(), notice(0)),
            None,
            "the first claim wins"
        );
        assert_eq!(
            registry.claim_login(
                7,
                second,
                Arc::new(Notify::new()),
                tokio::sync::mpsc::unbounded_channel().0,
                notice(GameMsg::ResponseAnotherauth as u16),
            ),
            Some(first),
            "the second connection is refused and told who holds the account"
        );
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_millis(100), first_rx.recv())
                .await
                .expect("the holder's outbound queue got the notice"),
            Some(notice(GameMsg::ResponseAnotherauth as u16)),
            "the holder is handed its duplicate-login notice before it is woken"
        );
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(100),
                first_close.notified()
            )
            .await
            .is_ok(),
            "the holder's close signal is woken so its read loop disconnects it"
        );
        assert_eq!(
            registry.claim_login(7, first, first_close.clone(), first_tx.clone(), notice(0)),
            None,
            "the holder re-claiming its own account is not a conflict"
        );
        assert_eq!(
            registry.claim_login(
                8,
                second,
                Arc::new(Notify::new()),
                tokio::sync::mpsc::unbounded_channel().0,
                notice(0),
            ),
            None,
            "a different account is free"
        );

        // The holder's guard dropping is every exit path (`Registered`), and it
        // must release the account.
        let guard = Registered::new(registry.clone(), first);
        drop(guard);
        assert_eq!(
            registry.claim_login(
                7,
                second,
                Arc::new(Notify::new()),
                tokio::sync::mpsc::unbounded_channel().0,
                notice(0),
            ),
            None,
            "the account is free once the holder is gone"
        );
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
        // (40, 45) on map 7 — 68 tiles wide, so `sx = (68-45-1+40)/20 = 3`,
        // `sy = 85/30 = 2`. The skewed cell, not the old `4, 3` rectangle.
        assert_eq!(cell_of(session.x, session.y, session.map_id), (3, 2));
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
    /// walk of `ITZMap.cpp:71-87` clamped at the map's edge, over the skewed
    /// cells `GetCellIndex` produces (`ITZMap.cpp:116-122`).
    ///
    /// Every coordinate below is chosen for the cell it lands in on map 7
    /// (68 tiles wide), which is asserted rather than assumed — the whole point
    /// of B2 is that these are no longer `x/10, y/15`.
    #[test]
    fn broadcast_near_is_the_three_by_three_cell_block_on_one_map() {
        let registry = Registry::new();
        // Origin cell (3, 1): sx = (68-20-1+15)/20 = 3, sy = 35/30 = 1.
        let (origin_key, _rself) = join(&registry, info(11, "Origin", 15, 20));
        // Inside the block: the same cell (3, 1) and the near neighbour (3, 0).
        let (_same, mut rsame) = join(&registry, info(12, "SameCell", 29, 29));
        let (_neighbour, mut rneighbour) = join(&registry, info(13, "Neighbour", 0, 0));
        // Two cells away on sx — (50, 5) gives sx = 112/20 = 5, sy = 55/30 = 1.
        let (_far, mut rfar) = join(&registry, info(14, "Far", 50, 5));
        // Same coordinates, different map.
        let (_other, mut rother) = {
            let mut elsewhere = info(15, "Elsewhere", 15, 20);
            elsewhere.map_id = 8;
            join(&registry, elsewhere)
        };

        let origin = registry.info(origin_key).unwrap();
        assert_eq!(origin.map_id, 7);
        assert_eq!(cell_of(origin.x, origin.y, origin.map_id), (3, 1));
        assert_eq!(cell_of(29, 29, 7), (3, 1), "shares the origin's cell");
        assert_eq!(cell_of(0, 0, 7), (3, 0), "one cell away on sy");
        assert_eq!(cell_of(50, 5, 7), (5, 1), "two cells away on sx");

        let sent = registry.broadcast_near(&origin, 122, b"head".to_vec());
        assert_eq!(sent, 3, "the far cell and the other map are out");

        for rx in [&mut rsame, &mut rneighbour] {
            let frame = rx.try_recv().unwrap();
            assert_eq!(frame.cmd, 122);
            assert_eq!(frame.payload, b"head");
        }
        assert_silent(&mut rfar, "two cells away");
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
        let (_neighbour, mut rneighbour) = join(&registry, info(13, "Neighbour", 0, 0));
        let (_far, mut rfar) = join(&registry, info(14, "Far", 50, 5));

        let origin = registry.info(origin_key).unwrap();
        assert_eq!(
            registry.broadcast_near_others(&origin, origin_key, 200, b"cell".to_vec()),
            2,
            "the two near observers, not the sender"
        );

        for rx in [&mut rsame, &mut rneighbour] {
            let frame = rx.try_recv().expect("a near observer gets the frame");
            assert_eq!(frame.cmd, 200);
            assert_eq!(frame.payload, b"cell");
        }
        assert_silent(&mut rself, "the sender writes its own copy directly");
        assert_silent(&mut rfar, "two cells away");

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
        // (5, 5) on map 7: sx = (68-5-1+5)/20 = 3, sy = 10/30 = 0 — cell (3, 0).
        let (walker_key, _rwalker) = join(&registry, info(11, "Walker", 5, 5));
        // (27, 87): sx = (68-87-1+27)/20 = 0, sy = 114/30 = 3 — cell (0, 3).
        let (_ahead, mut rahead) = join(&registry, info(12, "Ahead", 27, 87));
        let (_behind, mut rbehind) = join(&registry, info(13, "Behind", 5, 5));

        let mut origin = registry.info(walker_key).unwrap();
        assert_eq!(cell_of(origin.x, origin.y, origin.map_id), (3, 0));
        assert_eq!(
            registry.broadcast_near_others(&origin, walker_key, 200, b"a".to_vec()),
            1,
            "only the character sharing its cell"
        );
        assert_silent(&mut rahead, "cell (0, 3) is three cells away on sx");
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
        // Cell (3, 0) to cell (0, 3) is three steps out on both axes, so the
        // character left behind drops out of the fan — the observer set is not a
        // fixed list.
        assert_silent(&mut rbehind, "far away once the walker moved on");
    }

    /// The other half of a cell crossing: `near(was) - near(now)`, i.e. who the
    /// mover just walked out of sight of. This is `m_pRemoveCell`
    /// (`ITZMap.cpp:73-150`) feeding `GAMEMSG_RESPONSE_LOGOUT`
    /// (`ServerMap.cpp:51-63`), and the frame the port never sent.
    #[test]
    fn no_longer_near_is_the_difference_between_the_two_neighbourhoods() {
        let registry = Registry::new();
        // All cells below are on map 7 (68 tiles wide) and are asserted, not
        // assumed. Origin (15, 20) -> (3, 1); after the step (60, 40) -> (4, 3).
        let (mover_key, _rmover) = join(&registry, info(11, "Mover", 15, 20));
        // (32, 35) -> (3, 2): one cell from (3, 1) and one cell from (4, 3).
        let (_stays, _rstays) = join(&registry, info(12, "Stays", 32, 35));
        // (15, 10) -> (3, 0): one cell from (3, 1), three cells from (4, 3).
        let (leaves_key, _rleaves) = join(&registry, info(13, "Leaves", 15, 10));
        // (64, 28) -> (5, 3): one cell from (4, 3), two from (3, 1).
        let (arrives_key, _rarrives) = join(&registry, info(15, "Arrives", 64, 28));
        // (50, 5) -> (5, 1): two cells from both, never inside either block.
        let (_far, _rfar) = join(&registry, info(14, "Far", 50, 5));

        let was = registry.info(mover_key).unwrap();
        let mut now = was.clone();
        now.x = 60;
        now.y = 40;
        assert_eq!(cell_of(was.x, was.y, was.map_id), (3, 1));
        assert_eq!(cell_of(now.x, now.y, now.map_id), (4, 3));
        assert_eq!(cell_of(32, 35, 7), (3, 2), "stays in both blocks");
        assert_eq!(cell_of(15, 10, 7), (3, 0), "in the old block only");
        assert_eq!(cell_of(64, 28, 7), (5, 3), "in the new block only");
        assert_eq!(cell_of(50, 5, 7), (5, 1), "in neither block");

        let lost = registry.no_longer_near(&was, &now, mover_key);
        assert_eq!(
            lost.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
            vec![leaves_key],
            "only the character that fell out of the 3x3 block"
        );
        assert!(
            !lost.iter().any(|(key, _)| *key == mover_key),
            "a client is never told to delete itself"
        );

        // Walking back is the mirror: the character that came into view is the
        // one that now drops out. Same query, arguments swapped.
        let back = registry.no_longer_near(&now, &was, mover_key);
        assert_eq!(
            back.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
            vec![arrives_key],
            "the inverse direction picks the character that gained sight"
        );
    }

    /// The appearance half: `near(now) - near(was)`, the legacy's `m_pNewCell`,
    /// and the only set that may be sent a `GAMEMSG_RESPONSE_LOGINMANY`.
    ///
    /// Sending the 6 to somebody who is already adjacent is not a harmless
    /// refresh. `SetCharReservedChar` (`Client-SetChar.cpp:111-119`) finds a
    /// record for a `char_unique` it already holds and **deletes that avatar**
    /// (`DeleteInCell` + `DeleteChar`) before re-`SetChar`ing it from the record —
    /// so a walker's observer would see it warp back to whatever cell the record
    /// names, with the 241's walk order discarded along with it.
    #[test]
    fn newly_near_is_only_the_cells_the_mover_just_entered() {
        let registry = Registry::new();
        // Same five sessions and cells as `no_longer_near_...` above, so the two
        // halves of one crossing are pinned against each other.
        let (mover_key, _rmover) = join(&registry, info(11, "Mover", 15, 20));
        let (stays_key, _rstays) = join(&registry, info(12, "Stays", 32, 35));
        let (leaves_key, _rleaves) = join(&registry, info(13, "Leaves", 15, 10));
        let (arrives_key, _rarrives) = join(&registry, info(15, "Arrives", 64, 28));
        let (far_key, _rfar) = join(&registry, info(14, "Far", 50, 5));

        let was = registry.info(mover_key).unwrap();
        let mut now = was.clone();
        now.x = 60;
        now.y = 40;

        let gained = registry.newly_near(&was, &now, mover_key);
        let gained_keys: Vec<SessionKey> = gained.iter().map(|(key, _)| *key).collect();
        assert_eq!(
            gained_keys,
            vec![arrives_key],
            "only the character that could not see the mover before this step"
        );
        // The three that must NOT be announced to, each for its own reason.
        assert!(
            !gained_keys.contains(&stays_key),
            "already watching: a repeat 6 would delete and recreate its avatar"
        );
        assert!(!gained_keys.contains(&leaves_key), "not in the new block");
        assert!(!gained_keys.contains(&far_key), "in neither block");
        assert!(
            !gained_keys.contains(&mover_key),
            "a client is never sent a record of itself"
        );

        // Walking back is the mirror, exactly as it is for `no_longer_near`.
        let back = registry.newly_near(&now, &was, mover_key);
        assert_eq!(
            back.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
            vec![leaves_key],
            "the inverse direction picks the character that gained sight"
        );
    }

    /// A character that never moved has an empty difference even though it may
    /// have a large neighbourhood — `step_movement` must not log out an observer
    /// on every tile step, only on a cell crossing.
    #[test]
    fn no_longer_near_is_empty_when_the_cell_did_not_change() {
        let registry = Registry::new();
        let (mover_key, _rmover) = join(&registry, info(11, "Mover", 15, 20));
        let (_watcher, _rwatcher) = join(&registry, info(12, "Watcher", 0, 0));

        let was = registry.info(mover_key).unwrap();
        let mut now = was.clone();
        now.x = 22; // Still cell (3, 1): sx = (68-20-1+22)/20 = 3.
        assert_eq!(
            cell_of(now.x, now.y, now.map_id),
            cell_of(was.x, was.y, was.map_id)
        );
        assert!(registry.no_longer_near(&was, &now, mover_key).is_empty());
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

    /// Hydrating a session's friends and pushing 6207 tells every friend who
    /// is currently online that this character logged in — the legacy's
    /// `siLogin == 1` loop (`ServerDB-Friend.cpp:45-52`). The frame carries the
    /// *sender's* group for the friend, and its own person id / charunique /
    /// account; an online session whose person id was never set is skipped, and
    /// a session without basic_info has nobody to announce.
    #[test]
    fn friend_login_push_notifies_the_online_friends() {
        let registry = Registry::new();
        let (alice, _ra) = join(&registry, info(11, "Alice", 0, 0));
        let (bob, mut rb) = join(&registry, info(12, "Bob", 0, 0));
        let (_carol, _rc) = join(&registry, info(13, "Carol", 0, 0));
        registry.set_basic_info(
            alice,
            BasicInfo {
                character_id: 42,
                ..basic_info_fixture(34)
            },
        );
        registry.set_basic_info(
            bob,
            BasicInfo {
                character_id: 43,
                ..basic_info_fixture(20)
            },
        );

        // Alice lists Bob under group 2 and Carol under group 1 — but Carol's
        // person id (44) has no session registering it, so she stays offline.
        registry.set_friends(
            alice,
            vec![
                FriendEntry {
                    person_id: 43,
                    group: 2,
                    name: "Bob".into(),
                    account_id: "tester".into(),
                },
                FriendEntry {
                    person_id: 44,
                    group: 1,
                    name: "Carol".into(),
                    account_id: "tester".into(),
                },
            ],
        );

        assert_eq!(
            registry.friend_login_push(alice),
            1,
            "Bob is online; Carol has no registered person id"
        );
        let frame = rb.try_recv().unwrap();
        assert_eq!(frame.cmd, GameMsg::ResponseFriendlogin as u16);
        let body = &frame.payload;
        assert_eq!(body[0], 2, "Bob's group in Alice's own list");
        assert_eq!(
            i32::from_le_bytes(body[4..8].try_into().unwrap()),
            42,
            "Alice's person id"
        );
        assert_eq!(
            i32::from_le_bytes(body[8..12].try_into().unwrap()),
            11,
            "Alice's charunique"
        );
        assert_eq!(&body[12..18], b"tester", "Alice's account id");
        assert_eq!(body.len(), gz_proto::friends::RESPONSE_FRIENDLOGIN_SIZE);

        // A session that never world-entered has no basic_info, so no push.
        let (stranger, mut rs) = join(&registry, info(14, "Stranger", 0, 0));
        assert_eq!(registry.friend_login_push(stranger), 0);
        assert_silent(&mut rs, "no person to announce, nothing sent");
    }

    /// A session ending pushes 6208 to the friends of its character who are
    /// still online, so their client clears the person's connect status
    /// (`NNewFriendDlg.cpp:889-905`). The register-time LOGOUT fan goes first.
    #[test]
    fn remove_tells_online_friends_the_character_went_offline() {
        let registry = Registry::new();
        let (alice, mut ra) = join(&registry, info(11, "Alice", 0, 0));
        let (bob, mut rb) = join(&registry, info(12, "Bob", 0, 0));
        registry.set_basic_info(
            alice,
            BasicInfo {
                character_id: 42,
                ..basic_info_fixture(34)
            },
        );
        registry.set_basic_info(
            bob,
            BasicInfo {
                character_id: 43,
                ..basic_info_fixture(20)
            },
        );
        // Alice listed Bob (person 43) — a friend of the departing character
        // who is still online, so *his* client must learn she went.
        registry.set_friends(
            alice,
            vec![FriendEntry {
                person_id: 43,
                group: 1,
                name: "Bob".into(),
                account_id: "tester".into(),
            }],
        );

        registry.remove(alice);

        let logout = rb.try_recv().unwrap();
        assert_eq!(logout.cmd, GameMsg::ResponseLogout as u16);
        let gone = rb.try_recv().unwrap();
        assert_eq!(gone.cmd, GameMsg::ResponseFriendlogout as u16);
        assert_eq!(
            i32::from_le_bytes(gone.payload.try_into().unwrap()),
            42,
            "the departing character's person id"
        );
        // Alice's session is gone with the removal, so her inbox is closed
        // (either `Empty` or `Disconnected`) — but had the fan wrongly queued
        // her a notice, it would surface here as `Ok`.
        assert!(
            matches!(
                ra.try_recv(),
                Err(TryRecvError::Empty | TryRecvError::Disconnected)
            ),
            "Alice's own inbox gets no friend notice"
        );
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

    /// A stat block for the inspect test — the fields 215's `clIP` reads, with
    /// distinctive values so a wrong lookup would show.
    fn basic_info_fixture(level: i16) -> BasicInfo {
        BasicInfo {
            account_idnum: 7,
            account_id: "tester".into(),
            slot: 1,
            sex: 1,
            class_kind: 2,
            home_village: 3,
            map_index: 7,
            x: 15,
            y: 20,
            character_id: 42,
            name: "Target".into(),
            str_: 10,
            dex: 11,
            vit: 12,
            mag: 13,
            hnd: 14,
            luk: 15,
            wis: 16,
            level,
            bonus: 2,
            life: 200,
            mana: 100,
            exp: 1234,
            money: 5000,
        }
    }

    /// The 214 inspect reads the target's block out of the registry, so it has
    /// to be stored at world entry, findable by `char_unique`, refreshable on a
    /// stat spend, and gone once the session leaves.
    #[test]
    fn basic_info_is_stored_read_back_and_refreshed_for_the_inspect() {
        let registry = Registry::new();
        let (key, _rx) = join(&registry, info(1471, "Target", 15, 20));
        assert_eq!(registry.basic_info(key), None, "nothing captured yet");

        // The handler's path: char_unique -> key -> block.
        let lookup = |cu: i16| {
            registry
                .find_by_charunique(cu)
                .and_then(|k| registry.basic_info(k))
        };
        assert_eq!(lookup(1471), None);

        registry.set_basic_info(key, basic_info_fixture(20));
        assert_eq!(lookup(1471).unwrap().level, 20);

        // A 250 spend re-captures the block, so the next inspect is not stale.
        registry.set_basic_info(key, basic_info_fixture(21));
        assert_eq!(lookup(1471).unwrap().level, 21);

        assert_eq!(lookup(0), None, "the id bound still applies");
        assert_eq!(lookup(1470), None, "an unseen character has no block");

        registry.remove(key);
        assert_eq!(lookup(1471), None, "the block goes with the session");
    }

    /// The exempt table is the whole input side of the AFK rule: automated
    /// keepalives, the `SetChar`-driven fetches and the two postures never
    /// reset the clock; everything else — movement, chat, a real shop
    /// transaction, even opcodes this binary does not handle yet — does, so
    /// an unlisted *player* action can never fail to count.
    #[test]
    fn only_frames_that_prove_a_player_reset_the_idle_clock() {
        for cmd in [
            GameMsg::RequestHeartbeat as u16,
            GameMsg::RequestChangestatusinfo as u16,
            GameMsg::RequestHeadchat as u16,
            GameMsg::RequestAddinfo as u16,
            GameMsg::RequestElectionstump as u16,
            GameMsg::RequestPersonalshopbanner as u16,
            GameMsg::RequestNewitemrepairrequestinfo as u16,
            GameMsg::RequestSitdown as u16,
            GameMsg::RequestStartpersonalshop as u16,
            GameMsg::RequestStoppersonalshop as u16,
        ] {
            assert!(!counts_as_activity(cmd), "opcode {cmd} must not reset");
        }
        for cmd in [
            GameMsg::RequestUserorder as u16,       // movement
            GameMsg::RequestChat as u16,            // player speech
            GameMsg::RequestHit as u16,             // combat
            GameMsg::RequestBuypersonalshop as u16, // a real shop transaction
            4242,                                   // unknown: resets (safe for the player)
        ] {
            assert!(counts_as_activity(cmd), "opcode {cmd} must reset");
        }
    }

    /// The clock itself: activity advances it, exempt frames read it without
    /// touching it, and a connection with no session has no clock.
    #[test]
    fn the_idle_clock_advances_only_for_activity() {
        let registry = Registry::new();
        let (key, _rx) = join(&registry, info(11, "Idler", 0, 0));

        // The first frame after registration seeds the clock — activity or
        // not — and has read 0 idle so far.
        assert_eq!(
            registry.note_activity(key, GameMsg::RequestHeartbeat as u16, 1_000),
            Some(0)
        );
        // A movement 4 s later: readings are taken *before* the frame lands,
        // so this is the idle the movement is about to clear.
        assert_eq!(
            registry.note_activity(key, GameMsg::RequestUserorder as u16, 5_000),
            Some(4_000)
        );
        // Keepalives read the running clock without advancing it...
        assert_eq!(
            registry.note_activity(key, GameMsg::RequestHeartbeat as u16, 9_000),
            Some(4_000)
        );
        assert_eq!(
            registry.note_activity(key, GameMsg::RequestChangestatusinfo as u16, 12_000),
            Some(7_000)
        );
        // ...and neither does sitting.
        assert_eq!(
            registry.note_activity(key, GameMsg::RequestSitdown as u16, 20_000),
            Some(15_000)
        );
        // The next real action lands on a clock that ran straight through,
        // and only then moves it.
        assert_eq!(
            registry.note_activity(key, GameMsg::RequestChat as u16, 21_000),
            Some(16_000)
        );
        assert_eq!(
            registry.note_activity(key, GameMsg::RequestHeartbeat as u16, 21_500),
            Some(500),
            "the chat activity reset the clock"
        );
        // A connection that never selected a character has no clock.
        assert_eq!(
            registry.note_activity(
                Registry::next_key(),
                GameMsg::RequestUserorder as u16,
                30_000
            ),
            None
        );
    }

    /// Required behaviour 2: a player idle *less* 30 minutes is not
    /// force-aggroed — a passive monster in detection range keeps ignoring
    /// them.
    #[test]
    fn a_player_idle_under_thirty_minutes_is_not_force_aggroed() {
        let registry = Registry::new();
        let (key, _rx) = join(&registry, info(11, "Sitter", 0, 0));
        // World entry, then nothing but sitting and the client's automated
        // frames: none of them may reset the clock.
        registry.note_activity(key, GameMsg::RequestSitdown as u16, 0);
        registry.note_activity(key, GameMsg::RequestHeartbeat as u16, 600_000);
        registry.note_activity(key, GameMsg::RequestChangestatusinfo as u16, 1_200_000);
        let idle_ms = registry
            .note_activity(key, GameMsg::RequestHeartbeat as u16, 1_799_000)
            .unwrap();
        assert_eq!(idle_ms, 1_799_000, "nothing reset the clock");

        let idle_secs = idle_ms / 1000;
        assert!(!gz_sim::ai::force_aggro(idle_secs));
        assert!(!gz_sim::ai::acquires_target(false, true, idle_secs));
    }

    /// Required behaviour 3: thirty minutes of *any* posture — standing
    /// (automated traffic only), sitting, or vending — runs the clock to the
    /// threshold, and a *passive* monster in detection range then acquires
    /// the player immediately.
    #[test]
    fn thirty_minutes_of_any_posture_feeds_the_force_aggro_gate() {
        // Each posture's frames, exactly as an idle connection emits them.
        let postures: &[(&str, &[u16])] = &[
            (
                "standing",
                &[
                    GameMsg::RequestHeartbeat as u16,
                    GameMsg::RequestChangestatusinfo as u16,
                ],
            ),
            (
                "sitting",
                &[
                    GameMsg::RequestSitdown as u16,
                    GameMsg::RequestHeartbeat as u16,
                ],
            ),
            (
                "vending",
                &[
                    GameMsg::RequestStartpersonalshop as u16,
                    // a shop owner walked into view: the client auto-fetches
                    // their banner (`CommonLogic-SetChar.cpp:203-209`)
                    GameMsg::RequestPersonalshopbanner as u16,
                    GameMsg::RequestHeartbeat as u16,
                ],
            ),
        ];
        for (posture, frames) in postures {
            let registry = Registry::new();
            let (key, _rx) = join(&registry, info(11, "Afk", 0, 0));
            // World entry starts the clock...
            registry.note_activity(key, frames[0], 0);
            // ...thirty minutes pass with only this posture's traffic...
            for frame in *frames {
                registry.note_activity(key, *frame, 1_800_000);
            }
            let idle_ms = registry
                .note_activity(key, GameMsg::RequestHeartbeat as u16, 1_800_000)
                .unwrap();
            assert_eq!(idle_ms, 1_800_000, "{posture}: the clock ran a full 30 min");
            // ...and the nearby passive monster comes for them anyway.
            assert!(
                gz_sim::ai::acquires_target(false, true, idle_ms / 1000),
                "{posture}: force aggro opens at the threshold"
            );
        }
    }
}
