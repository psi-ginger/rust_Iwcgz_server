//! Deterministic property tests for the T-009 fuzz targets' invariants.
//!
//! **This is not a substitute for libFuzzer.** It has no coverage feedback and
//! no mutation engine, so it explores far less of the input space than the
//! targets in `fuzz/fuzz_targets/`. It lives here, in the main workspace,
//! rather than in `fuzz/` for two reasons:
//!
//! 1. It runs on **stable** with no extra toolchain, so it is a real CI gate on
//!    every machine rather than only where the fuzz toolchain is set up.
//! 2. `cargo test` inside `fuzz/` also tries to build the libFuzzer binaries,
//!    which need ASan and a nightly toolchain; keeping the harness in the
//!    workspace keeps `cargo test --workspace` green.
//!
//! The invariants below are deliberately the *same* ones the fuzz targets
//! assert, so a regression shows up here first and the fuzz target confirms it.
//!
//! Run: `cargo test -p gz-net --release`

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// A tiny deterministic PRNG (xorshift64*). Fixed seed => reproducible runs,
/// so a failure can always be re-triggered from the printed case number.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }
    fn fill(&mut self, buf: &mut [u8]) {
        for b in buf {
            *b = (self.next_u64() >> 24) as u8;
        }
    }
}

// ---------------------------------------------------------------- frame parser

/// Mirrors the `frame_parser` fuzz target's invariants.
fn check_frame_parser(data: &[u8]) {
    let split_byte = data.first().copied().unwrap_or(0);
    let body = if data.is_empty() { &[] } else { &data[1..] };

    let mut parser = gz_net::FrameParser::new();
    let mut fed = 0usize;
    let mut drained = 0usize;
    let mut first_error: Option<String> = None;

    let mut at = 0usize;
    while at < body.len() {
        let take = if split_byte == 0 {
            body.len() - at
        } else {
            let span = 1 + (at.wrapping_mul(31).wrapping_add(split_byte as usize) % 8);
            span.min(body.len() - at)
        };
        let chunk = &body[at..at + take];
        at += take;
        fed += take;

        match parser.feed(chunk) {
            Ok(()) => {}
            Err(e) => {
                let msg = e.to_string();
                match &first_error {
                    None => first_error = Some(msg),
                    Some(prev) => assert_eq!(prev, &msg, "failure changed after rejection"),
                }
            }
        }

        while let Ok(Some(frame)) = parser.next_frame() {
            let bytes = frame.as_bytes();
            let declared = u16::from_le_bytes([bytes[0], bytes[1]]) as usize;
            assert_eq!(
                frame.wire_len(),
                declared,
                "wire_len != declared size field"
            );
            drained += frame.wire_len();
            assert!(drained <= fed, "produced {drained} bytes from {fed} fed");
        }
    }

    assert!(parser.buffered() <= fed.saturating_sub(drained));

    if first_error.is_some() {
        for _ in 0..4 {
            assert!(
                parser.next_frame().is_err(),
                "poisoned parser yielded a frame"
            );
            assert!(
                parser.feed(&[0u8; 5]).is_err(),
                "poisoned parser accepted a feed"
            );
        }
        parser.clear();
        assert!(parser.is_empty(), "clear() left bytes buffered");
        // clear() deliberately does NOT un-poison: FrameParser::clear documents
        // this, mirroring ITZRecvBuffer::ClearBuffer. A poisoned parser has no
        // recovery path -- a fresh one is the only way forward.
        assert!(
            parser.failure().is_some(),
            "clear() unexpectedly un-poisoned"
        );
        assert!(
            parser.feed(&[5, 0, 0x34, 0x12, 0x99]).is_err(),
            "cleared parser resumed"
        );

        let mut fresh = gz_net::FrameParser::new();
        assert!(fresh.is_empty() && fresh.failure().is_none());
        assert!(fresh.feed(&[5, 0, 0x34, 0x12, 0x99]).is_ok());
        assert!(fresh.next_frame().expect("no error").is_some());
    }
}

// ---------------------------------------------------------------- cipher

