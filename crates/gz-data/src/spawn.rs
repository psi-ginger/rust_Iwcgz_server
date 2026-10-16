//! Safe spawn points and the roam leash for monster groups.
//!
//! A monster group is one `NPCGroup` row: a map, a centre tile and a radius
//! (`common\NPCManager\NPC.cpp:203-213`, header columns
//! `NPCGroupID MaxNPCNum MapIndex CenterX CenterY Radius Interval BOSS`). The
//! legacy parses that into `cltNPCGroup` (`:223`) and trusts the data: its own
//! placement search is the expanding ring of `cltMapCommon::
//! FindEmptyAreaForCharLocate` (`common\Map\MapCommon.cpp:853-904`), which
//! takes the *first* non-colliding tile around the origin and never asks
//! whether that tile can be walked *to*. This module is the stricter port the
//! rewrite needs: no tile is handed out that a player cannot reach.
//!
//! # Walkability ([`is_walkable`])
//!
//! One rule, the legacy's movement rule: a cell is passable for a kind when
//! none of the cell's collision classes is in the kind's block mask —
//! `cltMapCommon::IsColl(x, y, block)` is `pCollMap[..] & block` (`map.cpp:
//! 257-265`), `cltCharCommon::Block = siBlock | BLOCK_SYSTEMNPC`
//! (`Char-Common.cpp:731`), and a move step only happens when that test is
//! false (`char-move.cpp:162`). Out of map is always blocked (`map.cpp:260`).
//!
//! The cell classes themselves are baked into the shipped `CollMap` by the
//! map editor/loader: walls, trees, buildings → `BLOCK_NORMAL`, water →
//! `BLOCK_SHALLOWWATER`/`BLOCK_DEEPWATER`, and every non-water cell carries
//! `BLOCK_LAND` (`MapCommon.cpp:589-640`). A kind's mask is its `siblock`
//! from `KindInfo_Create.txt` (see [`crate::kindinfo`]), so a normal land
//! monster (`BLOCK_NORMAL|...|BLOCK_CUSTOM`) is stopped by walls and deep
//! water while a water kind, whose mask carries `BLOCK_LAND`, is stopped by
//! land — the terrain rules fall out of the one mask.
//!
//! Runtime `BLOCK_CUSTOM`/`BLOCK_EVENT*` cells are honoured only in the kind's
//! mask; the legacy's placement search additionally refuses every `EVENT`
//! cell (`MapCommon.cpp:863-875`), and a caller spawning during an active
//! event can OR those bits into the mask it passes. Nothing else is guessed.
//!
//! # Island rejection ([`spawn_points`])
//!
//! The candidate set is the group's leash square (`CentreX ± Radius`,
//! `CentreY ± Radius` — the exact test `cltNPCGroup::IsInNPCGroup` makes,
//! `NPC.cpp:109-120`). A tile is a spawn point only if it is
//!
//! 1. walkable for the kind ([`is_walkable`]), and
//! 2. **connected**: reachable from the group's centre by a path of adjacent
//!    walkable tiles (8-neighbour, the movement step's diagonal included).
//!
//! The centre tile is the BFS root. If the centre itself is unwalkable the
//! anchor falls back to the nearest walkable tile of the square, searched in
//! the legacy's expanding-ring order (`MapCommon.cpp:885-890`); a square with
//! no walkable tile at all yields no spawn points. Reachability is computed
//! over the *whole map*, not the square — a tile inside the square that can
//! only be reached by walking out of the square and back is still reachable
//! and stays valid; a walkable tile sealed off by walls or water (a cliff
//! pocket, a lone island in deep water) is not reachable from the centre and
//! is discarded. That is the anti-stuck/anti-island guarantee: every spawn
//! tile sits on the centre's own walking ground, so a player can walk from
//! the group's centre to the monster and back.
//!
//! # The leash ([`SpawnGroup::return_home`])
//!
//! Each instance is anchored to its group. The legacy checks the anchor every
//! wander tick (`cltCharServer`, `char-wait.cpp:217-249`): outside the group
//! square the monster is ordered straight back to the centre (`:222-232`) —
//! so a combat chase that drags a monster past the radius ends with it walking
//! home — and inside the square it only ever takes a step to a tile that is
//! in map area and non-colliding (`:244`). [`SpawnGroup::constrain`] is that
//! policy as a pure function: `Some(centre)` means "return to spawn now",
//! `None` means the position is inside the leash and the caller's own
//! wander/step logic proceeds (its step gate is [`is_walkable`]).

use std::collections::{HashSet, VecDeque};

use crate::map::MapFile;

