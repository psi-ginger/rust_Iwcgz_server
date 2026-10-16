//! `NPCGroup\*.txt` — the field monster (and map-NPC) groups the legacy server
//! spawns, parsed.
//!
//! One file is one group: a header row `NPCGroupID MaxNPCNum MapIndex CenterX
//! CenterY Radius Interval BOSS` (`NPC.cpp:203-213`) and a unit block
//! `NPCName MinNum Level` — the kinds the group holds, each with its own count
//! (`siKindMaxNum`) and spawn level. `scripts\generate-npc-placements.ps1` reads
//! the same files for the *static* town-NPC table and refuses the mobile groups
//! (`Radius != 1` or `MinNum > 1`); this parser is for exactly those leftovers,
//! the field groups.
//!
//! The files are plain ASCII/ANSI tab-separated text (checked against the
//! shipped `New Era Live\NPCGroup` tree), so the bytes are read lossily rather
//! than through the UTF-16LE path the `KindInfo_*.txt` files need.
//!
//! # What this is not
//!
//! It does not resolve the unit's code to a wire id or look up its stats — the
//! code is the join key, and [`crate::kindinfo`] holds the tables that answer
//! the rest. It also does not decide *where* a monster stands: the group's
//! `CenterX`/`CenterY`/`Radius` become a [`crate::spawn::SpawnGroup`] and
//! [`crate::spawn::spawn_points`] applies the walkability and connectivity
//! filter.

use std::fs;
use std::io;
use std::path::Path;

/// One unit row of an `NPCGroup` file: `NPCName MinNum Level`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NpcGroupUnit {
    /// `NPCName` — a `KIND_*` code, the join key into `KindInfo_Create.txt` and
    /// `KindInfo_Unique.txt`.
    pub kind: String,
    /// `MinNum` — how many of this kind the group holds (the legacy's
    /// `siKindMaxNum`; the header name is a misnomer in the shipped data).
    pub max_num: i32,
    /// `Level` — the spawn level.
    pub level: i32,
}

/// One group row of an `NPCGroup` file, plus its units.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NpcGroup {
    /// `NPCGroupID`.
    pub id: i32,
    /// `MaxNPCNum` — the group's overall capacity, the cap on how many of its
    /// units may be alive at once (`NPC.cpp:938-950`).
    pub max_npc_num: i32,
    /// `MapIndex` — the server's map id (0 is the main field, `map.h:67`).
    pub map_index: i32,
    /// `CenterX` — the leash centre (also the respawn anchor).
    pub center_x: i32,
    /// `CenterY`.
    pub center_y: i32,
    /// `Radius` — the leash is the square `± radius`.
    pub radius: i32,
    /// `Interval` — the respawn interval column; `0` in the shipped field data,
    /// kept so a caller can honour it rather than invent one.
    pub interval: i32,
    /// `BOSS` — non-zero when the group is a boss group.
    pub boss: bool,
    /// The unit block, in file order.
    pub units: Vec<NpcGroupUnit>,
}

/// Parse one `NPCGroup` file.
///
/// `None` when the file carries no group data row — a header with no row under
/// it, or a file that is not an `NPCGroup` file at all. A group whose unit
/// block is empty (`NULL` straight after `NPCName`, as the event files have)
/// parses with no units; the caller decides whether to keep it.
///
/// The legacy keeps one group per file (`scripts\generate-npc-placements.ps1`
/// relies on it too), so only the first group data row is read.
#[must_use]
pub fn parse_npc_group(bytes: &[u8]) -> Option<NpcGroup> {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text.lines().collect();
    let cells =
        |line: &str| -> Vec<String> { line.split('\t').map(|c| c.trim().to_owned()).collect() };
    let first_cell = |line: &str| line.split('\t').next().unwrap_or("").trim().to_owned();

    // `NPCGroupID MaxNPCNum MapIndex CenterX CenterY Radius Interval BOSS`.
    let header = lines
        .iter()
        .position(|l| first_cell(l).eq_ignore_ascii_case("NPCGroupID"))?;

    // The group data row: the first row after the header whose first cell is an
    // integer and which carries enough columns. This skips blank lines and the
    // `NPCName` unit header.
    let group = lines[header + 1..].iter().find_map(|l| {
        let f = cells(l);
        (f.len() >= 7 && f[0].parse::<i32>().is_ok()).then_some(f)
    })?;

    // `NPCName MinNum Level`.
    let unit_header = lines
        .iter()
        .position(|l| first_cell(l).eq_ignore_ascii_case("NPCName"))?;

    let mut units = Vec::new();
    for line in &lines[unit_header + 1..] {
        let f = cells(line);
        let kind = f.first().map_or("", String::as_str);
        // The block ends at `NULL` or the first row that is not a unit row.
        // A count of 0 ends it too, the generator's rule
        // (`scripts\generate-npc-placements.ps1`).
        let Some(max_num) = f.get(1).and_then(|s| s.parse::<i32>().ok()) else {
            break;
        };
        if kind.is_empty() || kind.eq_ignore_ascii_case("NULL") || max_num <= 0 {
            break;
        }
        let level = f.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);
        units.push(NpcGroupUnit {
            kind: kind.to_owned(),
            max_num,
            level,
        });
    }

    Some(NpcGroup {
        id: group[0].parse().ok()?,
        max_npc_num: group[1].parse().ok()?,
        map_index: group[2].parse().ok()?,
        center_x: group[3].parse().ok()?,
        center_y: group[4].parse().ok()?,
        radius: group[5].parse().ok()?,
        interval: group.get(6).and_then(|s| s.parse().ok()).unwrap_or(0),
        boss: group
            .get(7)
            .is_some_and(|s| s.parse::<i32>().is_ok_and(|v| v != 0)),
        units,
    })
}

