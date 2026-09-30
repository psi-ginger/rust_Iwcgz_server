//! Stream-level framing tests: the parser is fed the way a socket feeds it.
//!
//! These are deliberately separate from the unit tests in `src/lib.rs`. The unit
//! tests pin the guard table and the single-frame cases; these pin the properties
//! a link actually depends on — every split point works, byte-exact round trips,
//! and a long stream stays framed.

use gz_net::{Frame, FrameError, FrameParser, MAX_FRAME_SIZE};

/// Frames the legacy accepts without a cipher: the framing layer never decrypts
/// (`ITZRecvBuffer` has no decode step), so this is the plain-link case.
fn wire(cmd: u16, payload: &[u8]) -> Vec<u8> {
    Frame::new(cmd, payload).unwrap().into_bytes()
}

/// A deterministic byte pattern, so a mismatch points at a specific offset.
fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// Everything currently framed, in order.
fn drain(parser: &mut FrameParser) -> Result<Vec<Frame>, FrameError> {
    let mut frames = Vec::new();
    while let Some(frame) = parser.next_frame()? {
        frames.push(frame);
    }
    Ok(frames)
}

/// A stream of many frames, flattened onto the wire.
fn stream(frames: &[Frame]) -> Vec<u8> {
    frames.iter().flat_map(Frame::as_bytes).copied().collect()
}

#[test]
fn round_trip_is_byte_exact_for_every_split_point() {
    let body = pattern(200);
    let bytes = wire(0x0a0b, &body);
    let expected = Frame::new(0x0a0b, &body).unwrap();

    for split in 0..=bytes.len() {
        let mut parser = FrameParser::new();
        parser.feed(&bytes[..split]).unwrap();
        parser.feed(&bytes[split..]).unwrap();
        let frames = drain(&mut parser).unwrap();
        assert_eq!(
            frames.as_slice(),
            std::slice::from_ref(&expected),
            "split at {split}"
        );
        assert_eq!(frames[0].as_bytes(), bytes.as_slice(), "split at {split}");
    }
}

#[test]
fn round_trip_is_byte_exact_one_byte_at_a_time() {
    let body = pattern(64);
    let bytes = wire(0x0100, &body);
    let mut parser = FrameParser::new();
    let mut frames = Vec::new();
    for byte in &bytes {
        parser.feed(std::slice::from_ref(byte)).unwrap();
        frames.extend(drain(&mut parser).unwrap());
    }
    assert_eq!(frames, [Frame::new(0x0100, &body).unwrap()]);
}

#[test]
fn a_long_stream_stays_framed_across_arbitrary_chunk_boundaries() {
    // The legacy recv buffers in production run from 30 KB to 20 MB
    // (blueprint 01 Part A §1.6), so a stream has to hold many frames.
    let frames: Vec<Frame> = (0..200u16)
        .map(|i| Frame::new(i, &pattern(usize::from(i % 97))).unwrap())
        .collect();
    let bytes = stream(&frames);

    for chunk_size in [1, 2, 5, 13, 64, 512, 4096, 70_000] {
        let mut parser = FrameParser::new();
        let mut got = Vec::new();
        for chunk in bytes.chunks(chunk_size) {
            parser.feed(chunk).unwrap();
            got.extend(drain(&mut parser).unwrap());
        }
        assert_eq!(got, frames, "chunk size {chunk_size}");
        assert!(parser.is_empty(), "chunk size {chunk_size}");
    }
}

#[test]
fn a_frame_at_the_receive_limit_survives_the_parser() {
    let body = pattern(MAX_FRAME_SIZE - 5);
    let bytes = wire(0x4242, &body);
    assert_eq!(bytes.len(), MAX_FRAME_SIZE);

    let mut parser = FrameParser::new();
    // Split so the parser holds the header long before the rest of the frame.
    parser.feed(&bytes[..2]).unwrap();
    assert_eq!(parser.buffered(), 2);
    parser.feed(&bytes[2..]).unwrap();

    let frames = drain(&mut parser).unwrap();
    assert_eq!(frames[0].payload().len(), MAX_FRAME_SIZE - 5);
    assert_eq!(frames[0].payload(), body.as_slice());
    assert_eq!(frames[0].cmd(), 0x4242);
}

#[test]
fn declared_sizes_below_the_header_are_rejected_not_ignored() {
    // BUG-013: sizes 0..=3 are accepted verbatim by the legacy cipher, and the
    // legacy recv guard only refuses below 2. The framing layer must refuse them.
    for declared in 0u16..5 {
        // The header claims `declared` bytes but more follow, so the guard has its
        // 2 bytes to read. This is the shape a peer would actually send.
        let mut bytes = declared.to_le_bytes().to_vec();
        bytes.extend_from_slice(&pattern(8));

        let mut parser = FrameParser::new();
        let err = parser.feed(&bytes).unwrap_err();
        assert_eq!(err.declared(), Some(declared));
        assert!(
            matches!(err, FrameError::SizeBelowMinimum { .. })
                || matches!(err, FrameError::SizeBelowHeader { .. }),
            "declared {declared} gave {err}"
        );
        assert_eq!(parser.next_frame(), Err(err));
    }
}

#[test]
fn a_buffer_shorter_than_the_size_field_is_only_held() {
    // Nothing is rejected before the size field can be read - otherwise the first
    // byte of a legitimate frame would look like a malformed one.
    for held in [0usize, 1] {
        let bytes = wire(0x55aa, b"body");
        let mut parser = FrameParser::new();
        parser.feed(&bytes[..held]).unwrap();
        assert_eq!(parser.buffered(), held);
        assert!(drain(&mut parser).unwrap().is_empty());
        assert_eq!(parser.failure(), None);
        parser.feed(&bytes[held..]).unwrap();
        assert_eq!(drain(&mut parser).unwrap().len(), 1);
    }
}

