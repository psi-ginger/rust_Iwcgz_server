# Regenerating the framing ground truth

The table in `../tests/data/vectors.rs` is recorded from the legacy source
itself, not from this crate. A round trip through `gz-net`'s own tests would pass
for any mutually consistent pair of wrong implementations, so the parse decisions
and the frame boundaries are pinned against the code that shipped.

1. **The legacy C++**, compiled verbatim. `ITZRecvBuffer.cpp` / `.h` are used
   byte-for-byte; `itzbase.h` is stubbed because the real one pulls in
   `winsock2.h` / `windows.h` and MSVC-isms that `ITZRecvBuffer` never uses. The
   typedefs and `PACKET_MAX_SIZE` are copied across as-is.
2. **`emit_vectors.py`**, which parses the harness output into a Rust table.
   There is no independent re-implementation here, unlike `gz-cipher`'s
   `itzencrypt_ref.py`: framing is a length guard and a pointer walk, so the
   thing worth double-checking is the *facts*, and those come from the legacy
   binary. The emitter instead refuses to write an inconsistent fixture - see
   below.
3. **The Rust port** in `../src/lib.rs`, replayed by `../tests/legacy_vectors.rs`.

## Regenerate

Needs `g++` and Python 3. The legacy tree is read-only and is only ever read
from; copy the two files out rather than building in place.

```sh
LEGACY="/path/to/GoonZu_Source/NetworkLib/ITZNetLib"
cp "$LEGACY/ITZRecvBuffer.cpp" "$LEGACY/ITZRecvBuffer.h" .

g++ -O1 -w -o harness.exe harness.cpp ITZRecvBuffer.cpp
./harness.exe > ground_truth.json

python emit_vectors.py
rustfmt --edition 2024 ../tests/data/vectors.rs
```

`ground_truth.json` is an intermediate and is **not** committed; the committed
fixture is `vectors.rs`. Regenerate both together - a formatting drift in the
checked-in table is then always a real change.

`emit_vectors.py` refuses to emit a table when

- the harness output is truncated — no closing `]`, which is what a harness that
  died mid-run leaves behind. Without this the emitter would happily write a
  smaller fixture that still passes every test, so a crash would look like a
  legitimate deletion rather than a failed run;
- a case name repeats, or a case has no steps;
- a chunk or frame is odd-length hex, or the harness hit `RING_OVERFLOW`
  (which would mean the harness, not the legacy, got the ring geometry wrong);
- a recorded frame's length disagrees with its own declared size - i.e. the
  legacy would be reporting something self-inconsistent;
- a refused read handed out a frame, or any read was recorded after a refusal
  (the legacy caller disconnects, so those reads never happened);
- a name in `DIVERGENT` has no matching case, so the divergence allowlist cannot
  quietly stop covering what it claims to cover.

## Two lifecycle details the harness has to reproduce

Both cost a debugging session to find, so they are worth writing down:

- **`CreateRecvBuffer` does not set `m_pBeginPtr`.** It stays `NULL` from the
  constructor (`ITZRecvBuffer.cpp:8`), because `CreateRecvBuffer` (`:47-71`) sets
  every other pointer. `ITZSession::SetSocket` calls `ClearBuffer()` before the
  session goes `CONNECTED` (`ITZSession.cpp:181`), and `_closesocket` calls it
  again on close (`:305`) because sessions are pooled and reused. Call
  `CreateRecvBuffer` without `ClearBuffer` and `GetFirstPacket` dereferences
  `NULL`. The harness calls both, in that order.
- **A refused `Completion` is a dead session.** `recv_completion` sets
  `SESSION_STATE_DISCONNECTED` on `FALSE` (`ITZNetIOCP.cpp:145-150`), so the
  caller stops feeding the buffer. The harness stops too; draining a refused
  buffer would measure a session that no longer exists.

## What the harness covers

21 cases, 52 reads, chosen to hit every branch and both boundaries:

- **accepted frames** — single frame; two frames in one chunk; one frame split
  across three chunks; one byte at a time (25 reads); the two size bytes and the
  command split across three reads; an empty read before and after a frame; a
  frame followed by an incomplete frame.
- **held, not refused** — a legal size whose body has not arrived, at both ends
  of the range. This is the important half of the contract: a slow peer must not
  be disconnected.
- **the low end** — declared sizes 0, 1, 2, 3, 4 and 5. Sizes 0 and 1 are
  refused by the legacy's own `usPacketSize < 2` guard (`:163`); **2, 3 and 4 are
  accepted**, which is the BUG-013 divergence, measured rather than remembered.
- **the high end** — 64993 and 65535, both refused (`:172`).
- **a refusal is not a skip** — a legal frame followed by an over-limit header.
  The legacy delivers nothing at all, because the good frame is behind the bad
  header in the same buffer. This is the case that makes eager validation in
  `feed` the right design rather than a stylistic choice.
- **a frame larger than the ring** — the one case that does *not* use the big
  default ring. `ring_smaller_than_frame` sets the ring to 32 bytes and sends a
  40-byte frame in two reads, so the second read wraps `m_pLastPtr` over the
  first. The legacy delivers `0500aabbcc`, a 5-byte "frame" assembled from the
  second read's bytes, which is BUG-014 and nothing like the frame that was sent.
  The geometry is not invented: `CreateFactory` passes 30000/50000 for the game
  client's link (`GameProtocol.cpp:25,27`) while the guard allows 64992.

Not covered here, on purpose:

- **A full-length frame.** A 64992-byte frame is 130 KB of hex, which is not
  worth carrying in a source fixture. The decision that 64992 is *accepted* is
  pinned by the short-body case; that a full frame is *deliverable* end to end is
  covered by `../tests/framing.rs`.
- **A frame that straddles the ring end.** The legacy linearises one by copying
  it into the `PACKET_MAX_SIZE`-byte slack zone before `m_pBasePtr`
  (`:250-257`). `gz-net` buffers in a `VecDeque` and has no ring, so there is
  nothing to linearise, and the copy exists only to paper over the wrap. Ring
  *overflow* — a frame that does not fit at all — is covered by
  `ring_smaller_than_frame` above; this entry is only about the wrap itself.
- **`headerType == 1`.** No call site in the tree passes it (blueprint 01 Part A
  §1.6). Every case runs with the default 0.
