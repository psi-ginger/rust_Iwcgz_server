//! World: the per-character world-entry sequence and the live-frame generator —
//! 1101 VILLAGESTRUCT, SERVERINFO, the generated world-entry prologue, the
//! per-character state frame set (244/248/249), the nearby-character announce
//! walk, and `LiveFrames`, which regenerates an inbound frame from the database.

use std::time::{SystemTime, UNIX_EPOCH};

use gz_cipher::Cipher;
use gz_data::npc::{NpcPlacement, char_unique_for, placements_on};
use gz_db::{CharacterFull, ItemRepository, PgItemRepository};
use gz_proto::game::{
    MAX_CONSTRUCTION_NUM, MAX_VILLAGE_NUMBER, MakeCharInfo, ServerInfo, VillageStruct,
    client_map_index, encode_all_char_info_one, encode_all_char_info_two, encode_change_horse,
    encode_family_update, encode_get_village_info, encode_login_many_batches, encode_person_horse,
    encode_response_name, encode_skillbook_magic_info,
};
use gz_proto::item::{Item, MAX_ITEM_PER_PERSON, encode_person_item_info};
use gz_proto::loginmany::{LoginManyRecord, NpcRecord, encode_login_many_batches_mixed};
use gz_proto::opcodes_game::GameMsg;
use gz_proto::quest::PersonQuest;
use tokio::net::TcpStream;

