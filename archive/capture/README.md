# archive/capture — the retired replay mode

**Not part of the build.** Nothing under `goonzu-rs/` compiles or imports this
directory; `Cargo.toml` does not reference it. It is kept out-of-tree for reference
only, and git history holds the full history of the live versions of these files.

## What was here

The **capture prologue mode** (`GAME_PROLOGUE=capture`), which replayed a recorded
reference session — `login_capture.pcapng` — to answer world entry instead of
generating it from the database. Retired 2026-10-09.

| file | was |
|---|---|
| `generated_capture.rs` | the replay plan (`REPLY_PLAN`), the prologue tail range, and `CAPTURED_CHAR_UNIQUE` |
| `generated_frames.rs` | the frame table `CAP_FRAMES` |
| `cap_s2c.bin` | the captured server→client payload blob (`CAP_S2C`, 92,059 bytes) |

## Why it was retired

The mode made the *reference session* look like a *server*. Two accounts logging in
saw the same world, and anything a player changed was undone at the next login. It
also pinned **every** session's `char_unique` to `0x05BF`, which made two-client
testing impossible: the two clients shared one character id, so each treated the
other's frames as its own (`bselfswitch`, `Client-Person-BigInfo.cpp:1160-1176`)
and wrote the other's life/mana into its own avatar (`:1228-1238`, unconditional).

The server now generates everything from PostgreSQL and has no dual-mode branching.

## What it was still good for — and where that went

`cap_s2c.bin` was also the **only measured ground truth** for frame lengths and
layouts, and 21 test sites used it as an oracle. Those measurements are immutable —
a wire layout does not change between runs — so they were extracted into named
constants before the blob was moved:

**`crates/gz-proto/src/captured_lengths.rs`**

Each constant cites the opcode and the byte count the capture carried, e.g.
`CAPTURED_ALL_CHAR_INFO_TWO_LEN = 10_960` with
`// Verified against official capture frame 249: 10,960 bytes`.

The bit *layouts* were never read from this blob — they were pinned separately by
tests that assert on actual offsets (`MakeCharInfo`'s packed dwords, the 200's
24-byte field map, the 249 item stride). Those assertions are unaffected.

## If you need it again

`cap_s2c.bin` is still here and still byte-exact. To read a frame out of it without
the retired module, the frame table is `generated_frames.rs` — `(cmd, offset, len)`
rows into `cap_s2c.bin` — so frame *i* is
`cap_s2c[offset .. offset + len]` with `(cmd, offset, len) = CAP_FRAMES[i]`.

The capture itself, `login_capture.pcapng`, lives outside the repository; the
recorder that produces one is `goonzu-rs/tools/capture/`, which is **still live
and still built** — only the *replay* half was retired, not the ability to record.
