//! The two monster kind-data files the legacy loads per kind, plus the runtime
//! base-stat math that turns a kind row into the numbers a spawned monster
//! actually plays with.
//!
//! # The files
//!
//! - `Data\KindInfo_Create.txt` — loaded by
//!   `cltKindInfoSet::AddCharKindInfoSetFromFile`
//!   (`common\Char\KindInfo\kindinfo-CharManager.cpp:30`, file at `:105`,
//!   column table `pCreateInfo` at `:146-188`, 37 columns). This module keeps
//!   the server-relevant subset: the join key `szcode` (column 1 — the same
//!   key `FindKindFromCode` compares, `kindinfo-Manager.cpp:579`), `SortNUM`
//!   (2), `movespeed` (3, stored as `siMoveSpeed`, `kindinfo.h:380`) and the
//!   `siblock` mask (5, parsed by `blockatbparser.GetValueByString`,
//!   `kindinfo-CharManager.cpp:218-219`, into `siBlock`, `kindinfo.h:383`).
//!   The remaining 32 columns are draw/effect data the server never reads.
//!
//! - `CharInfo\CharAttackInfoE.txt` — the English attack/ability table loaded
//!   by `cltKindInfoSet::SetCharAttackInfo` (`kindinfo-Manager.cpp:590`,
//!   column table at `:640-669`, 27 columns). Storage: MinDmg/MaxDmg/AC/range
//!   go to `clDefaultWeaponInfo` (`:688-689`, `:698`), Str/Dex/Vit/Mag/Hnd/Luk/
//!   Wis to `clBA` (`:691-692`, `:699`) — **Chm and Mth are parsed by the
//!   loader but never stored** (`clBA.Set` takes seven stats, not nine), so
//!   this parser mirrors the legacy and drops them — then Exp (`:701`),
//!   `dwAttackInterval` (`:703`), `siCriticalRate` (`:705`), `siAggressive`
//!   (`:710`) and the DefenE/R + AttackE/R pairs in `clBPB.Set(50, 0, 0,
//!   AttackE, AttackR, DefenE, DefenR)` (`:694-695`).
//!
//! The committed runtime master for the attack table is
//! `data/CharAttackInfoCanonical.txt`: the KR baseline's values in every column it
//! carries, the EN values in the seven columns only EN has (`SortNUM`, `Luk`,
//! `AttackE`, `AttackR`, `DefenE`, `DefenR`, `Wis`) and the English display names,
//! merged by `scripts/generate-kr-canonical-tables.ps1`
//! (`docs/DESIGN_MONSTER_KR_BASELINE_STRATEGY.md`). The EN rows are followed by the
//! verified exact-code KR-only kinds (`KIND_BOMBDRAGON`, `KIND_JANGBOGO`) — KR-only
//! codes that already carry an EN wire id, so the EN client can resolve them — and the
//! human-confirmed rename pairs (Tier C, `docs/MONSTER_PAIRING_CANDIDATES.md`) upgrade
//! the EN-only row of the paired EN code in place. The shipped EN file below has the
//! same schema, so one parser reads both.
//!
//! # File mechanics (a port of `NDataLoader`)
//!
//! Both files are UTF-16LE, tab-separated. `NDataLoader::ReadyToRead`
//! (`NLib\NDataLoader.cpp:173-241`) skips everything before the `<Data>` row
//! and the column-name row right after it (`SkipRow(2)` at `:194`);
//! `ReadData` stops at an `<End>` row (`:272-283` clamps the end pointer, so
//! anything after it — both shipped files carry one vestigial column-header
//! row after `<End>` — is never read). An empty cell is zero-filled
//! (`:302-304`), and a short row is padded with zeros for the missing columns
//! (`:352-357`); a column-count mismatch only ever pops a debug MessageBox
//! (`:342-350`), so the parser here is tolerant in the same way. Rows whose
//! key cell is empty are skipped by the legacy loaders themselves
//! (`kindinfo-Manager.cpp:680`, `kindinfo-CharManager.cpp:199`) and are
//! skipped here.
//!
//! # Runtime base stats ([`MonsterStats`])
//!
//! A spawned NPC/monster is built by `cltCommonLogic::SetChar`
//! (`CommonLogic\CommonLogic-SetChar.cpp:44`): the kind's `clBA` becomes the
//! character's basic ability (`:81`), `SetLevel` applies the group's `Level`
//! (`:108`), and — only while `Switch_HealthAddChange` is **off** — the level
//! is folded into the stats (`Str+(level-1)*2`, `Dex/Mag/Vit+(level-1)`,
//! `:113-124`). HP is then `clPB.GetMaxLife()` (`:146`), which dispatches on
//! the same switch (`Parametabox.cpp:7268-7282`):
//!
//! - switch **off** (legacy): `MaxLife = totalVit * VIT_LIFE_RATE`
//!   (`Parametabox.cpp:5798`; `VIT_LIFE_RATE = 4`, `parametabox.h:11`).
//! - switch **on** ("HealthAddChange"): `totalVit * (8000 + level*5) / 1000`
//!   with integer truncation (`SetHitPoint`, `Parametabox.cpp:3366-3374`).
//!
//! [`MonsterStats::at`] reproduces exactly that coupling: the level-scaling
//! and the legacy HP formula come as a pair (switch off), the unscaled stats
//! and the new formula as the other pair (switch on). The per-kind random-Vit
//! overrides for the raccoon event kinds (`CommonLogic-SetChar.cpp:85-106`)
//! are deliberately not ported — they are hard-coded event exceptions, not
//! data.
//!
//! Min/Max attack are the kind's `MinDmg`/`MaxDmg` verbatim (the "default
//! weapon" of an unarmed kind, `kindinfo-Manager.cpp:689`), AC is the `AC`
//! column, and move speed is the Create file's `movespeed`.

