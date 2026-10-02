//! LZF compression — exact port of libLZF (blueprint 01 Part A §3, task T-005).
//!
//! Legacy source: `NetworkLib\ITZNetLib\libLZF\lzf_c.c`, `lzf_d.c`, `lzfP.h`, `lzf.h`.
//! Compile-time settings: HLOG=16, VERY_FAST=1, ULTRA_FAST=0, CHECK_INPUT=1,
//! INIT_HTAB=0, LZF_STATE_ARG=0, STRICT_ALIGN=0.
//!
//! # Compressed format
//!
//! ```text
//! 000LLLLL <L+1>    ; literal run
//! LLLooooo oooooooo ; backref (len < 7)
//! 111ooooo LLLLLLLL oooooooo ; backref (len >= 7)
//! ```

/// Why a call could not be completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LzfError {
    /// The output buffer is too small to hold the result.
    OutputTooSmall,
    /// The input is not a valid LZF stream.
    InvalidInput,
    /// The input is empty or the output buffer is empty.
    Empty,
}

impl std::fmt::Display for LzfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutputTooSmall => write!(f, "output buffer too small"),
            Self::InvalidInput => write!(f, "invalid LZF stream"),
            Self::Empty => write!(f, "empty input or output"),
        }
    }
}

impl std::error::Error for LzfError {}

// Compile-time constants matching lzfP.h
const HLOG: u32 = 16;
const HSIZE: usize = 1 << HLOG; // 65536
const MAX_LIT: usize = 1 << 5; // 32
const MAX_OFF: usize = 1 << 13; // 8192
const MAX_REF: usize = (1 << 8) + (1 << 3); // 264

/// Hash function: FRST(p) = (p[0] << 8) | p[1]
#[inline(always)]
fn frst(p: &[u8]) -> u32 {
    ((p[0] as u32) << 8) | p[1] as u32
}

/// Hash function: NEXT(v, p) = (v << 8) | p[2]
#[inline(always)]
fn next(v: u32, p: &[u8]) -> u32 {
    (v << 8) | p[2] as u32
}

/// Hash function: IDX(h) = ((h >> 8) - h*5) & (HSIZE - 1)  [VERY_FAST]
#[inline(always)]
fn idx(h: u32) -> usize {
    ((h >> 8).wrapping_sub(h.wrapping_mul(5)) & (HSIZE as u32 - 1)) as usize
}

