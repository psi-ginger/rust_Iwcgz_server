//! Wire-compatibility spot checks for the generated front-server opcode table.
//!
//! Every value asserted here comes from blueprint 01 Part A §4.2 (rows 1-24,
//! 1000-1008, 9000-9002, 10000-10002, 11000, 30000), not from the generator.

use gz_proto::opcodes_front::{self, FrontMsg};

/// Blueprint §4.2 first block: `FRONTMSG_REQUEST_LOGIN = 1` runs to
/// `FRONTMSG_REQUEST_KOREA_LOGIN = 24`.
const REQUEST_BLOCK: &[(&str, u16)] = &[
    ("FRONTMSG_REQUEST_LOGIN", 1),
    ("FRONTMSG_REQUEST_NOTICE", 2),
    ("FRONTMSG_REQUEST_WORLDLIST", 3),
    ("FRONTMSG_REQUEST_PATCHINFO", 4),
    ("FRONTMSG_REQUEST_PATCHTEXT", 5),
    ("FRONTMSG_REQUEST_PATCHTEXT2", 6),
    ("FRONTMSG_REQUEST_JOINGAME", 7),
    ("FRONTMSG_HEARTBEAT", 8),
    ("FRONTMSG_REQUEST_NETMARBLEJP_LOGIN", 9),
    ("FRONTMSG_REQUEST_PINGTEST", 10),
    ("FRONTMSG_RESPONSE_PINGTEST", 11),
    ("FRONTMSG_REQUEST_HANAUTH_LOGIN", 12),
    ("FRONTMSG_REQUEST_DIFFHELLMAN_KEY", 13),
    ("FRONTMSG_REQUEST_TAIWAN_LOGIN", 14),
    ("FRONTMSG_REQUEST_PORTEAUTH_LOGIN", 15),
    ("FRONTMSG_REQUEST_HANGAMEJP_LOGIN", 16),
    ("FRONTMSG_REQUEST_GAMANIAJP_LOGIN", 17),
    ("FRONTMSG_REQUEST_GAMEPOTJP_LOGIN", 18),
    ("FRONTMSG_REQUEST_NDEJP_LOGIN", 19),
    ("FRONTMSG_REQUEST_NEXONJP_LOGIN", 20),
    ("FRONTMSG_REQUEST_ENGLISH_LOGIN", 21),
    ("FRONTMSG_REQUEST_USA_LOGIN", 22),
    ("FRONTMSG_REQUEST_EUROPE_LOGIN", 23),
    ("FRONTMSG_REQUEST_KOREA_LOGIN", 24),
];

/// Blueprint §4.2 second block, rebased to 1000.
const RESPONSE_BLOCK: &[(&str, u16)] = &[
    ("FRONTMSG_RESPONSE_LOGIN", 1000),
    ("FRONTMSG_RESPONSE_NOTICE", 1001),
    ("FRONTMSG_RESPONSE_WORLDLIST", 1002),
    ("FRONTMSG_RESPONSE_PATCHINFO", 1003),
    ("FRONTMSG_RESPONSE_PATCHTEXT", 1004),
    ("FRONTMSG_RESPONSE_PATCHTEXT2", 1005),
    ("FRONTMSG_RESPONSE_JOINGAME", 1006),
    ("FRONTMSG_RESPONSE_NETMARBLEJP_LOGIN", 1007),
    ("FRONTMSG_RESPONSE_DIFFHELLMAN_KEY", 1008),
];

/// Blueprint §4.2 later blocks: notify (9000), game-server info (10000),
/// China promoter info (11000), and the session-key packet (30000).
const TAIL_BLOCK: &[(&str, u16)] = &[
    ("FRONTMSG_NOTIFY_MESSAGE", 9000),
    ("FRONTMSG_NOTIFY_WORLDUPDATE", 9001),
    ("FRONTMSG_NOTIFY_WORLDREFRESHALL", 9002),
    ("FRONTMSG_REPORT_GAMEHEARTBEAT", 10000),
    ("FRONTMSG_REQUEST_GAMESERVERINFO", 10001),
    ("FRONTMSG_RESPONSE_GAMESERVERINFO", 10002),
    ("FRONTMSG_RESPONSE_PROMOTER_INFO", 11000),
    ("FRONTMSG_RESPONSE_SECRETKEY", 30000),
];

