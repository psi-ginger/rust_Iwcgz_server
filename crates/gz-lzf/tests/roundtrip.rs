//! Integration tests for gz-lzf: round-trip, edge cases, and known vectors.

use gz_lzf::{compress, decompress, LzfError};

// ============================================================
// Round-trip tests
// ============================================================

#[test]
fn round_trip_empty() {
    // Empty input is an error for compress
    let input: &[u8] = b"";
    let mut output = [0u8; 100];
    assert_eq!(compress(input, &mut output), Err(LzfError::Empty));
}

#[test]
fn round_trip_single_byte() {
    let input = b"X";
    let mut compressed = vec![0u8; 100];
    let n = compress(input, &mut compressed).unwrap();
    compressed.truncate(n);

    let mut decompressed = vec![0u8; 1];
    let m = decompress(&compressed, &mut decompressed).unwrap();
    decompressed.truncate(m);

    assert_eq!(decompressed, input);
}

#[test]
fn round_trip_two_bytes() {
    let input = b"Hi";
    let mut compressed = vec![0u8; 100];
    let n = compress(input, &mut compressed).unwrap();
    compressed.truncate(n);

    let mut decompressed = vec![0u8; 2];
    let m = decompress(&compressed, &mut decompressed).unwrap();
    decompressed.truncate(m);

    assert_eq!(decompressed, input);
}

#[test]
fn round_trip_three_bytes() {
    let input = b"abc";
    let mut compressed = vec![0u8; 100];
    let n = compress(input, &mut compressed).unwrap();
    compressed.truncate(n);

    let mut decompressed = vec![0u8; 3];
    let m = decompress(&compressed, &mut decompressed).unwrap();
    decompressed.truncate(m);

    assert_eq!(decompressed, input);
}

#[test]
fn round_trip_short_text() {
    let input = b"Hello, World!";
    let mut compressed = vec![0u8; input.len() * 2];
    let n = compress(input, &mut compressed).unwrap();
    compressed.truncate(n);

    let mut decompressed = vec![0u8; input.len()];
    let m = decompress(&compressed, &mut decompressed).unwrap();
    decompressed.truncate(m);

    assert_eq!(decompressed, input);
}

#[test]
fn round_trip_repetitive() {
    let input = vec![b'A'; 1000];
    let mut compressed = vec![0u8; input.len() * 2];
    let n = compress(&input, &mut compressed).unwrap();
    compressed.truncate(n);

    let mut decompressed = vec![0u8; input.len()];
    let m = decompress(&compressed, &mut decompressed).unwrap();
    decompressed.truncate(m);

    assert_eq!(decompressed, input);
    assert!(n < input.len() / 10, "repetitive data should compress well");
}

#[test]
fn round_trip_all_zeros() {
    let input = vec![0u8; 500];
    let mut compressed = vec![0u8; input.len() * 2];
    let n = compress(&input, &mut compressed).unwrap();
    compressed.truncate(n);

    let mut decompressed = vec![0u8; input.len()];
    let m = decompress(&compressed, &mut decompressed).unwrap();
    decompressed.truncate(m);

    assert_eq!(decompressed, input);
}

#[test]
fn round_trip_all_same_nonzero() {
    let input = vec![0xFFu8; 300];
    let mut compressed = vec![0u8; input.len() * 2];
    let n = compress(&input, &mut compressed).unwrap();
    compressed.truncate(n);

    let mut decompressed = vec![0u8; input.len()];
    let m = decompress(&compressed, &mut decompressed).unwrap();
    decompressed.truncate(m);

    assert_eq!(decompressed, input);
}

#[test]
fn round_trip_text_with_repeated_patterns() {
    let input = b"the quick brown fox jumps over the lazy dog. \
                  the quick brown fox jumps over the lazy dog. \
                  the quick brown fox jumps over the lazy dog.";
    let mut compressed = vec![0u8; input.len() * 2];
    let n = compress(input, &mut compressed).unwrap();
    compressed.truncate(n);

    let mut decompressed = vec![0u8; input.len()];
    let m = decompress(&compressed, &mut decompressed).unwrap();
    decompressed.truncate(m);

    assert_eq!(decompressed, input);
}

#[test]
fn round_trip_large_data() {
    let input: Vec<u8> = (0..10000).map(|i| (i % 251) as u8).collect();
    let mut compressed = vec![0u8; input.len() * 2];
    let n = compress(&input, &mut compressed).unwrap();
    compressed.truncate(n);

    let mut decompressed = vec![0u8; input.len()];
    let m = decompress(&compressed, &mut decompressed).unwrap();
    decompressed.truncate(m);

    assert_eq!(decompressed, input);
}

#[test]
fn round_trip_binary_data() {
    let input: Vec<u8> = (0..=255u8).flat_map(|i| vec![i; 10]).collect();
    let mut compressed = vec![0u8; input.len() * 2];
    let n = compress(&input, &mut compressed).unwrap();
    compressed.truncate(n);

    let mut decompressed = vec![0u8; input.len()];
    let m = decompress(&compressed, &mut decompressed).unwrap();
    decompressed.truncate(m);

    assert_eq!(decompressed, input);
}