/// One monster group's anchor: map, centre tile and leash radius, in the
/// units the `NPCGroup` header carries (`NPC.cpp:203-213`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnGroup {
    /// The `MapIndex` column — the server's map id (map 0 is the main field,
    /// `map.h:67`).
    pub map_index: i32,
    /// The `CenterX` column.
    pub center_x: i32,
    /// The `CenterY` column.
    pub center_y: i32,
    /// The `Radius` column — the leash is the square `± radius` on both axes
    /// (`NPC.cpp:112-113`).
    pub radius: i32,
}

impl SpawnGroup {
    /// `cltNPCGroup::IsInNPCGroup` (`NPC.cpp:109-120`): same map, and both
    /// axes inside `centre ± radius` (a square, not a circle).
    #[must_use]
    pub fn contains(&self, map_index: i32, x: i32, y: i32) -> bool {
        map_index == self.map_index
            && x >= self.center_x - self.radius
            && x <= self.center_x + self.radius
            && y >= self.center_y - self.radius
            && y <= self.center_y + self.radius
    }

    /// The return-to-spawn rule of `char-wait.cpp:219-236`: a position outside
    /// the leash (or on another map) sends the monster back to the group
    /// centre; an inside position needs no correction.
    #[must_use]
    pub fn return_home(&self, map_index: i32, x: i32, y: i32) -> Option<(i32, i32)> {
        if self.contains(map_index, x, y) {
            None
        } else {
            Some((self.center_x, self.center_y))
        }
    }
}

/// `cltMapCommon::IsColl(x, y, block) == false` for a kind whose mask is
/// `block` — the one movement rule (module docs). Out of map is blocked
/// (`map.cpp:260`).
#[must_use]
pub fn is_walkable(map: &MapFile, x: i32, y: i32, block: u16) -> bool {
    x >= 0
        && y >= 0
        && x < map.xsize() as i32
        && y < map.ysize() as i32
        && (map.coll(x, y) & block) == 0
}

/// Every safe spawn tile of `group` for a kind with block mask `block`,
/// sorted by `(x, y)` for a stable order across restarts. See the module docs
/// for the two filters (walkable + connected to the centre) and their cites.
#[must_use]
pub fn spawn_points(map: &MapFile, group: &SpawnGroup, block: u16) -> Vec<(i32, i32)> {
    spawn_points_in(
        map.xsize() as i32,
        map.ysize() as i32,
        |x, y| map.coll(x, y),
        group,
        block,
    )
}

