//! Map 0's field monsters — Phases 1 and 2 of
//! `docs\DESIGN_MONSTER_COMBAT_AND_AUTOHUNT.md` §10.2/§10.3.
//!
//! The map's `NPCGroup` field groups are loaded (the ones
//! `scripts\generate-npc-placements.ps1` refuses: `Radius != 1` or several units
//! per group), each unit's kind is resolved against the committed kind tables,
//! and every instance is placed on a tile of the group's own walking ground
//! inside its leash ([`gz_data::spawn`]). A placed instance is announced to the
//! clients that can see it as a `GAMEMSG_RESPONSE_LOGINMANY` (6) body with an
//! NPC record — the frame that creates a character on a client
//! (`Client-Login.cpp:53-85`), and the only one that does.
//!
//! [`MonsterWorld::tick`] then keeps them alive: an idle monster wanders inside
//! its leash, an engaged one walks into range and swings, and a dead one comes
//! back after a delay. Both halves broadcast the same frames a player's does —
//! 241 `RESPONSE_USERORDER` for the order and 200 `RESPONSE_CHARUPDATE` per
//! committed tile — so a bystander's client animates a monster exactly the way
//! it animates a player.
//!
//! # What this is not
//!
//! `MaxNPCNum` is only the top-up gate (see [`MonsterWorld::spawn_all`]), not a
//! hard cap. There is no monster-vs-monster combat, no fear or fleeing (the
//! legacy's `ATB_FEAR` branch is unported), and a player's death is left at
//! zero life for the existing revive path to handle — the legacy's
//! `DeadChar_PC` penalty rules are `§11` UNKNOWN. The `GetDamage` defence,
//! elemental, set-item and enchant layers are absent rather than approximated
//! (`§11` marks them mechanical-port-only).

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use gz_data::drops::{DropEntry, roll_drop};
use gz_data::kindinfo::{self, CharAttackRow, KindCreateRow, MonsterStats};
use gz_data::map::MapFile;
use gz_data::npcgroup;
use gz_data::spawn::{SpawnField, SpawnGroup, is_walkable};
use gz_proto::game::{client_map_index, encode_jim, encode_logout};
use gz_proto::loginmany::{LoginManyRecord, NpcRecord, encode_login_many_batches_mixed};
use gz_proto::map_geometry::{cell_of, cells_adjacent};
use gz_proto::opcodes_game::GameMsg;
use gz_proto::order::{BasicCharUpdate, OrderData};

use crate::combat::{self, Rng};
use crate::ids::{self, IdAllocator, MonsterEntityId, SpawnContext, SpawnTier};
use crate::movement::Movement;
use crate::registry::{Registry, SessionKey};
use crate::{logln, logln_err};

/// The one map this phase spawns: the main field (`map.h:67`).
pub(crate) const FIELD_MAP: i32 = 0;

/// How often [`MonsterWorld::tick`] is called, in milliseconds.
///
/// It has to sample a monster's step more than once per tile or the walk runs
/// slow: [`Movement`] commits at most one tile per call, and a tile costs
/// `5000 / move_speed` ms. A shipped monster moves at 5..40, so 100 ms covers
/// every one of them.
pub(crate) const AI_TICK_MS: u64 = 100;

/// One action slot, in milliseconds.
///
/// The legacy runs a character's action once per `CHAR_ACTION_INTERVAL_FRAME`
/// frames (`Char-Common.h:221` = 6) at 50 ms a frame — `char-move.cpp:145`
/// derives its 350 ms move cap as `(CHAR_ACTION_INTERVAL_FRAME + 1) * 50` — and
/// `cltCharCommon::Action()` returns unless `GetID() % 6 == GetFrame() % 6`
/// (`Char-Common.cpp:1476-1480`). So a character, monster included, gets one
/// action every **300 ms**.
pub(crate) const AI_SLOT_MS: u64 = 300;

/// The idle decision is a `1 / AI_DECIDE_ONE_IN` draw **per action slot**, not a
/// deadline.
///
/// `char-wait.cpp:342` is
/// `data = (rand() + siID) % (150 / CHAR_ACTION_INTERVAL_FRAME)` (= `% 25`) and
/// `if (data == 0)` at `:344` is the whole gate. `rand()` there is a fresh draw,
/// not a frame counter, so the interval is **geometric**: mean `25 * 300 ms` =
/// **7,500 ms**, which is also `150 frames * 50 ms` — the `150` in the source is
/// that same 7.5 s window expressed in frames, and the two routes agreeing is
/// the check that the reading is right. `siID` only decorrelates each
/// character's random stream; it is not a timer offset.
///
/// The previous `AI_ACT_MS = 6_250` was `25 * 250 ms`: the wrong slot length
/// *and* a fixed beat in place of a draw, which is why idle monsters could sit
/// frozen and then all re-decide together.
///
/// It gates **wandering and the enemy search only**. A monster with a target
/// runs on [`AI_TICK_MS`] instead, and its swing on its own
/// [`SpawnUnit::attack_interval`] — see [`MonsterWorld::tick`].
const AI_DECIDE_ONE_IN: u32 = 25;

/// How far a monster will step toward a wander target, in tiles. The legacy
/// picks ±5 inside a group and ±10 outside (`char-wait.cpp:202-300`); ours
/// always has a group, and the leash bounds it either way, so ±5 is the whole
/// rule.
const WANDER_RADIUS: i32 = 5;

/// How far an occupied spawn pick may search outward for free, walkable,
/// in-leash ground — Cat's zero-collision spawner rule (2026-10-21): a
/// stride pick a live monster already stands on falls through the unit's
/// own safe tiles and then to rings of this radius around the pick. Every
/// shipped group leash is at least this wide, so the search stays inside
/// the group's square.
const SPAWN_EXPAND_RADIUS: i32 = 5;

/// One group's kind, resolved against the kind tables and the map.
#[derive(Debug, Clone)]
struct SpawnUnit {
    /// `uiMakeCharKind` — the `KindInfo_Unique.txt` id.
    wire_kind: u16,
    /// The `NPCGroup` unit's `Level`.
    level: i32,
    /// How many of this kind the group holds (`siKindMaxNum`).
    max_num: i32,
    /// `uiTotalMoveSpeed` (the Create file's `movespeed`, 6 bits).
    move_speed: u8,
    /// `uiLife` — the kind's max life at `level` (`MonsterStats::at`).
    life: u32,
    /// The group's safe tiles for this kind's block mask, from
    /// [`SpawnField::spawn_points`]. Sorted by `(x, y)`, which is what makes
    /// [`walkable_path`] and the wander scan a binary search.
    tiles: Vec<(i32, i32)>,
    /// `MinDmg` — the low end of this kind's swing (`CharAttackRow::min_dmg`).
    min_atk: i32,
    /// `MaxDmg` (`CharAttackRow::max_dmg`).
    max_atk: i32,
    /// `AC` (`CharAttackRow::ac`) — the defence a player's swing is reduced by
    /// ([`combat::reduced_by_ac`]).
    ac: i32,
    /// `Exp` (`CharAttackRow::exp`) — the pool [`combat::exp_for_hit`] shares
    /// out per hit.
    exp: i64,
    /// `siAggressive` (`CharAttackRow::aggressive`) — how far this kind looks
    /// for a target. 0 means it never starts a fight.
    aggressive: i32,
    /// `range` (`CharAttackRow::range`) — how close it has to be to swing.
    range: i32,
    /// `Attack_Interval` ([`combat::attack_interval_ms`]) — this kind's swing
    /// cadence, which is also the clock its client-side animation repeats on.
    attack_interval: u64,
    /// `siCriticalRate`, percent (`CharAttackRow::critical_rate`).
    critical_rate: i32,
    /// This kind's AGI — the `dex` column of its [`MonsterStats`] (the ON
    /// branch passes the raw `clBA` value through). Drives hit and dodge both
    /// ways under Cat's §4.3.2 reformation: it is the dodge a player's swing
    /// is tested against and the accuracy of this monster's own swings.
    dex: i32,
    /// `(Str+Dex+Mag+Vit)/2` over this unit's [`MonsterStats`]
    /// (`GetBattlePower`, `Parametabox.cpp:4162`) — the enemy half of
    /// `GetExp`'s weak-monster gate (`Server-Attack.cpp:4003`).
    battle_power: i32,
    /// This kind's drop table, resolved once from the committed canonical
    /// master so a death never re-looks the kind up.
    drops: &'static [DropEntry],
}

/// One field group plus its resolved, placeable units.
#[derive(Debug, Clone)]
struct FieldGroup {
    anchor: SpawnGroup,
    /// `MaxNPCNum` — the group's capacity. The legacy gates the whole group on
    /// its live count once per tick and then fills each unit to its target
    /// (`NPC.cpp:922-938`), so this is a top-up gate, **not** a hard cap: a
    /// group whose unit targets sum above it overshoots it by design.
    capacity: i32,
    /// `BOSS` — non-zero in the shipped file. A boss group's dead monster
    /// returns on the group's own [`FieldGroup::respawn_ms`] rather than the
    /// fixed [`RESPAWN_MS`] stand-in (`NPC.cpp:862-888`).
    boss: bool,
    /// `Interval` — the file's seconds converted to milliseconds at load, the
    /// one conversion the legacy does in the group constructor
    /// (`NPC.cpp:223`: `interval * 1000`). 0 when the file carries none, which
    /// is most field groups.
    respawn_ms: u64,
    units: Vec<SpawnUnit>,
}

/// One live monster instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Monster {
    /// `uiMakeCharUnique` — a [`ids::StandardMonsterId`] in H3's band (never a
    /// static NPC's id, never a session's), or a [`MonsterEntityId::Tier3`]
    /// once P3 opens the extended band. The type is what keeps that claim
    /// checkable at every call site (design doc §5.0).
    unique: MonsterEntityId,
    wire_kind: u16,
    /// Cat's internal log tag (2026-10-21): this kind's nth instance minted
    /// this boot — `[032_01]` for the first raccoon ([`Monster::tag`]).
    /// Server-side logging only; packets carry [`Monster::wire_kind`] and
    /// nothing else.
    instance: u16,
    level: u8,
    /// Current life. Reaches 0 only long enough for the death path to remove
    /// the instance, because a body is deleted rather than left lying
    /// (`CharManager.cpp:257-277`).
    life: u32,
    x: i32,
    y: i32,
    move_speed: u8,
    /// Index into [`MonsterWorld::groups`] — the group this instance belongs to,
    /// so a tick can count it against the group's capacity.
    group: usize,
    /// Index into that group's `units`, which is where the combat numbers live.
    unit: usize,
    /// The `char_unique` of the player this monster is fighting, or `None` when
    /// it is idle. The legacy's `siTargetCharUnique`.
    target: Option<i16>,
    /// The `char_unique` of the player this monster is *jim-ed* by, or `None`
    /// until someone lands a hit. The legacy's `cltJim::siJimHostCharUnique`
    /// (`Char-Common.h:436`).
    ///
    /// Independent of [`Monster::target`] and far more stubborn: the first
    /// player to hit takes it and it is **not** released when combat ends —
    /// only death, the host switching its own bar to another monster, or
    /// another player displacing it clears it, and `SetBeJimed` refuses a
    /// second host outright (`Char-Common.cpp:6226`).
    /// It is also the only thing that can raise an ordinary mob's overhead
    /// health bar, which is the whole reason it is worth carrying here — see
    /// [`crate::game::encode_jim`].
    jim_host: Option<i16>,
    /// A walk in progress — usually the walk into range this monster's target
    /// stands at. It outlives the `target`, so the two are held at once.
    walk: Option<Movement>,
    /// The `now_tick()` stamp this monster next acts on — the legacy's
    /// `(rand() + siID) % 25` gate, as a deadline.
    next_act_ms: u64,
    /// The `now_tick()` stamp this monster next *swings* on: its kind's
    /// `Attack_Interval`, as a deadline (`char-attack.cpp:1869`). A separate
    /// clock from `next_act_ms` because the legacy separates them too — the
    /// wander/search decision is every-25-frames, the swing is the kind's own
    /// interval — and the client animates the swing on this one.
    next_swing_ms: u64,
}

impl Monster {
    /// The server-side log tag — `[<kind>_<instance>]`, e.g. `[032_01]` for
    /// the first raccoon (Cat 2026-10-21). For humans reading a log; never
    /// serialized, never on the wire.
    fn tag(&self) -> String {
        format!("[{:03}_{:02}]", self.wire_kind, self.instance)
    }
}

/// What a monster's swing did to a player, for the caller to broadcast.
/// [`crate::main`] does the broadcasting because it owns the registry fan and
/// this method must not hold the world lock across an await.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlayerHit {
    /// The struck player's session key.
    pub(crate) key: SessionKey,
    /// Its `char_unique`, the field the 268 carries.
    pub(crate) char_unique: i16,
    /// Where it was standing — the recipient set is "near the target"
    /// (`Server-Attack.cpp:2775`).
    pub(crate) x: i32,
    pub(crate) y: i32,
    /// Life after the hit.
    pub(crate) life: u32,
    pub(crate) damage: i32,
    /// The swinging monster's unique, so the caller can echo the 241 order.
    pub(crate) attacker_unique: u16,
    /// Hit, dodge or miss — Cat's §4.3.2 AGI roll. A dodge or miss carries
    /// zero damage and the unchanged life; the caller maps it onto the
    /// 268's `b_dodge_hit` / `b_miss_hit` flags.
    pub(crate) outcome: combat::SwingOutcome,
}

/// What a player's swing did to a monster: the 268 body's fields, plus what a
/// death owes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MonsterHit {
    pub(crate) unique: u16,
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) life: u32,
    pub(crate) died: bool,
    /// The damage actually applied — after [`combat::reduced_by_ac`] took the
    /// monster's own defence out of the swing.
    pub(crate) damage: i32,
    /// The kind's `Exp` column — the pool the caller shares out with
    /// [`combat::exp_for_hit`].
    pub(crate) exp: i64,
    pub(crate) level: i32,
    pub(crate) max_life: u32,
    /// The kind's battle power at these stats — the enemy half of `GetExp`'s
    /// weak-monster gate (`Server-Attack.cpp:4003`).
    pub(crate) battle_power: i32,
    /// `(item_unique, count)` pairs rolled at death, empty on a survivor.
    pub(crate) drops: Vec<(i32, i32)>,
    /// Hit, dodge or miss — Cat's §4.3.2 AGI roll, resolved in
    /// [`crate::combat::player_swing`] against this kind's [`SpawnUnit::dex`].
    pub(crate) outcome: combat::SwingOutcome,
}

/// How long a map with nobody on it stays ticked before it hibernates, in
/// milliseconds.
///
/// The recorded decision (docs/DESIGN_MAP_DORMANCY.md §3.1) is anti-flapping
/// hysteresis: the last player leaving does **not** freeze the map — warps
/// bounce and clients sit in loading screens — so the map counts down 30 s and
/// only then stops being sampled. A player entering inside the window discards
/// the countdown.
const DORMANCY_GRACE_MS: u64 = 30_000;

/// One map's sleep state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MapSleep {
    /// Players present (or just arrived).
    Awake,
    /// Nobody on the map, counting down to hibernation.
    Grace { deadline_ms: u64 },
    /// Nobody for the whole grace window: the tick loop skips the map.
    Hibernating,
}

/// Which maps are being watched, and which may sleep.
///
/// Encapsulates the whole dormancy state machine so the monster loop stays a
/// consumer: [`MonsterWorld`] owns one manager, feeds it the per-map player
/// counts once per tick through [`Self::update`], and asks [`Self::is_dormant`]
/// before sampling a monster. No timestamp math or state check leaks across
/// that boundary, and the manager is testable on its own — no world, no
/// registry, no network.
///
/// Every deadline is an absolute timestamp in the house style (`next_act_ms`,
/// `respawn`): there is no timer object and nothing to cancel — a player
/// entering simply moves the map back to [`MapSleep::Awake`].
#[derive(Debug, Default)]
pub(crate) struct MapDormancyManager {
    maps: HashMap<i32, MapSleep>,
}

impl MapDormancyManager {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Advances one map's sleep state from the number of players on it.
    ///
    /// An unseen map defaults to [`MapSleep::Awake`], so a map with players is
    /// always awake, and a map whose very first observation has nobody on it
    /// opens its grace window rather than freezing outright.
    pub(crate) fn update(&mut self, map_id: i32, player_count: usize, now_ms: u64) {
        let state = self.maps.entry(map_id).or_insert(MapSleep::Awake);
        *state = if player_count > 0 {
            // Anybody home: awake, and any countdown is simply discarded.
            MapSleep::Awake
        } else {
            match *state {
                // The last player left: open the grace window.
                MapSleep::Awake => MapSleep::Grace {
                    deadline_ms: now_ms + DORMANCY_GRACE_MS,
                },
                // Still counting down: hold the deadline...
                MapSleep::Grace { deadline_ms } if now_ms < deadline_ms => {
                    MapSleep::Grace { deadline_ms }
                }
                // ...or the window ran out with nobody coming back.
                MapSleep::Grace { .. } | MapSleep::Hibernating => MapSleep::Hibernating,
            }
        };
    }

    /// True when the map is past its grace window with nobody on it. A map the
    /// manager has never seen is awake by definition.
    #[must_use]
    pub(crate) fn is_dormant(&self, map_id: i32) -> bool {
        matches!(self.maps.get(&map_id), Some(MapSleep::Hibernating))
    }
}

/// Map 0's monster population: the loaded groups and the instances they spawned.
#[derive(Debug, Default)]
pub(crate) struct MonsterWorld {
    groups: Vec<FieldGroup>,
    monsters: Vec<Monster>,
    /// The id pools behind [`Self::spawn_one`] (design doc §3.5): the standard
    /// band with the T1 ↔ T2 borrow, and the isolated Tier-3 window. Replaced
    /// batch-2's `next_unique`/`free` pair wholesale; the pools still bound
    /// *simultaneous* monsters rather than lifetime kills, because an id is
    /// freed only at corpse removal — after its 11 has gone out — so a client
    /// still drawing the death animation never sees the id reused (audit H3,
    /// the legacy's `FindEmptyCharUnique`, `Char-Server.h:1006-1037`).
    ids: IdAllocator,
    /// `kind -> how many of that kind this boot has minted` — the per-kind
    /// counter behind [`Monster::instance`] (Cat's log tag, 2026-10-21).
    kind_seq: HashMap<u16, u16>,
    /// `(group index, when it is due)` — the legacy's respawn gate, one entry
    /// per monster that has died.
    respawn: Vec<(usize, u64)>,
    /// `(the monster, when its corpse's 11 is due)` — the dead still on the map
    /// while their death animation plays. Held out of [`Self::monsters`] so a
    /// corpse cannot be walked into, targeted, or announced to someone who
    /// walks in late.
    corpses: Vec<(Monster, u64)>,
    /// `attacker char_unique -> the monster it last jIMMed` — who has an
    /// overhead bar up, per attacker, so a swing that switches monsters can
    /// take the old bar down again (the un-jim in [`Self::hit_by_player`]).
    /// At most one entry per attacker; a new jim replaces the old one.
    jim_by: HashMap<i16, MonsterEntityId>,
    /// Map 0 itself, so the ground can be judged for a *player* and not only for
    /// a spawning monster. `None` in [`MonsterWorld::empty`], which has no ground
    /// either — nothing can be approached from an unloaded world.
    map: Option<MapFile>,
    /// `KindInfo_Unique` id -> that kind's `KindInfo_Create` block mask. The
    /// attacker's own class decides what it may step onto, and only the world
    /// load sees both tables at once.
    block_by_kind: HashMap<u16, u16>,
    /// `KindInfo_Unique` id -> that kind's display name, from the canonical
    /// drop table's `Name` column — the death frame's killer name
    /// ([`Self::monster_display_name`]). `""` for a kind the table names not.
    name_by_kind: HashMap<u16, &'static str>,
    /// Which maps are being watched and which may sleep — the dormancy state
    /// machine behind the tick loop's skip (docs/DESIGN_MAP_DORMANCY.md).
    dormancy: MapDormancyManager,
}

