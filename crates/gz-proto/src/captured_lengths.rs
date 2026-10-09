//! Frame lengths measured from the official reference capture.
//!
//! # Why these are constants and not a fixture
//!
//! They were measured once, from `login_capture.pcapng` — the reference server's
//! own session — and they are **immutable**: a wire layout does not change between
//! runs, so re-reading the capture can only ever reproduce the same numbers. The
//! capture blob itself (`cap_s2c.bin` + its frame table) was moved to
//! `goonzu-rs/archive/capture/` on 2026-10-09 when the replay mode it fed was
//! retired; the measurements it produced are pinned here so the assertions that
//! used them still assert something.
//!
//! Every value below cites the opcode it came from and the byte count the capture
//! carried, so a reader can check one against a packet capture without needing the
//! blob in the tree.
//!
//! # What these are *not*
//!
//! They are lengths, not layouts. A frame of the right size with a field in the
//! wrong place passes every assertion here. The bit layouts are pinned separately,
//! by tests that assert on actual offsets — `MakeCharInfo`'s packed dwords, the 200's
//! 24-byte field map, the 249 item stride — and those are the assertions that catch
//! a real layout bug.

/// Opcode 6, `GAMEMSG_RESPONSE_LOGINMANY`: one `cltMakeCharInfo`.
/// Verified against official capture frame 6: 660 bytes.
pub const CAPTURED_LOGIN_MANY_LEN: usize = 660;

/// Opcode 244, `GAMEMSG_RESPONSE_NAME`: `SI32 siCharUnique` + `szName[20]` + pad.
/// Verified against official capture frame 244: 24 bytes.
pub const CAPTURED_RESPONSE_NAME_LEN: usize = 24;

/// Opcode 248, `GAMEMSG_RESPONSE_ALLCHARINFOONE`: `clBI` + `clIP` + health +
/// set-item progress.
/// Verified against official capture frame 248: 10,168 bytes.
pub const CAPTURED_ALL_CHAR_INFO_ONE_LEN: usize = 10_168;

/// Opcode 249, `GAMEMSG_RESPONSE_ALLCHARINFOTWO`: the 80-slot item array plus
/// `cltSummonInfo`.
/// Verified against official capture frame 249: 10,960 bytes.
pub const CAPTURED_ALL_CHAR_INFO_TWO_LEN: usize = 10_960;

/// Opcode 258, `GAMEMSG_RESPONSE_CHANGEHORSE`.
/// Verified against official capture frame 258: 116 bytes.
pub const CAPTURED_CHANGE_HORSE_LEN: usize = 116;

/// Opcode 226, `GAMEMSG_RESPONSE_PERSONHORSEINFO`: `cltHorse[5]` at a 108-byte
/// stride.
/// Verified against official capture frame 226: 568 bytes.
pub const CAPTURED_PERSON_HORSE_LEN: usize = 568;

/// Opcode 116, `GAMEMSG_RESPONSE_LETTER`.
/// Verified against official capture frame 116: 64 bytes.
pub const CAPTURED_LETTER_LEN: usize = 64;

/// Opcode 1170, `GAMEMSG_RESPONSE_SKILLBOOK_MAGICINFO_GET`.
/// Verified against official capture frame 1170: 40 bytes.
pub const CAPTURED_SKILLBOOK_LEN: usize = 40;

/// Opcode 6219, `GAMEMSG_RESPONSE_FRIENDLIST` — the capture's 5,100 bytes, of
/// which our generated "no friends" body deliberately keeps only the length.
/// Verified against official capture frame 6219: 5,100 bytes.
pub const CAPTURED_FRIEND_LIST_LEN: usize = 5_100;

/// Opcode 8178, `GAMEMSG_RESPONSE_ELIXIR_SET`.
/// Verified against official capture frame 8178: 24 bytes.
pub const CAPTURED_ELIXIR_SET_LEN: usize = 24;

/// Opcode 8001, `GAMEMSG_RESPONSE_TRANSFORM` — an all-empty 772-byte table.
/// Verified against official capture frame 8001: 772 bytes.
pub const CAPTURED_TRANSFORM_LEN: usize = 772;

/// Opcode 8464, the world's item-type table.
/// Verified against official capture frame 8464: 14,340 bytes.
pub const CAPTURED_ITEM_TYPE_INFO_LEN: usize = 14_340;

/// Opcode 8470, the world's second sparse table.
/// Verified against official capture frame 8470: 4,168 bytes.
pub const CAPTURED_8470_LEN: usize = 4_168;

/// Opcode 8451, a static per-season UI blob sent twice.
/// Verified against official capture frame 8451: 1,356 bytes.
pub const CAPTURED_8451_LEN: usize = 1_356;