/// Compress `input` into `output`. Returns the number of bytes written.
///
/// Matches `lzf_compress` from lzf_c.c with HLOG=16, VERY_FAST=1, ULTRA_FAST=0,
/// INIT_HTAB=0, LZF_STATE_ARG=0, STRICT_ALIGN=0.
///
/// # Errors
///
/// * [`LzfError::Empty`] if `input` or `output` is empty.
/// * [`LzfError::OutputTooSmall`] if `output` cannot hold the compressed data.
pub fn compress(input: &[u8], output: &mut [u8]) -> Result<usize, LzfError> {
    if input.is_empty() || output.is_empty() {
        return Err(LzfError::Empty);
    }

    let in_len = input.len();
    let out_len = output.len();

    // Hash table: stores offset from input start, 0 means empty.
    // A valid ref must be at offset >= 1 (ref > in_data in the C code).
    let mut htab = vec![0usize; HSIZE];

    let mut ip: usize = 0; // input pointer (offset)
    let mut op: usize = 0; // output pointer (offset)
    let in_end = in_len;
    let out_end = out_len;

    let mut lit: usize = 0;
    op += 1; // start run

    // FRST(ip) reads ip[0] and ip[1]; guard against short input.
    let mut hval = if in_len >= 2 { frst(input) } else { 0 };

    // Main loop: ip < in_end - 2 (equivalent to ip + 2 < in_end for in_len >= 2).
    // For in_len < 3, skip the loop (the C code has UB here).
    while ip + 2 < in_end {
        hval = next(hval, &input[ip..]);
        let hslot_idx = idx(hval);
        let ref_off = htab[hslot_idx];
        htab[hslot_idx] = ip;

        // Check match conditions
        let off = ip.wrapping_sub(ref_off).wrapping_sub(1);
        if off < MAX_OFF && ip + 4 < in_end && ref_off > 0 {
            let ref_slice = &input[ref_off..];
            let ip_slice = &input[ip..];

            // STRICT_ALIGN=0: compare u16 + byte (equivalent to comparing 3 bytes)
            if ref_slice[0] == ip_slice[0]
                && ref_slice[1] == ip_slice[1]
                && ref_slice[2] == ip_slice[2]
            {
                // Match found
                let mut len: usize = 2;
                let mut maxlen = in_end - ip - len;
                if maxlen > MAX_REF {
                    maxlen = MAX_REF;
                }

                // Stop run
                output[op - lit - 1] = lit.wrapping_sub(1) as u8;
                if lit == 0 {
                    op -= 1; // undo run if length is zero
                }

                // Check output space
                if op + 4 >= out_end {
                    return Err(LzfError::OutputTooSmall);
                }

                // Extend match
                //
                // clippy::never_loop fires here and it is wrong. The legacy is
                // `for (;;) { ... break; }` with that trailing `break;` at
                // lzf_c.c:208, so the body genuinely runs exactly once.
                // Restructuring it would diverge from the source this crate
                // exists to mirror byte for byte, which is the point of the
                // port. examples/diff_oracle.rs holds that fidelity claim to
                // account against the compiled original.
                #[allow(clippy::never_loop)]
                loop {
                    if maxlen > 16 {
                        // Unrolled 16 bytes
                        len += 1;
                        if ref_slice[len] != ip_slice[len] {
                            break;
                        }
                        len += 1;
                        if ref_slice[len] != ip_slice[len] {
                            break;
                        }
                        len += 1;
                        if ref_slice[len] != ip_slice[len] {
                            break;
                        }
                        len += 1;
                        if ref_slice[len] != ip_slice[len] {
                            break;
                        }
                        len += 1;
                        if ref_slice[len] != ip_slice[len] {
                            break;
                        }
                        len += 1;
                        if ref_slice[len] != ip_slice[len] {
                            break;
                        }
                        len += 1;
                        if ref_slice[len] != ip_slice[len] {
                            break;
                        }
                        len += 1;
                        if ref_slice[len] != ip_slice[len] {
                            break;
                        }
                        len += 1;
                        if ref_slice[len] != ip_slice[len] {
                            break;
                        }
                        len += 1;
                        if ref_slice[len] != ip_slice[len] {
                            break;
                        }
                        len += 1;
                        if ref_slice[len] != ip_slice[len] {
                            break;
                        }
                        len += 1;
                        if ref_slice[len] != ip_slice[len] {
                            break;
                        }
                        len += 1;
                        if ref_slice[len] != ip_slice[len] {
                            break;
                        }
                        len += 1;
                        if ref_slice[len] != ip_slice[len] {
                            break;
                        }
                        len += 1;
                        if ref_slice[len] != ip_slice[len] {
                            break;
                        }
                        len += 1;
                        if ref_slice[len] != ip_slice[len] {
                            break;
                        }
                    }

                    // Remaining bytes.
                    //
                    // Faithful to legacy lzf_c.c:204-206, which is a do-while:
                    //     do len++; while (len < maxlen && ref[len] == ip[len]);
                    // The increment is unconditional, so the legacy always
                    // consumes one more octet *before* testing the bound. A
                    // plain `while` here lands one octet short, and the port
                    // then has to emit the leftover as its own literal run --
                    // two extra bytes on the wire for any match that ends on
                    // the bound. Measured against the compiled legacy: this input
                    // produced 26 bytes with a plain `while` and 24 bytes with
                    // the do-while, which is what lzf_c.c emits. See
                    // crates/gz-lzf/examples/diff_oracle.rs.
                    //
                    // The unconditional increment cannot read out of bounds: it
                    // dereferences only on the following test, which is guarded
                    // by `len < maxlen`, and maxlen = in_end - ip - 2, so the
                    // largest index ever touched is maxlen - 1.
                    len += 1;
                    while len < maxlen && ref_slice[len] == ip_slice[len] {
                        len += 1;
                    }
                    break;
                }

                len -= 2; // len is now #octets - 1
                ip += 1;

                // Encode backref
                if len < 7 {
                    output[op] = (off >> 8) as u8 + (len << 5) as u8;
                    op += 1;
                } else {
                    output[op] = (off >> 8) as u8 + (7 << 5) as u8;
                    op += 1;
                    output[op] = (len - 7) as u8;
                    op += 1;
                }
                output[op] = off as u8;
                op += 1;

                lit = 0;
                op += 1; // start run

                ip += len + 1;

                if ip >= in_end - 2 {
                    break;
                }

                // VERY_FAST rehashing (two rehashes per match)
                ip -= 2;
                hval = frst(&input[ip..]);

                hval = next(hval, &input[ip..]);
                htab[idx(hval)] = ip;
                ip += 1;

                hval = next(hval, &input[ip..]);
                htab[idx(hval)] = ip;
                ip += 1;

                continue;
            }
        }

        // Literal byte
        if op >= out_end {
            return Err(LzfError::OutputTooSmall);
        }

        lit += 1;
        output[op] = input[ip];
        op += 1;
        ip += 1;

        if lit == MAX_LIT {
            output[op - lit - 1] = lit.wrapping_sub(1) as u8;
            lit = 0;
            op += 1; // start run
        }
    }

    // Check output space for remaining literals
    if op + 3 > out_end {
        return Err(LzfError::OutputTooSmall);
    }

    // Copy remaining bytes as literals
    while ip < in_end {
        lit += 1;
        output[op] = input[ip];
        op += 1;
        ip += 1;

        if lit == MAX_LIT {
            output[op - lit - 1] = lit.wrapping_sub(1) as u8;
            lit = 0;
            op += 1;
        }
    }

    // End run
    output[op - lit - 1] = lit.wrapping_sub(1) as u8;
    if lit == 0 {
        op -= 1;
    }

    Ok(op)
}

