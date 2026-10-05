//! Byte-level packet framing shared by every `ITZNetLib` link
//! (blueprint 01 Part A §1, task T-006).
//!
//! Legacy source: `NetworkLib\ITZNetLib\itzbase.h`, `ITZRecvBuffer.{h,cpp}`,
//! `ITZSendBuffer.cpp`, `ITZSession.h`. A frame is the 5-byte `sPacketHeader`
//! followed by a payload:
//!
//! ```text
//! offset 0..2   usSize    little-endian u16, TOTAL frame length, header included
//! offset 2      ucChk     reserved; the link cipher's integrity slot
//! offset 3..5   usCmd     little-endian u16 opcode; bit 15 is the LZF flag
//! offset 5..    payload
//! ```
//!
//! Bit 15 of `usCmd` is the LZF compression flag, not part of the opcode — see
//! [`Frame::cmd`]. A `VecDeque` has no ring, so there is no per-link capacity
//! limit either; see the capacity note on [`FrameParser`].
//!
//! The size field is **raw memory, not network order** — the legacy never applies
//! `htons` to packet data, only to socket addresses (blueprint 01 Part A §1.6), so
//! little-endian is what a peer on x86 puts on the wire.
//!
//! [`FrameParser`] turns an arbitrarily chunked byte stream into frames and is the
//! only stateful piece here. It is synchronous and dependency-free, so an async
//! layer can sit on top and T-009 can fuzz [`validate_size`] and the parser
//! directly.
//!
//! [`session`] is the other half: the `ITZSession` state machine and the three
//! legacy timings (10 s handshake, 7 s outbound connect, 600 ms close throttle)
//! that T-006 left as notes and T-016 enforces. It is policy only — no I/O — so
//! it stays dependency-free too.
//!
//! # Deliberate divergence: a frame must be able to hold its own header
//!
//! The legacy recv guard is `usPacketSize < 2` (`ITZRecvBuffer.cpp:163`), so it
//! accepts a declared size of 2, 3 or 4 and hands the caller a "packet" with no
//! room for `usCmd`. The cipher has the matching hole one layer down: a declared
//! size `<= 3` is copied verbatim and accepted **with no verification at all**
//! (`ITZEncrypt.cpp:79-83`, BUG-013). This port rejects anything below
//! [`HEADER_SIZE`], which is where BUG-013 asks the framing layer to draw the line.
//! The two sub-cases are separate error variants so the divergence is visible in
//! the type rather than hidden in a single `< 5` check — see [`FrameError`].
//!
//! # `headerType == 1` is not implemented
//!
//! The legacy buffers can byte-swap the size field and add 8 to it
//! (`ITZRecvBuffer.cpp:140,157,246,279`, `ITZSendBuffer.cpp:134-138`). **No call
//! site anywhere in the tree passes `headerType = 1`** — the only non-default
//! arguments to `CreateFactory` in the whole repository are `0, true` on the game
//! server's two client-facing session factories (blueprint 01 Part A §1.6). It is
//! dead code retained for another product of the library vendor, and implementing
//! it here would only invite an untested byte-order combination.

/// The `ITZSession` lifecycle: states, the three legacy timings, and
/// "a rejection is a disconnect". See the module docs for the legacy citations
/// and for the one place the window differs from the source.
pub mod session;

pub use session::{
    CLOSE_THROTTLE, CONNECT_TIMEOUT, HANDSHAKE_TIMEOUT, Rejection, Session, SessionTimeouts, Tick,
};

/// `sizeof(sPacketHeader)`: `UI16 usSize` + `UI08 ucChk` + `UI16 usCmd` under
/// `#pragma pack(1)` (`itzbase.h:63-68`).
///
/// Also the smallest frame this port will accept — see the module docs.
pub const HEADER_SIZE: usize = 5;

/// `PACKET_MAX_SIZE`, `itzbase.h:45`.
pub const PACKET_MAX_SIZE: usize = 65000;

/// Largest frame the receive path accepts: `PACKET_MAX_SIZE - 8`, the literal
/// arithmetic at `ITZRecvBuffer.cpp:172`.
pub const MAX_FRAME_SIZE: usize = PACKET_MAX_SIZE - 8;

