# gz-net

Byte-level packet framing shared by every `ITZNetLib` link. Task T-006;
blueprint 01 Part A §1.

```rust
use gz_net::{Frame, FrameParser};

// Build a frame to send (plain link: the framing layer never encrypts).
let frame = Frame::new(cmd, &payload)?;
socket.write_all(frame.as_bytes())?;

// Read: feed whatever the socket handed over, pull whole frames out.
let mut parser = FrameParser::new();
parser.feed(&chunk)?;                       // Err = disconnect, see below
while let Some(frame) = parser.next_frame()? {
    dispatch(frame.cmd(), frame.payload());
}
```

## Wire format

The 5-byte `sPacketHeader` (`itzbase.h:63-68`, `#pragma pack(1)`), then a payload:

| Offset | Field | Bytes | Meaning |
|---|---|---|---|
| 0..2 | `usSize` | 2 | **Total** frame length, header included. Little-endian. |
| 2 | `ucChk` | 1 | Reserved?" see below. |
| 3..5 | `usCmd` | 2 | Opcode, little-endian. **Bit 15 is the LZF flag** — see below. |
| 5.. | payload | | | |

`usSize` is **raw memory, not network order**: the legacy never applies `htons`
to packet data, only to socket addresses (blueprint 01 Part A A1.6), so
little-endian is what an x86 peer puts on the wire.

Limits: smallest frame 5 bytes, largest **64992** (`PACKET_MAX_SIZE - 8`,
`itzbase.h:45` and the arithmetic at `ITZRecvBuffer.cpp:172`).

**These are frame-size limits, not a promise about capacity.** The legacy backs
each link with a ring whose size is chosen per link, and the largest frame the
size guard accepts can exceed the smallest such ring — see *Ring capacity* in
Deliberate divergences below.

## `ucChk` is reserved, not a checksum

`Frame::integrity()` is offset 2. It is deliberately not named `chk`:

- No legacy producer ever writes it on a plain link, and nothing ever validates
  it. A tree-wide search for `ucChk` finds four references, and the only two that
  read it read the **raw source** buffer, not a decoded one.
- On the encrypted game link `gz-cipher` overwrites it with a one-byte resync tag
  (`plaintext[size-1] + key1`), which catches roughly 1 in 256 single-byte
  corruptions. Filed as BUG-012 in `known_legacy_bugs.md`.

`Frame::new` writes zero. Do not build integrity on it; if real integrity is
wanted, that is its own decision (see SEC-014, which also says the link cipher
is obfuscation and not a security boundary).

## Bit 15 of `usCmd` is the LZF flag, not part of the opcode

`usCmd` is a `USHORT`, so the legacy spent its top bit as a side channel:
`GameProtocol.cpp:109` tests `usCmd & 0x8000` and, when set, runs the payload
through LZF before dispatch. It only clears the bit afterwards, at `:141`
(`usCmd -= 0x8000`), in the same block that installs the decompressed payload.
The opcode id itself is always below 32768, so the bit is otherwise free.

Framing does not touch it, but three accessors make the trap explicit:

| Call | Returns |
|---|---|
| `frame.cmd()` | the raw `usCmd`, flag and all |
| `frame.compressed()` | whether bit 15 is set |
| `frame.cmd_id()` | `cmd()` with bit 15 cleared |

`cmd_id()` is what `gz_proto::*::from_id` expects, and it is **only correct once
the payload has been decompressed** — on a still-compressed frame it reports an
opcode that does not describe the bytes you are holding. On an uncompressed frame
it is exactly `cmd()`. Until `gz-lzf` exists (T-005) nothing in this workspace can
produce a flagged frame, which is exactly why the flag was easy to overlook.

## What `cmd` and `payload` are worth on each link

Framing happens **before** decryption — `ITZRecvBuffer` has no decode step, and
the cipher never changes a frame's length. So on the game client link the cipher
enciphers everything from offset 3 onwards and `Frame::cmd()` / `payload()` read
**ciphertext** until the frame has been through `gz_cipher::Cipher::decode`.

| Link | `cmd` / `payload` as parsed |
|---|---|
| front (`:5700`), auth, agent, DB, front↔game, front↔auth | plaintext — usable directly |
| game client link (`:4010`) | ciphertext — decode the frame first |

The cipher hook is the seam: run `gz_cipher` over `frame.as_bytes()` and read the
fields from the result. Only the game server's two client-facing session
factories pass `bEncrypt = true` (blueprint 01 Part A §1.6).

## Limits, and what a rejection means

`validate_size` is the whole guard table, in one place:

| Declared size | Legacy recv buffer | `gz-net` |
|---|---|---|
| 0, 1 | refused (`usPacketSize < 2`, `:163`) | refused — `SizeBelowMinimum` |
| 2, 3, 4 | **accepted** | refused — `SizeBelowHeader` |
| 5 ..= 64992 | accepted | accepted |
| 64993 ..= 65535 | refused (`> PACKET_MAX_SIZE - 8`, `:172`) | refused — `SizeTooLarge` |

**Every rejection means disconnect.** The legacy turns `Completion`'s `FALSE`
into `SESSION_STATE_DISCONNECTED` (`ITZNetIOCP.cpp:145-150`) — it is not a
drop-this-packet-and-carry-on. `gz-net` matches that, with one addition: the
rejection is **sticky**. A rejected parser keeps returning the original error, so
a caller that ignores it by accident cannot resynchronise onto attacker-chosen
bytes.

