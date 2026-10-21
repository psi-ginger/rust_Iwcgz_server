//! Movement: the stepped move family. `Movement` and its accumulator mirror
//! `cltCharCommon::Move` (`char-move.cpp:38-227`); `char_update_body` /
//! `send_char_update` / `fan_char_update` build and spread the 200
//! `CHARUPDATE`; and `handle_userorder` / `handle_leave_request` answer the 240
//! `REQUEST_USERORDER` and the 107/109 leave requests. The read loop feeds this
//! module's `step_movement` from its timer arm; the request handlers are called
//! from the dispatcher in `main.rs`. The gate and village tables those order
//! checks consult — and the revive constants beside them — live in
//! [`crate::geography`].

use std::time::{Duration, Instant};

use gz_cipher::Cipher;
use gz_db::{CharacterFull, CharacterRepository, ItemRepository, PgCharacterRepository};
use gz_proto::game::encode_logout;
use gz_proto::map_geometry::cell_of;
use gz_proto::opcodes_game::GameMsg;
use gz_proto::order::{BasicCharUpdate, ORDER_MOVE, ORDER_SITDOWN, OrderData, SitDown};
use tokio::net::TcpStream;

use crate::geography::{
    GATES, Gate, gate_at, gate_within, village_at, village_exit, village_warp_gate, village_within,
    warp_gate_to,
};
use crate::ids::PlayerCharUnique;
use crate::registry::{Registry, SessionInfo, SessionKey};
use crate::{
    AnyError, LiveFrames, announce_map_npcs, announce_nearby_characters, logln, make_char_info_for,
    send_encoded,
};

/// `INIT_LEVEL` — the level below which a revive always returns to the palace
/// (`CommonHeader.h:672`, `#define INIT_LEVEL 10`).
pub(crate) const INIT_LEVEL: i16 = 10;

/// `MAPINDEX_PALACE` at the tile the login spawn uses — where `Revive()` sends every
/// branch that has no better answer (`char.cpp:4923`, `:5000`, `:5009`, `:5019`,
/// `:5045`, `:5061`, all `cltWarpInfo(1, MAPINDEX_PALACE, false, false, 27, 87, …)`).
pub(crate) const REVIVE_PALACE: (i32, i32, i32) = (158, 27, 87);

/// The maps this port treats as towns, so a revive inside one stays there rather than
/// being sent to the map that gate-enters it.
fn is_town(map_id: i32) -> bool {
    map_id == 0 || map_id == REVIVE_PALACE.0
}

/// Where a revive sends a character, in `cltCharServer::Revive()`'s own order
/// (`char.cpp:4887-5100`).
///
/// Kept:
///
/// 1. **`level < INIT_LEVEL` → palace (158) at (27,87)** — `:5027-5046`, and it runs
///    *before* the village branch, so a beginner always lands in Hanyang even when a
///    nearer village resolved (`:5069-5085`).
/// 2. **A dungeon returns to the map it is entered from** — the hunt-map rule at
///    `:4914-4917` sets the parent map as the destination. This port has no map table,
///    so the parent is read off the gate table: the row that enters the map names it
///    (for the Raccoon Lair, row 561's source map 0 at (224,314) — its mouth).
/// 3. The palace fallback (`:5007-5013`).
///
/// Not modelled, and why: the instance-map branches (`:4955-5015`) need instance maps;
/// `KILLEDMODE_GOTOSAFEZONE` (`:5017-5025`) needs death modes; the PK-map switch
/// (`:5057-5068`) is off in this port's SERVERINFO; and the nearest-village branch
/// (`:5069-5085`, which warps to the village map's **centre** from the map type) needs
/// the village table (`Data\villageinfoE.txt`) and the map-type table, neither of which
/// is loaded here.
#[must_use]
pub(crate) fn revive_target(level: i16, map_id: i32) -> (i32, i32, i32) {
    if level < INIT_LEVEL {
        return REVIVE_PALACE;
    }
    if !is_town(map_id)
        && let Some(entrance) = GATES.iter().find(|g| g.dst_map == map_id)
    {
        return (entrance.src_map, entrance.src_x, entrance.src_y);
    }
    REVIVE_PALACE
}

/// `GAMEMSG_REQUEST_REVIVE` (7225) — the client's revive prompt.
///
/// The body is `cltGameMsgRequest_Revive` (`MsgType-Person.h:1448-1460`): three
/// `SI16`s — `siCharUnique`, `siParam`, `siDieMode`. The client fills it from its death
/// dialog (`NCommonMsgDlg.cpp:172-178`, `(charid, 1, NORMAL)`), and the handler is a
/// thin shell around `Revive()`: `Server-Person.cpp:3330-3345` validates the id, applies
/// a one-second throttle, and calls it — there is **no HP check**, so the packet is
/// honoured whatever state the character is in.
///
/// The warp itself reuses the gate path with a synthesised one-cell gate: a revive and a
/// gate differ only in how the destination is chosen, and both end in the same
/// `SetWarpInfo`/`Warp` pair the client is told about
/// (`char.cpp:5080-5084`). The gate path's own entry-item check sees 0/0 and stays out
/// of the way.
///
/// # Errors
///
/// A short body is logged and ignored, like the rest of this family; repository and
/// socket failures propagate.
pub(crate) async fn handle_revive(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    characters: &PgCharacterRepository,
    registry: &Registry,
    session_key: SessionKey,
    live: &mut LiveFrames<'_>,
    payload: &[u8],
) -> Result<(), AnyError> {
    const REVIVE_REQUEST_SIZE: usize = 6;
    if payload.len() < REVIVE_REQUEST_SIZE {
        logln!("game: malformed 7225 ({} bytes) — ignored", payload.len());
        return Ok(());
    }
    let named = i16::from_le_bytes([payload[0], payload[1]]);
    let die_mode = i16::from_le_bytes([payload[4], payload[5]]);
    // `:3341` — a request that does not name this session's character is dropped.
    if i32::from(named) != live.char_unique {
        logln!(
            "game: revive for char {named} ignored: this session is {}",
            live.char_unique
        );
        return Ok(());
    }
    let Some(character) = live.character.as_ref() else {
        logln!("game: revive before the character row is loaded — ignored");
        return Ok(());
    };
    let (from_map, level, max_life) = (character.map_id, character.level, character.max_life);
    let (dst_map, x, y) = revive_target(level, from_map);
    logln!(
        "game: revive: char {} (level {level}, mode {die_mode}) on map {from_map} -> map {dst_map} at ({x}, {y})",
        character.character_id
    );
    // `char.cpp:4949` and `:5113` — both of `Revive()`'s branches set life back
    // to `GetMaxLife()` before they warp, so a revived character is at full
    // strength on arrival rather than standing dead at the palace. Life lives
    // only in the session here, and the opcode-5 record the warp is about to
    // send already carries `max_life` as its life field
    // (`make_char_info_for`), so the client and the server agree without a
    // second frame.
    let life = u32::try_from(max_life.max(0)).unwrap_or(0);
    if registry.restore_life(session_key, life).is_none() {
        logln!("game: revive: session is gone; life not restored");
    }
    let gate = Gate {
        src_map: from_map,
        src_x: 0,
        src_y: 0,
        xsize: 1,
        ysize: 1,
        center_x: 0,
        center_y: 0,
        item_unique: 0,
        item_num: 0,
        dst_map,
        dst_x: x,
        dst_y: y,
    };
    warp_through_gate(
        stream,
        outbound,
        characters,
        registry,
        session_key,
        live,
        &gate,
    )
    .await
}

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

/// The direction bits the 7231 sit request carries (`CommonHeader.h:388-394`).
const NORTH: i32 = 16;
/// See [`NORTH`].
const EAST: i32 = 64;
/// See [`NORTH`].
const SOUTH: i32 = 1;
/// See [`NORTH`].
const WEST: i32 = 4;

/// The 8-way facings `DoMsg_GAMEMSG_REQUEST_SITDOWN` accepts
/// (`Server-Person.cpp:3299-3319`) — the `SOUTH|EAST`..`EAST` cases. Any other
/// value (0 included) is dropped rather than guessed at.
const SIT_DIRECTIONS: [i32; 8] = [
    SOUTH | EAST,
    SOUTH,
    SOUTH | WEST,
    WEST,
    WEST | NORTH,
    NORTH,
    NORTH | EAST,
    EAST,
];

