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

const GUILD_STATUS_LEN: usize = 76;

/// How many times the reference server sent the guild-status record. 35 is the
/// count in the capture, not a limit the legacy states anywhere, so it is
/// pinned to what the client was actually sent rather than guessed at.
const GUILD_STATUS_RECORDS: usize = 35;

/// Offset of the `SI32` session character id inside the guild-status record,
/// the last field of `cltGameMsgResponse_Guild_SetGuildStatus`
/// (`MsgType-Guild.h:490`). Confirmed against the capture: every one of the 35
/// records holds `bf 05 00 00` here, which is `0x05BF` = 1471 = the capture's
/// `CAPTURED_CHAR_UNIQUE`.
const GUILD_CHAR_UNIQUE_OFFSET: usize = 72;

/// Offset of `cltSimpleGuildRank.uiConfirm` (a `UI08` at struct offset 25).
///
/// Derived from the declaration (`CommonHeader.h:1164-1173`) with
/// `GUILD_MAXNAMENUM` = 13 (`CommonHeader.h:1158`): `siType` + 3 pad, two
/// `SI32`, `szGuildName[13]`, then `uiConfirm`. The capture confirms it — 44
/// holds `SpecialNeeds\0` and the byte at 57 holds `01` = `GUILDSTATUS_NORMAL`
/// (`Guild.h:144`), with `/Zp8` padding after it.
const GUILD_UI_CONFIRM_OFFSET: usize = 57;

/// `cltSimpleGuildRank::Init()`'s value for `uiConfirm` (`Common.cpp:424`).
///
/// The legacy reuses the *rank-type* constant here rather than
/// `GUILDSTATUS_STANDING`; the client's `IsGuildMember()` never reads it for a
/// non-member because `siGuildUnitIndex <= 0` already returned `false`
/// (`Common.cpp:520-535`), so the value is sent as `Init()` writes it rather
/// than "corrected".
const GUILDRANKTYPE_STANDING: u8 = 3;

/// The 76-byte body of `GAMEMSG_RESPONSE_GUILD_SETGUILDSTATUS` for a character
/// in **no guild**: an all-zero `cltGuildStatus`, `cltSimpleGuildRank::Init()`,
/// and this session's character id.
///
/// # Why this is not the captured record
///
/// The capture's 35 copies carry the reference character's *real* guild, and
/// generating them is what made VIT read 54. The record is
/// `cltGuildStatus clinfo` (32 bytes) + `cltSimpleGuildRank clGuild` (40) +
/// `SI32 siCharUnique` (4) (`MsgType-Guild.h:485-490`), and the captured bytes
/// say:
///
/// | offset | value | meaning |
/// |---|---|---|
/// | 8 | **49** | `clinfo.siVit` — `cltBasicAbility` is str, dex, **vit**, mag, hnd, luk, wis (`CommonHeader.h:2746-2752`) |
/// | 32 | 2 | `siType` = `GUILDRANKTYPE_MEMBER` (`CommonHeader.h:1155`) |
/// | 36 | 32 | `siVillageUnique` |
/// | 40 | 4 | `siGuildUnitIndex` |
/// | 44 | `SpecialNeeds` | `szGuildName` |
/// | 57 | 1 | `uiConfirm` = `GUILDSTATUS_NORMAL` |
///
/// So all four gates of `IsGuildMember()` (`Common.cpp:520-535`) pass, the
/// client takes the `Set` branch (`Client-Server\Client-Guild.cpp:327-337`),
/// and `clGuildBA.Vit` becomes 49. `clTotalBA` adds that term **raw**
/// (`Parametabox.cpp:4379`) even though the status window prints it with a `%`
/// (`NCharPerson.cpp:3237`), so `5 + 49 = 54` and
/// `54 * (8000 + 1*5) / 1000 = 432` (`SetHitPoint`, `Parametabox.cpp:3374`) —
/// the reported "VIT 54, HP 40/432", to the digit.
///
/// # What the legacy sends instead
///
/// For a character with no guild this is exactly the legacy's body:
/// `pclGuildtemp.Init()` + `clGuildtemp.Init()` give a zeroed status and a
/// non-member rank (`ServerDB-Person.cpp:3247-3258`). The client then takes
/// the `else` at `Client-Server\Client-Guild.cpp:339-344` and calls `Init()` on
/// both `clGuildRank` and `clGuildBA`, so the VIT term is 0 by construction
/// rather than by luck — which matters because `cltParametaBox::Init()` never
/// touches `clGuildBA`, so an unsent record leaves it as uninitialised memory.
///
/// **Two deliberate deviations:**
///
/// * The legacy sends *one* record; the capture holds **35**. The count stays
///   35 so `generated_prologue_matches_the_capture_frame_for_frame` keeps
///   pinning the frame count and every `(opcode, length)` to the capture, and
///   the duplicates are idempotent — each copy runs the same `Init()`.
/// * Bytes 58 and 59 are `/Zp8` padding `Init()` never writes
///   (`Common.cpp:418-428`), so the legacy's own body holds uninitialised
///   stack there — the same `f7 59`-shaped artefact seen in `PERSONITEMINFO`.
///   We send zeros.
///
/// # Citations
///
/// `IsGuildMember` gates, `cltSimpleGuildRank::Init`, the no-guild send site,
/// and the client's `Set`/`Init` branches are all named above; the arithmetic
/// is pinned in `the_captured_guild_vit_reproduces_the_reported_max_hp`.
#[must_use]
pub fn guild_status(char_unique: u16) -> Vec<u8> {
    let mut record = vec![0u8; GUILD_STATUS_LEN];
    record[GUILD_UI_CONFIRM_OFFSET] = GUILDRANKTYPE_STANDING;
    record[GUILD_CHAR_UNIQUE_OFFSET..GUILD_CHAR_UNIQUE_OFFSET + 4]
        .copy_from_slice(&u32::from(char_unique).to_le_bytes());
    record
}

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
pub struct WorldEntry {
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

