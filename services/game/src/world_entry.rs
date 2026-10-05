//! Generation of the world-entry sequence, replacing the capture replay.
//!
//! T-016's predecessor replayed `login_capture.pcapng` for everything the client
//! sees after char-select. That was never a real fix: it made the *reference
//! session* look like a *server*, so two accounts logging in saw the same
//! world, and anything a player changed was undone at the next login. This
//! module replaces the replay with generation, and keeps exactly one frame on
//! the capture (see [`WorldEntry::person_item_info`]).
//!
//! # What the sequence turned out to be
//!
//! The 70 frames the reference server sent across world entry were measured
//! before any of this was written. Only 8 KB of the 75.8 KB (11%) sits in a
//! frame that is more than 10% non-zero, and most of that is four tiny frames.
//! The rest is **sparse fixed-size record arrays** -- the usual "N fixed-width
//! slots, all empty except a few" -- which is why generation turned out to be
//! tractable where it looked hopeless from the frame count alone:
//!
//! | shape | frames | bytes |
//! |---|---|---|
//! | entirely zero | 12 | 21,024 (28%) |
//! | under 10% non-zero | 12 | ~5,000 |
//! | 10% non-zero or more | 46 | 7,982 (11%) |
//!
//! So the work is not "reverse 75 KB of protocol". It is "know which slots are
//! live, and zero the rest", and the capture is a byte-level oracle for that
//! without ever being the source at runtime.
//!
//! # Every generated frame is length-pinned to the capture
//!
//! [`prologue`] returns frames whose `(opcode, length)` pairs are asserted
//! equal to the captured ones, frame for frame, in
//! `generated_prologue_matches_the_capture_frame_for_frame`. A wrong length is
//! the failure mode that would desync the client silently, so it is the one
//! thing pinned by test rather than by review.
//!
//! # Citations
//!
//! The oracle for the tail is `cltServer::DoMsg_GAMEMSG_REQUEST_ALLCHARINFO`
//! (`Server-Person.cpp:2190`), which the reference server's frames 81-90 match
//! in order: `249`, `248`, `DATE`, `SERVERTIME`, `ENEMYCAPTUREALL`,
//! `LOGINMANY`. `cltGameMsgResponse_ServerTime` is a bare
//! `cltGameMsgResponse_ServerTime{ _SYSTEMTIME sTime }`, and `_SYSTEMTIME` is
//! the Win32 `SYSTEMTIME` -- eight `u16` in the order year, month, **day of
//! week**, day, hour, minute, second, millisecond. The captured body confirms
//! it exactly: `00 00 | ea 07 | 0a 00 | 00 00 | 04 00 | 0f 00 | 38 00 |
//! 0c 00 | d6 01` reads as 2026-10-04 15:56:12.470 with day-of-week 0, and
//! 2026-10-04 was a Sunday.

use gz_proto::opcodes_game::GameMsg;

/// The single 76-byte guild-status record, repeated [`GUILD_STATUS_RECORDS`]
/// times.
///
/// All 35 captured copies are **byte-identical** (verified 2026-10-05 by
/// comparing every one of the 35 against the first), so this is one record and
/// a count rather than a table. The record carries the session character id at
/// [`GUILD_CHAR_UNIQUE_OFFSET`] and the fixed category string `SpecialNeeds`
/// at offset 44; everything else is constant.
const GUILD_STATUS: [u8; GUILD_STATUS_LEN] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x31, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x02, 0x2a, 0xb3, 0x00, 0x20, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x53, 0x70, 0x65, 0x63,
    0x69, 0x61, 0x6c, 0x4e, 0x65, 0x65, 0x64, 0x73, 0x00, 0x01, 0x69, 0x06, 0x1e, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

const GUILD_STATUS_LEN: usize = 76;

/// How many times the reference server sent the guild-status record. 35 is the
/// count in the capture, not a limit the legacy states anywhere, so it is
/// pinned to what the client was actually sent rather than guessed at.
const GUILD_STATUS_RECORDS: usize = 35;

