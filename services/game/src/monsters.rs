//! Field-monster spawning on map 0 — Phase 1 of
//! `docs\DESIGN_MONSTER_COMBAT_AND_AUTOHUNT.md` §10.2.
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
//! # What this is not
//!
//! No combat and no wandering: an instance appears, stands where it spawned, and
//! is never removed, so there is no death, no drop, and no respawn timer yet.
//! The group's `Interval`/`BOSS` columns are parsed but unused for the same
//! reason, and `MaxNPCNum` is only the top-up gate (see [`MonsterWorld::spawn_all`]).
//! Positions are a deterministic spread, not the legacy's random pick, so the
//! field looks the same across restarts — which the client's `CharUniqueInfo`
//! cache rewards.

use std::collections::HashMap;
use std::path::Path;

use gz_data::kindinfo::{self, CharAttackRow, KindCreateRow, MonsterStats};
use gz_data::map::MapFile;
use gz_data::npc::{NPC_CHAR_UNIQUE_BASE, NPC_PLACEMENTS};
use gz_data::npcgroup;
use gz_data::spawn::{SpawnField, SpawnGroup};
use gz_proto::game::client_map_index;
use gz_proto::loginmany::{LoginManyRecord, NpcRecord, encode_login_many_batches_mixed};
use gz_proto::map_geometry::{cell_of, cells_adjacent};
use gz_proto::opcodes_game::GameMsg;

use crate::registry::{Registry, SessionKey};
use crate::{logln, logln_err};

/// The one map this phase spawns: the main field (`map.h:67`).
pub(crate) const FIELD_MAP: i32 = 0;

/// `MAX_CHAR_NUMBER` (`CommonHeader.h:639`) — the exclusive top of the client's
/// NPC id range, so a monster id must stay below it (`IsNPC` is `8500..20000`).
const MAX_CHAR_NUMBER: u32 = 20_000;

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
    /// [`SpawnField::spawn_points`]. Sorted by `(x, y)`.
    tiles: Vec<(i32, i32)>,
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
    units: Vec<SpawnUnit>,
}

/// One live monster instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Monster {
    /// `uiMakeCharUnique` — inside the client's `IsNPC` range, and never one of
    /// the static NPCs' ids.
    unique: u16,
    wire_kind: u16,
    level: u8,
    life: u32,
    x: i32,
    y: i32,
    move_speed: u8,
    /// Index into [`MonsterWorld::groups`] — the group this instance belongs to,
    /// so a later tick can count it against the group's capacity.
    group: usize,
}

/// Map 0's monster population: the loaded groups and the instances they spawned.
#[derive(Debug, Default)]
pub(crate) struct MonsterWorld {
    groups: Vec<FieldGroup>,
    monsters: Vec<Monster>,
}

