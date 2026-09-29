# gz-cipher

Byte-exact port of the ITZ link cipher used on the client-facing links
(front `:5700`, game `:4010`). Task T-004; blueprint 01 Part A §2.

The shipped legacy client is unchanged, so this has to agree with
`NetworkLib\ITZNetLib\ITZEncrypt.cpp` byte for byte or nothing talks to us.
`CryptTool\` is a *different* cipher for asset files and is out of scope.

```rust
use gz_cipher::Cipher;

// client -> server: the negotiated-key instance
let mut tx = Cipher::session_cipher(key1, key2);
let wire = tx.encode(&packet)?;

// server -> client on the game link: the fixed-key instance
let mut rx = Cipher::link_cipher();          // 0x12, 0x34
let mut plain = vec![0u8; wire.len()];
if rx.decode(&wire, &mut plain)? { /* accepted */ }
```

## Wire format

A frame is the 5-byte `sPacketHeader` (`#pragma pack()`), so the layout is
fixed by the offset arithmetic in the cipher:

| Offset | Field | On the wire |
|---|---|---|
| 0..2 | `usSize` | little-endian total frame length, in the clear |
| 2 | `ucChk` | one-byte integrity value (see below) |
| 3..4 | `usCmd` | enciphered |
| 5.. | payload | enciphered |

Output length always equals input length, so the cipher never changes framing.
`usSize` stays readable in the clear, which is what lets `ITZRecvBuffer` cut
frames before any decryption happens.

## Semantics

**Both keys advance by one on every single call.** This is the property most
likely to be got wrong. The increment happens before any check, so it applies to
the `size <= 3` passthrough branch and to frames `decode` rejects, not just to
successfully enciphered packets. Peers stay in step only if they make the same
number of calls in the same order, so there is exactly one `Cipher` per
direction per connection and a dropped or extra call desynchronises the link
permanently. `set_key` is also how a desynchronised link is resynchronised,
because the keys *are* the counter.

**`size <= 3` is a verbatim copy.** Exactly `size` bytes are written, so a 0-byte
or 1-byte output is shorter than the size prefix a decode must read. The real
`usCmd` is at offset 3, so this branch is defensive and unreachable in practice.

**Offset 2 is an integrity slot, not a payload byte.** The sender overwrites it
with `plaintext[size-1] + key1`, and the receiver checks
`src[2] - key1 == plaintext[size-1]`. That is one byte, so roughly 1 in 256
single-byte corruptions slips through. It is a resync check, not a checksum, and
must not be relied on to detect corruption. The sender's own `ucChk` is never
transmitted as written.

**`decode` never writes output offset 2.** The legacy loop's first write is
`dest[3]`, so the decoded `ucChk` is whatever the caller's destination buffer
already held. Legacy callers pass a reused scratch buffer, meaning the `ucChk`
they go on to read is *stale data from the previous packet*. This port
reproduces that rather than hiding it; a caller that cares should zero the field
after decoding.

**Bytes past `usSize` are ignored.** A buffer may hold more than it declares.

**`encode` has no size ceiling but `decode` does.** `PACKET_MAX_SIZE` is 65000
(`itzbase.h` line 45), so `encode` will happily build a frame declaring up to
65535 that `decode` then refuses. Preserved deliberately: tidying it up would
accept frames the shipped peer never sends.

## Where encryption is enabled

Two independent `ITZEncrypt` instances exist per `ITZSession`
(`ITZSession.cpp` lines 13-14), and **each is used in exactly one direction**,
which is what keeps the two counters in step. Both are named in the card as
blueprint 01 Part A §2.2-§2.3.

| | `GetEncrypt()` | `GetEncrypt2()` |
|---|---|---|
| Key | negotiated from the server | fixed `0x12, 0x34` |
| Keyed by | client on `FRONTMSG_RESPONSE_SECRETKEY` (`Client-Winsock.cpp:228`), server at `char.cpp:5201` | `ITZSession::Init` (`ITZSession.cpp:56`) |
| Applied by | client `Encode` (`Client-Winsock.cpp:146,195,200,242`) | server `Encode` in `ITZSendBuffer::Write` (`ITZSendBuffer.cpp:124`) |
| Removed by | server `Decode` (`GameProtocol.cpp:76`, `FrontClientProtocol.cpp:85`) | client `Decode` (`Client-Winsock.cpp:872`) |
| Direction | client → server | server → client |

