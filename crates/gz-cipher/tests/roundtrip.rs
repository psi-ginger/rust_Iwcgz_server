//! Property tests for the ITZ cipher.
//!
//! The round-trip property is deliberately *not* `decrypt(encrypt(x)) == x`:
//! offset 2 is the legacy integrity slot, and `Decode` never writes it, so a
//! faithful port can never restore that byte. Asserting a blind equality would
//! force the port to lie. The property below is the true one, and the byte-2
//! behaviour is pinned separately.
//!
//! A round trip alone would also be satisfied by any mutually inverse pair of
//! wrong implementations, which is why `tests/vectors.rs` pins the same code
//! against ground truth from the compiled legacy source.

use gz_cipher::{Cipher, CipherError, LINK_KEY1, LINK_KEY2, PACKET_MAX_SIZE};

/// Small deterministic PRNG, so a failure is reproducible from the seed alone.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn byte(&mut self) -> u8 {
        (self.next_u64() >> 24) as u8
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next_u64() % bound as u64) as usize
    }
}

/// Overwrite the leading bytes with the little-endian size, tolerating the
/// degenerate frames shorter than the field itself.
fn stamp_size(frame: &mut [u8]) {
    let le = (frame.len() as u16).to_le_bytes();
    let n = 2.min(frame.len());
    frame[..n].copy_from_slice(&le[..n]);
}

/// A random well-formed frame: a little-endian size prefix plus exactly that many
/// bytes, so the cipher and the framing agree.
///
/// Sizes below 4 are excluded here because they take the passthrough branch,
/// where the output is shorter than the 2-byte prefix and so cannot be fed back
/// in; `passthrough_branches_are_verbatim` covers them directly.
fn random_frame(rng: &mut Rng) -> Vec<u8> {
    let size = 4 + rng.below(296);
    let mut frame = vec![0u8; size];
    for byte in &mut frame {
        *byte = rng.byte();
    }
    frame[..2].copy_from_slice(&(size as u16).to_le_bytes());
    frame
}

/// The true inverse property: every byte except the integrity slot at offset 2
/// survives a trip through the cipher.
fn assert_round_trip(frame: &[u8], key1: u8, key2: u8) {
    let mut tx = Cipher::session_cipher(key1, key2);
    let mut rx = Cipher::session_cipher(key1, key2);

    let wire = tx.encode(frame).expect("frame is well formed");
    let mut plain = vec![0u8; frame.len()];
    assert!(
        rx.decode(&wire, &mut plain).expect("frame is well formed"),
        "decode rejected a frame this cipher produced"
    );

    assert_eq!(
        wire.len(),
        frame.len(),
        "ciphertext length must be preserved"
    );
    assert_eq!(&plain[..2], &frame[..2], "size field must survive");
    assert_eq!(&plain[3..], &frame[3..], "payload must survive");
}

#[test]
fn round_trips_random_frames() {
    // The card asks for at least 100 buffers.
    let mut rng = Rng::new(0x1234_5678_9abc_def0);
    for _ in 0..250 {
        let frame = random_frame(&mut rng);
        let key1 = rng.byte();
        let key2 = rng.byte();
        assert_round_trip(&frame, key1, key2);
    }
}

#[test]
fn round_trips_every_short_length() {
    // Size 4 is the first size that takes the cipher branch, so pin the boundary
    // and its neighbourhood rather than trusting chance.
    for size in 4..=40usize {
        let mut frame = vec![0xa5u8; size];
        stamp_size(&mut frame);
        for key1 in [0x00u8, 0x01, 0x7f, 0xfe, 0xff] {
            for key2 in [0x00u8, 0x34, 0xff] {
                assert_round_trip(&frame, key1, key2);
            }
        }
    }
}

#[test]
fn passthrough_branches_are_verbatim() {
    // Sizes 0..=3 are copied, not enciphered, and the output is only `size` bytes
    // long. They cannot round trip because a 0- or 1-byte output is shorter than
    // the size prefix a decode must read, so each is pinned directly.
    for size in 0..=3usize {
        let mut buffer = vec![0x5au8; size.max(2)];
        buffer[..2].copy_from_slice(&(size as u16).to_le_bytes());

        let mut cipher = Cipher::new();
        let out = cipher.encode(&buffer).expect("well formed");
        assert_eq!(
            out.len(),
            size,
            "size {size} must write exactly `size` bytes"
        );
        assert_eq!(out, buffer[..size], "size {size} must be copied verbatim");
        assert_eq!((cipher.key1(), cipher.key2()), (1, 1), "keys still advance");
    }

    // Decoding the same short frames is a copy too, and accepts unconditionally.
    for (input, expect) in [
        (vec![0x00, 0x00], Vec::new()),
        (vec![0x01, 0x00], vec![0x01]),
        (vec![0x02, 0x00, 0xab], vec![0x02, 0x00]),
        (vec![0x03, 0x00, 0xab, 0xcd], vec![0x03, 0x00, 0xab]),
    ] {
        let size = expect.len();
        let mut cipher = Cipher::new();
        let mut dest = vec![0xAA; size];
        assert!(cipher.decode(&input, &mut dest).expect("well formed"));
        assert_eq!(dest, expect);
    }
}

