//! Wire-compatibility spot checks for the generated game opcode table.
//!
//! Every value asserted here comes from an independent source, not from the
//! generator: the T-002 task card and the blueprint's rebase/annotation tables
//! (docs `rewrite-blueprint/01-network-protocol.md` Part B §1.1-§2.5). If the
//! generator ever drifts, these fail.

use gz_proto::opcodes_game::{self, GameMsg};

/// Values named explicitly in the T-002 task card, plus the hand-verified
/// movement and stat-sync opcodes from blueprint Part B §2.4 / §2.5.
const CARD_SPOT_CHECKS: &[(&str, u16)] = &[
    ("GAMEMSG_RESPONSE_CLIENTCHARUNIQUE", 1),
    ("GAMEMSG_REQUEST_LOGIN", 2),
    ("GAMEMSG_RESPONSE_CHARLIST", 3),
    ("GAMEMSG_REQUEST_SELECTCHAR", 4),
    ("GAMEMSG_RESPONSE_LOGIN", 5),
    ("GAMEMSG_REQUEST_MAKENEWCHAR", 7),
    ("GAMEMSG_REQUEST_HEARTBEAT", 100),
    ("GAMEMSG_REQUEST_CHAT", 117),
    ("GAMEMSG_RESPONSE_CHAT", 118),
    ("GAMEMSG_RESPONSE_PERSON_INNERSTATUS", 228),
    ("GAMEMSG_REQUEST_CHANGESTATUSINFO", 230),
    ("GAMEMSG_RESPONSE_CHANGESTATUSINFO", 231),
    ("GAMEMSG_REQUEST_USERORDER", 240),
    ("GAMEMSG_RESPONSE_USERORDER", 241),
];

/// Every explicit rebase in blueprint Part B §1.1 (line -> enumerator -> value).
/// These are the anchors that pin the implicit +1 counting across the whole enum.
const BLUEPRINT_REBASES: &[(&str, u16)] = &[
    ("GAMEMSG_RESPONSE_CLIENTCHARUNIQUE", 1),
    ("GAMEMSG_REQUEST_APEXDATA", 77),
    ("GAMEMSG_REQUEST_HANAUTH_LOGIN", 80),
    ("GAMEMSG_REQUEST_HEARTBEAT", 100),
    ("GAMEMSG_RESPONSE_CHARUPDATE", 200),
    ("GAMEMSG_REQUEST_MAKEITEMINV", 300),
    ("GAMEMSG_REQUEST_GETBYUNGZOINFO", 400),
    ("GAMEMSG_REQUEST_BECHIEF", 1100),
    ("GAMEMSG_REQUEST_CREATEBANKACCOUNT", 1200),
    ("GAMEMSG_REQUEST_STOCKORDERINFO", 1300),
    ("GAMEMSG_REQUEST_GETFROMMARKET", 1400),
    ("GAMEMSG_REQUEST_CHANGEHOUSECONTRACT", 1500),
    ("GAMEMSG_REQUEST_AGRICULTUREAUTOGAIN", 1800),
    ("GAMEMSG_REQUEST_ENTERHUNT", 1900),
    ("GAMEMSG_REQUEST_POSTOFFICE_SENDITEM", 2000),
    ("GAMEMSG_REQUEST_HORSEMARKET_OUTHORSE", 2100),
    ("GAMEMSG_REQUEST_REALESTATEMARKET_CANCELORDER", 2200),
    ("GAMEMSG_REQUEST_SUMMONMARKET_CANCELORDER", 2300),
    ("GAMEMSG_REQUEST_LAND_CHANGECONTRACT", 2500),
    ("GAMEMSG_REQUEST_FEAST_RESERVE", 2600),
    ("GAMEMSG_REQUEST_SUMMONHEROMARKET_CANCELORDER", 2700),
    ("GAMEMSG_REQUEST_MINE_CHANGECONTRACT", 2800),
    ("GAMEMSG_REQUEST_GUILD_CREATEGUILD", 2900),
    ("GAMEMSG_RESPONSE_REALESTATE", 5000),
    ("GAMEMSG_REQUEST_STRINFO", 5100),
    ("GAMEMSG_REQUEST_SELLNPC", 5200),
    ("GAMEMSG_REQUEST_SELLTOFOREIGNMERCHANT", 5300),
    ("GAMEMSG_REQUEST_PRIVATETRADE", 5400),
    ("GAMEMSG_REQUEST_RIDEHORSE", 5500),
    ("GAMEMSG_REQUEST_SETQUESTGOING", 5700),
    ("GAMEMSG_REQUEST_PARTY_CREATEINVITE", 5800),
    ("GAMEMSG_RESPONSE_GAMEMASTERLEVEL", 5900),
    ("GAMEMSG_RESPONSE_NEWS", 6000),
    ("GAMEMSG_REQUEST_FRIENDADD", 6200),
    ("GAMEMSG_REQUEST_PROPOSE_QUESTION", 6500),
    ("GAMEMSG_REQUEST_ITEMMALLEXISTENCE", 6800),
    ("GAMEMSG_REQUEST_GACHAPRODUCT_LIST", 6900),
    ("GAMEMSG_REQUEST_PERSONALSHOPBANNER", 7000),
    ("GAMEMSG_REQUEST_SUMMONFREE", 7100),
    ("GAMEMSG_REQUEST_VILLAGEDETAILINFO", 7200),
    ("GAMEMSG_REQUEST_BLACKARMYAPPLY", 7300),
    ("GAMEMSG_RESPONSE_SETWORLDMONEY_BUYDLG", 7400),
    ("GAMEMSG_REQUEST_TRADESERVER_SET_SELLORDER", 7500),
    ("GAMEMSG_REQUEST_WORLDPASSPORT_SET", 7550),
    ("GAMEMSG_RESPONSE_COUNTRYINFO", 7600),
    ("GAMEMSG_REQUEST_MOFUMOFUEVENT_RESERVE", 7700),
    ("GAMEMSG_REQUEST_NAMINGQUEST_START", 7800),
    ("GAMEMSG_REQUEST_SYSTEMREWARDLIST", 7810),
    ("GAMEMSG_RESPONSE_DEMANDATTEND", 7820),
    ("GAMEMSG_REQUEST_HAPPYBEANQUEST_START", 7900),
    ("GAMEMSG_REQUEST_CREATEEMPIRE", 7950),
    ("GAMEMSG_REQUEST_TRANSFORM", 8000),
    ("GAMEMSG_RESPONSE_TIMEMETERRATE_UPDATE", 8050),
];