/// Bit 15 of `usCmd`: the LZF compression flag.
///
/// Set by the legacy producer on a compressed frame; cleared by the consumer
/// *after* decompressing (`GameProtocol.cpp:141`). See [`Frame::compressed`].
pub const COMPRESSION_FLAG: u16 = 0x8000;

/// Why a frame could not be accepted.
///
/// The legacy returns `BOOL` from `ITZRecvBuffer::Completion` and the caller
/// turns `FALSE` into `SESSION_STATE_DISCONNECTED` (`ITZNetIOCP.cpp:145-150`), so
/// **every** variant here means "kill the connection", not "drop this packet".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// Declared size below 2. The legacy rejects this too (`ITZRecvBuffer.cpp:163`).
    SizeBelowMinimum {
        /// Size taken from the frame header.
        declared: u16,
    },
    /// Declared size of 2..=4: too small to hold the 5-byte header, but *above*
    /// the legacy's own `< 2` guard, so a legacy peer would have accepted and
    /// delivered this frame with no room for `usCmd`.
    ///
    /// Rejected here per BUG-013. Same disconnect consequence as the variants
    /// above; it is a separate variant so a capture or log can tell "the legacy
    /// would also have refused this" apart from "the legacy would have accepted
    /// garbage".
    SizeBelowHeader {
        /// Size taken from the frame header.
        declared: u16,
    },
    /// Declared size above [`MAX_FRAME_SIZE`]. The legacy rejects this too
    /// (`ITZRecvBuffer.cpp:172`).
    SizeTooLarge {
        /// Size taken from the frame header.
        declared: u16,
    },
    /// A locally built frame's payload does not fit a frame a peer will accept.
    PayloadTooLarge {
        /// Payload length that was asked for.
        len: usize,
    },
}

impl FrameError {
    /// The declared size that caused the rejection, if the error came from a header.
    #[must_use]
    pub const fn declared(&self) -> Option<u16> {
        match self {
            Self::SizeBelowMinimum { declared }
            | Self::SizeBelowHeader { declared }
            | Self::SizeTooLarge { declared } => Some(*declared),
            Self::PayloadTooLarge { .. } => None,
        }
    }
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SizeBelowMinimum { declared } => write!(
                f,
                "frame declares {declared} bytes, below the legacy minimum of 2"
            ),
            Self::SizeBelowHeader { declared } => write!(
                f,
                "frame declares {declared} bytes, too small to hold the {HEADER_SIZE}-byte header"
            ),
            Self::SizeTooLarge { declared } => write!(
                f,
                "frame declares {declared} bytes, above the {MAX_FRAME_SIZE}-byte receive limit"
            ),
            Self::PayloadTooLarge { len } => write!(
                f,
                "payload of {len} bytes does not fit a {MAX_FRAME_SIZE}-byte frame"
            ),
        }
    }
}

impl std::error::Error for FrameError {}

/// The one place the receive guards live, shared by [`FrameParser::feed`] and
/// [`FrameParser::next_frame`] so they cannot drift.
///
/// Exposed on its own so T-009 can fuzz the guard table directly.
///
/// # Errors
///
/// [`FrameError::SizeBelowMinimum`] for a declared size below 2 (the legacy's own
/// guard), [`FrameError::SizeBelowHeader`] for 2..=4 (rejected here, accepted by
/// the legacy — BUG-013), and [`FrameError::SizeTooLarge`] above
/// [`MAX_FRAME_SIZE`].
#[inline]
pub const fn validate_size(declared: u16) -> Result<(), FrameError> {
    if declared < 2 {
        return Err(FrameError::SizeBelowMinimum { declared });
    }
    if declared < HEADER_SIZE as u16 {
        return Err(FrameError::SizeBelowHeader { declared });
    }
    if declared > MAX_FRAME_SIZE as u16 {
        return Err(FrameError::SizeTooLarge { declared });
    }
    Ok(())
}