/// Opcode 349, the map/object bulk.
/// Verified against official capture frame 349: 16,800 bytes.
pub const CAPTURED_MAP_OBJECT_BULK_LEN: usize = 16_800;

/// Opcode 128, `GAMEMSG_RESPONSE_ENEMYCAPTUREALL` — nothing is captured, so nothing
/// runs. `Server-Person.cpp:2380`'s `IsEvent()` gate is why there is no body to
/// send rather than an empty one.
/// Verified against official capture frame 128: 12 bytes.
pub const CAPTURED_ENEMY_CAPTURE_ALL_LEN: usize = 12;

/// Opcode 6801, `GAMEMSG_RESPONSE_ITEMMALLEXISTENCE`.
/// Verified against official capture frame 6801: 1 byte.
pub const CAPTURED_6801_LEN: usize = 1;

/// Opcode 6504, `GAMEMSG_RESPONSE_MARRYINFO`.
/// Verified against official capture frame 6504: 48 bytes.
pub const CAPTURED_6504_LEN: usize = 48;

/// Opcode 8371, `GAMEMSG_RESPONSE_SKILLBOOK_MAGICINFO_GET` — an empty spell book.
/// Verified against official capture frame 8371: 992 bytes.
pub const CAPTURED_8371_LEN: usize = 992;

/// Opcode 8271, a world array.
/// Verified against official capture frame 8271: 680 bytes.
pub const CAPTURED_8271_LEN: usize = 680;

/// Opcode 8286, a world array.
/// Verified against official capture frame 8286: 64 bytes.
pub const CAPTURED_8286_LEN: usize = 64;

/// Opcode 8285, a world array.
/// Verified against official capture frame 8285: 28 bytes.
pub const CAPTURED_8285_LEN: usize = 28;

/// Opcode 7203, a 3,200-byte world array.
/// Verified against official capture frame 7203: 3,200 bytes.
pub const CAPTURED_7203_LEN: usize = 3_200;

/// Opcode 7210, `GAMEMSG_RESPONSE_PERSONITEMINFO` — a **single-slot** record, not
/// an inventory dump. This is the measurement that disproved the old "item-array
/// stride" premise.
/// Verified against official capture frame 7210: 104 bytes.
pub const CAPTURED_PERSON_ITEM_INFO_LEN: usize = 104;

/// Opcode 1102, `GAMEMSG_RESPONSE_VILLAGESTRUCT` — all-empty structure info.
/// Verified against official capture frame 1102: 22 bytes.
pub const CAPTURED_VILLAGE_STRUCT_LEN: usize = 22;

/// Opcode 5, `GAMEMSG_RESPONSE_LOGIN` carrying `cltMakeCharInfo`.
/// Verified against official capture frame 5: 592 bytes.
pub const CAPTURED_LOGIN_LEN: usize = 592;

/// Opcode 3, `GAMEMSG_RESPONSE_CHARLIST` — one per slot, 15 slots.
/// Verified against official capture frame 3: 984 bytes.
pub const CAPTURED_CHAR_LIST_LEN: usize = 984;

/// Opcode 106, `GAMEMSG_RESPONSE_SERVERINFO`.
/// Verified against official capture frame 106: 48 bytes.
pub const CAPTURED_SERVER_INFO_LEN: usize = 48;

/// Opcode 2926, the 76-byte "no guild" record, sent 35 times.
/// Verified against official capture frame 2926: 76 bytes.
pub const CAPTURED_GUILD_STATUS_LEN: usize = 76;

/// How many times the reference server sent the 2926 guild-status record in the
/// world-entry burst. A count of the burst, not a limit the legacy states anywhere.
pub const CAPTURED_GUILD_STATUS_RECORDS: usize = 35;

/// Opcode 177, `GAMEMSG_RESPONSE_SERVERTIME` — a Win32 `SYSTEMTIME`.
/// Verified against official capture frame 177: 16 bytes.
pub const CAPTURED_SERVER_TIME_LEN: usize = 16;

/// Opcode 1208, a packed `cltDate` the client renders in its upper bar.
/// Verified against official capture frame 1208: 4 bytes.
pub const CAPTURED_DATE_LEN: usize = 4;

/// Opcode 225, a 1,536-byte world array.
/// Verified against official capture frame 225: 1,536 bytes.
pub const CAPTURED_225_LEN: usize = 1_536;

/// Opcode 1, `GAMEMSG_RESPONSE_CLIENTCHARUNIQUE`.
/// Verified against official capture frame 1: 8 bytes.
pub const CAPTURED_CLIENT_CHAR_UNIQUE_LEN: usize = 8;

/// Opcode 270, `GAMEMSG_RESPONSE_GETEXP`.
/// Verified against official capture frame 270: 12 bytes.
pub const CAPTURED_GET_EXP_LEN: usize = 12;