/// Compile-time assertions for the values the T-002 card calls out by name, so a
/// wrong table cannot even build.
const _: () = {
    assert!(GameMsg::ResponseClientcharunique.id() == 1);
    assert!(GameMsg::RequestLogin.id() == 2);
    assert!(GameMsg::ResponseCharlist.id() == 3);
    assert!(GameMsg::RequestSelectchar.id() == 4);
    assert!(GameMsg::ResponseLogin.id() == 5);
    assert!(GameMsg::RequestMakenewchar.id() == 7);
    assert!(GameMsg::RequestHeartbeat.id() == 100);
    assert!(GameMsg::RequestChat.id() == 117);
    assert!(GameMsg::ResponseChat.id() == 118);
};

#[test]
fn card_spot_checks_match() {
    for (legacy, expected) in CARD_SPOT_CHECKS {
        let actual = opcodes_game::name(*expected);
        assert_eq!(
            actual, *legacy,
            "opcode {expected} should be `{legacy}`, generated table says `{actual}`"
        );
    }
}

#[test]
fn blueprint_rebase_anchors_match() {
    assert_eq!(
        BLUEPRINT_REBASES.len(),
        53,
        "blueprint Part B §1.1 lists 53 rebases"
    );
    for (legacy, expected) in BLUEPRINT_REBASES {
        let actual = opcodes_game::name(*expected);
        assert_eq!(
            actual, *legacy,
            "rebase anchor {expected} should be `{legacy}`, generated table says `{actual}`"
        );
    }
}

#[test]
fn rebase_anchors_are_strictly_increasing() {
    for pair in BLUEPRINT_REBASES.windows(2) {
        assert!(
            pair[0].1 < pair[1].1,
            "rebases must increase: {} then {}",
            pair[0].0,
            pair[1].0
        );
    }
}