/// Offset of the `u16` session character id inside [`GUILD_STATUS`]. Confirmed
/// against the capture: every one of the 35 records holds `bf 05` here, which
/// is `0x05BF` = 1471 = the capture's `CAPTURED_CHAR_UNIQUE`.
const GUILD_CHAR_UNIQUE_OFFSET: usize = 72;

/// The item-type table head. `SETITEMTYPEINFO` is world configuration, not
/// character state, and the two leading `u16` (20, 3) are the only non-zero
/// bytes in the 20-byte body.
const ITEM_TYPE_INFO: [u8; 20] = [
    0x14, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00,
];

/// The client-file CRC acknowledgement. One `u32`, always 1: the client sends
/// its own CRC table first and the server answers "received".
const FILE_CRC_ACK: [u8; 4] = [0x01, 0x00, 0x00, 0x00];

/// A Win32 `SYSTEMTIME`, the body of `GAMEMSG_RESPONSE_SERVERTIME`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerTime {
    pub year: u16,
    pub month: u16,
    /// 0 is Sunday. Sent because `SYSTEMTIME` carries it; the client ignores it,
    /// but dropping the field would shift every value after it.
    pub day_of_week: u16,
    pub day: u16,
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
    pub millisecond: u16,
}

impl ServerTime {
    /// Splits a Unix timestamp into calendar fields in **UTC**.
    ///
    /// UTC rather than local time deliberately: the server clock is shared state
    /// and must not depend on the machine's zone, and the captured frame reads
    /// as a plain local timestamp, so UTC keeps generation reproducible on any
    /// host while staying within a day of whatever the legacy produced.
    pub fn from_unix_seconds(seconds: u64) -> Self {
        let days = (seconds / 86_400) as i64;
        let rem = seconds % 86_400;
        let (hour, minute, second) = (rem / 3_600, (rem % 3_600) / 60, rem % 60);
        let (year, month, day) = civil_from_days(days);
        // 1970-01-01 was a Thursday, which is weekday 4.
        let day_of_week = (days + 4).rem_euclid(7) as u16;
        Self {
            year: year as u16,
            month: month as u16,
            day_of_week,
            day: day as u16,
            hour: hour as u16,
            minute: minute as u16,
            second: second as u16,
            millisecond: 0,
        }
    }
}

/// Days since the Unix epoch to `(year, month, day)`.
///
/// Howard Hinnant's `civil_from_days`, which is exact for the whole proleptic
/// Gregorian range. Written out rather than pulled in because it is 12 lines
/// and the alternative is a date dependency for one function.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The inputs the sequence needs that are not world constants.
pub struct WorldEntry<'a> {
    /// The session character id, written into every record that carries it.
    ///
    /// This is the **session's** id, not the character's durable
    /// `characters.game_unique`, and that is deliberate: the legacy's
    /// `GetCharUnique()` returns the session's value too, and 45 captured
    /// offsets all hold the same id. Pinning per-character records to
    /// `characters.game_unique` instead is what would make the client's
    /// `SetCharReservedChar` drop them.
    pub char_unique: u16,

    /// The clock behind `SERVERTIME`.
    pub now: ServerTime,

    /// `GAMEMSG_RESPONSE_PERSONITEMINFO`, 104 bytes.
    ///
    /// **The one frame still taken from the capture.** The 104-byte body holds
    /// an item count and a packed item array whose stride is not yet mapped, so
    /// deriving it from `character_items` is the next slice of this card. It is
    /// a parameter rather than hidden inside this module so that the remaining
    /// capture dependency is one named argument rather than a fallback buried
    /// in a generator, and so the length-pinning test below still covers it.
    pub person_item_info: &'a [u8],
}

