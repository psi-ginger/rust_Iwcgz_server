//! The port is replayed against the legacy, not against itself.
//!
//! `tests/data/vectors.rs` is generated from `reference/harness.cpp`, which links
//! the byte-for-byte unmodified `ITZRecvBuffer.cpp` and drives chunked streams
//! through it the way the IOCP does. A round trip through this crate's own tests
//! would pass for any mutually-consistent pair of wrong implementations; these
//! vectors were recorded from the legacy itself.
//!
//! Every case must agree with the legacy on both questions that matter: was the
//! buffer refused (`Completion`'s return value), and which bytes came out. The
//! single exception is listed in `DIVERGENT` and asserted explicitly, so the
//! BUG-013 divergence is pinned in data rather than left to a comment.

mod data {
    include!("data/vectors.rs");
}

use data::{CASES, DIVERGENT};
use gz_net::{FrameParser, HEADER_SIZE, MAX_FRAME_SIZE};

fn unhex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "odd-length hex: {s}");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex digit"))
        .collect()
}

/// What this port did with one case's chunks, compared step by step against the
/// legacy.
///
/// Returns `Err(())` if the two disagree anywhere, so the caller can report which
/// case and which read went wrong.
fn replay(case: &data::Case) -> Result<(), String> {
    let divergent = DIVERGENT.contains(&case.name);
    let mut parser = FrameParser::new();

    for (index, step) in case.steps.iter().enumerate() {
        let at = format!("{} read {index}", case.name);

        if divergent {
            // The only divergence: the legacy accepted a frame with no room for a
            // command id, this port refuses the connection. Asserted in both
            // directions, so neither side can drift into agreeing by accident.
            let refused = parser.feed(&unhex(step.chunk)).is_err();
            assert!(refused, "{at}: expected this port to refuse");
            assert!(step.completed, "{at}: legacy is expected to accept");
            let handed = step.packets;
            assert_eq!(handed.len(), 1, "{at}: expected exactly one legacy frame");
            assert!(
                unhex(handed[0]).len() < HEADER_SIZE,
                "{at}: divergence case must be a sub-header frame"
            );
            assert!(
                parser.next_frame().is_err(),
                "{at}: a refused parser must stay refused"
            );
            return Ok(());
        }

        match parser.feed(&unhex(step.chunk)) {
            Ok(()) => {
                if !step.completed {
                    return Err(format!("{at}: the legacy refused but this port accepted"));
                }
            }
            Err(err) => {
                if step.completed {
                    return Err(format!(
                        "{at}: this port refused ({err}) but the legacy accepted"
                    ));
                }
                // Both refused: agreement, and a refusal hands out nothing.
                if !step.packets.is_empty() {
                    return Err(format!("{at}: a refused buffer handed out a frame"));
                }
                assert!(
                    parser.next_frame().is_err(),
                    "{at}: a refused parser must stay refused"
                );
                return Ok(());
            }
        }

        let mut got: Vec<Vec<u8>> = Vec::new();
        while let Some(frame) = parser.next_frame().map_err(|e| format!("{at}: {e}"))? {
            got.push(frame.into_bytes());
        }
        let want: Vec<Vec<u8>> = step.packets.iter().map(|p| unhex(p)).collect();
        if got != want {
            return Err(format!(
                "{at}: frames differ\n  legacy: {want:x?}\n  this port: {got:x?}"
            ));
        }
    }

    Ok(())
}

#[test]
fn the_fixture_only_contains_frames_that_describe_themselves() {
    // A ground-truth table that recorded a 2-byte frame could never be replayed,
    // so say so here rather than letting the divergence hide in a loop.
    for case in CASES {
        for step in case.steps {
            for packet in step.packets {
                let bytes = unhex(packet);
                let declared = u16::from_le_bytes([bytes[0], bytes[1]]);
                assert_eq!(
                    usize::from(declared),
                    bytes.len(),
                    "{}: packet length disagrees with its own header",
                    case.name
                );
            }
        }
    }
}