impl MonsterWorld {
    /// An empty world — what a failed load degrades to, so a missing data tree
    /// does not stop the server (the same posture the quest load takes).
    #[must_use]
    pub(crate) fn empty() -> Self {
        // The default allocator seeds its floor above the static-NPC block,
        // exactly like `load`, so a hand-built world can never mint a static
        // NPC's id or a player's (H3).
        Self::default()
    }

    /// Load map 0's field groups and resolve every unit's kind and safe ground.
    ///
    /// # Errors
    ///
    /// A message naming the file when the map, the group tree, or a kind table
    /// cannot be read. A unit whose kind is missing from a table, has no safe
    /// tile in its group, or would spawn at zero life is logged and skipped
    /// rather than failing the load — a partly-populated map beats none.
    pub(crate) fn load(root: &Path) -> Result<Self, String> {
        let map_path = root.join("Map/MainMap/MainMap.map");
        let map = MapFile::load(&map_path).map_err(|e| format!("monsters: {map_path:?}: {e}"))?;

        let groups_dir = root.join("NPCGroup");
        let groups = npcgroup::load_npc_groups(&groups_dir, FIELD_MAP)
            .map_err(|e| format!("monsters: {groups_dir:?}: {e}"))?;

        let create = kindinfo::load_kind_create(root).map_err(|e| format!("monsters: {e}"))?;
        let unique = kindinfo::load_kind_unique(root).map_err(|e| format!("monsters: {e}"))?;
        let attack = kindinfo::canonical_char_attack();
        let drops = gz_data::drops::canonical_char_drops();

        let create_by_code: HashMap<&str, &KindCreateRow> =
            create.iter().map(|r| (r.code.as_str(), r)).collect();
        let id_by_code: HashMap<&str, u16> =
            unique.iter().map(|r| (r.code.as_str(), r.id)).collect();
        let attack_by_code: HashMap<&str, &CharAttackRow> =
            attack.iter().map(|r| (r.code.as_str(), r)).collect();
        let drops_by_code: HashMap<&str, &[DropEntry]> = drops
            .iter()
            .map(|r| (r.code.as_str(), r.drops.as_slice()))
            .collect();
        let name_by_code: HashMap<&str, &str> = drops
            .iter()
            .map(|r| (r.code.as_str(), r.name.as_str()))
            .collect();
        // The killer's display name for the death frame: every kind id gets an
        // entry, `""` where the drop table carries no name, so a lookup at
        // death time cannot fail on a data gap.
        let name_by_kind: HashMap<u16, &'static str> = unique
            .iter()
            .map(|u| {
                (
                    u.id,
                    name_by_code.get(u.code.as_str()).copied().unwrap_or(""),
                )
            })
            .collect();

        // One connectivity flood per kind mask, shared by every group of that
        // kind (the expensive half of `spawn_points`).
        let mut fields: HashMap<u16, SpawnField> = HashMap::new();
        let mut out = Vec::with_capacity(groups.len());
        for group in &groups {
            let anchor = SpawnGroup {
                map_index: group.map_index,
                center_x: group.center_x,
                center_y: group.center_y,
                radius: group.radius,
            };
            // Cat's density fix (2026-10-21): the ×3 is for the field. A group
            // the static-placement generator takes verbatim — `Radius` 1 with
            // every unit at `MinNum` 1 (`generate-npc-placements.ps1` refuses
            // anything else) — is a town NPC: a trader, a quest giver, a store
            // clerk. Those keep their shipped count, so a town stays one copy
            // of each NPC instead of three stacked.
            let density = if group.radius == 1 && group.units.iter().all(|u| u.max_num == 1) {
                1
            } else {
                DENSITY_MULTIPLIER
            };
            let mut units = Vec::with_capacity(group.units.len());
            for unit in &group.units {
                let (Some(create_row), Some(&wire_kind), Some(attack_row)) = (
                    create_by_code.get(unit.kind.as_str()).copied(),
                    id_by_code.get(unit.kind.as_str()),
                    attack_by_code.get(unit.kind.as_str()).copied(),
                ) else {
                    // Report what is missing so a data gap is one log line, not
                    // a silent hole in the map.
                    if !create_by_code.contains_key(unit.kind.as_str()) {
                        logln_err!("monsters: kind {} has no KindInfo_Create row", unit.kind);
                    } else if !id_by_code.contains_key(unit.kind.as_str()) {
                        logln_err!("monsters: kind {} has no KindInfo_Unique id", unit.kind);
                    } else {
                        logln_err!("monsters: kind {} has no CharAttackInfo row", unit.kind);
                    }
                    continue;
                };
                // `Switch_HealthAddChange` is `1` in every shipped
                // `Data\CountrySwitch.txt`, and the client's `clPB.GetMaxLife()`
                // gates the bar's denominator on that same switch for NPCs as
                // well as players (`Parametabox.cpp:7268`) — so the pair has to be
                // the one the client uses, not the legacy's older branch. This
                // also matches the player path (`stats::derived_max_life`), which
                // was aligned to this same `×5` level term in the 2026-10-21
                // HUD-bar fix; the §4.3.4 `×50` had desynced player life from the
                // client-computed max and pinned the player's own bar full.
                // shortcut: hardcoded rather than read from CountrySwitch.txt; read
                // the file if a data set ships `Switch_HealthAddChange 0`.
                let stats = MonsterStats::at(create_row, attack_row, unit.level, true);
                let Ok(life) = u32::try_from(stats.max_hp) else {
                    logln_err!("monsters: kind {} has negative life", unit.kind);
                    continue;
                };
                if life == 0 {
                    // The server refuses a zero-life record (`char.cpp:9375`).
                    logln_err!("monsters: kind {} spawns at zero life", unit.kind);
                    continue;
                }
                let field = fields
                    .entry(create_row.block)
                    .or_insert_with(|| SpawnField::new(&map, create_row.block));
                let tiles = field.spawn_points(&anchor);
                if tiles.is_empty() {
                    logln_err!(
                        "monsters: group {} (kind {}, centre {},{}) has no safe spawn tile",
                        group.id,
                        unit.kind,
                        group.center_x,
                        group.center_y
                    );
                    continue;
                }
                units.push(SpawnUnit {
                    wire_kind,
                    level: unit.level,
                    max_num: unit.max_num.saturating_mul(density),
                    move_speed: stats.move_speed.clamp(0, 63) as u8,
                    life,
                    tiles,
                    min_atk: attack_row.min_dmg,
                    max_atk: attack_row.max_dmg.max(attack_row.min_dmg),
                    ac: attack_row.ac,
                    exp: attack_row.exp,
                    aggressive: attack_row.aggressive,
                    range: attack_row.range.max(1),
                    attack_interval: combat::attack_interval_ms(attack_row.attack_interval),
                    critical_rate: attack_row.critical_rate,
                    // §4.3.2: this kind's AGI — hit and dodge, both ways.
                    dex: stats.dex,
                    // The enemy half of `GetExp`'s weak-monster gate, from the
                    // same stats the unit fights with.
                    battle_power: stats.battle_power(),
                    // A kind with no drop row drops nothing; the parser already
                    // dropped malformed entries, so this is a plain lookup.
                    drops: drops_by_code
                        .get(unit.kind.as_str())
                        .copied()
                        .unwrap_or(&[]),
                });
            }
            if !units.is_empty() {
                out.push(FieldGroup {
                    anchor,
                    capacity: group.max_npc_num.saturating_mul(density),
                    boss: group.boss,
                    // The file carries seconds; the legacy converts once at
                    // load (`NPC.cpp:223`). A malformed negative is 0.
                    respawn_ms: u64::try_from(group.interval).unwrap_or(0) * 1_000,
                    units,
                });
            }
        }
        // The block mask is the one kind column this world has to resolve for a
        // *player*: `KindInfo_Create` is what says which tiles a class may stand
        // on, and only here are the id table and the rows already together.
        let block_by_kind: HashMap<u16, u16> = unique
            .iter()
            .filter_map(|u| Some((u.id, create_by_code.get(u.code.as_str())?.block)))
            .collect();

        Ok(Self {
            groups: out,
            monsters: Vec::new(),
            // The pools start above the static-NPC block, so the two id
            // ranges cannot overlap (`IsNPC` covers both, `npc.rs`).
            ids: IdAllocator::new(),
            kind_seq: HashMap::new(),
            respawn: Vec::new(),
            corpses: Vec::new(),
            jim_by: HashMap::new(),
            map: Some(map),
            block_by_kind,
            name_by_kind,
            dormancy: MapDormancyManager::new(),
        })
    }

    /// The number of live monsters.
    #[must_use]
    pub(crate) fn monster_count(&self) -> usize {
        self.monsters.len()
    }

    /// Spawns every loaded group toward its capacity, and queues each new
    /// monster to the sessions already near it — the live half of the spawn
    /// broadcast (none at boot; a caller that reloads later gets real
    /// recipients).
    ///
    /// One pass is one spawn tick of the legacy's `SetChar` loop: a group at or
    /// above `MaxNPCNum` is skipped whole, otherwise every unit is filled to its
    /// `siKindMaxNum` (`NPC.cpp:922-938`).
    pub(crate) fn spawn_all(&mut self, registry: &Registry) {
        for index in 0..self.groups.len() {
            let live = self.monsters.iter().filter(|m| m.group == index).count() as i32;
            if live >= self.groups[index].capacity {
                continue;
            }
            for unit_index in 0..self.groups[index].units.len() {
                let unit = &self.groups[index].units[unit_index];
                let want = unit.max_num.max(0) as usize;
                // The stride is a *preference*, not the placement: a
                // deterministic first pick across the sorted safe tiles (not
                // the legacy's random `FindEmptyAreaForCharLocate`), and
                // [`Self::open_spawn_tile`] then enforces Cat's
                // one-tile-one-monster rule (2026-10-21). The old stride
                // wrapped once a target outran the tile list and stacked 37%
                // of the map on shared tiles; that is gone.
                let step = (unit.tiles.len() / want.max(1)).max(1);
                let places: Vec<(i32, i32)> = (0..want)
                    .map(|k| unit.tiles[(k * step) % unit.tiles.len()])
                    .collect();
                // The leash, enforced where it can be: `spawn_points` only
                // returns the group's square, and this states the invariant
                // rather than trusting it.
                debug_assert!(
                    places
                        .iter()
                        .all(|&(x, y)| self.groups[index].anchor.contains(FIELD_MAP, x, y)),
                    "monster placed outside its group's leash"
                );
                // Resolved above so the mutable borrow below is the only one.
                for base in places {
                    // Cat's zero-collision rule (2026-10-21): never place on
                    // a tile a live monster already stands on.
                    let Some(at) = self.open_spawn_tile(index, unit_index, base) else {
                        logln_err!(
                            "monsters: no free tile near ({}, {}) for group {index} \
                             unit {unit_index} — spawn refused",
                            base.0,
                            base.1
                        );
                        continue;
                    };
                    if !self.spawn_one(registry, index, unit_index, at) {
                        return;
                    }
                }
            }
        }
        // Boot assertion of the zero-collision rule (Cat's directive,
        // 2026-10-21): one tile holds exactly one monster. The data test
        // pins this deterministically; this catches a live data change.
        let mut per_tile: HashMap<(i32, i32), u32> = HashMap::new();
        for m in &self.monsters {
            *per_tile.entry((m.x, m.y)).or_default() += 1;
        }
        let collisions: u32 = per_tile.values().filter(|&&n| n > 1).sum();
        if collisions > 0 {
            logln_err!(
                "monsters: {collisions} monster(s) share a spawn tile — the \
                 one-tile-one-monster rule is broken"
            );
        } else {
            logln!(
                "game: monsters: {} monster(s) on {} distinct tile(s) — 0 tile \
                 collisions",
                self.monsters.len(),
                per_tile.len()
            );
        }
    }

    /// Whether any live monster stands on `t` — one membership test over
    /// every group and unit, which is the whole one-tile-one-monster rule.
    fn occupied(&self, t: (i32, i32)) -> bool {
        self.monsters.iter().any(|m| (m.x, m.y) == t)
    }

    /// The runtime half of the one-tile-one-monster rule: whether any *other*
    /// monster already holds `t` as its effective slot — the tile it is walking
    /// to if it walks, else the tile it stands on.
    ///
    /// [`Self::occupied`] covers the boot fill; this covers movement. Gating
    /// every walk destination on it is what keeps an idle `wander` or an
    /// engage-approach from stacking two monsters on one tile — the click-bounce
    /// sibling (two damaged monsters on the same cell alternate their life bar
    /// under `FindLandChar`) that a spawn-time check can never see, because the
    /// overlap is acquired after the fill. Reserving the walk *destination*
    /// (not the tile being left) means two monsters deciding in the same AI pass
    /// cannot both commit to the same tile: the first's reservation is visible
    /// to the second.
    fn slot_held(&self, except: usize, t: (i32, i32)) -> bool {
        self.monsters
            .iter()
            .enumerate()
            .any(|(i, m)| i != except && m.walk.as_ref().map(|w| w.dest).unwrap_or((m.x, m.y)) == t)
    }

    /// Cat's zero-collision placement (2026-10-21): the first tile nothing
    /// stands on — `base`, then the unit's own sorted safe tiles, then
    /// walkable tiles on expanding rings around `base` ("dynamically search
    /// an unoccupied adjacent walkable tile or expand the candidate
    /// radius"). Ring candidates stay inside the group's leash and are
    /// judged with the same kind block [`Self::approach_tile`] uses. `None`
    /// only when the whole neighbourhood is taken — the caller then refuses
    /// that one spawn, logged, never a panic (A8).
    fn open_spawn_tile(&self, group: usize, unit: usize, base: (i32, i32)) -> Option<(i32, i32)> {
        if !self.occupied(base) {
            return Some(base);
        }
        for &t in &self.groups[group].units[unit].tiles {
            if !self.occupied(t) {
                return Some(t);
            }
        }
        let u = &self.groups[group].units[unit];
        let block = *self.block_by_kind.get(&u.wire_kind)?;
        let map = self.map.as_ref()?;
        for r in 1..=SPAWN_EXPAND_RADIUS {
            for dy in -r..=r {
                for dx in -r..=r {
                    // The tiles a smaller ring already covered are interior.
                    if dx.abs().max(dy.abs()) != r {
                        continue;
                    }
                    let t = (base.0 + dx, base.1 + dy);
                    if self.groups[group].anchor.contains(FIELD_MAP, t.0, t.1)
                        && is_walkable(map, t.0, t.1, block)
                        && !self.occupied(t)
                    {
                        return Some(t);
                    }
                }
            }
        }
        None
    }

    /// Places one monster into `group` at `at`, taking the next id and
    /// announcing it. `false` when the id pools are spent, which stops the
    /// caller's whole loop.
    ///
    /// Which pool an id comes from is [`ids::spawn_tier`]'s answer (design doc
    /// §3.3): today every group is a field group and `tier3_level_floor` is
    /// `None`, so every spawn is Tier 1 — the structure is in place and the
    /// switch stays off until the P3 client rebuild (§4). Exhaustion mirrors
    /// the legacy's `FindEmptyCharUnique` returning 0: the spawn is refused
    /// and logged, never a panic (A8).
    fn spawn_one(
        &mut self,
        registry: &Registry,
        group: usize,
        unit: usize,
        at: (i32, i32),
    ) -> bool {
        let u = &self.groups[group].units[unit];
        // Every group this loader produces is a field group (map 0); a dungeon
        // map's groups prefer Tier 2 when multi-map loading arrives — P1 ships
        // the routing rule, not a dungeon to apply it to (§3.3).
        let context = if self.groups[group].anchor.map_index == FIELD_MAP {
            SpawnContext::WorldMap
        } else {
            SpawnContext::Dungeon
        };
        let tier = ids::spawn_tier(u.level, context, self.ids.tier3_level_floor());
        let unique = match tier {
            SpawnTier::Tier1 => self.ids.alloc_tier1().map(MonsterEntityId::Standard),
            SpawnTier::Tier2 => self.ids.alloc_tier2().map(MonsterEntityId::Standard),
            // Tier 3 exhausted falls back to the context's standard tier
            // rather than refusing outright (§3.3); unreachable while the P1
            // floor is `None`.
            SpawnTier::Tier3 => self
                .ids
                .alloc_tier3()
                .map(MonsterEntityId::Tier3)
                .or_else(|| match context {
                    SpawnContext::WorldMap => self.ids.alloc_tier1().map(MonsterEntityId::Standard),
                    SpawnContext::Dungeon => self.ids.alloc_tier2().map(MonsterEntityId::Standard),
                }),
        };
        let Some(unique) = unique else {
            logln_err!(
                "monsters: id pools exhausted at {} monster(s)",
                self.monsters.len()
            );
            return false;
        };
        // Cat's log tag (2026-10-21): this kind's nth instance this boot.
        let instance = {
            let next = self.kind_seq.entry(u.wire_kind).or_insert(0);
            *next = next.saturating_add(1);
            *next
        };
        let monster = Monster {
            unique,
            wire_kind: u.wire_kind,
            instance,
            level: u.level.clamp(0, 255) as u8,
            life: u.life,
            x: at.0,
            y: at.1,
            move_speed: u.move_speed,
            group,
            unit,
            target: None,
            jim_host: None,
            walk: None,
            // No stagger here: the legacy's `+ siID` term only decorrelates
            // the random streams, which the per-slot draw already does.
            next_act_ms: 0,
            next_swing_ms: 0,
        };
        self.monsters.push(monster);
        broadcast_spawn(registry, FIELD_MAP, &monster);
        true
    }

