//! Loopback byte-exactness: the acceptance test for task T-007.
//!
//! These spin up a real target listener and proxy a live TCP session through
//! `serve_connection`, then assert the recorded files contain exactly the bytes
//! that crossed the wire. A capture whose files differ from the stream by even
//! one byte is worse than no capture at all, so the tests compare against
//! deterministic patterns rather than trusting any round trip.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use capture::recorder::{Dir, data_path, meta_path};
use capture::verify;

const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Deterministic pseudo-random byte pattern, so any single missing or reordered
/// byte is caught by an equality assertion.
fn pattern(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed;
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as u8
        })
        .collect()
}

fn read_all(stream: &mut TcpStream) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => return out,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => panic!("read failed: {e}"),
        }
    }
}

fn write_all(mut stream: &TcpStream, data: &[u8]) {
    stream.write_all(data).unwrap();
}

fn next_test_dir(name: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("capture-t007-{name}-{nanos}-{n}"))
}

/// A session where the client sends a large pattern and the target answers with
/// a *different* pattern, so both directions are verified independently.
#[test]
fn captures_exact_bytes_in_both_directions() {
    let c2s_expected = pattern(0xDEAD_BEEF, 1_000_000);
    let s2c_expected = pattern(0x00C0_FFEE, 234_567);

    let server_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let server_addr = server_listener.local_addr().unwrap();
    let proxy_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let out_base = next_test_dir("byte-exact").join("session");
    let target = server_addr.to_string();

    let server_thread = {
        let c2s_expected = c2s_expected.clone();
        let s2c_expected = s2c_expected.clone();
        thread::spawn(move || {
            let (mut server, _) = server_listener.accept().unwrap();
            server.set_read_timeout(Some(READ_TIMEOUT)).unwrap();
            assert_eq!(
                read_all(&mut server),
                c2s_expected,
                "target must receive exactly what the client sent"
            );
            write_all(&server, &s2c_expected);
            let _ = server.shutdown(Shutdown::Write);
        })
    };

    let serve_thread = {
        let base = out_base.clone();
        thread::spawn(move || {
            let (client, _) = proxy_listener.accept().unwrap();
            capture::proxy::serve_connection(client, &target, &base, &proxy_addr.to_string())
                .unwrap()
        })
    };

    let mut client = TcpStream::connect(proxy_addr).unwrap();
    client.set_read_timeout(Some(READ_TIMEOUT)).unwrap();
    write_all(&client, &c2s_expected);
    client.shutdown(Shutdown::Write).unwrap();
    assert_eq!(read_all(&mut client), s2c_expected);

    server_thread.join().unwrap();
    let served = serve_thread.join().unwrap();
    assert_eq!(served.meta.end_reason, "clean");

    // The recorded files are the whole point: they must equal the wire bytes.
    assert_eq!(
        std::fs::read(data_path(&out_base, Dir::C2s)).unwrap(),
        c2s_expected
    );
    assert_eq!(
        std::fs::read(data_path(&out_base, Dir::S2c)).unwrap(),
        s2c_expected
    );

    let report = verify::verify(&out_base).unwrap();
    assert!(report.is_ok(), "verify failed: {:?}", report.problems);

    let meta = std::fs::read_to_string(meta_path(&out_base)).unwrap();
    assert!(meta.contains(&format!("\"bytes\":{}", c2s_expected.len())));
    assert!(meta.contains(&format!("\"bytes\":{}", s2c_expected.len())));
    assert!(meta.contains("\"end_reason\":\"clean\""));
}

/// The client half-closes (FIN) after sending a short message but keeps reading.
/// The target must still receive the message *and* observe the FIN, then answer.
/// A proxy that hard-closed both sockets on the first EOF would truncate this.
#[test]
fn provokes_and_propagates_half_close() {
    let request = b"shutdown-write-now".to_vec();
    let response = b"hello-from-the-other-side".to_vec();

    let server_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let server_addr = server_listener.local_addr().unwrap();
    let proxy_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let out_base = next_test_dir("half-close").join("session");
    let target = server_addr.to_string();

    // Target: read until the client's FIN arrives (the proxy must forward it),
    // then answer — proving the response stream survives the half-close.
    let server_thread = {
        let request = request.clone();
        let response = response.clone();
        thread::spawn(move || {
            let (mut server, _) = server_listener.accept().unwrap();
            server.set_read_timeout(Some(READ_TIMEOUT)).unwrap();
            assert_eq!(read_all(&mut server), request);
            write_all(&server, &response);
            let _ = server.shutdown(Shutdown::Write);
        })
    };

    let serve_thread = {
        let base = out_base.clone();
        thread::spawn(move || {
            let (client, _) = proxy_listener.accept().unwrap();
            capture::proxy::serve_connection(client, &target, &base, &proxy_addr.to_string())
                .unwrap()
        })
    };

    let mut client = TcpStream::connect(proxy_addr).unwrap();
    client.set_read_timeout(Some(READ_TIMEOUT)).unwrap();
    write_all(&client, &request);
    client.shutdown(Shutdown::Write).unwrap();
    assert_eq!(
        read_all(&mut client),
        response,
        "client must receive the full answer after its own half-close"
    );

    server_thread.join().unwrap();
    let served = serve_thread.join().unwrap().meta;
    assert_eq!(served.end_reason, "clean");
    assert_eq!(
        std::fs::read(data_path(&out_base, Dir::C2s)).unwrap(),
        request
    );
    assert_eq!(
        std::fs::read(data_path(&out_base, Dir::S2c)).unwrap(),
        response
    );
    assert!(verify::verify(&out_base).unwrap().is_ok());
}