/// One framed packet: the header and its payload, exactly as it will be or was
/// on the wire.
///
/// The header fields are derived from the bytes rather than stored alongside them,
/// so there is no way to build a `Frame` whose `size` disagrees with its length —
/// which is the class of bug this crate exists to rule out (BUG-012/BUG-013). The
/// observable surface is the legacy's: total `size`, a reserved byte, `cmd` and
/// the payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    bytes: Vec<u8>,
}

impl Frame {
    /// Builds a frame for `cmd` and `payload`, with the reserved byte zeroed.
    ///
    /// # Errors
    ///
    /// [`FrameError::PayloadTooLarge`] when `HEADER_SIZE + payload.len()` exceeds
    /// [`MAX_FRAME_SIZE`]. Bounding a locally built frame by the same limit the
    /// parser enforces keeps us from emitting something a peer would refuse.
    pub fn new(cmd: u16, payload: &[u8]) -> Result<Self, FrameError> {
        let len = HEADER_SIZE + payload.len();
        if len > MAX_FRAME_SIZE {
            return Err(FrameError::PayloadTooLarge { len: payload.len() });
        }

        let mut bytes = Vec::with_capacity(len);
        bytes.extend_from_slice(&(len as u16).to_le_bytes());
        bytes.push(0);
        bytes.extend_from_slice(&cmd.to_le_bytes());
        bytes.extend_from_slice(payload);
        Ok(Self { bytes })
    }

    /// The little-endian `usSize` read straight out of the header.
    ///
    /// Includes the header, so it equals [`Frame::wire_len`].
    #[must_use]
    pub fn size(&self) -> u16 {
        declared_size([self.bytes[0], self.bytes[1]])
    }

    /// Offset 2, the legacy's `ucChk`.
    ///
    /// Deliberately *not* named `chk`, because it is not a checksum. No legacy
    /// producer ever writes it on a plain link and nothing ever validates it; on
    /// the encrypted game link it is overwritten by `gz-cipher` with a one-byte
    /// resync tag (BUG-012). Treat it as reserved: do not build integrity on it.
    #[must_use]
    pub fn integrity(&self) -> u8 {
        self.bytes[2]
    }

    /// The little-endian `usCmd` opcode.
    ///
    /// **Bit 15 (`0x8000`) is the LZF compression flag, not part of the opcode.**
    /// The legacy tests it before decompressing (`GameProtocol.cpp:109`,
    /// `Client-Winsock.cpp:896`) and clears it *afterwards* with
    /// `usCmd -= 0x8000` (`GameProtocol.cpp:141`). Every real opcode is below
    /// `0x8000`, which `gz-proto` asserts for all three tables.
    ///
    /// So the flag is the **decompressing consumer's** to clear, not the framing
    /// layer's: masking it here would silently mislabel a compressed frame. Use
    /// [`Frame::compressed`] to branch, and [`Frame::cmd_id`] to get the opcode
    /// once the payload has been decompressed.
    ///
    /// Passing this straight to `gz_proto::*::from_id` returns `None` for every
    /// compressed packet — that is the trap this doc exists to prevent.
    ///
    /// **Also only meaningful on an unencrypted link.** On the game client link the
    /// cipher enciphers everything from offset 3 onwards, so this reads ciphertext
    /// unless the frame has already been through `gz_cipher::Cipher::decode`.
    #[must_use]
    pub fn cmd(&self) -> u16 {
        u16::from_le_bytes([self.bytes[3], self.bytes[4]])
    }

    /// Bit 15 of [`Frame::cmd`]: the legacy's LZF compression flag.
    ///
    /// Only answerable on a decrypted link. When this is set, the payload is
    /// LZF-compressed and the opcode is still `cmd() & !0x8000`.
    #[must_use]
    pub fn compressed(&self) -> bool {
        self.cmd() & COMPRESSION_FLAG != 0
    }

    /// The opcode with the compression flag cleared, i.e. what
    /// `gz_proto::*::from_id` expects.
    ///
    /// **Only correct once the payload has been decompressed.** The legacy clears
    /// the flag at the same point it replaces the payload
    /// (`GameProtocol.cpp:140-141`), so calling this on a still-compressed frame
    /// hands back an opcode that does not describe the bytes you hold. On an
    /// *uncompressed* frame it is exactly [`Frame::cmd`].
    #[must_use]
    pub fn cmd_id(&self) -> u16 {
        self.cmd() & !COMPRESSION_FLAG
    }