    /// Cat's internal log tag (2026-10-21) — `[032_01]` for the first
    /// raccoon — for log lines that carry only the unique. A bare `#<unique>`
    /// when the world does not hold the monster (died, or a session's id).
    pub(crate) fn log_tag(&self, unique: u16) -> String {
        self.monsters
            .iter()
            .find(|m| m.unique.u16() == unique)
            .map_or_else(|| format!("#{unique}"), Monster::tag)
    }

    /// Tells one session about every monster in its 3×3 cell block — the monster
    /// half of `cltServerMap::SendNewOthersInfoToPerson` (`ServerMap.cpp:73-150`).
    /// A no-op on any map this world does not hold.
    pub(crate) fn announce_near(
        &self,
        registry: &Registry,
        key: SessionKey,
        map_id: i32,
        x: i32,
        y: i32,
    ) {
        self.announce(registry, key, map_id, &format!("near ({x},{y})"), |m| {
            near(x, y, m.x, m.y, map_id)
        });
    }

    /// Tells one session about the monsters in the cells it has just entered —
    /// the cell-crossing half of the near mechanism, so a player walking across
    /// the field meets new monsters instead of only the ones near where they
    /// logged in.
    ///
    /// Only the cells that were *not* in `was_cell`'s block: re-sending a
    /// monster the client already holds makes it delete and re-`SetChar` the
    /// avatar (`Client-SetChar.cpp:111-119`), the non-idempotency
    /// [`crate::announce_nearby_characters`] documents for players — harmless in
    /// position for a stationary monster, but a redraw with no cause.
    pub(crate) fn announce_new_cells(
        &self,
        registry: &Registry,
        key: SessionKey,
        map_id: i32,
        was_cell: (i32, i32),
        now_cell: (i32, i32),
    ) {
        self.announce(registry, key, map_id, "newly in view", |m| {
            let at = cell_of(m.x, m.y, map_id);
            cells_adjacent(now_cell, at) && !cells_adjacent(was_cell, at)
        });
    }

    /// Queues one 6 body per batch of the monsters `keep` selects, `what`
    /// naming the selection for the log line.
    fn announce(
        &self,
        registry: &Registry,
        key: SessionKey,
        map_id: i32,
        what: &str,
        keep: impl Fn(&Monster) -> bool,
    ) {
        if map_id != FIELD_MAP {
            return;
        }
        let records: Vec<LoginManyRecord> = self
            .monsters
            .iter()
            .filter(|m| keep(m))
            .map(|m| LoginManyRecord::Npc(record_for(map_id, m)))
            .collect();
        if records.is_empty() {
            return;
        }
        let count = records.len();
        let mut frames = 0;
        for body in encode_login_many_batches_mixed(&records) {
            registry.send_to(key, GameMsg::ResponseLoginmany as u16, body);
            frames += 1;
        }
        logln!("game: announced {count} monster(s) {what} on map {map_id} in {frames} frame(s)");
    }

    /// One AI pass: respawn what is due, step every walk that is running, then
    /// let each monster whose turn it is act.
    ///
    /// Returns what the monsters did to players, so the caller can broadcast it
    /// once the world lock is dropped.
    pub(crate) fn tick(
        &mut self,
        registry: &Registry,
        now_ms: u64,
        rng: &mut Rng,
    ) -> Vec<PlayerHit> {
        if self.monsters.is_empty() && self.respawn.is_empty() && self.corpses.is_empty() {
            return Vec::new();
        }
        // Map dormancy first (docs/DESIGN_MAP_DORMANCY.md): advance every
        // group's map through the sleep state machine from the live player
        // counts, then build the awake mask the loop reads. A hibernating map
        // is skipped wholesale below — no draws, no walks, no deadline
        // evaluation — and its timestamps simply wait; the first tick after a
        // player enters fires whatever came due.
        let map_counts = registry.online_map_counts();
        let mut group_awake = vec![true; self.groups.len()];
        for (group_index, group) in self.groups.iter().enumerate() {
            self.dormancy.update(
                group.anchor.map_index,
                map_counts
                    .get(&group.anchor.map_index)
                    .copied()
                    .unwrap_or(0),
                now_ms,
            );
            group_awake[group_index] = !self.dormancy.is_dormant(group.anchor.map_index);
        }
        self.despawn_corpses(registry, now_ms);
        self.respawn_due(registry, now_ms);
        self.advance_walks(registry, now_ms);
        let mut hits = Vec::new();
        for index in 0..self.monsters.len() {
            // A hibernating map has nobody to watch it: no draws, no walks, no
            // deadline evaluation. Its state is exactly as it was when the map
            // slept, and the wake fires due timestamps lazily.
            if !group_awake[self.monsters[index].group] {
                continue;
            }
            // Two clocks, as the legacy runs them. The legacy gates the wander
            // and the enemy search on a fresh `1/25` draw **per action slot**
            // (`char-wait.cpp:342`, one slot = 300 ms, see [`AI_SLOT_MS`]), and
            // gates the swing separately on the kind's own `Attack_Interval`
            // (`char-attack.cpp:1869`). So an idle monster is sampled on the
            // slot and one with a target runs on the fine tick; the swing has
            // its own deadline inside [`Self::act`].
            if now_ms < self.monsters[index].next_act_ms {
                continue;
            }
            if let Some(hit) = self.act(registry, now_ms, rng, index) {
                hits.push(hit);
            }
            // Read **after** the act, so a monster that took a target on this
            // very pass gets the fine clock instead of being queued behind the
            // long one it was classified under a moment earlier. `.max` keeps
            // whatever deadline the act itself armed: `order_move`'s deferral
            // while a walk starts, and the long-gate backoff for a blocked route.
            let gate = if self.monsters[index].target.is_some() {
                AI_TICK_MS
            } else {
                AI_SLOT_MS
            };
            self.monsters[index].next_act_ms = self.monsters[index].next_act_ms.max(now_ms + gate);
        }
        hits
    }

    /// The live tile a monster stands on, for a range gate that must read the
    /// server's copy of the target rather than one the client claimed. `None`
    /// for a monster the world does not hold.
    pub(crate) fn position(&self, monster_unique: u16) -> Option<(i32, i32)> {
        self.monsters
            .iter()
            .find(|m| m.unique.u16() == monster_unique)
            .map(|m| (m.x, m.y))
    }

    /// This kind's AGI — the dodge a player's swing is tested against under
    /// Cat's §4.3.2 reformation. `None` for a monster the world does not hold;
    /// the caller then treats the target as having no dodge.
    pub(crate) fn unit_agi(&self, monster_unique: u16) -> Option<i32> {
        self.monsters
            .iter()
            .find(|m| m.unique.u16() == monster_unique)
            .map(|m| self.groups[m.group].units[m.unit].dex)
    }