/// The exact `Instant` when the next movement step will be ready.
///
/// `now` is the caller's clock, so the deadline stays a pure function of the
/// accumulator and the production path is testable; `main.rs` passes
/// `Instant::now()`.
///
/// # Why the result sits inside [`MOVE_DELAY_CAP_MS`]
///
/// [`Movement::advance`] credits **at most 350 ms** of elapsed time per call —
/// the legacy's `min((CHAR_ACTION_INTERVAL_FRAME + 1) * 50, ...)` per frame
/// (`char-move.cpp:145`) — so a wake further out than the cap *discards* the
/// excess instead of banking it, and the step lands late.
///
/// Measured 2026-10-10 with an uncapped deadline: at speed 10 the deadline
/// armed for the full 500 ms, `advance` credited only 350 ms (70 points, not
/// the 100 needed), so the loop re-armed for the missing 150 ms and the tile
/// committed at **650 ms** instead of 500 ms — a 30 % slowdown, visible in
/// `target/m1-logs/game.log` as `fanned 200` every ~650 ms. The observing
/// client animates one single-tile order per 200 and that cycle is exactly
/// `5000 / movespeed` ms, so any slower cadence leaves the avatar idle for
/// the difference: **one step, a pause, repeat.**
///
/// Capping keeps every gap between two [`Movement::advance`] calls inside the
/// credit window, so nothing is lost: the wake at the cap is a no-op and the
/// follow-up wake lands on the exact legacy cadence of `5000 / movespeed` ms
/// per tile.
pub(crate) fn next_move_deadline(movement: &Movement, now: Instant) -> Instant {
    now + movement.next_delay()
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
/// and then each stepped cell (the captured run was 241, 200, 200).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Movement {
    /// Destination from the 240's `siPara1` (`x | (y << 16)`).
    pub(crate) dest: (i32, i32),
    /// The authoritative tile, advanced one step at a time.
    pub(crate) pos: (i32, i32),
    /// Map the move is on, for the drop cell.
    pub(crate) map_id: i32,
    /// The mover's effective `GetTotalMoveSpeed()` for the whole walk. Captured
    /// once when the order is answered, so gear changes mid-walk do not rescale
    /// an in-flight step.
    pub(crate) move_speed: i32,
    /// `clMoveInfo.siMovePercent`.
    pub(crate) percent: i32,
    /// When [`Movement::advance`] last ran, i.e. `clMoveInfo.dwLastMoveClock`.
    pub(crate) last: Instant,
    /// Absolute deadline for the next step. `None` when the move has arrived.
    /// Stored here so `step_movement` can update it after each step.
    pub(crate) deadline: Option<Instant>,
}

impl Movement {
    pub(crate) fn new(dest: (i32, i32), pos: (i32, i32), map_id: i32, move_speed: i32) -> Self {
        Self {
            dest,
            pos,
            map_id,
            move_speed,
            // `MoveInit()` and `SetMove()` both start a fresh order's
            // accumulator at 0 (`Char-Common.cpp:4898`, `char.cpp:4235`), and
            // `char-move.cpp:47` reads `siMovePercent == 0` as "no walk is
            // running yet". So the **first** tile of an order costs half a tile
            // of points — 250 ms at speed 10 — and every tile after it costs a
            // full one, which is the phase the client animates its own avatar
            // on. Starting a half tile behind instead put the server's
            // authoritative tile a permanent `5000 / speed / 2` ms behind the
            // avatar it is supposed to track: every 200 arrived describing a
            // step the client had already shown, and the walk read as sluggish.
            percent: 0,
            last: Instant::now(),
            deadline: None,
        }
    }

