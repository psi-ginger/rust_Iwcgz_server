# Legacy LZF differential oracle

Answers one question: **is `gz-lzf::compress` byte-identical to the legacy
`lzf_compress`?**

## Why it exists

`crates/gz-lzf/tests/fixtures.rs` round-trips committed `.bin`/`.lzf` pairs,
but both sides of that round trip are our own code — the `.lzf` files were
produced by `examples/generate_fixtures.rs` using the port itself. That proves
the port is *self-consistent*, not that it agrees with the legacy. A divergence
in the encoding would pass every committed test while breaking wire
compatibility with running clients.

This compares against ground truth: the unmodified `lzf_c.c` from the read-only
legacy tree, compiled with the parameters that tree actually resolves to
(asserted at compile time in `lzf_ref.c`).

## Usage

```powershell
tools\lzf-oracle\build_oracle.bat
cargo run --release -p gz-lzf --example diff_oracle
```

Expected output:

```
cases:  225
agreed:  225 (of which 0 refused by both)
oracle self-check failures: 0

RESULT: port is byte-identical to legacy lzf_compress on all cases.
```

The oracle is not built by `cargo`, so this is **not** part of
`scripts/check.ps1` — it needs MSVC and the legacy tree. Re-run it whenever
`crates/gz-lzf/src/lib.rs` changes.

## What it has already caught

The match-extension loop ended up one octet short. The legacy extends a match
with a **do-while** (`lzf_c.c:204-206`):

```c
do
  len++;
while (len < maxlen && ref[len] == ip[len]);
```

The increment is unconditional — the legacy always consumes one more octet
*before* testing the bound. The port had a plain `while`, so any match that
ended exactly on the bound came up a byte short and the leftover had to be
spilled into its own literal run: two extra bytes on the wire. A 300-byte
period-16 ramp compressed to 26 bytes instead of 24.

Fixed in `crates/gz-lzf/src/lib.rs` and pinned by
`compress_matches_legacy_bytes_on_bound_length_match`, whose expected bytes are
the legacy's, not ours.

## Why the oracle takes a file path, not stdin

This is the single most important detail in this directory, and getting it wrong
is expensive.

MSVC opens `stdin` in **text** mode, and the CRT translates `0x1A` (Ctrl-Z, the
MS-DOS end-of-file marker) into end-of-file. So `fread` on stdin silently returns
a truncated input. That truncation is indistinguishable from a compression
divergence: it produced 71 phantom "mismatches" before it was found, including a
128-byte input that arrived as 75 bytes.

The tell is a file of the same length whose bytes avoid `0x1A` reading back at
full length. Hence `fopen(..., "rb")`, and hence the unconditional `got <n>`
line — if the transport ever drops bytes again, it shows up in the output rather
than having to be inferred from mismatches.

## Layout

- `lzf_ref.c` — harness: reads a file, runs `lzf_compress`, reports text.
- `build_oracle.bat` — builds it with `lzf_c.c` and `lzf_d.c` via `vcvars64.bat`.
- `build/` — output. Not worth committing; rebuild takes seconds.
- Copies of `lzf_c.c`, `lzf_d.c`, `lzf.h`, `lzfP.h` — snapshots of the legacy
  sources, taken so a later legacy change cannot silently alter the oracle and
  invalidate a comparison. They are read-only reference copies.

## Parameters

Pinned via `/D` on the build command and asserted at compile time in
`lzf_ref.c`:

| macro | value | source |
| --- | --- | --- |
| `HLOG` | 16 | `lzfP.h` default |
| `VERY_FAST` | 1 | `lzfP.h` default |
| `ULTRA_FAST` | 0 | `lzfP.h` default |
| `CHECK_INPUT` | 1 | `lzfP.h` default |
| `INIT_HTAB` | 0 | `lzfP.h` default |
| `LZF_STATE_ARG` | 0 | `lzfP.h` default |
| `STRICT_ALIGN` | 0 | pinned explicitly; MSVC would default to 1 since it defines neither `__i386` nor `__amd64` |

`STRICT_ALIGN` was verified not to affect output: rebuilding with `=1` produced
identical bytes. It only selects the unaligned-read path.

## Known non-determinism

With `INIT_HTAB=0` and `LZF_STATE_ARG=0`, `lzf_c.c:107` declares `htab` as a
**local stack array** and the `memset` that would clear it sits inside
`#if INIT_HTAB` (`lzf_c.c:134-135`) — so it is never cleared. The legacy
therefore searches 512 KB of uninitialised stack.

The port allocates a fresh zeroed table per call instead. Empirically the two
still agree byte for byte on this corpus, because the guards at `lzf_c.c:155-157`
(`off < MAX_OFF` and `ref > in_data`) reject stale pointers that fall outside a
plausible earlier input position. That is luck as much as design, so treat the
legacy's compressed output as **not** a stable golden reference, and do not build
a fixture corpus out of bytes captured from it. The fixtures stay self-generated
and the differential test stays the real guarantee.