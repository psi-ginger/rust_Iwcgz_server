//! Generate LZF fixture files for testing.
//!
//! Run with: cargo run -p gz-lzf --example generate_fixtures
//!
//! Creates pairs of .bin (original) and .lzf (compressed) files in the
//! fixtures/ directory.

use gz_lzf::{LzfError, compress};
use std::fs;
use std::path::Path;

fn main() {
    let fixtures_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    fs::create_dir_all(&fixtures_dir).expect("failed to create fixtures directory");

    let test_cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", vec![]),
        ("single_byte", b"X".to_vec()),
        ("two_bytes", b"Hi".to_vec()),
        ("three_bytes", b"abc".to_vec()),
        ("hello_world", b"Hello, World!".to_vec()),
        (
            "short_text",
            b"The quick brown fox jumps over the lazy dog.".to_vec(),
        ),
        ("repetitive_100", vec![b'A'; 100]),
        ("repetitive_1000", vec![b'Z'; 1000]),
        ("all_zeros_500", vec![0u8; 500]),
        ("all_ff_300", vec![0xFFu8; 300]),
        ("abab_pattern", b"ABABABABABABABABABABABABABABABAB".to_vec()),
        (
            "text_repeated",
            b"the quick brown fox jumps over the lazy dog. \
                            the quick brown fox jumps over the lazy dog. \
                            the quick brown fox jumps over the lazy dog."
                .to_vec(),
        ),
        ("binary_256", (0..256).map(|i| i as u8).collect()),
        (
            "binary_repeated",
            (0..256).flat_map(|i| vec![i as u8; 10]).collect(),
        ),
        ("large_10k", (0..10000).map(|i| (i % 251) as u8).collect()),
        ("mixed_content", {
            let mut v = Vec::new();
            for i in 0..100 {
                if i % 3 == 0 {
                    v.extend(vec![b'X'; 50]);
                } else {
                    v.extend((0..50).map(|j| (i * 50 + j) as u8));
                }
            }
            v
        }),
        (
            "long_pattern_800",
            b"ABCDEFGH".iter().cycle().take(800).copied().collect(),
        ),
        (
            "near_boundary_31",
            (0..31).map(|i| (i * 37 + 11) as u8).collect(),
        ),
        (
            "near_boundary_32",
            (0..32).map(|i| (i * 37 + 11) as u8).collect(),
        ),
        (
            "near_boundary_33",
            (0..33).map(|i| (i * 37 + 11) as u8).collect(),
        ),
    ];

    for (name, input) in &test_cases {
        let bin_path = fixtures_dir.join(format!("{}.bin", name));
        let lzf_path = fixtures_dir.join(format!("{}.lzf", name));

        // Write the original data
        fs::write(&bin_path, input).expect("failed to write .bin file");

        // Compress and write the compressed data
        if input.is_empty() {
            // Empty input is an error for compress, write empty file
            fs::write(&lzf_path, []).expect("failed to write .lzf file");
        } else {
            let mut compressed = vec![0u8; input.len() * 2 + 100];
            match compress(input, &mut compressed) {
                Ok(n) => {
                    compressed.truncate(n);
                    fs::write(&lzf_path, &compressed).expect("failed to write .lzf file");
                    println!(
                        "  {}: {} -> {} bytes (ratio: {:.2}%)",
                        name,
                        input.len(),
                        n,
                        (n as f64 / input.len() as f64) * 100.0
                    );
                }
                Err(LzfError::OutputTooSmall) => {
                    // Write a marker file indicating compression failed
                    fs::write(&lzf_path, []).expect("failed to write .lzf file");
                    println!(
                        "  {}: {} bytes (compression failed: output too small)",
                        name,
                        input.len()
                    );
                }
                Err(e) => {
                    panic!("compression failed for {}: {}", name, e);
                }
            }
        }
    }

    println!("\nFixtures generated in: {}", fixtures_dir.display());
}