/// `BLOCK_NONE` (`common\Map\map.h:49`).
pub const BLOCK_NONE: u16 = 1;
/// `BLOCK_DEEPWATER` (`map.h:50`).
pub const BLOCK_DEEPWATER: u16 = 1 << 1;
/// `BLOCK_SHALLOWWATER` (`map.h:51`).
pub const BLOCK_SHALLOWWATER: u16 = 1 << 2;
/// `BLOCK_NORMAL` (`map.h:52`) — walls, trees, buildings: the "impassable
/// terrain" bit a default land kind is stopped by.
pub const BLOCK_NORMAL: u16 = 1 << 3;
/// `BLOCK_LAND` (`map.h:53`) — set on every non-water cell
/// (`MapCommon.cpp:637-640`), i.e. the bit a water-bound kind (whose
/// `siblock` carries it) is stopped by.
pub const BLOCK_LAND: u16 = 1 << 4;
/// `BLOCK_CUSTOM` (`map.h:54`).
pub const BLOCK_CUSTOM: u16 = 1 << 5;
/// `BLOCK_EVENT1` (`map.h:55`).
pub const BLOCK_EVENT1: u16 = 1 << 6;
/// `BLOCK_EVENT2` (`map.h:56`).
pub const BLOCK_EVENT2: u16 = 1 << 7;
/// `BLOCK_EVENT3` (`map.h:57`).
pub const BLOCK_EVENT3: u16 = 1 << 8;
/// `BLOCK_EVENT4` (`map.h:58`).
pub const BLOCK_EVENT4: u16 = 1 << 9;
/// `BLOCK_EVENT5` (`map.h:59`).
pub const BLOCK_EVENT5: u16 = 1 << 10;
/// `BLOCK_EVENT6` (`map.h:60`).
pub const BLOCK_EVENT6: u16 = 1 << 11;
/// `BLOCK_EVENT7` (`map.h:61`).
pub const BLOCK_EVENT7: u16 = 1 << 13;

/// `VIT_LIFE_RATE` — legacy `MaxLife = Vit * VIT_LIFE_RATE`
/// (`parametabox.h:11`, used at `Parametabox.cpp:5798`).
pub const VIT_LIFE_RATE: i32 = 4;

/// One `Data\KindInfo_Create.txt` row — the server-relevant subset. See the
/// module docs for the column cites.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindCreateRow {
    /// `szcode` (column 1) — the join key shared with the attack file and with
    /// `NPCGroup` unit rows.
    pub code: String,
    /// `SortNUM` (column 2) — the legacy's ordering hint, stored but unused by
    /// the maths.
    pub sort_num: i32,
    /// `movespeed` (column 3) — the kind's `siMoveSpeed` (`kindinfo.h:380`),
    /// the tiles-per-tick step budget `cltCharCommon::Move` spends
    /// (`char-move.cpp:140`, `:154`).
    pub move_speed: i32,
    /// `siblock` (column 5) — the cell classes that stop this kind, as a
    /// [`BLOCK_*`](BLOCK_NORMAL) mask (`kindinfo-CharManager.cpp:218-219`).
    pub block: u16,
}

/// One `CharInfo\CharAttackInfoE.txt` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharAttackRow {
    /// Column 0 (`szcode`), the join key.
    pub code: String,
    /// Column 3 (`szattacktype`) — kept as the raw code string; the legacy
    /// resolves it to an id (`:685`) that this port does not need yet.
    pub attack_type: String,
    /// Column 6 — `siAggressive` (`:710`).
    pub aggressive: i32,
    /// Column 7 — attack range, part of `clDefaultWeaponInfo` (`:689`).
    pub range: i32,
    /// Column 8 (`mindmg`) — the runtime Min attack (`:689`).
    pub min_dmg: i32,
    /// Column 9 (`maxdmg`) — the runtime Max attack (`:689`).
    pub max_dmg: i32,
    /// Column 10 — `siCriticalRate`, percent (`:705`).
    pub critical_rate: i32,
    /// Column 11 — `dwAttackInterval`, milliseconds (`:703`).
    pub attack_interval: i32,
    /// Column 12 (`ac`) — the runtime defence, part of
    /// `clDefaultWeaponInfo` (`:689`).
    pub ac: i32,
    /// Column 13 — `clBA.Set(Str, ...)` (`:692`).
    pub str: i32,
    /// Column 14 — `clBA.Set(..., Dex, ...)` (`:692`).
    pub dex: i32,
    /// Column 15 — `clBA.Set(..., Vit, ...)` (`:692`); the HP root.
    pub vit: i32,
    /// Column 16 (`Intl`) — `clBA.Set(..., Mag, ...)` (`:692`).
    pub intl: i32,
    /// Column 18 — `clBA.Set(..., Hnd, ...)` (`:692`).
    pub hnd: i32,
    /// Column 19 — `clBA.Set(..., Luk, ...)` (`:692`).
    pub luk: i32,
    /// Column 26 (last) — `clBA.Set(..., Wis)` (`:692`).
    pub wis: i32,
    /// Column 21 (`Exp`) — `NDATA_INT64` (`:663`), the exp a kill yields
    /// (`:701`). The legacy header widened this from `SHORT`
    /// (`kindinfo.h:393`) for overflow reasons the loader comment names
    /// (`:632`).
    pub exp: i64,
    /// Column 22 — `clBPB` AttackE (`:695`).
    pub attack_e: i32,
    /// Column 23 — `clBPB` AttackR (`:695`).
    pub attack_r: i32,
    /// Column 24 — `clBPB` DefenE (`:695`).
    pub defen_e: i32,
    /// Column 25 — `clBPB` DefenR (`:695`).
    pub defen_r: i32,
}