/// Decompress `input` into `output`. Returns the number of bytes written.
///
/// Matches `lzf_decompress` from lzf_d.c with CHECK_INPUT=1.
///
/// # Errors
///
/// * [`LzfError::OutputTooSmall`] if `output` cannot hold the decompressed data.
/// * [`LzfError::InvalidInput`] if `input` is not a valid LZF stream.
pub fn decompress(input: &[u8], output: &mut [u8]) -> Result<usize, LzfError> {
    let mut ip: usize = 0;
    let mut op: usize = 0;
    let in_end = input.len();
    let out_end = output.len();

    loop {
        if ip >= in_end {
            break;
        }

        let ctrl = input[ip];
        ip += 1;

        if ctrl < (1 << 5) {
            // Literal run
            let len = (ctrl + 1) as usize;

            if op + len > out_end {
                return Err(LzfError::OutputTooSmall);
            }
            if ip + len > in_end {
                return Err(LzfError::InvalidInput);
            }

            output[op..op + len].copy_from_slice(&input[ip..ip + len]);
            op += len;
            ip += len;
        } else {
            // Back reference
            let mut len = (ctrl >> 5) as usize;

            // Check ip >= in_end (before reading the offset byte)
            if ip >= in_end {
                return Err(LzfError::InvalidInput);
            }

            if len == 7 {
                len += input[ip] as usize;
                ip += 1;
                if ip >= in_end {
                    return Err(LzfError::InvalidInput);
                }
            }

            let ref_off = op as isize - (((ctrl & 0x1f) as isize) << 8) - 1 - (input[ip] as isize);
            ip += 1;

            if op + len + 2 > out_end {
                return Err(LzfError::OutputTooSmall);
            }
            if ref_off < 0 {
                return Err(LzfError::InvalidInput);
            }
            let ref_off = ref_off as usize;

            // Copy len+2 bytes from ref to op (byte-by-byte for overlapping copies)
            for i in 0..len + 2 {
                output[op + i] = output[ref_off + i];
            }
            op += len + 2;
        }
    }

    Ok(op)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for the `do`-while in the match-extension loop.
    ///
    /// The legacy extends a match with `do len++; while (len < maxlen && ...)`
    /// (lzf_c.c:204-206). The unconditional increment is load-bearing: the
    /// legacy always consumes one more octet *before* testing the bound. An
    /// earlier port of this function used a plain `while`, which ended the
    /// match one octet early and then had to spill the leftover into its own
    /// literal run -- two extra bytes on the wire for any match that finished
    /// exactly on the bound.
    ///
    /// This input is the smallest shape in `examples/diff_oracle.rs` that
    /// exposed it: a 300-byte period-16 ramp. The expected bytes below are
    /// not hand-derived -- they are what the *compiled* `lzf_compress` from the
    /// legacy tree emits for this exact input, captured via
    /// `tools/lzf-oracle` (see `build_oracle.bat`).
    #[test]
    fn compress_matches_legacy_bytes_on_bound_length_match() {
        // Period-16 ramp: value[i] = (i % 16) * 7 + 13.
        let input: Vec<u8> = (0..300).map(|i| ((i % 16) * 7 + 13) as u8).collect();

        // Verified against legacy lzf_c.c, not against our own output.
        let expected: Vec<u8> = vec![
            0x10, 0x0d, 0x14, 0x1b, 0x22, 0x29, 0x30, 0x37, 0x3e, 0x45, 0x4c, 0x53, 0x5a, 0x61,
            0x68, 0x6f, 0x76, 0x0d, 0xe0, 0xff, 0x0f, 0xe1, 0x0a, 0x0f,
        ];

        // lzf_compress_bound from lzfP.c, which the crate does not export:
        // the legacy body is `length + length / 16 + 4 + 3`.
        let bound = input.len() + input.len() / 16 + 4 + 3;
        let mut out = vec![0u8; bound];
        let n = compress(&input, &mut out).expect("compress");
        assert_eq!(
            &out[..n],
            expected.as_slice(),
            "compressed bytes diverged from the legacy lzf_compress"
        );

        // And it must still round-trip.
        let mut back = vec![0u8; input.len()];
        let m = decompress(&out[..n], &mut back).expect("decompress");
        assert_eq!(m, input.len());
        assert_eq!(&back[..m], input.as_slice());
    }

    #[test]
    fn compress_empty_input_returns_error() {
        let input: &[u8] = b"";
        let mut output = [0u8; 100];
        assert_eq!(compress(input, &mut output), Err(LzfError::Empty));
    }

    #[test]
    fn compress_empty_output_returns_error() {
        let input = b"hello";
        let output: &mut [u8] = &mut [];
        assert_eq!(compress(input, output), Err(LzfError::Empty));
    }

    #[test]
    fn decompress_empty_input_returns_zero() {
        let input: &[u8] = b"";
        let mut output = [0u8; 100];
        assert_eq!(decompress(input, &mut output), Ok(0));
    }

    #[test]
    fn round_trip_simple() {
        let input = b"Hello, World!";
        let mut compressed = vec![0u8; input.len() * 2];
        let compressed_len = compress(input, &mut compressed).unwrap();
        compressed.truncate(compressed_len);

        let mut decompressed = vec![0u8; input.len()];
        let decompressed_len = decompress(&compressed, &mut decompressed).unwrap();
        decompressed.truncate(decompressed_len);

        assert_eq!(decompressed, input);
    }

    #[test]
    fn round_trip_repetitive() {
        let input = vec![b'A'; 1000];
        let mut compressed = vec![0u8; input.len() * 2];
        let compressed_len = compress(&input, &mut compressed).unwrap();
        compressed.truncate(compressed_len);

        let mut decompressed = vec![0u8; input.len()];
        let decompressed_len = decompress(&compressed, &mut decompressed).unwrap();
        decompressed.truncate(decompressed_len);

        assert_eq!(decompressed, input);
        // Repetitive data should compress well
        assert!(compressed_len < input.len() / 10);
    }

    #[test]
    fn round_trip_random() {
        let input: Vec<u8> = (0..256).map(|i| (i * 7 + 13) as u8).collect();
        let mut compressed = vec![0u8; input.len() * 2];
        let compressed_len = compress(&input, &mut compressed).unwrap();
        compressed.truncate(compressed_len);

        let mut decompressed = vec![0u8; input.len()];
        let decompressed_len = decompress(&compressed, &mut decompressed).unwrap();
        decompressed.truncate(decompressed_len);

        assert_eq!(decompressed, input);
    }

    #[test]
    fn round_trip_single_byte() {
        let input = b"X";
        let mut compressed = vec![0u8; 100];
        let compressed_len = compress(input, &mut compressed).unwrap();
        compressed.truncate(compressed_len);

        let mut decompressed = vec![0u8; 1];
        let decompressed_len = decompress(&compressed, &mut decompressed).unwrap();
        decompressed.truncate(decompressed_len);

        assert_eq!(decompressed, input);
    }

    #[test]
    fn round_trip_two_bytes() {
        let input = b"Hi";
        let mut compressed = vec![0u8; 100];
        let compressed_len = compress(input, &mut compressed).unwrap();
        compressed.truncate(compressed_len);

        let mut decompressed = vec![0u8; 2];
        let decompressed_len = decompress(&compressed, &mut decompressed).unwrap();
        decompressed.truncate(decompressed_len);

        assert_eq!(decompressed, input);
    }

    #[test]
    fn round_trip_all_zeros() {
        let input = vec![0u8; 500];
        let mut compressed = vec![0u8; input.len() * 2];
        let compressed_len = compress(&input, &mut compressed).unwrap();
        compressed.truncate(compressed_len);

        let mut decompressed = vec![0u8; input.len()];
        let decompressed_len = decompress(&compressed, &mut decompressed).unwrap();
        decompressed.truncate(decompressed_len);

        assert_eq!(decompressed, input);
    }

    #[test]
    fn round_trip_text_with_repeated_patterns() {
        let input = b"the quick brown fox jumps over the lazy dog. \
                      the quick brown fox jumps over the lazy dog. \
                      the quick brown fox jumps over the lazy dog.";
        let mut compressed = vec![0u8; input.len() * 2];
        let compressed_len = compress(input, &mut compressed).unwrap();
        compressed.truncate(compressed_len);

        let mut decompressed = vec![0u8; input.len()];
        let decompressed_len = decompress(&compressed, &mut decompressed).unwrap();
        decompressed.truncate(decompressed_len);

        assert_eq!(decompressed, input);
    }

    #[test]
    fn compress_output_too_small() {
        let input = b"Hello, World! This is a test of the emergency broadcast system.";
        let mut output = [0u8; 5]; // Way too small
        assert_eq!(compress(input, &mut output), Err(LzfError::OutputTooSmall));
    }

    #[test]
    fn decompress_output_too_small() {
        // Create a valid compressed stream
        let input = b"Hello, World!";
        let mut compressed = vec![0u8; input.len() * 2];
        let compressed_len = compress(input, &mut compressed).unwrap();
        compressed.truncate(compressed_len);

        let mut output = [0u8; 5]; // Too small
        assert_eq!(
            decompress(&compressed, &mut output),
            Err(LzfError::OutputTooSmall)
        );
    }

    #[test]
    fn decompress_invalid_input_truncated_literal() {
        // Literal run claiming 10 bytes but only 3 available
        let input = [0x09, b'a', b'b', b'c'];
        let mut output = [0u8; 100];
        assert_eq!(decompress(&input, &mut output), Err(LzfError::InvalidInput));
    }

    #[test]
    fn decompress_invalid_input_truncated_backref() {
        // Back reference with no offset byte
        let input = [0xe0]; // len=7, needs 2 more bytes
        let mut output = [0u8; 100];
        assert_eq!(decompress(&input, &mut output), Err(LzfError::InvalidInput));
    }

    #[test]
    fn decompress_invalid_input_bad_offset() {
        // Back reference with offset pointing before output start
        let input = [0xff, 0xff]; // len=7+255=262, offset=8191
        let mut output = [0u8; 1000];
        assert_eq!(decompress(&input, &mut output), Err(LzfError::InvalidInput));
    }

    #[test]
    fn round_trip_large_data() {
        let input: Vec<u8> = (0..10000).map(|i| (i % 251) as u8).collect();
        let mut compressed = vec![0u8; input.len() * 2];
        let compressed_len = compress(&input, &mut compressed).unwrap();
        compressed.truncate(compressed_len);

        let mut decompressed = vec![0u8; input.len()];
        let decompressed_len = decompress(&compressed, &mut decompressed).unwrap();
        decompressed.truncate(decompressed_len);

        assert_eq!(decompressed, input);
    }

    #[test]
    fn round_trip_binary_data() {
        let input: Vec<u8> = (0..=255u8).flat_map(|i| vec![i; 10]).collect();
        let mut compressed = vec![0u8; input.len() * 2];
        let compressed_len = compress(&input, &mut compressed).unwrap();
        compressed.truncate(compressed_len);

        let mut decompressed = vec![0u8; input.len()];
        let decompressed_len = decompress(&compressed, &mut decompressed).unwrap();
        decompressed.truncate(decompressed_len);

        assert_eq!(decompressed, input);
    }

    #[test]
    fn hash_function_matches_c() {
        // Test FRST
        let p = [0x12, 0x34, 0x56];
        assert_eq!(frst(&p), 0x1234);

        // Test NEXT
        assert_eq!(next(0x1234, &p), 0x123456);

        // Test IDX (VERY_FAST)
        let h = 0x123456u32;
        let expected = ((h >> 8).wrapping_sub(h.wrapping_mul(5)) & 0xFFFF) as usize;
        assert_eq!(idx(h), expected);
    }

    #[test]
    fn compress_produces_valid_lzf_stream() {
        let input = b"ABABABABABABABABABABABABABABABAB";
        let mut compressed = vec![0u8; input.len() * 2];
        let compressed_len = compress(input, &mut compressed).unwrap();
        compressed.truncate(compressed_len);

        // Verify the stream can be decompressed
        let mut decompressed = vec![0u8; input.len()];
        let decompressed_len = decompress(&compressed, &mut decompressed).unwrap();
        decompressed.truncate(decompressed_len);
        assert_eq!(decompressed, input);
    }

    #[test]
    fn round_trip_empty_after_compress() {
        // Compressing a single byte should produce a valid stream
        let input = b"A";
        let mut compressed = vec![0u8; 100];
        let compressed_len = compress(input, &mut compressed).unwrap();
        assert!(compressed_len > 0);
        compressed.truncate(compressed_len);

        let mut decompressed = vec![0u8; 1];
        let decompressed_len = decompress(&compressed, &mut decompressed).unwrap();
        assert_eq!(decompressed_len, 1);
        assert_eq!(decompressed[0], b'A');
    }
}