    /// `GAMEMSG_RESPONSE_PERSONITEMINFO`, 104 bytes, generated from `character_items`.
    ///
    /// The captured frame is a **single-slot** notification, not an inventory dump:
    /// `siItemPos` is 8, `siOption`/`siChangedNum`/`siExp`/`siPrice` are zero, and
    /// all 84 bytes of `cltItem` are zero, i.e. *"slot 8 is empty"*. That is
    /// consistent with the 249 frame in the same burst, whose slot 8 is one of the
    /// 40 empty slots of 80.
    ///
    /// **Deviation:** bytes 6..8 are `/Zp8` padding between `siChangedNum` and
    /// `siExp`, and the capture holds `f7 59` there. We send zeros. That value is
    /// uninitialised stack: the same two bytes appear at offset 896 of the
    /// char-list frame and inside 248's passport blob, in both cases in regions
    /// the legacy never writes.
    pub person_item_info: Vec<u8>,
}

/// Builds the 45-frame world-entry sequence, in the order the reference server
/// sent it.
///
/// # Panics
///
/// Panics if [`WorldEntry::person_item_info`] is not the expected 104 bytes,
/// which would otherwise be sent as a body the client cannot parse. That is a
/// programming error in the caller, not a runtime condition.
pub fn prologue(input: &WorldEntry) -> Vec<(u16, Vec<u8>)> {
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

    // The guild-status block: 35 identical "this character is in no guild"
    // records, each stamped with this session's character id. `guild_status`
    // says why the capture's bytes are not reused.
    for _ in 0..GUILD_STATUS_RECORDS {
        out.push((
            GameMsg::ResponseGuildSetguildstatus as u16,
            guild_status(input.char_unique),
        ));
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

    // The one slot the reference server announced as empty. Generated; see the
    // field's docs for why the captured padding bytes are not reproduced.
    out.push((
        GameMsg::ResponsePersoniteminfo as u16,
        input.person_item_info.clone(),
    ));

    // Friend list. **Deliberately all zeros even though the capture is not.**
    // The captured 5,100-byte body is 19.5% non-zero, and replaying it would
    // show every account someone else's friends -- both wrong and a data leak.
    // We model no friends, so the honest body is 5,100 zero bytes.
    //
    // An earlier version of this comment blamed those bytes on the reference
    // character's real friend list. That was probably wrong, and the correction
    // is the reason: the non-zero bytes are short runs at irregular offsets
    // (6, 16, 21, 32, 36, 44, ...), and a fixed-stride record array would be
    // regular. A live check on 2026-10-05 settled it from the other side --
    // the reference character had no friends on the live server either. So the
    // likelier explanation is an uninitialised buffer, the same `f7 59` that
    // sits in `PERSONITEMINFO`'s padding and in 248's passport blob. Zeros are
    // right either way; only the reasoning was wrong.
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

/// `siItemPos` in the captured `PERSONITEMINFO`: the slot it announces.
///
/// Slot 8 is empty in the captured 249 item array, so the frame's whole content
/// is "nothing here". It is generated rather than replayed so the same code path
/// reports a populated slot correctly for any other character.
pub const PERSON_ITEM_INFO_SLOT: u16 = 8;
const FRIEND_LIST_LEN: usize = 5_100;
const CHAR_OPTION_LEN: usize = 6;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generated_capture;

    fn entry(char_unique: u16) -> WorldEntry {
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
            person_item_info: gz_proto::item::encode_person_item_info(
                PERSON_ITEM_INFO_SLOT,
                &gz_proto::item::Item::empty(),
            )
            .to_vec(),
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

    /// `cltSimpleGuildRank::IsGuildMember()` (`Common.cpp:520-535`), evaluated on
    /// a captured/generated record rather than asserted as prose, so the test
    /// states the client's own decision and not a byte we chose.
    ///
    /// The four gates in order: `siType == GUILDRANKTYPE_STANDING` (3),
    /// `siGuildUnitIndex <= 0`, `siVillageUnique <= 0`,
    /// `uiConfirm == GUILDSTATUS_STANDING` (0).
    fn is_guild_member(record: &[u8]) -> bool {
        let i32_at =
            |at: usize| i32::from_le_bytes(record[at..at + 4].try_into().expect("an SI32 field"));
        if record[32] == 3 {
            return false;
        }
        if i32_at(40) <= 0 {
            return false;
        }
        if i32_at(36) <= 0 {
            return false;
        }
        record[57] != 0
    }

    /// The deviation this fix exists for, asserted from both sides.
    ///
    /// The capture's record is a *member* of guild `SpecialNeeds` carrying
    /// `siVit` = 49; ours says "no guild". Generating the captured bytes —
    /// which is what both `prologue` and `replay` did — put 49 into
    /// `clGuildBA.Vit`, and `clTotalBA` adds that term raw, which is the
    /// reported `Vit 54`.
    #[test]
    fn the_guild_record_says_no_guild_while_the_capture_carries_a_real_one() {
        let Some((_, captured)) = generated_capture::frames_in(23, 24).next() else {
            panic!("the capture should hold the first guild record at index 23");
        };
        // Prove the premise before asserting on the deviation: if the captured
        // record were already a non-member, "we send a non-member" would be
        // vacuous and the +49 would have to come from somewhere else.
        assert_eq!(
            i32::from_le_bytes(captured[8..12].try_into().unwrap()),
            49,
            "the captured record's cltBasicAbility slot 2 (siVit)"
        );
        assert!(
            is_guild_member(captured),
            "the captured record must be a guild member for this test to mean anything"
        );
        assert_eq!(&captured[44..57], b"SpecialNeeds\0");

        let generated = prologue(&entry(0x1234));
        let guilds: Vec<&[u8]> = generated
            .iter()
            .filter(|(op, _)| *op == GameMsg::ResponseGuildSetguildstatus as u16)
            .map(|(_, body)| body.as_slice())
            .collect();
        assert_eq!(guilds.len(), GUILD_STATUS_RECORDS);
        for record in guilds {
            assert_eq!(record.len(), GUILD_STATUS_LEN);
            // No guild stats at all: `cltGuildStatus` is the first 32 bytes.
            assert!(
                record[..32].iter().all(|&b| b == 0),
                "clinfo must be zeroed, got {:02x?}",
                &record[..32]
            );
            assert!(
                !is_guild_member(record),
                "the client must take the Init branch"
            );
            assert_eq!(
                record[57], GUILDRANKTYPE_STANDING,
                "cltSimpleGuildRank::Init()"
            );
            // Nothing else may be planted into a rank the client reads.
            assert!(record[58..72].iter().all(|&b| b == 0));
            assert_eq!(
                &record[GUILD_CHAR_UNIQUE_OFFSET..GUILD_CHAR_UNIQUE_OFFSET + 4],
                &[0x34, 0x12, 0x00, 0x00]
            );
        }
    }

    /// The arithmetic that makes 49 the *whole* anomaly, pinned so a future
    /// change cannot quietly leave another +49 behind.
    ///
    /// `clTotalBA` sums `clGuildBA` raw (`Parametabox.cpp:4379`) and
    /// `SetHitPoint` turns the total into a maximum at `Parametabox.cpp:3374`:
    /// `siVitHitpoint = (vit * (8000 + level*5)) / 1000`.
    #[test]
    fn the_captured_guild_vit_reproduces_the_reported_max_hp() {
        let Some((_, captured)) = generated_capture::frames_in(23, 24).next() else {
            panic!("the capture should hold the first guild record at index 23");
        };
        let guild_vit = i32::from_le_bytes(captured[8..12].try_into().unwrap());
        let base_vit = 5; // `character_stats.vit DEFAULT 5`, level 1
        let level = 1;
        assert_eq!(
            (base_vit + guild_vit) * (8000 + level * 5) / 1000,
            432,
            "5 + 49 must be exactly the observed maximum, 432"
        );
        // And with the guild term gone, the spec's level-1 figure.
        assert_eq!(base_vit * (8000 + level * 5) / 1000, 40);
    }

    #[test]
    fn the_friend_list_is_empty_rather_than_the_capture_bytes() {
        let Some((_, captured)) = generated_capture::frames_in(64, 65).next() else {
            panic!("the capture should hold FRIENDALLLIST at index 64");
        };
        // Prove the premise before asserting on it: if the captured body were
        // already all zeros, "we generate zeros" would be a vacuous claim.
        assert!(
            captured.iter().any(|&b| b != 0),
            "the captured friend list is expected to be non-empty; if it is not, \
             the zero-fill needs no justification"
        );

        // The shape of the non-zero bytes is why we call them an uninitialised
        // buffer rather than friend records: a fixed-stride array would put them
        // at a regular interval, and these sit at 6, 16, 21, 32, 36, 44, ...
        // This asserts that irregularity, so the diagnosis cannot rot silently
        // if someone re-derives the body.
        let offsets: Vec<usize> = captured
            .iter()
            .enumerate()
            .filter(|&(_, &b)| b != 0)
            .map(|(i, _)| i)
            .collect();
        let first_window = &offsets[..offsets.len().min(8)];
        assert!(
            first_window.windows(2).any(|w| w[1] - w[0] > 4),
            "expected irregular spacing between non-zero runs, got {first_window:?}"
        );

        let generated = prologue(&entry(1));
        let (_, body) = generated
            .iter()
            .find(|(op, _)| *op == GameMsg::ResponseFriendalllist as u16)
            .expect("a friend-list frame");
        assert!(body.iter().all(|&b| b == 0), "friend list must be empty");
    }

    /// `live` sends no letter, and must not grow one without saying so.
    ///
    /// 116 `RESPONSE_LETTER` is per-event in the legacy — every `SendLetterMsg`
    /// call site is driven by a delivery, a guild invite, a rank change, a
    /// village dividend (`ServerDB-Structure.cpp:1326`) — so no event means no
    /// frame, and the generated prologue correctly omits it. The `replay` side is
    /// `withhold_always` in `main.rs`, and it drops 116 for the same reason.
    ///
    /// This asserts the omission on purpose: were `prologue` ever to gain a 116,
    /// the two modes would disagree and the memo dialog would appear only under
    /// `live`. Adding a letter table is the thing that should change this test,
    /// and it should have to delete this line to do so.
    #[test]
    fn the_generated_prologue_sends_no_letter() {
        let generated = prologue(&entry(1));
        assert!(
            !generated
                .iter()
                .any(|(op, _)| *op == GameMsg::ResponseLetter as u16),
            "no event, no letter: if a mail table lands, build a real one here \
             instead of deleting the assertion"
        );
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
        let bad = WorldEntry {
            person_item_info: vec![0u8; 10],
            ..entry(1)
        };
        let _ = prologue(&bad);
    }
}
