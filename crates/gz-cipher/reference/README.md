# Regenerating the cipher test vectors

The vectors in `../tests/data/vectors.rs` are ground truth from the legacy
source itself, not from this crate. Three independent implementations have to
agree before they are trusted:

1. **The legacy C++**, compiled verbatim. The only substitution is `itzbase.h`,
   which is stubbed because the real one pulls in `winsock2.h`/`windows.h` and
   MSVC-isms that `ITZEncrypt` never uses. The typedefs and `PACKET_MAX_SIZE`
   are copied across as-is, and `ITZEncrypt.cpp` / `ITZEncrypt.h` are used
   byte-for-byte.
2. **`itzencrypt_ref.py`**, an independent Python translation of the C. It
   exists to catch a misreading in either the harness or the port.
3. **The Rust port** in `../src/lib.rs`, checked by `../tests/vectors.rs`.

## Regenerate

Needs `g++` and Python 3. The legacy tree is read-only and is only ever read
from; copy the two files out rather than building in place.

```sh
LEGACY="/path/to/GoonZu_Source/NetworkLib/ITZNetLib"
cp "$LEGACY/ITZEncrypt.cpp" "$LEGACY/ITZEncrypt.h" .

g++ -O1 -w -o harness.exe harness.cpp ITZEncrypt.cpp
./harness.exe > ground_truth.json          # optional: eyeball the diff first

# Confirm the independent Python port agrees with the compiled legacy.
python itzencrypt_ref.py
# -> 27 cases / 40 ops cross-checked, 0 mismatches

# Rewrite the Rust table, then format it so `cargo fmt --check` stays clean.
python itzencrypt_ref.py --emit-rs > ../tests/data/vectors.rs
rustfmt --edition 2024 ../tests/data/vectors.rs
```

Expect the harness output and the Rust table to be regenerated together. A
mismatch between steps 1 and 2 means the *understanding* of the algorithm is
wrong, which is the failure this double check exists to catch. The generator plus
`rustfmt` reproduces the committed `vectors.rs` byte for byte, so a formatting
drift in the checked-in fixture is always a real change.

## What `harness.cpp` covers

27 cases chosen to hit every branch and every boundary the legacy has:

- **A, passthrough** — sizes 0, 1, 2, 3, and a case proving a 3-byte passthrough
  still ticks the keys, which is the easiest rule to break.
- **B, cipher branch** — sizes 4, 5, 8, 64, i.e. the first size that reaches the
  loop and a realistic packet.
- **C, the `0x12,0x34` link key** — encode, a 16-byte packet, the same plaintext
  twice to show the counter moving, round trips, a three-packet burst, and a
  buffer with a byte past `usSize`.
- **D, wraparound** — `0xFF` rolling to `0x00`, across calls, and a round trip.
- **E, decode** — valid, corrupted payload, oversize, passthrough, wrong key, and
  a corrupted integrity slot.

Each decode runs against a *second* cipher instance keyed identically, the way a
real client and server each hold one, and the destination buffer is pre-filled
with `0xAA` so the offset-2 behaviour is visible rather than incidental. The
harness also asserts no call writes past `size` (`overrun` in the JSON).

Two things are intentionally not in the generated table and are written by hand
in `../tests/vectors.rs` instead: the oversize frame, which the legacy rejects
without writing anything, and `encode`'s missing size ceiling.

## `itzencrypt_ref.py`

Also emits the table, and cross-checks it. `--max-hex` skips any call whose
buffers are too large to keep in a source file; the only call it drops is the
oversize one described above.