/// The runtime base numbers of one monster instance at one level — what
/// `SetChar` derives before equipment, skills, buffs and PVP multipliers layer
/// on top (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonsterStats {
    /// The spawn level (the `NPCGroup` unit's `Level` column).
    pub level: i32,
    /// Scaled Str (legacy branch) or the raw `clBA` value (new branch).
    pub str: i32,
    /// Scaled/raw Dex, see [`MonsterStats::str`].
    pub dex: i32,
    /// Scaled/raw Vit — the HP root.
    pub vit: i32,
    /// Scaled/raw Mag (0 stays 0, `CommonLogic-SetChar.cpp:118-121`).
    pub intl: i32,
    /// Raw Hnd (never level-scaled).
    pub hnd: i32,
    /// Raw Luk (never level-scaled).
    pub luk: i32,
    /// Raw Wis (never level-scaled).
    pub wis: i32,
    /// `GetMaxLife()` on a bare monster — the formula pair in the module docs.
    pub max_hp: i32,
    /// `MinDmg` verbatim.
    pub min_atk: i32,
    /// `MaxDmg` verbatim.
    pub max_atk: i32,
    /// `AC` verbatim (no equipment layers on a bare spawn).
    pub ac: i32,
    /// The Create file's `movespeed`.
    pub move_speed: i32,
}

impl MonsterStats {
    /// A kind's stats at `level`, exactly as `SetChar` derives them.
    ///
    /// `health_add_change` is the runtime `IsCountrySwitch(Switch_HealthAddChange)`
    /// answer. The legacy keys **both** the stat scaling
    /// (`CommonLogic-SetChar.cpp:113-124`, only when off) and the HP formula
    /// dispatch (`Parametabox.cpp:7268-7282`) on that one switch, so this
    /// function takes it once and keeps the coupling: off → scaled stats +
    /// `vit * VIT_LIFE_RATE`; on → raw stats + `vit * (8000 + level*5) / 1000`.
    ///
    /// The HP arithmetic is C-style integer maths: truncation toward zero at
    /// the `/1000` (`Parametabox.cpp:3374`), which for the shipped non-negative
    /// stats is a floor.
    #[must_use]
    pub fn at(
        create: &KindCreateRow,
        attack: &CharAttackRow,
        level: i32,
        health_add_change: bool,
    ) -> Self {
        let (str, dex, vit, intl) = if health_add_change {
            (attack.str, attack.dex, attack.vit, attack.intl)
        } else {
            let d = level - 1;
            (
                attack.str + d * 2,
                attack.dex + d,
                attack.vit + d,
                if attack.intl != 0 { attack.intl + d } else { 0 },
            )
        };
        let max_hp = if health_add_change {
            (vit * (8000 + level * 5)) / 1000
        } else {
            vit * VIT_LIFE_RATE
        };
        Self {
            level,
            str,
            dex,
            vit,
            intl,
            hnd: attack.hnd,
            luk: attack.luk,
            wis: attack.wis,
            max_hp,
            min_atk: attack.min_dmg,
            max_atk: attack.max_dmg,
            ac: attack.ac,
            move_speed: create.move_speed,
        }
    }
}

/// Why a kind-data file could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KindDataError {
    /// No `<Data>` row anywhere in the file — not an `NDataLoader` data file.
    NoDataMarker,
}

impl std::fmt::Display for KindDataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoDataMarker => write!(f, "no <Data> section marker in file"),
        }
    }
}

impl std::error::Error for KindDataError {}

