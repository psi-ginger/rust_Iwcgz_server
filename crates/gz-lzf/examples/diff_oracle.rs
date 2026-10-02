//! Differential test: `gz-lzf::compress` vs the compiled legacy `lzf_compress`.
//!
//! Run with the oracle built first:
//!   tools\lzf-oracle\build_oracle.bat
//!   cargo run --release -p gz-lzf --example diff_oracle
//!
//! # Why this exists
//!
//! `crates/gz-lzf/tests/fixtures.rs` round-trips the committed `.bin`/`.lzf`
//! pairs, but both sides of that round trip are *our own* code: the `.lzf`
//! files were produced by `generate_fixtures.rs` using the port itself. That
//! proves the port is self-consistent, not that it agrees with the legacy.
//! A divergence in the encoding would pass every existing test while breaking
//! wire compatibility with running clients.
//!
//! So this compares against ground truth: the unmodified `lzf_c.c` from the
//! legacy tree, compiled with the parameters that tree actually resolves to
//! (asserted at compile time in `lzf_ref.c`).
//!
//! The corpus deliberately over-samples repetitive inputs. The suspect region
//! is the match-extension loop, where legacy `lzf_c.c:204-206` uses a
//! *do-while* (`do len++; while (cond);`) and a plain `while (cond) { len++ }`
//! is not equivalent: the former always increments once before testing. The two
//! only diverge when the 16 unrolled comparisons land exactly on `maxlen`, so
//! the inputs here are sized and shaped to hit `MAX_REF` (264) boundaries.

use std::process::Command;

use gz_lzf::{compress, decompress};

const ORACLE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tools/lzf-oracle/build/lzf_ref.exe"
);

struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
    fn fill(&mut self, buf: &mut [u8]) {
        for b in buf {
            *b = (self.next_u64() >> 24) as u8;
        }
    }
}

/// Oracle result: the legacy compressor's bytes, plus whether the legacy
/// decompressor agrees they round-trip (so the stream is genuinely valid).
struct Oracle {
    bytes: Vec<u8>,
    legacy_valid: bool,
}

fn temp_path(tag: &str) -> std::path::PathBuf {
    let pid = std::process::id();
    std::env::temp_dir().join(format!("gz-lzf-oracle-{pid}-{tag}.bin"))
}

/// Returns `None` when the legacy compressor refused the input.
fn run_oracle(input: &[u8], tag: &str) -> Result<Option<Oracle>, String> {
    // The oracle takes a FILE PATH, not stdin. Passing bytes on stdin is a
    // trap: MSVC opens stdin in TEXT mode and the CRT turns 0x1A (Ctrl-Z, the
    // MS-DOS end-of-file marker) into end-of-file, so `fread` silently returns
    // a truncated input. That truncation is indistinguishable from a real
    // compression divergence -- it produced ~70 phantom "mismatches" on
    // 2026-10-02 before it was found. Always go through a real file.
    let path = temp_path(tag);
    std::fs::write(&path, input).map_err(|e| format!("write {}: {e}", path.display()))?;

    let result = (|| -> Result<Option<Oracle>, String> {
        let out = Command::new(ORACLE)
            .arg(&path)
            .output()
            .map_err(|e| format!("spawn {ORACLE}: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "oracle exit {:?}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }

        // Oracle reports as text, one `key value` line at a time:
        //   got N / maxout N / n N / wrote N / hex <hex> / valid 0|1
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let mut got: Option<usize> = None;
        let mut n: Option<usize> = None;
        let mut wrote: Option<usize> = None;
        let mut hex = String::new();
        let mut valid = false;

        for line in text.lines() {
            let (key, value) = match line.split_once(' ') {
                Some(kv) => kv,
                None => continue,
            };
            let v = value.trim();
            match key {
                "got" => got = v.parse().ok(),
                "n" => n = v.parse().ok(),
                "wrote" => wrote = v.parse().ok(),
                "hex" => hex = v.to_string(),
                "valid" => valid = v == "1",
                _ => {}
            }
        }

        // Falsification gate #1: did the oracle actually receive our bytes? If
        // the transport dropped any, nothing it reports is about our input.
        match got {
            Some(g) if g == input.len() => {}
            Some(g) => {
                return Err(format!(
                    "ORACLE GOT {g} BYTES, EXPECTED {} -- transport truncated the input",
                    input.len()
                ));
            }
            None => return Err(format!("oracle printed no 'got' line: {text:?}")),
        }

        let n = n.ok_or_else(|| format!("oracle printed no 'n' line: {text:?}"))?;
        if n == 0 {
            return Ok(None);
        }

        // Falsification gate #2: a correct lzf_compress never writes more than
        // the space it was given, and never disagrees with its own fwrite.
        if let Some(w) = wrote
            && w > n
        {
            return Err(format!("oracle inconsistent: wrote {w} > n {n}"));
        }

        Ok(Some(Oracle {
            bytes: hex_decode(&hex)?,
            legacy_valid: valid,
        }))
    })();

    let _ = std::fs::remove_file(&path);
    result
}

fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    if s.is_empty() {
        return Ok(Vec::new());
    }
    if !s.len().is_multiple_of(2) {
        return Err(format!("odd-length hex: {s:?}"));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| format!("bad hex at {i}: {e}")))
        .collect()
}