/// Builds the 45-frame world-entry sequence, in the order the reference server
/// sent it.
///
/// # Panics
///
/// Panics if [`WorldEntry::person_item_info`] is not the expected 104 bytes,
/// which would otherwise be sent as a body the client cannot parse. That is a
/// programming error in the caller, not a runtime condition.
pub fn prologue(input: &WorldEntry<'_>) -> Vec<(u16, Vec<u8>)> {
    assert_eq!(
        input.person_item_info.len(),
        PERSON_ITEM_INFO_LEN,
        "PERSONITEMINFO must be {PERSON_ITEM_INFO_LEN} bytes"
    );
    let ServerTime {
        year,
        month,
        day_of_week,
        day,
        hour,
        minute,
        second,
        millisecond,
    } = input.now;

    let mut out: Vec<(u16, Vec<u8>)> = Vec::with_capacity(45);

    // Country/war state: this character is in no country war.
    out.push((
        GameMsg::ResponseCountryinfo as u16,
        vec![0u8; COUNTRY_INFO_LEN],
    ));
    // A one-byte UI flag, off.
    out.push((
        GameMsg::ResponseSetshowpuchikomi as u16,
        vec![0u8; SHOW_PUCHIKOMI_LEN],
    ));

    // The guild-status block. 35 identical records, each stamped with this
    // session's character id.
    for _ in 0..GUILD_STATUS_RECORDS {
        let mut record = GUILD_STATUS.to_vec();
        record[GUILD_CHAR_UNIQUE_OFFSET..GUILD_CHAR_UNIQUE_OFFSET + 2]
            .copy_from_slice(&input.char_unique.to_le_bytes());
        out.push((GameMsg::ResponseGuildSetguildstatus as u16, record));
    }

    // Village event NPCs: none active.
    out.push((
        GameMsg::ResponseVillagehaveeventnpc as u16,
        vec![0u8; EVENT_NPC_LEN],
    ));
    // Item-type table: world configuration.
    out.push((
        GameMsg::ResponseSetitemtypeinfo as u16,
        ITEM_TYPE_INFO.to_vec(),
    ));
    // Transform table: this character is not a transformed creature, and the
    // whole 772-byte table is empty in the capture.
    out.push((GameMsg::ResponseTransform as u16, vec![0u8; TRANSFORM_LEN]));
    // Client file-CRC acknowledgement.
    out.push((GameMsg::ResponseFilecrccheck as u16, FILE_CRC_ACK.to_vec()));

    // `SYSTEMTIME`. The first `u16` is `wYear`, so year 2026 is 0x07ea --
    // which is why the captured body starts `ea 07` and not a BCD pair.
    let mut time = Vec::with_capacity(SERVER_TIME_LEN);
    for field in [
        year,
        month,
        day_of_week,
        day,
        hour,
        minute,
        second,
        millisecond,
    ] {
        time.extend_from_slice(&field.to_le_bytes());
    }
    debug_assert_eq!(time.len(), SERVER_TIME_LEN);
    out.push((GameMsg::ResponseServertime as u16, time));

    // The character's inventory -- still the capture's, see the field's docs.
    out.push((
        GameMsg::ResponsePersoniteminfo as u16,
        input.person_item_info.to_vec(),
    ));

    // Friend list. **Deliberately all zeros even though the capture is not.**
    // The captured 5,100-byte body is 19.5% non-zero: it is the reference
    // character's actual friends. Replaying it would show every account someone
    // else's friends, which is both wrong and a data leak. We model no friends,
    // so the honest body is 5,100 zero bytes.
    out.push((
        GameMsg::ResponseFriendalllist as u16,
        vec![0u8; FRIEND_LIST_LEN],
    ));

    // Character options: none set.
    out.push((
        GameMsg::ResponseCharGetOption as u16,
        vec![0u8; CHAR_OPTION_LEN],
    ));

    debug_assert_eq!(out.len(), 45);
    out
}