/// Mirrors the `cipher_decrypt` fuzz target's invariants.
fn check_cipher(data: &[u8]) {
    let (src, sel) = if data.len() >= 4 {
        (&data[4..], [data[0], data[1], data[2], 0])
    } else {
        (&[][..], [0, 0, 0, 0])
    };
    let (key1, key2) = (sel[0], sel[1]);

    let mut cipher = gz_cipher::Cipher::session_cipher(key1, key2);
    let dest_len = if src.len() >= 6 {
        (src[2] as usize % 16).min(src.len() + 8)
    } else {
        src.len()
    };

    const CANARY: u8 = 0xA5;
    // The canary tail sits OUTSIDE the slice handed to decode. Handing decode
    // the whole vector would let it legally fill dest_len+16 bytes and trip the
    // canary on correct behaviour.
    let mut dest = vec![CANARY; dest_len + 16];

    let declared_from = |s: &[u8]| -> usize {
        match s.get(..2) {
            Some(h) => u16::from_le_bytes([h[0], h[1]]) as usize,
            None => 0,
        }
    };

    let before1 = cipher.key1();
    let before2 = cipher.key2();
    let result = cipher.decode(src, &mut dest[..dest_len]);

    assert_eq!(
        cipher.key1(),
        before1.wrapping_add(1),
        "key1 did not advance"
    );
    assert_eq!(
        cipher.key2(),
        before2.wrapping_add(1),
        "key2 did not advance"
    );

    for (i, b) in dest[dest_len..].iter().enumerate() {
        assert_eq!(*b, CANARY, "decode wrote past dest end at +{i}");
    }

    if let Err(gz_cipher::CipherError::DestTooSmall { declared, .. }) = result {
        assert!(declared as usize > dest_len);
        assert_eq!(declared as usize, declared_from(src));
    }

    let mut enc = gz_cipher::Cipher::session_cipher(key1, key2);
    if let Ok(bytes) = enc.encode(src) {
        assert_eq!(
            bytes.len(),
            declared_from(src),
            "encode length != declared size"
        );
        if bytes.len() > 3 {
            let mut dec = gz_cipher::Cipher::session_cipher(key1, key2);
            let mut out = vec![0u8; bytes.len()];
            let ok = dec
                .decode(&bytes, &mut out)
                .expect("decode of own encode failed");
            // decode yields the PLAINTEXT: compare against src, not bytes.
            assert!(ok, "integrity check failed on our own encode output");
            assert_eq!(out.len(), bytes.len());
            assert_eq!(&out[..2], &bytes[..2]);
            for i in 3..bytes.len() {
                assert_eq!(out[i], src[i], "round-trip mismatch at {i}");
            }
        }
    }
}

// ---------------------------------------------------------------- composition