/// UTF-16LE (with or without BOM) to `String`, the lossy way — a code unit
/// that is not a scalar value becomes U+FFFD instead of panicking, which can
/// only affect cells this parser does not read.
pub(crate) fn decode_utf16le(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    let units = units.strip_prefix(&[0xFEFF]).unwrap_or(&units);
    char::decode_utf16(units.iter().copied())
        .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// The data rows of an `NDataLoader` file: everything after the `<Data>` row
/// and its column-name row, cut at the first `<End>` row (module docs).
pub(crate) fn data_rows(text: &str) -> Result<Vec<&str>, KindDataError> {
    let rows: Vec<&str> = text
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    let starts_with = |l: &str, tag: &[u8]| {
        l.as_bytes()
            .get(..tag.len())
            .is_some_and(|b| b.eq_ignore_ascii_case(tag))
    };
    let start = rows
        .iter()
        .position(|l| starts_with(l, b"<data>"))
        .ok_or(KindDataError::NoDataMarker)?;
    // The two skipped rows are the `<Data>` row itself and the column-name row
    // (`NDataLoader.cpp:194` `SkipRow(2)`).
    let body = &rows[start + 2..];
    let end = body
        .iter()
        .position(|l| starts_with(l, b"<end>"))
        .unwrap_or(body.len());
    Ok(body[..end].to_vec())
}

/// Tab-separated cell `index`, or `""` when the row is short — the legacy
/// zero-fills missing trailing columns (`NDataLoader.cpp:352-357`).
pub(crate) fn field(row: &str, index: usize) -> &str {
    row.split('\t').nth(index).unwrap_or("")
}

/// `_wtoi` for clean data: leading/trailing whitespace tolerant, a field that
/// is not a plain integer is 0 (the same answer an empty cell gets,
/// `NDataLoader.cpp:302-304`).
pub(crate) fn wtoi(s: &str) -> i32 {
    s.trim().parse::<i32>().unwrap_or(0)
}

/// `NDataAtbParser` over the `BLOCK_*` names (`kindinfo-CharManager.cpp:85-97`,
/// values in `map.h:49-61`): `|`-separated, unknown names contribute nothing.
fn block_mask(s: &str) -> u16 {
    s.split('|').fold(0u16, |acc, name| {
        acc | match name.trim() {
            "BLOCK_NONE" => BLOCK_NONE,
            "BLOCK_DEEPWATER" => BLOCK_DEEPWATER,
            "BLOCK_SHALLOWWATER" => BLOCK_SHALLOWWATER,
            "BLOCK_NORMAL" => BLOCK_NORMAL,
            "BLOCK_LAND" => BLOCK_LAND,
            "BLOCK_CUSTOM" => BLOCK_CUSTOM,
            "BLOCK_EVENT1" => BLOCK_EVENT1,
            "BLOCK_EVENT2" => BLOCK_EVENT2,
            "BLOCK_EVENT3" => BLOCK_EVENT3,
            "BLOCK_EVENT4" => BLOCK_EVENT4,
            "BLOCK_EVENT5" => BLOCK_EVENT5,
            "BLOCK_EVENT6" => BLOCK_EVENT6,
            "BLOCK_EVENT7" => BLOCK_EVENT7,
            _ => 0,
        }
    })
}

/// Parse a whole `KindInfo_Create.txt` (UTF-16LE bytes as shipped).
pub fn parse_kind_create(bytes: &[u8]) -> Result<Vec<KindCreateRow>, KindDataError> {
    let text = decode_utf16le(bytes);
    let mut out = Vec::new();
    for row in data_rows(&text)? {
        let code = field(row, 1);
        // The legacy skips a row with an empty kindtype (`:199`); an empty
        // code would break the join, so both empties are skipped here.
        if field(row, 0).is_empty() || code.is_empty() {
            continue;
        }
        out.push(KindCreateRow {
            code: code.to_owned(),
            sort_num: wtoi(field(row, 2)),
            move_speed: wtoi(field(row, 3)),
            block: block_mask(field(row, 5)),
        });
    }
    Ok(out)
}

/// Parse a whole `CharAttackInfoE.txt` (UTF-16LE bytes as shipped).
pub fn parse_char_attack(bytes: &[u8]) -> Result<Vec<CharAttackRow>, KindDataError> {
    let text = decode_utf16le(bytes);
    let mut out = Vec::new();
    for row in data_rows(&text)? {
        let code = field(row, 0);
        // `if (szcode[0] != NULL)` (`kindinfo-Manager.cpp:680`).
        if code.is_empty() {
            continue;
        }
        out.push(CharAttackRow {
            code: code.to_owned(),
            attack_type: field(row, 3).to_owned(),
            aggressive: wtoi(field(row, 6)),
            range: wtoi(field(row, 7)),
            min_dmg: wtoi(field(row, 8)),
            max_dmg: wtoi(field(row, 9)),
            critical_rate: wtoi(field(row, 10)),
            attack_interval: wtoi(field(row, 11)),
            ac: wtoi(field(row, 12)),
            str: wtoi(field(row, 13)),
            dex: wtoi(field(row, 14)),
            vit: wtoi(field(row, 15)),
            intl: wtoi(field(row, 16)),
            hnd: wtoi(field(row, 18)),
            luk: wtoi(field(row, 19)),
            wis: wtoi(field(row, 26)),
            exp: field(row, 21).trim().parse::<i64>().unwrap_or(0),
            attack_e: wtoi(field(row, 22)),
            attack_r: wtoi(field(row, 23)),
            defen_e: wtoi(field(row, 24)),
            defen_r: wtoi(field(row, 25)),
        });
    }
    Ok(out)
}

/// One `Data\KindInfo_Unique.txt` row: a `KIND_*` code and the `uiMakeCharKind`
/// id the client resolves its sprite and display name from.
///
/// This is the join the announce path needs that `KindInfo_Create.txt` cannot
/// give: the Create file keys on the code, the wire wants the id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindUniqueRow {
    /// Column `Unique` — `uiMakeCharKind`.
    pub id: u16,
    /// Column `KindType` — the `szcode` shared with the Create and attack files
    /// and with `NPCGroup` unit rows.
    pub code: String,
}

/// Parse a whole `KindInfo_Unique.txt` (UTF-16LE bytes as shipped).
///
/// Rows whose `KindType` is not a `KIND_*` code — the preamble and any
/// non-kind rows — are skipped, matching the generator's filter
/// (`scripts\generate-npc-placements.ps1`).
pub fn parse_kind_unique(bytes: &[u8]) -> Result<Vec<KindUniqueRow>, KindDataError> {
    let text = decode_utf16le(bytes);
    let mut out = Vec::new();
    for row in data_rows(&text)? {
        let code = field(row, 1).trim();
        let id = wtoi(field(row, 0));
        if !code.starts_with("KIND_") || !(0..=i32::from(u16::MAX)).contains(&id) {
            continue;
        }
        out.push(KindUniqueRow {
            id: id as u16,
            code: code.to_owned(),
        });
    }
    Ok(out)
}