    /// The display name of the monster `monster_unique` is — the name the
    /// death frame owes for a killing blow (`DeadChar_PC` copies the killer's
    /// name into `szKillMeCharName`, `Server-Attack.cpp:6358`; the death
    /// dialog's text has a slot for it, `NDeathDlg.cpp:112`).
    ///
    /// The legacy leaves that slot empty for an NPC killer — an NPC's
    /// `clBI.clPerson.szName` is never written — so this is the port's fix per
    /// Cat's 2026-10-21 directive: the kind's display name from the canonical
    /// drop table's `Name` column (`Evil Raccoon` for the starter field).
    /// `None` for a monster the world does not hold; the caller then sends the
    /// legacy's empty string.
    pub(crate) fn monster_display_name(&self, monster_unique: u16) -> Option<&'static str> {
        let monster = self
            .monsters
            .iter()
            .find(|m| m.unique.u16() == monster_unique)?;
        self.name_by_kind.get(&monster.wire_kind).copied()
    }

    /// The tile an attacking player should walk to when `range` does not reach
    /// `monster_unique`: the closest tile the weapon range covers that this
    /// player may stand on and that a walk can actually arrive on.
    ///
    /// This is the port's half of `CHARWORKSTEP_DECIDELENGTH`. The legacy never
    /// rejects an attack for distance — `ORDER_ATTACK` sets that sub-order and
    /// keeps the fight (`char-order.cpp:232-238`) — and the client walks its own
    /// copy of the approach (`Client.cpp:3297` applies the order locally before
    /// sending it), which is exactly why the avatar closed the gap while the
    /// server's own tile stayed put and every swing came back `OutOfRange`.
    ///
    /// `None` when the target is gone, the class has no `KindInfo_Create` row so
    /// there is no mask to judge the ground with, the world holds no map, or
    /// nothing in range is reachable from `from` — the caller then keeps waiting,
    /// which is all [`crate::main::Swing::OutOfRange`] ever did.
    pub(crate) fn approach_tile(
        &self,
        monster_unique: u16,
        from: (i32, i32),
        range: i32,
        class_kind: u16,
    ) -> Option<(i32, i32)> {
        let target = self.position(monster_unique)?;
        let block = *self.block_by_kind.get(&class_kind)?;
        let map = self.map.as_ref()?;
        approach_tile_in(target, from, range, |x, y| is_walkable(map, x, y, block))
    }

    /// Applies one player's swing to one monster and returns what it did. A
    /// monster that dies is removed here — the body is deleted in the same pass
    /// the legacy's `DeleteChar` runs in (`CharManager.cpp:257-277`) — and its
    /// group is queued to respawn it.
    ///
    /// `raw_power` is the attacker's damage *before* the monster's defence: the
    /// caller owns its own critical roll ([`combat::apply_critical`]) because an
    /// attacker's crit rate is the attacker's number, while the defender's AC is
    /// only knowable here.
    ///
    /// `now_ms` is the respawn clock; the drop rolls come from the caller's RNG
    /// so a test can replay a fight exactly.
    ///
    /// `attacker` is the struck character's unique. It becomes this monster's
    /// target, which is how a passive kind fights back: the legacy records the
    /// attacker on the *defender* (`SetBeAttackedSwitch`,
    /// `Server-Attack.cpp:587`) and the defender's own wait tick turns it into
    /// an order (`BeAttackedAction` → `FindEnemyAction` → `OrderAttack`,
    /// `CharServer-Attack.cpp:144`, `charorg.cpp:112`). `siAggressive` gates
    /// only `SearchEnemy`, so it is never consulted for retaliation.
    ///
    /// That same `SetBeAttackedSwitch` call also registers the attacker as this
    /// monster's **jim host** and broadcasts the 263 that makes a client draw
    /// the overhead health bar (`CharServer-Attack.cpp:44-74`) — the bar's only
    /// trigger for an ordinary mob. Hence `registry`: the bar is a broadcast,
    /// not a local state change.
    /// Applies one player swing to a monster.
    ///
    /// `outcome` is Cat's §4.3.2 AGI roll, resolved by the caller against
    /// this kind's AGI (a dodge or miss still runs the retaliation/jim path
    /// with zero damage — the legacy set `SetBeAttackedSwitch` *before* the
    /// dodge check, `Server-Attack.cpp:585-591`). `drop_add_rate` is the LUK
    /// bonus percent (§4.3.3) for the death roll.
    // The scalar arg list mirrors the legacy call it ports; grouping the two
    // §4.3 additions into a struct would outlive their only caller.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn hit_by_player(
        &mut self,
        registry: &Registry,
        monster_unique: u16,
        attacker: i16,
        raw_power: i32,
        outcome: combat::SwingOutcome,
        drop_add_rate: i32,
        now_ms: u64,
        rng: &mut Rng,
    ) -> Option<MonsterHit> {
        let index = self
            .monsters
            .iter()
            .position(|m| m.unique.u16() == monster_unique)?;
        let monster = self.monsters[index];
        let (group, unit) = (monster.group, monster.unit);
        // Copy the shared numbers out before touching `monsters`, so the two
        // borrows do not overlap.
        let (max_life, exp, level, ac, table, battle_power) = {
            let u = &self.groups[group].units[unit];
            (u.life, u.exp, u.level, u.ac, u.drops, u.battle_power)
        };
        let damage = if outcome == combat::SwingOutcome::Hit {
            combat::reduced_by_ac(raw_power, ac)
        } else {
            0
        };
        let new_life = monster.life.saturating_sub(damage.max(0) as u32);
        let died = new_life == 0;

        let drops = if died {
            // `GetRandDropItem` rolls **once** per kill
            // (`kindinfo-Manager.cpp:68-107`), so a kind drops at most one item
            // type per death. `AddRate` is the LUK bonus percent (§4.3.3).
            roll_drop(
                table,
                drop_add_rate,
                rng.below(i32::MAX as u32) as i32,
                rng.below(i32::MAX as u32) as i32,
            )
            .into_iter()
            .collect()
        } else {
            Vec::new()
        };

        if died {
            self.monsters.remove(index);
            // The corpse stays on the map for its death animation; the 11 that
            // removes the avatar goes out when [`Self::despawn_corpses`] runs
            // the deadline down.
            self.corpses.push((monster, now_ms + CORPSE_MS));
            // The bar this monster carried dies with it: drop the host's
            // pointer so an id the pool reissues later is not un-jimmed as
            // if the corpse's fight were still on.
            if let Some(host) = monster.jim_host
                && self.jim_by.get(&host) == Some(&monster.unique)
            {
                self.jim_by.remove(&host);
            }
            // A boss group's `Interval` (seconds in the file, ms at load —
            // `NPC.cpp:223`) is its respawn delay: the legacy holds the whole
            // group dark for the window and only then drip-refills it
            // (`NPC.cpp:862-888`, `:1077`). An ordinary group keeps the fixed
            // stand-in — the legacy's non-boss interval is a *batch top-up
            // cadence* for the whole group, not a per-monster delay, and is
            // deliberately not modelled here.
            let delay = {
                let group_data = &self.groups[group];
                if group_data.boss && group_data.respawn_ms > 0 {
                    group_data.respawn_ms
                } else {
                    RESPAWN_MS
                }
            };
            self.respawn.push((group, now_ms + delay));
            logln!(
                "game: monster {monster_unique} (kind {}, group {group}) died at ({}, {})",
                monster.wire_kind,
                monster.x,
                monster.y
            );
        } else {
            self.monsters[index].life = new_life;
            // Retaliation. Only a survivor holds a grudge — a monster removed
            // above is not in the vector to hold one. Logged once per
            // engagement rather than per hit: the acquisition is the event
            // worth seeing, the swings are the 268s that follow it.
            if self.monsters[index].target != Some(attacker) {
                logln!(
                    "game: monster {monster_unique} turns on {attacker} after being hit \
                     ({new_life} life left)"
                );
            }
            self.monsters[index].target = Some(attacker);
            // `SetBeAttackedSwitch` (`CharServer-Attack.cpp:26-76`) — being hit
            // registers the attacker as this monster's jim host, and that alone
            // is what raises the overhead health bar. `SetBeJimed` takes the
            // first host and thereafter accepts only that same one
            // (`Char-Common.cpp:6226`), so a monster already fought by another
            // player keeps *their* bar and this attacker never sees one. Every
            // accepted hit re-broadcasts, which is what the legacy does too.
            let jimed = match self.monsters[index].jim_host {
                None => {
                    self.monsters[index].jim_host = Some(attacker);
                    true
                }
                Some(host) => host == attacker,
            };
            if jimed {
                // The legacy never takes a jim back — `SetBeJimed` refuses a
                // second host outright (`Char-Common.cpp:6226`) — so a player
                // who switches targets inside a stacked cluster leaves every
                // old monster's overhead bar up at once, and stacked sprites
                // draw those bars at one screen position (the live report:
                // one bar jumping between raccoons' life values). The 263 is
                // the bar's only trigger, so it is also its only undo: the
                // client's handler runs `clJim.Init()` *before* `SetBeJimed`
                // (`Client-Person.cpp:508-512`) and the draw gate re-reads
                // `siJimHostCharUnique` every frame (`Char-Draw.cpp:1174`,
                // `:2775`), so the same frame addressed to the old monster
                // with the host field cleared (0) takes its stale bar down.
                if let Some(previous) = self.jim_by.insert(attacker, monster.unique)
                    && previous != monster.unique
                    && let Some(stale) = self.monsters.iter().position(|m| m.unique == previous)
                    && self.monsters[stale].jim_host == Some(attacker)
                    && let Some(stale_unique) = self.monsters[stale].unique.si16()
                {
                    let (x, y) = (self.monsters[stale].x, self.monsters[stale].y);
                    self.monsters[stale].jim_host = None;
                    registry.broadcast_near_point(
                        FIELD_MAP,
                        x,
                        y,
                        GameMsg::ResponseJim as u16,
                        encode_jim(stale_unique, 0),
                    );
                    // Cat's Bug-5 id trace (2026-10-21): the bar only ever
                    // jumps when this pair fires — old monster's bar down,
                    // new monster's bar up at its (possibly fresh) life. A
                    // bounce without this line is not a target switch.
                    logln!(
                        "game: 263: target switch by {attacker}: un-jim monster \
                         {stale_unique} {} at ({x}, {y}), bar follows {} {}",
                        self.monsters[stale].tag(),
                        monster.unique,
                        monster.tag()
                    );
                }
                // Pre-P4 a Tier-3 id above the SI16 cap has no 263 frame to
                // carry (§4); no pool mints one until P3, so this is the
                // frame-skip rule rather than a live path.
                if let Some(jimed_unique) = monster.unique.si16() {
                    registry.broadcast_near_point(
                        FIELD_MAP,
                        monster.x,
                        monster.y,
                        GameMsg::ResponseJim as u16,
                        encode_jim(jimed_unique, attacker),
                    );
                }
            }
        }

        Some(MonsterHit {
            unique: monster_unique,
            x: monster.x,
            y: monster.y,
            life: new_life,
            died,
            damage,
            exp,
            level,
            max_life,
            battle_power,
            drops,
            outcome,
        })
    }

    /// Removes every corpse whose linger has run out, broadcasting the 11 that
    /// deletes its avatar (`encode_logout`) to everyone still near the tile it
    /// died on.
    ///
    /// This is the second half of a kill, and it has to be a *second* half: the
    /// legacy holds a dead char in the manager until its own action loop finds
    /// it invalid (`CharManager.cpp:257-277`), which gives the client the
    /// `ORDER_DIE` frame it was sent at death to play out in.
    fn despawn_corpses(&mut self, registry: &Registry, now_ms: u64) {
        let mut due = 0;
        while due < self.corpses.len() {
            if self.corpses[due].1 > now_ms {
                due += 1;
                continue;
            }
            let (monster, _) = self.corpses.remove(due);
            registry.broadcast_near_point(
                FIELD_MAP,
                monster.x,
                monster.y,
                GameMsg::ResponseLogout as u16,
                encode_logout(i32::from(monster.unique.u16())),
            );
            // The 11 has gone out, so every client near the tile has deleted
            // the avatar: the id is safe to hand out again (audit H3).
            self.ids.free_monster(monster.unique);
            logln!(
                "game: monster {} (kind {}) corpse removed at ({}, {})",
                monster.unique,
                monster.wire_kind,
                monster.x,
                monster.y
            );
        }
    }

    /// Respawn every group whose deadline has passed, one monster each. The
    /// legacy's `SetChar` gate runs once per wait tick before any AI
    /// (`NPC.cpp:922-938`), so respawning here rather than in the act loop keeps
    /// that order.
    fn respawn_due(&mut self, registry: &Registry, now_ms: u64) {
        let mut due = 0;
        while due < self.respawn.len() {
            if self.respawn[due].1 > now_ms {
                due += 1;
                continue;
            }
            let (group, _) = self.respawn.remove(due);
            self.respawn_one(registry, group);
        }
    }

    /// Replaces one monster in `group` toward its unit targets.
    fn respawn_one(&mut self, registry: &Registry, group: usize) {
        let live = self.monsters.iter().filter(|m| m.group == group).count() as i32;
        if live >= self.groups[group].capacity {
            return;
        }
        for unit_index in 0..self.groups[group].units.len() {
            let unit = &self.groups[group].units[unit_index];
            let want = unit.max_num.max(0);
            let have = self
                .monsters
                .iter()
                .filter(|m| m.group == group && m.unit == unit_index)
                .count() as i32;
            if have >= want || unit.tiles.is_empty() {
                continue;
            }
            // The same deterministic index the boot pass would have used for
            // this slot, then the zero-collision rule: a wanderer may have
            // walked onto that tile since (respawn is a spawn — Cat,
            // 2026-10-21), and one tile holds one monster.
            let base = unit.tiles[(have as usize).min(unit.tiles.len() - 1)];
            let Some(at) = self.open_spawn_tile(group, unit_index, base) else {
                return;
            };
            self.spawn_one(registry, group, unit_index, at);
            return;
        }
    }
    /// Steps every running walk one tile and broadcasts the 200 that carries the
    /// new cell — the NPC half of `cltCharCommon::Move` (`char-move.cpp:145-171`),
    /// which the observer fan already does for players.
    fn advance_walks(&mut self, registry: &Registry, now_ms: u64) {
        // `now_ms` is the deadline clock; the accumulator reads the real
        // instant so a walk's cadence is not distorted by a stale stamp.
        let now = Instant::now();
        for index in 0..self.monsters.len() {
            let Some(mut walk) = self.monsters[index].walk else {
                continue;
            };
            let Some(cell) = walk.advance(now) else {
                // Keep the deadline moving so `next_delay` stays meaningful.
                self.monsters[index].walk = Some(walk);
                continue;
            };
            // The route was checked before the order went out, so this is an
            // invariant, not a gate — but a monster must never be left on a tile
            // a client cannot draw it on.
            if self.groups[self.monsters[index].group].units[self.monsters[index].unit]
                .tiles
                .binary_search(&cell)
                .is_err()
            {
                self.monsters[index].walk = None;
                continue;
            }
            self.monsters[index].x = cell.0;
            self.monsters[index].y = cell.1;
            self.monsters[index].walk = if walk.arrived() { None } else { Some(walk) };
            let m = self.monsters[index];
            broadcast_char_update(registry, FIELD_MAP, &m);
            // A walk keeps its monster busy: it cannot also look for a fight.
            self.monsters[index].next_act_ms =
                self.monsters[index].next_act_ms.max(now_ms + AI_TICK_MS);
        }
    }

    /// One monster's turn — the legacy's `OperateWaitServer` body
    /// (`char-wait.cpp:315-406`): an idle one wanders, an engaged one walks into
    /// range or swings.
    fn act(
        &mut self,
        registry: &Registry,
        now_ms: u64,
        rng: &mut Rng,
        index: usize,
    ) -> Option<PlayerHit> {
        let (group, unit) = (self.monsters[index].group, self.monsters[index].unit);
        let (aggressive, range, attack_interval, min_atk, max_atk, crit, monster_agi) = {
            let u = &self.groups[group].units[unit];
            (
                u.aggressive,
                u.range.max(1),
                u.attack_interval,
                u.min_atk,
                u.max_atk,
                u.critical_rate,
                // §4.3.2: this monster's AGI — the accuracy of its own swing.
                u.dex,
            )
        };

        if self.monsters[index].target.is_none() {
            // A walking monster is in `ORDER_MOVE`, not `ORDER_WAIT`: the
            // legacy's dispatcher reaches `OperateWaitServer` only from the
            // wait branch (`Char-Common.cpp:1634-1640`), so a character never
            // draws this gate — and never starts a fight — while it walks.
            // The draw stays `ORDER_WAIT`-shaped and waits out the walk.
            if self.monsters[index].walk.is_some() {
                return None;
            }
            // One draw per action slot, and a miss costs the slot: the legacy's
            // `(rand() + siID) % 25` gate (`char-wait.cpp:342`), not a timer.
            // Re-arming is the tick loop's job — an idle monster is on
            // [`AI_SLOT_MS`] there.
            if rng.below(AI_DECIDE_ONE_IN) != 0 {
                return None;
            }
            self.engage(registry, index, aggressive);
            if self.monsters[index].target.is_none() {
                // Nothing to fight: `RandomMove` (`char-wait.cpp:202`).
                self.wander(registry, index, now_ms, rng);
                return None;
            }
        }

        let target_unique = self.monsters[index].target?;
        // A target that has logged out is not one any more; the legacy's
        // `CR[id]` null guard drops the hit the same way.
        let Some(target_key) = registry.find_by_charunique(target_unique) else {
            self.monsters[index].target = None;
            return None;
        };
        let Some(target) = registry.info(target_key) else {
            self.monsters[index].target = None;
            return None;
        };
        // `CanAttack`: a dead or departed target cannot be fought.
        if target.life == 0 || target.map_id != FIELD_MAP {
            self.monsters[index].target = None;
            return None;
        }

        let (x, y) = (self.monsters[index].x, self.monsters[index].y);
        let (dx, dy) = ((target.x - x).abs(), (target.y - y).abs());
        if dx > range || dy > range {
            // `Action_Attack` walks into range before swinging. A monster already
            // walking that route does not re-order it: the client walks the order
            // it was given, and a fresh 241 every fine tick would restart it in
            // place instead of letting it arrive. One with nowhere to walk — a
            // wall on the route — waits an action slot rather than rescanning a
            // blocked path every 100 ms tick.
            if self.monsters[index].walk.is_none() {
                self.order_move(registry, index, (target.x, target.y), now_ms);
                if self.monsters[index].walk.is_none() {
                    self.monsters[index].next_act_ms =
                        self.monsters[index].next_act_ms.max(now_ms + AI_SLOT_MS);
                }
            }
            return None;
        }

        // The kind's own clock, not `next_act_ms`: the client repeats this
        // monster's swing animation on `Attack_Interval`
        // (`char-attack.cpp:1869`), so a hit paced by the act gate lands whole
        // animations after the swing that shows it — the live report of the
        // raccoon swinging 3-4 times before a damage number appeared.
        if now_ms < self.monsters[index].next_swing_ms {
            return None;
        }
        self.monsters[index].next_swing_ms = now_ms + attack_interval;

        // The swing, so a bystander's client plays it (`MakeOrder_ATTACK`,
        // `order.cpp:314`).
        registry.broadcast_near_point(
            FIELD_MAP,
            target.x,
            target.y,
            GameMsg::ResponseUserorder as u16,
            OrderData::attack_order(i32::from(self.monsters[index].unique.u16()), target_unique)
                .encode()
                .to_vec(),
        );
        // Cat's §4.3.2 Ghost Dodge fix, monster→player half: the roll is
        // tested against the target's own AGI (its `dex` column — the client's
        // legacy-formula `siDodgeRate` in the 267 is ignored). A dodge or a
        // miss sends the same zeroed 268 with the flag set the legacy did
        // (`Server-Attack.cpp:610-634`) — life unchanged, nothing spent.
        // Missing a session with no `BasicInfo` (only possible off the world-
        // entry path) reads as zero dodge, which is the pre-reformation rate.
        let basic = registry.basic_info(target_key);
        let (player_agi, player_vit) = (
            basic.as_ref().map_or(0, |info| info.dex),
            basic.as_ref().map_or(0, |info| info.vit),
        );
        let outcome = combat::swing_outcome(monster_agi, player_agi, rng.below(10_000) as i32);
        if outcome != combat::SwingOutcome::Hit {
            return Some(PlayerHit {
                key: target_key,
                char_unique: target_unique,
                x: target.x,
                y: target.y,
                life: target.life,
                damage: 0,
                attacker_unique: self.monsters[index].unique.u16(),
                outcome,
            });
        }
        let raw = combat::roll_power(min_atk, max_atk, rng.up_to(max_atk - min_atk));
        let damage = combat::reduced_by_ac(
            // §4.3.3: monster crits keep the kind rate and carry no LUK term —
            // LUK scaling belongs to the player's swing.
            combat::apply_critical(raw, crit, 0, rng.up_to(99)),
            // §4.3.4: the player's defence is its level AC plus the natural
            // flat defence its VIT buys.
            combat::level_ac(target.level) + combat::natural_flat_def(player_vit, target.level),
        );
        let life = registry.set_life(target_key, damage)?;
        Some(PlayerHit {
            key: target_key,
            char_unique: target_unique,
            x: target.x,
            y: target.y,
            life,
            damage,
            attacker_unique: self.monsters[index].unique.u16(),
            outcome,
        })
    }

    /// `SearchEnemy(siAggressive)` (`char-wait.cpp:22-200`) — take the nearest
    /// live player inside this monster's aggression box. The legacy also gates
    /// on `IsEnemy` and `CanAttack`; with one player race and no safe zone on
    /// map 0, being alive and in range is the whole test.
    fn engage(&mut self, registry: &Registry, index: usize, aggressive: i32) {
        if aggressive <= 0 {
            return;
        }
        let (x, y) = (self.monsters[index].x, self.monsters[index].y);
        for (_, session) in registry.sessions_near_point(FIELD_MAP, x, y) {
            if session.life == 0 {
                continue;
            }
            if (session.x - x).abs() <= aggressive && (session.y - y).abs() <= aggressive {
                self.monsters[index].target = Some(session.char_unique.si16());
                logln!(
                    "game: monster {} {} engaged character {}",
                    self.monsters[index].unique,
                    self.monsters[index].tag(),
                    session.char_unique
                );
                break;
            }
        }
    }

    /// `RandomMove` (`char-wait.cpp:202-300`): an idle monster picks a tile of
    /// its own group's ground near it and walks there. Ours always has a group,
    /// so the legacy's no-group ±10 branch never applies.
    fn wander(&mut self, registry: &Registry, index: usize, now_ms: u64, rng: &mut Rng) {
        let (group, unit) = (self.monsters[index].group, self.monsters[index].unit);
        let (x, y) = (self.monsters[index].x, self.monsters[index].y);
        let tiles = &self.groups[group].units[unit].tiles;
        // The legacy picks a point in the ±5 box and rejects a wall; ours picks
        // a tile that is *already* walkable and already inside the box, so it
        // can never choose one it would then have to refuse.
        // A monster walks the order it was given. Re-picking while the last one
        // is still in progress would rebuild `Movement` from zero and throw away
        // the tiles already banked, so a long wander ends up walked in place —
        // the same trap the range branch avoids.
        if self.monsters[index].walk.is_some() {
            return;
        }
        let near = tiles
            .iter()
            .filter(|(tx, ty)| (*tx - x).abs() <= WANDER_RADIUS && (*ty - y).abs() <= WANDER_RADIUS)
            .count();
        if near == 0 {
            return;
        }
        let pick = rng.below(near as u32) as usize;
        let dest = tiles
            .iter()
            .filter(|(tx, ty)| (*tx - x).abs() <= WANDER_RADIUS && (*ty - y).abs() <= WANDER_RADIUS)
            .nth(pick)
            .copied();
        let Some(dest) = dest else { return };
        self.order_move(registry, index, dest, now_ms);
    }

    /// Issues the 241 for a monster's move and starts the step — the NPC half of
    /// `OrderMove` / `OrderStraightMove` (`char-wait.cpp:230-300`).
    ///
    /// The route is cleared *before* the order goes out, because the client walks
    /// it whether or not the server approves each tile: a monster that starts a
    /// walk it cannot finish is a monster that desyncs.
    fn order_move(&mut self, registry: &Registry, index: usize, dest: (i32, i32), now_ms: u64) {
        let (group, unit) = (self.monsters[index].group, self.monsters[index].unit);
        let (x, y) = (self.monsters[index].x, self.monsters[index].y);
        if dest == (x, y) {
            return;
        }
        // Runtime one-tile-one-monster: never walk onto a tile another monster
        // already holds (its walk destination or its stance). A wander or an
        // engage-approach that would land on an occupied tile simply waits an
        // action slot and re-decides — the caller's `walk.is_none()` guard
        // already arms that, so this cannot busy-loop.
        if self.slot_held(index, dest) {
            return;
        }
        if !walkable_path(&self.groups[group].units[unit].tiles, (x, y), dest) {
            return;
        }
        let unique = self.monsters[index].unique;
        registry.broadcast_near_point(
            FIELD_MAP,
            dest.0,
            dest.1,
            GameMsg::ResponseUserorder as u16,
            OrderData::move_order(
                i32::from(unique.u16()),
                dest.0.clamp(0, 0x3FF) as i16,
                dest.1.clamp(0, 0x3FF) as i16,
            )
            .encode()
            .to_vec(),
        );
        self.monsters[index].walk = Some(Movement::new(
            dest,
            (x, y),
            FIELD_MAP,
            i32::from(self.monsters[index].move_speed),
        ));
        // The monster is walking; it should not swing on the same pass.
        self.monsters[index].next_act_ms = now_ms + AI_TICK_MS;
    }
}

/// How long a dead monster stays gone before its group respawns it, for every
/// group whose own cadence does not apply: any non-boss group (the legacy's
/// non-boss `Interval` is a group top-up cadence, not a per-monster delay), and
/// a boss group whose file carries `Interval = 0`. A boss group with an
/// interval honours it instead — see [`FieldGroup::respawn_ms`].
///
/// Tuned 30 s -> 10 s (Cat, 2026-10-20); still above [`CORPSE_MS`], so a
/// corpse frees its id before its respawn draws one.
const RESPAWN_MS: u64 = 10_000;

/// How long a corpse stays on the map before the 11 that removes its avatar
/// goes out.
///
/// Death is `ORDER_DIE` first and the removal after it, not both at once — the
/// legacy holds the char in the manager until its action loop sees it invalid
/// (`CharManager.cpp:257-277`). Sent together the client deletes the avatar in
/// the same frame the order arrives, so the death animation is never drawn.
///
/// The legacy's own linger is the time its `ORDER_DIE` action takes to run;
/// 7 s sits inside the 5-10 s the live game showed and clears every shipped
/// death animation with room to spare.
const CORPSE_MS: u64 = 7_000;

/// World-map spawn density (Cat, 2026-10-20 — 3x the shipped population):
/// `load` multiplies every *field* group's file-derived unit target *and*
/// group cap by this. Both scale together, so a group's `MaxNPCNum` can never
/// re-clamp its own amplified targets — the field lands on exactly 3x, not
/// 3x-then-clamped.
///
/// Not for the static town NPCs (Cat, 2026-10-21): a group the
/// static-placement generator takes verbatim — `Radius` 1, every unit at
/// `MinNum` 1 — keeps its shipped count, so a town holds one copy of each
/// NPC instead of three stacked.
const DENSITY_MULTIPLIER: i32 = 3;

/// A corpse must free its id before its respawn draws one; the ordering above
/// (respawn 10 s > corpse 7 s) is what bounds ids in flight to live monsters.
const _: () = assert!(RESPAWN_MS > CORPSE_MS);

/// Whether the straight line [`Movement`] would walk from `from` to `to` stays
/// on `tiles` at every committed step.
///
/// [`Movement`] steps diagonally until it is aligned on one axis and then
/// straight (`char-move.cpp:145-171`), so this mirrors that order rather than
/// any L-shaped route: it is the path the client will actually be told to walk.
fn walkable_path(tiles: &[(i32, i32)], from: (i32, i32), to: (i32, i32)) -> bool {
    let mut at = from;
    while at != to {
        if at.0 != to.0 {
            at.0 += (to.0 - at.0).signum();
        }
        if at.1 != to.1 {
            at.1 += (to.1 - at.1).signum();
        }
        if tiles.binary_search(&at).is_err() {
            return false;
        }
    }
    true
}

/// The tile an attacking player walks to so `range` can reach `target`: the
/// first tile within range along the straight line [`Movement`] would walk —
/// `None` when the line never gets there.
///
/// This is the legacy's own approach rather than a search of its own devising.
/// Out of range it does `SetMove(targetX, targetY)` and steps there under
/// `CHARWORKSTEP_MOVE`, re-checking `IsInRange` as it goes
/// (`char-attack.cpp:1878-1887`), so the fight starts the moment the weapon
/// reaches and not on the target's tile. Walking that same line is also what
/// keeps this server on the client's tiles: the client applies the attack order
/// locally before sending it (`Client.cpp:3297` `LetOrder`) and runs the same
/// approach, so a cleverer route here would surface as the avatar snapping back
/// to the server's.
///
/// The distance is provably the least any in-range tile can cost — every
/// in-range tile is at least `chebyshev(from, target) - range` away, and this
/// tile is exactly that — so there is nothing to optimise over.
///
/// A wall on the line stops the walk where the legacy's own `Move()` stops on a
/// collision — but it stops *there*, it does not throw the whole approach away.
/// The caller re-plans from whatever tile was reached on its next tick
/// (`main.rs`), so the gap keeps closing whenever the line to the target opens,
/// which is also what happens as the target wanders.
///
/// This is deliberately still not a pathfind: neither the legacy nor the client
/// routes around an obstacle for `CHARWORKSTEP_DECIDELENGTH`, so a route only
/// this server knew would desync the two. Both sides walk the same straight line
/// and both stop at the same wall.
fn approach_tile_in(
    target: (i32, i32),
    from: (i32, i32),
    range: i32,
    standable: impl Fn(i32, i32) -> bool,
) -> Option<(i32, i32)> {
    let reach = range.max(1);
    // In range already: there is nothing to walk to, and the target's own tile
    // is never an answer. The swing arm only asks from out of range, so this is
    // the contract stated rather than a path it takes.
    if (from.0 - target.0).abs().max((from.1 - target.1).abs()) <= reach {
        return None;
    }
    let mut at = from;
    // The closest tile the line has offered that is still out of reach. Every
    // step moves toward the target, so this only ever gets closer.
    let mut nearest_open: Option<(i32, i32)> = None;
    while at != target {
        if at.0 != target.0 {
            at.0 += (target.0 - at.0).signum();
        }
        if at.1 != target.1 {
            at.1 += (target.1 - at.1).signum();
        }
        if !standable(at.0, at.1) {
            break;
        }
        // Chebyshev is `IsInRange`'s operand (`Char-Common.cpp:1939`), which is
        // also the gate the swing was rejected on.
        if (at.0 - target.0).abs().max((at.1 - target.1).abs()) <= reach {
            return Some(at);
        }
        nearest_open = Some(at);
    }
    // The line hit something. Handing back the last open tile closes the gap that
    // was reachable instead of discarding it; `None` means the very first step was
    // blocked, so there is nowhere this player may go — never the tile they are
    // already standing on, which would spin the re-plan forever.
    nearest_open
}