/// `lzf_compress_bound` from lzf_c.c.
fn compress_bound(len: usize) -> usize {
    len + len / 16 + 4 + 3
}

/// Outcome of one differential case.
enum Outcome {
    /// Port and legacy produced identical bytes (or both refused the input).
    Agreed,
    /// The legacy compressor refused; the port refused too.
    BothRefused,
}

fn check(input: &[u8], tag: &str) -> Result<Outcome, String> {
    let expected = run_oracle(input, tag)?;

    let mut buf = vec![0u8; compress_bound(input.len())];
    let ours = compress(input, &mut buf);

    match (expected, ours) {
        (None, Err(_)) => Ok(Outcome::BothRefused), // both refuse
        (None, Ok(n)) => Err(format!("legacy refused, port returned Ok({n})")),
        (Some(exp), Err(e)) => Err(format!(
            "legacy produced {} bytes (self-roundtrip={}), port errored: {e}",
            exp.bytes.len(),
            exp.legacy_valid
        )),
        (Some(exp), Ok(n)) => {
            let got = &buf[..n];

            // Falsification gate. If the legacy's OWN decompressor cannot read
            // the legacy compressor's output, the oracle is not a trustworthy
            // reference for these inputs and any byte difference is the
            // harness's problem, not the port's. Never report a port bug on a
            // stream the legacy itself cannot decode.
            if !exp.legacy_valid {
                return Err(format!(
                    "ORACLE SELF-CHECK FAILED (legacy_d cannot read legacy_c output): \
                     in_len={} legacy_len={}",
                    input.len(),
                    exp.bytes.len()
                ));
            }

            if got == exp.bytes.as_slice() {
                return Ok(Outcome::Agreed);
            }

            // The oracle is trustworthy here, so a difference is real. Still
            // record whether our decompressor accepts the legacy bytes: that is
            // the direction that actually matters, because the server has to
            // read streams a legacy client produced.
            let mut rt = vec![0u8; input.len() + 64];
            let ours_reads_legacy = match decompress(&exp.bytes, &mut rt) {
                Ok(m) if m == input.len() && &rt[..m] == input => "yes".to_string(),
                Ok(m) => format!("wrong-length (m={m}, want {})", input.len()),
                Err(e) => format!("NO - {e}"),
            };

            let first = got
                .iter()
                .zip(&exp.bytes)
                .position(|(a, b)| a != b)
                .unwrap_or(got.len().min(exp.bytes.len()));

            // Short streams are far easier to compare in full than as a window
            // around the first difference, so dump them whole.
            let show = |v: &[u8]| -> String {
                if v.len() <= 64 {
                    format!("{:02x?}", v)
                } else {
                    format!(
                        "[{} bytes] head {:02x?} ... at {first}: {:02x?}",
                        v.len(),
                        &v[..16],
                        &v[first.min(v.len())..(first + 16).min(v.len())]
                    )
                }
            };

            Err(format!(
                "MISMATCH: in_len={} legacy_len={} port_len={} first_diff_at={first}\n    \
                 our decompress reads legacy output: {ours_reads_legacy}\n    \
                 legacy = {}\n    port   = {}",
                input.len(),
                exp.bytes.len(),
                n,
                show(&exp.bytes),
                show(got),
            ))
        }
    }
}

