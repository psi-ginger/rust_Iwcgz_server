//! Re-reading a capture without a JSON parser.
//!
//! `--verify` exists so CI can assert a recorded corpus is intact (plan §10:
//! "recorded sessions must replay clean"). It deliberately does **not** parse
//! `.meta.json` — there is no JSON parser in this crate, and the timing sidecar
//! is line-oriented, so the chunk lengths can be summed with `split` and checked
//! against the recorded byte counts.
//!
//! The strongest check available without a full parser is therefore:
//!
//! 1. both direction files exist and are readable;
//! 2. the per-chunk lengths in the sidecar sum to exactly each file's length —
//!    which would catch truncation, a dropped append, or a chunk recorded but
//!    never written;
//! 3. timestamps never go backwards within a direction.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

use crate::recorder::{Dir, data_path, timing_path};

/// What `--verify` found.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Per-direction byte totals read from the sidecar.
    pub bytes: BTreeMap<String, u64>,
    /// Per-direction chunk counts.
    pub chunks: BTreeMap<String, u64>,
    /// Per-direction on-disk file length.
    pub files: BTreeMap<String, u64>,
    /// Human-readable problems, empty when the capture is consistent.
    pub problems: Vec<String>,
}

impl Report {
    /// Whether the capture is internally consistent.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.problems.is_empty()
    }
}

/// Check the capture stored at `base` (the stem, e.g. `sessions/login`).
pub fn verify(base: &Path) -> io::Result<Report> {
    let mut report = Report::default();

    for dir in [Dir::C2s, Dir::S2c] {
        let path = data_path(base, dir);
        let len = match fs::metadata(&path) {
            Ok(m) => m.len(),
            Err(e) => {
                report.problems.push(format!("{}: {e}", path.display()));
                continue;
            }
        };
        report.files.insert(dir.as_str().to_owned(), len);
    }

    let sidecar = timing_path(base);
    let text = match fs::read_to_string(&sidecar) {
        Ok(t) => t,
        Err(e) => {
            report.problems.push(format!("{}: {e}", sidecar.display()));
            return Ok(report);
        }
    };

    let mut last_t: BTreeMap<String, u64> = BTreeMap::new();
    for (lineno, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let dir = field(line, "dir");
        let t = field(line, "t_us").and_then(|v| v.parse::<u64>().ok());
        let n = field(line, "n").and_then(|v| v.parse::<u64>().ok());
        let (dir, t, n) = match (dir, t, n) {
            (Some(d), Some(t), Some(n)) => (d, t, n),
            _ => {
                report.problems.push(format!(
                    "{}:{}: malformed chunk",
                    sidecar.display(),
                    lineno + 1
                ));
                continue;
            }
        };
        if let Some(prev) = last_t.insert(dir.clone(), t)
            && t < prev
        {
            report.problems.push(format!(
                "{}:{}: timestamp went backwards for {dir} ({prev} -> {t})",
                sidecar.display(),
                lineno + 1
            ));
        }
        *report.bytes.entry(dir.clone()).or_default() += n;
        *report.chunks.entry(dir).or_default() += 1;
    }

    for (dir, len) in &report.files {
        match report.bytes.get(dir) {
            Some(summed) if summed == len => {}
            Some(summed) => report.problems.push(format!(
                "{dir}: sidecar records {summed} bytes but {} holds {len}",
                data_path(base, dir_of(dir)).display()
            )),
            None => report
                .problems
                .push(format!("{dir}: no chunks recorded in the sidecar")),
        }
    }

    Ok(report)
}

fn dir_of(name: &str) -> Dir {
    if name == Dir::S2c.as_str() {
        Dir::S2c
    } else {
        Dir::C2s
    }
}

/// Pull `"key":value` out of a sidecar line. Values are plain, unescaped
/// strings or integers, which is all the writer ever emits.
fn field(line: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    if let Some(stripped) = rest.strip_prefix('"') {
        let end = stripped.find('"')?;
        Some(stripped[..end].to_owned())
    } else {
        let end = rest.find([',', '}']).unwrap_or(rest.len());
        Some(rest[..end].to_owned())
    }
}
