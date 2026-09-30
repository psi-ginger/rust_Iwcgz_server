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
| 2 | `ucChk` | 1 | Reserved — see below. |
| 3..5 | `usCmd` | 2 | Opcode, little-endian. |
| 5.. | payload | | |

`usSize` is **raw memory, not network order**: the legacy never applies `htons`
to packet data, only to socket addresses (blueprint 01 Part A §1.6), so
little-endian is what an x86 peer puts on the wire.

Limits: smallest frame 5 bytes, largest **64992** (`PACKET_MAX_SIZE - 8`,
`itzbase.h:45` and the arithmetic at `ITZRecvBuffer.cpp:172`).

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

All four are intentional and none changes a byte on the wire for a valid frame.

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
  5, 64992, 64993, 65535), the `Frame` accessors, sticky rejection, and the
  `SessionState` mapping to the `#define`s.
- `tests/framing.rs` pins the stream properties: byte-exact round trip at **every**
  split point of a frame, one byte at a time, 200 frames across eight chunk sizes,
  a frame at the receive limit, interleaved split/merge ordering, and that a
  truncated final frame is held rather than emitted.
- `tests/legacy_vectors.rs` replays **20 recorded cases from the legacy
  `ITZRecvBuffer`** and requires agreement on both the refusal decision and the
  exact frame bytes — except the three sub-header sizes, which are asserted to
  differ in the direction we want. See `reference/README.md` for how to
  regenerate.

## Not in this crate

Deliberately, because they belong elsewhere: the async/IO layer and session
management (later task), LZF decompression (`gz-lzf`, T-005), the cipher
(`gz-cipher`), and message codecs (`gz-proto`).