    /// Bytes after the header.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.bytes[HEADER_SIZE..]
    }

    /// The whole frame, header included — what goes on the wire.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Frame length in bytes, header included.
    #[must_use]
    pub fn wire_len(&self) -> usize {
        self.bytes.len()
    }

    /// Consumes the frame and returns its wire bytes.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// The declared size read from the two `usSize` bytes. Callers must have checked
/// that both bytes are present.
fn declared_size(bytes: [u8; 2]) -> u16 {
    u16::from_le_bytes(bytes)
}

/// The legacy session states, `ITZSession.h:6-9`.
///
/// The `as_legacy` / `from_legacy` pair exists so a value can be traced back to
/// the `#define` it came from; nothing in this crate needs the numeric form.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SessionState {
    /// `SESSION_STATE_NONE` — the slot exists, no socket.
    #[default]
    None,
    /// `SESSION_STATE_CONNECTED` — socket accepted or connect() issued, buffers
    /// not yet armed.
    Connected,
    /// `SESSION_STATE_ESTABLISHED` — the first receive is posted; frames may flow.
    Established,
    /// `SESSION_STATE_DISCONNECTED` — terminal.
    Disconnected,
}

impl SessionState {
    /// The `#define` value from `ITZSession.h:6-9`.
    #[must_use]
    pub const fn as_legacy(self) -> i32 {
        match self {
            Self::None => 0,
            Self::Connected => 1,
            Self::Established => 2,
            Self::Disconnected => 3,
        }
    }

    /// The `#define` value from `ITZSession.h:6-9` back to a state. `None` for
    /// anything outside `0..=3`.
    #[must_use]
    pub const fn from_legacy(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::None),
            1 => Some(Self::Connected),
            2 => Some(Self::Established),
            3 => Some(Self::Disconnected),
            _ => None,
        }
    }

    /// Whether frames may flow. The legacy gates its per-tick `SessionProc` walk
    /// on exactly this (`ITZNetworkThread.cpp`).
    #[must_use]
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Established)
    }
}

/// Stream → frame conversion.
///
/// Feed it whatever `read` handed you; pull whole frames out with
/// [`FrameParser::next_frame`]. A frame split across any number of chunks and
/// several frames sharing one chunk are both handled — the legacy has the same
/// property, it just does it with a ring buffer and a `PACKET_MAX_SIZE`-byte
/// slack zone (`ITZRecvBuffer::CreateRecvBuffer`, `ITZRecvBuffer.cpp:56-58`)
/// instead of a deque.
///
/// # Capacity: no ring means no second ceiling
///
/// The legacy's per-link receive ring is a *second* limit that its size guard
/// never sees. `GetRecvParam` wraps `m_pLastPtr` to `m_pBasePtr` whenever the
/// ring is full and nothing has been consumed (`ITZRecvBuffer.cpp:78-89`), which
/// overwrites a half-received frame — so the game client link, whose recv buffer
/// is only 30000/50000 bytes (`GameProtocol.cpp:25,27`), corrupts any frame
/// larger than its ring even though 64992 is inside the size guard. Recorded as
/// BUG-014.
///
/// Two consequences for callers:
/// - **Size each link's buffer from [`MAX_FRAME_SIZE`]**, not from the legacy's
///   per-link numbers, or we inherit the corruption.
/// - [`FrameParser::feed`] does **not** cap how much it will buffer. Buffering is
///   bounded by [`MAX_FRAME_SIZE`] *provided the caller drains frames*; a caller
///   that only ever calls `feed` grows without limit, where the legacy refuses
///   once its ring fills.
///
/// # Rejection is sticky
///
/// A malformed header is a disconnect, not a dropped packet, so a rejected parser
/// stays rejected: [`feed`](FrameParser::feed) and
/// [`next_frame`](FrameParser::next_frame) keep returning the original error
/// rather than resuming on the next plausible-looking byte. Callers are expected
/// to drop the connection, and this makes ignoring the error safe.
///
/// # Thread safety
///
/// Not shared. The legacy relied on the enclosing session's critical section for
/// the same reason; give each connection its own parser.
#[derive(Debug, Default)]
pub struct FrameParser {
    buf: std::collections::VecDeque<u8>,
    failure: Option<FrameError>,
}