use crate::config::GameConfig;
use crate::registry::{Registry, SessionInfo, SessionKey};
use crate::stats::derived_max_life;
use crate::{AnyError, basic_info_for, logln, send_encoded, send_raw, world_entry};
/// Answers `GAMEMSG_REQUEST_VILLAGESTRUCT` (1101). The client blocks player
/// movement (`OrderAction`) and stops popping reserved characters
/// (`Client-SetChar.cpp:66`) until this arrives, so a village map that never
/// gets an answer looks frozen. Zero structure info renders the empty lots.
pub(crate) async fn handle_village_struct(
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

/// The `vitality` a town NPC's life is computed from: the base every character
/// has (`character_stats`' DDL default, and the value that reproduces the
/// captured NPC's `uiLife` of 40 at level 1).
const NPC_BASE_VIT: i32 = 5;

/// Announces `map_id`'s static NPCs to one session, as one or more `LOGINMANY` (6)
/// bodies.
///
/// Opcode 6 is the **only** frame that creates a character on a client
/// (`Client-Login.cpp:53-85`), so an NPC the client is never told about does not
/// exist — which is why Hanyang was empty: the town's NPCs live in the map's
/// `NPCGroup` files and nothing announced them.
///
/// **Deviation:** the legacy announces the NPCs of the cells the newcomer just
/// entered and re-announces on every cell crossing (`ServerMap.cpp:73-150`). We
/// send the map's whole roster once, at world entry. For characters that never
/// move and never leave that is the same client-visible outcome, and it keeps the
/// cell bookkeeping to sessions; a moving NPC (A9, monsters) needs the per-cell
/// fan back.
///
/// # Errors
///
/// A placement whose id cannot be computed is skipped with a log line rather than
/// announced with a wrong id; the caller gets no error from that.
pub(crate) fn announce_map_npcs(registry: &Registry, session_key: SessionKey, map_id: i32) {
    let records: Vec<LoginManyRecord> = placements_on(map_id)
        .filter_map(npc_record_for)
        .map(LoginManyRecord::Npc)
        .collect();
    if records.is_empty() {
        logln!("game: map {map_id} has no static NPCs to announce");
        return;
    }
    let count = records.len();
    let mut frames = 0;
    for body in encode_login_many_batches_mixed(&records) {
        registry.send_to(session_key, GameMsg::ResponseLoginmany as u16, body);
        frames += 1;
    }
    logln!("game: announced {count} NPC(s) on map {map_id} in {frames} frame(s)");
}

/// The wire record for one placement.
///
/// `life` comes from the port's own base-life formula at the placement's level —
/// the same 40 the captured NPC carries at level 1 — because the server will not
/// announce a character at zero life (`char.cpp:9375`).
fn npc_record_for(placement: &'static NpcPlacement) -> Option<NpcRecord> {
    let make_char_unique = char_unique_for(placement).or_else(|| {
        logln!(
            "game: NPC kind {} at ({},{}) has no id — not announced",
            placement.kind,
            placement.x,
            placement.y
        );
        None
    })?;
    Some(NpcRecord {
        make_char_kind: placement.kind,
        make_char_unique,
        level: placement.level,
        life: u32::try_from(derived_max_life(NPC_BASE_VIT, i16::from(placement.level)))
            .unwrap_or(0),
        x: placement.x,
        y: placement.y,
        map_index: client_map_index(placement.map_id),
        total_move_speed: 0,
        home_village_unique: 0,
    })
}

/// The `GAMEMSG_RESPONSE_SERVERINFO` body for this single-world dev server.
///
/// `service_area` must match the client's `ServiceArea.txt`
/// (`SERVICEAREA_ENGLISH` → `0x08`) or `DoMsg_..._SERVERINFO` drops the packet
/// and world entry stalls — the failure the 2026-10-05 login-alert RE traced to
/// a *mask byte* rather than to this value. It comes from configuration so a
/// client built for another region is a config change, not a rebuild.
pub(crate) fn server_info(cfg: &GameConfig) -> ServerInfo {
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

/// Sends the generated world-entry sequence.
///
/// # Errors
///
/// Propagates any cipher or socket failure from [`send_raw`]; a write failure
/// ends the session. [`world_entry::prologue`] itself panics only on a
/// programmer error in the frame it is handed.
pub(crate) async fn send_world_entry(
    stream: &mut TcpStream,
    cipher: &mut Cipher,
    live: &LiveFrames<'_>,
) -> Result<(), AnyError> {
    let Some(character) = live.character.as_ref() else {
        return Err("world entry before the character row is loaded".into());
    };
    let character_id = character.character_id;
    // `clMakeCharInfo.uiMakeCharUnique` is a 16-bit field, so the session id has
    // to fit one; the same narrowing `handle_select_char` does.
    let char_unique = u16::try_from(live.char_unique)
        .map_err(|_| format!("session char_unique {} does not fit u16", live.char_unique))?;
    // `PERSONITEMINFO` is a single-slot notification. The captured one announces
    // slot 8 as empty, so we send whatever the database holds for that slot --
    // which for the reference character is also empty, and for any other is
    // correct rather than a replay of someone else's inventory.
    let slot = world_entry::PERSON_ITEM_INFO_SLOT;
    let stored = live
        .items
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
        friend_list: live.friend_list.clone(),
    });

    let total: usize = sequence.iter().map(|(_, body)| body.len()).sum();
    logln!(
        "game: generating world-entry prologue: {} frames, {total} payload bytes",
        sequence.len()
    );

    for (cmd, body) in sequence {
        send_raw(stream, cipher, cmd, &body).await?;
    }
    // 225 rides the entry burst, as it does in the capture: there it sits between
    // 8464 and 226, i.e. ahead of the 244/248/249 block below (`generated_frames.rs`
    // frame order). The client only learns a quest is cleared from this frame, and
    // its own gate check reads that belief (`MouseOperation.cpp:5500-5511`).
    crate::quest::push_quest_state(stream, cipher, &live.quest).await?;
    send_character_state(stream, cipher, live).await
}

/// The frames that carry the character's own identity and stats, in the order the
/// reference session sent them within its 230 burst: 244 names the avatar, 248 is
/// the `clBI` + `clIP` block, 249 the item array.
///
/// A named constant rather than a literal in the loop, so a test can assert the set
/// — the bug this exists to fix was precisely these three never being sent under
/// the only world-entry path there is, and a test that could have named them is the point.
pub(crate) const LIVE_CHARACTER_STATE: &[u16] = &[
    GameMsg::ResponseName as u16,
    GameMsg::ResponseAllcharinfoone as u16,
    GameMsg::ResponseAllcharinfotwo as u16,
];

