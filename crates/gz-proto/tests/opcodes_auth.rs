//! Wire-compatibility spot checks for the generated auth-server opcode table.
//!
//! Every value asserted here comes from blueprint 01 Part A §5.1, not from the
//! generator.

use gz_proto::opcodes_auth::{self, AuthMsg};

/// Blueprint §5.1: requests are the implicit sequence 1..=23.
const REQUEST_BLOCK: &[(&str, u16)] = &[
    ("AUTHMSG_REQUEST_FRONTAUTH", 1),
    ("AUTHMSG_REQUEST_GAMEAUTH", 2),
    ("AUTHMSG_REQUEST_MASTERAUTH", 3),
    ("AUTHMSG_REQUEST_FRONTJOINGAME", 4),
    ("AUTHMSG_REQUEST_GAMELOGOUT", 5),
    ("AUTHMSG_REQUEST_GAMELOGIN", 6),
    ("AUTHMSG_REQUEST_HANAUTH_FRONTAUTH", 7),
    ("AUTHMSG_REQUEST_HANAUTH_GAMEAUTH", 8),
    ("AUTHMSG_REQUEST_PORTEAUTH_FRONTAUTH", 9),
    ("AUTHMSG_REQUEST_PORTEAUTH_GAMEAUTH", 10),
    ("AUTHMSG_REQUEST_TAIWAN_FRONTAUTH", 11),
    ("AUTHMSG_REQUEST_TAIWAN_GAMEAUTH", 12),
    ("AUTHMSG_REQUEST_HANAUTH_GAMERUNAUTH", 13),
    ("AUTHMSG_REQUEST_GMTOOLAUTH", 14),
    ("AUTHMSG_REQUEST_HANGAMEJP_FRONTAUTH", 15),
    ("AUTHMSG_REQUEST_GAMANIAJP_FRONTAUTH", 16),
    ("AUTHMSG_REQUEST_GAMEPOTJP_FRONTAUTH", 17),
    ("AUTHMSG_REQUEST_NDEJP_FRONTAUTH", 18),
    ("AUTHMSG_REQUEST_NEXONJP_FRONTAUTH", 19),
    ("AUTHMSG_REQUEST_ENGLISH_FRONTAUTH", 20),
    ("AUTHMSG_REQUEST_USA_FRONTAUTH", 21),
    ("AUTHMSG_REQUEST_EUROPE_FRONTAUTH", 22),
    ("AUTHMSG_REQUEST_KOREA_FRONTAUTH", 23),
];

/// Blueprint §5.1: responses are the implicit sequence rebased to 1000.
const RESPONSE_BLOCK: &[(&str, u16)] = &[
    ("AUTHMSG_RESPONSE_FRONTAUTH", 1000),
    ("AUTHMSG_RESPONSE_GAMEAUTH", 1001),
    ("AUTHMSG_RESPONSE_MASTERAUTH", 1002),
    ("AUTHMSG_RESPONSE_FRONTJOINGAME", 1003),
    ("AUTHMSG_RESPONSE_GAMELOGOUT", 1004),
    ("AUTHMSG_RESPONSE_GAMELOGIN", 1005),
    ("AUTHMSG_RESPONSE_HANAUTH_FRONTAUTH", 1006),
    ("AUTHMSG_RESPONSE_HANAUTH_GAMEAUTH", 1007),
    ("AUTHMSG_RESPONSE_HANAUTH_GAMERUNAUTH", 1008),
    ("AUTHMSG_RESPONSE_GMTOOLAUTH", 1009),
];

/// Compile-time assertions for the values the T-003 card names.
const _: () = {
    assert!(AuthMsg::RequestEnglishFrontauth.id() == 20);
    assert!(AuthMsg::ResponseFrontauth.id() == 1000);
};

#[test]
fn blueprint_rows_match() {
    assert_eq!(
        REQUEST_BLOCK.len() + RESPONSE_BLOCK.len() + 1,
        opcodes_auth::count(),
        "generated count must equal the number of rows in blueprint Part A 5.1"
    );
    for (legacy, expected) in REQUEST_BLOCK.iter().chain(RESPONSE_BLOCK) {
        assert_eq!(
            opcodes_auth::name(*expected),
            *legacy,
            "opcode {expected} should be `{legacy}`"
        );
    }
    // The heartbeat is the one explicit tail anchor.
    assert_eq!(opcodes_auth::name(10000), "AUTHMSG_REPORT_GAMEHEARTBEAT");
    assert_eq!(AuthMsg::ReportGameheartbeat.id(), 10000);
}

#[test]
fn every_opcode_round_trips_through_both_lookups() {
    for value in 0..=u16::MAX {
        match opcodes_auth::from_id(value) {
            Some(msg) => {
                assert_eq!(msg.id(), value, "{msg:?} reports the wrong id");
                assert_eq!(msg.legacy_name(), opcodes_auth::name(value));
            }
            None => assert_eq!(
                opcodes_auth::name(value),
                "<unknown>",
                "opcode {value} is unmapped but `name` returned a value"
            ),
        }
    }
}

#[test]
fn no_opcode_collides_with_the_compression_flag() {
    for value in 0..=u16::MAX {
        if let Some(msg) = opcodes_auth::from_id(value) {
            assert!(
                msg.id() < 0x8000,
                "{msg:?} collides with the compression flag"
            );
        }
    }
}

#[test]
fn numbering_is_sparse_between_blocks() {
    // 24-999 is the gap between the request and response blocks; 1010-9999 is
    // empty too.
    assert_eq!(opcodes_auth::name(24), "<unknown>");
    assert_eq!(opcodes_auth::from_id(24), None);
    assert_eq!(opcodes_auth::name(9999), "<unknown>");
}

/// Blueprint §5.1: the China-only server has its own reduced `AuthMsg.h`
/// (`Server\AUTHSERVERCHINA\AuthMsg.h`, requests 1-6 / responses 1000-1005) that
/// is explicitly out of scope. Those names are the same, so this test can only
/// assert the count; the guard against generating that variant lives in the
/// `TARGETS` table in `tools/codegen/src/main.rs`.
#[test]
fn table_matches_the_full_variant_not_the_china_one() {
    assert_eq!(
        opcodes_auth::count(),
        34,
        "full auth enum is 34 entries; the China variant would be 12"
    );
}
