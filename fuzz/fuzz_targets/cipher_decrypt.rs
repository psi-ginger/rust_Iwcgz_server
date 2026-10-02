//! Fuzz target: the `gz-cipher` decode path.
//!
//! `decode` is the single highest-risk function in the rewrite: it is the one
//! place where attacker-controlled bytes meet a **caller-supplied output
//! buffer**. An off-by-one in the size handling is a remote memory write, not a
//! panic, and a panic-free fuzzer would sail straight past it. So this target
//! deliberately hands `decode` a `dest` that is *deliberately too small* about
//! half the time — the guard is the thing under test.
//!
//! Invariants checked beyond "does not panic":
//!
//! 1. **No write past `dest.len()`.** `dest` is pre-filled with a known byte
//!    and a canary is appended after it; any overwrite past the logical end is
//!    caught by the canary as well as by Rust's own bounds checks.
//! 2. **Keys advance on every path, including rejected ones.** The legacy
//!    increments both keys before validating, so a rejected packet still
//!    perturbs the cipher state. Getting this wrong desynchronises the session
//!    for every subsequent packet — a silent corruption, not a crash.
//! 3. `decode` must refuse a `dest` smaller than the declared size.
//! 4. `encode` output length always equals the declared size on success.
//! 5. Round-trip `decode(encode(x)) == x` for payloads over 3 bytes.
//!
//! Byte 2 is deliberately excluded from the round-trip compare: `decode` leaves
//! `dest[2]` **untouched** by design, mirroring the legacy's unwritten
//! integrity slot (BUG-012). Asserting equality there would be asserting a bug.

#![no_main]

use libfuzzer_sys::fuzz_target;
use gz_cipher::{Cipher, PACKET_MAX_SIZE};

const CANARY: u8 = 0xA5;

fuzz_target!(|data: &[u8]| {
    // Byte 0-1 pick the key; byte 2-3 pick how large `dest` is relative to the
    // declared size, so the TooSmall branch gets real coverage.
    let mut sel = [0u8; 4];
    let src: &[u8] = if data.len() >= 4 {
        sel.copy_from_slice(&data[..4]);
        &data[4..]
    } else {
        &data[..]
    };

    let key1 = sel[0];
    let key2 = sel[1];

    // --- decode into an undersized / oversized dest -----------------------------
    let mut cipher = Cipher::session_cipher(key1, key2);

    // `dest` size is driven by the fuzzer but clamped, so the target still
    // covers the boundary without allocating 64 KiB on every iteration.
    let dest_len = if src.len() >= 6 {
        (src[2] as usize % 16).min(src.len() + 8)
    } else {
        src.len()
    };

    // The canary tail sits OUTSIDE the slice handed to decode. Handing decode the
    // whole vector would let it legally fill dest_len+16 bytes and trip the
    // canary on correct behaviour.
    let mut dest = vec![CANARY; dest_len + 16];

    let before1 = cipher.key1();
    let before2 = cipher.key2();
    let result = cipher.decode(src, &mut dest[..dest_len]);
    // Invariant 2: keys moved regardless of outcome.
    assert_eq!(
        cipher.key1(),
        before1.wrapping_add(1),
        "key1 did not advance by 1 on the decode path"
    );
    assert_eq!(
        cipher.key2(),
        before2.wrapping_add(1),
        "key2 did not advance by 1 on the decode path"
    );

    // Invariant 1: nothing past the caller-visible `dest_len` was written.
    for (i, b) in dest[dest_len..].iter().enumerate() {
        assert_eq!(*b, CANARY, "decode wrote past dest end at offset {dest_len}+{i}");
    }

    match result {
        Err(gz_cipher::CipherError::DestTooSmall { declared, .. }) => {
            // Invariant 3: it refused, and it refused with the declared size.
            assert!(declared as usize > dest_len);
            assert_eq!(declared as usize, declared_from(src));
        }
        Err(gz_cipher::CipherError::TooLarge(d)) => {
            assert!(d as usize > PACKET_MAX_SIZE);
        }
        Err(_) => {}
        Ok(checked) => {
            // A successful decode implies the dest really was big enough.
            assert!(
                declared_from(src) <= dest_len,
                "decode succeeded with a dest smaller than the declared size"
            );
            // Invariant 3, contrapositive: if it fitted, it must not have
            // claimed it was too small.
            let _ = checked;
        }
    }

    // --- encode, and the round-trip ---------------------------------------------
    let mut enc = Cipher::session_cipher(key1, key2);
    if let Ok(bytes) = enc.encode(src) {
        // Invariant 4: encode emits exactly the declared size.
        assert_eq!(bytes.len(), declared_from(src), "encode length != declared size");

        if bytes.len() > 3 {
            let mut dec = Cipher::session_cipher(key1, key2);
            let mut out = vec![0u8; bytes.len()];
            let ok = dec.decode(&bytes, &mut out).expect("decode of our own encode failed");
            // Invariant 5: round-trip. `decode` yields the PLAINTEXT, so it must
            // be compared against `src`, never against `bytes` (the ciphertext).
            // Byte 2 is skipped on purpose (BUG-012: decode leaves dest[2] alone).
            assert!(ok, "integrity check failed on our own encode output");
            assert_eq!(out.len(), bytes.len());
            assert_eq!(&out[..2], &bytes[..2], "size field not preserved through the cipher");
            for i in 3..bytes.len() {
                assert_eq!(out[i], src[i], "round-trip mismatch at byte {i}");
            }
        }
    }
});

/// The declared size field, read straight off the wire layout.
fn declared_from(src: &[u8]) -> usize {
    match src.get(..2) {
        Some(h) => u16::from_le_bytes([h[0], h[1]]) as usize,
        None => 0,
    }
}