#[test]
fn round_trip_max_literal_run() {
    // 32 bytes of non-repeating data to trigger MAX_LIT boundary
    let input: Vec<u8> = (0..32).map(|i| (i * 37 + 11) as u8).collect();
    let mut compressed = vec![0u8; input.len() * 2];
    let n = compress(&input, &mut compressed).unwrap();
    compressed.truncate(n);

    let mut decompressed = vec![0u8; input.len()];
    let m = decompress(&compressed, &mut decompressed).unwrap();
    decompressed.truncate(m);

    assert_eq!(decompressed, input);
}

#[test]
fn round_trip_overlapping_backref() {
    // Data that produces overlapping back-references (run-length encoding)
    let input = vec![b'Z'; 200];
    let mut compressed = vec![0u8; input.len() * 2];
    let n = compress(&input, &mut compressed).unwrap();
    compressed.truncate(n);

    let mut decompressed = vec![0u8; input.len()];
    let m = decompress(&compressed, &mut decompressed).unwrap();
    decompressed.truncate(m);

    assert_eq!(decompressed, input);
}

#[test]
fn round_trip_long_repeated_pattern() {
    // Pattern that repeats with a period > 1
    let pattern = b"ABCDEFGH";
    let input: Vec<u8> = pattern.iter().cycle().take(800).copied().collect();
    let mut compressed = vec![0u8; input.len() * 2];
    let n = compress(&input, &mut compressed).unwrap();
    compressed.truncate(n);

    let mut decompressed = vec![0u8; input.len()];
    let m = decompress(&compressed, &mut decompressed).unwrap();
    decompressed.truncate(m);

    assert_eq!(decompressed, input);
}

#[test]
fn round_trip_mixed_content() {
    // Mix of repetitive and non-repetitive data
    let mut input = Vec::new();
    for i in 0..100 {
        if i % 3 == 0 {
            input.extend(vec![b'X'; 50]);
        } else {
            input.extend((0..50).map(|j| (i * 50 + j) as u8));
        }
    }
    let mut compressed = vec![0u8; input.len() * 2];
    let n = compress(&input, &mut compressed).unwrap();
    compressed.truncate(n);

    let mut decompressed = vec![0u8; input.len()];
    let m = decompress(&compressed, &mut decompressed).unwrap();
    decompressed.truncate(m);

    assert_eq!(decompressed, input);
}

// ============================================================
// Error handling tests
// ============================================================

#[test]
fn compress_output_too_small() {
    let input = b"Hello, World! This is a test.";
    let mut output = [0u8; 3];
    assert_eq!(compress(input, &mut output), Err(LzfError::OutputTooSmall));
}

#[test]
fn compress_empty_input() {
    let input: &[u8] = b"";
    let mut output = [0u8; 100];
    assert_eq!(compress(input, &mut output), Err(LzfError::Empty));
}

#[test]
fn compress_empty_output() {
    let input = b"hello";
    let output: &mut [u8] = &mut [];
    assert_eq!(compress(input, output), Err(LzfError::Empty));
}

#[test]
fn decompress_output_too_small() {
    let input = b"Hello, World!";
    let mut compressed = vec![0u8; input.len() * 2];
    let n = compress(input, &mut compressed).unwrap();
    compressed.truncate(n);

    let mut output = [0u8; 3];
    assert_eq!(decompress(&compressed, &mut output), Err(LzfError::OutputTooSmall));
}

#[test]
fn decompress_invalid_truncated_literal() {
    // Literal run claiming 10 bytes but only 3 available
    let input = [0x09, b'a', b'b', b'c'];
    let mut output = [0u8; 100];
    assert_eq!(decompress(&input, &mut output), Err(LzfError::InvalidInput));
}

#[test]
fn decompress_invalid_truncated_backref() {
    // Back reference with no offset byte
    let input = [0xe0];
    let mut output = [0u8; 100];
    assert_eq!(decompress(&input, &mut output), Err(LzfError::InvalidInput));
}

#[test]
fn decompress_invalid_bad_offset() {
    // Back reference with offset pointing before output start
    let input = [0xff, 0xff];
    let mut output = [0u8; 1000];
    assert_eq!(decompress(&input, &mut output), Err(LzfError::InvalidInput));
}

#[test]
fn decompress_invalid_truncated_after_len7() {
    // Back reference with len=7, has the len byte but not the offset byte
    let input = [0xe0, 0x05];
    let mut output = [0u8; 100];
    assert_eq!(decompress(&input, &mut output), Err(LzfError::InvalidInput));
}

#[test]
fn decompress_empty_input() {
    let input: &[u8] = b"";
    let mut output = [0u8; 100];
    assert_eq!(decompress(input, &mut output), Ok(0));
}

// ============================================================
// Known vector tests (generated by the Rust implementation)
// ============================================================