#[test]
fn size_five_is_the_smallest_frame_that_parses() {
    let mut parser = FrameParser::new();
    parser.feed(&wire(0x0102, &[])).unwrap();
    let frames = drain(&mut parser).unwrap();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].size(), 5);
    assert_eq!(frames[0].cmd(), 0x0102);
    assert!(frames[0].payload().is_empty());
}

#[test]
fn an_oversize_header_rejects_before_the_body_is_needed() {
    let mut parser = FrameParser::new();
    let mut bytes = ((MAX_FRAME_SIZE + 1) as u16).to_le_bytes().to_vec();
    bytes.extend_from_slice(&pattern(1024));
    assert_eq!(
        parser.feed(&bytes),
        Err(FrameError::SizeTooLarge {
            declared: (MAX_FRAME_SIZE + 1) as u16,
        })
    );
}

#[test]
fn a_truncated_final_frame_never_comes_out() {
    let bytes = wire(1, b"complete");
    assert_eq!(bytes.len(), 13);

    // One byte short of the declared size: held, never delivered, however it is
    // chunked.
    let mut parser = FrameParser::new();
    for chunk in bytes[..12].chunks(5) {
        parser.feed(chunk).unwrap();
        assert!(drain(&mut parser).unwrap().is_empty());
    }
    assert_eq!(parser.buffered(), 12);

    // Completing it releases exactly one frame and nothing else.
    parser.feed(&bytes[12..]).unwrap();
    let frames = drain(&mut parser).unwrap();
    assert_eq!(frames, [Frame::new(1, b"complete").unwrap()]);
    assert!(parser.is_empty());
}

#[test]
fn a_rejected_parser_never_recovers() {
    let mut parser = FrameParser::new();
    parser.feed(&wire(1, b"a")).unwrap();
    assert_eq!(parser.buffered(), 6);

    // A frame that claims fewer bytes than its own header, behind a good one.
    assert_eq!(
        parser.feed(&[0x03, 0x00, 0x01, 0x02, 0x03]),
        Err(FrameError::SizeBelowHeader { declared: 3 })
    );

    let poisoned = parser.failure().unwrap();
    for _ in 0..3 {
        assert_eq!(parser.feed(&wire(2, b"b")), Err(poisoned));
        assert_eq!(parser.next_frame(), Err(poisoned));
        assert_eq!(parser.failure(), Some(poisoned));
    }
}

#[test]
fn a_header_split_across_two_chunks_is_still_checked() {
    // The guard must run as soon as both size bytes are in hand - not a byte
    // later - which is the point of the legacy's `m_pLastPtr - 1` stop.
    let mut parser = FrameParser::new();
    parser.feed(&[0x00]).unwrap();
    assert_eq!(parser.buffered(), 1);
    assert_eq!(parser.failure(), None);
    assert_eq!(
        parser.feed(&[0x00]),
        Err(FrameError::SizeBelowMinimum { declared: 0 })
    );
    assert_eq!(parser.failure().unwrap().declared(), Some(0));
}

#[test]
fn interleaved_split_and_merge_keeps_order() {
    let expected: Vec<Frame> = (0..32u16)
        .map(|i| Frame::new(0x1000 + i, &pattern(usize::from(i) + 1)).unwrap())
        .collect();
    let bytes = stream(&expected);

    // Feed in ragged pieces that never align with a frame boundary.
    let mut parser = FrameParser::new();
    let mut got = Vec::new();
    let mut at = 0usize;
    for chunk_size in [7usize, 3, 41, 1, 128, 5, 999, 2].iter().copied().cycle() {
        let end = (at + chunk_size).min(bytes.len());
        parser.feed(&bytes[at..end]).unwrap();
        got.extend(drain(&mut parser).unwrap());
        at = end;
        if at == bytes.len() {
            break;
        }
    }
    assert_eq!(got, expected);
}

#[test]
fn clearing_the_parser_drops_a_partial_frame() {
    // ITZRecvBuffer::ClearBuffer resets the read pointers without touching the
    // connection; gz-net's clear() must not resurrect the discarded bytes.
    let bytes = wire(1, b"abc");
    let mut parser = FrameParser::new();
    parser.feed(&bytes[..bytes.len() - 1]).unwrap();
    assert!(!parser.is_empty());

    parser.clear();
    assert!(parser.is_empty());

    // Bytes that do not start a frame are held, not delivered.
    parser.feed(&pattern(bytes.len() - 1)).unwrap();
    assert!(drain(&mut parser).unwrap().is_empty());
    assert!(!parser.is_empty());

    // A fresh frame parses normally once the dead bytes are gone, and the
    // rejection state (there is none) is unaffected by clearing.
    parser.clear();
    parser.feed(&wire(2, b"ok")).unwrap();
    assert_eq!(parser.failure(), None);
    assert_eq!(drain(&mut parser).unwrap(), [Frame::new(2, b"ok").unwrap()]);
}

#[test]
fn a_frame_larger_than_the_receive_limit_is_rejected_after_good_ones() {
    let good = wire(1, b"fine");
    let mut parser = FrameParser::new();
    let mut stream_bytes = good.clone();
    stream_bytes.extend_from_slice(&((MAX_FRAME_SIZE + 1) as u16).to_le_bytes());

    assert_eq!(
        parser.feed(&stream_bytes),
        Err(FrameError::SizeTooLarge {
            declared: (MAX_FRAME_SIZE + 1) as u16,
        })
    );
    // The good frame ahead of it is never handed out: a rejection is a
    // disconnect, not a skip-and-resync.
    assert!(parser.next_frame().is_err());
}