/// The `cltMakeCharInfo` for one character, from its database row.
///
/// One function for both callers — the session's own `RESPONSE_LOGIN` and the
/// record another player receives in a 6 — so the two can never disagree about a
/// character's class, life, or cell.
pub(crate) fn make_char_info_for(
    character: &CharacterFull,
    char_unique: i32,
) -> Result<MakeCharInfo, AnyError> {
    Ok(MakeCharInfo {
        make_char_unique: u16::try_from(char_unique).map_err(|_| {
            format!("session char_unique {char_unique} does not fit uiMakeCharUnique's u16")
        })?,
        make_char_kind: u16::try_from(character.class_kind.max(0)).unwrap_or(0),
        level: u8::try_from(character.level.clamp(0, 255)).unwrap_or(1),
        life: u32::try_from(character.max_life.max(0)).unwrap_or(0),
        x: u16::try_from(character.pos_x.max(0)).unwrap_or(0) & 0x3FF,
        y: u16::try_from(character.pos_y.max(0)).unwrap_or(0) & 0x3FF,
        map_index: client_map_index(character.map_id),
        total_move_speed: u8::try_from(character.move_speed.clamp(0, 63)).unwrap_or(0),
        home_village_unique: u8::try_from(character.school_id.clamp(0, 255)).unwrap_or(0),
        game_master_level: 0,
        map_unique: 0,
        make_ani_type: 0,
    })
}

/// Makes a newly-entered character exist for everyone nearby, and tells it about
/// everyone already there.
///
/// This is `cltServerMap::SendNewOthersInfoToPerson` (`ServerMap.cpp:73-150`), and
/// it is the frame the whole observer story rests on. The legacy runs it in **both**
/// directions when a character enters a cell:
///
/// * the newcomer receives one `GAMEMSG_RESPONSE_LOGINMANY` (6) listing everyone
///   already in its block;
/// * every already-present character in that block receives a 6 listing the
///   newcomer.
///
/// The client turns each record into a real avatar: `DoMsg_GAMEMSG_RESPONSE_LOGINMANY`
/// walks the body and `PushChar`es each `cltMakeCharInfo` (`Client-Login.cpp:53-85`),
/// and `SetCharReservedChar` pops that queue into `SetChar`
/// (`Client-SetChar.cpp:112-138`) — the only creator of an avatar object and the
/// only filler of `cltCharCommon::CharUniqueInfo`.
///
/// **This was the missing piece behind "nearby avatars are invisible".** Without a
/// 6 the observer has no `CR[id]` for the other player, so every per-character
/// frame about them — including the 200 fan — is discarded on arrival at
/// `Client-Person-BigInfo.cpp:1166-1169` (`if(charid <= 0) return;`). The fan was
/// arriving correctly and being thrown away, which is why a socket-level probe
/// passed while the screen showed nothing.
///
/// The server's own record is never included in its own 6: the legacy never lists
/// the receiver among "others", and a client re-`SetChar`ing itself would reset the
/// character it is already standing on.
///
/// # `was` — and why a repeat announcement warps the avatar
///
/// `was` is the mover's registry entry *before* the step that crossed a cell
/// boundary; `None` at world entry, where there is no before and the whole block
/// is new.
///
/// It is load-bearing rather than a nicety. The 6 is **not** idempotent on the
/// client: `SetCharReservedChar` (`Client-SetChar.cpp:111-119`) finds a record
/// for a `char_unique` it already holds and **deletes that avatar**
/// (`DeleteInCell` + `DeleteChar`) before re-`SetChar`ing it from the record. So
/// announcing the mover to somebody already watching does not refresh them — it
/// teleports their copy to whatever cell the record names and discards the walk
/// order the 241 installed. The observer sees a few steps and then a **warp
/// back**, and after that a one-tile-per-200 crawl with a pause between tiles.
///
/// With `was` present the recipients are [`Registry::newly_near`] — the legacy's
/// `m_pNewCell` — instead of the whole current block, so a neighbour that could
/// already see the mover gets nothing, exactly as
/// `SendNewOthersInfoToPerson` (`ServerMap.cpp:73-150`) is driven.
///
/// # Errors
///
/// A `char_unique` that does not fit `uiMakeCharUnique`'s 16 bits, or a tile that
/// does not fit the 10-bit `uiX`/`uiY`. Both are bounded by the allocation and the
/// map data respectively, so in practice neither fires.
pub(crate) fn announce_nearby_characters(
    registry: &Registry,
    session_key: SessionKey,
    self_row: &CharacterFull,
    self_char_unique: i32,
    was: Option<&SessionInfo>,
) -> Result<(), AnyError> {
    let origin = registry
        .info(session_key)
        .ok_or("nearby announcement before the session is in the registry")?;

    // Who has to be told. At world entry (`was == None`) every cell of the
    // block is new, so it is the whole block. On a mid-walk cell crossing it is
    // only the cells just entered — the legacy's `m_pNewCell` — because the 6
    // is *not* idempotent on the client: `SetCharReservedChar`
    // (`Client-SetChar.cpp:111-119`) finds a `char_unique` it already holds and
    // **deletes that avatar** before re-`SetChar`ing it from the record. Aimed at
    // somebody already watching, that teleports the avatar to the record's cell
    // and throws away the walk order the 241 installed — "walks, then warps
    // back", then a one-tile-per-200 crawl for the rest of the walk. Both
    // directions are the same set: adjacency is symmetric, so "the cells I
    // entered" and "the sessions I just became adjacent to" are the same pairs.
    let new: Vec<(SessionKey, SessionInfo)> = match was {
        Some(was) => registry.newly_near(was, &origin, session_key),
        None => registry.near_others(&origin, session_key),
    };
    // Nothing new: either a crossing where every neighbour was already adjacent
    // (the normal case) or the legacy's own `if(index)` guard
    // (`ServerMap.cpp:140`) for an empty near block, which must yield no body at
    // all rather than a zero-length one — the client reads `uiType` before it
    // knows it got any bytes (`Client-Login.cpp:61-63`).
    if new.is_empty() {
        return Ok(());
    }
    let self_record = make_char_info_for(self_row, self_char_unique)?;

    // To the newcomer: everyone in the cells it just entered.
    let existing = &new;
    let to_self: Vec<MakeCharInfo> = existing
        .iter()
        .filter_map(|(_, info)| make_char_info_from_session(info).ok())
        .collect();
    // An empty near block yields no body at all, which is the legacy's own
    // `if(index)` guard (`ServerMap.cpp:140`) and the reason this frame used to be
    // withheld wholesale.
    for body in encode_login_many_batches(&to_self) {
        registry.send_to(session_key, GameMsg::ResponseLoginmany as u16, body);
    }
    let existing_count = existing.len();

    // To each of them: the newcomer. One frame each, as the legacy does — the
    // record is identical, only the recipient differs.
    // One record always fits in one frame, so `encode_login_many_batches` yields
    // exactly one body here; the loop stays so a future wider record keeps working.
    for body in encode_login_many_batches(std::slice::from_ref(&self_record)) {
        for (key, _) in &new {
            registry.send_to(*key, GameMsg::ResponseLoginmany as u16, body.clone());
        }
    }

    logln!(
        "game: char {self_char_unique} announced to {} existing character(s); \
         learned about {} in its near block",
        existing_count,
        to_self.len()
    );
    Ok(())
}