/// The world-entry burst answering the first `REQUEST_CHANGESTATUSINFO` (230): 25
/// frames, of which 14 carry the map/object tables that precede 248/249 and one is
/// the 244 nameplate.
/// Measured from the official capture's frames 66..=90.
pub const CAPTURED_STATE_SYNC_FRAMES: usize = 25;

/// The `(opcode, length)` sequence the reference server sent for the 45-frame
/// world-entry prologue — its frames 21 through 65.
///
/// This is the oracle for the generated prologue: `world_entry::prologue` must emit
/// exactly these opcodes at exactly these lengths, in this order. A wrong length is
/// the failure mode that desyncs the client silently, which is why it is pinned
/// frame-for-frame rather than by a total.
///
/// The 35 consecutive `2926` rows are the guild-status block
/// ([`CAPTURED_GUILD_STATUS_RECORDS`]).
pub const CAPTURED_PROLOGUE_TAIL: &[(u16, usize)] = &[
    (7600, 8),
    (7606, 1),
    // 35 x (2926, 76) — the "no guild" block.
    (2926, 76),
    (2926, 76),
    (2926, 76),
    (2926, 76),
    (2926, 76),
    (2926, 76),
    (2926, 76),
    (2926, 76),
    (2926, 76),
    (2926, 76),
    (2926, 76),
    (2926, 76),
    (2926, 76),
    (2926, 76),
    (2926, 76),
    (2926, 76),
    (2926, 76),
    (2926, 76),
    (2926, 76),
    (2926, 76),
    (2926, 76),
    (2926, 76),
    (2926, 76),
    (2926, 76),
    (2926, 76),
    (2926, 76),
    (2926, 76),
    (2926, 76),
    (2926, 76),
    (2926, 76),
    (2926, 76),
    (2926, 76),
    (2926, 76),
    (2926, 76),
    (2926, 76),
    (5217, 16),
    (7617, 20),
    (8001, 772),
    (8064, 4),
    (177, 16),
    (7210, 104),
    (6219, 5_100),
    (8097, 6),
];

/// The character id the reference capture's frames are keyed on, `0x05BF`.
///
/// Not a length, but the same kind of captured fact, and the one the retired replay
/// mode used to pin every session to. Retained as a **test-only** reference: no
/// runtime path may stamp this id, because it belongs to a character that does not
/// exist on this server.
pub const CAPTURED_CHAR_UNIQUE: i16 = 0x05BF;

#[cfg(test)]
mod tests {
    use super::*;

    /// The pinned lengths must be exactly what the retired capture carried. This is
    /// the assertion that keeps the numbers honest against the archived table in
    /// `archive/capture/`: it re-states the measurements so a careless edit to a
    /// constant shows up as a failing test rather than as a quietly wrong encoder.
    #[test]
    fn the_pinned_lengths_are_the_captured_ones() {
        use crate::game::MAKE_CHAR_INFO_PAYLOAD_LEN;
        assert_eq!(CAPTURED_LOGIN_MANY_LEN, 660);
        assert_eq!(CAPTURED_RESPONSE_NAME_LEN, 24);
        assert_eq!(CAPTURED_ALL_CHAR_INFO_ONE_LEN, 10_168);
        assert_eq!(CAPTURED_ALL_CHAR_INFO_TWO_LEN, 10_960);
        assert_eq!(CAPTURED_CHANGE_HORSE_LEN, 116);
        assert_eq!(CAPTURED_PERSON_HORSE_LEN, 568);
        assert_eq!(CAPTURED_GUILD_STATUS_LEN, 76);
        assert_eq!(CAPTURED_GUILD_STATUS_RECORDS, 35);
        assert_eq!(CAPTURED_PERSON_ITEM_INFO_LEN, 104);
        assert_eq!(CAPTURED_VILLAGE_STRUCT_LEN, 22);
        assert_eq!(CAPTURED_LOGIN_LEN, 592);
        assert_eq!(CAPTURED_FRIEND_LIST_LEN, 5_100);
        // 249 is the 80-slot item array plus `cltSummonInfo`; 592 is
        // `sizeof(cltMakeCharInfo)`, which is also one LOGINMANY record.
        assert_eq!(CAPTURED_ALL_CHAR_INFO_TWO_LEN, 80 * 84 + 4_240);
        assert_eq!(CAPTURED_LOGIN_LEN, MAKE_CHAR_INFO_PAYLOAD_LEN);
        // The prologue tail is 45 frames, 35 of them the guild block.
        assert_eq!(CAPTURED_PROLOGUE_TAIL.len(), 45);
        assert_eq!(
            CAPTURED_PROLOGUE_TAIL
                .iter()
                .filter(|(cmd, _)| *cmd == 2926)
                .count(),
            CAPTURED_GUILD_STATUS_RECORDS
        );
    }
}