#[test]
fn known_vector_hello() {
    // "Hello, World!" compressed
    let input = b"Hello, World!";
    let expected_compressed: &[u8] = &[
        0x0C, b'H', b'e', b'l', b'l', b'o', b',', b' ', b'W', b'o', b'r', b'l', b'd', b'!',
    ];
    let mut compressed = vec![0u8; 100];
    let n = compress(input, &mut compressed).unwrap();
    assert_eq!(&compressed[..n], expected_compressed);

    let mut decompressed = vec![0u8; input.len()];
    let m = decompress(expected_compressed, &mut decompressed).unwrap();
    decompressed.truncate(m);
    assert_eq!(decompressed, input);
}

#[test]
fn known_vector_repetitive() {
    // "AAAAAAAAAA" (10 A's) compressed
    let input = b"AAAAAAAAAA";
    let mut compressed = vec![0u8; 100];
    let n = compress(input, &mut compressed).unwrap();
    compressed.truncate(n);

    let mut decompressed = vec![0u8; input.len()];
    let m = decompress(&compressed, &mut decompressed).unwrap();
    decompressed.truncate(m);
    assert_eq!(decompressed, input);
}

#[test]
fn known_vector_abab() {
    // "ABABABABAB" compressed
    let input = b"ABABABABAB";
    let mut compressed = vec![0u8; 100];
    let n = compress(input, &mut compressed).unwrap();
    compressed.truncate(n);

    let mut decompressed = vec![0u8; input.len()];
    let m = decompress(&compressed, &mut decompressed).unwrap();
    decompressed.truncate(m);
    assert_eq!(decompressed, input);
}

// ============================================================
// Property-based style tests
// ============================================================

#[test]
fn round_trip_various_sizes() {
    for size in [1, 2, 3, 4, 5, 7, 8, 15, 16, 17, 31, 32, 33, 63, 64, 65, 127, 128, 129, 255, 256, 257, 511, 512, 513, 1023, 1024, 1025] {
        let input: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        let mut compressed = vec![0u8; size * 2 + 100];
        let n = compress(&input, &mut compressed).unwrap();
        compressed.truncate(n);

        let mut decompressed = vec![0u8; size];
        let m = decompress(&compressed, &mut decompressed).unwrap();
        decompressed.truncate(m);

        assert_eq!(decompressed, input, "round-trip failed for size {}", size);
    }
}

#[test]
fn round_trip_various_patterns() {
    // All zeros
    for size in [1, 10, 100, 1000] {
        let input = vec![0u8; size];
        let mut compressed = vec![0u8; size * 2 + 100];
        let n = compress(&input, &mut compressed).unwrap();
        compressed.truncate(n);

        let mut decompressed = vec![0u8; size];
        let m = decompress(&compressed, &mut decompressed).unwrap();
        decompressed.truncate(m);

        assert_eq!(decompressed, input, "zeros round-trip failed for size {}", size);
    }

    // All same byte
    for size in [1, 10, 100, 1000] {
        let input = vec![b'Q'; size];
        let mut compressed = vec![0u8; size * 2 + 100];
        let n = compress(&input, &mut compressed).unwrap();
        compressed.truncate(n);

        let mut decompressed = vec![0u8; size];
        let m = decompress(&compressed, &mut decompressed).unwrap();
        decompressed.truncate(m);

        assert_eq!(decompressed, input, "same-byte round-trip failed for size {}", size);
    }

    // Alternating pattern
    for size in [1, 10, 100, 1000] {
        let input: Vec<u8> = (0..size).map(|i| if i % 2 == 0 { b'A' } else { b'B' }).collect();
        let mut compressed = vec![0u8; size * 2 + 100];
        let n = compress(&input, &mut compressed).unwrap();
        compressed.truncate(n);

        let mut decompressed = vec![0u8; size];
        let m = decompress(&compressed, &mut decompressed).unwrap();
        decompressed.truncate(m);

        assert_eq!(decompressed, input, "alternating round-trip failed for size {}", size);
    }
}

#[test]
fn compress_never_expands_beyond_input_plus_overhead() {
    // LZF can expand incompressible data, but only by a small amount
    let input: Vec<u8> = (0..1000).map(|i| (i * 31 + 7) as u8).collect();
    let mut compressed = vec![0u8; input.len() * 2];
    let n = compress(&input, &mut compressed).unwrap();

    // Compressed should not exceed input + 104% (per lzf.h docs)
    assert!(n <= input.len() + input.len() / 16 + 64, "compressed too large: {} vs input {}", n, input.len());
}

#[test]
fn decompress_returns_correct_length() {
    let input = b"Test data for length verification";
    let mut compressed = vec![0u8; input.len() * 2];
    let n = compress(input, &mut compressed).unwrap();
    compressed.truncate(n);

    let mut decompressed = vec![0u8; input.len()];
    let m = decompress(&compressed, &mut decompressed).unwrap();

    assert_eq!(m, input.len());
}