/// The `cltMakeCharInfo` a client needs to draw one avatar, from a registry entry.
///
/// Field-for-field the same source as the session's own login frame
/// ([`make_char_info_for`]), because the two must agree: a client that draws its
/// own avatar from one record and another player's from a slightly different one
/// ends up with two characters that disagree about their own class or life.
pub(crate) fn make_char_info_from_session(info: &SessionInfo) -> Result<MakeCharInfo, AnyError> {
    Ok(MakeCharInfo {
        make_char_unique: u16::try_from(info.char_unique).map_err(|_| {
            format!(
                "session char_unique {} does not fit uiMakeCharUnique's u16",
                info.char_unique
            )
        })?,
        make_char_kind: info.class_kind,
        level: u8::try_from(info.level.clamp(0, 255)).unwrap_or(1),
        life: info.life,
        x: u16::try_from(info.x).unwrap_or(0) & 0x3FF,
        y: u16::try_from(info.y).unwrap_or(0) & 0x3FF,
        map_index: client_map_index(info.map_id),
        total_move_speed: info.move_speed.min(63),
        home_village_unique: u8::try_from(info.school_id.clamp(0, 255)).unwrap_or(0),
        game_master_level: info.gm_level,
        map_unique: 0,
        make_ani_type: 0,
    })
}

