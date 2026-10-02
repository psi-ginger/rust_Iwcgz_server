//! Wire framing for the ops channel.
//!
//! # Why length-prefixed and not newline-delimited
//!
//! The obvious encoding for "JSON over a local socket" is one JSON object per
//! line. It is wrong here. Everything the ops channel carries is untrusted
//! text: log lines, player names, config values, error messages from the
//! database driver. Any of those can contain `\n`. A CP949-to-UTF-8
//! conversion can produce one, a crafted player name can, and a legacy log
//! line that already contained a carriage return can. The moment the receiver
//! splits on newline instead of on a length, one such payload desynchronises
//! the stream: the tail of the string is parsed as a fresh frame, and every
//! subsequent request is off by one.
//!
//! A 4-byte big-endian length prefix removes that entire class of bug. It also
//! means a frame can carry embedded newlines, NULs and invalid UTF-8-looking
//! byte sequences without any special handling by the transport.
//!
//! # Why big-endian
//!
//! Network byte order, stated explicitly rather than inherited from the host.
//! Both ends are Rust on the same machine in the common case, but this is a
//! published contract and a future remote transport should not have to
//! negotiate endianness.
//!
//! # Oversized frames are fatal, not skipped
//!
//! A frame declaring a length above [`MAX_FRAME_BYTES`] is a protocol error
//! and the connection is closed. It is tempting to instead read-and-discard
//! the declared length so the stream can resynchronise. That is a denial of
//! service waiting to happen: a peer declares a 4 GiB frame and then dribbles
//! bytes, and the receiver is obliged to spin in a read loop discarding them.
//! Closing costs one reconnect, which is cheap for a local tool talking to a
//! supervisor it can restart.

use std::fmt;

/// Number of bytes in the length prefix.
pub const HEADER_LEN: usize = 4;

/// Largest payload a single frame may declare.
///
/// 64 KiB is far above any legitimate ops message. The largest realistic
/// payload is a `logs.tail` burst or a chunk of config, and those are capped
/// well below this by the method layer. Anything larger is either a bug or a
/// probe, and neither is worth trying to salvage.
pub const MAX_FRAME_BYTES: usize = 65_536;

/// A framing failure. Every variant is fatal to the connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    /// The declared payload length exceeds [`MAX_FRAME_BYTES`]. Fatal.
    TooLarge {
        /// Length the peer declared.
        declared: usize,
    },
    /// The declared payload length is zero, which cannot be valid JSON.
    ///
    /// Treated as an error rather than an empty frame so that "empty payload"
    /// has exactly one meaning (an error) instead of two.
    ZeroLength,
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { declared } => write!(
                f,
                "frame declares {declared} bytes, limit is {MAX_FRAME_BYTES}"
            ),
            Self::ZeroLength => write!(f, "frame declares a zero-length payload"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Wrap a payload in the length prefix.
///
/// # Panics
///
/// Panics if `payload.len() > MAX_FRAME_BYTES`. This is a programming error
/// in the caller, and the alternative -- silently emitting a frame the peer
/// will reject -- would be worse.
pub fn encode(payload: &[u8]) -> Vec<u8> {
    assert!(
        payload.len() <= MAX_FRAME_BYTES,
        "payload of {} bytes exceeds MAX_FRAME_BYTES ({MAX_FRAME_BYTES})",
        payload.len()
    );

    let len = payload.len() as u32;
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// Decode exactly one frame from `buf`, consuming it on success.
///
/// Returns `Ok(None)` when `buf` does not yet hold a complete frame, which is
/// the normal case on a stream socket and is **not** an error. The caller
/// should read more bytes and call again.
///
/// On error the buffer is left untouched, because a malformed length prefix
/// means the stream position is no longer trustworthy and the caller is
/// expected to close rather than continue.
pub fn decode(buf: &mut Vec<u8>) -> Result<Option<Vec<u8>>, FrameError> {
    if buf.len() < HEADER_LEN {
        return Ok(None);
    }

    let declared = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;

    if declared > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge { declared });
    }
    if declared == 0 {
        return Err(FrameError::ZeroLength);
    }

    if buf.len() < HEADER_LEN + declared {
        return Ok(None);
    }

    let payload = buf[HEADER_LEN..HEADER_LEN + declared].to_vec();
    buf.drain(..HEADER_LEN + declared);
    Ok(Some(payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_prefixes_big_endian_length() {
        let f = encode(b"hi");
        assert_eq!(&f[..4], &[0, 0, 0, 2]);
        assert_eq!(&f[4..], b"hi");
    }

    #[test]
    fn round_trips_including_embedded_newlines_and_nuls() {
        // The whole reason this module exists. A JSON string may legally
        // contain both of these, and a line-oriented transport would split on
        // the first one.
        let payload = br#"{"m":"line one\nline two\0and a nul"}"#;
        let mut buf = encode(payload);

        let got = decode(&mut buf).unwrap().expect("complete frame");
        assert_eq!(got, payload);
        assert!(buf.is_empty(), "frame must be consumed exactly");
    }

    #[test]
    fn decodes_two_frames_from_one_buffer() {
        let mut buf = encode(b"first");
        buf.extend(encode(b"second"));

        assert_eq!(decode(&mut buf).unwrap().unwrap(), b"first");
        assert_eq!(decode(&mut buf).unwrap().unwrap(), b"second");
        assert_eq!(decode(&mut buf).unwrap(), None);
        assert!(buf.is_empty());
    }

    #[test]
    fn partial_frames_return_none_without_consuming() {
        let full = encode(b"abcdefgh");

        for split in 0..full.len() {
            let mut buf = full[..split].to_vec();
            assert_eq!(
                decode(&mut buf).unwrap(),
                None,
                "prefix of {split} bytes must not yield a frame"
            );
            assert_eq!(buf, full[..split], "buffer must be untouched");
        }

        let mut buf = full;
        assert_eq!(decode(&mut buf).unwrap().unwrap(), b"abcdefgh");
    }

    #[test]
    fn oversized_declaration_is_rejected_and_leaves_buffer_intact() {
        let mut buf = vec![0x00, 0x01, 0x00, 0x00]; // 65536 -- exactly the max
        buf.extend_from_slice(&[0u8; MAX_FRAME_BYTES]);
        assert!(decode(&mut buf).is_ok(), "exactly at the limit is allowed");

        let mut buf = vec![0x00, 0x01, 0x00, 0x01]; // one over
        buf.extend_from_slice(b"x");
        assert_eq!(
            decode(&mut buf),
            Err(FrameError::TooLarge { declared: 65_537 })
        );
        assert_eq!(buf.len(), 5, "buffer untouched so the caller can close");
    }

    #[test]
    fn zero_length_is_an_error_not_an_empty_frame() {
        let mut buf = vec![0, 0, 0, 0];
        assert_eq!(decode(&mut buf), Err(FrameError::ZeroLength));
    }

    #[test]
    fn maximum_size_frame_round_trips() {
        let payload = vec![0xABu8; MAX_FRAME_BYTES];
        let mut buf = encode(&payload);
        assert_eq!(decode(&mut buf).unwrap().unwrap(), payload);
    }

    #[test]
    fn encode_panics_above_the_limit() {
        // Prose in the signature claims this; check it rather than trust it.
        let too_big = vec![0u8; MAX_FRAME_BYTES + 1];
        let result = std::panic::catch_unwind(|| encode(&too_big));
        assert!(result.is_err(), "encode must reject oversized payloads");
    }
}