/// Load every `*.txt` group file under `dir` that has a group on `map_index`
/// and at least one unit, ordered by file name so the caller's ids are stable
/// across restarts (the client caches `char_unique`s).
///
/// # Errors
///
/// The `io::Error` from reading the directory or any file in it.
pub fn load_npc_groups(dir: &Path, map_index: i32) -> io::Result<Vec<NpcGroup>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("txt") {
            continue;
        }
        files.push(path);
    }
    files.sort();

    let mut out = Vec::new();
    for path in files {
        if let Some(group) = parse_npc_group(&fs::read(&path)?)
            && group.map_index == map_index
            && !group.units.is_empty()
        {
            out.push(group);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped shape: a one-group, one-unit field file.
    const RACCOON: &str = "NPCmonbase02\t\t\t\t\t\t\n\
2007.04.19\t\t\t\t\t\t\n\
v0.1\t\t\t\t\t\t\n\
YTG\t\t\t\t\t\t\n\
\n\
NPCGroupID\tMaxNPCNum\tMapIndex\tCenterX\tCenterY\tRadius\tInterval\tBOSS\n\
1\t20\t0\t246\t332\t10\t0\t0\n\
\n\
\n\
NPCName\tMinNum\tLevel\t\t\t\t\n\
KIND_RACCOON\t10\t1\t\t\t\t\n\
NULL\t\t\t\t\t\t\n";

    #[test]
    fn parses_the_group_row_and_its_unit_block() {
        let group = parse_npc_group(RACCOON.as_bytes()).expect("a group");
        assert_eq!(group.id, 1);
        assert_eq!(group.max_npc_num, 20);
        assert_eq!(group.map_index, 0);
        assert_eq!((group.center_x, group.center_y), (246, 332));
        assert_eq!(group.radius, 10);
        assert!(!group.boss);
        assert_eq!(
            group.units,
            vec![NpcGroupUnit {
                kind: "KIND_RACCOON".to_owned(),
                max_num: 10,
                level: 1,
            }]
        );
    }

    #[test]
    fn a_unit_block_that_is_null_parses_with_no_units() {
        // `npcevent01.txt`'s shape: a group row, then `NPCName` with `NULL`.
        let bytes = b"NPCevent01\t\t\n\
2006-12-08\n\
NPCGroupID\tMaxNPCNum\tMapIndex\tCenterX\tCenterY\tRadius\tInterval\n\
10\t40\t0\t169\t39\t50\t0\n\
NPCName\tMinNum\tLevel\n\
NULL\t\t\n";
        let group = parse_npc_group(bytes).expect("a group");
        assert_eq!(group.map_index, 0);
        assert_eq!(group.radius, 50);
        assert!(group.units.is_empty());
    }

    #[test]
    fn multiple_units_are_read_in_order_until_null() {
        let bytes = b"NPCGroupID\tMaxNPCNum\tMapIndex\tCenterX\tCenterY\tRadius\tInterval\tBOSS\n\
1\t10\t0\t5\t5\t3\t0\t0\n\
NPCName\tMinNum\tLevel\n\
KIND_A\t3\t5\n\
KIND_B\t7\t9\n\
NULL\t\t\n";
        let group = parse_npc_group(bytes).expect("a group");
        assert_eq!(group.units.len(), 2);
        assert_eq!(group.units[0].kind, "KIND_A");
        assert_eq!(group.units[0].level, 5);
        assert_eq!(group.units[1].max_num, 7);
    }

    #[test]
    fn a_group_row_without_the_optional_columns_still_parses() {
        // `npcevent01.txt`'s header ends at `Interval` — no `BOSS` column.
        let bytes = b"NPCGroupID\tMaxNPCNum\tMapIndex\tCenterX\tCenterY\tRadius\tInterval\n\
10\t40\t0\t169\t39\t50\t0\n\
NPCName\tMinNum\tLevel\n\
NULL\t\t\n";
        let group = parse_npc_group(bytes).expect("a group");
        assert!(!group.boss);
        assert_eq!(group.interval, 0);
    }

    #[test]
    fn a_file_without_a_group_row_parses_to_none() {
        assert!(parse_npc_group(b"just some text\nwith no header\n").is_none());
    }
}