/// Compile-time assertions for the values the T-003 card names, so a wrong table
/// cannot even build.
const _: () = {
    assert!(FrontMsg::RequestEnglishLogin.id() == 21);
    assert!(FrontMsg::ResponseLogin.id() == 1000);
    assert!(FrontMsg::RequestWorldlist.id() == 3);
    assert!(FrontMsg::ResponseWorldlist.id() == 1002);
    assert!(FrontMsg::RequestNotice.id() == 2);
    assert!(FrontMsg::ResponseNotice.id() == 1001);
};

fn all_blueprint_rows() -> Vec<(&'static str, u16)> {
    REQUEST_BLOCK
        .iter()
        .chain(RESPONSE_BLOCK)
        .chain(TAIL_BLOCK)
        .copied()
        .collect()
}

#[test]
fn blueprint_rows_match() {
    let rows = all_blueprint_rows();
    assert_eq!(
        rows.len(),
        opcodes_front::count(),
        "generated count must equal the number of rows in blueprint Part A 4.2"
    );
    for (legacy, expected) in rows {
        assert_eq!(
            opcodes_front::name(expected),
            legacy,
            "opcode {expected} should be `{legacy}`"
        );
    }
}

/// `FrontMsg.h:59` has a line-commented `FRONTMSG_REQUEST_HANGAMEJP_LOGIN`, but a
/// *real* one of the same name at line 80 in the KHY 0617 block. So the name
/// alone cannot prove the comment was skipped - the numbering can. If the parser
/// honoured the comment, `FRONTMSG_REQUEST_DIFFHELLMAN_KEY` would be 14, not 13.
#[test]
fn commented_out_hangamejp_login_does_not_shift_the_numbering() {
    let count = (0..=u16::MAX)
        .filter_map(opcodes_front::from_id)
        .filter(|m| m.legacy_name() == "FRONTMSG_REQUEST_HANGAMEJP_LOGIN")
        .count();
    assert_eq!(count, 1, "only the live enumerator may be generated");
    assert_eq!(FrontMsg::RequestDiffhellmanKey.id(), 13);
    assert_eq!(FrontMsg::RequestHangamejpLogin.id(), 16);
}

/// Blueprint §1.5: `FRONTMSG_RESPONSE_SECRETKEY = 30000` carries the ITZ cipher
/// keys and must be sent before any encrypted message, or the client will never
/// leave the login screen. It is also the largest opcode on this link.
#[test]
fn secret_key_packet_is_the_highest_opcode() {
    assert_eq!(FrontMsg::ResponseSecretkey.id(), 30000);
    assert_eq!(opcodes_front::name(30000), "FRONTMSG_RESPONSE_SECRETKEY");
    let highest = (0..=u16::MAX)
        .filter_map(opcodes_front::from_id)
        .max_by_key(|m| m.id())
        .expect("table is not empty");
    assert_eq!(highest, FrontMsg::ResponseSecretkey);
}

/// Blueprint §1.5 and §2.1: the same compression flag applies on every link.
#[test]
fn no_opcode_collides_with_the_compression_flag() {
    for value in 0..=u16::MAX {
        if let Some(msg) = opcodes_front::from_id(value) {
            assert!(
                msg.id() < 0x8000,
                "{msg:?} collides with the compression flag"
            );
        }
    }
}

#[test]
fn every_opcode_round_trips_through_both_lookups() {
    for value in 0..=u16::MAX {
        match opcodes_front::from_id(value) {
            Some(msg) => {
                assert_eq!(msg.id(), value, "{msg:?} reports the wrong id");
                assert_eq!(msg.legacy_name(), opcodes_front::name(value));
            }
            None => assert_eq!(
                opcodes_front::name(value),
                "<unknown>",
                "opcode {value} is unmapped but `name` returned a value"
            ),
        }
    }
}

#[test]
fn numbering_is_sparse_between_blocks() {
    // 25-999 is the gap between the login and response blocks.
    assert_eq!(opcodes_front::name(25), "<unknown>");
    assert_eq!(opcodes_front::from_id(25), None);
    // 1009-8999 is empty as well.
    assert_eq!(opcodes_front::name(8999), "<unknown>");
    // Nothing between 11000 and 30000.
    assert_eq!(opcodes_front::name(20000), "<unknown>");
    assert_eq!(opcodes_front::count(), 41);
}