const COUNTRY_INFO_LEN: usize = 8;
const SHOW_PUCHIKOMI_LEN: usize = 1;
const EVENT_NPC_LEN: usize = 16;
const TRANSFORM_LEN: usize = 772;
const SERVER_TIME_LEN: usize = 16;
const PERSON_ITEM_INFO_LEN: usize = 104;
const FRIEND_LIST_LEN: usize = 5_100;
const CHAR_OPTION_LEN: usize = 6;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generated_capture;

    /// A stand-in for the captured `PERSONITEMINFO` body, so the tests below can
    /// exercise `prologue` without reaching for the capture at every call site.
    const ITEM_INFO_STUB: [u8; PERSON_ITEM_INFO_LEN] = [0u8; PERSON_ITEM_INFO_LEN];

    fn entry(char_unique: u16) -> WorldEntry<'static> {
        WorldEntry {
            char_unique,
            now: ServerTime {
                year: 2026,
                month: 10,
                day_of_week: 4,
                day: 5,
                hour: 21,
                minute: 46,
                second: 27,
                millisecond: 500,
            },
            person_item_info: &ITEM_INFO_STUB,
        }
    }

    /// The load-bearing test. Every generated frame must match the captured one
    /// on both opcode and length, position for position. A wrong length is the
    /// failure the client would absorb without complaint, so it is pinned here
    /// rather than by review.
    #[test]
    fn generated_prologue_matches_the_capture_frame_for_frame() {
        let generated = prologue(&entry(0x05BF));
        let captured: Vec<(u16, usize)> = generated_capture::frames_in(21, 66)
            .map(|(cmd, body)| (cmd, body.len()))
            .collect();

        assert_eq!(generated.len(), captured.len(), "frame count");
        for (i, ((got_op, got_body), &(want_op, want_len))) in
            generated.iter().zip(&captured).enumerate()
        {
            assert_eq!(
                (*got_op, got_body.len()),
                (want_op, want_len),
                "frame {i} of the world-entry prologue"
            );
        }
    }

    #[test]
    fn every_length_is_pinned_to_its_own_constant() {
        // Guards against a constant being edited without regenerating the body
        // that uses it: `vec![0u8; N]` would otherwise follow N silently.
        let generated = prologue(&entry(1));
        let expected: &[(u16, usize)] = &[
            (GameMsg::ResponseCountryinfo as u16, COUNTRY_INFO_LEN),
            (GameMsg::ResponseSetshowpuchikomi as u16, SHOW_PUCHIKOMI_LEN),
            (
                GameMsg::ResponseGuildSetguildstatus as u16,
                GUILD_STATUS_LEN,
            ),
            (GameMsg::ResponseVillagehaveeventnpc as u16, EVENT_NPC_LEN),
            (
                GameMsg::ResponseSetitemtypeinfo as u16,
                ITEM_TYPE_INFO.len(),
            ),
            (GameMsg::ResponseTransform as u16, TRANSFORM_LEN),
            (GameMsg::ResponseFilecrccheck as u16, FILE_CRC_ACK.len()),
            (GameMsg::ResponseServertime as u16, SERVER_TIME_LEN),
            (GameMsg::ResponsePersoniteminfo as u16, PERSON_ITEM_INFO_LEN),
            (GameMsg::ResponseFriendalllist as u16, FRIEND_LIST_LEN),
            (GameMsg::ResponseCharGetOption as u16, CHAR_OPTION_LEN),
        ];
        for (op, len) in expected {
            let matches: Vec<&(u16, Vec<u8>)> =
                generated.iter().filter(|(gop, _)| gop == op).collect();
            assert!(!matches.is_empty(), "no frame for opcode {op}");
            for (_, body) in matches {
                assert_eq!(body.len(), *len, "opcode {op}");
            }
        }
    }

    #[test]
    fn the_guild_block_is_35_records_that_differ_only_in_the_character_id() {
        let a = prologue(&entry(0x05BF));
        let b = prologue(&entry(0x1234));

        let guilds = |frames: &[(u16, Vec<u8>)]| -> Vec<Vec<u8>> {
            frames
                .iter()
                .filter(|(op, _)| *op == GameMsg::ResponseGuildSetguildstatus as u16)
                .map(|(_, body)| body.clone())
                .collect()
        };
        let (ga, gb) = (guilds(&a), guilds(&b));
        assert_eq!(ga.len(), GUILD_STATUS_RECORDS);
        assert_eq!(ga.len(), gb.len());

        for (i, (x, y)) in ga.iter().zip(&gb).enumerate() {
            let differing: Vec<usize> = x
                .iter()
                .zip(y)
                .enumerate()
                .filter(|(_, (p, q))| p != q)
                .map(|(off, _)| off)
                .collect();
            assert_eq!(
                differing,
                vec![GUILD_CHAR_UNIQUE_OFFSET, GUILD_CHAR_UNIQUE_OFFSET + 1,],
                "guild record {i} differs outside the character-id field"
            );
            // And the id really was written, little-endian.
            assert_eq!(
                &x[GUILD_CHAR_UNIQUE_OFFSET..GUILD_CHAR_UNIQUE_OFFSET + 2],
                &[0xBF, 0x05]
            );
            assert_eq!(
                &y[GUILD_CHAR_UNIQUE_OFFSET..GUILD_CHAR_UNIQUE_OFFSET + 2],
                &[0x34, 0x12]
            );
        }
    }

    #[test]
    fn the_generated_guild_record_is_byte_identical_to_the_captured_one() {
        // The template was transcribed by hand from the capture; this is what
        // proves the transcription.
        let Some((_, captured)) = generated_capture::frames_in(23, 24).next() else {
            panic!("the capture should hold the first guild record at index 23");
        };
        let generated = prologue(&entry(0x05BF));
        let ours = generated
            .iter()
            .find(|(op, _)| *op == GameMsg::ResponseGuildSetguildstatus as u16)
            .map(|(_, body)| body.as_slice())
            .expect("a guild record");
        assert_eq!(ours, captured);
    }

    #[test]
    fn the_friend_list_is_empty_rather_than_the_capture_characters_friends() {
        let Some((_, captured)) = generated_capture::frames_in(64, 65).next() else {
            panic!("the capture should hold FRIENDALLLIST at index 64");
        };
        // Prove the premise before asserting on it: if the captured body were
        // already all zeros, "we generate zeros" would be a vacuous claim.
        assert!(
            captured.iter().any(|&b| b != 0),
            "the captured friend list is expected to be non-empty; if it is not, \
             the comment claiming it leaks another character's friends is wrong"
        );
        let generated = prologue(&entry(1));
        let (_, body) = generated
            .iter()
            .find(|(op, _)| *op == GameMsg::ResponseFriendalllist as u16)
            .expect("a friend-list frame");
        assert!(body.iter().all(|&b| b == 0), "friend list must be empty");
    }

    #[test]
    fn server_time_is_eight_little_endian_u16s_in_systemtime_order() {
        let now = ServerTime {
            year: 2026,
            month: 10,
            day_of_week: 0,
            day: 4,
            hour: 15,
            minute: 56,
            second: 12,
            millisecond: 470,
        };
        let mut frame = entry(1);
        frame.now = now;
        let generated = prologue(&frame);
        let (_, body) = generated
            .iter()
            .find(|(op, _)| *op == GameMsg::ResponseServertime as u16)
            .expect("a server-time frame");

        let want: Vec<u8> = [2026u16, 10, 0, 4, 15, 56, 12, 470]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        assert_eq!(body, &want);
        // And byte-for-byte the body the capture holds, which is how the
        // day-of-week slot was identified in the first place.
        let Some((_, captured)) = generated_capture::frames_in(62, 63).next() else {
            panic!("the capture should hold SERVERTIME at index 62");
        };
        assert_eq!(body, captured);
    }

    #[test]
    fn unix_seconds_becomes_a_calendar_date_and_weekday() {
        // 2026-10-04T15:56:12Z. Chosen because it is the moment the capture was
        // taken, and its weekday (Sunday == 0) is the independent check on the
        // day-of-week field.
        let t = ServerTime::from_unix_seconds(1_791_129_372);
        assert_eq!((t.year, t.month, t.day), (2026, 10, 4));
        assert_eq!((t.hour, t.minute, t.second), (15, 56, 12));
        assert_eq!(t.day_of_week, 0, "2026-10-04 was a Sunday");

        // The epoch itself, and a leap day, for the arithmetic.
        let e = ServerTime::from_unix_seconds(0);
        assert_eq!((e.year, e.month, e.day, e.day_of_week), (1970, 1, 1, 4));
        let leap = ServerTime::from_unix_seconds(1_709_164_800);
        assert_eq!((leap.year, leap.month, leap.day), (2024, 2, 29));
    }

    #[test]
    #[should_panic(expected = "PERSONITEMINFO must be 104 bytes")]
    fn a_wrong_sized_inventory_frame_is_rejected_at_build_time() {
        let short = [0u8; 10];
        let bad = WorldEntry {
            person_item_info: &short,
            ..entry(1)
        };
        let _ = prologue(&bad);
    }
}
