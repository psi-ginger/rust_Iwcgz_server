//! Replay plan for the captured reference session.
//!
//! The interleaved transcript of `login_capture.pcapng` (see
//! `generated_frames::CAP_S2C` for the payloads) shows exactly which frames the
//! reference server sent after each client request. That mapping is what this
//! module encodes, as half-open index ranges into
//! [`generated_frames::CAP_FRAMES`].
//!
//! Two mappings matter, and they were both wrong in the first attempt:
//!
//! * The 25-frame block that carries `RESPONSE_ALLCHARINFOONE` (248) and
//!   `RESPONSE_ALLCHARINFTWO` (249) is **not** the answer to
//!   `REQUEST_ALLCHARINFO` (246) — the reference server replies to 246 with
//!   nothing at all. It answers `REQUEST_CHANGESTATUSINFO` (230).
//! * That block starts 14 frames *earlier* than 248/249: frames 66..=79 carry
//!   the map/object bulk (349, 8451, 8470, 8178, 6504, 8271, 8285, 8286, 8371,
//!   8464, 225, 226, 8451, 244). Without them the client still faults on the
//!   same null per-character record.

use crate::generated_frames::{CAP_FRAMES, CAP_S2C};

/// The character id the captured frames are keyed on.
///
/// Every per-character payload in [`CAP_S2C`] identifies the character as
/// `0x05BF`; the dev session has to present the same id or the client treats the
/// records as belonging to somebody else's character and ignores them.
pub const CAPTURED_CHAR_UNIQUE: i32 = 0x05BF;

/// The tail of the reference server's world-entry prologue: everything it sent
/// after `RESPONSE_LOGIN` (frame 20) up to the `REQUEST_CHANGESTATUSINFO`
/// round-trip — 7600, 7606, 2926 x35, 5217, 7617, 8001, 8064, 177, 7210, 6219,
/// 8097.
///
/// Frames 16..=20 (7216, 106, 1170, 7228, 5) are deliberately *not* replayed:
/// the dev service emits its own `SERVERINFO` + `LOGIN` pair there, which the
/// client already accepts.
pub const PROLOGUE_TAIL: (u32, u32) = (21, 66);

/// Client opcode -> the run of captured frames the reference server sent next.
///
/// Ordered: the plan is walked as a cursor, so a repeated opcode (121) can map
/// to different replies on its first and second appearance, exactly as captured.
pub static REPLY_PLAN: &[(u16, &[(u32, u32)])] = &[
    // 230 -> the map/object bulk, then ALLCHARINFO one/two, village struct, ...
    (230, &[(66, 91)]),
    (121, &[(91, 92)]),
    (240, &[(92, 96)]),
    (7117, &[(96, 100)]),
    (121, &[(100, 101)]),
];

/// Every captured frame in the half-open index range `start..end`, paired with
/// its slice of the payload blob.
pub fn frames_in(start: u32, end: u32) -> impl Iterator<Item = (u16, &'static [u8])> {
    (start..end).map(move |i| {
        let (cmd, offset, len) = CAP_FRAMES[i as usize];
        (cmd, &CAP_S2C[offset as usize..(offset + len) as usize])
    })
}

/// The first captured frame carrying `cmd`, if the reference session sent one.
///
/// Used by the importer to read a record out of the fixture instead of the
/// network: it must see the same bytes the replay will send.
#[must_use]
pub fn frame(cmd: u16) -> Option<&'static [u8]> {
    CAP_FRAMES
        .iter()
        .find(|(c, _, _)| *c == cmd)
        .map(|&(_, offset, len)| &CAP_S2C[offset as usize..(offset + len) as usize])
}
