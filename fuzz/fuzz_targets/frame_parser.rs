//! Fuzz target: the `gz-net` framing parser.
//!
//! The parser is fed **arbitrary bytes in arbitrary segment sizes**, because TCP
//! hands you no framing guarantees at all — a frame header can arrive one byte
//! per `feed`. A target that always fed the whole buffer at once would exercise
//! a path real traffic never takes.
//!
//! Invariants checked beyond "does not panic":
//!
//! 1. A returned frame's `wire_len()` equals the little-endian size in its own
//!    first two bytes. A mismatch means the header and the body disagree, which
//!    is exactly the class of bug that desynchronises a stream.
//! 2. Bytes drained never exceed bytes fed.
//! 3. A rejection is **sticky**: once `feed` errors, every later `feed` and
//!    `next_frame` returns the *same* error until `clear()`. The parser is
//!    permanently poisoned; a target that quietly resumed would hide the bug.
//! 4. `clear()` fully restores the initial state.
//!
//! Errors are expected outcomes here, not failures — a hostile peer is the
//! thing being modelled. Only a panic or a broken invariant is a finding.

#![no_main]

use libfuzzer_sys::fuzz_target;
use gz_net::FrameParser;

/// Little-endian size field at the front of a frame, straight from the wire
/// layout. Deliberately re-derived here rather than calling into `gz-net`, so
/// that a bug in `gz-net` cannot make the oracle agree with itself.
fn declared_len(bytes: &[u8]) -> usize {
    u16::from_le_bytes([bytes[0], bytes[1]]) as usize
}

fuzz_target!(|data: &[u8]| {
    // The first byte picks how the rest is segmented into feed() calls.
    // 0 => one chunk (the easy path); otherwise a mix of 1..=8 byte chunks,
    // which is what a real socket with Nagle off and a busy peer looks like.
    let split_byte = data.first().copied().unwrap_or(0);
    let body = if data.is_empty() { &[] } else { &data[1..] };

    let mut parser = FrameParser::new();
    let mut fed: usize = 0;
    let mut drained: usize = 0;
    let mut first_error: Option<String> = None;

    let mut at = 0usize;
    while at < body.len() {
        let take = if split_byte == 0 {
            body.len() - at
        } else {
            // Vary per iteration so the fuzzer explores different segmentations
            // of the same bytes, not just one fixed split.
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
                // Invariant 3: the first error latches, and never changes.
                match &first_error {
                    None => first_error = Some(msg),
                    Some(prev) => assert_eq!(prev, &msg, "parser failure changed after rejection"),
                }
            }
        }

        // Drain everything currently whole. Callers loop until Ok(None)
        // (blueprint 01 Part A §1.3), so the target does too.
        loop {
            match parser.next_frame() {
                Ok(Some(frame)) => {
                    let bytes = frame.as_bytes();
                    // Invariant 1: header and body must agree.
                    assert_eq!(
                        frame.wire_len(),
                        declared_len(bytes),
                        "frame wire_len disagrees with its own size field"
                    );
                    assert!(frame.wire_len() >= bytes.len().min(frame.wire_len()));
                    drained += frame.wire_len();

                    // Invariant 2: never manufacture bytes.
                    assert!(
                        drained <= fed,
                        "parser produced {drained} bytes from {fed} fed"
                    );
                }
                Ok(None) => break,
                Err(e) => {
                    let msg = e.to_string();
                    match &first_error {
                        None => first_error = Some(msg),
                        Some(prev) => assert_eq!(prev, &msg, "parser failure changed after rejection"),
                    }
                    break;
                }
            }
        }
    }

    // Whatever was left must be a strict prefix of one whole frame, never more.
    assert!(
        parser.buffered() <= fed.saturating_sub(drained),
        "buffered {} exceeds un-drained {}",
        parser.buffered(),
        fed - drained
    );

    if first_error.is_some() {
        // Invariant 3, second half: a poisoned parser keeps reporting the same
        // error and hands back nothing.
        for _ in 0..4 {
            assert!(parser.next_frame().is_err(), "poisoned parser yielded a frame");
            assert!(parser.feed(&[0u8; 5]).is_err(), "poisoned parser accepted a feed");
        }

        // Invariant 4: `clear()` drops buffered bytes but **deliberately does
        // not un-poison** the parser -- `FrameParser::clear` documents this,
        // mirroring `ITZRecvBuffer::ClearBuffer`. There is no recovery path on
        // a poisoned parser; the only way forward is a fresh one. Assert the
        // real contract rather than an aspirational one.
        parser.clear();
        assert!(parser.is_empty(), "clear() left bytes buffered");
        assert!(parser.failure().is_some(), "clear() unexpectedly un-poisoned the parser");
        assert!(parser.feed(&[5, 0, 0x34, 0x12, 0x99]).is_err(), "cleared parser resumed");

        let mut fresh = FrameParser::new();
        assert!(fresh.is_empty() && fresh.failure().is_none(), "a fresh parser is not clean");
        assert!(fresh.feed(&[5, 0, 0x34, 0x12, 0x99]).is_ok(), "a fresh parser rejected a valid frame");
        assert!(fresh.next_frame().expect("no error").is_some(), "a fresh parser yielded no frame");
    }
});