#[test]
fn the_port_agrees_with_the_legacy_on_everything_but_the_declared_divergence() {
    for case in CASES {
        // The fixture must not record reads after a refusal: the legacy caller
        // disconnects and stops feeding, so those reads never happened.
        let last_refusal = case
            .steps
            .iter()
            .rposition(|step| !step.completed)
            .unwrap_or(usize::MAX);
        for (index, step) in case.steps.iter().enumerate() {
            if !step.completed {
                assert_eq!(
                    index, last_refusal,
                    "{}: reads recorded after a refusal",
                    case.name
                );
                assert!(
                    step.packets.is_empty(),
                    "{}: a refused buffer handed out a frame",
                    case.name
                );
            }
        }

        if let Err(why) = replay(case) {
            panic!("{why}");
        }
    }
}

#[test]
fn the_divergence_is_exactly_the_three_sub_header_sizes() {
    // If a new case is ever added to DIVERGENT this pins down what it is allowed
    // to be: a declared size of 2, 3 or 4 and nothing else.
    assert_eq!(
        DIVERGENT,
        &["declared_2", "declared_3", "declared_4"],
        "the divergence list changed; re-verify it against the legacy"
    );

    for name in DIVERGENT {
        let case = CASES
            .iter()
            .find(|case| case.name == *name)
            .expect("divergent case missing from the fixture");
        let frames: Vec<Vec<u8>> = case
            .steps
            .iter()
            .flat_map(|step| step.packets)
            .map(|packet| unhex(packet))
            .collect();
        assert_eq!(frames.len(), 1, "{name}");
        assert!(frames[0].len() < HEADER_SIZE, "{name}");
    }
}

#[test]
fn every_case_name_is_unique_and_the_fixture_is_not_trivial() {
    let mut names: Vec<&str> = CASES.iter().map(|case| case.name).collect();
    names.sort_unstable();
    let before = names.len();
    names.dedup();
    assert_eq!(before, names.len(), "duplicate case names");
    assert!(
        CASES.len() >= 15,
        "the framing fixture shrank to {} cases",
        CASES.len()
    );
}

#[test]
fn the_limit_cases_in_the_fixture_sit_on_the_documented_constants() {
    // The interesting boundaries must be measured, not assumed. If the constants
    // move and the fixture does not, this is what notices.
    let by_name = |name: &str| {
        CASES
            .iter()
            .find(|case| case.name == name)
            .unwrap_or_else(|| panic!("missing case {name}"))
    };

    // 5 is accepted: the smallest frame that can hold a header.
    assert!(by_name("declared_5").steps[0].completed);
    assert_eq!(by_name("declared_5").steps[0].packets.len(), 1);
    assert_eq!(
        unhex(by_name("declared_5").steps[0].packets[0]).len(),
        HEADER_SIZE
    );

    // A legal size whose body has not arrived is held, not refused - this is what
    // pins 64992 as inside the limit without carrying 130 KB of hex around. The
    // matching full-length case lives in framing.rs.
    assert!(by_name("declared_64992_short_body").steps[0].completed);
    assert!(
        by_name("declared_64992_short_body").steps[0]
            .packets
            .is_empty()
    );
    assert!(by_name("declared_300_short_body").steps[0].completed);

    // One byte past PACKET_MAX_SIZE - 8, and the u16 ceiling, are refused.
    assert!(!by_name("declared_64993_over_limit").steps[0].completed);
    assert!(!by_name("declared_65535").steps[0].completed);

    // A legal frame ahead of an over-limit header is never delivered: the legacy
    // refuses the whole buffer, so the good frame does not escape first.
    let good_then_over = by_name("good_then_over_limit");
    assert!(!good_then_over.steps[0].completed);
    assert!(good_then_over.steps[0].packets.is_empty());
    assert!(!unhex(good_then_over.steps[0].chunk).is_empty());

    assert_eq!(MAX_FRAME_SIZE, 64992);
}
