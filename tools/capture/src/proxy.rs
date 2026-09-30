//! The proxy itself: connect, pump both directions, record.
//!
//! Three rules make this trustworthy as a *recording* proxy, and each is easy to
//! get subtly wrong:
//!
//! 1. **Forward with `write_all`, never `write`.** A short write would put the
//!    forwarded stream out of order relative to what is recorded.
//! 2. **Half-close, never hard-close.** When a direction reaches EOF the peer's
//!    *write* half is shut down so the far end sees end-of-stream while the other
//!    direction keeps running. Closing both sockets on the first EOF truncates
//!    sessions, which would silently corrupt the corpus.
//! 3. **A late transport error still writes the metadata.** Everything recorded
//!    up to the failure is worth keeping, and `end_reason` records what happened.
//!    Losing a session because it ended badly would defeat the point.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::recorder::{CHUNK, Direction, Session, SessionInfo, now_unix, write_meta};

/// Shared end-of-session state: the first direction to end decides why, and an
/// error overrides a clean end so the reason stays accurate.
#[derive(Debug, Default)]
pub struct Done {
    finished: AtomicBool,
    reason: Mutex<Option<String>>,
}

impl Done {
    /// Mark the session ended cleanly, unless something already claimed it.
    pub fn complete(&self) {
        if self.finished.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Ok(mut slot) = self.reason.lock() {
            *slot = Some("clean".to_owned());
        }
    }

    /// Mark the session ended in error. Overrides a previous clean mark, so the
    /// recorded reason is never "clean" for a session that actually faulted.
    pub fn fail(&self, why: &str) {
        self.finished.store(true, Ordering::SeqCst);
        if let Ok(mut slot) = self.reason.lock() {
            *slot = Some(format!("error: {why}"));
        }
    }

    /// The end reason recorded so far.
    #[must_use]
    pub fn reason(&self) -> Option<String> {
        self.reason.lock().ok().and_then(|r| r.clone())
    }
}

/// The sockets either pump thread can force shut, to unblock the other.
#[derive(Debug)]
struct Kill {
    client: TcpStream,
    server: TcpStream,
}

impl Kill {
    fn trip(&self) {
        let _ = self.client.shutdown(Shutdown::Both);
        let _ = self.server.shutdown(Shutdown::Both);
    }
}

/// Copy one direction, recording everything on the way through.
///
/// Returns when the source reaches EOF, or on a transport error. On a clean end
/// the destination's write half is shut down so the far end observes EOF.
pub fn pump(
    mut from: TcpStream,
    mut to: TcpStream,
    rec: &mut Direction,
    started: &std::time::Instant,
) -> io::Result<()> {
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = match from.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        // Record before forwarding, so the file reflects what was on the wire
        // even if the forward then fails.
        rec.record(started.elapsed().as_micros() as u64, &buf[..n])?;
        to.write_all(&buf[..n])?;
    }
    let _ = to.shutdown(Shutdown::Write);
    Ok(())
}

/// Result of proxying one connection.
#[derive(Debug)]
pub struct Served {
    /// The metadata that was written.
    pub meta: crate::meta::Meta,
    /// Where the metadata was written.
    pub meta_path: PathBuf,
}

/// Proxy exactly one already-accepted client connection.
///
/// Returns once both directions have ended. A transport failure is reported
/// through [`Served::meta`]'s `end_reason` rather than as an `Err`, because the
/// bytes recorded before the failure are still worth keeping; an `Err` here means
/// the capture could not be written at all, or a pump thread panicked.
pub fn serve_connection(
    client: TcpStream,
    target: &str,
    out_base: &Path,
    listen_label: &str,
) -> io::Result<Served> {
    let server = TcpStream::connect(target)?;
    // Small packets must not wait on Nagle: this is a capture tool, so latency
    // is noise and coalescing would blur the chunk timings in the sidecar.
    let _ = client.set_nodelay(true);
    let _ = server.set_nodelay(true);

    let started_unix = now_unix();
    let session = Session::create(out_base, listen_label, target)?;
    let (info, mut c2s, mut s2c) = session.into_parts();
    let clock = Arc::clone(&info.started);

    let done = Arc::new(Done::default());
    let kill = Arc::new(Kill {
        client: client.try_clone()?,
        server: server.try_clone()?,
    });

    let up = {
        let (kill, done, clock) = (Arc::clone(&kill), Arc::clone(&done), Arc::clone(&clock));
        let (from, to) = (client.try_clone()?, server.try_clone()?);
        thread::spawn(move || {
            let result = pump(from, to, &mut c2s, &clock);
            match &result {
                Ok(()) => done.complete(),
                Err(e) => {
                    done.fail(&e.to_string());
                    kill.trip();
                }
            }
            (result, c2s)
        })
    };

    let down = {
        let (kill, done, clock) = (Arc::clone(&kill), Arc::clone(&done), Arc::clone(&clock));
        thread::spawn(move || {
            let result = pump(server, client, &mut s2c, &clock);
            if let Err(e) = &result {
                done.fail(&e.to_string());
                kill.trip();
            }
            (result, s2c)
        })
    };

    // A panic means a bug in the pump, not a transport problem, so it is fatal.
    let (up_result, c2s) = up
        .join()
        .map_err(|_| io::Error::other("c2s pump thread panicked"))?;
    let (down_result, s2c) = down
        .join()
        .map_err(|_| io::Error::other("s2c pump thread panicked"))?;
    drop((up_result, down_result));

    let reason = done.reason().unwrap_or_else(|| "clean".to_owned());
    let (meta, meta_path) = write_meta(&info, c2s.finish()?, s2c.finish()?, started_unix, &reason)?;
    Ok(Served { meta, meta_path })
}

/// Listen and proxy connections one at a time.
///
/// Sequential on purpose: a corpus is recorded one session at a time, and it
/// keeps thread and file-handle counts bounded however the client behaves.
pub fn run(listen: &str, target: &str, out_base: &Path) -> io::Result<()> {
    let listener = TcpListener::bind(listen)?;
    let bound = listener.local_addr()?;
    eprintln!("capture: listening on {bound} -> {target}");
    eprintln!("capture: writing captures under {}", out_base.display());
    for incoming in listener.incoming() {
        let client = match incoming {
            Ok(c) => c,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        match serve_connection(client, target, out_base, &bound.to_string()) {
            Ok(served) => {
                let m = &served.meta;
                eprintln!(
                    "capture: session done ({}) c2s={}B/{}chunks s2c={}B/{}chunks {}ms -> {}",
                    m.end_reason,
                    m.c2s.bytes,
                    m.c2s.chunks,
                    m.s2c.bytes,
                    m.s2c.chunks,
                    m.duration_ms,
                    served.meta_path.display()
                );
            }
            Err(e) => eprintln!("capture: session failed: {e}"),
        }
    }
    Ok(())
}

/// The session fields the metadata needs, re-exported for callers building a
/// capture by hand in tests.
pub type Info = SessionInfo;