/// Mirrors the `frame_then_decipher` fuzz target: the real receive seam.
fn check_frame_then_decipher(data: &[u8]) {
    use gz_net::{COMPRESSION_FLAG, Frame, FrameParser, MAX_FRAME_SIZE};

    if data.len() < 4 {
        return;
    }
    let key1 = data[0];
    let key2 = data[1];
    let cmd = u16::from_le_bytes([data[2], data[3]]);
    let payload_src = &data[4..];

    let want_compress = payload_src.first().copied().unwrap_or(0) & 1 == 1;
    let raw = if payload_src.len() > 1 {
        &payload_src[1..]
    } else {
        &[][..]
    };
    let raw = &raw[..raw.len().min(MAX_FRAME_SIZE)];

    let mut payload = raw.to_vec();
    let mut compressed = false;
    if want_compress && !raw.is_empty() {
        let bound = raw.len() + raw.len() / 16 + 1 + 5;
        let mut buf = vec![0u8; bound];
        if let Ok(n) = gz_lzf::compress(raw, &mut buf) {
            buf.truncate(n);
            if buf.len() < raw.len() {
                payload = buf;
                compressed = true;
            }
        }
    }

    // Mask the input's flag bit first: the fuzzer's `cmd` may already carry
    // COMPRESSION_FLAG over a plain payload, which is a valid hostile packet
    // but not one this harness produced.
    let cmd_wire = (cmd & !gz_net::COMPRESSION_FLAG)
        | if compressed {
            gz_net::COMPRESSION_FLAG
        } else {
            0
        };
    let hostile_flag = (cmd & gz_net::COMPRESSION_FLAG) != 0 && !compressed;
    let frame = match Frame::new(cmd_wire, &payload) {
        Ok(f) => f,
        Err(_) => return,
    };

    assert_eq!(frame.compressed(), compressed);
    assert_eq!(frame.cmd_id(), cmd & !COMPRESSION_FLAG);

    let mut enc = gz_cipher::Cipher::session_cipher(key1, key2);
    let wire = match enc.encode(frame.as_bytes()) {
        Ok(w) => w,
        Err(_) => return,
    };

    let mut parser = FrameParser::new();
    if parser.feed(&wire).is_err() {
        return;
    }
    let got = match parser.next_frame() {
        Ok(Some(f)) => f,
        _ => return,
    };

    let mut dec = gz_cipher::Cipher::session_cipher(key1, key2);
    let mut plain = vec![0u8; got.wire_len()];
    if dec.decode(got.as_bytes(), &mut plain).is_err() {
        return;
    }

    let original = frame.as_bytes();
    assert_eq!(plain.len(), original.len());
    assert_eq!(
        &plain[..2],
        &original[..2],
        "size field not preserved end to end"
    );
    for i in 3..original.len() {
        assert_eq!(plain[i], original[i], "end-to-end mismatch at {i}");
    }

    // Decipher the DECIPHERED bytes; `got` still holds the enciphered frame.
    let plain_cmd = u16::from_le_bytes([plain[3], plain[4]]);
    if plain_cmd & COMPRESSION_FLAG != 0 {
        let mut out = vec![0u8; MAX_FRAME_SIZE];
        match gz_lzf::decompress(&plain[gz_net::HEADER_SIZE..], &mut out) {
            Ok(n) => assert!(n <= MAX_FRAME_SIZE),
            // Ours must decode; a hostile flag over a plain payload may not.
            Err(e) => assert!(
                hostile_flag,
                "our own compressed payload failed to decompress: {e}"
            ),
        }
    }
}

// ---------------------------------------------------------------- tests

/// 20k cases per harness. Sizes span empty, tiny, boundary-ish and large.
/// `MAX_FRAME_SIZE` is ~65k, so a few big cases keep the cost honest without
/// making the test slow.
const CASES: usize = 20_000;

fn corpus_len(rng: &mut Rng) -> usize {
    match rng.below(100) {
        0..=19 => rng.below(8),     // tiny / empty
        20..=79 => rng.below(256),  // typical frame
        80..=94 => rng.below(2048), // large
        _ => rng.below(70_000),     // near the wire limit
    }
}

#[test]
fn frame_parser_invariants_hold_over_corpus() {
    let mut rng = Rng::new(0x9E37_79B9_7F4A_7C15);
    let mut buf = vec![0u8; 70_000];
    for case in 0..CASES {
        let len = corpus_len(&mut rng);
        rng.fill(&mut buf[..len]);
        if let Err(e) = std::panic::catch_unwind(|| check_frame_parser(&buf[..len])) {
            panic!("frame_parser invariant failed on case {case}: {e:?}");
        }
    }
}

#[test]
fn cipher_invariants_hold_over_corpus() {
    let mut rng = Rng::new(0xD1B5_4A32_D192_ED03);
    let mut buf = vec![0u8; 4096];
    for case in 0..CASES {
        let len = corpus_len(&mut rng).min(4096);
        rng.fill(&mut buf[..len]);
        if let Err(e) = std::panic::catch_unwind(|| check_cipher(&buf[..len])) {
            panic!("cipher invariant failed on case {case}: {e:?}");
        }
    }
}

#[test]
fn frame_then_decipher_invariants_hold_over_corpus() {
    let mut rng = Rng::new(0x5DEE_CE66_D1B4_9A32);
    let mut buf = vec![0u8; 8192];
    for case in 0..CASES {
        let len = corpus_len(&mut rng).min(8192);
        rng.fill(&mut buf[..len]);
        if let Err(e) = std::panic::catch_unwind(|| check_frame_then_decipher(&buf[..len])) {
            panic!("frame_then_decipher invariant failed on case {case}: {e:?}");
        }
    }
}

