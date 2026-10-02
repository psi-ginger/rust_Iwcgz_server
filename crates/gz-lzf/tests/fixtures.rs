//! Fixture-based tests for gz-lzf.
//!
//! Loads .bin/.lzf pairs from the fixtures/ directory and verifies:
//! 1. Decompressing the .lzf file produces the .bin file
//! 2. Compressing the .bin file produces the .lzf file (wire compatibility)

use gz_lzf::{compress, decompress, LzfError};
use std::fs;
use std::path::Path;

fn load_fixtures() -> Vec<(String, Vec<u8>, Vec<u8>)> {
    let fixtures_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let mut fixtures = Vec::new();

    if !fixtures_dir.exists() {
        return fixtures;
    }

    let mut entries: Vec<_> = fs::read_dir(&fixtures_dir)
        .expect("failed to read fixtures directory")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "bin").unwrap_or(false))
        .collect();

    entries.sort();

    for bin_path in entries {
        let name = bin_path.file_stem().unwrap().to_str().unwrap().to_string();
        let lzf_path = bin_path.with_extension("lzf");

        let bin_data = fs::read(&bin_path).expect("failed to read .bin file");
        let lzf_data = if lzf_path.exists() {
            fs::read(&lzf_path).expect("failed to read .lzf file")
        } else {
            Vec::new()
        };

        fixtures.push((name, bin_data, lzf_data));
    }

    fixtures
}

#[test]
fn fixture_decompress_matches_original() {
    let fixtures = load_fixtures();
    assert!(!fixtures.is_empty(), "no fixtures found");

    for (name, bin_data, lzf_data) in &fixtures {
        if bin_data.is_empty() {
            // Empty input: compressed should also be empty
            assert!(lzf_data.is_empty(), "fixture {}: empty input should have empty compressed", name);
            continue;
        }

        if lzf_data.is_empty() {
            // Compression failed (output too small), skip
            continue;
        }

        let mut decompressed = vec![0u8; bin_data.len()];
        match decompress(lzf_data, &mut decompressed) {
            Ok(n) => {
                decompressed.truncate(n);
                assert_eq!(&decompressed, bin_data, "fixture {}: decompressed data doesn't match original", name);
            }
            Err(LzfError::OutputTooSmall) => {
                // Try with a larger buffer
                let mut decompressed = vec![0u8; bin_data.len() * 2];
                let n = decompress(lzf_data, &mut decompressed).expect("decompress failed");
                decompressed.truncate(n);
                assert_eq!(&decompressed, bin_data, "fixture {}: decompressed data doesn't match original", name);
            }
            Err(e) => {
                panic!("fixture {}: decompress failed: {}", name, e);
            }
        }
    }
}

#[test]
fn fixture_compress_matches_compressed() {
    let fixtures = load_fixtures();
    assert!(!fixtures.is_empty(), "no fixtures found");

    for (name, bin_data, lzf_data) in &fixtures {
        if bin_data.is_empty() {
            continue;
        }

        if lzf_data.is_empty() {
            // Compression failed (output too small), skip
            continue;
        }

        let mut compressed = vec![0u8; bin_data.len() * 2 + 100];
        let n = compress(bin_data, &mut compressed).expect("compress failed");
        compressed.truncate(n);

        assert_eq!(&compressed, lzf_data, "fixture {}: compressed data doesn't match fixture", name);
    }
}

#[test]
fn fixture_round_trip() {
    let fixtures = load_fixtures();
    assert!(!fixtures.is_empty(), "no fixtures found");

    for (name, bin_data, _) in &fixtures {
        if bin_data.is_empty() {
            continue;
        }

        let mut compressed = vec![0u8; bin_data.len() * 2 + 100];
        let n = compress(bin_data, &mut compressed).expect("compress failed");
        compressed.truncate(n);

        let mut decompressed = vec![0u8; bin_data.len()];
        let m = decompress(&compressed, &mut decompressed).expect("decompress failed");
        decompressed.truncate(m);

        assert_eq!(&decompressed, bin_data, "fixture {}: round-trip failed", name);
    }
}

#[test]
fn fixture_compressed_size_reasonable() {
    let fixtures = load_fixtures();
    assert!(!fixtures.is_empty(), "no fixtures found");

    for (name, bin_data, lzf_data) in &fixtures {
        if bin_data.is_empty() || lzf_data.is_empty() {
            continue;
        }

        // Compressed should not exceed input + 104% (per lzf.h docs)
        let max_size = bin_data.len() + bin_data.len() / 16 + 64;
        assert!(
            lzf_data.len() <= max_size,
            "fixture {}: compressed size {} exceeds maximum {}",
            name,
            lzf_data.len(),
            max_size
        );
    }
}