    /// Advances the accumulator and commits at most one tile
    /// (`char-move.cpp:145-171`). The step is limited by
    /// [`MOVE_DELAY_CAP_MS`], so a stalled tick banks no more than one tile.
    /// Returns the new cell when a tile was committed.
    pub(crate) fn advance(&mut self, now: Instant) -> Option<(i32, i32)> {
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
    pub(crate) fn arrived(&self) -> bool {
        self.pos == self.dest
    }

    /// How long until the accumulator should next reach [`MOVE_PERCENT_STEP`]:
    /// `ceil(needed * 50 / movespeed)` ms, **capped at**
    /// [`MOVE_DELAY_CAP_MS`] exactly as one legacy tick is capped
    /// (`char-move.cpp:145`).
    ///
    /// The cap is load-bearing rather than cosmetic — see
    /// [`next_move_deadline`]: `advance` credits at most that many
    /// milliseconds per call, so the deadline must never be further out or
    /// the difference is silently dropped and every tile lands late.
    pub(crate) fn next_delay(&self) -> Duration {
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
/// Sends one 200 `CHARUPDATE` for the given cell.
///
/// `urgent=true` for teleports/long jumps (>3 tiles); `false` for normal steps.
pub(crate) async fn send_char_update(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    live: &LiveFrames<'_>,
    x: i32,
    y: i32,
    urgent: bool,
) -> Result<(), AnyError> {
    let character = live
        .character
        .as_ref()
        .ok_or("char update before the character row is loaded")?;
    let body = char_update_body(live.char_unique, character, x, y, urgent)?;
    send_encoded(stream, outbound, GameMsg::ResponseCharupdate, &body).await
}

/// Builds the 200 `CHARUPDATE` body for one cell, as
/// `cltServer::SendCharUpdate` builds `cltBasicCharUpdateInfo`
/// (`Server.cpp:1654-1698`).
///
/// Factored out of [`send_char_update`] so the observer fan ([`fan_char_update`])
/// can queue byte-identical bodies without a second encoder that could drift, and
/// taken as a row rather than a `LiveFrames` so it is testable without a database.
///
/// # Errors
///
/// A `char_unique` or tile that does not fit the struct's narrower fields.
/// Builds the 200 `CHARUPDATE` body for one cell.
///
/// `urgent` controls `bUrgentMode`:
/// - `true`  -> teleports / long jumps (>3 tiles) where the client must snap and animate via `MoveTo`+`MoveInit`.
/// - `false` -> normal single-tile steps. The 241 ORDER_MOVE already told the client the destination;
///   200 packets are just position confirmations. Sending `b_urgent_mode=true` every tile would
///   call `MoveTo`+`MoveInit` repeatedly, resetting the animation and causing step-and-pause.
pub(crate) fn char_update_body(
    char_unique: i32,
    character: &CharacterFull,
    x: i32,
    y: i32,
    urgent: bool,
) -> Result<Vec<u8>, AnyError> {
    let update = BasicCharUpdate {
        si_char_unique: i16::try_from(char_unique).map_err(|_| {
            format!("session char_unique {char_unique} does not fit the SI16 siCharUnique")
        })?,
        // The character's effective `GetTotalMoveSpeed()`; the client uses it for
        // the avatar's step animation rate.
        si_move_speed: i8::try_from(character.move_speed.clamp(0, i16::from(i8::MAX))).unwrap_or(0),
        // `bUrgentMode`:
        // - `true`  -> teleport / long jump (>3 tiles): client calls `MoveTo` + `MoveInit`.
        // - `false` -> normal step: 241 ORDER_MOVE already gave destination; 200 is just confirmation.
        b_urgent_mode: urgent,
        si_life: character.max_life,
        si_reserved_life: 0,
        si_mana: character.max_mana,
        si_x: i16::try_from(x).map_err(|_| format!("x {x} does not fit SI16"))?,
        si_y: i16::try_from(y).map_err(|_| format!("y {y} does not fit SI16"))?,
        ui_gm_mode: 0,
        b_pc_room: false,
        si_make_ani_type: 0,
    };
    Ok(update.encode().to_vec())
}

/// Queues the 200 `CHARUPDATE` for every character in the mover's 3×3 near cell,
/// self included — T-018 slice 6 B1.
///
/// The legacy fans exactly this frame: `SendCharUpdate` builds one
/// `cltBasicCharUpdateInfo` and hands it to
/// `pclchar->SendNeighbourMsg(&clMsg, true)` (`Server.cpp:1689`), which lands on
/// `pCell->SendPacketNearAll` over the `m_pNearCell[9]` block — the 3×3 cells
/// around the sender's, self included (`Char-Neighbour.cpp:29-78`,
/// `ITZMap.cpp:71-87`). The `true` is the self-inclusion flag.
///
/// # What this fan is for: position and walk animation
///
/// Not HP. The client never draws a bystander's health bar — the floating gauge
/// is `cltNPersonStatus::DrawCharStatus`, which hardcodes `pclCM->CR[1]`
/// (`NCharPerson.cpp:3920`) and `CR[1]` is the local player
/// (`Client-Winsock.cpp:130`, and the client's own
/// `charunique == MyCharUnique → charid = 1` at
/// `Client-Person-BigInfo.cpp:1160-1163`). Life and mana do ride along in the
/// struct and are written to the remote object
/// (`SetLife`/`SetMana`, `:1228-1238`); they just stay unrendered for a bystander.
///
/// The fields an observer actually acts on are `siX`/`siY`/`siMoveSpeed`, in the
/// branch the client runs *only* for other characters
/// (`Client-Person-BigInfo.cpp:1187-1222`):
///
/// ```text
/// SI32 length = max( TABS(x - CR[charid]->GetX()), TABS(y - CR[charid]->GetY()));
/// if(length && bselfswitch == false) { ... OrderMove(charid, x, y, BY_COMPUTER); ... }
/// CR[charid]->MoveTo(x, y);
/// CR[charid]->MoveInit();
/// ```
///
/// `bselfswitch == false` is why the mover's own client animates itself off the
/// 241 and needs nothing from a 200 (slice 3), and its absence on the position
/// branch is why observers need this fan. Without it a second client's copy of
/// another avatar is stranded on whatever tile it was created at.
///
/// A send to a session that just disconnected is dropped, not an error — the same
/// tolerance `Registry::send_to` documents for the chat paths.
///
/// Self is **excluded** ([`Registry::broadcast_near_others`]) because every caller
/// also writes the frame to its own socket directly, and one writer per connection
/// keeps a queued copy from overtaking a later direct one.
///
/// `urgent=true` for teleports/long jumps (>3 tiles); `false` for normal steps.
///
/// # Errors
///
/// No character row yet, or a tile/`char_unique` that does not fit the struct.
pub(crate) fn fan_char_update(
    registry: &Registry,
    session_key: SessionKey,
    char_unique: i32,
    character: &CharacterFull,
    x: i32,
    y: i32,
    urgent: bool,
) -> Result<usize, AnyError> {
    let body = char_update_body(char_unique, character, x, y, urgent)?;
    let origin = registry
        .info(session_key)
        .ok_or("char update fan before the session is in the registry")?;
    Ok(registry.broadcast_near_others(
        &origin,
        session_key,
        GameMsg::ResponseCharupdate as u16,
        body,
    ))
}

/// Sends `GAMEMSG_RESPONSE_LOGOUT` (11) to every character that could see the
/// mover at `was` and cannot see it at `now` — the counterpart of the 200 fan
/// and of [`announce_nearby_characters`]. T-018 slice 6 B3a.
///
/// Both directions of a cell crossing have to exist, and they are the same event
/// seen from two ends: someone arriving is an *appearance* (a 6 naming them) and
/// someone leaving is a *disappearance* (an 11 removing them). Shipping only the
/// first half is what left avatars frozen on screen when their owner walked off
/// — the client kept the object it had created and simply stopped being told
/// where it was, until its own screen culling deleted it.
///
/// `was` is the mover's registry entry **before** the tile step and `now` the one
/// after, which is the whole input: the recipient set is `near(was) - near(now)`
/// ([`Registry::no_longer_near`]). The legacy carries that difference in
/// `m_pRemoveCell` (`ITZMap.cpp:73-150`) and hands it to `SendPacket`
/// (`ServerMap.cpp:58-61`); recomputing it is the same answer, because the
/// neighbourhood is a pure function of the tile.
///
/// Returns how many connections took the frame.
pub(crate) fn logout_departed(
    registry: &Registry,
    was: &SessionInfo,
    now: &SessionInfo,
    mover_key: SessionKey,
    char_unique: i32,
) -> usize {
    let payload = encode_logout(char_unique);
    let mut sent = 0;
    for (key, _) in registry.no_longer_near(was, now, mover_key) {
        if registry.send_to(key, GameMsg::ResponseLogout as u16, payload.clone()) {
            sent += 1;
        }
    }
    sent
}

/// Advances an in-flight move by one tick. When the tick commits a tile it sends
/// the 200 `CHARUPDATE` for the new cell, updates the session's drop cell and
/// the registry's copy of the position (so the near fan sees where the avatar
/// actually stands), and ends the move on arrival. A committed step never warps:
/// the legacy arms a warp only from an order's destination (`char-order.cpp:173`), so
/// stepping onto an entrance block or a gate cell keeps walking. A no-op when no
/// move is active or the tile is not due yet.
///
/// # Errors
///
/// A socket or cipher failure while sending the update.
pub(crate) async fn step_movement(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    live: &mut LiveFrames<'_>,
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
    // A movement belongs to one map. A warp clears it (the gate path returns no
    // movement), so a movement whose map no longer matches the row's is stale —
    // stepping it would write a tile on the map the character left.
    if live
        .character
        .as_ref()
        .is_some_and(|c| c.map_id != mv.map_id)
    {
        mv.deadline = None;
        *movement = None;
        return Ok(());
    }
    // A committed step never leaves the map.
    //
    // The legacy arms a warp in exactly one place: the order handler, over the
    // order's *destination* (`char-order.cpp:173` is `CharServerMoveByGate`'s only
    // caller). Warping on the committed tile as well was this port's own widening,
    // and it misfired the way it was always going to: an entrance block is the
    // town's whole stamped footprint, so standing on the open ground beside
    // Ankaran's wall and taking one step pulled the character into town (owner
    // report, Test12). Walking past a town or across a gate is now just a walk;
    // entering, leaving and gates fire only from an explicit order, which
    // `handle_userorder` checks on the destination.
    if mv.arrived() {
        mv.deadline = None;
        *movement = None;
    }
    // The stepped tile is now the authoritative cell, and it has to become so
    // *before* anything is built from the row: `announce_nearby_characters`
    // stamps `live.character`'s position into the `LOGINMANY` record it hands
    // to the neighbours. `handle_userorder` wrote the **destination** there
    // when the walk was ordered, so announcing from a stale row placed the
    // avatar at the far end of the path (or one tile behind) and the client,
    // which deletes and re-creates on a repeat record, warped to it.
    //
    // A 248 resync mid-walk and a re-issued 240 have to see this tile too, not
    // the destination the row was set to.
    if let Some(character) = live.character.as_mut() {
        character.pos_x = x;
        character.pos_y = y;
    }
    // The legacy fans this frame to the 3×3 near block (`Server.cpp:1689`), so a
    // bystander's copy of this avatar steps with ours instead of standing still
    // (slice 6 B1). `update_pos` runs *before* the fan so the observer set is
    // computed from the cell we have just reached, not the one we left.
    let was = registry.info(session_key);
    let was_cell = was.as_ref().map(|i| cell_of(i.x, i.y, i.map_id));
    registry.update_pos(session_key, x, y);
    {
        let character = live
            .character
            .as_ref()
            .ok_or("char update fan before the character row is loaded")?;
        fan_char_update(
            registry,
            session_key,
            live.char_unique,
            character,
            x,
            y,
            false,
        )?;
    }
    let now = registry.info(session_key);
    if let (Some(was), Some(now)) = (was.as_ref(), now.as_ref())
        && was_cell.is_some_and(|was| was != cell_of(x, y, now.map_id))
    {
        // Crossing a cell boundary is an *appearance* event, not just a movement
        // one.
        //
        // The legacy maintains a `m_pNewCell` set per character and runs
        // `SendNewOthersInfoToPerson` for it on cell entry, not only on world entry
        // (`ServerMap.cpp:73-150`, and the `m_uiNewCellCount` walk). Without this, a
        // player who walks up to somebody is never told they exist: no LOGINMANY, so no
        // avatar, so the 200 fan above is discarded on arrival.
        if let Some(character) = live.character.as_ref() {
            announce_nearby_characters(
                registry,
                session_key,
                character,
                live.char_unique,
                Some(was),
            )?;
        }
        // ...and the mirror image, which was missing entirely until now: the people
        // who *had* us and no longer do have to be told the avatar is gone.
        //
        // `SendPersonLeaveMsgToOthers` (`ServerMap.cpp:51-63`) sends
        // `GAMEMSG_RESPONSE_LOGOUT` to `m_pRemoveCell` — the cells the character is
        // leaving — and the client answers it by deleting the object
        // (`Client-Login.cpp:396-417`). Without the frame a departing avatar simply
        // stops receiving 200s and lingers on screen until the client's own screen
        // culling drops it, which is the abrupt pop at the screen edge.
        let left = logout_departed(registry, was, now, session_key, live.char_unique);
        if left > 0 {
            logln!("game: char {session_key} sent 11 to {left} observer(s) that lost sight of it");
        }
    }

    send_char_update(stream, outbound, live, x, y, false).await?;
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
/// The exit branch of [`handle_userorder`], run on the **order's destination** before the
/// walk is persisted — exactly where the legacy checks it (`char-order.cpp:173` →
/// `CharServerMoveByGate(x, y)`). `true` when a stamped exit gate was hit: the warp to
/// map 0 has replaced the walk and the order ends. `false` for any other tile, which
/// then walks on.
///
/// Both town exits lead the same way. At every town's load `MapCommon.cpp` stamps the
/// north `Gate_Wood1ToMain` on the map type's anchor (`:436-453`) *and* FO 292
/// `Gate3_MainToBear1` on the fixed (35,63) (`:463,474-490`), each from the same
/// `cltGate(siVillageUnique, ..., 0, mapposx, mapposy, false)` (`:431-432`, `:469-470`) —
/// so whichever gate the destination names, the landing is the road just south of the
/// entrance block on map 0, and `GateInfoE.txt` has no row that leaves a village.
#[allow(clippy::too_many_arguments)]
async fn warp_out_of_village(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    characters: &dyn CharacterRepository,
    registry: &Registry,
    session_key: SessionKey,
    live: &mut LiveFrames<'_>,
    map_id: i32,
    dest: (i32, i32),
) -> Result<bool, AnyError> {
    let Some((village, landing)) = village_exit(map_id, dest.0, dest.1) else {
        return Ok(false);
    };
    logln!(
        "game: village {} exit: map {map_id} ordered {dest:?} -> map 0 at {landing:?}",
        village.unique
    );
    let gate = warp_gate_to(map_id, 0, landing.0, landing.1);
    warp_through_gate(
        stream,
        outbound,
        characters,
        registry,
        session_key,
        live,
        &gate,
    )
    .await?;
    Ok(true)
}

pub(crate) async fn handle_userorder(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    characters: &PgCharacterRepository,
    registry: &Registry,
    session_key: SessionKey,
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
    //
    // One 241 goes to the mover and the *same* bytes fan to the observers: the
    // legacy's `SendNeighbourMsg(&clMsg, true)` (`char-order.cpp:170`) writes a
    // single frame to the whole 3×3 block, the mover included, so there is no
    // second self-copy to send here.
    let order_241_body: Vec<u8> = OrderData::move_order(live.char_unique, x16, y16)
        .encode()
        .to_vec();
    send_encoded(
        stream,
        outbound,
        GameMsg::ResponseUserorder,
        &order_241_body,
    )
    .await?;
    // The observer's client turns this 241 into the walk to the destination
    // (`DoMsg_GAMEMSG_RESPONSE_USERORDER` → `LetOrder` → `PushOrder`,
    // `Client-SystemNPC.cpp:538/689`); without it a bystander only sees the
    // per-tile 200s and animates each tile as a discrete jump.
    //
    // Build a temporary SessionInfo for the mover to fan the 241.
    let mover_session_info = session_info_for(live)?;
    registry.broadcast_near_others(
        &mover_session_info,
        session_key,
        GameMsg::ResponseUserorder as u16,
        order_241_body.clone(),
    );
    // The reference server broadcasts the cell the move starts from, then one
    // 200 per committed tile (the captured run was 241, 200, 200). The start-cell
    // 200 is fanned too (slice 6 B1): the legacy's `SendCharUpdate` is one call for
    // both, so an observer that missed the origin tile would otherwise start its
    // walk from a stale one. Self still gets it directly, before the fan, so the
    // ordering the capture shows (241, 200, 200…) is preserved for this client.
    send_char_update(stream, outbound, live, start.0, start.1, false).await?;
    fan_char_update(
        registry,
        session_key,
        live.char_unique,
        live.character
            .as_ref()
            .ok_or("userorder before the row is loaded")?,
        start.0,
        start.1,
        false,
    )?;

    // Leaving a town runs on the **order's destination** before the walk is persisted,
    // where the legacy checks it (`char-order.cpp:173`) — a hit on either of the town's
    // two stamped gates replaces the walk with a warp; see `warp_out_of_village`.
    if warp_out_of_village(
        stream,
        outbound,
        characters,
        registry,
        session_key,
        live,
        map_id,
        dest,
    )
    .await?
    {
        return Ok(None);
    }
    // The village entrances are checked on the same seam, and before the gates: the
    // legacy runs one object lookup over the ordered cell and dispatches on its type
    // (`char.cpp:8975-8983` village, `:9028` gate).
    if let Some(village) = village_at(map_id, dest.0, dest.1) {
        logln!(
            "game: village {} entered: map {map_id} ({}, {}) -> map {} at ({}, {})",
            village.unique,
            village.src_x,
            village.src_y,
            village.dst_map,
            village.dst_x,
            village.dst_y
        );
        let gate = village_warp_gate(map_id, village);
        warp_through_gate(
            stream,
            outbound,
            characters,
            registry,
            session_key,
            live,
            &gate,
        )
        .await?;
        return Ok(None);
    }
    if let Some((village, distance)) = village_within(map_id, dest.0, dest.1, 3) {
        logln!(
            "game: move order to {dest:?} on map {map_id} is {distance} tile(s) from the village \
             {} entrance at ({}, {}) — if the client sent this cell for a village click, the \
             entrance's block is wider than the table",
            village.unique,
            village.src_x,
            village.src_y
        );
    }
    if let Some(gate) = gate_at(map_id, dest.0, dest.1) {
        warp_through_gate(
            stream,
            outbound,
            characters,
            registry,
            session_key,
            live,
            gate,
        )
        .await?;
        // No step is armed: the character is not walking to that tile, it left the
        // map. The read loop drops the previous movement when this returns `None`.
        return Ok(None);
    }
    // Not a gate. If the order landed *near* one, say so: the client resolves a gate
    // by the field object the player clicked and sends that cell, so a click on a
    // gate we do not recognise is how a wider-than-one-tile gate would show up. See
    // [`gate_within`].
    if let Some((gate, distance)) = gate_within(map_id, dest.0, dest.1, 2) {
        logln!(
            "game: move order to {dest:?} on map {map_id} is {distance} tile(s) from the gate \
             recorded at ({}, {}) -> map {} — if the client sent this cell for a gate click, the \
             gate's footprint is wider than the table",
            gate.src_x,
            gate.src_y,
            gate.dst_map
        );
    }

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

/// Moves a character through `gate`: the map change, the frames that carry it, and
/// every store that has to agree afterwards.
///
/// The legacy's shape is `clCharServer::CharServerMoveByGate` → `SetWarpInfo` →
/// `Warp(true)` (`char.cpp:9317-9345`, `:4043`), and a warp is literally "re-send
/// the login packet": `char.cpp:4574` builds the character's own
/// `cltMakeCharInfo` for the new map and sends it as `GAMEMSG_RESPONSE_LOGIN` (5).
/// The client then loads the destination map from its own files (`map/MainMap/
/// MainMap.map`) — terrain never rides the wire — and the ordinary cell-join path
/// tells it who (and which NPCs) are already there.
///
/// Order matters: the row, the cached character and the registry entry move
/// first, because every frame below is built from them and a stale map on any of
/// the three would aim the next fan at the map the character just left.
///
/// # Errors
///
/// Propagates database, encoder and socket failures.
async fn warp_through_gate(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    characters: &dyn CharacterRepository,
    registry: &Registry,
    session_key: SessionKey,
    live: &mut LiveFrames<'_>,
    gate: &Gate,
) -> Result<(), AnyError> {
    // The entry requirement first, exactly where the legacy checks it
    // (`char.cpp:9123-9133`) — before anything moves, and without consuming the item.
    if gate.item_unique > 0 && gate.item_num > 0 {
        let character_id = live
            .character
            .as_ref()
            .ok_or("gate entry before the character row is loaded")?
            .character_id;
        let have: i64 = live
            .items
            .list(character_id)
            .await?
            .iter()
            .filter(|row| row.item_unique == gate.item_unique)
            .map(|row| i64::from(row.count))
            .sum();
        if have < i64::from(gate.item_num) {
            logln!(
                "game: gate {} -> map {} refused: needs item {} x{}, character {character_id} has {have}",
                gate.src_map,
                gate.dst_map,
                gate.item_unique,
                gate.item_num
            );
            return Ok(());
        }
    }
    let warp = apply_gate_move(
        characters,
        registry,
        session_key,
        live.char_unique,
        &mut live.character,
        gate,
    )
    .await?;
    let character_id = warp.moved.character_id;

    // The map change itself: opcode 5 with this character's record.
    let info = make_char_info_for(&warp.moved, live.char_unique)?;
    send_encoded(stream, outbound, GameMsg::ResponseLogin, &info.encode()).await?;

    // Both ends of the move. The departure fan is computed against the *new* cell
    // in the *new* map, so every session still near the old tile gets the 11 and
    // nobody on the destination gets a spurious one.
    let left = logout_departed(
        registry,
        &warp.was,
        &warp.now,
        session_key,
        live.char_unique,
    );
    announce_nearby_characters(registry, session_key, &warp.moved, live.char_unique, None)?;
    // The destination's own NPCs, which the client has never been told about.
    announce_map_npcs(registry, session_key, warp.now.map_id);

    logln!(
        "game: char {character_id} took the gate {} -> map {} at ({}, {}); {left} observer(s) \
         on the old map were told",
        warp.was.map_id,
        gate.dst_map,
        gate.dst_x,
        gate.dst_y
    );
    Ok(())
}

/// What a gate move changed, before any frame is built from it.
struct GateWarp {
    /// The character row, already moved to the destination.
    moved: CharacterFull,
    /// The near-fan view *before* the move — the set the departure 11 is diffed
    /// against (`logout_departed`).
    was: SessionInfo,
    /// The same view after it.
    now: SessionInfo,
}

/// Applies a gate move to every store that names the character's place: the row,
/// the cached copy and the registry entry.
///
/// Separated from the frames so the decision is testable without a socket or a
/// database — the same split the quest handlers use (T-019 slice 3 review, F2).
///
/// # Errors
///
/// [`gz_db::RepositoryError`] from the map write, which the caller propagates.
async fn apply_gate_move(
    characters: &dyn CharacterRepository,
    registry: &Registry,
    session_key: SessionKey,
    char_unique: i32,
    character: &mut Option<CharacterFull>,
    gate: &Gate,
) -> Result<GateWarp, AnyError> {
    let was = session_info_from(character, char_unique)?;
    let row = character
        .as_mut()
        .ok_or("a gate move before the character row is loaded")?;

    characters
        .update_map_position(row.character_id, gate.dst_map, gate.dst_x, gate.dst_y)
        .await?;
    row.map_id = gate.dst_map;
    row.pos_x = gate.dst_x;
    row.pos_y = gate.dst_y;
    let moved = row.clone();
    registry.update_map_pos(session_key, gate.dst_map, gate.dst_x, gate.dst_y);

    let now = SessionInfo {
        map_id: moved.map_id,
        x: moved.pos_x,
        y: moved.pos_y,
        ..was.clone()
    };
    Ok(GateWarp { moved, was, now })
}

/// The [`SessionInfo`] the near fan keys off, built from the connection's live
/// row. Shared by the move 241 fan and the sit/stand 241 fan so both compute the
/// observer set from the one character state.
///
/// # Errors
///
/// A live `char_unique` that is not a player-band id. Registered sessions
/// cannot hold one (`register` gates it and the type refuses it), so this is
/// the invariant speaking, not the flow (design doc §5.3).
fn session_info_for(live: &LiveFrames<'_>) -> Result<SessionInfo, AnyError> {
    session_info_from(&live.character, live.char_unique)
}

/// [`session_info_for`] over a bare row, so a test can build the same view
/// without a connection.
///
/// # Errors
///
/// The same refusal as [`session_info_for`]: the typed conversion refuses
/// anything outside `1..=PLAYER_CHAR_UNIQUE_TOP` — including ids that used to
/// slip through the old `as i16` truncation.
fn session_info_from(
    character: &Option<CharacterFull>,
    char_unique: i32,
) -> Result<SessionInfo, AnyError> {
    let character = character.as_ref();
    Ok(SessionInfo {
        char_unique: PlayerCharUnique::try_from_si16(i16::try_from(char_unique).map_err(
            |_| format!("session char_unique {char_unique} does not fit the SI16 siCharUnique"),
        )?)?,
        name: character.map(|c| c.name.clone()).unwrap_or_default(),
        level: character.map(|c| c.level as i32).unwrap_or(1),
        map_id: character.map(|c| c.map_id).unwrap_or(0),
        x: character.map(|c| c.pos_x).unwrap_or(0),
        y: character.map(|c| c.pos_y).unwrap_or(0),
        school_id: character.map(|c| c.school_id).unwrap_or(0),
        class_kind: character.map(|c| c.class_kind as u16).unwrap_or(1),
        life: character.map(|c| c.max_life as u32).unwrap_or(173),
        move_speed: character
            .map(|c| c.move_speed.clamp(0, 63) as u8)
            .unwrap_or(12),
        sex: character.map(|c| c.gender as u8).unwrap_or(1),
        gm_level: 0,
    })
}

/// Applies `GAMEMSG_REQUEST_SITDOWN` (7231) — the sit/stand toggle, T-018
/// slice 6 B4's sit/stand leg.
///
/// The client sends this opcode, **not** a 240 `ORDER_SITDOWN`
/// (`MouseOperation.cpp:4132`): the 240/241 order in [`handle_userorder`] is a
/// different flow. Here the server's answer is the 241 `RESPONSE_USERORDER` that
/// tells the sitter's own client and every observer which stance to draw.
///
/// `bSitDown` picks `ORDER_SITDOWN` (the 8-way facing rides in `siPara1`) or
/// `ORDER_FORCESTOP` (`Server-Person.cpp:3297-3324`). A sit whose facing is not
/// one of the eight the legacy accepts is dropped, as the legacy's `default:`
/// returns (`:3317`). The combat leg of B4 (the 268 hit fan) is deliberately not
/// here.
///
/// # Errors
///
/// A malformed body, or a socket/cipher failure.
pub(crate) async fn handle_sit_down(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    registry: &Registry,
    session_key: SessionKey,
    live: &LiveFrames<'_>,
    payload: &[u8],
) -> Result<(), AnyError> {
    let request = SitDown::decode(payload)?;
    let order = if request.b_sit_down {
        if !SIT_DIRECTIONS.contains(&request.direction) {
            logln!(
                "game: sitdown facing {} -> no reply (legacy drops a non-8-way \
                 direction; Server-Person.cpp:3317)",
                request.direction
            );
            return Ok(());
        }
        OrderData::sit_order(live.char_unique, request.direction)
    } else {
        OrderData::force_stop_order(live.char_unique)
    };
    let body = order.encode().to_vec();
    // Self gets the frame directly — its client only adopts the stance once the
    // 241 lands, since `OperateAutoLeftOrder` just sends the request — then the
    // same bytes fan to the near block, self excluded, exactly as the move 241
    // does (`char-order.cpp:526` `SendNeighbourMsg(..., true)`).
    send_encoded(stream, outbound, GameMsg::ResponseUserorder, &body).await?;
    let origin = session_info_for(live)?;
    let fanned = registry.broadcast_near_others(
        &origin,
        session_key,
        GameMsg::ResponseUserorder as u16,
        body,
    );
    logln!(
        "game: sitdown char {} {} (order {}) fanned 241 to {fanned} observer(s)",
        live.char_unique,
        if order.si_order == ORDER_SITDOWN {
            "sit"
        } else {
            "stand"
        },
        order.si_order
    );
    Ok(())
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
pub(crate) async fn handle_leave_request(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    characters: &PgCharacterRepository,
    character: Option<&CharacterFull>,
    ack: GameMsg,
) -> Result<(), AnyError> {
    // Where the character stands is the row's business, not a session copy's:
    // `step_movement` re-seats the row per committed tile and a gate warp moves
    // it with the map, so this save follows a warp instead of undoing it.
    if let Some(character) = character {
        characters
            .update_position(character.character_id, character.pos_x, character.pos_y)
            .await?;
    }
    send_encoded(stream, outbound, ack, &[]).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geography::{VILLAGES, village_exit2_block};
    use gz_db::{PgItemRepository, RepositoryError};

    /// The gate table is a transcription of `Data\GateInfoE.txt`, so the test
    /// states the file's own facts: Hanyang's three exits land on the outdoor map
    /// at its gate tile, and the outdoor map's way back lands on the palace tile
    /// the login spawn uses.
    #[test]
    fn the_gate_table_matches_the_shipped_gate_rows() {
        assert_eq!(gate_at(158, 14, 90).unwrap().dst_map, 0);
        assert_eq!(gate_at(158, 28, 101).unwrap().dst_map, 0);
        assert_eq!(gate_at(158, 41, 90).unwrap().dst_map, 0);
        for (x, y) in [(14, 90), (28, 101), (41, 90)] {
            let gate = gate_at(158, x, y).unwrap();
            assert_eq!(
                (gate.dst_x, gate.dst_y),
                (288, 354),
                "one shared landing tile"
            );
        }
        // Row 306, the reverse: the destination is the (27,87) the spawn uses
        // (`Server-Login.cpp:375`).
        let back = gate_at(0, 291, 350).unwrap();
        assert_eq!((back.dst_map, back.dst_x, back.dst_y), (158, 27, 87));

        // A tile that is not a gate is not one, on either map.
        assert!(
            gate_at(158, 27, 87).is_none(),
            "the spawn tile is not a gate"
        );
        assert!(
            gate_at(158, 288, 354).is_none(),
            "the landing tile is on map 0"
        );
        assert!(gate_at(1, 14, 90).is_none(), "gates are per-map");
        // The whole file is loaded now, so the table is no longer only Hanyang's pair:
        // the outdoor map alone carries dozens of gates (46 rows start on map 0).
        let outdoor = GATES.iter().filter(|g| g.src_map == 0).count();
        assert!(outdoor >= 40, "only {outdoor} gate(s) start on map 0");
        // `PutFieldObject` refuses a block that overlaps one already stamped
        // (`Map-FieldObject.cpp:88-97`), and the client's gate table is keyed by unique,
        // so two rows can never own the same cell. `gate_at` returns the first match, so
        // an overlap would also make which gate wins depend on row order.
        for (i, gate) in GATES.iter().enumerate() {
            for other in &GATES[i + 1..] {
                if gate.src_map != other.src_map {
                    continue;
                }
                let (l1, t1, r1, b1) = gate.block();
                let (l2, t2, r2, b2) = other.block();
                assert!(
                    r1 < l2 || r2 < l1 || b1 < t2 || b2 < t1,
                    "gate blocks overlap on map {}: ({l1},{t1})-({r1},{b1}) and ({l2},{t2})-({r2},{b2})",
                    gate.src_map
                );
            }
        }
        // No two rows share an anchor either: the file's one duplicate (unique 670 on
        // map 418) is dropped by the generator with a warning.
        for (i, gate) in GATES.iter().enumerate() {
            for other in &GATES[i + 1..] {
                assert!(
                    !(gate.src_map == other.src_map
                        && gate.src_x == other.src_x
                        && gate.src_y == other.src_y),
                    "two gate rows share the anchor ({}, {}) on map {}",
                    gate.src_x,
                    gate.src_y,
                    gate.src_map
                );
            }
        }
        // The entry requirement, straight off the gate rows: exactly eight gates in the
        // file carry one, and the Raccoon Lair (row 561, anchor map 0 (224,314)) is the
        // one a player meets first — item 24107, one of them (`char.cpp:9123-9133`).
        let lair = GATES
            .iter()
            .find(|g| g.dst_map == 320)
            .expect("the Raccoon Lair gate");
        assert_eq!(
            (lair.dst_map, lair.item_unique, lair.item_num),
            (320, 24107, 1)
        );
        let gated = GATES
            .iter()
            .filter(|g| g.item_unique > 0 && g.item_num > 0)
            .count();
        assert_eq!(gated, 8, "the file's gated rows");
        // Hanyang's exits and the mine take nothing. The mine is looked up in its stamped
        // block, which holds the editor's marker (402,580).
        for (map, x, y) in [(158, 14, 90), (158, 28, 101), (158, 41, 90), (0, 402, 580)] {
            let gate = gate_at(map, x, y).unwrap_or_else(|| panic!("({map}, {x}, {y})"));
            assert_eq!(
                (gate.item_unique, gate.item_num),
                (0, 0),
                "({map}, {x}, {y}) must be free"
            );
        }
    }
    /// The destinations are inside the 10-bit fields the record packs, and the
    /// client's map index for them is the map id itself
    /// (`gz_proto::game::client_map_index`).
    /// The order path and the near-gate diagnostic both hang off these two
    /// lookups, so their semantics are pinned here.
    ///
    /// The gate is a **4×4 block ending at the anchor**: `PutFieldObject`
    /// (`Map-FieldObject.cpp:102-107`) stamps `x = six + centerx - xsize + 1 + j` and
    /// `y = siy + centery - ysize + 1 + i` with `Gate_Warp01`'s own `4 4 0 0`.
    #[test]
    fn every_cell_of_the_gate_block_is_the_gate() {
        // Map 158's centre exit is row 308 of `GateInfoE.txt`: (28, 101) -> map 0 (288, 354).
        assert!(gate_at(158, 28, 101).is_some_and(|g| g.dst_map == 0));
        // Once more for the anchor binding, then the block: x in [25, 28], y in [98, 101].
        // The cells the live run actually ordered to are inside it, which is the finding.
        for (x, y) in [
            (26, 98),
            (26, 100),
            (27, 100),
            (26, 101),
            (28, 99),
            (25, 98),
        ] {
            assert!(
                gate_at(158, x, y).is_some(),
                "({x}, {y}) is inside the circle"
            );
        }
        // One cell outside the block on each side is not a gate.
        assert!(gate_at(158, 24, 101).is_none());
        assert!(gate_at(158, 28, 97).is_none());
        assert!(gate_at(158, 29, 101).is_none());
        assert!(gate_at(158, 28, 102).is_none());
        // The two side exits of the same map, and the outward gate on map 0.
        assert!(gate_at(158, 14, 90).is_some());
        assert!(gate_at(158, 41, 90).is_some());
        assert!(gate_at(0, 291, 350).is_some_and(|g| g.dst_map == 158));
        // A gate block is not honoured on another map.
        assert!(gate_at(109, 28, 101).is_none());
    }

    /// `Revive()`'s destination rules, and the two facts that decide them: `INIT_LEVEL`
    /// is 10 (`CommonHeader.h:672`) and the palace spawn is (158, 27, 87)
    /// (`char.cpp:5045`).
    #[test]
    fn a_revive_sends_beginners_to_hanyang_and_dungeon_dwellers_back_to_the_mouth() {
        // Below 10: always Hanyang, whatever map they died on (`char.cpp:5027-5046`).
        assert_eq!(revive_target(1, 320), REVIVE_PALACE);
        assert_eq!(revive_target(9, 0), REVIVE_PALACE);
        assert_eq!(revive_target(9, 158), REVIVE_PALACE);
        // At or above it, a dungeon returns to the map that gate-enters it
        // (`char.cpp:4914-4917`): the Lair to its mouth on map 0, the mine likewise.
        // Read the row rather than hard-coding: the anchor is the *stamped* block's, measured
        // from the map file — the cell `GateInfoE.txt` names is the same one, the block's
        // `ValidSwitch` cell.
        for map_id in [320, 157] {
            let entrance = GATES
                .iter()
                .find(|g| g.dst_map == map_id)
                .unwrap_or_else(|| panic!("no entrance into map {map_id}"));
            assert_eq!(
                revive_target(30, map_id),
                (entrance.src_map, entrance.src_x, entrance.src_y)
            );
        }
        // A town keeps you in a town: the palace is entered by a gate row too, and
        // sending a Hanyang character outside to that gate would not be "the nearest
        // town".
        assert_eq!(revive_target(30, 158), REVIVE_PALACE);
        assert_eq!(revive_target(30, 0), REVIVE_PALACE);
        // A map nothing enters falls back to the palace (`char.cpp:5007-5013`).
        assert_eq!(revive_target(30, 999), REVIVE_PALACE);
    }

    /// The village table: village 1 (Ankaran) is stamped at x[263..270] y[319..326] around the
    /// marker (269,325) and village 3 (Talasha) at x[235..242] y[297..304] around (241,303)
    /// — `villageinfoE.txt:12,14` — and both land on their own map at the engine's own
    /// centre, (25,13): the `SetMapType` value the legacy entry warp reads
    /// (`char.cpp:9011-9012`), not the dead file's (24,22).
    #[test]
    fn a_village_entrance_warps_to_the_town_entry_spawn() {
        let ankaran = village_at(0, 263, 319).expect("Ankaran's entrance");
        assert_eq!(
            (
                ankaran.unique,
                ankaran.dst_map,
                ankaran.dst_x,
                ankaran.dst_y
            ),
            (1, 1, 25, 13)
        );
        let talasha = village_at(0, 235, 297).expect("Talasha's entrance");
        assert_eq!(
            (
                talasha.unique,
                talasha.dst_map,
                talasha.dst_x,
                talasha.dst_y
            ),
            (3, 3, 25, 13)
        );
        // Every row is an entrance on map 0 with a destination that has a centre.
        for village in VILLAGES {
            assert!(village.src_x > 0 && village.src_y > 0, "{village:?}");
            assert!(village.dst_map > 0, "{village:?}");
            assert!(village.dst_x > 0 && village.dst_y > 0, "{village:?}");
        }
        assert!(VILLAGES.len() >= 4, "only {} villages", VILLAGES.len());
        // Villages live on the world map, so nothing else answers.
        assert!(village_at(158, 269, 325).is_none());
        // The blocks the shipped `MainMap.map` stamps: Ankaran's entrance is x[263..270]
        // y[319..326] with the editor's marker (269,325) inside it, Talasha's x[235..242]
        // y[297..304].
        let (left, top, right, bottom) = ankaran.block();
        assert_eq!((left, top, right, bottom), (263, 319, 270, 326));
        let (left, top, right, bottom) = talasha.block();
        assert_eq!((left, top, right, bottom), (235, 297, 242, 304));
        // The ordered-cell rule stays exact: only the block's own cells.
        assert!(village_at(0, 263, 319).is_some());
        assert!(village_at(0, 270, 326).is_some());
        assert!(
            village_at(0, 262, 322).is_none(),
            "one cell outside the block"
        );
        assert!(village_at(0, 271, 327).is_none(), "and past the far corner");
        // No range is invented on top: the client sends the cell under the cursor and nothing
        // else (see `village_at`'s doc for the file:line trail), so the rule is the block, cell
        // for cell, however far away the character stood when the click happened.
        assert!(
            village_at(0, 259, 315).is_none(),
            "four tiles off is not the entrance"
        );
        assert!(village_at(0, 257, 313).is_none(), "nor six");
        assert!(
            village_at(158, 269, 325).is_none(),
            "villages live on map 0"
        );
        // Leaving: at map load the engine stamps Ankaran's `Gate_Wood1ToMain` over
        // [23..26]x[6..8] (4x3, anchor (25,7)), and the exit lands on the road just south
        // of the entrance block, at (266, 327).
        let (village, landing) = village_exit(1, 25, 7).expect("the village gate");
        assert_eq!(village.unique, 1);
        assert_eq!(landing, (266, 327));
        // All 12 stamped cells exit, far corner included.
        for (x, y) in [(23, 6), (26, 6), (23, 8), (26, 8)] {
            assert!(village_exit(1, x, y).is_some(), "stamped cell ({x},{y})");
        }
        // One tile outside does not — and neither do the stairs/spawn: the spawn itself
        // (25,13) is the engine centre, and neither it, the dead-file centre (24,22) nor
        // the tile below the spawn sits near the stamped block.
        for (x, y) in [
            (22, 7),
            (27, 7),
            (25, 5),
            (25, 9),
            (24, 22),
            (25, 13),
            (25, 14),
        ] {
            assert!(
                village_exit(1, x, y).is_none(),
                "outside the block ({x},{y})"
            );
        }
        assert!(
            village_exit(0, 25, 7).is_none(),
            "map 0 is not a village map"
        );
        // The diagnostic measures to the block: (261,322) is two tiles left of it.
        let (near, distance) = village_within(0, 261, 322, 3).expect("two tiles away");
        assert_eq!((near.unique, distance), (1, 2));
        assert!(village_within(158, 270, 322, 3).is_none());
    }

    /// The mine's gate: `GateInfoE.txt:19` names the source cell (402,580), and the shipped
    /// `MainMap.map` stamps the object's block at x[400..403] y[577..581] — the named cell is
    /// the block's `ValidSwitch` cell, inside it. The table carries the measured anchor, so a
    /// click anywhere in the block is the gate.
    #[test]
    fn the_mine_gate_is_the_block_the_map_stamps() {
        let gate = gate_at(0, 402, 580).expect("the stamp block");
        assert_eq!((gate.dst_map, gate.dst_x, gate.dst_y), (157, 60, 97));
        let (left, top, right, bottom) = gate.block();
        assert_eq!((left, top, right, bottom), (400, 577, 403, 581));
        assert!(
            gate_at(0, 400, 577).is_some(),
            "the block's other corner is a gate too"
        );
        assert!(
            gate_at(0, 403, 582).is_none(),
            "one cell past the far corner is not"
        );
    }

    #[test]
    fn the_near_gate_diagnostic_reports_the_closest_row_within_radius() {
        // Distance is measured to the anchor cell, and the diagnostic is only about
        // telling an unrecognised order apart from a gate hit — which the block check in
        // [`every_cell_of_the_gate_block_is_the_gate`] now covers.
        let (gate, distance) = gate_within(158, 28, 101, 0).expect("the anchor itself");
        assert_eq!((gate.src_x, gate.src_y), (28, 101));
        assert_eq!(distance, 0);
        // Five tiles north of the anchor is out of a two-tile radius and in a six-tile one.
        assert!(gate_within(158, 28, 96, 2).is_none());
        assert_eq!(gate_within(158, 28, 96, 6).map(|(_, d)| d), Some(5));
        assert!(gate_within(109, 28, 101, 2).is_none());
    }

    #[test]
    fn every_gate_destination_fits_the_record() {
        for gate in GATES {
            assert!((0..1024).contains(&gate.dst_x), "uiX is 10 bits");
            assert!((0..1024).contains(&gate.dst_y), "uiY is 10 bits");
            assert!(
                gz_proto::game::client_map_index(gate.dst_map) == gate.dst_map as u16,
                "the frame carries the client's map index"
            );
        }
    }

    /// One tile can only be one gate: `gate_at` returns the first match, so a
    /// duplicate row would silently shadow the other one.
    #[test]
    fn no_two_gate_rows_share_a_source_tile() {
        let mut seen = std::collections::HashSet::new();
        for gate in GATES {
            assert!(
                seen.insert((gate.src_map, gate.src_x, gate.src_y)),
                "two gate rows claim ({}, {}):{}",
                gate.src_map,
                gate.src_x,
                gate.src_y
            );
        }
    }

    /// A character repository that records the two position writes and nothing
    /// else — the seam `apply_gate_move` takes so the warp's decision is testable
    /// without PostgreSQL (T-019 slice 3 review, F2).
    #[derive(Default)]
    struct MemoryCharacters {
        map_writes: std::sync::Mutex<Vec<(i32, i32, i32, i32)>>,
    }

    #[async_trait::async_trait]
    impl CharacterRepository for MemoryCharacters {
        async fn list_for_account(
            &self,
            _account_id: i32,
        ) -> Result<Vec<gz_db::CharacterSummary>, RepositoryError> {
            Ok(Vec::new())
        }

        async fn find_by_slot(
            &self,
            _account_id: i32,
            _slot: i16,
        ) -> Result<Option<CharacterFull>, RepositoryError> {
            Ok(None)
        }

        async fn find_by_name(
            &self,
            _name: &str,
        ) -> Result<Option<CharacterFull>, RepositoryError> {
            Ok(None)
        }

        async fn create_character(
            &self,
            _account_id: i32,
            _name: &str,
            _class_kind: i16,
            _gender: i16,
            _slot: i16,
        ) -> Result<i32, RepositoryError> {
            Err(RepositoryError::NotFound)
        }

        async fn delete_character(
            &self,
            _account_id: i32,
            _slot: i16,
        ) -> Result<Option<i32>, RepositoryError> {
            Ok(None)
        }

        async fn set_delete_pending(
            &self,
            _account_id: i32,
            _slot: i16,
            _pending: bool,
        ) -> Result<Option<i32>, RepositoryError> {
            Ok(None)
        }

        async fn resolve_game_unique(&self, _character_id: i32) -> Result<i32, RepositoryError> {
            Err(RepositoryError::NotFound)
        }

        async fn update_position(
            &self,
            _character_id: i32,
            _x: i32,
            _y: i32,
        ) -> Result<(), RepositoryError> {
            Ok(())
        }

        async fn update_map_position(
            &self,
            character_id: i32,
            map_id: i32,
            x: i32,
            y: i32,
        ) -> Result<(), RepositoryError> {
            self.map_writes
                .lock()
                .unwrap()
                .push((character_id, map_id, x, y));
            Ok(())
        }

        /// The quest reward path pays through this; the movement tests do not, so it
        /// only has to be honest about doing nothing.
        async fn add_money(&self, _character_id: i32, _amount: i64) -> Result<(), RepositoryError> {
            Ok(())
        }

        async fn update_stats(
            &self,
            _character_id: i32,
            _stats: &gz_db::StatsUpdate,
        ) -> Result<(), RepositoryError> {
            Ok(())
        }

        /// Like the others, does not track anything; it only reports the delta
        /// back as the new total so a caller's arithmetic is still exercised.
        async fn add_exp(&self, _character_id: i32, delta: i64) -> Result<i64, RepositoryError> {
            Ok(delta)
        }

        /// No test levels a character up — the legacy-shaped increments are
        /// the Pg implementation's business. The post-state is echoed back
        /// plausible (the requested maxima, one level on) so a caller reading
        /// the row still gets one.
        async fn level_up(
            &self,
            _character_id: i32,
            max_life: i32,
            max_mana: i32,
        ) -> Result<gz_db::LevelUpRow, RepositoryError> {
            Ok(gz_db::LevelUpRow {
                level: 2,
                exp: 0,
                bonus_points: 5,
                max_life,
                max_mana,
            })
        }
    }

    /// A row standing on Hanyang's east gate, as the character loader would hand
    /// it over.
    fn row_on_the_east_gate() -> CharacterFull {
        CharacterFull {
            character_id: 1,
            name: "Tester".to_owned(),
            class_kind: 1,
            gender: 1,
            level: 13,
            max_life: 173,
            max_mana: 40,
            move_speed: 12,
            exp: 0,
            money: 1000,
            map_id: 158,
            pos_x: 41,
            pos_y: 90,
            slot: 0,
            school_id: 0,
            game_unique: None,
            str: 20,
            dex: 20,
            vit: 5,
            mag: 5,
            hnd: 10,
            luk: 10,
            wis: 10,
            bonus_points: 5,
        }
    }

    /// The warp's decision, end to end and without a socket: the row, the cached
    /// copy and the registry all move together, and `was`/`now` describe the two
    /// ends the departure 11 is diffed against.
    #[tokio::test]
    async fn a_gate_move_moves_every_store_that_names_the_map() {
        let characters = MemoryCharacters::default();
        let registry = Registry::new();
        let key = Registry::next_key();
        let (tx, _rx) = tokio::sync::mpsc::channel(crate::registry::OUTBOUND_CAP);
        registry.register(
            key,
            tx,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            session_info_from(&Some(row_on_the_east_gate()), 1473)
                .expect("the test id is in the player band"),
        );

        let mut character = Some(row_on_the_east_gate());
        let gate = gate_at(158, 41, 90).expect("the east gate");
        let warp = apply_gate_move(&characters, &registry, key, 1473, &mut character, gate)
            .await
            .expect("the gate move applies");

        // 1. The durable row.
        assert_eq!(
            characters.map_writes.lock().unwrap().clone(),
            vec![(1, 0, 288, 354)],
            "the map must move with the position, or a re-login returns to Hanyang"
        );
        // 2. The cached copy every frame is built from.
        let cached = character.as_ref().expect("still loaded");
        assert_eq!((cached.map_id, cached.pos_x, cached.pos_y), (0, 288, 354));
        assert_eq!(warp.moved.map_id, 0);
        // 3. The registry entry the near fans key off.
        let stored = registry.info(key).expect("still registered");
        assert_eq!((stored.map_id, stored.x, stored.y), (0, 288, 354));
        // 4. The two ends the departure 11 diffs.
        assert_eq!((warp.was.map_id, warp.was.x, warp.was.y), (158, 41, 90));
        assert_eq!((warp.now.map_id, warp.now.x, warp.now.y), (0, 288, 354));
        assert!(
            !gz_proto::map_geometry::cells_adjacent(
                cell_of(warp.was.x, warp.was.y, warp.was.map_id),
                cell_of(warp.now.x, warp.now.y, warp.now.map_id),
            ),
            "crossing maps must never look like a step within one"
        );
    }

    /// A gate move with no character row is refused rather than writing a
    /// position for nobody.
    #[tokio::test]
    async fn a_gate_move_without_a_row_is_refused() {
        let characters = MemoryCharacters::default();
        let registry = Registry::new();
        let key = Registry::next_key();
        let (tx, _rx) = tokio::sync::mpsc::channel(crate::registry::OUTBOUND_CAP);
        registry.register(
            key,
            tx,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            session_info_from(&None, 1473).expect("the test id is in the player band"),
        );

        let mut character = None;
        let gate = gate_at(158, 41, 90).expect("the east gate");
        assert!(
            apply_gate_move(&characters, &registry, key, 1473, &mut character, gate)
                .await
                .is_err()
        );
        assert!(characters.map_writes.lock().unwrap().is_empty());
    }

    /// A connected socket pair: `step_movement` writes its 200s into one end
    /// and the test holds the other open, so nothing blocks on a full buffer.
    async fn socket_pair() -> (TcpStream, TcpStream) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port");
        let addr = listener.local_addr().expect("the bound address");
        let writer = TcpStream::connect(addr).await.expect("the client end");
        let (reader, _) = listener.accept().await.expect("the server end");
        (writer, reader)
    }

    /// A movement that is due on the next tick: the accumulator already holds a
    /// full step, so `step_movement`'s own `advance` commits the first tile.
    fn due_movement(dest: (i32, i32), pos: (i32, i32), map_id: i32) -> Movement {
        let mut movement = Movement::new(dest, pos, map_id, 12);
        movement.percent = MOVE_PERCENT_STEP;
        movement
    }

    /// A row standing at `(x, y)` on map 0.
    fn row_on_map0(x: i32, y: i32) -> CharacterFull {
        let mut row = row_on_the_east_gate();
        row.map_id = 0;
        row.pos_x = x;
        row.pos_y = y;
        row
    }

    /// The item repository `LiveFrames` carries: a pool that never connects,
    /// because the step path reads no items.
    fn lazy_items() -> PgItemRepository {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://none:none@127.0.0.1:1/none")
            .expect("a parseable URL, connected only on first use");
        PgItemRepository::new(pool)
    }

    /// The owner's live report, as a test: Test12 stood on the open ground
    /// beside Ankaran's wall — the entrance block's footprint is eight tiles
    /// deep around the marker — and one step pulled it into town. A committed
    /// step is **never** a warp: the legacy arms one only from an order's
    /// destination (`char-order.cpp:173` is `CharServerMoveByGate`'s only
    /// caller), so walking beside the wall is just walking.
    #[tokio::test]
    async fn a_step_across_an_entrance_block_does_not_warp() {
        // (263, 322) is Ankaran's leftmost entrance cell and (262, 322) is one
        // tile of open ground west of it, so the walk crosses the boundary.
        assert!(village_at(0, 263, 322).is_some(), "the block starts here");
        assert!(village_at(0, 262, 322).is_none(), "and this is off it");

        let (mut stream, _peer) = socket_pair().await;
        let mut outbound = Cipher::new();
        let items = lazy_items();
        let row = row_on_map0(262, 322);
        let mut live = LiveFrames {
            items: &items,
            account_id: 0,
            account_name: String::new(),
            character: Some(row.clone()),
            char_unique: 1473,
            friend_list: Vec::new(),
            quest: gz_proto::quest::PersonQuest::new(),
        };
        let registry = Registry::new();
        let key = Registry::next_key();
        let (tx, _rx) = tokio::sync::mpsc::channel(crate::registry::OUTBOUND_CAP);
        registry.register(
            key,
            tx,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            session_info_from(&Some(row), 1473).expect("the test id is in the player band"),
        );

        let mut movement = Some(due_movement((265, 322), (262, 322), 0));
        step_movement(
            &mut stream,
            &mut outbound,
            &mut live,
            &mut movement,
            &registry,
            key,
        )
        .await
        .expect("the step commits");

        let character = live.character.as_ref().expect("still loaded");
        assert_eq!(
            (character.map_id, character.pos_x, character.pos_y),
            (0, 263, 322),
            "the step lands on the entrance block and stays on map 0"
        );
        let stored = registry.info(key).expect("still registered");
        assert_eq!((stored.map_id, stored.x, stored.y), (0, 263, 322));
        assert!(movement.is_some(), "the walk continues past the block");
    }

    /// The same rule for a gate: the palace gate's block on map 0 is nine by
    /// four (`x[284..292] y[347..350]`), and stepping onto it must not move the
    /// character to Hanyang. Only an order that names such a cell does that.
    #[tokio::test]
    async fn a_step_across_a_gate_block_does_not_warp() {
        assert!(gate_at(0, 284, 350).is_some(), "the block starts here");
        assert!(gate_at(0, 283, 350).is_none(), "and this is off it");

        let (mut stream, _peer) = socket_pair().await;
        let mut outbound = Cipher::new();
        let items = lazy_items();
        let row = row_on_map0(283, 350);
        let mut live = LiveFrames {
            items: &items,
            account_id: 0,
            account_name: String::new(),
            character: Some(row.clone()),
            char_unique: 1473,
            friend_list: Vec::new(),
            quest: gz_proto::quest::PersonQuest::new(),
        };
        let registry = Registry::new();
        let key = Registry::next_key();
        let (tx, _rx) = tokio::sync::mpsc::channel(crate::registry::OUTBOUND_CAP);
        registry.register(
            key,
            tx,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            session_info_from(&Some(row), 1473).expect("the test id is in the player band"),
        );

        let mut movement = Some(due_movement((286, 350), (283, 350), 0));
        step_movement(
            &mut stream,
            &mut outbound,
            &mut live,
            &mut movement,
            &registry,
            key,
        )
        .await
        .expect("the step commits");

        let character = live.character.as_ref().expect("still loaded");
        assert_eq!(
            (character.map_id, character.pos_x, character.pos_y),
            (0, 284, 350),
            "the step lands on the gate block and stays on map 0"
        );
        let stored = registry.info(key).expect("still registered");
        assert_eq!((stored.map_id, stored.x, stored.y), (0, 284, 350));
    }

    /// And for leaving: the village map's exit is the stamped 4x3 gate block at anchor
    /// (25,7) — x[23..26] y[6..8] — on Ankaran's own map, and a step onto one of its
    /// cells is a step too.
    #[tokio::test]
    async fn a_step_across_a_village_exit_does_not_warp() {
        assert!(village_exit(1, 24, 8).is_some(), "the exit tile");

        let (mut stream, _peer) = socket_pair().await;
        let mut outbound = Cipher::new();
        let items = lazy_items();
        let mut row = row_on_map0(24, 9);
        row.map_id = 1;
        let mut live = LiveFrames {
            items: &items,
            account_id: 0,
            account_name: String::new(),
            character: Some(row.clone()),
            char_unique: 1473,
            friend_list: Vec::new(),
            quest: gz_proto::quest::PersonQuest::new(),
        };
        let registry = Registry::new();
        let key = Registry::next_key();
        let (tx, _rx) = tokio::sync::mpsc::channel(crate::registry::OUTBOUND_CAP);
        registry.register(
            key,
            tx,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            session_info_from(&Some(row), 1473).expect("the test id is in the player band"),
        );

        let mut movement = Some(due_movement((24, 8), (24, 9), 1));
        step_movement(
            &mut stream,
            &mut outbound,
            &mut live,
            &mut movement,
            &registry,
            key,
        )
        .await
        .expect("the step commits");

        let character = live.character.as_ref().expect("still loaded");
        assert_eq!(
            (character.map_id, character.pos_x, character.pos_y),
            (1, 24, 8),
            "the step lands on the exit tile and stays in the village"
        );
        let stored = registry.info(key).expect("still registered");
        assert_eq!((stored.map_id, stored.x, stored.y), (1, 24, 8));
    }

    /// A town map has exactly two exits, both stamped by the engine at load: the north
    /// `Gate_Wood1ToMain` block and the south `Gate3_MainToBear1` block. Every other
    /// cell of the town — spawn, stairs, centre — is not an exit, on any of the 42
    /// towns, and both gates share one map-0 landing.
    #[test]
    fn a_town_has_exactly_two_exit_gates() {
        let north = (23, 6, 26, 8); // anchor (25,7), FO 1840 `4 3 1 1`
        let south = village_exit2_block();
        assert_eq!(south, (34, 63, 37, 65)); // anchor (35,63), FO 292 `4 3 2 2`

        for map in 1..=42 {
            let mut landing = None;
            // the whole town: `Village01.map` is 63x68, the file every town loads
            for y in 0..68 {
                for x in 0..63 {
                    let is_exit = ((north.0..=north.2).contains(&x)
                        && (north.1..=north.3).contains(&y))
                        || ((south.0..=south.2).contains(&x) && (south.1..=south.3).contains(&y));
                    match village_exit(map, x, y) {
                        Some((village, at)) => {
                            assert!(is_exit, "map {map}: ({x},{y}) is not on either gate");
                            assert_eq!(village.dst_map, map);
                            match landing {
                                None => landing = Some(at),
                                Some(first) => {
                                    assert_eq!(first, at, "map {map}: one landing for both gates")
                                }
                            }
                        }
                        None => assert!(!is_exit, "map {map}: gate cell ({x},{y}) missed"),
                    }
                }
            }
            assert!(landing.is_some(), "map {map}: both gates exit");
        }

        // Neither block leaves a non-town map, and one tile outside either block is a
        // step: corners of the south block plus the spawn (25,13), the tile below it,
        // and the dead-file centre (24,22) the old reach-6 window used to swallow.
        assert!(village_exit(0, 25, 7).is_none());
        assert!(village_exit(0, 35, 63).is_none());
        for (x, y) in [
            (33, 63),
            (38, 63),
            (35, 62),
            (35, 66),
            (34, 66),
            (37, 62),
            (24, 22),
            (25, 13),
            (25, 14),
        ] {
            assert!(village_exit(1, x, y).is_none(), "not an exit: ({x},{y})");
        }
    }

    /// Both gates through the production branch: `warp_out_of_village` moves the durable
    /// row, the cached copy and the registry entry to map 0's road, while any other tile
    /// returns `false` and writes nothing.
    #[tokio::test]
    async fn both_town_exit_gates_warp_to_map_zero_and_other_tiles_walk() {
        for (dest, expected_warp) in [((25, 7), true), ((35, 63), true), ((30, 40), false)] {
            let characters = MemoryCharacters::default();
            let registry = Registry::new();
            let key = Registry::next_key();
            let (tx, _rx) = tokio::sync::mpsc::channel(crate::registry::OUTBOUND_CAP);
            let mut row = row_on_map0(25, 9);
            row.map_id = 1;
            let character_id = row.character_id;
            registry.register(
                key,
                tx,
                std::sync::Arc::new(tokio::sync::Notify::new()),
                session_info_from(&Some(row.clone()), 1473)
                    .expect("the test id is in the player band"),
            );

            let (mut stream, _peer) = socket_pair().await;
            let mut outbound = Cipher::new();
            let items = lazy_items();
            let mut live = LiveFrames {
                items: &items,
                account_id: 0,
                account_name: String::new(),
                character: Some(row.clone()),
                char_unique: 1473,
                friend_list: Vec::new(),
                quest: gz_proto::quest::PersonQuest::new(),
            };

            let warped = warp_out_of_village(
                &mut stream,
                &mut outbound,
                &characters,
                &registry,
                key,
                &mut live,
                1,
                dest,
            )
            .await
            .expect("the order path completes");
            assert_eq!(warped, expected_warp, "order to {dest:?}");

            let cached = live.character.as_ref().expect("still loaded");
            let stored = registry.info(key).expect("still registered");
            if expected_warp {
                // (266, 327): the road just south of Ankaran's entrance block — the
                // one landing both gates share, on map 0.
                assert_eq!(
                    characters.map_writes.lock().unwrap().clone(),
                    vec![(character_id, 0, 266, 327)],
                    "{dest:?}: the map must move with the position"
                );
                assert_eq!((cached.map_id, cached.pos_x, cached.pos_y), (0, 266, 327));
                assert_eq!((stored.map_id, stored.x, stored.y), (0, 266, 327));
            } else {
                assert!(characters.map_writes.lock().unwrap().is_empty());
                assert_eq!((cached.map_id, cached.pos_x, cached.pos_y), (1, 25, 9));
                assert_eq!((stored.map_id, stored.x, stored.y), (1, 25, 9));
            }
        }
    }
}
