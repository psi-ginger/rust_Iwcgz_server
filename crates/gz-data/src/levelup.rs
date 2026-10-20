//! `LevelUpExp.dat` — the cumulative character experience thresholds.
//!
//! One row per level after a five-line banner (title, build, version,
//! generator, column names), tab-separated: `Level  NeedExp  IncExp  Calc
//! Vary`. `NeedExp` is the **cumulative** experience a character must hold to
//! *be* that level. The legacy reads the file from its working directory
//! (`LoadExpData`, `Parametabox.cpp:8300-8378`) into
//! `ExpForLevelUp[MAX_EXPTABLE_LEVEL]` (`common/CommonHeader.h:81`) and asks it
//! through `GetRequiredExpForLevel` (`Parametabox.cpp:8226`).
//!
//! shortcut: the shipped English file (`New Era Live\LevelUpExp.dat`) is
//! plaintext; an NCR-crypted build (`NLoadCryptFile`) parses as garbage and
//! [`LevelUpTable::load`] fails loudly, which the game service degrades from
//! like any other missing data tree. Port `NByteDecrypt` (`NUtil.cpp`) if a
//! data set ships encrypted.

/// `MAX_CHARACTER_LEVEL` — the level cap (`common/CommonHeader.h:73`, the
/// non-NHN build).
pub const MAX_CHARACTER_LEVEL: i16 = 120;

/// `MAX_EXPTABLE_LEVEL` — rows the legacy keeps (`common/CommonHeader.h:72`).
/// The shipped file carries 251; everything past this is ignored, the way
/// `LoadExpData` stops at `ExpForLevelUp.GetSize()`.
const MAX_EXPTABLE_LEVEL: usize = 120;

/// The threshold table: `need[i]` is `ExpForLevelUp[i][1]`, the cumulative exp
/// to *be* level `i + 1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LevelUpTable {
    need: Vec<i64>,
}

impl LevelUpTable {
    /// No rows — every threshold read is [`None`](Self::next_threshold) and
    /// nobody levels. What the game service falls back to when
    /// `<root>/LevelUpExp.dat` will not load, deliberately unlike the legacy:
    /// its empty table makes `GetRequiredExpForLevel` return 0 past
    /// `GetSize()` (`Parametabox.cpp:8229`), so `IncreaseExp` would return
    /// TRUE on every grant and level the character on every kill.
    #[must_use]
    pub fn empty() -> Self {
        Self { need: Vec::new() }
    }

    /// Reads and parses `<root>/LevelUpExp.dat`, the file the legacy opens by
    /// relative name (`LoadExpData`, `Parametabox.cpp:8315`).
    ///
    /// # Errors
    ///
    /// The `io::Error` from the read, or fewer than [`MAX_EXPTABLE_LEVEL`]
    /// data rows as [`io::ErrorKind::InvalidData`].
    pub fn load(root: &std::path::Path) -> std::io::Result<Self> {
        let bytes = std::fs::read(root.join("LevelUpExp.dat"))?;
        Self::parse(&bytes).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// Parses the file's bytes: banner and column lines carry no integer
    /// second field and are skipped by content, where the legacy counts line
    /// endings instead (`lc < 5`) — content survives a header of a different
    /// length. Columns are positional as in `LoadExpData`; only the second
    /// (`NeedExp`) is kept, and rows past [`MAX_EXPTABLE_LEVEL`] are dropped.
    ///
    /// # Errors
    ///
    /// Fewer than [`MAX_EXPTABLE_LEVEL`] data rows, naming the count.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let text = String::from_utf8_lossy(bytes);
        let mut need = Vec::with_capacity(MAX_EXPTABLE_LEVEL);
        for line in text.lines() {
            if need.len() == MAX_EXPTABLE_LEVEL {
                break;
            }
            let mut cols = line.split_whitespace();
            let Some(_first) = cols.next() else {
                continue; // blank line
            };
            let Some(need_exp) = cols.next().and_then(|field| field.parse::<i64>().ok()) else {
                continue; // `LevelUpExp`, `38804`, `V0.2`, `GamePot`, `Level NeedExp ...`
            };
            need.push(need_exp);
        }
        if need.len() < MAX_EXPTABLE_LEVEL {
            return Err(format!(
                "LevelUpExp.dat holds {} of {MAX_EXPTABLE_LEVEL} level rows",
                need.len()
            ));
        }
        Ok(Self { need })
    }

    /// `GetRequiredExpForLevel(level + 1)` (`Parametabox.cpp:8226-8246`): the
    /// total a character at `level` must hold to become `level + 1`. `None`
    /// below level 1 and at the cap — row 121 does not exist, and the legacy
    /// stops on `GetLevel() < MAX_CHARACTER_LEVEL` (`:49`) at the same point.
    #[must_use]
    pub fn next_threshold(&self, level: i16) -> Option<i64> {
        if level < 1 {
            return None;
        }
        usize::try_from(level)
            .ok()
            .and_then(|index| self.need.get(index))
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped file's real banner and real end rows: the parse must not
    /// care that the banner is words, and must land column 2 (`NeedExp`) in
    /// the right place at both ends of the table.
    fn synthetic() -> String {
        let mut file = String::from(
            "LevelUpExp\t\t\t\t\r\n\
             38804\t\t\t\t\r\n\
             V0.2\t\t\t\t\r\n\
             GamePot\t\t\t\t\r\n\
             Level\tNeedExp\tIncExp\tCalc\tVary\r\n",
        );
        file.push_str("1\t0\t75\t20\t20\r\n");
        file.push_str("2\t75\t105\t40\t20\r\n");
        file.push_str("3\t180\t165\t60\t20\r\n");
        for level in 4..=119 {
            file.push_str(&format!("{level}\t{level}\t0\t0\t0\r\n"));
        }
        // The cap row as shipped (`New Era Live\LevelUpExp.dat`).
        file.push_str("120\t9713379124\t1010156892\t49834407\t768611\r\n");
        // The file carries 251 levels; the tail is ignored.
        file.push_str("121\t10723536016\t1084908502\t55691970\t5857563\r\n");
        file
    }

    #[test]
    fn thresholds_read_need_exp_by_row() {
        let table = LevelUpTable::parse(synthetic().as_bytes()).expect("120 rows");
        assert_eq!(table.next_threshold(0), None, "no level below 1");
        assert_eq!(table.next_threshold(1), Some(75), "level 2 needs 75");
        assert_eq!(table.next_threshold(2), Some(180), "level 3 needs 180");
        assert_eq!(
            table.next_threshold(119),
            Some(9_713_379_124),
            "level 120 needs the shipped cumulative"
        );
        assert_eq!(table.next_threshold(120), None, "the cap has no next level");
        assert_eq!(table.next_threshold(121), None, "nor past it");
    }

    #[test]
    fn an_empty_table_refuses_every_threshold() {
        assert_eq!(LevelUpTable::empty().next_threshold(1), None);
    }

    #[test]
    fn a_short_table_is_rejected() {
        let short: String = synthetic()
            .lines()
            .take(5 + 119)
            .map(|line| format!("{line}\r\n"))
            .collect();
        let err = LevelUpTable::parse(short.as_bytes()).expect_err("119 rows");
        assert!(err.contains("119 of 120"), "{err}");
    }
}