**The `Encrypt2` layer is only enabled on the game link, server → client.**
`ITZSessionFactory::CreateFactory` takes a `bEncrypt` flag that defaults to
`false`, and exactly two call sites in the whole tree pass `true`:
`GameProtocol.cpp` lines 25 and 27, both the game server's client-facing session
factory. Every other link — front, auth, DB, GM, agent — has it off, and
`ITZRecvBuffer` performs no decryption at all, so the layer is applied purely on
the send path.

Sending is gated on the key having arrived: `SendFrontServerMsg` and
`SendMsgToServer` return `FALSE` while `bFrontServerSecretKeySwitch` or
`bGameServerSecretKeySwitch` is false (`Client-Winsock.cpp:141,171`).

### Unknowns

- **No caller for `ITZSession::Init` was found in the tree.** That function is
  what installs `0x12,0x34` on the second instance, and the `ITZEncrypt`
  constructor is empty, so if `Init` is skipped the legacy link cipher runs on
  uninitialised memory. Confirm the rewrite's session setup always constructs the
  link cipher explicitly rather than relying on an init call.
- **`headerType == 1` framing is never combined with encryption.** That mode
  byte-swaps the length (`ntohs(...) + 8` in `ITZSendBuffer::Write`), while the
  cipher always reads and writes little-endian. The only encrypted sessions use
  `headerType 0`, so the combination is untested upstream; do not enable it
  without checking the byte order against a capture.
- **The front link looks plaintext server → client.** `FrontClientProtocol`'s
  session factory leaves `bEncrypt` off and no front-server code encodes with
  `GetEncrypt()`, while the client only decodes with `Encrypt2`. Verify against
  a capture before assuming the front link is unprotected in the rewrite too.
- Which legacy producer writes `FRONTMSG_RESPONSE_SECRETKEY`, and whether the key
  pair is ever reused across sessions, is outside this crate.

## Deviations from the legacy

The port is safe Rust, which is not free. All four differences are deliberate and
none changes the bytes on the wire.

1. **Keys start at zero.** The legacy constructor body is empty, so its keys are
   indeterminate until `SetKey`. Zero is this port's defined choice; prefer
   `session_cipher` / `link_cipher` over `new`.
2. **`decode` does not mutate its input.** The legacy rewrites the caller's
   source buffer at offset 2 (line 85). Doing it to a local leaves both the
   decrypted output and the accept flag identical, and keeps `&[u8]` inputs.
3. **Rejections are typed.** `decode` returns `Err(TooLarge)` for an oversize
   frame where the legacy returns `FALSE` for that and for an integrity failure
   alike. Callers that logged both as "bad packet" can now tell them apart.
4. **Length fields are validated.** A frame whose `usSize` overruns its buffer,
   or a destination shorter than `usSize`, is an error instead of the legacy's
   out-of-bounds read. All byte arithmetic is explicit `wrapping_add` rather than
   relying on C truncation.

## Testing

Two independent checks, because either alone is weak:

- `tests/vectors.rs` replays fixed vectors from `tests/data/vectors.rs`, which was
  produced by compiling the **byte-for-byte unmodified** legacy `ITZEncrypt.cpp`
  and recording its output. A round trip alone would pass for any mutually
  inverse pair of wrong implementations, so the bytes are pinned against the
  legacy itself.
- `tests/roundtrip.rs` covers the properties: every short length across the
  size-3/4 boundary, 250 random frames, a 200-packet burst through both ends,
  both key-setup paths, counter wraparound, and the fact that a misaligned
  counter is *rejected* rather than silently accepted.

`reference/` holds the C++ harness, the independent Python port used to confirm
it, and the recorded ground truth. See `reference/README.md` to regenerate.