impl MonsterWorld {
    /// An empty world — what a failed load degrades to, so a missing data tree
    /// does not stop the server (the same posture the quest load takes).
    #[must_use]
    pub(crate) fn empty() -> Self {
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

        let create_by_code: HashMap<&str, &KindCreateRow> =
            create.iter().map(|r| (r.code.as_str(), r)).collect();
        let id_by_code: HashMap<&str, u16> =
            unique.iter().map(|r| (r.code.as_str(), r.id)).collect();
        let attack_by_code: HashMap<&str, &CharAttackRow> =
            attack.iter().map(|r| (r.code.as_str(), r)).collect();

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
                // `health_add_change` off is the legacy default branch
                // (`MonsterStats::at`); the port has no country-switch table.
                let stats = MonsterStats::at(create_row, attack_row, unit.level, false);
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
                    max_num: unit.max_num,
                    move_speed: stats.move_speed.clamp(0, 63) as u8,
                    life,
                    tiles,
                });
            }
            if !units.is_empty() {
                out.push(FieldGroup {
                    anchor,
                    capacity: group.max_npc_num,
                    units,
                });
            }
        }
        Ok(Self {
            groups: out,
            monsters: Vec::new(),
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
        // Monsters number from just above the static-NPC block, so the two id
        // ranges cannot overlap (`IsNPC` covers both, `npc.rs`).
        let mut next = u32::from(NPC_CHAR_UNIQUE_BASE) + NPC_PLACEMENTS.len() as u32;
        for (index, group) in self.groups.iter().enumerate() {
            let live = self.monsters.iter().filter(|m| m.group == index).count() as i32;
            if live >= group.capacity {
                continue;
            }
            for unit in &group.units {
                let want = unit.max_num.max(0) as usize;
                // shortcut: a deterministic stride across the sorted safe tiles,
                // not the legacy's random `FindEmptyAreaForCharLocate`. Upgrade
                // when roaming/respawn makes placement density matter.
                let step = (unit.tiles.len() / want.max(1)).max(1);
                for k in 0..want {
                    if next >= MAX_CHAR_NUMBER {
                        logln_err!(
                            "monsters: NPC id range exhausted at {} monster(s)",
                            self.monsters.len()
                        );
                        return;
                    }
                    let (x, y) = unit.tiles[(k * step).min(unit.tiles.len() - 1)];
                    // The leash, enforced where it can be: `spawn_points` only
                    // returns the group's square, and this states the invariant
                    // rather than trusting it. (`SpawnGroup::return_home` is the
                    // hook the movement phase will use to walk a stray back.)
                    debug_assert!(
                        group.anchor.contains(FIELD_MAP, x, y),
                        "monster placed outside its group's leash"
                    );
                    let monster = Monster {
                        unique: next as u16,
                        wire_kind: unit.wire_kind,
                        level: unit.level.clamp(0, 255) as u8,
                        life: unit.life,
                        x,
                        y,
                        move_speed: unit.move_speed,
                        group: index,
                    };
                    next += 1;
                    self.monsters.push(monster);
                    broadcast_spawn(registry, FIELD_MAP, &monster);
                }
            }
        }
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

/// The wire record for one monster — the same `cltMakeCharInfoCommon` shape a
/// static NPC is announced with (`npc_record_for`, `world.rs`), with the kind's
/// move speed so a monster is distinguishable from a standing NPC.
fn record_for(map_id: i32, monster: &Monster) -> NpcRecord {
    NpcRecord {
        make_char_kind: monster.wire_kind,
        make_char_unique: monster.unique,
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
        assert_eq!(
            world.monster_count(),
            1_536,
            "the shipped map-0 unit targets"
        );

        let npc_top = u32::from(NPC_CHAR_UNIQUE_BASE) + NPC_PLACEMENTS.len() as u32;
        let mut seen = std::collections::HashSet::new();
        for m in &world.monsters {
            assert!(
                u32::from(m.unique) >= npc_top && u32::from(m.unique) < MAX_CHAR_NUMBER,
                "id {} outside the monster block",
                m.unique
            );
            assert!(seen.insert(m.unique), "duplicate id {}", m.unique);
            assert!(m.life > 0, "zero-life monster {}", m.unique);
            assert!(m.move_speed <= 63);
            assert!((0..800).contains(&m.x) && (0..800).contains(&m.y));
        }
    }

    #[test]
    fn spawn_all_fills_each_unit_to_its_target_and_the_capacity_gate_stops_a_second_pass() {
        let registry = Registry::new();
        let base = u32::from(NPC_CHAR_UNIQUE_BASE) + NPC_PLACEMENTS.len() as u32;
        let tiles: Vec<(i32, i32)> = (0..10).map(|i| (10 + i, 10)).collect();
        let mut world = MonsterWorld {
            groups: vec![FieldGroup {
                anchor: SpawnGroup {
                    map_index: FIELD_MAP,
                    center_x: 10,
                    center_y: 10,
                    radius: 20,
                },
                // Capacity 3 with a unit target of 10: from empty the group is
                // below capacity, so the unit fills to its target and overshoots
                // `MaxNPCNum` — the top-up gate, not a cap (`NPC.cpp:925`).
                capacity: 3,
                units: vec![SpawnUnit {
                    wire_kind: 24,
                    level: 7,
                    max_num: 10,
                    move_speed: 5,
                    life: 40,
                    tiles: tiles.clone(),
                }],
            }],
            monsters: Vec::new(),
        };
        world.spawn_all(&registry);

        assert_eq!(world.monster_count(), 10);
        for m in &world.monsters {
            assert_eq!((m.wire_kind, m.level, m.life, m.move_speed), (24, 7, 40, 5));
            assert_eq!(m.group, 0);
            assert!(tiles.contains(&(m.x, m.y)), "spawned off the safe tiles");
        }
        let ids: Vec<u32> = world.monsters.iter().map(|m| u32::from(m.unique)).collect();
        let expected: Vec<u32> = (0..10).map(|i| base + i).collect();
        assert_eq!(ids, expected);

        // A second tick finds the group at 10 live >= its capacity of 3 and
        // leaves it alone.
        world.spawn_all(&registry);
        assert_eq!(
            world.monster_count(),
            10,
            "a full group is not topped up again"
        );
    }

    #[test]
    fn the_record_is_one_the_client_can_walk() {
        let monster = Monster {
            unique: 9001,
            wire_kind: 24,
            level: 7,
            life: 40,
            x: 246,
            y: 332,
            move_speed: 5,
            group: 0,
        };
        let record = record_for(FIELD_MAP, &monster);
        assert_eq!(record.map_index, 0);
        assert_eq!(record.total_move_speed, 5);
        // The encoder's own decoder is the oracle that the bytes round-trip.
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
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Outbound>();
        registry.register(
            key,
            tx,
            SessionInfo {
                char_unique: 1473,
                name: "Tester".to_owned(),
                level: 13,
                map_id: FIELD_MAP,
                x: 246,
                y: 332,
                school_id: 0,
                class_kind: 1,
                life: 161,
                move_speed: 12,
                sex: 1,
                gm_level: 0,
            },
        );

        // One monster the walk reaches, one it does not.
        let (near_x, near_y) = (246, 332);
        let (far_x, far_y) = (700, 700);
        let world = MonsterWorld {
            groups: Vec::new(),
            monsters: vec![
                Monster {
                    unique: 9001,
                    wire_kind: 24,
                    level: 7,
                    life: 40,
                    x: near_x,
                    y: near_y,
                    move_speed: 5,
                    group: 0,
                },
                Monster {
                    unique: 9002,
                    wire_kind: 24,
                    level: 7,
                    life: 40,
                    x: far_x,
                    y: far_y,
                    move_speed: 5,
                    group: 0,
                },
            ],
        };

        // Stepping from the far cell into the near one: the near monster is new,
        // the far one is not re-sent.
        world.announce_new_cells(
            &registry,
            key,
            FIELD_MAP,
            cell_of(far_x, far_y, FIELD_MAP),
            cell_of(near_x, near_y, FIELD_MAP),
        );
        let frame = rx.try_recv().expect("the reached monster is announced");
        assert_eq!(frame.cmd, GameMsg::ResponseLoginmany as u16);
        let record = NpcRecord::decode(&frame.payload).expect("the records decode");
        assert_eq!(record.make_char_unique, 9001);
        assert!(
            rx.try_recv().is_err(),
            "the monster outside the gained cells is not announced"
        );

        // Standing still inside the block it already holds hands out nothing.
        world.announce_new_cells(
            &registry,
            key,
            FIELD_MAP,
            cell_of(near_x, near_y, FIELD_MAP),
            cell_of(near_x, near_y, FIELD_MAP),
        );
        assert!(
            rx.try_recv().is_err(),
            "a monster already in view is not re-sent"
        );
    }
}