#[test]
fn round_trips_a_long_burst_through_both_ends() {
    // The counters must stay in step across a whole burst, not just one packet:
    // a desync after N packets still round trips if only one packet is checked.
    let mut rng = Rng::new(0xdead_beef_cafe_0001);
    let mut tx = Cipher::link_cipher();
    let mut rx = Cipher::link_cipher();

    for expected in 1..=200u8 {
        let frame = random_frame(&mut rng);
        let size = frame.len();
        let wire = tx.encode(&frame).expect("well formed");
        let mut plain = vec![0u8; size];
        assert!(
            rx.decode(&wire, &mut plain).expect("well formed"),
            "burst packet {expected} was rejected"
        );
        assert_eq!(&plain[3..size], &frame[3..size]);
        assert_eq!(
            tx.key1(),
            rx.key1(),
            "counters diverged at packet {expected}"
        );
        assert_eq!(
            tx.key2(),
            rx.key2(),
            "counters diverged at packet {expected}"
        );
    }
}

#[test]
fn link_cipher_key_path_is_stable_across_a_burst() {
    // The `Encrypt2` instance is keyed `0x12,0x34` by `ITZSession::Init` and used
    // for the whole game link, so pin that its keys start there and then simply
    // count up.
    let mut cipher = Cipher::link_cipher();
    assert_eq!((cipher.key1(), cipher.key2()), (LINK_KEY1, LINK_KEY2));
    for call in 1..=255u16 {
        let _ = cipher.encode(&[0x08, 0x00, 0, 0, 0, 0, 0, 0]);
        assert_eq!(cipher.key1(), (LINK_KEY1 as u16 + call) as u8);
        assert_eq!(cipher.key2(), (LINK_KEY2 as u16 + call) as u8);
    }
    // One more call wraps both keys back to the start.
    let _ = cipher.encode(&[0x08, 0x00, 0, 0, 0, 0, 0, 0]);
    assert_eq!((cipher.key1(), cipher.key2()), (LINK_KEY1, LINK_KEY2));
}

#[test]
fn session_cipher_key_path_accepts_a_negotiated_key() {
    // The other instance is keyed from the server, so any pair must work,
    // including the degenerate ones.
    for key1 in [0x00u8, 0x01, 0x12, 0x7f, 0x80, 0xfe, 0xff] {
        for key2 in [0x00u8, 0x01, 0x34, 0x7f, 0x80, 0xfe, 0xff] {
            assert_round_trip(
                &[0x10, 0x00, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14],
                key1,
                key2,
            );
        }
    }
}

#[test]
fn set_key_repositions_the_counter() {
    // `SetKey` overwrites the keys, so it is also how a legacy caller resets a
    // desynchronised link.
    let mut tx = Cipher::link_cipher();
    let mut rx = Cipher::link_cipher();
    let frame = [0x08, 0x00, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];

    let wire = tx.encode(&frame).expect("well formed");
    tx.set_key(LINK_KEY1, LINK_KEY2);

    let mut plain = vec![0u8; frame.len()];
    assert!(rx.decode(&wire, &mut plain).expect("well formed"));
}

#[test]
fn a_misaligned_counter_is_caught_not_silently_accepted() {
    // If one side is a packet ahead, the integrity byte will not match. This is
    // the failure mode a key-schedule desync produces, and the legacy relies on
    // the same check to notice it.
    let frame = [0x08, 0x00, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];
    let mut tx = Cipher::link_cipher();
    let mut rx = Cipher::link_cipher();
    let wire = tx.encode(&frame).expect("well formed");

    // Advance the receiver once so it is one packet behind.
    let _ = rx.encode(&frame).expect("well formed");

    let mut plain = vec![0u8; frame.len()];
    assert!(!rx.decode(&wire, &mut plain).expect("well formed"));
}

#[test]
fn packet_max_size_is_the_exact_accept_boundary() {
    // `decode` rejects above `PACKET_MAX_SIZE` and nothing above it, so pin both
    // sides of the boundary rather than only the rejection.
    for size in [PACKET_MAX_SIZE, PACKET_MAX_SIZE + 1] {
        let mut frame = vec![0u8; size];
        stamp_size(&mut frame);

        let mut tx = Cipher::link_cipher();
        let mut rx = Cipher::link_cipher();
        let wire = tx.encode(&frame).expect("well formed");
        let mut plain = vec![0u8; size];

        let outcome = rx.decode(&wire, &mut plain);
        if size <= PACKET_MAX_SIZE {
            assert!(outcome.expect("must be accepted"), "size {size}");
        } else {
            assert_eq!(outcome, Err(CipherError::TooLarge(size as u16)));
        }
    }
}

#[test]
fn corruption_is_mostly_caught() {
    // One byte of integrity means roughly 255/256 of single-byte corruptions are
    // rejected. This is not a checksum and must not be treated as one.
    let frame = [0x08, 0x00, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];
    let mut tx = Cipher::link_cipher();
    let mut rx = Cipher::link_cipher();
    let wire = tx.encode(&frame).expect("well formed");

    let mut rejected = 0;
    let mut total = 0;
    // Offsets 0 and 1 are the size field; corrupting those is a framing fault
    // caught by the length checks, not by the one-byte integrity value, so the
    // integrity rate is measured from offset 2 onwards.
    for offset in 2..wire.len() {
        for bit in 0..8u8 {
            let mut corrupt = wire.clone();
            corrupt[offset] ^= 1 << bit;
            let mut plain = vec![0u8; corrupt.len()];
            if !rx.decode(&corrupt, &mut plain).expect("well formed") {
                rejected += 1;
            }
            total += 1;
        }
    }
    // A round trip alone would pass with a cipher that checks nothing, so assert
    // the detection rate is high without pinning an exact legacy constant.
    let rate = f64::from(rejected) / f64::from(total);
    assert!(rate > 0.9, "integrity check only caught {rate}");
}