/// Read and parse `<root>/Data/KindInfo_Create.txt` — the kind's `siblock`
/// mask and `movespeed`, the two columns the spawn filter and the wire record
/// need. The encoding handling stays in the parser (module docs).
///
/// # Errors
///
/// The `io::Error` from the read, or a [`KindDataError`] wrapped as
/// [`io::ErrorKind::InvalidData`] when the file is not an `NDataLoader` file.
pub fn load_kind_create(root: &std::path::Path) -> std::io::Result<Vec<KindCreateRow>> {
    let bytes = std::fs::read(root.join("Data/KindInfo_Create.txt"))?;
    parse_kind_create(&bytes).map_err(invalid_data)
}

/// Read and parse `<root>/Data/KindInfo_Unique.txt`. See [`load_kind_create`].
///
/// # Errors
///
/// The `io::Error` from the read, or a [`KindDataError`] wrapped as
/// [`io::ErrorKind::InvalidData`].
pub fn load_kind_unique(root: &std::path::Path) -> std::io::Result<Vec<KindUniqueRow>> {
    let bytes = std::fs::read(root.join("Data/KindInfo_Unique.txt"))?;
    parse_kind_unique(&bytes).map_err(invalid_data)
}

fn invalid_data(e: KindDataError) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
}

/// The committed KR-baseline attack master, embedded at build time so the game
/// service reads no data files for it (`data/CharAttackInfoCanonical.txt`,
/// generated by `scripts/generate-kr-canonical-tables.ps1`). Parsed once.
#[must_use]
pub fn canonical_char_attack() -> &'static [CharAttackRow] {
    static ROWS: std::sync::OnceLock<Vec<CharAttackRow>> = std::sync::OnceLock::new();
    ROWS.get_or_init(|| {
        parse_char_attack(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/CharAttackInfoCanonical.txt"
        )))
        .expect("the committed canonical attack table parses")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny UTF-16LE `NDataLoader` file: preamble, `<Data>`, column-name
    /// row, data rows, `<End>`, then a vestigial row that must be ignored.
    fn file(rows: &[&str]) -> Vec<u8> {
        let mut text = String::from("Preamble\tjunk\r\n<Data>\r\nKindType\tszcode\r\n");
        for row in rows {
            text.push_str(row);
            text.push_str("\r\n");
        }
        text.push_str("<End>\t\t\r\nKindType\tszcode\r\n");
        text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
    }

    #[test]
    fn the_create_parser_reads_columns_and_skips_everything_the_legacy_skips() {
        // KindBerserker's shipped row, cut to the kept columns.
        let bytes = file(&[
            "KIND_BERSERKER\tKIND_BERSERKER\t10\t10\t1\tBLOCK_NORMAL|BLOCK_SHALLOWWATER|BLOCK_DEEPWATER|BLOCK_CUSTOM\tATB_MONSTER",
            // Short row: the missing columns zero-fill like the legacy.
            "KIND_SHORT\tKIND_SHORT\t1",
            // Empty kindtype: skipped (`kindinfo-CharManager.cpp:199`).
            "\tKIND_GHOST\t2\t5\t1\tBLOCK_NONE",
        ]);
        let rows = parse_kind_create(&bytes).expect("parses");
        assert_eq!(
            rows,
            vec![
                KindCreateRow {
                    code: "KIND_BERSERKER".to_owned(),
                    sort_num: 10,
                    move_speed: 10,
                    block: BLOCK_NORMAL | BLOCK_SHALLOWWATER | BLOCK_DEEPWATER | BLOCK_CUSTOM,
                },
                KindCreateRow {
                    code: "KIND_SHORT".to_owned(),
                    sort_num: 1,
                    move_speed: 0,
                    block: 0,
                },
            ]
        );
    }

    #[test]
    fn the_attack_parser_keeps_the_columns_the_legacy_stores_and_drops_chm_and_mth() {
        // HERO1's shipped row.
        let bytes = file(&[
            "KIND_HERO1\tKENI\t1\tATTACKTYPE_HAND\tNULL\t0\t0\t1\t1\t2\t0\t1000\t5\t5\t5\t5\t5\t5\t5\t5\t5\t1\t0\t0\t0\t0\t5",
        ]);
        let rows = parse_char_attack(&bytes).expect("parses");
        let hero = &rows[0];
        assert_eq!(hero.min_dmg, 1);
        assert_eq!(hero.max_dmg, 2);
        assert_eq!(hero.ac, 5);
        assert_eq!(hero.attack_interval, 1000);
        assert_eq!(hero.exp, 1);
        // Chm (17) and Mth (20) are parsed by the loader and never stored —
        // `clBA.Set(Str, Dex, Vit, Mag, Hnd, Luk, Wis)` (`:692`).
        assert_eq!(
            (
                hero.str, hero.dex, hero.vit, hero.intl, hero.hnd, hero.luk, hero.wis
            ),
            (5, 5, 5, 5, 5, 5, 5)
        );
    }

    #[test]
    fn a_file_without_the_data_marker_is_refused_not_read_empty() {
        let bytes = "no marker here"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes());
        assert_eq!(
            parse_kind_create(&bytes.collect::<Vec<_>>()).unwrap_err(),
            KindDataError::NoDataMarker
        );
    }

    #[test]
    fn level_scaling_and_the_hp_formula_move_together() {
        let create = KindCreateRow {
            code: "KIND_X".to_owned(),
            sort_num: 1,
            move_speed: 7,
            block: BLOCK_NORMAL,
        };
        let attack = CharAttackRow {
            code: "KIND_X".to_owned(),
            attack_type: "ATTACKTYPE_HAND".to_owned(),
            aggressive: 0,
            range: 1,
            min_dmg: 3,
            max_dmg: 5,
            critical_rate: 0,
            attack_interval: 1000,
            ac: 9,
            str: 10,
            dex: 20,
            vit: 30,
            intl: 0,
            hnd: 5,
            luk: 5,
            wis: 5,
            exp: 1,
            attack_e: 0,
            attack_r: 0,
            defen_e: 0,
            defen_r: 0,
        };
        // Switch off: stats scale with level, HP is Vit * 4.
        let legacy = MonsterStats::at(&create, &attack, 10, false);
        assert_eq!(legacy.str, 10 + 9 * 2);
        assert_eq!(legacy.dex, 20 + 9);
        assert_eq!(legacy.vit, 30 + 9);
        assert_eq!(legacy.max_hp, (30 + 9) * VIT_LIFE_RATE);
        assert_eq!((legacy.min_atk, legacy.max_atk, legacy.ac), (3, 5, 9));
        assert_eq!(legacy.move_speed, 7);
        // Mag stays 0 when the kind has none (`CommonLogic-SetChar.cpp:118-121`).
        assert_eq!(legacy.intl, 0);
        // Switch on: raw stats, new formula with C truncation.
        let new = MonsterStats::at(&create, &attack, 10, true);
        assert_eq!(new.vit, 30);
        assert_eq!(new.max_hp, 30 * (8000 + 10 * 5) / 1000);
        // Level 1 is the raw table in both branches.
        assert_eq!(MonsterStats::at(&create, &attack, 1, false).max_hp, 30 * 4);
        assert_eq!(MonsterStats::at(&create, &attack, 1, true).max_hp, 30 * 8);
    }

    /// The shipped files, parsed end to end — skipped when the game data is
    /// not on this machine (same convention as `map.rs`).
    #[test]
    fn loads_the_shipped_kind_files() {
        let create_path =
            std::path::Path::new("C:/Data/04_Server_Goonzu/New Era Live/Data/KindInfo_Create.txt");
        let attack_path = std::path::Path::new(
            "C:/Data/04_Server_Goonzu/New Era Live/CharInfo/CharAttackInfoE.txt",
        );
        if !create_path.exists() || !attack_path.exists() {
            return; // the game data is not part of this repository
        }
        let creates =
            parse_kind_create(&std::fs::read(create_path).expect("readable")).expect("parses");
        let attacks =
            parse_char_attack(&std::fs::read(attack_path).expect("readable")).expect("parses");
        assert!(creates.len() >= 800, "824 shipped create rows");
        assert!(attacks.len() >= 800, "827 shipped attack rows");

        // KIND_BERSERKER is a real monster: aggressive attack row (258-460
        // damage) and the default land siblock.
        let berserker = creates
            .iter()
            .find(|r| r.code == "KIND_BERSERKER")
            .expect("KIND_BERSERKER is placed");
        assert_eq!(berserker.move_speed, 10);
        assert_eq!(
            berserker.block,
            BLOCK_NORMAL | BLOCK_SHALLOWWATER | BLOCK_DEEPWATER | BLOCK_CUSTOM
        );
        let attack = attacks
            .iter()
            .find(|r| r.code == "KIND_BERSERKER")
            .expect("KIND_BERSERKER has an attack row");
        assert_eq!((attack.min_dmg, attack.max_dmg), (258, 460));
        assert_eq!(attack.ac, 630);
        assert_eq!((attack.str, attack.dex, attack.vit), (143, 350, 125));

        let stats = MonsterStats::at(berserker, attack, 10, false);
        assert_eq!(stats.vit, 125 + 9);
        assert_eq!(stats.max_hp, (125 + 9) * VIT_LIFE_RATE);
        assert_eq!(stats.min_atk, 258);
        assert_eq!(stats.move_speed, 10);
    }

    /// The committed KR-baseline merge, parsed end to end. The file is in the
    /// repository, so this test always runs.
    #[test]
    fn the_canonical_table_carries_the_korean_stats_and_keeps_the_english_layer() {
        let path = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/CharAttackInfoCanonical.txt"
        ));
        let bytes = std::fs::read(path).expect("the canonical table is committed");
        let rows = parse_char_attack(&bytes).expect("parses");
        assert_eq!(
            rows.len(),
            829,
            "the EN row set plus the 2 Tier-B KR-only rows"
        );

        // KIND_RACCOON (map 108 starter): the KR combat values, with the EN-only
        // columns kept alongside.
        let raccoon = rows
            .iter()
            .find(|r| r.code == "KIND_RACCOON")
            .expect("RACCOON row");
        assert_eq!(raccoon.attack_type, "ATTACKTYPE_SWORD");
        assert_eq!((raccoon.min_dmg, raccoon.max_dmg, raccoon.ac), (1, 2, 5));
        assert_eq!(
            (raccoon.critical_rate, raccoon.attack_interval, raccoon.exp),
            (0, 2000, 4)
        );
        assert_eq!(
            (raccoon.str, raccoon.dex, raccoon.vit, raccoon.intl),
            (5, 3, 4, 0)
        );
        assert_eq!((raccoon.luk, raccoon.wis), (3, 3));
        // Level 1 on the KR Vit is the legacy HP formula.
        let create = KindCreateRow {
            code: "KIND_RACCOON".to_owned(),
            sort_num: 400,
            move_speed: 0,
            block: 0,
        };
        assert_eq!(
            MonsterStats::at(&create, raccoon, 1, false).max_hp,
            4 * VIT_LIFE_RATE
        );

        // KIND_Ancient_Xingshi01: the deep-table goldens from the audit.
        let xingshi = rows
            .iter()
            .find(|r| r.code == "KIND_Ancient_Xingshi01")
            .expect("Xingshi row");
        assert_eq!((xingshi.min_dmg, xingshi.max_dmg), (25688, 26753));
        assert_eq!(
            (xingshi.ac, xingshi.vit, xingshi.exp),
            (35550, 45900, 60627)
        );

        // KIND_BERSERKER is EN-only (provisional): its EN values are untouched.
        let berserker = rows
            .iter()
            .find(|r| r.code == "KIND_BERSERKER")
            .expect("BERSERKER row");
        assert_eq!(
            (berserker.min_dmg, berserker.max_dmg, berserker.ac),
            (258, 460, 630)
        );

        // Tier B: the 2 verified exact-code KR-only kinds (strategy doc 3.4) - a
        // KR-only code that already carries an EN wire id, so the EN client can
        // resolve it. Their KR rows are appended; the EN-only columns (SortNUM, Luk,
        // AttackE/R, DefenE/R, Wis) come through empty.
        let bombdragon = rows
            .iter()
            .find(|r| r.code == "KIND_BOMBDRAGON")
            .expect("BOMBDRAGON row");
        assert_eq!(bombdragon.attack_type, "ATTACKTYPE_SELFBOMB");
        assert_eq!(
            (bombdragon.min_dmg, bombdragon.max_dmg, bombdragon.ac),
            (90, 100, 80)
        );
        assert_eq!(
            (
                bombdragon.str,
                bombdragon.dex,
                bombdragon.vit,
                bombdragon.exp
            ),
            (100, 120, 150, 104)
        );
        let jangbogo = rows
            .iter()
            .find(|r| r.code == "KIND_JANGBOGO")
            .expect("JANGBOGO row");
        assert_eq!(jangbogo.attack_type, "ATTACKTYPE_WATERARROW");
        assert_eq!((jangbogo.min_dmg, jangbogo.max_dmg), (40, 50));
        assert_eq!((jangbogo.ac, jangbogo.vit, jangbogo.exp), (50, 80, 1));

        // Tier C: the 20 human-confirmed rename pairs (docs/MONSTER_PAIRING_CANDIDATES.md
        // section 2.1). The EN row keeps its code and English display name while taking
        // the KR combat values; the EN-only columns (SortNUM, Luk, AttackE/R, DefenE/R,
        // Wis) survive. KIND_DEACON <- KIND_BLOODDEACON.
        let deacon = rows
            .iter()
            .find(|r| r.code == "KIND_DEACON")
            .expect("DEACON row");
        assert_eq!(deacon.attack_type, "ATTACKTYPE_SWORD");
        assert_eq!((deacon.min_dmg, deacon.max_dmg, deacon.ac), (132, 149, 170));
        assert_eq!(
            (deacon.str, deacon.dex, deacon.vit, deacon.intl, deacon.exp),
            (101, 121, 305, 206, 371)
        );
        // The EN-only columns (not in the KR header) are still the EN values.
        assert_eq!((deacon.luk, deacon.wis), (5, 54));
    }

    /// The committed roster reconciliation and the spawn gate (strategy doc 3.4,
    /// 4): every kind in either roster or either spawn-group set, and the rule that
    /// a kind an English group spawns must carry a wire id. The only misses are the
    /// quest placeholders, which no roster holds. The file is committed, so this
    /// always runs; regenerate with `scripts/generate-kr-canonical-tables.ps1`.
    #[test]
    fn every_kind_an_english_group_spawns_resolves_to_a_wire_id() {
        let path = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/KindReconciliation.txt"
        ));
        let text = std::fs::read_to_string(path).expect("the reconciliation is committed");
        let mut lines = text.lines();
        assert_eq!(
            lines.next(),
            Some("code\ttier\ten_wire_id\tspawnable\tpairs_to")
        );

        let (mut rows, mut shared, mut en_only, mut kr_only) = (0, 0, 0, 0);
        let mut shared_without_id: Vec<&str> = Vec::new();
        let mut en_spawn_without_id: Vec<&str> = Vec::new();
        // Tier C: a KR-only code that a human paired to an EN code carries the EN
        // code's wire id and names the EN code it resolves to.
        let mut pairs_to: Vec<(&str, &str, &str)> = Vec::new();
        for line in lines.filter(|l| !l.is_empty()) {
            rows += 1;
            let f: Vec<&str> = line.split('\t').collect();
            assert_eq!(f.len(), 5, "row: {line}");
            match f[1] {
                "shared" => shared += 1,
                "en_only" => en_only += 1,
                "kr_only" => kr_only += 1,
                _ => {}
            }
            if f[2] == "-" {
                if f[1] == "shared" {
                    shared_without_id.push(f[0]);
                }
                if f[3].split('+').any(|s| s == "en") {
                    // The gate: a kind an EN group spawns must resolve, unless no
                    // roster holds it at all (the quest placeholders).
                    assert_eq!(
                        f[1], "unrostered",
                        "EN-spawned roster kind {} has no wire id",
                        f[0]
                    );
                    en_spawn_without_id.push(f[0]);
                }
            }
            if f[4] != "-" {
                assert_eq!(
                    f[1], "kr_only",
                    "rename pair source {} is not KR-only",
                    f[0]
                );
                assert_ne!(f[2], "-", "rename pair source {} carries no wire id", f[0]);
                pairs_to.push((f[0], f[2], f[4]));
            }
        }

        // en_only is 610, not 609: KIND_kelan and KIND_KELAN are the same kind under
        // the documented case-only-pair rule, so both rows carry the EN roster tier.
        assert_eq!(
            (rows, shared, en_only, kr_only),
            (1369, 218, 610, 499),
            "both rosters and both spawn-group sets, tiered"
        );

        // The 9 shared kinds still missing an EN `KindInfo_Unique` id (open item 3).
        let mut expected_shared = vec![
            "KIND_FireCrystal_State",
            "KIND_KoryuGURI_STATUE",
            "KIND_Koryu_CORALMUSHROOM",
            "KIND_Koryu_CRABGURI",
            "KIND_Koryu_SAMURAIBoss",
            "KIND_Koryu_SAMURAICRAB",
            "KIND_LandCrystal_State",
            "KIND_WaterCrystal_State",
            "KIND_WindCrystal_State",
        ];
        expected_shared.sort_unstable();
        shared_without_id.sort_unstable();
        assert_eq!(shared_without_id, expected_shared);

        // The only EN-group references that do not resolve: quest placeholders.
        let mut expected_en = vec!["KIND_QUESTETC1", "KIND_QUESTNPC1", "KIND_QUESTNPC2"];
        expected_en.sort_unstable();
        en_spawn_without_id.sort_unstable();
        assert_eq!(en_spawn_without_id, expected_en);

        // Tier C: the 39 rename pairs (sections 2.1, 2.2, and the three monster rows of
        // the 2.3 sprite screen). Each KR-only code now resolves to its EN code's wire id,
        // so it has left the quarantine while keeping its own row for the audit.
        let mut expected_pairs = vec![
            ("KIND_BEAST06YOUNG", "331", "KIND_HOR14YOUNG"),
            ("KIND_BLACKTIGER", "961", "KIND_BLACKTIGER_MELEE"),
            ("KIND_BLOODDEACON", "197", "KIND_DEACON"),
            ("KIND_BLOODPRIEST", "199", "KIND_PRIEST"),
            ("KIND_Bum_I", "931", "KIND_HUNTER_MELEE"),
            ("KIND_EMPERORWIZARD", "299", "KIND_EMPERORCAVALRY"),
            ("KIND_EMPERORWORKER", "301", "KIND_EMPERORPORTER"),
            ("KIND_FIREHAETAE", "358", "KIND_HOR16YOUNG"),
            ("KIND_FULLMOON", "332", "KIND_MOON"),
            ("KIND_GAGOIL", "203", "KIND_GARGOYLE"),
            ("KIND_GRAVEROBBERY", "300", "KIND_EMPERORROBBER"),
            ("KIND_Gum_I", "930", "KIND_GALE_RANGE"),
            ("KIND_KRAKEN", "635", "KIND_Kracken"),
            ("KIND_SASURI", "450", "KIND_HOR18YOUNG"),
            ("KIND_SPAMONKEY", "24", "KIND_MONKEYSON"),
            ("KIND_SPAPANDA", "23", "KIND_NINJAPANDA"),
            ("KIND_SPARACCON", "25", "KIND_RACCOONDOG"),
            ("KIND_SPATOAD", "26", "KIND_TOAD"),
            ("KIND_VIRGINGHOST", "604", "KIND_Ghost_NvGui1"),
            ("KIND_WERWOLF", "196", "KIND_WEREWOLF"),
            // Section 2.2 batch (2026-10-18): the 7 stage-3 family members and 9
            // standalones, three of them on corrected targets.
            ("KIND_BLACKPRIEST_3", "446", "KIND_BLACKPRIEST"),
            ("KIND_BLACKSOUL_3", "448", "KIND_BLACKSOUL"),
            ("KIND_CLOWN_3", "435", "KIND_CLOWN"),
            ("KIND_DUCHESS_3", "445", "KIND_DUCHESS"),
            ("KIND_FOGBRIDE_3", "436", "KIND_FOGBRIDE"),
            ("KIND_GENTLEMAN_3", "447", "KIND_GENTLEMAN"),
            ("KIND_GOONZUDAY", "693", "KIND_GOONZUDAYNPC"),
            ("KIND_KRAKEN_T", "636", "KIND_Kracken_T"),
            ("KIND_MINIRABBIT_GIRL", "474", "KIND_RABBITGIRL"),
            ("KIND_PAPAGOBLIN_FB", "305", "KIND_PAPAGOBLIN_2"),
            ("KIND_RUDOLF", "86", "KIND_RUDOLPH"),
            ("KIND_SOULGUARD_GodEagle", "571", "KIND_SOULGUARD_GoldEagle"),
            ("KIND_SOULGUARD_ManGhost", "699", "KIND_SOULGUARD_Ghostman"),
            (
                "KIND_SOULGUARD_WomanGhost",
                "698",
                "KIND_SOULGUARD_Ghostwoman",
            ),
            ("KIND_Soulguard_Miho", "816", "KIND_SOULGUARD_Miho_Elite"),
            ("KIND_THIRTEENFACE_3", "449", "KIND_THIRTEENFACE"),
            // Section 2.3 batch (2026-10-18): the monsters among the 7 candidates whose
            // sprite is unique on both sides. The four event props stay quarantined.
            ("KIND_breadtiger", "401", "KIND_DRAGON_BABY"),
            ("KIND_UNDERSEA_WHALE", "918", "KIND_DeepSeaWhale"),
            ("KIND_Xmas_Penguin", "960", "KIND_CAVINE_RANGE"),
        ];
        expected_pairs.sort_unstable();
        pairs_to.sort_unstable();
        assert_eq!(pairs_to, expected_pairs);
    }
}