/// Queues one monster's appearance to every session near it.
fn broadcast_spawn(registry: &Registry, map_id: i32, monster: &Monster) {
    for body in
        encode_login_many_batches_mixed(&[LoginManyRecord::Npc(record_for(map_id, monster))])
    {
        registry.broadcast_near_point(
            map_id,
            monster.x,
            monster.y,
            GameMsg::ResponseLoginmany as u16,
            body,
        );
    }
}

/// Queues one monster's per-tile position to every session near the new cell —
/// the 200 half of a walk. Life is the resulting total, as in every other 200.
fn broadcast_char_update(registry: &Registry, map_id: i32, monster: &Monster) {
    // Pre-P4, a Tier-3 id above the SI16 wire cap has no 200 frame to carry
    // it (§4); no pool mints one until P3, so this is the frame-skip rule
    // rather than a live path.
    let Some(si_unique) = monster.unique.si16() else {
        return;
    };
    registry.broadcast_near_point(
        map_id,
        monster.x,
        monster.y,
        GameMsg::ResponseCharupdate as u16,
        BasicCharUpdate {
            si_char_unique: si_unique,
            si_move_speed: monster.move_speed as i8,
            b_urgent_mode: false,
            si_life: monster.life as i32,
            si_reserved_life: monster.life as i32,
            si_mana: 0,
            si_x: monster.x.clamp(0, 0x3FF) as i16,
            si_y: monster.y.clamp(0, 0x3FF) as i16,
            ui_gm_mode: 0,
            b_pc_room: false,
            si_make_ani_type: 0,
        }
        .encode()
        .to_vec(),
    );
}

/// The wire record for one monster — the same `cltMakeCharInfoCommon` shape a
/// static NPC is announced with (`npc_record_for`, `world.rs`), with the kind's
/// move speed so a monster is distinguishable from a standing NPC.
///
/// No max life rides along with [`NpcRecord::life`], and none can: the client
/// derives it from KindInfo. An overhead health bar is gated on the kind having
/// `ATB_NOJJIM` (`Char-Draw.cpp:2745`, checked again at `:2821`), which only 110
/// of 563 shipped monster kinds carry — the raccoon and the other starter mobs
/// among them are not. That is why a field monster draws no bar, and it is not
/// reachable from here.
fn record_for(map_id: i32, monster: &Monster) -> NpcRecord {
    NpcRecord {
        make_char_kind: monster.wire_kind,
        make_char_unique: monster.unique.u16(),
        level: monster.level,
        life: monster.life,
        x: monster.x.clamp(0, 0x3FF) as u16,
        y: monster.y.clamp(0, 0x3FF) as u16,
        map_index: client_map_index(map_id),
        total_move_speed: monster.move_speed,
        home_village_unique: 0,
    }
}