The 2..=4 row is the only deliberate behavioural difference, and it is
deliberate: the header is 5 bytes and `usCmd` lives at offsets 3..4, so a frame
that declares 3 bytes has no room for a command id. `ITZEncrypt::Decode` has the
matching hole one layer down — a declared size `<= 3` is copied verbatim and
accepted with no verification at all (`ITZEncrypt.cpp:79-83`). BUG-013 asks the
framing layer to draw the line, and this is that line. The two sub-cases are
separate error variants so a log can tell "the legacy would also have refused
this" apart from "the legacy would have accepted garbage".

`feed` validates as soon as a header's 2 bytes have arrived, not when the frame
completes, so a peer cannot get frame *n* delivered by promising frame *n+1* in
the same read. The `good_then_over_limit` case in the legacy fixture pins the
legacy doing the same thing.

## Deliberate divergences from the legacy

All five are intentional and none changes a byte on the wire for a valid frame.

1. **Frames below 5 bytes are refused** (above). Measured against the legacy in
   `tests/legacy_vectors.rs`, not assumed.
2. **A header is only read once its 2 bytes are in hand.** The legacy stops one
   byte earlier (`m_pCompletePtr == m_pLastPtr - 1`, `:149-151`) but then reads
   a stale byte when a read lands exactly on a frame boundary. That path is
   reachable only when `siRecvdBytes == 0`, i.e. a graceful close, where the
   resulting refusal is benign — so this removes a latent stale read without
   ever accepting less.
3. **Rejections are typed.** The legacy returns `BOOL` and cannot distinguish
   "bad size" from "ring overflow" from "integrity failed" three layers up.
4. **A `VecDeque` replaces the ring buffer.** The legacy needs a
   `PACKET_MAX_SIZE`-byte slack zone before `m_pBasePtr` to linearise frames that
   straddle the ring end (`:250-257`); a deque has no wrap, so there is nothing
   to linearise. `clear()` covers `ClearBuffer`.
5. **Ring capacity cannot corrupt a frame.** This one is a legacy bug (BUG-014)
   that is measured, not assumed. The ring's size is chosen per link:
   `CreateFactory` is called with 30000/50000 for the game client's link
   (`GameProtocol.cpp:25,27`), while the size guard accepts frames up to 64992.
   `GetRecvParam` then wraps `m_pLastPtr` to `m_pBasePtr` once the ring is full
   and nothing has been consumed (`ITZRecvBuffer.cpp:78-89`), overwriting the
   front of a half-received frame, so the *next* size it reads comes from later
   bytes. The `ring_smaller_than_frame` fixture case pins it exactly: a 40-byte
   frame into a 32-byte ring arrives as a bogus 5-byte frame built out of the
   second read.

   `gz-net` has no ring, so the caller now owns that bound: **a link must be able
   to hold one maximum-size frame (64992 bytes) before it is drained.** `feed`
   copies every byte it is handed and never evicts, so draining after each
   `feed` makes the requirement vanish; a caller that buffers more than that
   before draining is its own ring, and needs to be at least that big.

`headerType == 1` (byte-swapped size, `+8`) is **not** implemented: no call site
anywhere in the tree passes it, and the only non-default arguments to
`CreateFactory` in the whole repository are `0, true` on the game server's
client-facing factories. It is dead code from another product of the library
vendor, and implementing it would only invite an untested byte-order combination.

## Sessions

`SessionState` mirrors the four `#define`s in `ITZSession.h:6-9` and nothing
more. The numeric form is exposed via `as_legacy` / `from_legacy` so a value can
be traced back to the macro; there is deliberately no `Session` type yet, because
the async/IO layer is a later task and a half-built one would be dead code.

| State | Set by | `is_active` |
|---|---|---|
| `None` | a pool slot with no socket | no |
| `Connected` | accept, or `connect` issued — `ITZSession::SetSocket` (`ITZSession.cpp:184`) | no |
| `Established` | first receive posted, `SESSION_STATE_ESTABLISHED` | yes |
| `Disconnected` | terminal | no |

Two more lifecycle rules the legacy relies on, recorded here because they are easy
to miss and are not visible in `ITZRecvBuffer`:

- **`CreateRecvBuffer` does not set `m_pBeginPtr`** — it stays `NULL`. The session
  calls `ClearBuffer` before going `CONNECTED` (`ITZSession.cpp:181`) and again
  on close (`:305`), because sessions are pooled and reused.
- **Server-side handshake timeout is 10 s** for a session that never reaches
  `ESTABLISHED` (`ITZNetIOCP.cpp:41-49`). Not implemented yet; noted so it is not
  forgotten.

## Testing

Three independent layers, because any one alone is weak:

- `src/lib.rs` unit tests pin the guard table (every boundary value 0, 1, 2, 3, 4,
  5, 64992, 64993, 65535), the `Frame` accessors including the compression-flag
  split, sticky rejection, and the `SessionState` mapping to the `#define`s.
- `tests/framing.rs` pins the stream properties: byte-exact round trip at **every**
  split point of a frame, one byte at a time, 200 frames across eight chunk sizes,
  a frame at the receive limit, interleaved split/merge ordering, and that a
  truncated final frame is held rather than emitted.
- `tests/legacy_vectors.rs` replays **21 recorded cases from the legacy
  `ITZRecvBuffer`** and requires agreement on both the refusal decision and the
  exact frame bytes — except the three sub-header sizes (which are asserted to
  differ in the direction we want) and the over-ring frame (where it asserts this
  port delivers what the peer actually sent). Every divergent case is pinned by
  name and reason, so widening the allowlist fails the build. See
  `reference/README.md` for how to regenerate.

## Not in this crate

Deliberately, because they belong elsewhere: the async/IO layer and session
management (later task), LZF decompression (`gz-lzf`, T-005), the cipher
(`gz-cipher`), and message codecs (`gz-proto`).