/// Well-formed frames must survive a full frame->encipher->parse->decipher
/// cycle byte for byte. This is the regression guard for the wire format, and
/// it is deterministic - no randomness involved.
#[test]
fn well_formed_frames_round_trip_on_the_wire() {
    use gz_net::{COMPRESSION_FLAG, Frame, FrameParser, MAX_FRAME_SIZE};

    for size in [
        0usize,
        1,
        2,
        3,
        4,
        5,
        63,
        64,
        255,
        256,
        1023,
        4096,
        MAX_FRAME_SIZE / 2,
    ] {
        for compress_it in [false, true] {
            let raw: Vec<u8> = (0..size).map(|i| (i * 7 + 13) as u8).collect();

            let (payload, compressed) = if compress_it && !raw.is_empty() {
                let bound = raw.len() + raw.len() / 16 + 1 + 5;
                let mut buf = vec![0u8; bound];
                match gz_lzf::compress(&raw, &mut buf) {
                    Ok(n) if n < raw.len() => {
                        buf.truncate(n);
                        (buf, true)
                    }
                    _ => (raw.clone(), false),
                }
            } else {
                (raw.clone(), false)
            };

            let cmd_wire = 0x0100u16 | if compressed { COMPRESSION_FLAG } else { 0 };
            let frame = Frame::new(cmd_wire, &payload).expect("frame should fit");
            assert_eq!(frame.compressed(), compressed);

            let mut enc = gz_cipher::Cipher::session_cipher(0x5A, 0xA5);
            let wire = enc.encode(frame.as_bytes()).expect("encode");

            let mut parser = FrameParser::new();
            parser.feed(&wire).expect("feed");
            let got = parser.next_frame().expect("no error").expect("a frame");

            let mut dec = gz_cipher::Cipher::session_cipher(0x5A, 0xA5);
            let mut plain = vec![0u8; got.wire_len()];
            assert!(dec.decode(got.as_bytes(), &mut plain).expect("decode"));

            let original = frame.as_bytes();
            assert_eq!(
                plain.len(),
                original.len(),
                "size={size} compress={compress_it}"
            );
            assert_eq!(&plain[..2], &original[..2]);
            for i in 3..original.len() {
                assert_eq!(plain[i], original[i], "size={size} i={i}");
            }

            if frame.compressed() {
                // Decipher first: `got` still holds the enciphered frame, so its
                // payload is ciphertext and is not a valid LZF stream.
                let plain_cmd = u16::from_le_bytes([plain[3], plain[4]]);
                assert!(
                    plain_cmd & COMPRESSION_FLAG != 0,
                    "compression flag lost end to end"
                );
                let mut out = vec![0u8; MAX_FRAME_SIZE];
                let n = gz_lzf::decompress(&plain[gz_net::HEADER_SIZE..], &mut out)
                    .expect("decompress of our own compressed payload");
                assert_eq!(&out[..n], &raw[..], "payload did not survive compression");
            }
        }
    }
}

/// The corpus files under `fuzz/corpus/` are hashed into the run so that adding
/// or editing a seed is a visible change in behaviour, and so a corrupt or
/// missing seed directory is caught immediately rather than silently skipping.
#[test]
fn committed_seeds_are_present_and_readable() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fuzz/corpus")
        .canonicalize()
        .expect("fuzz/corpus should exist");

    let mut hasher = DefaultHasher::new();
    let mut count = 0usize;
    let mut targets = 0usize;
    for entry in std::fs::read_dir(&root).expect("corpus readable") {
        let dir = entry.expect("dir entry").path();
        if !dir.is_dir() {
            continue;
        }
        targets += 1;
        dir.file_name().unwrap().to_string_lossy().hash(&mut hasher);
        for seed in std::fs::read_dir(&dir).expect("seed dir readable") {
            let bytes = std::fs::read(seed.expect("seed entry").path()).expect("seed readable");
            bytes.len().hash(&mut hasher);
            count += 1;
        }
    }
    assert_eq!(
        targets, 3,
        "expected one corpus dir per fuzz target, found {targets}"
    );
    assert!(
        count >= 20,
        "expected a meaningful seed corpus, found {count} files"
    );
    eprintln!(
        "{targets} targets, {count} seeds -> {:016x}",
        hasher.finish()
    );
}