/// Whether `(bx, by)` sits in the 3×3 cell block around `(ax, ay)` — the same
/// near rule [`gz_data::spawn`]'s callers use for sessions
/// (`gz_proto::map_geometry`).
fn near(ax: i32, ay: i32, bx: i32, by: i32, map_id: i32) -> bool {
    cells_adjacent(cell_of(ax, ay, map_id), cell_of(bx, by, map_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dev checkout's client data tree, as the other shipped-data tests
    /// locate it.
    const DATA_ROOT: &str = "C:/Data/04_Server_Goonzu/New Era Live";

    /// A player-band unique for a session fixture — the typed field can only
    /// be built through the band check (T11 pins the refusals).
    fn pc(raw: i16) -> crate::ids::PlayerCharUnique {
        crate::ids::PlayerCharUnique::try_from_si16(raw).expect("test id in the player band")
    }

    /// A standard-band monster unique for a hand-built `Monster`.
    fn sid(raw: u16) -> MonsterEntityId {
        MonsterEntityId::Standard(
            crate::ids::StandardMonsterId::try_from_u16(raw).expect("test id in the standard band"),
        )
    }

    /// Whether either standard free list holds this raw id (T7's H3 probe).
    fn free_holds(world: &MonsterWorld, raw: u16) -> bool {
        let (t1, t2, _) = world.ids.free_snapshot();
        t1.contains(&raw) || t2.contains(&raw)
    }

    /// Whether any pool's free list holds anything at all.
    fn free_holds_any(world: &MonsterWorld) -> bool {
        let (t1, t2, t3) = world.ids.free_snapshot();
        !t1.is_empty() || !t2.is_empty() || !t3.is_empty()
    }

    #[test]
    fn the_shipped_field_groups_spawn_on_safe_ground_with_usable_ids() {
        if !Path::new(DATA_ROOT).exists() {
            return; // the game data is not part of this repository
        }
        let mut world = MonsterWorld::load(Path::new(DATA_ROOT)).expect("the field data loads");
        let groups = world.groups.len();
        world.spawn_all(&Registry::new());

        // 157 shipped map-0 files carry a group; 5 carry a `NULL` unit block
        // (nothing to spawn) and 4 more (`npcsantaspring03..06`) sit entirely on
        // the snow mountain, where no tile passes the kind's mask — the legacy
        // skips those too (`NPC.cpp:961`). 157 - 5 - 4 = 148.
        assert_eq!(groups, 148, "one placeable group per shipped map-0 file");
        // The sum of the placeable groups' unit targets (`MinNum`) — the boot
        // tick fills each unit to its target, so this is the shipped population.
        // Two classes since Cat's density fix (2026-10-21): a field group
        // (anything but `Radius` 1 with every unit at `MinNum` 1) triples, a
        // static town group keeps its shipped count. 1_536 placeable units in
        // total; 42 of them sit in town groups and spawn once, the other 1_494
        // triple: 1_494 x 3 + 42 = 4_524.
        assert_eq!(
            world.monster_count(),
            4_524,
            "field unit targets x DENSITY_MULTIPLIER, town units x1"
        );
        // And every static town group holds exactly its shipped count — the
        // fix behind "3 copies of the same NPC stacked in town".
        for (index, group) in world.groups.iter().enumerate() {
            if group.anchor.radius != 1 {
                continue;
            }
            let shipped = usize::try_from(group.units.iter().map(|u| u.max_num).sum::<i32>())
                .expect("town unit targets are non-negative");
            let copies = world.monsters.iter().filter(|m| m.group == index).count();
            assert_eq!(
                copies, shipped,
                "town group at ({}, {}) must hold its shipped count",
                group.anchor.center_x, group.anchor.center_y
            );
        }

        let npc_top =
            gz_data::npc::NPC_CHAR_UNIQUE_BASE + gz_data::npc::NPC_PLACEMENTS.len() as u16;
        let mut seen = std::collections::HashSet::new();
        for m in &world.monsters {
            assert!(
                m.unique.u16() >= npc_top && m.unique.u16() <= ids::T2_TOP,
                "id {} outside the standard band",
                m.unique
            );
            assert!(seen.insert(m.unique), "duplicate id {}", m.unique);
            assert!(m.life > 0, "zero-life monster {}", m.unique);
            // Every instance resolves to a unit that knows how to fight, or the
            // combat phase would find a monster with no numbers behind it.
            let u = &world.groups[m.group].units[m.unit];
            assert!(
                u.max_atk >= u.min_atk,
                "kind {} inverted swing",
                u.wire_kind
            );
            assert!(u.range >= 1, "kind {} cannot reach", u.wire_kind);
        }

        // And every place is inside its own group's leash.
        for m in &world.monsters {
            assert!(
                world.groups[m.group].anchor.contains(FIELD_MAP, m.x, m.y),
                "monster {} outside its leash",
                m.unique
            );
        }
    }

    /// Cat's 2026-10-21 Bug 4: the death frame's killer name is the kind's
    /// display name out of the canonical drop table's `Name` column — the
    /// legacy left that slot blank for an NPC killer.
    #[test]
    fn a_spawned_monster_resolves_the_kind_display_name_for_the_death_frame() {
        if !Path::new(DATA_ROOT).exists() {
            return; // the game data is not part of this repository
        }
        let mut world = MonsterWorld::load(Path::new(DATA_ROOT)).expect("the field data loads");
        world.spawn_all(&Registry::new());
        // The starter field spawns Evil Raccoons; their kind's display name
        // must reach the lookup the death path uses.
        assert!(
            world
                .monsters
                .iter()
                .any(|m| world.monster_display_name(m.unique.u16()) == Some("Evil Raccoon")),
            "the map-0 raccoons must resolve to their kind's display name"
        );
        // A monster the world does not hold yields no name at all — the
        // caller then sends the legacy's blank rather than a guess.
        assert_eq!(world.monster_display_name(0), None);
    }

    /// Cat's zero-collision spawner directive (2026-10-21): at spawn, one
    /// tile holds exactly one monster — all 4,524 of them. The stride is
    /// only a preference now; an occupied pick falls through the unit's own
    /// safe tiles and then to walkable rings around it
    /// ([`MonsterWorld::open_spawn_tile`]). `per_pos.len() == monsters.len()`
    /// is exactly "no tile was ever shared".
    #[test]
    fn the_shipped_spawn_never_shares_a_tile() {
        if !Path::new(DATA_ROOT).exists() {
            return; // the game data is not part of this repository
        }
        let mut world = MonsterWorld::load(Path::new(DATA_ROOT)).expect("the field data loads");
        world.spawn_all(&Registry::new());
        let mut per_pos: std::collections::HashMap<(i32, i32), u32> =
            std::collections::HashMap::new();
        for m in &world.monsters {
            *per_pos.entry((m.x, m.y)).or_default() += 1;
        }
        assert_eq!(world.monsters.len(), 4_524, "every unit still fills");
        assert_eq!(
            per_pos.len(),
            world.monsters.len(),
            "one tile per monster — zero exact-coordinate collisions"
        );
    }

    /// The runtime half of the zero-collision directive (2026-10-21): after the
    /// AI has run — idle `wander`s and engage-approaches both — no two monsters
    /// hold the same slot (their walk destination if walking, else their stance).
    ///
    /// The boot fill proves spawn is clean; this proves movement never stacks
    /// two monsters on one tile, which is the click-bounce sibling a spawn check
    /// cannot see because the overlap is acquired *after* the fill. Two monsters
    /// with the same final destination would both come to rest on it and
    /// alternate their life bar under `FindLandChar`. Both movement kinds funnel
    /// through `order_move`, the `slot_held`-gated chokepoint.
    ///
    /// Asserted on the *effective slot* (destination-or-stance), the invariant
    /// `slot_held` maintains. A momentary crossing of two mid-walk paths can
    /// share an intermediate tile (transient, legacy parity) but never the
    /// resting tile, which is what a persistent bounce needs.
    #[test]
    fn running_the_ai_never_stacks_two_monsters_on_one_tile() {
        use crate::registry::{Outbound, SessionInfo};

        if !Path::new(DATA_ROOT).exists() {
            return; // the game data is not part of this repository
        }
        let mut world = MonsterWorld::load(Path::new(DATA_ROOT)).expect("the field data loads");
        let registry = Registry::new();
        world.spawn_all(&registry);

        // A player standing in the field so nearby aggressive monsters engage
        // and walk toward it (the engage-approach path); the rest idle-wander.
        let key = Registry::next_key();
        let (tx, _rx) = tokio::sync::mpsc::channel::<Outbound>(crate::registry::OUTBOUND_CAP);
        let at = (world.monsters[0].x, world.monsters[0].y);
        registry.register(
            key,
            tx,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            SessionInfo {
                char_unique: pc(1000),
                name: "Watcher".to_owned(),
                level: 1,
                map_id: FIELD_MAP,
                x: at.0,
                y: at.1,
                school_id: 0,
                class_kind: 1,
                life: 40,
                move_speed: 12,
                sex: 1,
                gm_level: 0,
            },
        );

        let mut rng = Rng::new(7);
        // Wander is a 1-in-25 draw per action slot and walks take slots to
        // settle, so a couple hundred slots exercises both the idle and the
        // engaged movement of the whole 4,524-monster field.
        for slot in 1..=250u64 {
            world.tick(&registry, slot * AI_SLOT_MS, &mut rng);
            let mut held: std::collections::HashMap<(i32, i32), u32> =
                std::collections::HashMap::new();
            for m in &world.monsters {
                let at = m.walk.as_ref().map(|w| w.dest).unwrap_or((m.x, m.y));
                *held.entry(at).or_default() += 1;
            }
            assert!(
                held.values().all(|&n| n == 1),
                "slot {slot}: two monsters share a tile — the click-bounce sibling"
            );
        }
    }

    /// The record the client is given has to survive its own round trip, or a
    /// monster is announced as something other than what it is.
    #[test]
    fn the_wire_record_round_trips() {
        let monster = Monster {
            unique: sid(9001),
            wire_kind: 15,
            instance: 1,
            level: 30,
            life: 40,
            x: 246,
            y: 332,
            move_speed: 12,
            group: 0,
            unit: 0,
            target: None,
            jim_host: None,
            walk: None,
            next_act_ms: 0,
            next_swing_ms: 0,
        };
        let record = record_for(FIELD_MAP, &monster);
        assert_eq!(
            NpcRecord::decode(&record.encode()).expect("decodes"),
            record
        );
    }

    #[test]
    fn announce_near_is_a_no_op_off_the_field_map() {
        let world = MonsterWorld::empty();
        // Off-map: no panic, and a session that is not registered is untouched.
        world.announce_near(&Registry::new(), 1, 158, 0, 0);
    }

    /// The walk half of the near mechanism: entering a cell announces the
    /// monsters in the cells the block *gained*, and only those — re-sending one
    /// the client already holds makes it delete and re-create the avatar
    /// (`Client-SetChar.cpp:111-119`).
    #[test]
    fn announce_new_cells_sends_only_the_monsters_the_block_gained() {
        use crate::registry::{Outbound, SessionInfo};

        let registry = Registry::new();
        let key = Registry::next_key();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Outbound>(crate::registry::OUTBOUND_CAP);
        registry.register(
            key,
            tx,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            SessionInfo {
                char_unique: pc(1000),
                name: "Watcher".to_owned(),
                level: 1,
                map_id: FIELD_MAP,
                x: 50,
                y: 50,
                school_id: 0,
                class_kind: 1,
                life: 40,
                move_speed: 12,
                sex: 1,
                gm_level: 0,
            },
        );

        let mut world = MonsterWorld::empty();
        // One monster in the block the session is entering, one in the block it
        // was already in.
        for (unique, at) in [(9001u16, (100, 100)), (9002, (200, 200))] {
            world.monsters.push(Monster {
                unique: sid(unique),
                wire_kind: 15,
                instance: 1,
                level: 1,
                life: 10,
                x: at.0,
                y: at.1,
                move_speed: 12,
                group: 0,
                unit: 0,
                target: None,
                jim_host: None,
                walk: None,
                next_act_ms: 0,
                next_swing_ms: 0,
            });
        }

        let was = cell_of(50, 50, FIELD_MAP);
        let now = cell_of(100, 100, FIELD_MAP);
        world.announce_new_cells(&registry, key, FIELD_MAP, was, now);

        let mut got = Vec::new();
        while let Ok(out) = rx.try_recv() {
            got.push(out);
        }
        // Only the newly-in-block monster, in one frame.
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].cmd, GameMsg::ResponseLoginmany as u16);
        let record = NpcRecord::decode(&got[0].payload).expect("decodes");
        assert_eq!(record.make_char_unique, 9001);
    }

    /// A route that would cross a tile the kind's mask rejects is refused before
    /// the 241 goes out — otherwise the client walks a wall.
    #[test]
    fn a_route_through_a_wall_is_refused() {
        // A 3-wide corridor along y = 0, broken in the middle.
        let tiles = [(0, 0), (1, 0), (3, 0), (4, 0)];
        assert!(walkable_path(&tiles, (0, 0), (1, 0)));
        assert!(
            !walkable_path(&tiles, (0, 0), (3, 0)),
            "the step through (2, 0) is not walkable"
        );
        // Staying put is trivially fine.
        assert!(walkable_path(&tiles, (1, 0), (1, 0)));
    }

    /// The diagonal-first stepping order is what gets checked, because that is
    /// the order [`Movement`] commits.
    #[test]
    fn a_diagonal_route_checks_the_diagonal_first() {
        // Only the diagonal tile is walkable, and it must be enough.
        let tiles = [(0, 0), (1, 1), (2, 2)];
        assert!(walkable_path(&tiles, (0, 0), (2, 2)));
        // Removing the diagonal breaks it even though the corners exist.
        let broken = [(0, 0), (2, 0), (0, 2), (2, 2)];
        assert!(!walkable_path(&broken, (0, 0), (2, 2)));
    }

    /// The approach stands where the weapon reaches — the first tile in range
    /// along the line [`Movement`] actually walks — and stops at a wall instead
    /// of carrying the player through it.
    #[test]
    fn an_approach_stops_where_the_weapon_reaches() {
        let open = |_, _| true;
        // Three tiles south of a reach-1 target: the last tile short of it is
        // the first one in range, so that is where the walk ends.
        assert_eq!(
            approach_tile_in((10, 10), (10, 14), 1, open),
            Some((10, 11))
        );
        // A longer reach stops earlier — only the distance the swing needs.
        assert_eq!(
            approach_tile_in((10, 10), (10, 14), 2, open),
            Some((10, 12))
        );
        // Diagonals step diagonally first, which is the order [`Movement`]
        // commits (`char-move.cpp:107-126`).
        assert_eq!(approach_tile_in((5, 5), (0, 0), 1, open), Some((4, 4)));
        // Ground the attacker may not stand on ends the approach *at the wall*:
        // the open tiles before it are still walked and the swing keeps waiting,
        // rather than the whole approach being abandoned — or, worse, the player
        // being planted on or through the wall.
        let wall = |x, y| (x, y) != (10, 12);
        assert_eq!(
            approach_tile_in((10, 10), (10, 14), 1, wall),
            Some((10, 13))
        );
        // Already in reach there is nothing to walk to, and the answer is never
        // the target's own tile.
        assert_eq!(approach_tile_in((10, 10), (10, 11), 1, open), None);
    }

    /// The read loop re-plans the approach on every tile it commits instead of
    /// once per attack interval (live log, 2026-10-17: a player followed a
    /// raccoon for four seconds at one tile a second, always two away, because
    /// the plan was only refreshed on the swing). Re-planning needs the answer
    /// to keep being an answer.
    #[test]
    fn an_approach_can_be_replanned_from_the_tile_it_just_reached() {
        let open = |_, _| true;
        // One plan's destination becomes the next plan's starting cell, so the
        // second question has to move the player again rather than hand back
        // the tile it is already standing on.
        assert_eq!(
            approach_tile_in((10, 10), (10, 15), 1, open),
            Some((10, 11))
        );
        // The target stepped away in between — an AI tick is 100 ms against a
        // walk tile's 350 — and the same question from the new cell still has
        // an answer that is not the cell the player stands on.
        assert_eq!(approach_tile_in((10, 9), (10, 11), 1, open), Some((10, 10)));
        // And the walk ends the moment the weapon reaches: no plan, no spin.
        assert_eq!(approach_tile_in((10, 10), (10, 11), 1, open), None);
    }

    /// The two halves of [`MonsterWorld::approach_tile`] that only the shipped
    /// tables can prove: map 0 has to be kept, and a character's `class_kind` —
    /// a `KindInfo_Unique` id, where `KIND_HERO1` is 1 — has to join to its
    /// `KindInfo_Create` block mask. A join keyed on any other column leaves
    /// that missing and every approach quietly resolves to `None`, which is this
    /// whole feature silently doing nothing.
    #[test]
    fn a_real_world_approaches_on_ground_a_player_class_may_stand_on() {
        if !Path::new(DATA_ROOT).exists() {
            return; // the game data is not part of this repository
        }
        let mut world = MonsterWorld::load(Path::new(DATA_ROOT)).expect("the field data loads");
        world.spawn_all(&Registry::new());

        const HERO1: u16 = 1; // `KindInfo_Unique.txt`: 1 KIND_HERO1
        assert!(
            world.block_by_kind.contains_key(&HERO1),
            "KIND_HERO1's block mask did not join"
        );
        let map = world
            .map
            .as_ref()
            .expect("map 0 was not kept for the approach");
        let block = world.block_by_kind[&HERO1];

        let mut closed = 0usize;
        for m in &world.monsters {
            let target = (m.x, m.y);
            for &(x, y) in &world.groups[m.group].units[m.unit].tiles {
                if (x - target.0).abs().max((y - target.1).abs()) <= 1 {
                    continue; // already in reach; the swing arm never asks
                }
                // Real ground, real mask: from any safe group tile out of reach, the
                // walk ends somewhere strictly closer and never on the target — on
                // the weapon's reach when the line is clear, on the last open tile
                // before a wall when it is not. What has to hold is that *some*
                // shipped pairing closes the gap.
                let standable = |ax, ay| is_walkable(map, ax, ay, block);
                let Some(to) = approach_tile_in(target, (x, y), 1, standable) else {
                    continue;
                };
                assert_ne!(to, target, "the approach stood on its target");
                let before = (x - target.0).abs().max((y - target.1).abs());
                let after = (to.0 - target.0).abs().max((to.1 - target.1).abs());
                assert!(
                    after < before,
                    "{to:?} is no closer to {target:?} than the tile walked from"
                );
                closed += 1;
                if closed >= 4 {
                    return;
                }
            }
        }
        assert!(closed > 0, "no shipped monster could be approached at all");
    }

    /// A wall on the approach line stops the walk at the wall — it does not
    /// cancel the approach.
    ///
    /// The live report this pins: a player at (291,358) clicking a monster at
    /// (285,371) on map 0 got `OutOfRange` for seventeen consecutive seconds and
    /// never moved. The line is 13 tiles and tile 3, (288,361), is not standable,
    /// so `approach_tile_in` handed back `None` and the caller stood still —
    /// while the client happily walked its own copy of that approach.
    #[test]
    fn a_wall_on_the_approach_line_stops_at_the_wall_instead_of_nowhere() {
        let target: (i32, i32) = (0, 0);
        let from: (i32, i32) = (0, 5);

        // The whole line open: unchanged behaviour, the in-range tile comes back.
        let open = |_, _| true;
        assert_eq!(approach_tile_in(target, from, 1, open), Some((0, 1)));

        // Three tiles in, the line hits something. The player still closes the two
        // tiles that were genuinely reachable rather than staying put.
        let wall_at_third = |x: i32, y: i32| (x, y) != (0, 3);
        assert_eq!(
            approach_tile_in(target, from, 1, wall_at_third),
            Some((0, 4)),
            "the approach must return the last open tile before the wall"
        );

        // Obstructed on the very first step: nowhere to go. It must not answer with
        // the tile the player is already standing on, or the per-tick re-plan in
        // `main.rs` would build a zero-length walk forever.
        let wall_at_first = |x: i32, y: i32| (x, y) != (0, 4);
        assert_eq!(approach_tile_in(target, from, 1, wall_at_first), None);
    }

    /// The real-map form of the same thing, using the geometry the live log
    /// recorded. Skipped when the game data is not on this machine.
    #[test]
    fn the_live_out_of_range_geometry_now_has_an_answer() {
        if !Path::new(DATA_ROOT).exists() {
            return; // the game data is not part of this repository
        }
        let world = MonsterWorld::load(Path::new(DATA_ROOT)).expect("the field data loads");
        let map = world.map.as_ref().expect("map 0 kept");
        // `class 7` is the character the 2026-10-17 live run was played on; the
        // mask joined then, so it has to join now or this is a different bug.
        let block = *world
            .block_by_kind
            .get(&7)
            .expect("class 7 joins KindInfo_Create");
        let target: (i32, i32) = (285, 371);
        let from: (i32, i32) = (291, 358);
        let plan = approach_tile_in(target, from, 1, |x, y| is_walkable(map, x, y, block));
        let Some(to) = plan else {
            panic!("(288,361) is the only obstruction on that line, so an answer is owed");
        };
        assert_ne!(to, from, "the approach made no progress at all");
        let before = (from.0 - target.0).abs().max((from.1 - target.1).abs());
        let after = (to.0 - target.0).abs().max((to.1 - target.1).abs());
        assert!(after < before, "{to:?} is not closer than the start");
    }

    /// A monster's spawn life is the denominator of every overhead bar: the
    /// client's `clPB.GetMaxLife()` dispatches on `Switch_HealthAddChange`
    /// (`Parametabox.cpp:7268-7282`), and the shipped `Data\CountrySwitch.txt`
    /// sets it to `1`. The flag here is read from that file rather than
    /// restated, so this fails if the data ever ships the other value *or* if
    /// the spawn path hardcodes one — which it did, quietly leaving the server
    /// on the older `vit * VIT_LIFE_RATE` branch while the client divided by
    /// something else.
    #[test]
    fn a_monsters_spawn_life_follows_the_shipped_country_switch() {
        if !Path::new(DATA_ROOT).exists() {
            return; // the game data is not part of this repository
        }
        let root = Path::new(DATA_ROOT);
        let raw = std::fs::read(root.join("Data/CountrySwitch.txt"))
            .expect("CountrySwitch.txt ships beside KindInfo_Create.txt");
        let units: Vec<u16> = raw
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        let units = units.strip_prefix(&[0xFEFF]).unwrap_or(&units);
        let text = String::from_utf16_lossy(units);
        assert!(
            text.contains("Switch_HealthAddChange"),
            "CountrySwitch.txt no longer names the switch the bar's denominator reads"
        );
        let health_add_change = text
            .lines()
            .any(|line| line.starts_with("Switch_HealthAddChange\t1"));

        let create = kindinfo::load_kind_create(root).expect("KindInfo_Create.txt loads");
        let unique = kindinfo::load_kind_unique(root).expect("KindInfo_Unique.txt loads");
        let attack = kindinfo::canonical_char_attack();
        let create_by_code: HashMap<&str, &KindCreateRow> =
            create.iter().map(|r| (r.code.as_str(), r)).collect();
        let attack_by_code: HashMap<&str, &CharAttackRow> =
            attack.iter().map(|r| (r.code.as_str(), r)).collect();
        let code_by_id: HashMap<u16, &str> =
            unique.iter().map(|r| (r.id, r.code.as_str())).collect();

        let world = MonsterWorld::load(root).expect("the field data loads");
        assert!(!world.groups.is_empty());
        let mut checked = 0usize;
        for group in &world.groups {
            for unit in &group.units {
                let Some(&code) = code_by_id.get(&unit.wire_kind) else {
                    continue; // `MonsterWorld::load` already logged the gap
                };
                let (Some(create_row), Some(attack_row)) = (
                    create_by_code.get(code).copied(),
                    attack_by_code.get(code).copied(),
                ) else {
                    continue;
                };
                let want = MonsterStats::at(create_row, attack_row, unit.level, health_add_change);
                assert_eq!(
                    unit.life,
                    u32::try_from(want.max_hp).expect("a kind with negative life"),
                    "kind {code} at level {} spawned at the wrong max",
                    unit.level
                );
                checked += 1;
            }
        }
        assert!(checked > 0, "no kind could be re-derived at all");
    }

    /// A hit that does not kill leaves the monster where and as it was; one
    /// that does removes it and queues the respawn.
    #[test]
    fn a_lethal_hit_removes_the_monster_and_a_surviving_one_loses_life() {
        let registry = Registry::new();
        let mut world = MonsterWorld::empty();
        world.groups.push(FieldGroup {
            anchor: SpawnGroup {
                map_index: FIELD_MAP,
                center_x: 0,
                center_y: 0,
                radius: 1,
            },
            capacity: 1,
            boss: false,
            respawn_ms: RESPAWN_MS,
            units: vec![SpawnUnit {
                wire_kind: 15,
                level: 1,
                max_num: 1,
                move_speed: 12,
                life: 20,
                tiles: vec![(0, 0)],
                min_atk: 1,
                max_atk: 2,
                ac: 0,
                exp: 90,
                aggressive: 0,
                range: 1,
                attack_interval: 2_000,
                critical_rate: 0,
                dex: 0,
                battle_power: 0,
                drops: &[],
            }],
        });
        world.spawn_all(&registry);
        assert_eq!(world.monster_count(), 1);
        let unique = world.monsters[0].unique;
        let mut rng = Rng::new(7);

        let hit = world
            .hit_by_player(
                &Registry::new(),
                unique.u16(),
                1000,
                5,
                combat::SwingOutcome::Hit,
                0,
                1_000,
                &mut rng,
            )
            .expect("the monster is there");
        assert_eq!(hit.life, 15);
        assert_eq!(hit.damage, 5, "ac 0 takes nothing off");
        assert!(!hit.died);
        assert!(hit.drops.is_empty());
        assert_eq!(world.monster_count(), 1);
        // Retaliation does not wait on `siAggressive`: this unit's is 0, and
        // the struck monster still turns on who hit it.
        assert_eq!(
            world.monsters[0].target,
            Some(1000),
            "a survivor targets its attacker"
        );

        let hit = world
            .hit_by_player(
                &Registry::new(),
                unique.u16(),
                1000,
                50,
                combat::SwingOutcome::Hit,
                0,
                1_000,
                &mut rng,
            )
            .expect("the monster is still there");
        assert_eq!(hit.life, 0);
        assert!(hit.died);
        assert_eq!(hit.exp, 90, "the kind's own Exp column");
        assert_eq!(world.monster_count(), 0, "the body is deleted");

        // And the respawn comes back on the same tile. Under H3 it reuses the
        // id the corpse freed: the 11 went out at corpse removal, so the
        // client's cached avatar is gone before the id is handed out again.
        world.tick(&registry, 1_000 + RESPAWN_MS + 1, &mut rng);
        assert_eq!(world.monster_count(), 1);
        assert_eq!(world.monsters[0].unique, unique, "the id was recycled");
        assert_eq!(world.monsters[0].life, 20);
    }

    /// T7 — H3/A8: an NPC id is freed when its corpse leaves the map, so the
    /// band bounds *simultaneous* monsters rather than lifetime kills — and a
    /// spent band refuses the spawn the way the legacy's `FindEmptyCharUnique`
    /// returns 0: logged and skipped, never a panic.
    #[test]
    fn npc_ids_are_recycled_at_corpse_removal_and_a_spent_band_refuses() {
        let registry = Registry::new();
        let mut world = MonsterWorld::empty();
        world.groups.push(FieldGroup {
            anchor: SpawnGroup {
                map_index: FIELD_MAP,
                center_x: 0,
                center_y: 0,
                radius: 1,
            },
            capacity: 1,
            boss: false,
            respawn_ms: RESPAWN_MS,
            units: vec![SpawnUnit {
                wire_kind: 15,
                level: 1,
                max_num: 1,
                move_speed: 12,
                life: 20,
                tiles: vec![(0, 0)],
                min_atk: 1,
                max_atk: 2,
                ac: 0,
                exp: 90,
                aggressive: 0,
                range: 1,
                attack_interval: 2_000,
                critical_rate: 0,
                dex: 0,
                battle_power: 0,
                drops: &[],
            }],
        });
        world.spawn_all(&registry);
        assert_eq!(world.monsters.len(), 1);
        let unique = world.monsters[0].unique;
        assert!(
            unique.u16() >= gz_data::npc::NPC_CHAR_UNIQUE_BASE,
            "the boot spawn already sits inside the NPC band"
        );
        let mut rng = Rng::new(7);

        // The kill: the id leaves `monsters` but stays held by the corpse —
        // the client is still drawing the death animation under that id.
        let hit = world
            .hit_by_player(
                &registry,
                unique.u16(),
                1000,
                50,
                combat::SwingOutcome::Hit,
                0,
                1_000,
                &mut rng,
            )
            .expect("the monster is there");
        assert!(hit.died);
        assert!(
            !free_holds(&world, unique.u16()),
            "the corpse still holds the id"
        );

        // Corpse removal frees it — after the 11, which is what makes reuse
        // safe for anyone who was watching the tile.
        world.tick(&registry, 1_000 + CORPSE_MS + 1, &mut rng);
        assert!(free_holds(&world, unique.u16()));

        // The respawn hands the same id straight back out.
        world.tick(&registry, 1_000 + RESPAWN_MS + 1, &mut rng);
        assert_eq!(world.monsters.len(), 1);
        assert_eq!(world.monsters[0].unique, unique, "the id was recycled");
        assert!(!free_holds_any(&world), "nothing is left in the free lists");

        // Spent pools refuse the spawn: no panic, nothing outside the bands.
        world.ids.exhaust_standard();
        assert!(!world.spawn_one(&registry, 0, 0, (0, 0)));
        assert_eq!(world.monsters.len(), 1, "nothing was spawned");

        // A foreign id in the free list is discarded, not spawned: the top of
        // the *player* band would shadow a session under the same unique.
        world
            .ids
            .inject_free_raw(crate::registry::PLAYER_CHAR_UNIQUE_TOP as u16);
        assert!(!world.spawn_one(&registry, 0, 0, (0, 0)));
        assert!(!free_holds_any(&world), "the foreign id was discarded");
        assert_eq!(world.monsters.len(), 1);
    }

    /// Churn at the amplified field's size: every monster killed and respawned,
    /// generation after generation, comes back to the exact population — no id
    /// issued twice (live uniques stay unique), none stranded (the free lists
    /// drain empty), none refused (the population never falls short). This is
    /// the 10 s [`RESPAWN_MS`] against the 7 s [`CORPSE_MS`] at full load.
    #[test]
    fn field_scale_churn_respawns_without_id_leaks_or_exhaustion() {
        let registry = Registry::new();
        let mut world = MonsterWorld::empty();
        world.groups.push(FieldGroup {
            anchor: SpawnGroup {
                map_index: FIELD_MAP,
                center_x: 0,
                center_y: 0,
                // Wide enough that every instance owns its own tile: the
                // zero-collision rule (Cat 2026-10-21) forbids the old shared
                // 100-tile wrap this test used to fill over.
                radius: 68,
            },
            capacity: 10_000,
            boss: false,
            respawn_ms: RESPAWN_MS,
            units: (0..8)
                .map(|unit_index| SpawnUnit {
                    wire_kind: 15,
                    level: 1,
                    max_num: 576,
                    move_speed: 12,
                    life: 20,
                    // Each unit owns a disjoint 576-tile slice of the 137x137
                    // leash square, so all 4_608 instances stand on distinct
                    // coordinates — and a wander stays inside its own unit's
                    // tiles (`walkable_path`), so respawn slots stay free too.
                    tiles: (0..576)
                        .map(|j| {
                            let i = unit_index * 576 + j;
                            ((i % 137) - 68, (i / 137) - 68)
                        })
                        .collect(),
                    min_atk: 1,
                    max_atk: 2,
                    ac: 0,
                    exp: 90,
                    aggressive: 0,
                    range: 1,
                    attack_interval: 2_000,
                    critical_rate: 0,
                    dex: 0,
                    battle_power: 0,
                    drops: &[],
                })
                .collect(),
        });
        world.spawn_all(&registry);
        // 8 units x 576 = the tuned field's exact 4_608 (1_536 x 3), each unit
        // on its own slice of the leash square — one tile per monster (the
        // zero-collision rule), at population scale, inside the 11,500-id
        // standard pool.
        let population: usize = 8 * 576;
        assert_eq!(world.monsters.len(), population, "the boot fill");
        let mut rng = Rng::new(7);
        let mut now = 1_000_u64;

        for generation in 0..3 {
            assert_eq!(
                world.monsters.len(),
                population,
                "generation {generation} starts at full population"
            );
            let live: Vec<u16> = world.monsters.iter().map(|m| m.unique.u16()).collect();
            let uniques: std::collections::HashSet<u16> = live.iter().copied().collect();
            assert_eq!(uniques.len(), population, "no id serves two monsters");

            for id in live {
                let hit = world
                    .hit_by_player(
                        &registry,
                        id,
                        1000,
                        100,
                        combat::SwingOutcome::Hit,
                        0,
                        now,
                        &mut rng,
                    )
                    .expect("the monster is there");
                assert!(hit.died, "one hit clears the 20 life");
            }
            assert_eq!(world.monsters.len(), 0, "every corpse left `monsters`");

            // Corpses free first, the respawns draw second: in-flight ids never
            // exceed the population, and the free lists end empty.
            world.tick(&registry, now + CORPSE_MS + 1, &mut rng);
            world.tick(&registry, now + RESPAWN_MS + 1, &mut rng);
            assert_eq!(world.monsters.len(), population, "no spawn was refused");
            assert!(
                !free_holds_any(&world),
                "generation {generation} left an id stranded"
            );
            now += RESPAWN_MS + 100;
        }
    }

    /// A boss group's `Interval` is its respawn delay; an ordinary group's is
    /// not. The file carries seconds and the load converts once
    /// (`NPC.cpp:223`), but the death path only *honours* the window for a
    /// boss group (`NPC.cpp:862-888` gates every group, `:928` then excludes
    /// boss groups from the batch top-up and `:1077` drip-refills them) — an
    /// ordinary group's interval is a group top-up cadence, not a per-monster
    /// delay, so it keeps the fixed stand-in.
    #[test]
    fn a_boss_group_respawns_on_its_interval_and_an_ordinary_group_does_not() {
        // `at` is this group's ground: the two groups get distinct tiles —
        // one tile holds one monster (Cat's zero-collision rule, 2026-10-21).
        fn group(boss: bool, respawn_ms: u64, at: (i32, i32)) -> FieldGroup {
            FieldGroup {
                anchor: SpawnGroup {
                    map_index: FIELD_MAP,
                    center_x: 0,
                    center_y: 0,
                    radius: 1,
                },
                capacity: 1,
                boss,
                respawn_ms,
                units: vec![SpawnUnit {
                    wire_kind: 15,
                    level: 1,
                    max_num: 1,
                    move_speed: 12,
                    life: 20,
                    tiles: vec![at],
                    min_atk: 1,
                    max_atk: 2,
                    ac: 0,
                    exp: 90,
                    aggressive: 0,
                    range: 1,
                    attack_interval: 2_000,
                    critical_rate: 0,
                    dex: 0,
                    battle_power: 0,
                    drops: &[],
                }],
            }
        }

        let registry = Registry::new();
        let mut world = MonsterWorld::empty();
        world.groups.push(group(true, 60_000, (0, 0)));
        world.groups.push(group(false, 60_000, (1, 0)));
        world.spawn_all(&registry);
        assert_eq!(world.monster_count(), 2);
        let mut rng = Rng::new(7);

        // Kill the boss first, then the ordinary one, so the respawn vec's
        // order is (group 0, group 1).
        for unique in [world.monsters[0].unique, world.monsters[1].unique] {
            let hit = world
                .hit_by_player(
                    &registry,
                    unique.u16(),
                    1000,
                    9_999,
                    combat::SwingOutcome::Hit,
                    0,
                    50_000,
                    &mut rng,
                )
                .expect("the monster is there");
            assert!(hit.died);
        }
        assert_eq!(
            world.respawn,
            vec![(0, 50_000 + 60_000), (1, 50_000 + RESPAWN_MS)],
            "the boss waits out its group's window; the ordinary mob keeps the stand-in"
        );
    }

    /// A boss group whose file carries `Interval = 0` — the shipped default for
    /// most groups — falls back to the fixed stand-in rather than respawning
    /// instantly: the legacy's cooldown gate only runs when the interval is
    /// non-zero (`NPC.cpp:862`), and a zero window here must not mean "no
    /// window".
    #[test]
    fn a_boss_group_without_an_interval_uses_the_default_delay() {
        let registry = Registry::new();
        let mut world = MonsterWorld::empty();
        world.groups.push(FieldGroup {
            anchor: SpawnGroup {
                map_index: FIELD_MAP,
                center_x: 0,
                center_y: 0,
                radius: 1,
            },
            capacity: 1,
            boss: true,
            respawn_ms: 0,
            units: vec![SpawnUnit {
                wire_kind: 15,
                level: 1,
                max_num: 1,
                move_speed: 12,
                life: 20,
                tiles: vec![(0, 0)],
                min_atk: 1,
                max_atk: 2,
                ac: 0,
                exp: 90,
                aggressive: 0,
                range: 1,
                attack_interval: 2_000,
                critical_rate: 0,
                dex: 0,
                battle_power: 0,
                drops: &[],
            }],
        });
        world.spawn_all(&registry);
        let mut rng = Rng::new(7);

        let unique = world.monsters[0].unique;
        world
            .hit_by_player(
                &registry,
                unique.u16(),
                1000,
                9_999,
                combat::SwingOutcome::Hit,
                0,
                1_000,
                &mut rng,
            )
            .expect("the monster is there")
            .died
            .then_some(())
            .expect("one swing kills at ac 0");
        assert_eq!(world.respawn, vec![(0, 1_000 + RESPAWN_MS)]);
    }

    /// A kill leaves a corpse behind, and the 11 that deletes the avatar waits
    /// out [`CORPSE_MS`] so the `ORDER_DIE` the kill already sent has a client
    /// left to draw it on. Sent in the same pass the avatar is deleted before
    /// the animation can run — which is what the live report of a monster
    /// vanishing with no death animation was.
    #[test]
    fn a_killed_monster_keeps_its_corpse_until_the_linger_runs_out() {
        use crate::registry::{Outbound, SessionInfo};

        let registry = Registry::new();
        let key = Registry::next_key();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Outbound>(crate::registry::OUTBOUND_CAP);
        registry.register(
            key,
            tx,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            SessionInfo {
                char_unique: pc(1000),
                name: "Killer".to_owned(),
                level: 1,
                map_id: FIELD_MAP,
                x: 0,
                y: 0,
                school_id: 0,
                class_kind: 1,
                life: 40,
                move_speed: 12,
                sex: 1,
                gm_level: 0,
            },
        );

        let mut world = MonsterWorld::empty();
        world.groups.push(FieldGroup {
            anchor: SpawnGroup {
                map_index: FIELD_MAP,
                center_x: 0,
                center_y: 0,
                radius: 1,
            },
            capacity: 1,
            boss: false,
            respawn_ms: RESPAWN_MS,
            units: vec![SpawnUnit {
                wire_kind: 15,
                level: 1,
                max_num: 1,
                move_speed: 12,
                life: 20,
                tiles: vec![(0, 0)],
                min_atk: 1,
                max_atk: 2,
                ac: 0,
                exp: 90,
                aggressive: 0,
                range: 1,
                attack_interval: 2_000,
                critical_rate: 0,
                dex: 0,
                battle_power: 0,
                drops: &[],
            }],
        });
        world.spawn_all(&registry);
        let unique = world.monsters[0].unique;
        let mut rng = Rng::new(7);

        let hit = world
            .hit_by_player(
                &Registry::new(),
                unique.u16(),
                1000,
                50,
                combat::SwingOutcome::Hit,
                0,
                1_000,
                &mut rng,
            )
            .expect("the monster is there");
        assert!(hit.died);
        assert_eq!(
            world.monster_count(),
            0,
            "the body leaves the world at once, so nothing can walk into or target it"
        );

        let logouts = |rx: &mut tokio::sync::mpsc::Receiver<Outbound>| {
            let mut found = Vec::new();
            while let Ok(out) = rx.try_recv() {
                if out.cmd == GameMsg::ResponseLogout as u16 {
                    found.push(out);
                }
            }
            found
        };

        // One tick short of the deadline the corpse is still there.
        world.tick(&registry, 1_000 + CORPSE_MS - 1, &mut rng);
        assert!(
            logouts(&mut rx).is_empty(),
            "the avatar is deleted before its death animation could play"
        );

        // On the deadline it goes, naming the monster that died.
        world.tick(&registry, 1_000 + CORPSE_MS, &mut rng);
        let logouts = logouts(&mut rx);
        assert_eq!(logouts.len(), 1, "exactly one removal, once");
        let payload = &logouts[0].payload;
        assert_eq!(payload.len(), 4, "the 11 is one char unique");
        assert_eq!(
            i32::from_le_bytes(payload.as_slice().try_into().expect("four bytes")),
            i32::from(unique.u16())
        );
    }

    /// A monster with no aggressive range never starts a fight, and one whose
    /// target has logged out forgets it rather than swinging at the air.
    #[test]
    fn an_aggression_zero_monster_never_engages() {
        let registry = Registry::new();
        let mut world = MonsterWorld::empty();
        world.groups.push(FieldGroup {
            anchor: SpawnGroup {
                map_index: FIELD_MAP,
                center_x: 0,
                center_y: 0,
                radius: 1,
            },
            capacity: 1,
            boss: false,
            respawn_ms: RESPAWN_MS,
            units: vec![SpawnUnit {
                wire_kind: 15,
                level: 1,
                max_num: 1,
                move_speed: 12,
                life: 20,
                tiles: vec![(0, 0)],
                min_atk: 1,
                max_atk: 2,
                ac: 0,
                exp: 0,
                aggressive: 0,
                range: 1,
                attack_interval: 2_000,
                critical_rate: 0,
                dex: 0,
                battle_power: 0,
                drops: &[],
            }],
        });
        world.spawn_all(&Registry::new());

        let mut rng = Rng::new(1);
        for _ in 0..5 {
            world.tick(&registry, 0, &mut rng);
        }
        assert_eq!(
            world.monsters[0].target, None,
            "aggressive 0 means the kind never starts a fight"
        );
    }

    /// An aggressive monster with a live player next to it engages, walks into
    /// range and takes the life off. The whole loop, in one assertion.
    #[test]
    fn an_aggressive_monster_engages_a_player_and_lowers_its_life() {
        use crate::registry::{Outbound, SessionInfo};

        let registry = Registry::new();
        let key = Registry::next_key();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Outbound>(crate::registry::OUTBOUND_CAP);
        registry.register(
            key,
            tx,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            SessionInfo {
                char_unique: pc(1000),
                name: "Victim".to_owned(),
                level: 1,
                map_id: FIELD_MAP,
                x: 0,
                y: 0,
                school_id: 0,
                class_kind: 1,
                life: 40,
                move_speed: 12,
                sex: 1,
                gm_level: 0,
            },
        );

        let mut world = MonsterWorld::empty();
        world.groups.push(FieldGroup {
            anchor: SpawnGroup {
                map_index: FIELD_MAP,
                center_x: 0,
                center_y: 0,
                radius: 1,
            },
            capacity: 1,
            boss: false,
            respawn_ms: RESPAWN_MS,
            units: vec![SpawnUnit {
                wire_kind: 15,
                level: 1,
                max_num: 1,
                move_speed: 12,
                life: 20,
                tiles: vec![(0, 0)],
                min_atk: 40,
                max_atk: 40,
                ac: 0,
                exp: 0,
                aggressive: 5,
                range: 1,
                attack_interval: 2_000,
                critical_rate: 0,
                dex: 0,
                battle_power: 0,
                drops: &[],
            }],
        });
        world.spawn_all(&Registry::new());
        // Start it on its own first act so the very first tick is a real turn.
        world.monsters[0].next_act_ms = 0;

        let mut rng = Rng::new(99);
        // Engagement is a 1/25 draw per 300 ms action slot now (`char-wait.cpp:342`),
        // so the first swing lands on the first slot whose draw hits, not on the
        // first pass. Walk the slots until one does — 25 slots is the mean, 200 is
        // eight means, far past any plausible tail for this fixed seed.
        let mut hits = Vec::new();
        for slot in 0..400u64 {
            hits = world.tick(&registry, slot * AI_SLOT_MS, &mut rng);
            if !hits.is_empty() {
                break;
            }
        }
        assert_eq!(hits.len(), 1, "one swing when the draw first hits");
        assert_eq!(hits[0].key, key);
        assert_eq!(hits[0].attacker_unique, world.monsters[0].unique.u16());
        // 40 raw, level-1 AC is 0, so the full 40 lands.
        assert_eq!(hits[0].damage, 40);
        assert_eq!(hits[0].life, 0, "40 life minus 40 damage");
        assert_eq!(
            registry.info(key).expect("the session is there").life,
            0,
            "the registry carries the new total"
        );

        // The swing goes out as a 241 attack order naming the monster.
        let mut frames = Vec::new();
        while let Ok(out) = rx.try_recv() {
            frames.push(out);
        }
        let order = frames
            .iter()
            .find(|f| f.cmd == GameMsg::ResponseUserorder as u16)
            .expect("an order was broadcast");
        let decoded = OrderData::decode(&order.payload).expect("decodes");
        assert_eq!(decoded.si_order, gz_proto::order::ORDER_ATTACK);
        assert_eq!(decoded.char_unique, i32::from(hits[0].attacker_unique));
        assert_eq!(
            decoded.si_para2, 1000,
            "siPara2 is the target's char unique"
        );
    }

    /// Being hit registers the attacker as the monster's **jim host** and
    /// broadcasts the 263 that makes a client draw the overhead health bar.
    /// This, not the attack order, is the bar's trigger: its gate is
    /// `clJim.siJimHostCharUnique == pclCM->CR[1]->GetCharUnique()`
    /// (`Char-Draw.cpp:2773-2780`), and the 268 never sets that field. It is
    /// also sticky in the legacy's way — the first attacker keeps the monster
    /// and a second one gets nothing, because `SetBeJimed` refuses a second
    /// host (`Char-Common.cpp:6226`).
    #[test]
    fn being_hit_registers_the_attacker_as_the_jim_host() {
        use crate::registry::{Outbound, SessionInfo};

        fn register(
            registry: &Registry,
            char_unique: i16,
            name: &str,
        ) -> tokio::sync::mpsc::Receiver<Outbound> {
            let (tx, rx) = tokio::sync::mpsc::channel::<Outbound>(crate::registry::OUTBOUND_CAP);
            registry.register(
                Registry::next_key(),
                tx,
                std::sync::Arc::new(tokio::sync::Notify::new()),
                SessionInfo {
                    char_unique: pc(char_unique),
                    name: name.to_owned(),
                    level: 1,
                    map_id: FIELD_MAP,
                    x: 0,
                    y: 0,
                    school_id: 0,
                    class_kind: 1,
                    life: 40,
                    move_speed: 12,
                    sex: 1,
                    gm_level: 0,
                },
            );
            rx
        }

        let registry = Registry::new();
        let mut rx = register(&registry, 1000, "Attacker");

        let mut world = MonsterWorld::empty();
        world.groups.push(FieldGroup {
            anchor: SpawnGroup {
                map_index: FIELD_MAP,
                center_x: 0,
                center_y: 0,
                radius: 1,
            },
            capacity: 1,
            boss: false,
            respawn_ms: RESPAWN_MS,
            units: vec![SpawnUnit {
                wire_kind: 15,
                level: 1,
                max_num: 1,
                move_speed: 12,
                life: 20,
                tiles: vec![(0, 0)],
                min_atk: 1,
                max_atk: 1,
                ac: 0,
                exp: 0,
                aggressive: 0,
                range: 1,
                attack_interval: 2_000,
                critical_rate: 0,
                dex: 0,
                battle_power: 0,
                drops: &[],
            }],
        });
        world.spawn_all(&Registry::new());
        let unique = world.monsters[0].unique;
        let mut rng = Rng::new(1);

        world
            .hit_by_player(
                &registry,
                unique.u16(),
                1000,
                5,
                combat::SwingOutcome::Hit,
                0,
                1_000,
                &mut rng,
            )
            .expect("the hit lands");

        let mut frames = Vec::new();
        while let Ok(out) = rx.try_recv() {
            frames.push(out);
        }
        let jim = frames
            .iter()
            .find(|f| f.cmd == GameMsg::ResponseJim as u16)
            .expect("being hit broadcasts the jim frame");
        assert_eq!(
            jim.payload,
            encode_jim(
                unique.si16().expect("a standard id fits the 263 frame"),
                1000
            ),
            "siCharUnique is the monster, siJimCharUnique is the attacker"
        );
        assert_eq!(world.monsters[0].jim_host, Some(1000));

        // A second player lands a hit and hears nothing: the monster belongs
        // to whoever hit it first, so only one person ever gets the bar.
        let mut rx2 = register(&registry, 2000, "Other");
        world
            .hit_by_player(
                &registry,
                unique.u16(),
                2000,
                5,
                combat::SwingOutcome::Hit,
                0,
                1_000,
                &mut rng,
            )
            .expect("the hit lands");
        assert!(
            rx2.try_recv().is_err(),
            "a monster already jim-ed by someone else broadcasts nothing"
        );
        assert_eq!(
            world.monsters[0].jim_host,
            Some(1000),
            "and it stays the first attacker's"
        );
    }

    /// A player who switches targets inside a stack takes the old monster's
    /// bar down: the port sends the previously jimmned monster a 263 with the
    /// host field cleared — the only frame that can undo a bar, since the
    /// client's handler `clJim.Init()`s before `SetBeJimed` and the draw gate
    /// re-reads the host every frame.
    #[test]
    fn switching_targets_un_jims_the_previous_monster() {
        use crate::registry::{Outbound, SessionInfo};

        fn register(
            registry: &Registry,
            char_unique: i16,
            name: &str,
        ) -> tokio::sync::mpsc::Receiver<Outbound> {
            let (tx, rx) = tokio::sync::mpsc::channel::<Outbound>(crate::registry::OUTBOUND_CAP);
            registry.register(
                Registry::next_key(),
                tx,
                std::sync::Arc::new(tokio::sync::Notify::new()),
                SessionInfo {
                    char_unique: pc(char_unique),
                    name: name.to_owned(),
                    level: 1,
                    map_id: FIELD_MAP,
                    x: 0,
                    y: 0,
                    school_id: 0,
                    class_kind: 1,
                    life: 40,
                    move_speed: 12,
                    sex: 1,
                    gm_level: 0,
                },
            );
            rx
        }

        let registry = Registry::new();
        let mut rx = register(&registry, 1000, "Attacker");

        let mut world = MonsterWorld::empty();
        world.groups.push(FieldGroup {
            anchor: SpawnGroup {
                map_index: FIELD_MAP,
                center_x: 0,
                center_y: 0,
                radius: 1,
            },
            capacity: 2,
            boss: false,
            respawn_ms: RESPAWN_MS,
            units: vec![SpawnUnit {
                wire_kind: 15,
                level: 1,
                max_num: 2,
                move_speed: 12,
                life: 20,
                tiles: vec![(0, 0), (1, 0)],
                min_atk: 1,
                max_atk: 1,
                ac: 0,
                exp: 0,
                aggressive: 0,
                range: 1,
                attack_interval: 2_000,
                critical_rate: 0,
                dex: 0,
                battle_power: 0,
                drops: &[],
            }],
        });
        world.spawn_all(&Registry::new());
        assert_eq!(world.monsters.len(), 2, "the group spawns both racers");
        let first = world.monsters[0].unique;
        let second = world.monsters[1].unique;
        let first_wire = first.si16().expect("a standard id fits the 263 frame");
        let second_wire = second.si16().expect("a standard id fits the 263 frame");
        let mut rng = Rng::new(1);

        world
            .hit_by_player(
                &registry,
                first.u16(),
                1000,
                5,
                combat::SwingOutcome::Hit,
                0,
                1_000,
                &mut rng,
            )
            .expect("the hit lands");
        let mut frames = Vec::new();
        while let Ok(out) = rx.try_recv() {
            frames.push(out);
        }
        assert_eq!(
            frames
                .iter()
                .filter(|f| f.cmd == GameMsg::ResponseJim as u16)
                .count(),
            1,
            "the first hit jims the first monster and nothing else"
        );

        world
            .hit_by_player(
                &registry,
                second.u16(),
                1000,
                5,
                combat::SwingOutcome::Hit,
                0,
                1_000,
                &mut rng,
            )
            .expect("the hit lands");
        let mut frames = Vec::new();
        while let Ok(out) = rx.try_recv() {
            frames.push(out);
        }
        let jims: Vec<_> = frames
            .iter()
            .filter(|f| f.cmd == GameMsg::ResponseJim as u16)
            .collect();
        assert_eq!(
            jims.len(),
            2,
            "switching targets sends the un-jim and the new jim"
        );
        assert!(
            jims.iter().any(|f| f.payload == encode_jim(first_wire, 0)),
            "the previous monster is jimmned with the host field cleared"
        );
        assert!(
            jims.iter()
                .any(|f| f.payload == encode_jim(second_wire, 1000)),
            "and the new monster gets the bar"
        );
        assert_eq!(
            world.monsters[0].jim_host, None,
            "the previous bar is released"
        );
        assert_eq!(
            world.monsters[1].jim_host,
            Some(1000),
            "and the new one holds it"
        );
    }

    /// A monster already in a fight swings on its kind's own
    /// `Attack_Interval`, not on the wander gate. The legacy keeps those two
    /// clocks apart — `char-wait.cpp:315-406` decides *whether* to look for a
    /// fight, `char-attack.cpp:1869` decides *when* the weapon fires — and the
    /// client repeats this monster's attack animation on that same interval
    /// (`dwAttackInterval`), so a hit paced by the idle slot lands about three
    /// animations after the swing that shows it. That is the live report: "the
    /// raccoon plays its attack animation 3-4 times before a single damage
    /// number registers".
    ///
    /// The swing is detected on the 100 ms AI tick, so it can land up to that
    /// late; `2_100` below is `attack_interval` rounded up to the next tick.
    #[test]
    fn an_engaged_monster_swings_on_its_attack_interval_not_the_wander_gate() {
        use crate::registry::{Outbound, SessionInfo};

        let registry = Registry::new();
        let key = Registry::next_key();
        let (tx, _rx) = tokio::sync::mpsc::channel::<Outbound>(crate::registry::OUTBOUND_CAP);
        registry.register(
            key,
            tx,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            SessionInfo {
                char_unique: pc(1000),
                name: "Victim".to_owned(),
                level: 1,
                map_id: FIELD_MAP,
                x: 0,
                y: 0,
                school_id: 0,
                class_kind: 1,
                // Comfortably above the three swings this test lands, so the
                // target never dies and drops out of the fight half way through.
                life: 400,
                move_speed: 12,
                sex: 1,
                gm_level: 0,
            },
        );

        let mut world = MonsterWorld::empty();
        world.groups.push(FieldGroup {
            anchor: SpawnGroup {
                map_index: FIELD_MAP,
                center_x: 0,
                center_y: 0,
                radius: 1,
            },
            capacity: 1,
            boss: false,
            respawn_ms: RESPAWN_MS,
            units: vec![SpawnUnit {
                wire_kind: 15,
                level: 1,
                max_num: 1,
                move_speed: 12,
                life: 20,
                tiles: vec![(0, 0)],
                min_atk: 40,
                max_atk: 40,
                ac: 0,
                exp: 0,
                aggressive: 5,
                range: 1,
                attack_interval: 2_000,
                critical_rate: 0,
                dex: 0,
                battle_power: 0,
                drops: &[],
            }],
        });
        world.spawn_all(&registry);

        let mut rng = Rng::new(99);
        // Engagement is a 1/25 draw per 300 ms action slot, so anchor every
        // timing assertion on the slot where this seed's stream first hits and
        // the fight opens.
        let mut t0 = None;
        for slot in 0..400u64 {
            let now = slot * AI_SLOT_MS;
            if world.tick(&registry, now, &mut rng).len() == 1 {
                t0 = Some(now);
                break;
            }
        }
        let t0 = t0.expect("a 1/25 draw lands well inside 400 slots");
        // Half way through the interval the act gate has long since opened —
        // `next_act_ms` is on `AI_TICK_MS` while a monster has a target — but
        // the weapon has not recovered.
        assert_eq!(
            world.tick(&registry, t0 + 1_000, &mut rng).len(),
            0,
            "the interval is only half spent"
        );
        // One AI tick past the kind's own deadline: another swing. Under the
        // old fixed gate this pass fell at 6 250 ms and there would be none
        // here.
        assert_eq!(
            world.tick(&registry, t0 + 2_100, &mut rng).len(),
            1,
            "Attack_Interval has elapsed"
        );
        assert_eq!(world.tick(&registry, t0 + 3_000, &mut rng).len(), 0);
        // The clock restarts from the swing itself, not from the act gate.
        assert_eq!(
            world.tick(&registry, t0 + 4_200, &mut rng).len(),
            1,
            "the second interval runs from the second swing"
        );
        assert_eq!(
            registry.info(key).expect("the session is there").life,
            400 - 3 * 40,
            "every swing landed"
        );
    }

    /// The idle decision is the legacy's `1/25` draw per 300 ms action slot
    /// (`char-wait.cpp:342`), not the fixed 6,250 ms beat this port used
    /// before. One world per seed, first slot whose tick produces a swing.
    /// A fixed beat would put every seed's first decision on the same slot and
    /// the same wall time; a geometric draw spreads them with a mean near 25
    /// slots (7,500 ms).
    ///
    /// Deliberately not measured through `wander`: [`Self::advance_walks`]
    /// accrues walk credit from `Instant::now()`, so in a tight test loop a
    /// started walk never finishes and every later draw dies on the
    /// already-walking guard. Engagement is the observable that does not
    /// depend on real time.
    #[test]
    fn the_idle_decision_is_a_one_in_twenty_five_draw_per_action_slot() {
        use crate::registry::{Outbound, SessionInfo};

        const SEEDS: u64 = 200;

        let mut first_swing_slot = Vec::new();
        for seed in 1..=SEEDS {
            let registry = Registry::new();
            let key = Registry::next_key();
            let (tx, _rx) = tokio::sync::mpsc::channel::<Outbound>(crate::registry::OUTBOUND_CAP);
            registry.register(
                key,
                tx,
                std::sync::Arc::new(tokio::sync::Notify::new()),
                SessionInfo {
                    char_unique: pc(1000),
                    name: "Victim".to_owned(),
                    level: 1,
                    map_id: FIELD_MAP,
                    x: 0,
                    y: 0,
                    school_id: 0,
                    class_kind: 1,
                    life: 400,
                    move_speed: 12,
                    sex: 1,
                    gm_level: 0,
                },
            );
            let mut world = MonsterWorld::empty();
            world.groups.push(FieldGroup {
                anchor: SpawnGroup {
                    map_index: FIELD_MAP,
                    center_x: 0,
                    center_y: 0,
                    radius: 1,
                },
                capacity: 1,
                boss: false,
                respawn_ms: RESPAWN_MS,
                units: vec![SpawnUnit {
                    wire_kind: 15,
                    level: 1,
                    max_num: 1,
                    move_speed: 12,
                    life: 20,
                    tiles: vec![(0, 0)],
                    min_atk: 40,
                    max_atk: 40,
                    ac: 0,
                    exp: 0,
                    aggressive: 5,
                    range: 1,
                    attack_interval: 2_000,
                    critical_rate: 0,
                    dex: 0,
                    battle_power: 0,
                    drops: &[],
                }],
            });
            world.spawn_all(&registry);
            world.monsters[0].next_act_ms = 0;

            let mut rng = Rng::new(seed);
            let mut slot = None;
            for s in 0..400u64 {
                if world.tick(&registry, s * AI_SLOT_MS, &mut rng).len() == 1 {
                    slot = Some(s);
                    break;
                }
            }
            first_swing_slot.push(slot.expect("a 1/25 draw lands well inside 400 slots"));
        }

        // A fixed 6,250 ms deadline arms the same wall time for every monster,
        // so under it every seed here would read the same slot. The draw does
        // not.
        let distinct: std::collections::HashSet<u64> = first_swing_slot.iter().copied().collect();
        assert!(
            distinct.len() > 10,
            "first decisions spread over many slots: {:?}",
            first_swing_slot
        );
        let mean: f64 = first_swing_slot.iter().sum::<u64>() as f64 / SEEDS as f64;
        assert!(
            (18.0..=32.0).contains(&mean),
            "the mean first decision sits near the legacy's 25 slots: {mean}"
        );
        assert!(
            first_swing_slot.iter().filter(|s| **s >= 100).count() <= 12,
            "the geometric tail keeps late first decisions rare (expected ~3.4 of {SEEDS} beyond slot 100, Poisson): {:?}",
            first_swing_slot
        );
    }

    /// The 1 -> 0 edge opens a grace window, it does not freeze the map: the
    /// map is still awake one millisecond before the deadline and hibernating
    /// at the deadline itself.
    #[test]
    fn a_map_is_not_frozen_on_the_last_player_leaving_but_hibernates_after_the_window() {
        let mut dormancy = MapDormancyManager::new();
        dormancy.update(FIELD_MAP, 1, 0);
        assert!(!dormancy.is_dormant(FIELD_MAP), "players keep it awake");

        dormancy.update(FIELD_MAP, 0, 1_000);
        assert!(!dormancy.is_dormant(FIELD_MAP), "grace, not hibernation");
        dormancy.update(FIELD_MAP, 0, 1_000 + DORMANCY_GRACE_MS - 1);
        assert!(!dormancy.is_dormant(FIELD_MAP), "the window is still open");
        dormancy.update(FIELD_MAP, 0, 1_000 + DORMANCY_GRACE_MS);
        assert!(
            dormancy.is_dormant(FIELD_MAP),
            "and it closes on the deadline"
        );
    }

    /// A player entering inside the grace window cancels the countdown
    /// seamlessly, and a later empty edge opens a *fresh* window from that
    /// edge — not the leftover deadline.
    #[test]
    fn a_player_entering_inside_the_grace_window_cancels_the_countdown() {
        let mut dormancy = MapDormancyManager::new();
        dormancy.update(FIELD_MAP, 0, 0);
        assert!(!dormancy.is_dormant(FIELD_MAP), "the window just opened");

        dormancy.update(FIELD_MAP, 1, DORMANCY_GRACE_MS - 1);
        assert!(!dormancy.is_dormant(FIELD_MAP), "someone warped back in");
        dormancy.update(FIELD_MAP, 0, DORMANCY_GRACE_MS - 1);
        assert!(
            !dormancy.is_dormant(FIELD_MAP),
            "a fresh window from this edge"
        );

        // The old deadline (30_000) would have hibernated the map by now; the
        // fresh one, opened at 29_999, runs to 59_999.
        dormancy.update(FIELD_MAP, 0, 2 * DORMANCY_GRACE_MS - 2);
        assert!(
            !dormancy.is_dormant(FIELD_MAP),
            "the fresh window is still open"
        );
        dormancy.update(FIELD_MAP, 0, 2 * DORMANCY_GRACE_MS - 1);
        assert!(
            dormancy.is_dormant(FIELD_MAP),
            "and it closes 30 s after the re-entry"
        );
    }

    /// Hibernation ends the moment a player enters — waking is not lazy — and
    /// the awakened map is a grace-protected awake, not a hair trigger.
    #[test]
    fn hibernation_ends_the_moment_a_player_enters() {
        let mut dormancy = MapDormancyManager::new();
        dormancy.update(FIELD_MAP, 0, 0);
        dormancy.update(FIELD_MAP, 0, DORMANCY_GRACE_MS);
        assert!(dormancy.is_dormant(FIELD_MAP), "the window ran out");

        dormancy.update(FIELD_MAP, 1, DORMANCY_GRACE_MS + 60_000);
        assert!(!dormancy.is_dormant(FIELD_MAP), "waking is immediate");
        dormancy.update(FIELD_MAP, 0, DORMANCY_GRACE_MS + 60_000);
        assert!(
            !dormancy.is_dormant(FIELD_MAP),
            "a new window opens, not instant freeze"
        );
    }

    /// An unseen map is awake by definition, and a map with players on it is
    /// never dormant however long it runs.
    #[test]
    fn an_unseen_map_and_an_occupied_map_are_never_dormant() {
        let mut dormancy = MapDormancyManager::new();
        assert!(!dormancy.is_dormant(7), "an unseen map is awake by default");

        dormancy.update(7, 3, 0);
        for now_ms in [1_000, 100_000, 10_000_000] {
            dormancy.update(7, 2, now_ms);
            assert!(
                !dormancy.is_dormant(7),
                "players keep a map awake at {now_ms}"
            );
        }
        // Only the empty edge opens a window at all.
        dormancy.update(7, 0, 10_000_000);
        assert!(!dormancy.is_dormant(7), "and even then, grace first");
    }

    /// End to end through [`MonsterWorld::tick`]: a map with nobody on it opens
    /// its grace window at the first tick and keeps acting through it (the
    /// monster still wanders), freezes at the deadline (position and walk
    /// never change again), and a returning player wakes it — the monster
    /// closes in and swings again.
    #[test]
    fn a_hibernating_map_is_skipped_and_resumes_when_a_player_returns() {
        use crate::registry::{Outbound, SessionInfo};

        let registry = Registry::new();
        let mut world = MonsterWorld::empty();
        world.groups.push(FieldGroup {
            anchor: SpawnGroup {
                map_index: FIELD_MAP,
                center_x: 0,
                center_y: 0,
                radius: 3,
            },
            capacity: 1,
            boss: false,
            respawn_ms: RESPAWN_MS,
            units: vec![SpawnUnit {
                wire_kind: 15,
                level: 1,
                max_num: 1,
                move_speed: 12,
                life: 20,
                tiles: vec![(0, 0), (1, 0), (2, 0), (3, 0), (4, 0)],
                min_atk: 40,
                max_atk: 40,
                ac: 0,
                exp: 0,
                aggressive: 5,
                range: 1,
                attack_interval: 2_000,
                critical_rate: 0,
                dex: 0,
                battle_power: 0,
                drops: &[],
            }],
        });
        world.spawn_all(&registry);
        world.monsters[0].x = 0;
        world.monsters[0].y = 0;
        world.monsters[0].next_act_ms = 0;

        let mut rng = Rng::new(7);
        // No session anywhere: the map opens its grace window at the very
        // first tick, and grace means alive — the monster still wanders.
        // (Seed 7's first gate-hit lands at slot 10; seed 5's lands at slot 95
        // and its wander pick selects the monster's own tile, so it walks
        // never.)
        let mut walked_while_grace = false;
        for slot in 0..100u64 {
            world.tick(&registry, slot * AI_SLOT_MS, &mut rng);
            walked_while_grace |= world.monsters[0].walk.is_some();
        }
        assert!(
            walked_while_grace,
            "grace keeps the map alive: the monster still wanders"
        );

        // The window closes at exactly slot 100's timestamp. The loop's own
        // re-arm stamp is the crisp observable of the skip: during grace every
        // pass re-arms `next_act_ms` to `now + AI_SLOT_MS` (it reads exactly
        // the boundary here), and once the map hibernates the loop `continue`s
        // before that re-arm, so the stamp freezes. Position and walk state are
        // deliberately *not* asserted frozen: `advance_walks` accrues credit
        // from `Instant::now()`, which a tight test loop never grants, so a
        // started walk sits unfinished no matter what the guard did — the trap
        // documented on the AI-distribution test.
        world.tick(&registry, 100 * AI_SLOT_MS, &mut rng);
        assert_eq!(
            world.monsters[0].next_act_ms,
            100 * AI_SLOT_MS,
            "grace re-armed the deadline right up to the boundary"
        );
        for extra in 0..200u64 {
            let now = 100 * AI_SLOT_MS + (extra + 1) * AI_TICK_MS;
            world.tick(&registry, now, &mut rng);
            assert_eq!(
                world.monsters[0].next_act_ms,
                100 * AI_SLOT_MS,
                "a hibernating map is skipped before the re-arm: the stamp freezes"
            );
        }

        // A player returns: the map wakes, the idle draw resumes, and the
        // monster — co-located with the returner, since the tight loop never
        // let its grace wander commit a tile — engages and swings. The stale
        // grace walk is dropped by hand: a real server finishes it in
        // wall-clock time, which a tight loop never grants.
        world.monsters[0].walk = None;
        let key = Registry::next_key();
        let (tx, _rx) = tokio::sync::mpsc::channel::<Outbound>(crate::registry::OUTBOUND_CAP);
        registry.register(
            key,
            tx,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            SessionInfo {
                char_unique: pc(1000),
                name: "Returner".to_owned(),
                level: 1,
                map_id: FIELD_MAP,
                x: 0,
                y: 0,
                school_id: 0,
                class_kind: 1,
                life: 400,
                move_speed: 12,
                sex: 1,
                gm_level: 0,
            },
        );
        let mut resumed = false;
        for slot in 0..400u64 {
            let now = 100 * AI_SLOT_MS + 200 * AI_TICK_MS + slot * AI_SLOT_MS;
            if world.tick(&registry, now, &mut rng).len() == 1 {
                resumed = true;
                break;
            }
        }
        assert!(resumed, "the map woke and the monster fought again");
    }

    /// `wander` must not re-pick while its last walk is still running: it
    /// would rebuild `Movement` from zero and discard the tiles already banked,
    /// so a long wander ends up walked in place forever. The out-of-range
    /// branch of `act` carries the same guard, and between them they make a
    /// monster's `walk` a walk it is allowed to finish.
    #[test]
    fn a_wander_will_not_rebuild_a_walk_it_has_not_finished() {
        let registry = Registry::new();
        let mut world = MonsterWorld::empty();
        world.groups.push(FieldGroup {
            anchor: SpawnGroup {
                map_index: FIELD_MAP,
                center_x: 0,
                center_y: 0,
                radius: 1,
            },
            capacity: 1,
            boss: false,
            respawn_ms: RESPAWN_MS,
            units: vec![SpawnUnit {
                wire_kind: 15,
                level: 1,
                max_num: 1,
                move_speed: 12,
                life: 20,
                tiles: vec![(0, 0), (1, 0), (2, 0)],
                min_atk: 1,
                max_atk: 2,
                ac: 0,
                exp: 0,
                aggressive: 0,
                range: 1,
                attack_interval: 2_000,
                critical_rate: 0,
                dex: 0,
                battle_power: 0,
                drops: &[],
            }],
        });
        world.spawn_all(&registry);

        // Stand the spawn where `wander` has all three tiles to choose from,
        // whatever the spawn picker landed on.
        world.monsters[0].x = 0;
        world.monsters[0].y = 0;

        let mut rng = Rng::new(1);
        world.order_move(&registry, 0, (2, 0), 0);
        let running = world.monsters[0].walk.expect("the walk started");

        // Sixteen picks would include tiles other than (2, 0), so the assertion
        // catches a re-issue rather than passing on the draw it happened to get.
        for roll in 0..16 {
            world.wander(&registry, 0, 100, &mut rng);
            assert_eq!(
                world.monsters[0].walk,
                Some(running),
                "wander rebuilt the running walk on roll {roll}"
            );
        }
    }
}