impl FrameParser {
    /// An empty parser.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buf: std::collections::VecDeque::new(),
            failure: None,
        }
    }

    /// Adds received bytes and validates every header that is now readable.
    ///
    /// Mirrors `ITZRecvBuffer::Completion`: the header is checked as soon as its
    /// 2 bytes have arrived, not when the whole frame has, so a peer cannot get
    /// frame *n* delivered by promising frame *n+1*.
    ///
    /// Feeding an empty chunk is a no-op. Feeding bytes into a rejected parser
    /// returns the original error.
    ///
    /// # Errors
    ///
    /// Whatever [`validate_size`] returns for the first header in the buffer that
    /// fails, after which the parser is permanently rejected.
    pub fn feed(&mut self, chunk: &[u8]) -> Result<(), FrameError> {
        if let Some(err) = self.failure {
            return Err(err);
        }
        self.buf.extend(chunk);
        self.scan()
    }

    /// Walks the buffered frames the way `ITZRecvBuffer::Completion` does, moving
    /// `m_pCompletePtr` forward frame by frame.
    ///
    /// A header is only read once its 2 bytes are in hand, and the walk stops at
    /// the last frame that is entirely buffered. The legacy stops one byte earlier
    /// (`m_pCompletePtr == m_pLastPtr - 1`, `ITZRecvBuffer.cpp:149-151`) but then
    /// reads a stale byte when it lands exactly on a boundary; requiring the full
    /// 2 here avoids that without ever accepting less.
    fn scan(&mut self) -> Result<(), FrameError> {
        let mut at = 0usize;
        while self.buf.len() - at >= 2 {
            let declared = declared_size([self.buf[at], self.buf[at + 1]]) as usize;
            if let Err(err) = validate_size(declared as u16) {
                self.failure = Some(err);
                return Err(err);
            }
            if self.buf.len() - at < declared {
                break;
            }
            at += declared;
        }
        Ok(())
    }

    /// Removes and returns the next whole frame, or `None` while the next one is
    /// still incomplete.
    ///
    /// The header is re-validated here as well as in [`feed`](FrameParser::feed):
    /// `feed` is the only thing that adds bytes, so this cannot currently fail,
    /// and having the guard call means the invariant does not depend on that.
    ///
    /// # Errors
    ///
    /// The same errors as [`feed`](FrameParser::feed). Consumers that loop over
    /// everything queued — as `AuthFrontProtocol::SessionProc` and `AuthProcess`
    /// do (blueprint 01 Part A §1.3) — want the `Ok(None)` end-of-queue case to
    /// be the only normal one.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, FrameError> {
        if let Some(err) = self.failure {
            return Err(err);
        }
        if self.buf.len() < 2 {
            return Ok(None);
        }

        let declared = declared_size([self.buf[0], self.buf[1]]);
        if let Err(err) = validate_size(declared) {
            self.failure = Some(err);
            return Err(err);
        }

        let declared = declared as usize;
        if self.buf.len() < declared {
            return Ok(None);
        }

        let bytes: Vec<u8> = self.buf.drain(..declared).collect();
        Ok(Some(Frame { bytes }))
    }

    /// Bytes held but not yet forming a whole frame.
    #[must_use]
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    /// Whether no partial frame is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// The rejection that poisoned this parser, if any.
    #[must_use]
    pub fn failure(&self) -> Option<FrameError> {
        self.failure
    }

    /// Discards buffered bytes, as `ITZRecvBuffer::ClearBuffer` does. Does not
    /// clear a rejection.
    pub fn clear(&mut self) {
        self.buf.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const _: () = {
        // itzbase.h:63-68 is packed, so the header is exactly 5 bytes.
        assert!(HEADER_SIZE == 5);
        // itzbase.h:45 and the arithmetic at ITZRecvBuffer.cpp:172.
        assert!(PACKET_MAX_SIZE == 65000);
        assert!(MAX_FRAME_SIZE == 64992);
    };

    /// A frame with an arbitrary declared size, for cases `Frame::new` refuses.
    fn raw(declared: u16, body: &[u8]) -> Vec<u8> {
        let mut bytes = declared.to_le_bytes().to_vec();
        bytes.extend_from_slice(body);
        bytes
    }

    #[test]
    fn frame_round_trips_through_its_wire_bytes() {
        let frame = Frame::new(0x1234, b"payload").unwrap();
        assert_eq!(frame.size(), 5 + 7);
        assert_eq!(frame.cmd(), 0x1234);
        assert_eq!(frame.integrity(), 0);
        assert_eq!(frame.payload(), b"payload");
        assert_eq!(frame.wire_len(), 12);
        assert_eq!(frame.as_bytes()[..5], [0x0c, 0x00, 0x00, 0x34, 0x12]);
    }

    #[test]
    fn empty_payload_is_still_a_whole_header() {
        let frame = Frame::new(7, &[]).unwrap();
        assert_eq!(frame.as_bytes(), [5, 0, 0, 7, 0]);
        assert!(frame.payload().is_empty());
    }

    #[test]
    fn frame_new_refuses_a_payload_no_peer_would_accept() {
        // One byte past the receive limit, header included.
        let payload = vec![0u8; MAX_FRAME_SIZE - HEADER_SIZE + 1];
        assert_eq!(
            Frame::new(1, &payload),
            Err(FrameError::PayloadTooLarge {
                len: MAX_FRAME_SIZE - HEADER_SIZE + 1
            })
        );
    }

    #[test]
    fn a_frame_exactly_at_the_receive_limit_is_buildable() {
        let payload = vec![0xa5u8; MAX_FRAME_SIZE - HEADER_SIZE];
        let frame = Frame::new(1, &payload).unwrap();
        assert_eq!(frame.size(), MAX_FRAME_SIZE as u16);
    }

    #[test]
    fn guard_table_is_a_range_and_nothing_else() {
        // BUG-013 asks for 0..=5 explicitly: below 2 the legacy also refuses,
        // 2..=4 only this port refuses, 5 is the first accepted frame.
        for declared in 0..5u16 {
            let err = validate_size(declared).unwrap_err();
            assert_eq!(err.declared(), Some(declared));
        }
        assert_eq!(
            validate_size(0),
            Err(FrameError::SizeBelowMinimum { declared: 0 })
        );
        assert_eq!(
            validate_size(1),
            Err(FrameError::SizeBelowMinimum { declared: 1 })
        );
        assert_eq!(
            validate_size(2),
            Err(FrameError::SizeBelowHeader { declared: 2 })
        );
        assert_eq!(
            validate_size(4),
            Err(FrameError::SizeBelowHeader { declared: 4 })
        );
        assert_eq!(validate_size(5), Ok(()));
        assert_eq!(validate_size(HEADER_SIZE as u16), Ok(()));
    }

    #[test]
    fn guard_table_boundaries_at_the_top() {
        assert_eq!(validate_size(MAX_FRAME_SIZE as u16), Ok(()));
        assert_eq!(
            validate_size(MAX_FRAME_SIZE as u16 + 1),
            Err(FrameError::SizeTooLarge { declared: 64993 })
        );
        assert_eq!(
            validate_size(u16::MAX),
            Err(FrameError::SizeTooLarge { declared: 65535 })
        );
    }

    #[test]
    fn parser_yields_a_single_frame() {
        let wire = Frame::new(0x0102, b"hello").unwrap().into_bytes();
        let mut parser = FrameParser::new();
        parser.feed(&wire).unwrap();
        assert_eq!(
            parser.next_frame().unwrap().unwrap(),
            Frame::new(0x0102, b"hello").unwrap()
        );
        assert!(parser.next_frame().unwrap().is_none());
        assert!(parser.is_empty());
    }

    #[test]
    fn two_frames_in_one_chunk_come_out_in_order() {
        let first = Frame::new(1, b"a").unwrap().into_bytes();
        let second = Frame::new(2, b"bb").unwrap().into_bytes();
        let mut parser = FrameParser::new();
        parser.feed(&[first, second].concat()).unwrap();
        assert_eq!(parser.next_frame().unwrap().unwrap().cmd(), 1);
        assert_eq!(parser.next_frame().unwrap().unwrap().cmd(), 2);
        assert!(parser.next_frame().unwrap().is_none());
    }

    #[test]
    fn a_frame_split_across_chunks_is_reassembled() {
        let wire = Frame::new(0x0304, b"split me").unwrap().into_bytes();
        let mut parser = FrameParser::new();
        for chunk in wire.chunks(3) {
            parser.feed(chunk).unwrap();
        }
        let frame = parser.next_frame().unwrap().unwrap();
        assert_eq!(frame.cmd(), 0x0304);
        assert_eq!(frame.payload(), b"split me");
    }

    #[test]
    fn a_single_byte_at_a_time_is_enough() {
        let wire = Frame::new(9, b"drip").unwrap().into_bytes();
        let mut parser = FrameParser::new();
        for (i, byte) in wire.iter().enumerate() {
            parser.feed(std::slice::from_ref(byte)).unwrap();
            let last = i == wire.len() - 1;
            assert_eq!(parser.next_frame().unwrap().is_some(), last);
        }
    }

    #[test]
    fn empty_reads_are_a_no_op() {
        let mut parser = FrameParser::new();
        parser.feed(&[]).unwrap();
        assert!(parser.next_frame().unwrap().is_none());
        assert!(parser.is_empty());
        assert_eq!(parser.buffered(), 0);
    }

    #[test]
    fn a_partial_header_waits_instead_of_guessing() {
        // One byte is not enough to read the size field, which is why the legacy
        // stops at `m_pLastPtr - 1` (ITZRecvBuffer.cpp:149-151).
        let wire = Frame::new(1, b"x").unwrap().into_bytes();
        let mut parser = FrameParser::new();
        parser.feed(&wire[..1]).unwrap();
        assert_eq!(parser.buffered(), 1);
        assert!(parser.next_frame().unwrap().is_none());
        parser.feed(&wire[1..]).unwrap();
        assert!(parser.next_frame().unwrap().is_some());
    }

    #[test]
    fn a_frame_shorter_than_the_header_is_rejected_on_feed() {
        let mut parser = FrameParser::new();
        assert_eq!(
            parser.feed(&raw(4, &[0xaa, 0xbb, 0xcc, 0xdd])),
            Err(FrameError::SizeBelowHeader { declared: 4 })
        );
    }

    #[test]
    fn a_size_the_legacy_also_refused_stays_distinguishable() {
        let mut parser = FrameParser::new();
        assert_eq!(
            parser.feed(&raw(1, &[0xaa])),
            Err(FrameError::SizeBelowMinimum { declared: 1 })
        );
    }

    #[test]
    fn oversize_is_rejected_before_the_body_is_needed() {
        // The legacy checks the ceiling straight after reading 2 bytes, so two
        // bytes are enough to drive the rejection (ITZRecvBuffer.cpp:172-180).
        let mut parser = FrameParser::new();
        assert_eq!(
            parser.feed(&raw(64993, &[])),
            Err(FrameError::SizeTooLarge { declared: 64993 })
        );
    }

    #[test]
    fn a_rejection_is_sticky_and_sticks_to_the_original_error() {
        let mut parser = FrameParser::new();
        assert!(parser.feed(&raw(0, &[])).is_err());
        assert_eq!(
            parser.failure(),
            Some(FrameError::SizeBelowMinimum { declared: 0 })
        );
        // Further good bytes change nothing, and neither does clear().
        let good = Frame::new(1, b"ok").unwrap().into_bytes();
        assert_eq!(
            parser.feed(&good),
            Err(FrameError::SizeBelowMinimum { declared: 0 })
        );
        parser.clear();
        assert_eq!(
            parser.next_frame(),
            Err(FrameError::SizeBelowMinimum { declared: 0 })
        );
    }

    #[test]
    fn a_bad_header_behind_a_good_frame_is_caught_at_feed_time() {
        // The legacy scans the whole buffer in Completion() rather than waiting
        // for the consumer to ask for frame 2, so frame 1 is never handed out.
        let good = Frame::new(1, b"fine").unwrap().into_bytes();
        let mut parser = FrameParser::new();
        assert_eq!(
            parser.feed(&[good, raw(3, &[1, 2, 3])].concat()),
            Err(FrameError::SizeBelowHeader { declared: 3 })
        );
        assert!(parser.next_frame().is_err());
    }

    #[test]
    fn a_declared_size_larger_than_the_body_waits_for_the_rest() {
        let wire = Frame::new(1, b"body").unwrap().into_bytes();
        let mut parser = FrameParser::new();
        parser.feed(&wire[..wire.len() - 1]).unwrap();
        assert_eq!(parser.buffered(), wire.len() - 1);
        assert!(parser.next_frame().unwrap().is_none());
        parser.feed(&wire[wire.len() - 1..]).unwrap();
        assert_eq!(parser.next_frame().unwrap().unwrap().payload(), b"body");
    }

    #[test]
    fn trailing_partial_bytes_are_kept_for_the_next_chunk() {
        let first = Frame::new(1, b"a").unwrap().into_bytes();
        let second = Frame::new(2, b"b").unwrap().into_bytes();
        let mut parser = FrameParser::new();
        let split = first.len() + 3;
        parser
            .feed(&[first.clone(), second.clone()].concat()[..split])
            .unwrap();
        assert_eq!(parser.next_frame().unwrap().unwrap().cmd(), 1);
        assert_eq!(parser.buffered(), 3);
        assert!(parser.next_frame().unwrap().is_none());
        parser.feed(&[first, second].concat()[split..]).unwrap();
        assert_eq!(parser.next_frame().unwrap().unwrap().cmd(), 2);
        assert!(parser.is_empty());
    }

    #[test]
    fn session_states_match_the_legacy_defines() {
        // ITZSession.h:6-9.
        assert_eq!(SessionState::None.as_legacy(), 0);
        assert_eq!(SessionState::Connected.as_legacy(), 1);
        assert_eq!(SessionState::Established.as_legacy(), 2);
        assert_eq!(SessionState::Disconnected.as_legacy(), 3);
        for value in 0..=3 {
            assert_eq!(SessionState::from_legacy(value).unwrap().as_legacy(), value);
        }
        assert_eq!(SessionState::from_legacy(4), None);
        assert_eq!(SessionState::from_legacy(-1), None);
    }

    #[test]
    fn only_established_sessions_carry_frames() {
        assert!(!SessionState::None.is_active());
        assert!(!SessionState::Connected.is_active());
        assert!(SessionState::Established.is_active());
        assert!(!SessionState::Disconnected.is_active());
        assert_eq!(SessionState::default(), SessionState::None);
    }

    #[test]
    fn the_compression_flag_is_readable_and_strippable() {
        // GameProtocol.cpp:109 tests bit 15, :141 clears it with `usCmd -= 0x8000`.
        let wire = Frame::new(117, b"hello").unwrap().into_bytes();
        let plain = Frame {
            bytes: wire.clone(),
        };
        assert!(!plain.compressed());
        assert_eq!(plain.cmd(), 117);
        assert_eq!(plain.cmd_id(), 117);

        // usCmd is little-endian at offsets 3..5, so bit 15 is the high byte's top bit.
        let mut flagged = wire;
        flagged[4] |= 0x80;
        let frame = Frame { bytes: flagged };
        assert!(frame.compressed());
        assert_eq!(frame.cmd(), 117 | COMPRESSION_FLAG);
        assert_eq!(frame.cmd_id(), 117);
    }

    #[test]
    fn errors_name_the_declared_size_and_render() {
        let err = FrameError::SizeTooLarge { declared: 4464 };
        assert_eq!(err.declared(), Some(4464));
        assert!(err.to_string().contains("4464"));
        assert!(err.to_string().contains("64992"));

        let err = FrameError::PayloadTooLarge { len: 7 };
        assert_eq!(err.declared(), None);
        assert!(err.to_string().contains('7'));
    }
}
