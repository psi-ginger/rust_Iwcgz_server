//! On-disk capture: one file per direction, plus the timing sidecar.
//!
//! The contract this module exists to keep is simple and load-bearing: **the
//! concatenation of everything recorded in a direction is exactly the byte
//! stream that direction carried.** Everything else is reporting.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::json::Obj;
use crate::meta::{DirectionMeta, Meta};

/// Read buffer size per direction. Large enough that a busy game link does not
/// syscall once per packet, small enough to stay off the stack.
pub const CHUNK: usize = 64 * 1024;

/// Which way bytes were moving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// Client to server.
    C2s,
    /// Server to client.
    S2c,
}

impl Dir {
    /// The short name used in file names and in the timing sidecar.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::C2s => "c2s",
            Self::S2c => "s2c",
        }
    }
}

/// A writer shared by both directions so the session has exactly one timing
/// sidecar instead of two recorders silently truncating each other's file.
type Timing = Arc<Mutex<BufWriter<File>>>;

/// One direction's recording.
#[derive(Debug)]
pub struct Direction {
    dir: Dir,
    out: BufWriter<File>,
    timing: Timing,
    bytes: u64,
    chunks: u64,
    first_byte_us: Option<u64>,
    last_byte_us: Option<u64>,
}

impl Direction {
    fn open(dir: Dir, base: &Path, timing: Timing) -> io::Result<Self> {
        Ok(Self {
            dir,
            out: BufWriter::new(File::create(data_path(base, dir))?),
            timing,
            bytes: 0,
            chunks: 0,
            first_byte_us: None,
            last_byte_us: None,
        })
    }

    /// Append a chunk exactly as it was read from the socket.
    ///
    /// Recording happens *before* the bytes are forwarded onward, so the file
    /// reflects what was on the wire even if forwarding then fails.
    pub fn record(&mut self, elapsed_us: u64, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.out.write_all(bytes)?;
        let mut line = Obj::new();
        line.str("dir", self.dir.as_str())
            .uint("t_us", elapsed_us)
            .uint("n", bytes.len() as u64);
        let mut timing = self.timing.lock().unwrap();
        timing.write_all(line.finish().as_bytes())?;
        timing.write_all(b"\n")?;
        drop(timing);

        self.bytes += bytes.len() as u64;
        self.chunks += 1;
        self.first_byte_us.get_or_insert(elapsed_us);
        self.last_byte_us = Some(elapsed_us);
        Ok(())
    }

    /// Total bytes recorded so far.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Number of chunks recorded so far.
    #[must_use]
    pub fn chunks(&self) -> u64 {
        self.chunks
    }

    /// Flush both files, then summarise.
    pub fn finish(mut self) -> io::Result<DirectionMeta> {
        self.out.flush()?;
        self.timing.lock().unwrap().flush()?;
        Ok(DirectionMeta {
            bytes: self.bytes,
            chunks: self.chunks,
            first_byte_us: self.first_byte_us,
            last_byte_us: self.last_byte_us,
        })
    }
}

/// What the metadata document needs from a session, kept after the recorders
/// have been moved onto their own threads.
#[derive(Debug)]
pub struct SessionInfo {
    /// The capture stem actually used, which may carry a `-2` suffix.
    pub base: PathBuf,
    /// When the session started.
    pub started: Arc<Instant>,
    /// Address the proxy listened on.
    pub listen: String,
    /// Address the proxy forwarded to.
    pub target: String,
}

/// A capture in progress. Owns one recorder per direction.
#[derive(Debug)]
pub struct Session {
    info: SessionInfo,
    c2s: Direction,
    s2c: Direction,
}

impl Session {
    /// Create the capture files for `base`, which is a *stem* with no extension.
    ///
    /// If files for that stem already exist, a `-2`, `-3`, … suffix is appended
    /// rather than overwriting. A golden corpus artifact that silently vanishes
    /// is worse than an awkward file name.
    pub fn create(base: &Path, listen: &str, target: &str) -> io::Result<Self> {
        let base = unique_base(base)?;
        if let Some(parent) = base.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        // One timing file for the whole session, shared by both directions.
        let timing: Timing = Arc::new(Mutex::new(BufWriter::new(File::create(timing_path(
            &base,
        ))?)));
        Ok(Self {
            c2s: Direction::open(Dir::C2s, &base, Arc::clone(&timing))?,
            s2c: Direction::open(Dir::S2c, &base, Arc::clone(&timing))?,
            info: SessionInfo {
                base,
                started: Arc::new(Instant::now()),
                listen: listen.to_owned(),
                target: target.to_owned(),
            },
        })
    }

    /// The stem this session is writing to.
    #[must_use]
    pub fn base(&self) -> &Path {
        &self.info.base
    }

    /// Split into the two recorders plus the info the metadata will need, so each
    /// direction can be pumped by its own thread.
    #[must_use]
    pub fn into_parts(self) -> (SessionInfo, Direction, Direction) {
        (self.info, self.c2s, self.s2c)
    }
}

/// Flush nothing (already done) and write the `.meta.json` document.
pub fn write_meta(
    info: &SessionInfo,
    c2s: DirectionMeta,
    s2c: DirectionMeta,
    started_unix: i64,
    end_reason: &str,
) -> io::Result<(Meta, PathBuf)> {
    let meta = Meta {
        tool: format!("capture {}", env!("CARGO_PKG_VERSION")),
        started_at: crate::meta::iso8601_utc(started_unix),
        started_unix,
        duration_ms: info.started.elapsed().as_millis() as u64,
        listen: info.listen.clone(),
        target: info.target.clone(),
        end_reason: end_reason.to_owned(),
        c2s,
        s2c,
    };
    let path = meta_path(&info.base);
    std::fs::write(&path, format!("{}\n", meta.to_json()))?;
    Ok((meta, path))
}

/// Current wall-clock time as Unix seconds.
#[must_use]
pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// `<base>.<dir>.bin`
#[must_use]
pub fn data_path(base: &Path, dir: Dir) -> PathBuf {
    base.with_file_name(format!("{}.{}.bin", stem_name(base), dir.as_str()))
}

/// `<base>.timing.jsonl`
#[must_use]
pub fn timing_path(base: &Path) -> PathBuf {
    stem(base, "timing.jsonl")
}

/// `<base>.meta.json`
#[must_use]
pub fn meta_path(base: &Path) -> PathBuf {
    stem(base, "meta.json")
}

fn stem_name(base: &Path) -> &str {
    base.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("capture")
}

fn stem(base: &Path, suffix: &str) -> PathBuf {
    base.with_file_name(format!("{}.{suffix}", stem_name(base)))
}

fn unique_base(base: &Path) -> io::Result<PathBuf> {
    if !data_path(base, Dir::C2s).exists() && !meta_path(base).exists() {
        return Ok(base.to_path_buf());
    }
    let name = stem_name(base);
    let parent = base.parent().map(Path::to_path_buf).unwrap_or_default();
    for n in 2..10_000 {
        let candidate = parent.join(format!("{name}-{n}"));
        if !data_path(&candidate, Dir::C2s).exists() && !meta_path(&candidate).exists() {
            return Ok(candidate);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("no unused capture name near {}", base.display()),
    ))
}