/// Corpus shapes, aimed at the match-extension loop rather than at entropy.
fn cases(rng: &mut Rng) -> Vec<Vec<u8>> {
    let mut out = Vec::new();

    // Pure repetition: every match runs to the cap, so the do-while divergence
    // is maximally likely. Periods chosen around MAX_REF (264).
    for period in [
        1usize, 2, 3, 5, 7, 16, 17, 31, 32, 33, 63, 64, 128, 260, 263, 264, 265, 528,
    ] {
        for len in [
            period,
            period + 1,
            period + 15,
            period + 16,
            period + 17,
            300,
            600,
            1024,
        ] {
            let mut v = vec![0u8; len];
            for (i, b) in v.iter_mut().enumerate() {
                *b = ((i % period) * 7 + 13) as u8;
            }
            out.push(v);
        }
    }

    // Repetition with a single mutated byte, at every offset near the cap.
    // This is what makes the match stop *exactly* at a chosen length.
    for period in [3usize, 16, 264] {
        for stop in [255usize, 256, 263, 264, 265, 279, 280] {
            let mut v: Vec<u8> = (0..600).map(|i| (i % period) as u8).collect();
            if stop < v.len() {
                v[stop] ^= 0xFF;
                out.push(v);
            }
        }
    }

    // Runs of a single byte: the literal-run path plus the run extension.
    for len in [1usize, 2, 3, 4, 17, 32, 33, 64, 265, 266, 600] {
        out.push(vec![0x41; len]);
        out.push(vec![0x00; len]);
    }

    // Mixed: a compressible prefix then random bytes.
    for split in [16usize, 64, 128, 264, 300] {
        let mut v: Vec<u8> = (0..split).map(|i| ((i * 31) % 251) as u8).collect();
        let mut tail = vec![0u8; 200];
        rng.fill(&mut tail);
        v.extend_from_slice(&tail);
        out.push(v);
    }

    // General random data of assorted sizes, including the harness's own bounds.
    for len in [
        1usize, 2, 3, 4, 5, 8, 15, 16, 17, 31, 32, 33, 63, 64, 65, 127, 128, 129, 255, 256, 257,
        511, 512, 1000, 2048, 4096, 8191,
    ] {
        let mut v = vec![0u8; len];
        rng.fill(&mut v);
        out.push(v);
    }

    // Short alphabet random data compresses better and exercises more matches.
    for len in [32usize, 100, 264, 300, 700, 2000] {
        let mut v = vec![0u8; len];
        for b in v.iter_mut() {
            *b = rng.below(4) as u8;
        }
        out.push(v);
    }

    out.retain(|v| !v.is_empty());
    out
}

fn main() {
    let mut rng = Rng(0xC0FF_EE00_1234_5678);
    let corpus = cases(&mut rng);
    println!("oracle: {ORACLE}");
    println!("cases:  {}", corpus.len());

    let mut agreed = 0usize;
    let mut both_refused = 0usize;
    let mut failures: Vec<(usize, String)> = Vec::new();

    for (i, input) in corpus.iter().enumerate() {
        match check(input, &format!("c{i}")) {
            Ok(Outcome::Agreed) => agreed += 1,
            Ok(Outcome::BothRefused) => {
                agreed += 1;
                both_refused += 1;
            }
            Err(e) => failures.push((i, e)),
        }
    }

    println!("agreed:  {agreed} (of which {both_refused} refused by both)");
    println!(
        "oracle self-check failures: {}",
        failures
            .iter()
            .filter(|(_, e)| e.contains("ORACLE"))
            .count()
    );

    if failures.is_empty() {
        println!("\nRESULT: port is byte-identical to legacy lzf_compress on all cases.");
        return;
    }

    println!("\nRESULT: {} DIVERGENCE(S):", failures.len());
    for (i, e) in failures.iter().take(5) {
        println!("  case {i}: {e}");
    }
    std::process::exit(1);
}