/// The per-character state block, generated from the database.
///
/// ## Why this is separate from [`world_entry::prologue`]
///
/// The 45 generated frames are all *context* — country war, the 35 guild records,
/// the transform and item-type tables, the friend list, server time. **None of
/// them describes the character.** Its identity and stats live in
/// `RESPONSE_ALLCHARINFOONE` (248, the `clBI` + `clIP` block) and
/// `RESPONSE_ALLCHARINFOTWO` (249, the item array), which in the reference
/// session rode the 25-frame burst answering the client's first
/// `REQUEST_CHANGESTATUSINFO` (230).
///
/// That burst was reachable only under the retired replay mode: both the push at
/// char-select and the 230 arm were gated on it, and `world_entry::prologue`
/// emitted neither frame. So the generated path sent no 248/249 at all, and the
/// client faulted on a null per-character record — the character stood on the map
/// with no name, black HP/MP bars, and could not move. Reported live 2026-10-09;
/// the four symptoms are all `clBI`/`clIP` fields (`szCharName`, `siLife`,
/// `siMaxLife`, `siMana`, `siMaxMana`).
///
/// ## Why it goes out with the entry prologue, not on the 230
///
/// The capture's own timing was the measured cause of the ~20 s re-entry stall
/// (2026-10-08): its first 230 rides a process-global 30 s timer anchored at
/// *first* world entry, so a burst deferred to that 230 would arrive up to 30 s
/// late — after the client had built a world without the character in it.
/// Sending it here is that fix, applied to the generated frames.
///
/// ## Why 248 goes out here at all
///
/// `SetOne` copies `clBI` wholesale, `clPos` included (`Client-Person.cpp:776`,
/// `PersonInfo.h:589`), and `cltCharCommon::GetX()` reads `clPos.siX` back out of
/// it (`Char-Common.cpp:1206`, `Common.cpp:2917`). That is why 248 must never be
/// *re-sent* after a mutation — it would teleport the player to the database's
/// cell. It does not apply to first entry, where the character has no `clBI` at
/// all without it, and where the database cell *is* current because
/// `handle_userorder` persists every `ORDER_MOVE` destination before the stepped
/// cell diverges from it. This is that first delivery, and it is unconditional.
/// The re-send case is handled where it arises: 230 is the horse auto-food
/// message and is never answered with 248.
///
/// # Errors
///
/// A database failure reading the item array, or a socket/cipher failure. A frame
/// with no generated body is logged and skipped rather than aborting the session,
/// matching [`replay`]'s tolerance for a frame it cannot rebuild.
pub(crate) async fn send_character_state(
    stream: &mut TcpStream,
    cipher: &mut Cipher,
    live: &LiveFrames<'_>,
) -> Result<(), AnyError> {
    let mut sent = 0usize;
    let mut bytes = 0usize;
    for cmd in LIVE_CHARACTER_STATE {
        let Some(body) = live.regenerate(*cmd).await? else {
            logln!("game: withholding live character-state frame {cmd}: no generated body");
            continue;
        };
        bytes += body.len();
        send_raw(stream, cipher, *cmd, &body).await?;
        sent += 1;
    }
    logln!("game: generated character state: {sent} frames, {bytes} payload bytes (244/248/249)");
    Ok(())
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

/// The captured frames this server owns rather than replays.
///
/// Every frame in the burst still comes from `login_capture.pcapng`; the point
/// is only that the parts we have modelled are read back out of PostgreSQL on
/// the way out, so a change the player makes in the world survives the next
/// login. Un-modelled frames stay byte-exact, which is what keeps the client
/// accepting the burst.
pub(crate) struct LiveFrames<'a> {
    pub(crate) items: &'a PgItemRepository,
    /// Needed for 248's `clBI.siIDNum`, which is the owning account rather than
    /// the character.
    pub(crate) account_id: i32,
    /// `clBI.szAccountID`. Captured once at login, where the account is already
    /// authenticated, so the 230 burst needs no further lookup.
    pub(crate) account_name: String,
    /// The selected character, held from `REQUEST_SELECTCHAR` on. 248 needs its
    /// slot, name, position and stats, and re-reading them per resync would mean
    /// a `characters` join on every status request.
    pub(crate) character: Option<CharacterFull>,
    /// This session's `char_unique`, as handed to the client in
    /// `RESPONSE_CLIENTCHARUNIQUE`.
    ///
    /// 244 and 258 both resolve their target by indexing the client's
    /// `CharUniqueInfo` table with a character id carried *in the payload*
    /// (`Client-SystemNPC.cpp:977`, `Client-Person.cpp:1065`) rather than using
    /// the frame's own `id`. Replaying the capture therefore applied the
    /// reference character to our session; generating them means stamping the
    /// session's id, not `characters.game_unique`.
    pub(crate) char_unique: i32,
    /// The 6219 friend-all-list body for this session, built at world entry
    /// from the hydrated `character_friends` rows plus who is online right now.
    pub(crate) friend_list: Vec<u8>,
    /// The character's quest block, hydrated from `character_quests` at
    /// char-select and mutated by the 5700/5701/5702 handlers. It is the body of
    /// 225 both in the entry burst and after every change.
    pub(crate) quest: PersonQuest,
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
            return Ok(Some(self.quest.encode()));
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
            // 177 rides the preamble twice (once in the prologue tail, once in the
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
        let basic = basic_info_for(self.account_id, &self.account_name, character)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Outbound;

    /// A session in the registry on Hanyang, with the outbound side a test can
    /// read — the same seam `tests.rs` uses for the announce walk.
    fn joined(
        registry: &Registry,
        char_unique: i16,
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
                name: "Tester".to_owned(),
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

    /// The static NPCs are the only reason the town is not empty: opcode 6 is what
    /// creates a character on a client (`Client-Login.cpp:53-85`), and the roster
    /// comes from the generated `NPCGroup` table. Sarah (kind 40,
    /// `KIND_WAITINGMAN`) is the one that matters — she hands out the chain that
    /// lifts the exit guard (T-019).
    #[test]
    fn announcing_the_map_npcs_puts_sarah_on_the_wire() {
        let registry = Registry::new();
        let (key, mut rx) = joined(&registry, 1473, 27, 87);

        announce_map_npcs(&registry, key, 158);

        let frame = rx.try_recv().expect("the map's NPCs are announced");
        assert_eq!(frame.cmd, GameMsg::ResponseLoginmany as u16);
        let stride = gz_proto::loginmany::NPC_RECORD_LEN;
        assert_eq!(
            frame.payload.len(),
            gz_data::npc::NPC_PLACEMENTS.len() * stride,
            "every Hanyang placement in one frame, at the NPC stride"
        );

        // Walk it the way the client does: each record's own uiType picks the
        // stride, so a player record in here would shift every following one.
        let mut records = Vec::new();
        let mut at = 0usize;
        while at < frame.payload.len() {
            let word = u32::from_le_bytes([
                frame.payload[at],
                frame.payload[at + 1],
                frame.payload[at + 2],
                frame.payload[at + 3],
            ]);
            assert_eq!(
                word & 0x1,
                gz_proto::loginmany::MAKECHARTYPE_NPC,
                "an NPC body must carry NPC records only"
            );
            records.push(NpcRecord::decode(&frame.payload[at..at + stride]).expect("decodes"));
            at += stride;
        }
        assert_eq!(records.len(), gz_data::npc::NPC_PLACEMENTS.len());

        let sarah = records
            .iter()
            .find(|r| r.make_char_kind == 40)
            .expect("KIND_WAITINGMAN is announced");
        assert_eq!((sarah.x, sarah.y), (16, 82));
        assert_eq!(
            sarah.map_index,
            client_map_index(158),
            "the client's map index, not the server's map id"
        );
        assert_eq!(sarah.life, 40, "the captured level-1 NPC's life");
        assert!(
            (gz_data::npc::NPC_CHAR_UNIQUE_BASE..19999).contains(&sarah.make_char_unique),
            "an NPC id has to satisfy IsNPC"
        );
    }

    /// A map with no placements must send nothing at all: a zero-length 6 body is
    /// walked as garbage by the client (`Client-Login.cpp:61-63`).
    #[test]
    fn a_map_without_npcs_announces_nothing() {
        let registry = Registry::new();
        let (key, mut rx) = joined(&registry, 1473, 27, 87);

        announce_map_npcs(&registry, key, 1);

        assert!(rx.try_recv().is_err(), "no frame for a map with no NPCs");
    }
}
