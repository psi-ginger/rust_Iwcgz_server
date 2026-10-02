# Fuzz harnesses for `gz-net` framing and `gz-cipher`

Three libFuzzer targets covering the receive path of the GoonZu wire protocol:

| target | what it drives | key invariants |
| --- | --- | --- |
| `frame_parser` | `FrameParser::feed` / `take_frame` with raw attacker bytes | wire length vs the declared size field; bytes drained never exceed bytes fed; a rejection is sticky; `clear()` does **not** un-poison the parser |
| `cipher_decrypt` | `Cipher::decode` with arbitrary payloads | no out-of-bounds write (canary around `dest`); both keys advance on every path, including rejected packets; round-trip against the *plaintext* input |
| `frame_then_decipher` | the real seam: frame → encipher → parse → decipher → decompress | end-to-end round trip; decompression reads the **deciphered** payload, never the enciphered one |

## Running them

Requires a nightly toolchain and, on MSVC, LLVM's x64 clang runtime:

```powershell
rustup toolchain install nightly
winget install LLVM.LLVM
```

Then:

```powershell
cargo +nightly fuzz build
cargo +nightly fuzz run frame_parser       -- -max_total_time=30
cargo +nightly fuzz run cipher_decrypt     -- -max_total_time=30
cargo +nightly fuzz run frame_then_decipher -- -max_total_time=30
```

Drop `-max_total_time` for an unbounded run; `Ctrl-C` stops it and the corpus is
saved as it goes.

## Windows specifics

The stock MSVC toolchain ships the ASan/libFuzzer runtimes **for x86 only**, so
the x64 link fails with `LNK1104` (missing
`clang_rt.asan_dynamic_runtime_thunk-x86_64.lib`). Installing LLVM supplies
them. Both link time and run time need the directory:

```powershell
$lib = "C:\PROGRA~1\LLVM\lib\clang\23\lib\windows"
$env:RUSTFLAGS = "-L native=$lib"     # 8.3 short path: RUSTFLAGS is space-split,
$env:PATH     = "$lib;$env:PATH"     # so "Program Files" cannot be quoted
```

`RUSTFLAGS` must carry the path rather than relying on MSVC's `LIBPATH`, because
`LIBPATH` is only honoured from an initialised Visual Studio environment.

On Linux none of this applies: rustup's bundled LLVM provides the runtime, and
Linux is the deployment target anyway.

## Long runs

30 seconds finds nothing, which is the point — it proves the harnesses build and
run. Real coverage comes from hours:

```powershell
# all three, 10 minutes each, sequentially
foreach ($t in @("frame_parser","cipher_decrypt","frame_then_decipher")) {
    cargo +nightly fuzz run $t -- -max_total_time=600
}
```

Run one target at a time. They are CPU-bound and share nothing.

## Corpus

`corpus/` holds 544 seeds: 25 hand-written ones covering the interesting
boundaries, plus 519 mutations libFuzzer derived from them. The mutations are
mostly redundant, but they are what the fuzzer actually reached, so they are
committed rather than pruned — at 183 KB total the cost is negligible and
discarding them would throw away coverage that took real time to find.

`crates/gz-net/tests/property_framing.rs` reads the same corpus on a **stable**
toolchain and re-checks every invariant over 20,000 pseudo-random cases per
test. That test is the gate that always runs; libFuzzer is for finding new
inputs, not for proving the properties hold.

## Wiring into CI

`scripts/check.ps1` runs the libFuzzer pass only when asked:

```powershell
powershell -File scripts\check.ps1 -Fuzz                 # 30s per target
powershell -File scripts\check.ps1 -Fuzz -FuzzSeconds 300
```

It is opt-in because it needs a nightly toolchain and LLVM, neither of which a
plain checkout is guaranteed to have. Without `-Fuzz` the invariants are still
covered, via step 3 (`cargo test --workspace`) running `property_framing.rs`.

## A trap worth knowing

libFuzzer on Windows **does not save the crashing artifact**. ASan aborts
(`0xc0000409`, `STATUS_STACK_BUFFER_OVERRUN`) before the corpus write happens,
even when the artifact directory already exists. Do not go looking for the
reproducer file — it will not be there.

To localise a finding, drive the same invariants from a `#[test]` with
`catch_unwind`, then narrow it with an exhaustive or randomised search. That is
how the `Cipher` round-trip bug was pinned down to a single
`(k1, k2, len, byte, declared)` tuple.