#[test]
fn highest_real_message_and_terminator() {
    // Blueprint Part B §1.1: highest real message is 8437, GAMEMSG_END is 8438.
    assert_eq!(
        opcodes_game::name(8437),
        "GAMEMSG_RESPONSE_VALENTINEDAY_CONFESSION_ANSWER"
    );
    assert_eq!(opcodes_game::name(8438), "GAMEMSG_END");
    assert_eq!(GameMsg::End.id(), 8438);
}

/// The legacy tree has a commented-out block of guild-war enumerators
/// (MsgType.h:1307-1327) that the C compiler skips. They must not appear in the
/// table, and they must not shift the numbering either: the next real
/// enumerator is an explicit rebase back to 5000.
#[test]
fn block_commented_enumerators_are_absent() {
    let generated: Vec<&str> = (0..=u16::MAX)
        .filter_map(opcodes_game::from_id)
        .map(GameMsg::legacy_name)
        .collect();
    for name in [
        "GAMEMSG_REQUEST_GUILD_APPLYWAR",
        "GAMEMSG_RESPONSE_GUILD_APPLYWAR",
        "GAMEMSG_REQUEST_GUILD_CONFIRMWAR",
        "GAMEMSG_RESPONSE_GUILD_CONFIRMWAR",
        "GAMEMSG_RESPONSE_GUILD_STARTWAR",
        "GAMEMSG_REQUEST_GUILD_GETHUNTINGMAP",
        "GAMEMSG_RESPONSE_GUILD_GETHUNTINGMAP",
        "GAMEMSG_REQUEST_GUILD_APPLYWARMEMBER",
        "GAMEMSG_RESPONSE_GUILD_APPLYWARMEMBER",
        "GAMEMSG_REQUEST_GUILD_CONFIRMWARMEMBER",
        "GAMEMSG_RESPONSE_GUILD_CONFIRMWARMEMBER",
        "GAMEMSG_REQUEST_GUILD_CONFIRMSTARTWAR",
        "GAMEMSG_RESPONSE_GUILD_CONFIRMSTARTWAR",
    ] {
        assert!(
            !generated.contains(&name),
            "`{name}` is block-commented out in the legacy header and must not be generated"
        );
    }
    // The rebase that follows the commented block.
    assert_eq!(opcodes_game::name(5000), "GAMEMSG_RESPONSE_REALESTATE");
}

#[test]
fn every_opcode_round_trips_through_both_lookups() {
    for value in 0..=u16::MAX {
        match opcodes_game::from_id(value) {
            Some(msg) => {
                assert_eq!(msg.id(), value, "{msg:?} reports the wrong id");
                assert_eq!(msg.legacy_name(), opcodes_game::name(value));
                assert!(
                    msg.legacy_name().starts_with("GAMEMSG_"),
                    "{msg:?} maps to a non-enum name"
                );
            }
            None => assert_eq!(
                opcodes_game::name(value),
                "<unknown>",
                "opcode {value} is unmapped but `name` returned a value"
            ),
        }
    }
}

#[test]
fn table_size_matches_the_legacy_enum() {
    // 1621 live enumerators: the 1633 `GAMEMSG_` lines in MsgType.h include 12
    // inside the `/* ... */` block at lines 1309-1327, which the compiler skips.
    assert_eq!(opcodes_game::count(), 1621);
}

#[test]
fn numbering_is_sparse_but_monotonic_in_source_order() {
    // Gaps are intentional (the legacy devs reserved ranges); value 12 is unused
    // because the next rebase jumps from 11 straight to 77.
    assert_eq!(opcodes_game::name(11), "GAMEMSG_RESPONSE_LOGOUT");
    assert_eq!(opcodes_game::name(12), "<unknown>");
    assert_eq!(opcodes_game::from_id(12), None);
}

/// `usCmd` is a `UI16` and bit 15 is the LZF compression flag, so every opcode
/// must fit below `0x8000` or it would collide with a compressed packet.
#[test]
fn no_opcode_collides_with_the_compression_flag() {
    for value in 0..=u16::MAX {
        if let Some(msg) = opcodes_game::from_id(value) {
            assert!(
                msg.id() < 0x8000,
                "{msg:?} collides with the compression flag"
            );
        }
    }
    assert_eq!(GameMsg::End.id() & 0x8000, 0);
}