/// [`spawn_points`] over a raw collision grid, so the logic is testable
/// without a `.map` file: `coll` answers the `CollMap` cell of any tile.
fn spawn_points_in(
    width: i32,
    height: i32,
    coll: impl Fn(i32, i32) -> u16,
    group: &SpawnGroup,
    block: u16,
) -> Vec<(i32, i32)> {
    let walk =
        |x: i32, y: i32| x >= 0 && y >= 0 && x < width && y < height && (coll(x, y) & block) == 0;

    // Anchor: the centre if it is walkable, else the nearest walkable tile of
    // the leash square in the legacy's expanding-ring order
    // (`MapCommon.cpp:885-890`).
    let radius = group.radius.max(0);
    let mut anchor = None;
    'ring: for ring in 0..=radius {
        for dy in -ring..=ring {
            for dx in -ring..=ring {
                if dx.abs() != ring && dy.abs() != ring {
                    continue;
                }
                let (x, y) = (group.center_x + dx, group.center_y + dy);
                if walk(x, y) {
                    anchor = Some((x, y));
                    break 'ring;
                }
            }
        }
    }
    let Some((ax, ay)) = anchor else {
        return Vec::new();
    };

    // Connectivity: 8-neighbour flood fill from the anchor (diagonals are
    // movement steps too), then intersect with the leash square.
    let mut seen = HashSet::from([(ax, ay)]);
    let mut queue = VecDeque::from([(ax, ay)]);
    let mut out = Vec::new();
    while let Some((x, y)) = queue.pop_front() {
        if group.contains(group.map_index, x, y) {
            out.push((x, y));
        }
        for dy in -1..=1 {
            for dx in -1..=1 {
                if dx == 0 && dy == 0 {
                    continue;
                }
                let next = (x + dx, y + dy);
                if walk(next.0, next.1) && seen.insert(next) {
                    queue.push_back(next);
                }
            }
        }
    }
    out.sort_unstable();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kindinfo::{BLOCK_LAND, BLOCK_NORMAL};

    /// A 13×13 map around a centre at (6,6): everything walkable except a
    /// closed ring of walls sealing the pocket tile (2,2) off — its eight
    /// neighbours are all `BLOCK_NORMAL`.
    fn pocket_map() -> impl Fn(i32, i32) -> u16 {
        let walls = [
            (1, 1),
            (2, 1),
            (3, 1),
            (1, 2),
            (3, 2),
            (1, 3),
            (2, 3),
            (3, 3),
        ];
        move |x, y| {
            if walls.contains(&(x, y)) {
                BLOCK_NORMAL
            } else {
                0
            }
        }
    }

    #[test]
    fn a_sealed_pocket_inside_the_radius_is_not_a_spawn_point() {
        let group = SpawnGroup {
            map_index: 98,
            center_x: 6,
            center_y: 6,
            radius: 4,
        };
        let points = spawn_points_in(13, 13, pocket_map(), &group, BLOCK_NORMAL);
        // The 9×9 leash square [2..10]²: 81 tiles minus the three wall tiles
        // inside it (3,2), (2,3), (3,3) — the other five walls sit just
        // outside the square and seal the pocket from there — minus the
        // pocket itself.
        assert_eq!(points.len(), 81 - 3 - 1);
        assert!(points.contains(&(6, 6)), "the centre is a spawn tile");
        assert!(
            !points.contains(&(2, 2)),
            "the walkable pocket cannot be reached from the centre"
        );
        assert!(!points.iter().any(|&(x, y)| !group.contains(98, x, y)));
        let mut sorted = points.clone();
        sorted.sort_unstable();
        assert_eq!(points, sorted, "stable order");
    }

    #[test]
    fn an_unwalkable_tile_is_never_handed_out() {
        let group = SpawnGroup {
            map_index: 98,
            center_x: 6,
            center_y: 6,
            radius: 4,
        };
        let points = spawn_points_in(13, 13, pocket_map(), &group, BLOCK_NORMAL);
        assert!(!points.contains(&(1, 1)), "a wall tile is excluded");
    }

    #[test]
    fn the_mask_decides_which_terrain_blocks_whom() {
        // A land cell (BLOCK_LAND, as every non-water cell carries,
        // `MapCommon.cpp:637-640`): walkable for a land kind, water for a
        // water kind whose siblock includes BLOCK_LAND.
        let group = SpawnGroup {
            map_index: 0,
            center_x: 2,
            center_y: 2,
            radius: 1,
        };
        let land = |_, _| BLOCK_LAND;
        assert_eq!(spawn_points_in(5, 5, land, &group, BLOCK_NORMAL).len(), 9);
        assert!(spawn_points_in(5, 5, land, &group, BLOCK_LAND).is_empty());
    }

    #[test]
    fn an_off_map_anchor_finds_the_nearest_walkable_tile_and_stays_on_the_map() {
        // Centre in the corner, half the leash square off the map.
        let group = SpawnGroup {
            map_index: 0,
            center_x: 0,
            center_y: 0,
            radius: 3,
        };
        let points = spawn_points_in(8, 8, |_, _| 0u16, &group, BLOCK_NORMAL);
        assert!(points.contains(&(0, 0)));
        assert!(points.iter().all(|&(x, y)| x >= 0 && y >= 0));
        assert_eq!(points.len(), 4 * 4, "the on-map part of the 7×7 square");
    }

    #[test]
    fn an_unwalkable_centre_falls_back_to_the_ring_and_a_fully_blocked_square_spawns_nothing() {
        let group = SpawnGroup {
            map_index: 0,
            center_x: 4,
            center_y: 4,
            radius: 2,
        };
        // Only the centre is blocked: the anchor steps to the ring, and every
        // other square tile still spawns.
        let blocked_center = |x: i32, y: i32| {
            if (x, y) == (4, 4) { BLOCK_NORMAL } else { 0 }
        };
        let points = spawn_points_in(11, 11, blocked_center, &group, BLOCK_NORMAL);
        assert_eq!(points.len(), 24, "25 square tiles minus the blocked centre");

        // Nothing walkable in the square: no anchor, no spawn points.
        let wall = |_, _| BLOCK_NORMAL;
        assert!(spawn_points_in(11, 11, wall, &group, BLOCK_NORMAL).is_empty());
    }

    #[test]
    fn the_leash_is_the_legacy_square_and_sends_strays_home() {
        let group = SpawnGroup {
            map_index: 158,
            center_x: 30,
            center_y: 40,
            radius: 5,
        };
        // `IsInNPCGroup` is inclusive on both edges (`NPC.cpp:112-113`).
        assert!(group.contains(158, 25, 35));
        assert!(group.contains(158, 30, 40));
        assert!(group.contains(158, 35, 45));
        assert!(!group.contains(158, 24, 40), "one tile past x-radius");
        assert!(!group.contains(158, 30, 46), "one tile past y-radius");
        assert!(!group.contains(157, 30, 40), "same tile, other map");

        // Inside the leash nothing pulls the monster back; outside — the
        // combat-chase case — it walks to the centre (`char-wait.cpp:222-232`).
        assert_eq!(group.return_home(158, 34, 44), None);
        assert_eq!(group.return_home(158, 90, 12), Some((30, 40)));
        assert_eq!(group.return_home(0, 30, 40), Some((30, 40)));
    }
}
