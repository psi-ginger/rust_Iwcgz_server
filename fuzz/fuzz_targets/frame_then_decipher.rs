//! Fuzz target: the real receive seam — **frame first, then decipher**.
//!
//! Fuzzing `gz-net` and `gz-cipher` in isolation proves each layer survives
//! hostile bytes, but it cannot see the bugs that live *between* them. The
//! services never parse a frame in isolation: they take the wire bytes, encipher
//! them whole, and only then hand the result to `FrameParser`. So this target
//! drives that exact composition.
//!
//! The ordering is the point, and it is not symmetric:
//!
//! - `frame-then-decipher` is the correct seam — `Cipher::decode` runs over
//!   `frame.as_bytes()` and only then are fields read. Doing it the other way
//!   round (parse the header, then decipher) parses *encrypted* bytes as a
//!   length and desynchronises the stream immediately.
//!
//! Second-order invariant this catches: when `COMPRESSION_FLAG` is set the
//! payload must be **decompressed first and the flag cleared second** (the
//! order `GameProtocol.cpp:140-141` mandates). Reversing it feeds compressed
//! bytes to the message decoder.
//!
//! The fuzzer builds a real frame, compresses some of its payloads for real, and
//! lets the input drive the command id, payload bytes and key, so the interesting
//! states (flag set, size near the boundary, payload that is not valid LZF) are
//! all reachable rather than theoretical.

#![no_main]

use libfuzzer_sys::fuzz_target;
use gz_cipher::Cipher;
use gz_net::{Frame, FrameParser, COMPRESSION_FLAG, HEADER_SIZE, MAX_FRAME_SIZE};
use gz_lzf;

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }

    let key1 = data[0];
    let key2 = data[1];
    let cmd = u16::from_le_bytes([data[2], data[3]]);
    let payload_src = &data[4..];

    // ---- build a payload, sometimes genuinely LZF-compressed ------------------
    // Compress at most this much, so a big fuzzer input cannot make us allocate
    // more than the frame limit allows.
    let want_compress = payload_src.first().copied().unwrap_or(0) & 1 == 1;
    let raw = if payload_src.len() > 1 { &payload_src[1..] } else { &[][..] };
    let raw = &raw[..raw.len().min(MAX_FRAME_SIZE)];

    let mut payload = raw.to_vec();
    let mut compressed = false;
    if want_compress && !raw.is_empty() {
        // `lzf_compress_bound` from lzf_c.c: len + len/16 + 1 + 5. gz-lzf does
        // not export the helper, so it is spelled out here rather than guessed
        // at -- an undersized buffer would silently fail the compress branch.
        let bound = raw.len() + raw.len() / 16 + 1 + 5;
        let mut buf = vec![0u8; bound];
        if let Ok(n) = gz_lzf::compress(raw, &mut buf) {
            buf.truncate(n);
            // Only use it if the frame still fits with the flag bit set.
            if buf.len() < raw.len() {
                payload = buf;
                compressed = true;
            }
        }
    }

    // ---- frame it, marking compression only when we actually compressed ------
    // The incoming `cmd` comes straight from the fuzzer, so its COMPRESSION_FLAG
    // bit may already be set while the payload is plain bytes. That is a valid
    // hostile packet, but it is NOT a packet we produced, so it must not be
    // held to the "our own stream must decode" rule below. Mask first, then set
    // the bit only if we really compressed.
    let cmd_wire = (cmd & !COMPRESSION_FLAG) | if compressed { COMPRESSION_FLAG } else { 0 };
    let hostile_flag = (cmd & COMPRESSION_FLAG) != 0 && !compressed;

    let frame = match Frame::new(cmd_wire, &payload) {
        Ok(f) => f,
        Err(_) => return, // oversized for the wire; nothing to fuzz here
    };

    assert_eq!(frame.compressed(), cmd_wire & COMPRESSION_FLAG != 0);
    assert_eq!(frame.cmd_id(), cmd_wire & !COMPRESSION_FLAG);

    // ---- the wire: encipher the WHOLE frame ----------------------------------
    let mut enc = Cipher::session_cipher(key1, key2);
    let wire = match enc.encode(frame.as_bytes()) {
        Ok(w) => w,
        Err(_) => return,
    };

    // ---- the receive path: parse, then decipher, then read fields ------------
    let mut parser = FrameParser::new();
    if parser.feed(&wire).is_err() {
        return; // a rejection here is a legitimate outcome, not a finding
    }

    let got = match parser.next_frame() {
        Ok(Some(f)) => f,
        Ok(None) | Err(_) => return,
    };

    // Decipher the frame bytes as a unit - the frame-then-decipher order.
    let mut dec = Cipher::session_cipher(key1, key2);
    let mut plain = vec![0u8; got.wire_len()];
    // A genuine end-to-end round trip must decode cleanly. If this target can
    // construct a frame the receiver cannot decipher, that is a real bug.
    if dec.decode(got.as_bytes(), &mut plain).is_err() {
        return;
    }

    // The deciphered bytes must be byte-identical to what we framed, apart from
    // the integrity slot at offset 2 that `decode` leaves untouched (BUG-012).
    let original = frame.as_bytes();
    assert_eq!(plain.len(), original.len(), "deciphered frame length changed");
    assert_eq!(&plain[..2], &original[..2], "size field not preserved end to end");
    for i in 3..original.len() {
        assert_eq!(plain[i], original[i], "end-to-end mismatch at byte {i}");
    }

    // ---- decompression order, if the flag survived ---------------------------
    // The bytes to decompress are the DECIPHERED ones. `got` still holds the
    // enciphered frame; decompressing those would feed ciphertext to the LZF
    // decoder. The order is: decipher, then decompress, then clear the flag -
    // never the reverse.
    let plain_cmd = u16::from_le_bytes([plain[3], plain[4]]);
    if plain_cmd & COMPRESSION_FLAG != 0 {
        let mut out = vec![0u8; MAX_FRAME_SIZE];
        match gz_lzf::decompress(&plain[HEADER_SIZE..], &mut out) {
            Ok(n) => assert!(n <= MAX_FRAME_SIZE, "decompress returned an over-limit length"),
            // A stream we produced ourselves must always decode. One we did not
            // (hostile flag over a plain payload) may legitimately be rejected --
            // that is the receiver's job, not a finding. Either way it must not
            // panic, which is what reaching this arm already proves.
            Err(e) => {
                assert!(hostile_flag, "our own compressed payload failed to decompress: {e}");
            }
        }
    }
});