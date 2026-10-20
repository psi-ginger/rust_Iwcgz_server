use super::*;
use crate::registry::SessionKey;
use async_trait::async_trait;
use gz_db::{CharacterFull, FriendRepository, OwnedFriend, RepositoryError};
use gz_proto::game::{
    GameLogin, MakeCharInfo, encode_all_char_info_one, encode_all_char_info_two,
    encode_change_horse, encode_family_update, encode_get_village_info, encode_login_many_batches,
    encode_person_horse, encode_response_name, encode_skillbook_magic_info,
};
use gz_proto::item::{Item, encode_person_item_info};
use gz_proto::order::BasicCharUpdate;
use gz_proto::personalshop::{ShopTradeData, shop_mode_for_request};
use std::time::Duration;
// The declared payload lengths. Only the tests below assert on them, so they
// are imported here rather than at the top of the file.
use gz_proto::game::{
    ALL_CHAR_INFO_ONE_LEN, ALL_CHAR_INFO_TWO_LEN, BASIC_INFO_LEN, CHANGE_HORSE_LEN,
    CLT_HEALTH_INFO_LEN, CLT_HEALTH_INFO_WIRE_LEN, FAMILY_UPDATE_LEN, GET_VILLAGE_INFO_LEN,
    HEALTH_INFO_OFFSET, HEALTH_POINT_OFFSET, HEALTH_POINT_SEED, PERSON_HORSE_HEADER_LEN,
    PERSON_HORSE_LEN, RESPONSE_NAME_LEN, SKILLBOOK_MAGIC_INFO_LEN,
};

/// The regression this locks down is the 2026-10-07 live failure: the client
/// reopened the character roster on the **same** connection (the exit
/// dialog's character-select button sets `GS_INITCHARLIST` with no server
/// message, so no reconnect happens) and sent `REQUEST_SELECTCHAR` again.
/// The port silently dropped it, so picking another character never entered
/// the world.
///
/// The legacy refuses only the *same* character (`ServerDB-Person.cpp:1763`
/// matches the PersonID and returns); a different slot is a real
/// tear-down-and-reload (`:1770` `CloseAction`, `:1795` `DeleteChar`).
#[test]
fn selectchar_no_ops_the_same_slot_and_switches_a_different_one() {
    // First entry: nothing is bound, so any slot proceeds.
    assert!(admit_select_char(None, 0));
    assert!(admit_select_char(None, 2));
    // Same slot as the already-bound character: silently ignored.
    assert!(!admit_select_char(Some(1), 1));
    assert!(!admit_select_char(Some(0), 0));
    // A different slot is a real switch and must be honoured.
    assert!(admit_select_char(Some(1), 0));
    assert!(admit_select_char(Some(1), 2));
    // Out-of-range slots never reach the predicate; `handle_select_char`
    // rejects them.
}

/// A make-new-char slot at or above the 15-row roster bound would persist a
/// character the roster never lists (`send_char_list` walks exactly 0..15), so
/// it can never be selected or deleted — an invisible, unloseable row. The same
/// bound guards selectchar (auth.rs) and the 9/83 delete pair.
#[test]
fn make_new_char_rejects_slots_at_or_above_the_roster_bound() {
    // All fifteen listable slots are accepted.
    for slot in 0..CHAR_SLOTS {
        assert!(admit_make_new_char_slot(slot as u8), "slot {slot}");
    }
    // Every byte the client could never legitimately send is refused.
    for slot in CHAR_SLOTS..=u8::MAX as i16 {
        assert!(!admit_make_new_char_slot(slot as u8), "slot {slot}");
    }
}

/// A click to attack from a distance hands the walk over at once — even when
/// the player's own ordered walk is still running (the 2026-10-19 live report
/// of the avatar hesitating on the first approach step).
///
/// The client replaces the order in flight on `ORDER_ATTACK`
/// (`clCurOrder.Set(&ReserveOrder)`, `char-order.cpp:226-263`) and applies it
/// locally before sending (`Client.cpp:3297`), so waiting for the old walk to
/// arrive plus a weapon interval walks the avatar away from the monster the
/// client's own copy is already approaching. A running *approach* keeps its
/// accumulator, because the client re-decides `CHARWORKSTEP_DECIDELENGTH` only
/// when the walk arrives.
#[test]
fn an_attack_click_takes_a_running_walk_over_but_a_running_approach_keeps_its() {
    // First click of the fight, from a standstill: the approach starts at once.
    assert!(may_start_approach(Swing::OutOfRange, false, None, 9_001));
    // Click while the player's own ordered walk is in flight — the stutter
    // fix: `approach` names nothing yet, so nothing is being abandoned.
    assert!(may_start_approach(Swing::OutOfRange, true, None, 9_001));
    // A click naming a different monster mid-chase retargets the walk.
    assert!(may_start_approach(
        Swing::OutOfRange,
        true,
        Some(9_002),
        9_001
    ));
    // The approach walk already heading for this target keeps going: re-
    // planning mid-tile would put the server ahead of the avatar it tracks.
    assert!(!may_start_approach(
        Swing::OutOfRange,
        true,
        Some(9_001),
        9_001
    ));
    // A landing swing ends the walk question; a dead target ends the fight.
    assert!(!may_start_approach(Swing::Landed, false, None, 9_001));
    assert!(!may_start_approach(
        Swing::NoTarget,
        true,
        Some(9_001),
        9_001
    ));
}

/// The 2026-10-10 audit's must-fix #1 (`docs/MUST_FIX.md`): the game login
/// checked only that `auth_key` equalled the account idnum and **ignored the
/// password**, so any client that knew (or guessed) an account's `SERIAL` id
/// could enter as that account without passing the front's login. The packet's
/// password is verified against the stored hash now.
#[test]
fn game_login_refuses_a_wrong_password() {
    let hash = gz_db::password::hash_password("secret");
    let login = |key: i32, password: &str| GameLogin {
        auth_key: key,
        userid: "bob".to_owned(),
        password: password.to_owned(),
        game_mode: 0,
    };

    // Correct key and correct password: admitted.
    assert!(admit_game_login(7, &hash, &login(7, "secret")).is_ok());
    // Correct key, wrong password: refused. (The bug: this used to succeed.)
    assert!(admit_game_login(7, &hash, &login(7, "guess")).is_err());
    assert!(admit_game_login(7, &hash, &login(7, "")).is_err());
    // Wrong key, correct password: refused.
    assert!(admit_game_login(7, &hash, &login(8, "secret")).is_err());
}

/// A fully-populated row for the allocation tests. Level 13 with abilities
/// 20/15/20/10/10/10/10 and bonus 5 is exactly the *repaired* dev seed: the
/// total is 100, which is `40 + 12*5`, so the starting 5 points are
/// spendable (at the old level 10 the same row totals 100 against an
/// allowance of 85 and every spend is refused).
fn seeded_tester() -> CharacterFull {
    CharacterFull {
        character_id: 1,
        name: "Tester".into(),
        class_kind: 1,
        gender: 1,
        level: 13,
        max_life: 173,
        max_mana: 40,
        move_speed: 8,
        exp: 0,
        money: 1000,
        map_id: 158,
        pos_x: 27,
        pos_y: 87,
        slot: 0,
        school_id: 0,
        game_unique: Some(1471),
        str: 20,
        dex: 15,
        vit: 20,
        mag: 10,
        hnd: 10,
        luk: 10,
        wis: 10,
        bonus_points: 5,
    }
}

/// B1: the fan carries the same bytes the mover's own socket gets, so the two
/// writers of one frame cannot drift. `fan_char_update` and `send_char_update`
/// both go through `char_update_body`, so this pins that they are one encoder.
///
/// It also pins what the frame actually carries for an observer: the cell, and
/// the move speed. `si_life`/`si_mana` ride along (the struct has no smaller
/// variant) but the client renders no bystander bar from them — the floating
/// gauge is `CR[1]`-only (`NCharPerson.cpp:3920`). The observer's animation
/// comes from `si_x`/`si_y` via the `bselfswitch == false` branch
/// (`Client-Person-BigInfo.cpp:1187-1222`), so those are the fields under test.
#[test]
fn the_observer_fan_carries_the_same_cell_and_speed_as_the_movers_own_frame() {
    let body = char_update_body(1471, &seeded_tester(), 28, 90, false).unwrap();
    let update = BasicCharUpdate::decode(&body).expect("the fan body decodes as a 200");

    assert_eq!(update.si_char_unique, 1471);
    assert_eq!((update.si_x, update.si_y), (28, 90));
    assert_eq!(
        update.si_move_speed, 8,
        "the character's own effective move speed, not a constant"
    );
    // The captured frame's length, so a field added to `BasicCharUpdate`
    // cannot silently change the wire size of every observer's frame.
    assert_eq!(body.len(), gz_proto::order::BASIC_CHAR_UPDATE_LEN);
}

/// The fan reaches observers in the 3×3 near block and never the sender, who
/// already wrote the frame directly (`registry.rs` tests the cell arithmetic;
/// this pins the wiring, including that a walker's own copy is not duplicated).
/// Registers one session in a registry for a fan test and hands back its inbox.
fn registry_join(
    registry: &Registry,
    char_unique: i16,
    name: &str,
    x: i32,
    y: i32,
) -> (SessionKey, tokio::sync::mpsc::Receiver<Outbound>) {
    let key = Registry::next_key();
    let (tx, rx) = tokio::sync::mpsc::channel(crate::registry::OUTBOUND_CAP);
    registry.register(
        key,
        tx,
        std::sync::Arc::new(tokio::sync::Notify::new()),
        SessionInfo {
            char_unique: PlayerCharUnique::try_from_si16(char_unique)
                .expect("test id in the player band"),
            name: name.to_owned(),
            level: 13,
            map_id: 158,
            x,
            y,
            school_id: 0,
            class_kind: 1,
            life: 161,
            move_speed: 12,
            sex: 1,
            gm_level: 0,
        },
    );
    (key, rx)
}

#[test]
fn the_fan_reaches_a_nearby_session_and_skips_the_mover() {
    let registry = Registry::new();
    // Both characters spawn on the palace tile (0009's deliberate choice), so
    // they are in one another's near cell on login.
    let (mover_key, mut rmover) = registry_join(&registry, 1471, "Tester", 27, 87);
    let (_observer_key, mut robeserver) = registry_join(&registry, 1472, "Tester2", 27, 87);
    let (_far_key, mut rfar) = registry_join(&registry, 1473, "Far", 400, 400);

    // `step_movement` updates the registry before fanning, so the observer set
    // is the cell just reached.
    registry.update_pos(mover_key, 28, 90);
    let observers =
        fan_char_update(&registry, mover_key, 1471, &seeded_tester(), 28, 90, false).unwrap();
    assert_eq!(
        observers, 1,
        "Tester2 shares the cell; the far one does not"
    );

    let frame = robeserver.try_recv().expect("the observer gets the 200");
    assert_eq!(frame.cmd, GameMsg::ResponseCharupdate as u16);
    let update = BasicCharUpdate::decode(&frame.payload).expect("observer body decodes");
    assert_eq!((update.si_x, update.si_y), (28, 90));
    assert!(
        rmover.try_recv().is_err(),
        "the mover writes its own copy directly; a queued one could land after a later direct one"
    );
    assert!(
        rfar.try_recv().is_err(),
        "cell (40, 26) is not in the 3x3 block around (2, 6)"
    );
}

/// World entry must send all three per-character state frames. This is the
/// regression test for the 2026-10-09 fault: under `live` no 248/249 was ever
/// sent, because both the char-select push and the 230 arm were gated on
/// the retired replay mode, so the client entered the world with a null
/// per-character record — no name, black HP/MP bars, unable to move.
///
/// Asserted against the *shape* (which opcodes, and that they are the ones that
/// carry `clBI`/`clIP`) rather than against a socket, because the original bug
/// was an absent call, not a wrong body.
#[test]
fn live_world_entry_sends_the_per_character_state_block() {
    // 244 names the avatar, 248 is clBI+clIP, 249 is the item array. Anything
    // missing here is a field the client reads out of a null record.
    for cmd in [
        GameMsg::ResponseName as u16,
        GameMsg::ResponseAllcharinfoone as u16,
        GameMsg::ResponseAllcharinfotwo as u16,
    ] {
        assert!(
            LIVE_CHARACTER_STATE.contains(&cmd),
            "opcode {cmd} carries the character state and must be sent at world entry"
        );
    }
    // Nothing else belongs here: these are the only three the burst frames
    // regenerate from the row, and each is one `regenerate` arm.
    assert_eq!(LIVE_CHARACTER_STATE.len(), 3);
    // The nameplate must precede the block that names the same character, or
    // the client can resolve an id it has not been told the name of yet.
    assert_eq!(
        LIVE_CHARACTER_STATE[0],
        GameMsg::ResponseName as u16,
        "244 names the avatar before 248/249 describe it"
    );
}

/// Two sessions must never share a character id — the property the retired
/// capture mode broke.
///
/// Pinning every session to one id made two clients the same character, so each
/// treated the other's 200 as its own (`bselfswitch`,
/// `Client-Person-BigInfo.cpp:1160-1176`) and wrote the other's life/mana into
/// its own avatar (`:1228-1238`, unconditional). That is a client-side identity
/// collision, which no server-side test can observe — hence it is pinned as a
/// property of the allocator rather than over a socket.
#[test]
fn two_sessions_get_distinct_character_ids() {
    let first = allocate_char_unique();
    let second = allocate_char_unique();
    assert_ne!(
        first, second,
        "two sessions must never share a character id"
    );
    // And every allocation stays inside the player band, which clears both the
    // client's `CharUniqueInfo` table (it indexes by `uiMakeCharUnique`) and
    // the NPC band the monster allocator owns (audit H3).
    for id in [first, second] {
        assert!(
            (1..=u32::from(PLAYER_CHAR_UNIQUE_TOP as u16)).contains(&id),
            "allocation {id} escaped the player band"
        );
    }
    assert_ne!(
        gz_proto::captured_lengths::CAPTURED_CHAR_UNIQUE as u64,
        u64::from(PLAYER_CHAR_UNIQUE_TOP as u16),
        "the retired capture's id is a different character, not the band"
    );
}

/// T6 — H3: the id bands partition the space. Players wrap strictly inside
/// `1..=8499`, and the typed id refuses 0, the NPC band floor and everything
/// above it *before a `SessionInfo` can be built* — a stronger statement than
/// `register` returning false, whose own band check stays as the
/// belt-and-braces behind it (design doc §5.3) — so a session can never
/// shadow a monster's unique.
#[test]
fn player_ids_wrap_inside_the_player_band_and_the_registry_refuses_foreign_ids() {
    let top = u32::from(PLAYER_CHAR_UNIQUE_TOP as u16);
    // Every counter phase across two whole wraps — including the boundary
    // itself, where the old allocator used to special-case a zero. The live
    // composition (fetch_add + this step) is exercised by the distinctness
    // test above; whatever raw value arrives here, the step must land in band.
    for raw in 0..top * 2 {
        let id = band_char_unique(raw);
        assert!(
            (1..=top).contains(&id),
            "raw {raw} yielded {id}, out of band"
        );
    }

    let registry = Registry::new();
    let enter = |char_unique: i16| {
        let (tx, _rx) = tokio::sync::mpsc::channel(crate::registry::OUTBOUND_CAP);
        registry.register(
            Registry::next_key(),
            tx,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            session_info(char_unique, "Band"),
        )
    };
    assert!(enter(1), "the band floor is a legal session id");
    assert!(enter(PLAYER_CHAR_UNIQUE_TOP), "so is the band ceiling");
    assert!(
        PlayerCharUnique::try_from_si16(0).is_err(),
        "0 is the legacy's empty pattern, never a session"
    );
    assert!(
        PlayerCharUnique::try_from_si16(gz_data::npc::NPC_CHAR_UNIQUE_BASE as i16).is_err(),
        "the NPC band floor belongs to the monster allocator"
    );
    assert!(
        PlayerCharUnique::try_from_si16(i16::MAX).is_err(),
        "and so does everything above it"
    );
}

/// LOGINMANY is the frame that makes another player exist, and it must be built
/// from the **registry** (other players) rather than from this session's row.
///
/// Regression test for "nearby avatars are invisible", reported live 2026-10-09.
/// The 200 fan was verified byte-correct on a real socket and still rendered
/// nothing, because the observer had no `CR[id]` for the mover: 6 is the only
/// frame that creates one (`Client-Login.cpp:53-85` -> `PushChar` ->
/// `SetCharReservedChar` -> `SetChar`, `Client-SetChar.cpp:112-138`), and every
/// per-character frame is discarded at `Client-Person-BigInfo.cpp:1166-1169`
/// when `CharUniqueInfo[charunique]` is 0.
#[test]
fn login_many_carries_the_other_players_own_identity() {
    let registry = Registry::new();
    // The observer, already in the world.
    let (observer_key, mut robeserver) = registry_join(&registry, 1472, "Tester2", 27, 87);
    // A newcomer joins the same cell.
    let (newcomer_key, mut rnewcomer) = registry_join(&registry, 1473, "Tester", 27, 87);

    announce_nearby_characters(&registry, newcomer_key, &seeded_tester(), 1473, None).unwrap();

    // The newcomer is told about the observer. Read it off the wire the server
    // actually queued — not off a locally rebuilt body, which would only prove
    // the encoder agrees with itself.
    let frame = rnewcomer
        .try_recv()
        .expect("the newcomer learns who is here");
    assert_eq!(frame.cmd, GameMsg::ResponseLoginmany as u16);
    assert_eq!(
        frame.payload.len(),
        gz_proto::game::MAKE_CHAR_INFO_PAYLOAD_LEN,
        "one packed record, no length prefix"
    );
    let announced = MakeCharInfo::decode(&frame.payload).expect("the record decodes");
    assert_eq!(
        announced.make_char_unique, 1472,
        "the newcomer must be told about the observer's id, not itself"
    );

    // ...and the observer is told about the newcomer. This second direction is
    // what the legacy also does (`ServerMap.cpp:73-150`), and without it the
    // observer would never see the newcomer appear.
    let frame = robeserver
        .try_recv()
        .expect("the observer learns of the newcomer");
    assert_eq!(frame.cmd, GameMsg::ResponseLoginmany as u16);
    let announced = MakeCharInfo::decode(&frame.payload).expect("the record decodes");
    assert_eq!(
        announced.make_char_unique, 1473,
        "the observer must be told about the newcomer's id"
    );
    assert_eq!(
        announced.make_char_kind, 1,
        "uiMakeCharKind is what the client draws the avatar from, not the name"
    );
    assert_eq!((announced.x, announced.y), (27, 87));
    assert_eq!(
        announced.map_index, 158,
        "the map the client must already hold"
    );
    // Never a record of itself: a client re-SetChar-ing its own avatar would
    // reset the character it is already standing on.
    let _ = observer_key;
}

/// A character that leaves the newcomer set gets no record — the near block is
/// the whole point of the frame, and an over-wide fan would put avatars on screen
/// that are not there.
#[test]
fn login_many_excludes_characters_outside_the_near_block() {
    let registry = Registry::new();
    let (newcomer_key, mut rnewcomer) = registry_join(&registry, 1473, "Tester", 27, 87);
    let (_near, mut rnear) = registry_join(&registry, 1472, "Near", 28, 88);
    let (_far, mut rfar) = registry_join(&registry, 1474, "Far", 400, 400);

    announce_nearby_characters(&registry, newcomer_key, &seeded_tester(), 1473, None).unwrap();

    let body = rnewcomer.try_recv().expect("one frame for the near cell");
    let announced = MakeCharInfo::decode(&body.payload).expect("the record decodes");
    assert_eq!(
        announced.make_char_unique, 1472,
        "only the character sharing the near cell"
    );
    assert!(
        rfar.try_recv().is_err(),
        "cell (40, 26) is not in the 3x3 block around (2, 5)"
    );
    // The bystander is told about the newcomer; the distant one is not told at all.
    let frame = rnear.try_recv().expect("the near bystander is told too");
    assert_eq!(frame.cmd, GameMsg::ResponseLoginmany as u16);
}

/// An empty near block sends **nothing**, never a zero-length frame.
///
/// `DoMsg_GAMEMSG_RESPONSE_LOGINMANY` casts the payload and reads `uiType`
/// before it knows it got any bytes (`Client-Login.cpp:61-63`), so an empty body
/// is walked as garbage. This is the legacy's own `if(index)` guard
/// (`ServerMap.cpp:140`), and the reason 6 used to be withheld wholesale.
#[test]
fn an_empty_near_block_sends_no_frame_at_all() {
    assert!(
        encode_login_many_batches(&[]).is_empty(),
        "no records must mean no frame, not a zero-length one"
    );
    let registry = Registry::new();
    let (lonely_key, mut rlonely) = registry_join(&registry, 1471, "Tester", 27, 87);
    announce_nearby_characters(&registry, lonely_key, &seeded_tester(), 1471, None).unwrap();
    assert!(
        rlonely.try_recv().is_err(),
        "alone in the world, nothing is announced"
    );
}

/// A mid-walk cell crossing must announce the mover **only** to the sessions
/// that could not see it a moment ago (`was = Some(..)`), never to a neighbour
/// that has been watching the whole walk.
///
/// Regression for "the other client's avatar walks, then warps back", reported
/// live 2026-10-10. The 6 is not idempotent: `SetCharReservedChar`
/// (`Client-SetChar.cpp:111-119`) finds a `char_unique` it already holds and
/// **deletes that avatar** before re-`SetChar`ing it from the record, so a repeat
/// announcement teleports the avatar to the record's cell and discards the walk
/// order the 241 installed. Afterwards it is driven one tile per 200, which is
/// the "one step, pause, repeat" that came with it.
#[test]
fn a_mid_walk_announce_reaches_only_the_cells_the_mover_just_entered() {
    use gz_proto::map_geometry::cell_of;

    let registry = Registry::new();
    // The mover, mid-walk: `now` is the tile it just stepped onto.
    let (mover_key, mut rmover) = registry_join(&registry, 1471, "Mover", 27, 87);
    // Has been watching the mover the whole time — adjacent before *and* after.
    let (_watcher_key, mut rwatcher) = registry_join(&registry, 1472, "Watcher", 28, 88);
    // Comes into view with this step and no sooner.
    let (_gainer_key, _rgainer) = registry_join(&registry, 1474, "Gainer", 8, 62);

    let now = registry.info(mover_key).unwrap();
    let mut was = now.clone();
    was.x = 38;
    was.y = 92;

    // Pin the geometry rather than assume it, so a future cell-size change
    // fails here instead of silently making the recipient set vacuous.
    assert_eq!(cell_of(was.x, was.y, 158), (2, 4), "the cell it left");
    assert_eq!(cell_of(now.x, now.y, 158), (2, 3), "the cell it entered");
    assert_eq!(
        cell_of(28, 88, 158),
        (2, 3),
        "the watcher shares both cells, so it saw the mover throughout"
    );
    assert_eq!(
        cell_of(8, 62, 158),
        (2, 2),
        "adjacent to the entered cell, two cells from the left one"
    );

    announce_nearby_characters(&registry, mover_key, &seeded_tester(), 1471, Some(&was)).unwrap();

    // The mover learns about the character it just walked into view of...
    let frame = rmover
        .try_recv()
        .expect("the newly-entered cell's occupant is announced to the mover");
    assert_eq!(frame.cmd, GameMsg::ResponseLoginmany as u16);
    assert_eq!(
        MakeCharInfo::decode(&frame.payload)
            .unwrap()
            .make_char_unique,
        1474
    );
    assert!(
        rmover.try_recv().is_err(),
        "the watcher is not re-announced to the mover either"
    );

    // ...and the observer who could already see the walker gets nothing at all.
    // A frame here is the warp-back: the client would delete the avatar it has
    // and rebuild it from the record.
    assert!(
        rwatcher.try_recv().is_err(),
        "an already-watching neighbour must not receive a second LOGINMANY"
    );
}

/// A generated record round-trips through the client's own stride rules.
///
/// This is the assertion that would have caught the bug in `MakeCharInfo::decode`
/// that the first version of the test above walked into: reading the header with
/// a sequential `Reader` skips the 0x07 and 0x11 **pad bytes** and shifts every
/// field after them, so a record decoded as `(768, 769)` instead of `(27, 87)`.
/// A fan built from mis-decoded coordinates would put avatars in the sea.
#[test]
fn a_generated_login_many_record_round_trips_through_its_own_layout() {
    for (unique, kind, level, life, x, y, map, speed) in [
        (1471u16, 1u16, 13u8, 161u32, 27u16, 87u16, 158u16, 12u8),
        (1, 0, 1, 40, 0, 0, 1, 10),
        // 10-bit fields at their maxima, which is where a masking bug shows.
        (19_999, 65_535, 255, 0xF_FFFF, 0x3FF, 0x3FF, 0x3FF, 63),
    ] {
        let record = MakeCharInfo {
            make_char_unique: unique,
            make_char_kind: kind,
            level,
            life,
            x,
            y,
            map_index: map,
            total_move_speed: speed,
            home_village_unique: 0,
            game_master_level: 0,
            map_unique: 0,
            make_ani_type: 0,
        };
        let decoded = MakeCharInfo::decode(&record.encode()).expect("decodes");
        assert_eq!(
            decoded, record,
            "record {unique} did not survive the round trip"
        );
    }
}

/// A record that did not round-trip would be worse than no record at all, so the
/// encode/decode pair is pinned against the *client's* bit layout rather than
/// against itself. The pack sites are the two dwords at 0x08 and 0x0C; a swapped
/// bit order there is silent and would scatter every avatar.
#[test]
fn the_login_many_header_uses_the_clients_bit_order() {
    use gz_proto::game::MAKECHARTYPE_PC;
    // A record with every header field distinct, so a bit-order mix-up in either
    // packed dword shows up as the wrong field rather than a coincidence. The
    // speed is 17, not `seeded_tester`'s 8, precisely to keep the two dwords
    // from agreeing on anything.
    let record = MakeCharInfo {
        make_char_unique: 1471,
        make_char_kind: 9,
        level: 42,
        life: 123_456,
        x: 333,
        y: 777,
        map_index: 158,
        total_move_speed: 17,
        home_village_unique: 3,
        game_master_level: 0,
        map_unique: 0,
        make_ani_type: 0,
    };
    let bytes = record.encode();
    // 0x00 uiType:1 | uiMakeCharKind:16 (low bits first).
    assert_eq!(
        u32::from_le_bytes([bytes[0x00], bytes[0x01], bytes[0x02], bytes[0x03]]),
        (MAKECHARTYPE_PC & 1) | (9 << 1),
        "uiType must be MAKECHARTYPE_PC or the client strides as a Common record"
    );
    // uiMakeCharUnique:16 @ 0x04, uiLevel:8 @ 0x06, pad @ 0x07.
    assert_eq!(u16::from_le_bytes([bytes[0x04], bytes[0x05]]), 1471);
    assert_eq!(bytes[0x06], 42);
    assert_eq!(bytes[0x07], 0, "the pad byte must be zero");
    // 0x08 uiLife:20 | uiX:10.
    let life_x = u32::from_le_bytes([bytes[0x08], bytes[0x09], bytes[0x0A], bytes[0x0B]]);
    assert_eq!(life_x & 0xF_FFFF, 123_456);
    assert_eq!((life_x >> 20) & 0x3FF, 333);
    // 0x0C uiY:10 | uiMapIndex:10 | uiTotalMoveSpeed:6.
    let y_map_speed = u32::from_le_bytes([bytes[0x0C], bytes[0x0D], bytes[0x0E], bytes[0x0F]]);
    assert_eq!(y_map_speed & 0x3FF, 777);
    assert_eq!((y_map_speed >> 10) & 0x3FF, 158);
    assert_eq!((y_map_speed >> 20) & 0x3F, 17);
    // 0x10 siHomeVillageUnique.
    assert_eq!(bytes[0x10], 3);
}

/// A busy cell arrives as a *series* of frames, never one oversized one, and no
/// record is dropped when the batch splits.
#[test]
fn a_login_many_batch_never_exceeds_the_frame_ceiling() {
    let record = seeded_tester();
    let many: Vec<MakeCharInfo> = (0..40)
        .map(|i| {
            let mut m = make_char_info_for(&record, 1473).unwrap();
            m.make_char_unique = u16::try_from(100 + i).unwrap();
            m
        })
        .collect();

    let batches = encode_login_many_batches(&many);
    assert!(batches.len() > 1, "40 records must not fit in one frame");
    let mut total = 0usize;
    for body in batches {
        assert!(
            body.len() <= gz_proto::game::MAX_CLTMSG_DATA_SIZE,
            "batch of {} bytes exceeds MAX_CLTMSG_DATA_SIZE",
            body.len()
        );
        assert!(!body.is_empty(), "a batch is never empty");
        // Records are packed, not length-prefixed: the body is a whole number of
        // them, which is how the client computes its stride.
        assert_eq!(body.len() % gz_proto::game::MAKE_CHAR_INFO_PAYLOAD_LEN, 0);
        total += body.len() / gz_proto::game::MAKE_CHAR_INFO_PAYLOAD_LEN;
    }
    assert_eq!(total, 40, "no record may be dropped when splitting");
}

/// The recalc formulas are the ones the dev seed's stored maxima must match:
/// both reproduce the `0001` DDL defaults exactly at level 1 (vit 5 -> 40,
/// mag 5 -> 20) and the repaired row's stored values at level 13.
#[test]
fn the_derived_maxima_reproduce_the_ddl_defaults_and_the_repaired_seed() {
    // Level 1 is the continuity pin: vit 5 -> 40 is the `0001` DDL default,
    // unchanged by §4.3.4 (its level term is 0 at level 1).
    assert_eq!(derived_max_life(5, 1), 40);
    assert_eq!(derived_max_mana(5, 1), 20);
    // §4.3.4 retunes the level term tenfold (0.005 -> 0.05 per level): the
    // level-13 seed row derives 173 under the reformed curve.
    assert_eq!(derived_max_life(20, 13), 173);
    assert_eq!(derived_max_mana(10, 13), 40);
}

/// Cat's §4.3.8 weight capacity (approved 2026-10-20), overriding both the
/// legacy STR-only `(STR * 200) + (Level * 800)` and the baseline doc's own
/// base-3k variant: `10_000 + Level * 2_000 + max(STR, INT) * 50`.
#[test]
fn weight_capacity_walks_the_directives_l1_and_l100_pins() {
    // Level 1 baseline: 10_000 + 2_000 + 5*50 = 12_250 — the ~12k start.
    assert_eq!(weight_capacity(1, 5, 5), 12_250);
    // Level 100, a full STR build: 10_000 + 200_000 + 25_000 = 235_000 — the
    // top of the directive's ~210k-235k band.
    assert_eq!(weight_capacity(100, 500, 50), 235_000);
    // A full INT build carries the same — the stronger of the two governs,
    // so a caster is not locked out of loot the way the legacy keyed it.
    assert_eq!(weight_capacity(100, 5, 500), 235_000);
    // The stat term is flat once it stops governing.
    assert_eq!(weight_capacity(100, 500, 10), 235_000);
}

/// The DDL invariant `abilities + bonus == level*5 + 35` rides the level-up:
/// the `gz-db` `level_up` UPDATE grants +5 bonus alongside `level + 1`, and
/// that is exactly how far the 250 allowance (`40 + (level-1)*5`, `stats.rs`)
/// grows — so a fully-allocated character stays legal to spend at the new
/// level instead of stranding its fresh bonus.
#[test]
fn a_level_up_keeps_the_ddl_stat_invariant() {
    let total = |character: &CharacterFull| {
        i64::from(character.bonus_points)
            + i64::from(character.str + character.dex + character.vit + character.mag)
            + i64::from(character.hnd + character.luk + character.wis)
    };
    let mut up = seeded_tester(); // level 13: 95 allocated + 5 bonus = 100 = 13*5+35
    assert_eq!(total(&up), i64::from(up.level) * 5 + 35);
    // What `level_up` applies, minus the derived maxima (those are formulas
    // over vit/mag/level and do not enter the invariant).
    up.level += 1;
    up.bonus_points += 5;
    assert_eq!(
        total(&up),
        i64::from(up.level) * 5 + 35,
        "the bonus grant moves the total with the level"
    );
    // The fresh bonus is spendable: the level-14 allowance is 105 = the new
    // total, and the check refuses only `total > allowance`.
    let out = allocate_stat(&up, &UseBonus { pos: 0, amount: 5 })
        .expect("the post-level-up allowance accepts the new bonus");
    assert_eq!((out.str_, out.bonus), (25, 5));
    let spent = i64::from(out.bonus)
        + i64::from(out.str_ + out.dex + out.vit + out.mag + out.hnd + out.luk + out.wis);
    assert_eq!(
        spent,
        i64::from(up.level) * 5 + 35,
        "a spend only moves parts"
    );
}

#[test]
fn allocating_spends_bonus_and_raises_the_stat() {
    let out = allocate_stat(&seeded_tester(), &UseBonus { pos: 0, amount: 5 }).unwrap();
    assert_eq!(out.bonus, 0);
    assert_eq!(out.str_, 25);
    // Vit/mag are untouched, so the derived maxima do not move.
    assert_eq!((out.max_life, out.max_mana), (173, 40));
    // The total is invariant under an allocation: 95 + 5 both ways.
}

#[test]
fn allocating_vit_recomputes_max_life_from_the_formula() {
    let out = allocate_stat(&seeded_tester(), &UseBonus { pos: 3, amount: 5 }).unwrap();
    assert_eq!(out.vit, 25);
    // §4.3.4: vit * (8000 + level*50) / 1000 — 25 * 8650 / 1000 = 216.
    assert_eq!(out.max_life, 25 * (8000 + 13 * 50) / 1000);
    assert_eq!(out.bonus, 0);
}

#[test]
fn allocate_rejects_zero_and_negative_amounts() {
    // `siAmount <= 0` is refused outright: the reset path (item 24035, the
    // `IDC_BUTTON_*2` space) is deferred, and zero is a no-op that must not
    // cheerfully "succeed".
    assert!(allocate_stat(&seeded_tester(), &UseBonus { pos: 0, amount: 0 }).is_err());
    assert!(allocate_stat(&seeded_tester(), &UseBonus { pos: 0, amount: -1 }).is_err());
    // The reset pos space never reaches the amount rule benefit-of-the-doubt.
    assert!(
        allocate_stat(
            &seeded_tester(),
            &UseBonus {
                pos: 1115,
                amount: -1
            }
        )
        .is_err()
    );
}

#[test]
fn allocate_rejects_pos_outside_the_zero_to_six_ability_space() {
    for pos in [-1, 7, 1115] {
        assert!(
            allocate_stat(&seeded_tester(), &UseBonus { pos, amount: 1 }).is_err(),
            "siPos {pos} must be refused"
        );
    }
}

#[test]
fn allocate_rejects_spending_more_than_the_unspent_vault() {
    // The client can only accumulate what the status window shows, but a
    // tampered `siAmount` must not conjure points from the server's vault.
    assert!(allocate_stat(&seeded_tester(), &UseBonus { pos: 0, amount: 6 }).is_err());
}

#[test]
fn allocate_caps_hnd_at_twice_the_level() {
    // Hnd at exactly the level-13 cap (26) with the row's total still 100:
    // 16 points moved out of Str keep the invariant while the cap is at its
    // edge. A spend that would land on 27 must be refused.
    let mut at_cap = seeded_tester();
    at_cap.hnd = 26;
    at_cap.str = 4;
    assert!(allocate_stat(&at_cap, &UseBonus { pos: 4, amount: 1 }).is_err());
    // The remaining headroom is spendable.
    let mut headroom = seeded_tester();
    headroom.hnd = 25;
    headroom.str = 5;
    let out = allocate_stat(&headroom, &UseBonus { pos: 4, amount: 1 }).unwrap();
    assert_eq!(out.hnd, 26);
}

#[test]
fn allocate_refuses_a_row_already_over_the_level_allowance() {
    // The unrepaired seed: the same abilities and bonus at level 10 total
    // 100 against an allowance of 85. The server refuses to let that row
    // spend at all — the soft-fail shape of the legacy's kick.
    let mut character = seeded_tester();
    character.level = 10;
    assert!(allocate_stat(&character, &UseBonus { pos: 0, amount: 1 }).is_err());
}

/// The seven hundred range personal-shop opcodes are the legacy pair, and the
/// reply body opens the dialog at the offsets the client reads
/// (`Client-PersonalShop.cpp:94,117,131`).
#[test]
fn personal_shop_reply_opens_the_dialog() {
    use gz_proto::personalshop::{PSHOP_MODE_SELL, RESPONSE_PERSONALSHOPDATA_SIZE};

    assert_eq!(GameMsg::RequestPersonalshopdata as u16, 7002);
    assert_eq!(GameMsg::ResponsePersonalshopdata as u16, 7003);

    let shop = ShopTradeData {
        shop_mode: shop_mode_for_request(false, false),
        person_id: 7,
        account_id: "Tester".to_owned(),
    };
    let body = shop.encode_response(0x05BF);
    assert_eq!(body.len(), RESPONSE_PERSONALSHOPDATA_SIZE);
    assert_eq!(&body[0..2], &0x05BFu16.to_le_bytes());
    assert_eq!(body[8], PSHOP_MODE_SELL as u8);
    assert_eq!(&body[12..16], &7i32.to_le_bytes());
    assert_eq!(&body[16..22], b"Tester");
}

/// The request flags select the legacy's shop mode
/// (`Server-PersonalShop.cpp:54-68`).
#[test]
fn personal_shop_request_flags_pick_the_legacy_mode() {
    use gz_proto::personalshop::{PSHOP_MODE_MAKEITEM, PSHOP_MODE_MAKEITEM_WANT, PSHOP_MODE_SELL};
    // An ordinary "open the shop" click (both flags false) is a seller shop.
    assert_eq!(shop_mode_for_request(false, false), PSHOP_MODE_SELL);
    assert_eq!(shop_mode_for_request(true, false), PSHOP_MODE_MAKEITEM);
    assert_eq!(shop_mode_for_request(true, true), PSHOP_MODE_MAKEITEM_WANT);
}

#[test]
fn deframe_leaves_an_uncompressed_body_alone() {
    let plain = Frame::new(GameMsg::RequestSelectchar as u16, &7i32.to_le_bytes())
        .unwrap()
        .into_bytes();
    let (cmd, payload) = deframe(&plain).unwrap();
    assert_eq!(cmd, GameMsg::RequestSelectchar as u16);
    assert_eq!(payload.as_ref(), 7i32.to_le_bytes().as_slice());
}

#[test]
fn deframe_decompresses_and_clears_the_flag() {
    // The client compresses game frames >= 200 bytes and sets bit 15
    // (`Client-Winsock.cpp:178-196`); the real client's MAKENEWCHAR arrived
    // as opcode 32775, i.e. 7 | 0x8000.
    let body = vec![0x41u8; 400];
    let mut compressed = vec![0u8; body.len() * 2];
    let n = gz_lzf::compress(&body, &mut compressed).unwrap();
    compressed.truncate(n);

    assert!(compressed.len() < 200, "test body should compress");

    let total = HEADER_SIZE + compressed.len();
    let mut plain = vec![0u8; total];
    plain[..2].copy_from_slice(&(total as u16).to_le_bytes());
    plain[2] = 0;
    plain[3..5]
        .copy_from_slice(&(GameMsg::RequestMakenewchar as u16 | COMPRESSION_FLAG).to_le_bytes());
    plain[HEADER_SIZE..].copy_from_slice(&compressed);

    let (cmd, payload) = deframe(&plain).unwrap();
    assert_eq!(cmd, GameMsg::RequestMakenewchar as u16);
    assert_eq!(payload.as_ref(), body.as_slice());
}

/// The 249 body is exactly the item array plus a zeroed `clSummonInfo`.
///
/// The client's willingness to accept the world-entry burst rests on this
/// frame having the size the reference server sent, so the length is pinned to
/// the capture's measurement. `clSummonInfo` is a documented deviation: the
/// reference character owned summons (115 non-zero bytes in the capture) and we
/// hold no table for them, so ours is honestly zero.
#[test]
fn the_item_array_splice_fills_exactly_the_captured_frame_length() {
    // Every slot empty: the encoder rejects a short slice and expects exactly
    // `MAX_ITEM_PER_PERSON` records, so the "empty bag" case is the full-width
    // one.
    let empty: Vec<gz_proto::item::Item> = vec![Item::empty(); gz_proto::item::MAX_ITEM_PER_PERSON];
    let body = encode_all_char_info_two(&empty).unwrap();
    assert_eq!(
        body.len(),
        gz_proto::captured_lengths::CAPTURED_ALL_CHAR_INFO_TWO_LEN,
        "249 must keep the size the reference server sent"
    );
    // The array itself is 80 whole records; anything left over is `clSummonInfo`
    // and must be zero, or the client walks a summon list that is not ours.
    assert_eq!(
        body.len() - gz_proto::item::ITEM_ARRAY_LEN,
        4_240,
        "the tail is clSummonInfo"
    );
    assert!(
        body[gz_proto::item::ITEM_ARRAY_LEN..]
            .iter()
            .all(|&b| b == 0),
        "clSummonInfo must be zero: we model no summons"
    );
}

/// The generated 248 must place its identity fields where the client reads
/// them, and must not carry the capture's.
///
/// Pinned against the capture rather than by review because the failure is
/// invisible: every field lands somewhere, the frame is the right length, and
/// the client renders a plausible world built from the wrong character.
#[test]
fn generated_all_char_info_one_places_identity_and_stats() {
    let basic = BasicInfo {
        account_idnum: 1471,
        account_id: "tester".into(),
        slot: 2,
        sex: 1,
        class_kind: 0x1234,
        home_village: 0,
        map_index: 35,
        x: 25,
        y: 13,
        character_id: 4242,
        name: "Tester".into(),
        str_: 130,
        dex: 50,
        vit: 5,
        mag: 5,
        hnd: 5,
        luk: 5,
        wis: 5,
        level: 34,
        bonus: 0,
        life: 433,
        mana: 20,
        exp: 349_415,
        money: 1000,
    };
    let body = encode_all_char_info_one(&basic, 20, 3, 0, 0);
    assert_eq!(body.len(), 10_168);

    // `clBI.clPos` @ 32..40 -- the field the client reads its own position
    // back out of (`Char-Common.cpp:1206`).
    assert_eq!(
        i16::from_le_bytes([body[32], body[33]]),
        35,
        "clPos.siMapIndex"
    );
    assert_eq!(i16::from_le_bytes([body[34], body[35]]), 25, "clPos.siX");
    assert_eq!(i16::from_le_bytes([body[36], body[37]]), 13, "clPos.siY");
    // `clBI.clPerson` @ 76..100.
    assert_eq!(i32::from_le_bytes(body[76..80].try_into().unwrap()), 4242);
    assert_eq!(&body[80..86], b"Tester");
    // `clIP.siLevel` @ 932 -- the same offset the char list uses, which is
    // what makes one `BasicInfo` encoder serve both frames.
    assert_eq!(i16::from_le_bytes([body[932], body[933]]), 34);
    assert_eq!(
        i32::from_le_bytes(body[904..908].try_into().unwrap()),
        130,
        "siStr"
    );
    assert_eq!(
        i64::from_le_bytes(body[944..952].try_into().unwrap()),
        349_415
    );
    // `clIP.siMoney` @ 960 -- the last eight bytes of the `clIP` window, so
    // there is no room for a field after it and the offset is self-checking.
    assert_eq!(
        i64::from_le_bytes(body[960..968].try_into().unwrap()),
        1000,
        "siMoney"
    );
    // The three fame fields the legacy seeds rather than loads.
    assert_eq!(i16::from_le_bytes([body[952], body[953]]), 0, "siFameLevel");
    assert_eq!(
        i16::from_le_bytes([body[954], body[955]]),
        1,
        "siSkillBonus -- cltPIPInfo::Init()"
    );
    assert_eq!(
        i32::from_le_bytes(body[956..960].try_into().unwrap()),
        0,
        "siFameExp"
    );
    // Trailing set-item fields.
    assert_eq!(i16::from_le_bytes([body[10_160], body[10_161]]), 20);
    assert_eq!(i16::from_le_bytes([body[10_162], body[10_163]]), 3);

    // The unmodelled sub-structs are the documented deviation: zero, where
    // the capture carried the reference character's skills, horse (`Foal744`)
    // and passport. `clHealthInfo` is carved out of that sweep because the one
    // byte the sender calls `uiHealthPoint` is written -- see
    // `the_health_point_offset_is_the_one_the_capture_pins`.
    for (i, &b) in body[968..10_160]
        .iter()
        .enumerate()
        .filter(|&(i, _)| 968 + i != HEALTH_POINT_OFFSET)
    {
        assert_eq!(b, 0, "unmodelled region byte at offset {}", 968 + i);
    }
}

/// `clHealthInfo.uiHealthPoint` is the one byte in `ALLCHARINFOONE` that must
/// not be zero, and its offset has to be pinned to the reference bytes rather
/// than to arithmetic over eight struct sizes nobody can re-check by eye.
///
/// The chain is short because `clHealthInfo` is declared immediately before
/// `clHorseInfo` (`PersonInfo.h:308`, `:313`), with nothing between them. The
/// capture's reference character rode the `HORSEUNIQUE_FOAL` named `Foal744`,
/// and `siHorseUnique`/`szName` sit at 0/2 inside `cltHorse`
/// (`CommonHeader.h:3254-3255`), so the nine-byte marker locates `clHorse[0]`
/// outright. The array's offset inside `cltPersonHorseInfo` is the same
/// [`PERSON_HORSE_HEADER_LEN`] the 226 frame independently pins.
///
/// What runs *back* from `clHorseInfo` is [`CLT_HEALTH_INFO_WIRE_LEN`], not
/// [`CLT_HEALTH_INFO_LEN`]. Those differ by four, and getting it wrong is what
/// broke the status window: 7596 instead of 7592 put the seed at 7606, four
/// bytes into the slot's tail where `New Era.exe` never looks, so writing 7606
/// and then 7607 both left the live client reading `-100%`. The wire width is
/// pinned by the client's own `cltHealthInfo::Set` at `0x0085B2A0`, which is a
/// single `movups`/`movups` pair — sixteen bytes, one SSE store.
///
/// This is also the regression for the 2026-10-06 live failure: the status
/// `clHealthInfo` starts here, and `uiHealthPoint` is the tenth byte of it.
///
/// Both offsets were measured against the capture (moved to
/// `archive/capture/` on 2026-10-09) and are pinned here as literals. The
/// arithmetic that produced them is preserved in the comments above
/// [`HEALTH_INFO_OFFSET`] and in the handoff entry for 2026-10-06.
///
/// What was wrong is worth keeping: `cltHealthInfo`'s *wire* width is
/// [`CLT_HEALTH_INFO_WIRE_LEN`], four bytes narrower than the source-declared
/// [`CLT_HEALTH_INFO_LEN`], because the client's own `Set` at `0x0085B2A0` is a
/// single 16-byte `movups` pair. Measuring back from the capture's horse marker
/// gave 7,592; the wrong 7,596 put the seed at 7,606, four bytes into a tail
/// `New Era.exe` never reads — which is why writing 7,606 and then 7,607 both
/// left the live status window showing `-100%`.
#[test]
fn the_health_point_offset_is_the_measured_one() {
    assert_eq!(HEALTH_INFO_OFFSET, 7_592);
    assert_eq!(HEALTH_POINT_OFFSET, 7_602);
    // `uiHealthPoint` is `CommonHeader.h:2898`'s tenth byte, which the client's
    // `SetHealthAddAbility` at `0x0084C380` reads as
    // `movzx eax, byte ptr [ecx+0Ah]`.
    assert_eq!(HEALTH_POINT_OFFSET - HEALTH_INFO_OFFSET, 10);
    // The *wire* width is the wider one: the client's `Set` copies sixteen bytes
    // in one `movups` pair, four past what this source declares.
    assert_eq!(CLT_HEALTH_INFO_WIRE_LEN - CLT_HEALTH_INFO_LEN, 4);

    // And what we now send. `cltHealthInfo::Init()` seeds `uiHealthPoint` at 100
    // (`Common.cpp:3253`), and 100 is the neutral value: it makes every
    // `siHealth*Rate` zero (`Parametabox.cpp:7727`), so `clHealthAdd` adds
    // nothing and the rest of `clTotalBA` supplies the real numbers.
    assert_eq!(HEALTH_POINT_SEED, 100);
    let generated = encode_all_char_info_one(
        &BasicInfo {
            account_idnum: 1471,
            account_id: "tester".into(),
            slot: 2,
            sex: 1,
            class_kind: 0,
            home_village: 0,
            map_index: 158,
            x: 27,
            y: 87,
            character_id: 4242,
            name: "test3".into(),
            str_: 5,
            dex: 5,
            vit: 5,
            mag: 5,
            hnd: 5,
            luk: 5,
            wis: 5,
            level: 1,
            bonus: 5,
            life: 40,
            mana: 20,
            exp: 0,
            money: 1000,
        },
        0,
        0,
        0,
        0,
    );
    assert_eq!(
        generated.len(),
        gz_proto::captured_lengths::CAPTURED_ALL_CHAR_INFO_ONE_LEN,
        "248 keeps the size the reference server sent"
    );
    assert_eq!(generated[HEALTH_POINT_OFFSET], HEALTH_POINT_SEED);
    assert_eq!(
        &generated[HEALTH_INFO_OFFSET..HEALTH_POINT_OFFSET],
        &[0u8; 10],
        "siHungry, siInnerOrg1..4 and uiOrg1..4Disease stay zero, as the sender \
             writes them"
    );
}

/// Regression guard for the 2026-10-06 client crash.
///
/// A probe ramp was written over body offsets 7,424..7,680 to make the client
/// report which byte it reads as `clHealthInfo.uiHealthPoint`. It killed
/// `New Era.exe` 1.1.1.3 on login: Application Error event 1000,
/// `0xc0000005` at `0x003caf30`.
///
/// That offset is an RVA, so the fault site is `0x00400000 + 0x3CAF30 =
/// 0x007CAF30`, and the client's own code there is a **null dereference from an
/// out-of-range table index**: it reads a word at `[ecx+0x37FA]`, bounds it
/// against 4, and only dereferences when the lookup succeeds *and* returns a
/// type-`0x11` entry. The ramp planted a value above 4.
///
/// This does not re-run the probe. It asserts the *shape* of what we send, so
/// that no future attempt to make the unmodelled band observable does it by
/// writing a run of distinct values into it.
#[test]
fn the_unmodelled_band_carries_no_probe_ramp() {
    let generated = encode_all_char_info_one(
        &BasicInfo {
            account_idnum: 1471,
            account_id: "tester".into(),
            slot: 2,
            sex: 1,
            class_kind: 0,
            home_village: 0,
            map_index: 158,
            x: 27,
            y: 87,
            character_id: 4242,
            name: "test3".into(),
            str_: 5,
            dex: 5,
            vit: 5,
            mag: 5,
            hnd: 5,
            luk: 5,
            wis: 5,
            level: 1,
            bonus: 5,
            life: 40,
            mana: 20,
            exp: 0,
            money: 1000,
        },
        0,
        0,
        0,
        0,
    );

    // `clHealthInfo` plus `clHorseInfo`'s 28-byte header is where the ramp
    // killed the client. Everything from the byte after `uiHealthPoint` to the
    // end of the horse array must stay zero: the client's fault site is a
    // bounded table index, and this band holds the values it reads.
    let horse_band = &generated[HEALTH_POINT_OFFSET + 1..HORSE_ARRAY_END];
    assert!(
        horse_band.iter().all(|b| *b == 0),
        "the horse band must stay zero: `New Era.exe` indexes a table by the word \
             at +0x37FA and null-derefs on anything above 4, and cltHorse::szName is a \
             fixed 12 bytes with no terminator slot. First non-zero at body offset {:?}",
        horse_band
            .iter()
            .position(|b| *b != 0)
            .map(|p| p + HEALTH_POINT_OFFSET + 1)
    );

    // And nothing anywhere in the band may form a long ascending run, which is
    // the ramp's fingerprint. `window(3)` over the whole unmodelled region
    // catches any future attempt to encode an index into it.
    let unmodelled = &generated[BASIC_INFO_LEN..HORSE_ARRAY_END];
    assert!(
        !unmodelled
            .windows(8)
            .any(|w| { w.windows(2).all(|p| p[1] == p[0].wrapping_add(1)) }),
        "an 8-byte ascending run in the unmodelled band is a probe ramp; it \
             crashed the client on 2026-10-06"
    );
}

/// `clHorse[0]` ends the region the ramp must never reach. Derived from the
/// capture rather than hardcoded: `clHorse[0]` starts 28 bytes after
/// `clHorseInfo`, holds `MAX_HORSE_NAME` 12 and is 108 bytes, and there are 5
/// of them (frame 226 is exactly `28 + 5 * 108`).
///
/// This must come out at **8176** (`0x1FF0`), which is the next source offset
/// in the client's own `cltPersonInfo::SetOne` copy table. If it ever stops
/// agreeing with that literal, the offsets have drifted apart again.
const HORSE_ARRAY_END: usize =
    HEALTH_INFO_OFFSET + CLT_HEALTH_INFO_WIRE_LEN + PERSON_HORSE_HEADER_LEN + 5 * 108;

/// The offset chain the status-window fix rests on, asserted as arithmetic so a
/// change to any link is a test failure rather than a live regression.
///
/// Every number here is independently observable: `0x1DA8` and `0x1DB8` are
/// literals in the client's `SetOne` (`New Era.exe` 1.1.1.3, VA `0x00461AC1`
/// and `0x00461AD9`), `0x1FF0` is the next one, and 28 + 5 * 108 is the horse
/// array the capture and frame 226 agree on.
#[test]
fn the_client_layout_chain_agrees_with_the_horse_array() {
    assert_eq!(HEALTH_INFO_OFFSET, 0x1DA8);
    assert_eq!(HEALTH_POINT_OFFSET, 0x1DA8 + 10);
    assert_eq!(HEALTH_INFO_OFFSET + CLT_HEALTH_INFO_WIRE_LEN, 0x1DB8);
    assert_eq!(HORSE_ARRAY_END, 0x1FF0);
    assert_eq!(PERSON_HORSE_HEADER_LEN, 28);
}

/// The generated `PERSONITEMINFO` is 104 bytes, the size the reference server
/// sent, and its 6..8 padding is ours (zero) where the legacy's was
/// uninitialised stack.
///
/// 104 is a *single-slot* notification, not an inventory dump — `siItemPos` = 8
/// with all 84 `cltItem` bytes zero, i.e. "slot 8 is empty". That measurement
/// is what disproved the old "item-array stride" premise.
#[test]
fn generated_person_item_info_keeps_the_captured_length() {
    let generated = encode_person_item_info(world_entry::PERSON_ITEM_INFO_SLOT, &Item::empty());
    assert_eq!(
        generated.len(),
        gz_proto::captured_lengths::CAPTURED_PERSON_ITEM_INFO_LEN,
        "104 bytes: a single-slot notification"
    );
    assert_eq!(
        i16::from_le_bytes([generated[0], generated[1]]),
        world_entry::PERSON_ITEM_INFO_SLOT as i16,
        "the slot is the one the reference server announced"
    );
    // 6..8 is the `/Zp8` pad between `siChangedNum` and `siExp`. The capture
    // held uninitialised stack there; we write zero, which is the whole of the
    // documented deviation from the captured bytes.
    assert_eq!(&generated[6..8], &[0, 0], "the pad is ours and is zero");
}

/// Every frame the world-entry burst generates must have the size the reference
/// server sent for the same opcode.
///
/// Length is the one property of a generated frame that cannot be argued about
/// from the struct declarations alone — `sizeof` depends on the shipped build's
/// packing — so it is pinned to the capture's measurements rather than to a
/// reviewer's arithmetic. This is the check that would have caught a
/// mis-derived `cltHorse`.
///
/// The right-hand side is [`gz_proto::captured_lengths`], which carries each
/// number's origin; the left is what this port's encoders produce. If they ever
/// diverge, the client walks the frame at the wrong stride and faults silently.
#[test]
fn every_generated_burst_frame_matches_its_captured_length() {
    use gz_proto::captured_lengths as captured;

    let generated: &[(GameMsg, usize, usize)] = &[
        (
            GameMsg::ResponseAllcharinfoone,
            ALL_CHAR_INFO_ONE_LEN,
            captured::CAPTURED_ALL_CHAR_INFO_ONE_LEN,
        ),
        (
            GameMsg::ResponseAllcharinfotwo,
            ALL_CHAR_INFO_TWO_LEN,
            captured::CAPTURED_ALL_CHAR_INFO_TWO_LEN,
        ),
        (
            GameMsg::ResponseName,
            RESPONSE_NAME_LEN,
            captured::CAPTURED_RESPONSE_NAME_LEN,
        ),
        (
            GameMsg::ResponsePersonHorse,
            PERSON_HORSE_LEN,
            captured::CAPTURED_PERSON_HORSE_LEN,
        ),
        (
            GameMsg::ResponseChangehorse,
            CHANGE_HORSE_LEN,
            captured::CAPTURED_CHANGE_HORSE_LEN,
        ),
        (
            GameMsg::ResponseSkillbookMagicinfoGet,
            SKILLBOOK_MAGIC_INFO_LEN,
            captured::CAPTURED_8371_LEN,
        ),
        (
            GameMsg::ResponseFamilyUpdate,
            FAMILY_UPDATE_LEN,
            captured::CAPTURED_8271_LEN,
        ),
        (
            GameMsg::ResponseGetvillageinfo,
            GET_VILLAGE_INFO_LEN,
            captured::CAPTURED_7203_LEN,
        ),
    ];

    for &(msg, declared, was) in generated {
        assert_eq!(
            declared, was,
            "declared length for opcode {} disagrees with the captured measurement",
            msg as u16
        );
    }
}

/// 244 is the nameplate, and it is keyed by a character id taken from the
/// payload rather than from the frame's `id`.
///
/// `Client-SystemNPC.cpp:977` indexes `CharUniqueInfo` with
/// `pclinfo->siCharUnique`, so a wrong value here silently renames somebody
/// else — or nobody. The retired replay mode did exactly that: the captured
/// frame carried 1471 *and* `Meirin`, and 1471 resolved to our own slot.
#[test]
fn generated_name_is_keyed_by_the_session_id_and_the_rows_name() {
    let body = encode_response_name(1471, "Tester");

    assert_eq!(
        body.len(),
        gz_proto::captured_lengths::CAPTURED_RESPONSE_NAME_LEN
    );
    // `siCharUnique` @ 0, `szName[20]` @ 4 (`MsgType.h:3199-3200`).
    assert_eq!(i32::from_le_bytes(body[0..4].try_into().unwrap()), 1471);
    assert_eq!(&body[4..10], b"Tester");
    assert!(body[10..].iter().all(|&b| b == 0), "NUL padded");
    // The name must come from the row, never from a constant.
    assert_ne!(
        &body[4..10],
        b"Meirin",
        "the capture's name must never leak"
    );
}

/// 258 is the unmount, and it only unmounts if its `siCharUnique` resolves to
/// the local player.
///
/// `Client-Person.cpp:1065-1071` does
/// `CR[CharUniqueInfo[siCharUnique]]->DeleteSlave()` and returns early when the
/// index does not resolve, so a stale mount survives unless this frame is
/// stamped with the session's id. The capture put 1471 at offset 112 — the last
/// real field before the tail padding — and so must we.
#[test]
fn generated_change_horse_dismounts_our_own_character() {
    let body = encode_change_horse(1471);

    assert_eq!(
        body.len(),
        gz_proto::captured_lengths::CAPTURED_CHANGE_HORSE_LEN
    );
    // `siChangeHorseIndex` @ 0 is the legacy's "show horse 0" value, so index 0
    // resolves to the empty `cltChangeHorse` rather than a named mount.
    assert_eq!(i16::from_le_bytes([body[0], body[1]]), 0);
    // `cltChangeHorse` @ 4..112 zeroed; `siHorseUnique` and `szName` included.
    assert!(body[4..112].iter().all(|&b| b == 0));
    // `siCharUnique` @ 112 -- the capture's offset.
    assert_eq!(i16::from_le_bytes([body[112], body[113]]), 1471);
}

/// 226 lists the five horse slots; empty means unmounted.
///
/// The header is `bool bAutoFoodSwitch`, `bool siCurrentHorseRIDEHORSE`,
/// `SI16 siCurrentShowHorse` (`Horse.h:223-227`). The horse array starts at 28,
/// which is 24 bytes past what those three members account for; see
/// [`PERSON_HORSE_HEADER_LEN`]. The capture was riding and held exactly one
/// horse at offset 28 — `Foal744`, preceded by `HORSEUNIQUE_FOAL` = 12 as an
/// `SI16`, a nine-byte marker.
#[test]
fn generated_person_horse_is_empty_and_unmounted() {
    let body = encode_person_horse();

    assert_eq!(
        body.len(),
        gz_proto::captured_lengths::CAPTURED_PERSON_HORSE_LEN
    );
    assert_eq!(PERSON_HORSE_LEN, 568, "28 header + 5 * 108");
    assert!(body.iter().all(|&b| b == 0), "no horse at all");
    // The stride is pinned by the capture independently: 568 = 28 + 5 * 108,
    // where 108 is `sizeof(cltHorse)` as it travels.
    assert_eq!(
        PERSON_HORSE_LEN,
        PERSON_HORSE_HEADER_LEN + 5 * gz_proto::game::CLT_HORSE_LEN
    );
}

/// The empty world frames are zero-filled and keep the capture's sizes.
///
/// All three legacy constructors are `ZeroMemory(this, sizeof(*this))`
/// (`MsgType-Item.h:1874`, `MsgType-Person.h:2997`, `MsgType.h:3119`), so zero
/// is the encoding the legacy itself uses for "none" rather than a stand-in for
/// data we failed to model.
#[test]
fn the_empty_world_frames_are_zero_and_capture_sized() {
    use gz_proto::captured_lengths as captured;

    for (msg, body, was) in [
        (
            GameMsg::ResponseSkillbookMagicinfoGet as u16,
            encode_skillbook_magic_info(),
            captured::CAPTURED_8371_LEN,
        ),
        (
            GameMsg::ResponseFamilyUpdate as u16,
            encode_family_update(),
            captured::CAPTURED_8271_LEN,
        ),
        (
            GameMsg::ResponseGetvillageinfo as u16,
            encode_get_village_info(),
            captured::CAPTURED_7203_LEN,
        ),
    ] {
        assert_eq!(body.len(), was, "opcode {msg}");
        assert!(
            body.iter().all(|&b| b == 0),
            "opcode {msg} must be all zero"
        );
    }
}

/// No generated frame may carry the captured character's identity.
///
/// The whole point of generating these is that the reference character's name
/// and horse stop leaking into a fresh session, so the absence is asserted
/// rather than assumed.
#[test]
fn no_generated_frame_carries_the_captured_character() {
    let bodies: Vec<Vec<u8>> = vec![
        encode_all_char_info_one(
            &BasicInfo {
                account_idnum: 1471,
                account_id: "tester".into(),
                slot: 0,
                sex: 1,
                class_kind: 1,
                home_village: 0,
                map_index: 1,
                x: 25,
                y: 13,
                character_id: 4242,
                name: "Tester".into(),
                str_: 5,
                dex: 5,
                vit: 5,
                mag: 5,
                hnd: 5,
                luk: 5,
                wis: 5,
                level: 1,
                bonus: 0,
                life: 20,
                mana: 20,
                exp: 0,
                money: 1000,
            },
            20,
            3,
            0,
            0,
        ),
        encode_response_name(1471, "Tester"),
        encode_person_horse(),
        encode_change_horse(1471),
        encode_skillbook_magic_info(),
        encode_family_update(),
        encode_get_village_info(),
    ];

    for body in &bodies {
        assert!(
            !contains_ascii(body, b"Meirin"),
            "the captured character's name leaked into a generated frame"
        );
        assert!(
            !contains_ascii(body, b"Foal744"),
            "the captured horse leaked into a generated frame"
        );
    }
}

/// A NUL-terminated ASCII needle anywhere in `haystack`.
fn contains_ascii(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// The generated world-entry frames keep the sizes the reference server sent.
///
/// The zero-filled ones are the honest encoding: their legacy constructors are
/// `ZeroMemory(this, sizeof(*this))`, so zero is what the legacy itself means
/// by "none" rather than a stand-in for data we failed to model.
#[test]
fn the_zero_state_frames_match_the_capture_lengths() {
    use gz_proto::captured_lengths as captured;

    let cases: &[(u16, Vec<u8>, usize)] = &[
        (
            GameMsg::ResponsePersonQuest as u16,
            world_entry::person_quest_empty(),
            captured::CAPTURED_225_LEN,
        ),
        (
            GameMsg::ResponseSoulguardGet as u16,
            world_entry::soulguard_get_empty(),
            captured::CAPTURED_8285_LEN,
        ),
        (
            GameMsg::ResponseSoulguardHouseGet as u16,
            world_entry::soulguard_house_get_default(),
            captured::CAPTURED_8286_LEN,
        ),
        (
            GameMsg::ResponseGetMarryInfo as u16,
            world_entry::marry_info_empty(),
            captured::CAPTURED_6504_LEN,
        ),
        (
            GameMsg::ResponseEnemycaptureall as u16,
            world_entry::enemy_capture_all_clear(),
            captured::CAPTURED_ENEMY_CAPTURE_ALL_LEN,
        ),
        (
            GameMsg::ResponseItemmallexistence as u16,
            world_entry::item_mall_existence_absent(),
            captured::CAPTURED_6801_LEN,
        ),
        (
            world_entry::OP_8464,
            world_entry::zero_frame(world_entry::OP_8464, world_entry::OP_8464_LEN),
            captured::CAPTURED_ITEM_TYPE_INFO_LEN,
        ),
        (
            world_entry::OP_8470,
            world_entry::zero_frame(world_entry::OP_8470, world_entry::OP_8470_LEN),
            captured::CAPTURED_8470_LEN,
        ),
    ];
    for &(cmd, ref body, was) in cases {
        assert_eq!(body.len(), was, "opcode {cmd}");
    }
    // 8286's only non-zero is the capacity default.
    let house = world_entry::soulguard_house_get_default();
    assert_eq!(&house[0..2], &[0x02, 0x00], "default capacity 2");
    assert!(house[2..].iter().all(|&b| b == 0));
    // 8178 needs the session id to start at offset 0. The id is a parameter, not
    // the retired capture's — the whole point of generating it.
    let elixir = world_entry::elixir_set_empty(0x05BF);
    assert_eq!(&elixir[0..4], &[0xBF, 0x05, 0x00, 0x00]);
    assert!(elixir[4..].iter().all(|&b| b == 0));
    assert_eq!(
        world_entry::elixir_set_empty(7)[0..4],
        [7, 0, 0, 0],
        "the id comes from the session, not a constant"
    );
}

/// A move commits one tile at a time along the direction of the destination
/// and stops there, at the cadence `char-move.cpp` gives: 100 accumulator
/// units per tile at `movespeed` units per 50 ms, i.e. `5000 / movespeed`
/// ms per tile (≈357 ms for the captured speed 14).
#[test]
fn a_move_steps_one_tile_at_a_time_on_the_legacy_cadence() {
    let mut movement = Movement::new((3, 0), (0, 0), 7, 14);
    let start = movement.last;
    let mut now = start;

    let mut steps = Vec::new();
    let mut intervals = Vec::new();
    let mut previous = start;
    while !movement.arrived() {
        now += movement.next_delay();
        if let Some(pos) = movement.advance(now) {
            steps.push(pos);
            intervals.push(now - previous);
            previous = now;
        }
        assert!(now - start < Duration::from_secs(5), "move never arrived");
    }

    assert_eq!(steps, vec![(1, 0), (2, 0), (3, 0)]);
    // After the first (cold-start) tile the cadence settles at one tile per
    // ~357 ms; the cap only reshapes a delayed tick, never the average.
    for interval in &intervals[1..] {
        assert!(
            *interval >= Duration::from_millis(340) && *interval <= Duration::from_millis(360),
            "tile interval {interval:?} is off the legacy cadence"
        );
    }
}

/// The read loop's real driver — `next_move_deadline` arms the wake and
/// `advance` banks the elapsed time — must deliver the legacy cadence of
/// `ceil(5000 / movespeed)` ms per tile.
///
/// This is the pair `main.rs` actually runs, and the one the older cadence
/// tests miss: they drive `advance` from `next_delay`, while the loop arms
/// `next_move_deadline`. The two drifted apart on 2026-10-10 when the deadline
/// stopped applying `MOVE_DELAY_CAP_MS` but `advance` kept crediting at most
/// 350 ms per call. At speed 10 that discarded 150 ms of every 500 ms step and
/// the tile committed at **650 ms** — measured in `target/m1-logs/game.log`
/// as `fanned 200` every ~650 ms — a 30 % slowdown against the `5000 /
/// movespeed` animation the observing client runs for its copy of the avatar.
/// The observer steps once per 200 and then idles for the difference, which
/// is the "walks one tile, stops, repeats" the owner reported.
///
/// Speed 10 is `Tester2`, the level-1 second account used for the two-client
/// observer test; 12 is `Tester` in its boots; 14 is the captured reference.
#[test]
fn the_read_loop_deadline_and_advance_agree_on_the_legacy_cadence() {
    for speed in [7_i32, 8, 10, 11, 12, 13, 14] {
        let mut movement = Movement::new((12, 0), (0, 0), 1, speed);
        let start = movement.last;
        let mut now = start;
        let mut previous = start;
        let mut intervals = Vec::new();

        while !movement.arrived() {
            // Exactly what `main.rs` does: arm the deadline, wake at it, bank
            // the elapsed time.
            now = next_move_deadline(&movement, now);
            if movement.advance(now).is_some() {
                intervals.push(now - previous);
                previous = now;
            }
            assert!(
                now - start < Duration::from_secs(30),
                "move never arrived at speed {speed}"
            );
        }

        assert_eq!(
            intervals.len(),
            12,
            "one tile per interval at speed {speed}"
        );
        // A fresh order's accumulator starts at 0 — `MoveInit()` and `SetMove()`
        // both write `siMovePercent = 0` (`Char-Common.cpp:4898`,
        // `char.cpp:4235`) — so the **first** tile is half a tile of points and
        // every commit then returns it to -50 for a full one. That half-tile
        // start is what the client animates its own avatar from, so the server
        // has to take it too or its 200s lag the walk they describe by half an
        // interval, forever.
        let cold_ms = (2500_i64 + i64::from(speed) - 1) / i64::from(speed);
        assert_eq!(
            intervals[0],
            Duration::from_millis(cold_ms as u64),
            "the cold-start tile at speed {speed} took {:?}, expected {cold_ms} ms",
            intervals[0]
        );
        let expected_ms = (5000_i64 + i64::from(speed) - 1) / i64::from(speed);
        let expected = Duration::from_millis(expected_ms as u64);
        for (i, interval) in intervals.iter().enumerate().skip(1) {
            assert_eq!(
                *interval, expected,
                "tile {i} at speed {speed} took {interval:?}, expected {expected:?} \
                 — the deadline woke outside advance()'s 350 ms credit window"
            );
        }
    }
}

/// A diagonal click steps both axes at once, the `siNX++/siNY++` pair at
/// `char-move.cpp:107-126`.
#[test]
fn a_diagonal_move_steps_both_axes() {
    let mut movement = Movement::new((2, 2), (0, 0), 1, 14);
    let mut now = movement.last;
    let mut steps = Vec::new();
    while !movement.arrived() {
        now += movement.next_delay();
        if let Some(pos) = movement.advance(now) {
            steps.push(pos);
        }
    }
    assert_eq!(steps, vec![(1, 1), (2, 2)]);
}

/// A stalled tick must not bank more than one tile: `advance` caps its
/// elapsed time at `(CHAR_ACTION_INTERVAL_FRAME + 1) * 50` ms
/// (`char-move.cpp:145`) and commits at most one tile however much it credits.
/// At speed 14 a capped 350 ms is 98 points — enough to cover the cold-start
/// half-tile, since `MoveInit()` starts a fresh order's accumulator at 0 — so
/// a four-second stall steps once and stops there.
#[test]
fn a_stalled_tick_commits_at_most_one_tile() {
    let mut movement = Movement::new((10, 0), (0, 0), 1, 14);
    let stalled = movement.last + Duration::from_secs(4);
    assert_eq!(movement.advance(stalled), Some((1, 0)));
    assert_eq!(movement.pos, (1, 0));
    // With no time passing nothing else can be credited: the character does not
    // drift further while the loop is idle.
    assert_eq!(movement.advance(movement.last), None);
    assert_eq!(movement.pos, (1, 0));
    // A second equally long stall takes exactly one more tile — the cap bounds
    // each call, so the four seconds behind it never becomes a teleport.
    assert_eq!(
        movement.advance(stalled + Duration::from_secs(4)),
        Some((2, 0))
    );
    assert_eq!(movement.pos, (2, 0));
}

/// `next_delay` never exceeds the legacy cap, so the read loop's timer arm
/// wakes at the same bound the legacy's `min((CHAR_ACTION_INTERVAL_FRAME+1)*50, ...)`
/// applies (`char-move.cpp:145`), and `advance` credits at most that much —
/// which is what keeps a late wake from banking time the client never spent.
#[test]
fn next_delay_is_capped_at_the_legacy_tick_bound() {
    // Speed 10 takes 500 ms per tile at steady state, well over the 350 ms cap.
    let mut movement = Movement::new((10, 0), (0, 0), 1, 10);
    let start = movement.last;
    // A fresh order's accumulator is at 0, so the cold-start tile is half a
    // tile of points: ceil(2500 / 10) = 250 ms, which fits inside the cap.
    assert_eq!(movement.next_delay(), Duration::from_millis(250));
    assert_eq!(
        movement.advance(start + Duration::from_millis(250)),
        Some((1, 0)),
        "the cold-start tile lands at 250 ms, not a full 500"
    );
    // Steady state needs the full 100 points: 500 ms, capped down to 350.
    assert_eq!(movement.next_delay(), Duration::from_millis(350));
    // The capped wake credits 10 * 350 / 50 = 70 points and still needs 30 —
    // 150 ms, inside the cap, so nothing is dropped.
    assert_eq!(movement.advance(start + Duration::from_millis(600)), None);
    assert_eq!(movement.next_delay(), Duration::from_millis(150));
}

/// The cadence follows the character, not a fixed reference: `advance` banks
/// `move_speed * elapsed / 50`, so a slower mover takes proportionally longer
/// per tile. At speed 7 a tile needs `5000 / 7 ≈ 715` ms against 313 ms at
/// speed 16.
///
/// Each walk also opens on a **half** interval, because `MoveInit()` and
/// `SetMove()` start `siMovePercent` at 0 (`Char-Common.cpp:4898`,
/// `char.cpp:4235`) and only a commit returns it to -50. The client animates
/// from the same start, so both halves are asserted here: a server that made
/// the cold start a full tile would sit a permanent half-interval behind the
/// avatar it is tracking.
#[test]
fn the_step_cadence_scales_with_the_characters_move_speed() {
    // (speed, cold-start interval, steady-state interval), all `ceil(2500/speed)`
    // and `ceil(5000/speed)` — speed 7 exceeds the 350 ms credit cap, so its
    // deadline is split across two wakes that still sum to the same figure.
    for (speed, cold_ms, full_ms) in [(7_i32, 358_i64, 715_i64), (16, 157, 313), (28, 90, 179)] {
        let mut movement = Movement::new((2, 0), (0, 0), 1, speed);
        let start = movement.last;
        let mut now = start;
        let mut previous = start;
        let mut intervals = Vec::new();
        while !movement.arrived() {
            now += movement.next_delay();
            if movement.advance(now).is_some() {
                intervals.push(now - previous);
                previous = now;
            }
            assert!(
                now - start < Duration::from_secs(5),
                "move never arrived at speed {speed}"
            );
        }
        assert_eq!(
            intervals,
            vec![
                Duration::from_millis(cold_ms as u64),
                Duration::from_millis(full_ms as u64),
            ],
            "tile intervals at speed {speed}"
        );
    }
}

/// The quit menu's two requests are the legacy pair, and each is answered by
/// the next opcode up with an empty body: 107 `REQUEST_HOPETOQUIT` → 108
/// `RESPONSE_HOPETOQUIT`, 109 `REQUEST_HOPETOFRONT` → 110
/// `RESPONSE_HOPETOFRONT` (`Server-System.cpp:1195`, `:1239`). Pinning the
/// numbers keeps the dispatcher's `c == … as u16` guards honest even if the
/// codegen ever renumbers the enum.
#[test]
fn leave_requests_are_the_legacy_107_109_pair() {
    assert_eq!(GameMsg::RequestHopetoquit as u16, 107);
    assert_eq!(GameMsg::ResponseHopetoquit as u16, 108);
    assert_eq!(GameMsg::RequestHopetofront as u16, 109);
    assert_eq!(GameMsg::ResponseHopetofront as u16, 110);
}

// ------------------------------------------------------------------ chat
//
// T-018 slice 5: the routing half of the chat family. The wire half is
// `gz_proto::chat`'s own tests; these pin the opcodes, the route order and
// the frames each handler actually queues.

/// A character with the defaults a chat test cares about.
fn chat_info(char_unique: i16, name: &str, x: i32, y: i32) -> SessionInfo {
    SessionInfo {
        char_unique: PlayerCharUnique::try_from_si16(char_unique)
            .expect("test id in the player band"),
        name: name.to_owned(),
        level: 34,
        map_id: 7,
        x,
        y,
        school_id: 3,
        class_kind: 1,
        life: 161,
        move_speed: 12,
        sex: 1,
        gm_level: 0,
    }
}

/// Registers a connection and hands back its key and inbox.
fn chat_join(
    registry: &Registry,
    info: SessionInfo,
) -> (SessionKey, tokio::sync::mpsc::Receiver<Outbound>) {
    let key = Registry::next_key();
    let (tx, rx) = tokio::sync::mpsc::channel(crate::registry::OUTBOUND_CAP);
    registry.register(
        key,
        tx,
        std::sync::Arc::new(tokio::sync::Notify::new()),
        info,
    );
    (key, rx)
}

/// Nothing queued, as distinct from "the sender is gone".
fn assert_chat_silent(rx: &mut tokio::sync::mpsc::Receiver<Outbound>, what: &str) {
    assert!(
        matches!(
            rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ),
        "{what}: expected no frame"
    );
}

/// The chat family's opcodes are the legacy's, and the dispatcher's
/// `c == … as u16` guards depend on them: 117 `REQUEST_CHAT`, 118
/// `RESPONSE_CHAT`, 119 `REQUEST_SECRETCHAT`, 120
/// `RESPONSE_SECRETCHATRESULT`, 121 `REQUEST_HEADCHAT`, 122
/// `RESPONSE_HEADCHAT`, 112 `RESPONSE_NOTICE`
/// (`MsgType-System.h:961-1160`, `MsgType.h:3256`).
#[test]
fn chat_opcodes_are_the_legacy_pair_plus_the_notice() {
    assert_eq!(GameMsg::RequestChat as u16, 117);
    assert_eq!(GameMsg::ResponseChat as u16, 118);
    assert_eq!(GameMsg::RequestSecretchat as u16, 119);
    assert_eq!(GameMsg::ResponseSecretchatresult as u16, 120);
    assert_eq!(GameMsg::RequestHeadchat as u16, 121);
    assert_eq!(GameMsg::ResponseHeadchat as u16, 122);
    assert_eq!(GameMsg::ResponseNotice as u16, 112);
}

/// The route decision reproduces the legacy's order, which is not
/// interchangeable: the leading `/` is tested *before* the mode
/// (`pChatMsg[0] == TEXT('/')` at `Server-System.cpp:859` runs ahead of the
/// mode chain at `:1018-1184`), a text too short to hold the command's two
/// characters is not a command at all, and the modes the legacy gates are
/// dropped rather than broadcast.
#[test]
fn route_chat_tests_the_slash_before_the_mode_and_drops_the_gated_modes() {
    use gz_proto::chat::{
        CHATMODE_ASK, CHATMODE_GUILD, CHATMODE_HEAD, CHATMODE_HOME, CHATMODE_KING,
        CHATMODE_NEWSITEM, CHATMODE_NORMAL, CHATMODE_PARTY, CHATMODE_SECRET, CHATMODE_TOWN,
        CHATMODE_VILLAGER,
    };

    // Empty text is nothing, whatever the mode.
    assert_eq!(route_chat("", CHATMODE_NORMAL), ChatRoute::Ignore);
    assert_eq!(route_chat("", CHATMODE_HEAD), ChatRoute::Ignore);

    // The slash wins over the mode: `/t ` under the head-chat mode is still
    // a whisper.
    assert_eq!(route_chat("/t Bob hi", CHATMODE_HEAD), ChatRoute::Whisper);
    assert_eq!(route_chat("/T Bob hi", CHATMODE_NORMAL), ChatRoute::Whisper);
    assert_eq!(route_chat("/p Bob", CHATMODE_NORMAL), ChatRoute::Party);
    assert_eq!(route_chat("/P Bob", CHATMODE_NORMAL), ChatRoute::Party);

    // "/t" without the space reads the zero-filled tail of the legacy's
    // `chatbuffer` (`:726`), so it is not a whisper — and neither is a
    // single "/".
    assert_eq!(route_chat("/t", CHATMODE_NORMAL), ChatRoute::Command);
    assert_eq!(route_chat("/", CHATMODE_NORMAL), ChatRoute::Command);
    assert_eq!(route_chat("/x", CHATMODE_NORMAL), ChatRoute::Command);

    // The gated modes drop, whatever the text.
    for mode in [
        CHATMODE_TOWN,
        CHATMODE_VILLAGER,
        CHATMODE_PARTY,
        CHATMODE_GUILD,
        CHATMODE_KING,
        CHATMODE_NEWSITEM,
    ] {
        assert_eq!(route_chat("hello", mode), ChatRoute::Dropped(mode));
    }

    // Head and home route by mode.
    assert_eq!(route_chat("hello", CHATMODE_HEAD), ChatRoute::Head);
    assert_eq!(route_chat("hello", CHATMODE_HOME), ChatRoute::Home);

    // `CHATMODE_SECRET` *without* a slash falls through the legacy's mode
    // chain (`:1018-1138`) exactly as it falls through here: plain text
    // under the whisper mode is ordinary chat.
    assert_eq!(route_chat("hello", CHATMODE_SECRET), ChatRoute::Broadcast);
    assert_eq!(route_chat("hello", CHATMODE_ASK), ChatRoute::Broadcast);
    assert_eq!(route_chat("hello", CHATMODE_NORMAL), ChatRoute::Broadcast);
}

/// The `/t` split reproduces the legacy's `_stscanf` + fixed-offset
/// arithmetic (`:865`, `:999`): the target is the second whitespace token
/// capped at 19 bytes, and the message starts `4 + target.len()` bytes into
/// the line — so extra whitespace, an over-cap target and no message at all
/// all shift or miss exactly as that arithmetic does.
#[test]
fn whisper_parts_reproduces_the_legacy_offset_arithmetic() {
    // "/t Target" — the message starts past the end of the line.
    let (target, message) = whisper_parts("/t Target");
    assert_eq!(target, "Target");
    assert_eq!(message, "", "the fixed offset runs off the end");

    // The message begins right after the target and one space, wherever the
    // whitespace actually was.
    let (target, message) = whisper_parts("/t  Bob   hello there");
    assert_eq!(target, "Bob");
    assert_eq!(message, "   hello there", "offset 4 + len, not a tokeniser");

    // A target over the cap is cut, and the offset follows the *cut* name.
    let long = "abcdefghijklmnopqrs"; // 19 bytes, the cap
    let (target, message) = whisper_parts(&format!("/t {long} hi"));
    assert_eq!(target, long);
    assert_eq!(message, "hi");

    let over = format!("/t {}X hi", "a".repeat(30));
    let (target, message) = whisper_parts(&over);
    assert_eq!(target.len(), 19, "the cap");
    assert_eq!(message, "aaaaaaaaaaX hi", "the offset follows the cut name");

    // No target at all: the legacy's `szname` stays empty and the message
    // starts at byte 4.
    let (target, message) = whisper_parts("/t ");
    assert_eq!(target, "");
    assert_eq!(message, "", "nothing after the command");

    // A multi-byte target is cut on a character boundary, and the offset
    // still follows the cut length.
    let (target, message) = whisper_parts("/t éééééééééé hi");
    assert_eq!(target, "é".repeat(9), "19 bytes of whole characters");
    assert_eq!(message, "é hi");
}

/// General chat is server-wide (the owner's instruction): every connected
/// player — the speaker included — gets the 118, with the request's
/// `siSystemTxt` echoed and level 0 (`:1144`).
#[test]
fn general_chat_reaches_the_whole_server_including_the_speaker() {
    use gz_proto::chat::CHATMODE_NORMAL;

    let registry = Registry::new();
    let (alice, mut ra) = chat_join(&registry, chat_info(11, "Alice", 0, 0));
    let (_bob, mut rb) = chat_join(&registry, chat_info(12, "Bob", 500, 500));
    let (_carol, mut rc) = chat_join(&registry, chat_info(13, "Carol", 0, 0));

    handle_chat(
        &registry,
        alice,
        ChatRequest {
            mode: CHATMODE_NORMAL,
            system_txt: 7,
            text: "hello world".to_owned(),
        },
    );

    for (rx, who) in [
        (&mut ra, "the speaker"),
        (&mut rb, "Bob"),
        (&mut rc, "Carol"),
    ] {
        let frame = rx
            .try_recv()
            .unwrap_or_else(|_| panic!("{who} gets the frame"));
        assert_eq!(frame.cmd, GameMsg::ResponseChat as u16);
        let body = &frame.payload;
        assert_eq!(i16::from_le_bytes([body[2], body[3]]), 11, "the sender");
        assert_eq!(i16::from_le_bytes([body[6], body[7]]), CHATMODE_NORMAL);
        assert_eq!(
            i32::from_le_bytes(body[12..16].try_into().unwrap()),
            7,
            "siSystemTxt echoed"
        );
        assert_eq!(
            i32::from_le_bytes(body[16..20].try_into().unwrap()),
            0,
            "level 0"
        );
        assert_eq!(&body[20..25], b"Alice");
        assert_eq!(&body[25..], b"hello world\0");
    }
}

/// A `/t` whisper delivers the 118 to the target with `CHATMODE_SECRET`,
/// level 0 and system text 0, then a `SECRETCHAT_SUCCESS` 120 carrying the
/// target's name to the speaker (`:1001-1008`).
#[test]
fn a_whisper_reaches_the_target_and_acks_the_speaker() {
    let registry = Registry::new();
    let (alice, mut ra) = chat_join(&registry, chat_info(11, "Alice", 0, 0));
    let (_bob, mut rb) = chat_join(&registry, chat_info(12, "Bob", 0, 0));

    handle_chat(
        &registry,
        alice,
        ChatRequest {
            mode: CHATMODE_SECRET,
            system_txt: 0,
            text: "/t Bob secret message".to_owned(),
        },
    );

    // The target gets the 118.
    let frame = rb.try_recv().expect("Bob gets the whisper");
    assert_eq!(frame.cmd, GameMsg::ResponseChat as u16);
    let body = &frame.payload;
    assert_eq!(i16::from_le_bytes([body[2], body[3]]), 11, "the sender");
    assert_eq!(i16::from_le_bytes([body[6], body[7]]), CHATMODE_SECRET);
    assert_eq!(
        i32::from_le_bytes(body[16..20].try_into().unwrap()),
        0,
        "level 0"
    );
    assert_eq!(&body[20..25], b"Alice");
    assert_eq!(&body[25..], b"secret message\0");

    // The speaker gets the 120 ack.
    let frame = ra.try_recv().expect("Alice gets the ack");
    assert_eq!(frame.cmd, GameMsg::ResponseSecretchatresult as u16);
    assert_eq!(
        i16::from_le_bytes([frame.payload[0], frame.payload[1]]),
        SECRETCHAT_SUCCESS
    );
    assert_eq!(&frame.payload[2..5], b"Bob");
    assert_eq!(
        i32::from_le_bytes(frame.payload[24..28].try_into().unwrap()),
        0
    );
}

/// A `/t` to a name that is not online answers `SECRETCHAT_FAIL_INVALIDNAME`
/// with the requested name and `siCharUnique` 0 (`:871`).
#[test]
fn a_whisper_to_an_unknown_name_fails_with_invalidname() {
    let registry = Registry::new();
    let (alice, mut ra) = chat_join(&registry, chat_info(11, "Alice", 0, 0));

    handle_chat(
        &registry,
        alice,
        ChatRequest {
            mode: CHATMODE_SECRET,
            system_txt: 0,
            text: "/t Nobody hello".to_owned(),
        },
    );

    let frame = ra.try_recv().expect("Alice gets the failure");
    assert_eq!(frame.cmd, GameMsg::ResponseSecretchatresult as u16);
    assert_eq!(
        i16::from_le_bytes([frame.payload[0], frame.payload[1]]),
        SECRETCHAT_FAIL_INVALIDNAME
    );
    assert_eq!(&frame.payload[2..8], b"Nobody");
    assert_eq!(
        i32::from_le_bytes(frame.payload[24..28].try_into().unwrap()),
        0
    );
}

/// `/p` is a party invite, and the legacy's `SRVAL_PARTYINVITE_*` responses
/// are not modelled — so the request is dropped silently (`:912-969`).
#[test]
fn a_party_invite_is_dropped_silently() {
    let registry = Registry::new();
    let (alice, mut ra) = chat_join(&registry, chat_info(11, "Alice", 0, 0));
    let (_bob, mut rb) = chat_join(&registry, chat_info(12, "Bob", 0, 0));

    handle_chat(
        &registry,
        alice,
        ChatRequest {
            mode: CHATMODE_SECRET,
            system_txt: 0,
            text: "/p Bob".to_owned(),
        },
    );

    assert_chat_silent(&mut ra, "the speaker");
    assert_chat_silent(&mut rb, "Bob");
}

/// Any other leading `/` is a GM order, and `OperateGameMasterOrder` always
/// fails here — so the speaker gets the 34-byte FAIL notice (`:978-984`).
#[test]
fn a_gm_order_fails_with_the_34_byte_notice() {
    let registry = Registry::new();
    let (alice, mut ra) = chat_join(&registry, chat_info(11, "Alice", 0, 0));
    let (_bob, mut rb) = chat_join(&registry, chat_info(12, "Bob", 0, 0));

    handle_chat(
        &registry,
        alice,
        ChatRequest {
            mode: CHATMODE_SECRET,
            system_txt: 0,
            text: "/w something".to_owned(),
        },
    );

    let frame = ra.try_recv().expect("Alice gets the notice");
    assert_eq!(frame.cmd, GameMsg::ResponseNotice as u16);
    assert_eq!(frame.payload.len(), 34, "the FAIL notice is 34 bytes");
    assert_eq!(&frame.payload[10..14], b"FAIL");
    assert_eq!(&frame.payload[14..32], b"Master Order Error");
    assert_chat_silent(&mut rb, "Bob");
}

/// Head chat stores the text (capped at 127) and fans the 122 out over the
/// near cells including the speaker (`:1050-1055`).
#[test]
fn head_chat_stores_and_fans_to_the_near_cells() {
    let registry = Registry::new();
    let (alice, mut ra) = chat_join(&registry, chat_info(11, "Alice", 0, 0));
    let (_bob, mut rb) = chat_join(&registry, chat_info(12, "Bob", 5, 5));
    let (_carol, mut rc) = chat_join(&registry, chat_info(13, "Carol", 500, 500));

    handle_chat(
        &registry,
        alice,
        ChatRequest {
            mode: CHATMODE_HEAD,
            system_txt: 0,
            text: "top of the world".to_owned(),
        },
    );

    // The text is stored for 121.
    assert_eq!(
        registry.head_chat(alice).as_deref(),
        Some("top of the world")
    );

    // Alice and Bob are in the same cell; Carol is not.
    for (rx, who) in [(&mut ra, "Alice"), (&mut rb, "Bob")] {
        let frame = rx
            .try_recv()
            .unwrap_or_else(|_| panic!("{who} gets the 122"));
        assert_eq!(frame.cmd, GameMsg::ResponseHeadchat as u16);
        assert_eq!(i16::from_le_bytes([frame.payload[0], frame.payload[1]]), 11);
        assert_eq!(&frame.payload[4..], b"top of the world\0\0");
    }
    assert_chat_silent(&mut rc, "Carol");
}

/// 121 asks for a *subject's* stored head chat and replies to the asker with
/// the subject's text — or nothing if the subject is offline or the switch
/// is off (`:708-712`).
#[test]
fn head_chat_request_replies_with_the_subjects_stored_text() {
    let registry = Registry::new();
    let (alice, mut ra) = chat_join(&registry, chat_info(11, "Alice", 0, 0));
    let (bob, mut rb) = chat_join(&registry, chat_info(12, "Bob", 0, 0));

    // Alice sets her head chat; both get the 122 fan.
    handle_chat(
        &registry,
        alice,
        ChatRequest {
            mode: CHATMODE_HEAD,
            system_txt: 0,
            text: "look at me".to_owned(),
        },
    );
    let _ = ra.try_recv().expect("Alice gets her own 122");
    let _ = rb.try_recv().expect("Bob gets Alice's 122");

    // Bob asks for Alice's head chat.
    handle_head_chat_request(&registry, bob, 11);

    let frame = rb.try_recv().expect("Bob gets the 121 reply");
    assert_eq!(frame.cmd, GameMsg::ResponseHeadchat as u16);
    assert_eq!(
        i16::from_le_bytes([frame.payload[0], frame.payload[1]]),
        11,
        "the subject"
    );
    assert_eq!(&frame.payload[4..], b"look at me\0\0");
    assert_chat_silent(&mut ra, "Alice");
}

/// 121 is silent when the subject's switch is off or the subject is not
/// online (`:708-712`).
#[test]
fn head_chat_request_is_silent_when_the_subject_is_offline_or_off() {
    use gz_proto::chat::CHATMODE_NORMAL;

    let registry = Registry::new();
    let (alice, mut ra) = chat_join(&registry, chat_info(11, "Alice", 0, 0));
    let (bob, mut rb) = chat_join(&registry, chat_info(12, "Bob", 0, 0));

    // Bob asks for Alice's head chat before she sets it: the switch is off.
    handle_head_chat_request(&registry, bob, 11);
    assert_chat_silent(&mut rb, "switch off");

    // Alice sets it, then clears it with a normal chat.
    handle_chat(
        &registry,
        alice,
        ChatRequest {
            mode: CHATMODE_HEAD,
            system_txt: 0,
            text: "on".to_owned(),
        },
    );
    let _ = ra.try_recv();
    let _ = rb.try_recv();
    handle_chat(
        &registry,
        alice,
        ChatRequest {
            mode: CHATMODE_NORMAL,
            system_txt: 0,
            text: "off".to_owned(),
        },
    );
    let _ = ra.try_recv();
    let _ = rb.try_recv();

    // The switch is off again.
    handle_head_chat_request(&registry, bob, 11);
    assert_chat_silent(&mut rb, "switch off again");

    // An unknown subject is silent too.
    handle_head_chat_request(&registry, bob, 999);
    assert_chat_silent(&mut rb, "unknown subject");
}

/// The HOME branch fans the 118 out over the speaker's home village,
/// speaker included (`:1061-1066`).
#[test]
fn home_chat_reaches_the_speaker_school() {
    let registry = Registry::new();
    let (alice, mut ra) = chat_join(&registry, chat_info(11, "Alice", 0, 0));
    let (_bob, mut rb) = {
        let mut info = chat_info(12, "Bob", 0, 0);
        info.school_id = 4;
        chat_join(&registry, info)
    };
    let (_carol, mut rc) = chat_join(&registry, chat_info(13, "Carol", 0, 0));

    handle_chat(
        &registry,
        alice,
        ChatRequest {
            mode: CHATMODE_HOME,
            system_txt: 0,
            text: "home sweet home".to_owned(),
        },
    );

    for (rx, who) in [(&mut ra, "Alice"), (&mut rc, "Carol")] {
        let frame = rx
            .try_recv()
            .unwrap_or_else(|_| panic!("{who} gets the 118"));
        assert_eq!(frame.cmd, GameMsg::ResponseChat as u16);
        assert_eq!(
            i16::from_le_bytes([frame.payload[6], frame.payload[7]]),
            CHATMODE_HOME
        );
    }
    assert_chat_silent(&mut rb, "Bob is in another school");
}

/// Empty text is nothing to say, nothing sent (`:762`).
#[test]
fn empty_chat_text_is_silent() {
    use gz_proto::chat::CHATMODE_NORMAL;

    let registry = Registry::new();
    let (alice, mut ra) = chat_join(&registry, chat_info(11, "Alice", 0, 0));
    let (_bob, mut rb) = chat_join(&registry, chat_info(12, "Bob", 0, 0));

    handle_chat(
        &registry,
        alice,
        ChatRequest {
            mode: CHATMODE_NORMAL,
            system_txt: 0,
            text: String::new(),
        },
    );

    assert_chat_silent(&mut ra, "the speaker");
    assert_chat_silent(&mut rb, "Bob");
}

/// 119 resolves the target by `siCharUnique` *and* by name, independently,
/// and delivers to the charunique target with the sender's level (`:551-593`)
/// — the quirk where a message carrying Alice's id and Bob's name reaches
/// Alice is locked down here.
#[test]
fn secret_chat_delivers_to_the_charunique_target_with_the_senders_level() {
    let registry = Registry::new();
    let (alice, mut ra) = chat_join(&registry, chat_info(11, "Alice", 0, 0));
    let (_bob, mut rb) = chat_join(&registry, chat_info(12, "Bob", 0, 0));

    // Alice sends to char_unique 12 (Bob) but names "Alice" — the legacy
    // delivers to the charunique target (Bob) because it never compares.
    handle_secret_chat(
        &registry,
        alice,
        SecretChatRequest {
            char_unique: 12,
            name: "Alice".to_owned(),
            text: "for Bob".to_owned(),
        },
    );

    // Bob gets the 118 with Alice's level.
    let frame = rb.try_recv().expect("Bob gets the 119 delivery");
    assert_eq!(frame.cmd, GameMsg::ResponseChat as u16);
    let body = &frame.payload;
    assert_eq!(i16::from_le_bytes([body[2], body[3]]), 11, "the sender");
    assert_eq!(i16::from_le_bytes([body[6], body[7]]), CHATMODE_SECRET);
    assert_eq!(
        i32::from_le_bytes(body[16..20].try_into().unwrap()),
        34,
        "the sender's level"
    );
    assert_eq!(&body[20..25], b"Alice");
    assert_eq!(&body[25..], b"for Bob\0");

    // No success ack on this path.
    assert_chat_silent(&mut ra, "Alice");
}

/// A 119 with an unknown char_unique or name answers `SECRETCHAT_FAIL_NOTUSER`
/// with the literal name `"NULL"` and the request's `siCharUnique` echoed
/// (`:554`, `:564`).
#[test]
fn secret_chat_failure_names_null_and_echoes_the_request_id() {
    let registry = Registry::new();
    let (alice, mut ra) = chat_join(&registry, chat_info(11, "Alice", 0, 0));

    // Unknown char_unique.
    handle_secret_chat(
        &registry,
        alice,
        SecretChatRequest {
            char_unique: 999,
            name: "Bob".to_owned(),
            text: "hello".to_owned(),
        },
    );
    let frame = ra.try_recv().expect("Alice gets the failure");
    assert_eq!(frame.cmd, GameMsg::ResponseSecretchatresult as u16);
    assert_eq!(
        i16::from_le_bytes([frame.payload[0], frame.payload[1]]),
        SECRETCHAT_FAIL_NOTUSER
    );
    assert_eq!(&frame.payload[2..6], b"NULL");
    assert_eq!(
        i32::from_le_bytes(frame.payload[24..28].try_into().unwrap()),
        999
    );

    // Known char_unique but unknown name.
    handle_secret_chat(
        &registry,
        alice,
        SecretChatRequest {
            char_unique: 11,
            name: "Nobody".to_owned(),
            text: "hello".to_owned(),
        },
    );
    let frame = ra.try_recv().expect("Alice gets the second failure");
    assert_eq!(
        i16::from_le_bytes([frame.payload[0], frame.payload[1]]),
        SECRETCHAT_FAIL_NOTUSER
    );
    assert_eq!(&frame.payload[2..6], b"NULL");
    assert_eq!(
        i32::from_le_bytes(frame.payload[24..28].try_into().unwrap()),
        11
    );
}

/// The whisper channel's opcodes are the legacy's, so the dispatcher's
/// `c == … as u16` guards are matched: 123 `REQUEST_MAKESECRETCHATCHANNEL`,
/// 124 `RESPONSE_MAKESECRETCHATCHANNEL`, 6200/6201 friend add,
/// 6202/6203 friend add accept and the 1108 general ack.
#[test]
fn whisper_channel_and_friend_opcodes_are_the_legacy_pair() {
    assert_eq!(GameMsg::RequestMakesecretchatchannel as u16, 123);
    assert_eq!(GameMsg::ResponseMakesecretchatchannel as u16, 124);
    assert_eq!(GameMsg::RequestFriendadd as u16, 6200);
    assert_eq!(GameMsg::ResponseFriendadd as u16, 6201);
    assert_eq!(GameMsg::RequestFriendaddaccept as u16, 6202);
    assert_eq!(GameMsg::ResponseFriendaddaccept as u16, 6203);
    assert_eq!(GameMsg::ResponseServerrtn as u16, 1108);
}

/// A stat block for the whisper/friend tests — the person id the request-side
/// frames carry, the account id the replies carry.
fn basic_info_for_test(level: i16, name: &str) -> BasicInfo {
    BasicInfo {
        account_idnum: 7,
        account_id: "tester".into(),
        slot: 1,
        sex: 1,
        class_kind: 2,
        home_village: 3,
        map_index: 7,
        x: 15,
        y: 20,
        character_id: 42,
        name: name.into(),
        str_: 10,
        dex: 11,
        vit: 12,
        mag: 13,
        hnd: 14,
        luk: 15,
        wis: 16,
        level,
        bonus: 2,
        life: 100,
        mana: 50,
        exp: 0,
        money: 0,
    }
}

/// A friend repository that never persists — the registry is the only store
/// these connection-level tests exercise, so a DB outage inside the 6202
/// accept handler must not change any assertion here.
struct MemoryFriends;

#[async_trait]
impl FriendRepository for MemoryFriends {
    async fn load_owned(
        &self,
        _owner_character_id: i32,
    ) -> Result<Vec<OwnedFriend>, RepositoryError> {
        Ok(Vec::new())
    }

    async fn add(
        &self,
        _owner_character_id: i32,
        _friend_character_id: i32,
        _group: u8,
        _list_slot: i16,
    ) -> Result<(), RepositoryError> {
        Ok(())
    }
}

/// A 123 for an online target is answered on *this* connection with a 124
/// carrying the target's identity (`Server-System.cpp:638-651`) — that is what
/// opens the requester's whisper window.
#[test]
fn make_chat_channel_answers_the_requester_with_the_target() {
    let registry = Registry::new();
    let (alice, mut ra) = chat_join(&registry, chat_info(11, "Alice", 0, 0));
    let (bob_key, _rb) = chat_join(&registry, chat_info(12, "Bob", 0, 0));
    registry.set_basic_info(bob_key, basic_info_for_test(34, "Bob"));

    handle_makesecret_chat_channel(
        &registry,
        alice,
        MakeSecretChatChannelRequest {
            person_id: 0,
            char_unique: 12,
            name: String::new(),
            chat_buffer: String::new(),
        },
    );

    let frame = ra.try_recv().expect("Alice gets the 124");
    assert_eq!(frame.cmd, GameMsg::ResponseMakesecretchatchannel as u16);
    let body = &frame.payload;
    assert_eq!(
        body.len(),
        gz_proto::chat::RESPONSE_MAKESECRETCHATCHANNEL_SIZE
    );
    assert_eq!(body[0], 0, "bGameMaster");
    assert_eq!(i16::from_le_bytes([body[2], body[3]]), 12, "the target");
    assert_eq!(
        i32::from_le_bytes(body[4..8].try_into().unwrap()),
        34,
        "its level"
    );
    assert_eq!(&body[8..11], b"Bob", "its name");
    assert_eq!(&body[28..34], b"tester", "its account id");
}

/// A 123 answers the `NONAME`/`NONMAE` failure for an unknown target, which the
/// client pops as its 900/901 notice (`Server-System.cpp:653-658`).
#[test]
fn make_chat_channel_fails_with_noname_for_unknown_targets() {
    let registry = Registry::new();
    let (alice, mut ra) = chat_join(&registry, chat_info(11, "Alice", 0, 0));

    handle_makesecret_chat_channel(
        &registry,
        alice,
        MakeSecretChatChannelRequest {
            person_id: 0,
            char_unique: 999,
            name: String::new(),
            chat_buffer: String::new(),
        },
    );

    let frame = ra.try_recv().expect("Alice gets the failure");
    assert_eq!(
        i16::from_le_bytes([frame.payload[2], frame.payload[3]]),
        0,
        "siCharUnique 0"
    );
    assert_eq!(
        i32::from_le_bytes(frame.payload[4..8].try_into().unwrap()),
        0,
        "siLevel 0"
    );
    assert_eq!(&frame.payload[8..14], b"NONAME");
    assert_eq!(
        &frame.payload[28..34],
        b"NONMAE",
        "the legacy's typo, pinned"
    );
}

/// The 123 target resolution is the legacy's chain: `siCharUnique`, then
/// `siPersonID`, then `szCharName` (`Server-System.cpp:617-624`).
#[test]
fn make_chat_channel_falls_back_to_person_id_then_name() {
    let registry = Registry::new();
    let (alice, mut ra) = chat_join(&registry, chat_info(11, "Alice", 0, 0));
    let (bob_key, _rb) = chat_join(&registry, chat_info(12, "Bob", 0, 0));
    registry.set_basic_info(bob_key, basic_info_for_test(34, "Bob"));

    handle_makesecret_chat_channel(
        &registry,
        alice,
        MakeSecretChatChannelRequest {
            person_id: 42,
            char_unique: 0,
            name: String::new(),
            chat_buffer: String::new(),
        },
    );
    let frame = ra.try_recv().expect("person-id fallback");
    assert_eq!(i16::from_le_bytes([frame.payload[2], frame.payload[3]]), 12);

    handle_makesecret_chat_channel(
        &registry,
        alice,
        MakeSecretChatChannelRequest {
            person_id: 0,
            char_unique: 0,
            name: "Bob".into(),
            chat_buffer: String::new(),
        },
    );
    let frame = ra.try_recv().expect("name fallback");
    assert_eq!(i16::from_le_bytes([frame.payload[2], frame.payload[3]]), 12);
}

/// A target at or above `BASIC_GAMEMASTER_LEVEL` (100) marks the 124 as a GM
/// (`Server-System.cpp:634`); the gmmode half of the gate is unmodelled and
/// treated as "showing".
#[test]
fn make_chat_channel_marks_a_game_master() {
    let registry = Registry::new();
    let (alice, mut ra) = chat_join(&registry, chat_info(11, "Alice", 0, 0));
    let mut info = chat_info(12, "GM", 0, 0);
    info.gm_level = 100;
    let (gm_key, _rg) = chat_join(&registry, info);
    registry.set_basic_info(gm_key, basic_info_for_test(34, "GM"));

    handle_makesecret_chat_channel(
        &registry,
        alice,
        MakeSecretChatChannelRequest {
            person_id: 0,
            char_unique: 12,
            name: String::new(),
            chat_buffer: String::new(),
        },
    );
    let frame = ra.try_recv().expect("Alice gets the 124");
    assert_eq!(frame.payload[0], 1, "bGameMaster");
}

/// A 6200 names the target; an online one gets the 6201 accept dialog carrying
/// the requester's group, char id, person id and name
/// (`Server-Friend.cpp:101-104`).
#[test]
fn friend_add_forwards_an_online_target() {
    let registry = Registry::new();
    let (alice, _ra) = chat_join(&registry, chat_info(11, "Alice", 0, 0));
    let (_bob_key, mut rb) = chat_join(&registry, chat_info(12, "Bob", 0, 0));
    registry.set_basic_info(alice, basic_info_for_test(34, "Alice"));

    let mut body = vec![0u8; gz_proto::friends::REQUEST_FRIENDADD_SIZE];
    body[0] = 3;
    body[1..4].copy_from_slice(b"Bob");
    handle_friend_add(&registry, alice, &body).unwrap();

    let frame = rb.try_recv().expect("Bob gets the 6201 accept dialog");
    assert_eq!(frame.cmd, GameMsg::ResponseFriendadd as u16);
    let body = &frame.payload;
    assert_eq!(body.len(), gz_proto::friends::RESPONSE_FRIENDADD_SIZE);
    assert_eq!(body[0], 3, "the group");
    assert_eq!(
        i32::from_le_bytes(body[4..8].try_into().unwrap()),
        11,
        "the requester's char id"
    );
    assert_eq!(
        i32::from_le_bytes(body[8..12].try_into().unwrap()),
        42,
        "the requester's person id"
    );
    assert_eq!(&body[12..17], b"Alice");
}

/// An offline target answers the *requester* with the 1108 "not connected"
/// notice (`Server-Friend.cpp:82`).
#[test]
fn friend_add_notices_when_the_target_is_offline() {
    let registry = Registry::new();
    let (alice, mut ra) = chat_join(&registry, chat_info(11, "Alice", 0, 0));
    registry.set_basic_info(alice, basic_info_for_test(34, "Alice"));

    let mut body = vec![0u8; gz_proto::friends::REQUEST_FRIENDADD_SIZE];
    body[1..4].copy_from_slice(b"Bob");
    handle_friend_add(&registry, alice, &body).unwrap();

    let frame = ra.try_recv().expect("Alice gets the notice");
    assert_eq!(frame.cmd, GameMsg::ResponseServerrtn as u16);
    assert_eq!(frame.payload.len(), 8);
    assert_eq!(
        u16::from_le_bytes([frame.payload[2], frame.payload[3]]),
        7500
    );
}

/// Adding yourself or an existing friend sends nothing (`Server-Friend.cpp:43`,
/// `:52-64`).
#[test]
fn friend_add_ignores_self_and_already_friends() {
    let registry = Registry::new();
    let (alice, mut ra) = chat_join(&registry, chat_info(11, "Alice", 0, 0));
    registry.set_basic_info(alice, basic_info_for_test(34, "Alice"));

    let mut body = vec![0u8; gz_proto::friends::REQUEST_FRIENDADD_SIZE];
    body[1..6].copy_from_slice(b"Alice");
    handle_friend_add(&registry, alice, &body).unwrap();
    assert_chat_silent(&mut ra, "self-add sends nothing");

    // After the friendship is recorded the duplicate guard blocks a re-ping
    // even though Bob is offline.
    body[1..6].fill(0);
    body[1..4].copy_from_slice(b"Bob");
    handle_friend_add(&registry, alice, &body).unwrap();
    let _ = ra.try_recv().expect("the offline notice");
    registry.friend_add(
        alice,
        crate::registry::FriendEntry {
            person_id: 42,
            group: 0,
            name: "Bob".into(),
            account_id: "tester".into(),
        },
    );
    handle_friend_add(&registry, alice, &body).unwrap();
    assert_chat_silent(&mut ra, "already-friends sends nothing");
}

/// The acceptor's 6202 records the friendship on both sides and appends the
/// accepted friend to the requester's "New Friend" list with a 6203 carrying
/// the *friend's* identity (`Client-Friend.cpp:47-59`).
#[tokio::test]
async fn friend_add_accept_appends_to_the_requester() {
    let registry = Registry::new();
    let (alice_key, mut ra) = chat_join(&registry, chat_info(11, "Alice", 0, 0));
    let (bob_key, mut rb) = chat_join(&registry, chat_info(12, "Bob", 0, 0));
    registry.set_basic_info(alice_key, basic_info_for_test(34, "Alice"));
    registry.set_basic_info(bob_key, basic_info_for_test(75, "Bob"));

    let mut body = vec![0u8; gz_proto::friends::REQUEST_FRIENDADDACCEPT_SIZE];
    body[0] = 9;
    body[4..8].copy_from_slice(&42i32.to_le_bytes());
    body[8..12].copy_from_slice(&11i32.to_le_bytes());
    body[12..17].copy_from_slice(b"Alice");
    handle_friend_add_accept(&MemoryFriends, &registry, bob_key, &body)
        .await
        .unwrap();

    let frame = ra.try_recv().expect("Alice gets the 6203");
    assert_eq!(frame.cmd, GameMsg::ResponseFriendaddaccept as u16);
    let body = &frame.payload;
    assert_eq!(body.len(), gz_proto::friends::RESPONSE_FRIENDADDACCEPT_SIZE);
    assert_eq!(i16::from_le_bytes([body[0], body[1]]), 0, "first slot");
    assert_eq!(body[2], 9, "the group");
    assert_eq!(i32::from_le_bytes(body[4..8].try_into().unwrap()), 42);
    assert_eq!(
        i32::from_le_bytes(body[8..12].try_into().unwrap()),
        12,
        "the friend's (Bob's) char id"
    );
    assert_eq!(&body[12..15], b"Bob");
    assert_eq!(&body[32..38], b"tester", "the friend's account id");

    // Both sides now list each other; the acceptor gets no reply, its dialog
    // simply closes.
    assert!(registry.is_friend(alice_key, "Bob"));
    assert!(registry.is_friend(bob_key, "Alice"));
    assert_chat_silent(&mut rb, "Bob gets no reply");

    // A duplicated accept — Bob clicking accept again after the friendship is
    // on his list — is dropped before any push (`Server-Friend.cpp:193-205`),
    // so Alice must not get a second 6203.
    handle_friend_add_accept(&MemoryFriends, &registry, bob_key, body)
        .await
        .unwrap();
    assert_chat_silent(&mut ra, "duplicate accept sends no second 6203");
}

/// The 6209 friend-list request is answered with a 6210 whose slots echo the
/// 6203-reported positions, so the requester's accepted friend shows up in its
/// tab under its group, with live map/level/kind while the friend is online
/// (`Client-Friend.cpp:107-205`).
#[tokio::test]
async fn friend_list_replies_with_requester_s_friends_at_their_slots() {
    let registry = Registry::new();
    let (alice_key, mut ra) = chat_join(&registry, chat_info(11, "Alice", 0, 0));
    let (bob_key, mut rb) = chat_join(&registry, chat_info(12, "Bob", 0, 0));
    registry.set_basic_info(alice_key, basic_info_for_test(34, "Alice"));
    registry.set_basic_info(bob_key, basic_info_for_test(75, "Bob"));

    let mut body = vec![0u8; gz_proto::friends::REQUEST_FRIENDADDACCEPT_SIZE];
    body[0] = 9;
    body[4..8].copy_from_slice(&42i32.to_le_bytes());
    body[8..12].copy_from_slice(&11i32.to_le_bytes());
    body[12..17].copy_from_slice(b"Alice");
    handle_friend_add_accept(&MemoryFriends, &registry, bob_key, &body)
        .await
        .unwrap();
    let _ = ra.try_recv().expect("the 6203");

    // The empty 1-byte request body is ignored, so handle it as the client does.
    handle_friend_list(&registry, alice_key);

    let frame = ra.try_recv().expect("Alice gets the 6210");
    assert_eq!(frame.cmd, GameMsg::ResponseFriendlist as u16);
    let body = &frame.payload;
    assert_eq!(body.len(), gz_proto::friends::RESPONSE_FRIENDLIST_SIZE);
    assert_eq!(body[0], 9, "slot 0's group echoes the 6203's");
    assert_eq!(
        i32::from_le_bytes(body[100..104].try_into().unwrap()),
        42,
        "slot 0's person id (Bob)"
    );
    assert_eq!(&body[104..107], b"Bob");
    assert_eq!(
        i32::from_le_bytes(body[124..128].try_into().unwrap()),
        0,
        "slot 1 is empty"
    );
    assert_eq!(
        i32::from_le_bytes(body[2500..2504].try_into().unwrap()),
        7,
        "siMapIndex[0] from Bob's live session"
    );
    assert_eq!(
        i32::from_le_bytes(body[2900..2904].try_into().unwrap()),
        34,
        "siLevel[0] from the chat_join fixture"
    );
    assert_eq!(
        i32::from_le_bytes(body[3700..3704].try_into().unwrap()),
        1,
        "siCharKind[0]"
    );
    assert_chat_silent(&mut rb, "Bob gets nothing from Alice's list request");
}

// ---------------------------------------------------------------------------
// Batch 1 (`AUDIT_WHOLE_SYSTEM_HARDENING.md` §7): backpressure (H1) and the
// handshake deadline / liveness decision (H2). Tests T1–T5.
// ---------------------------------------------------------------------------

/// A loopback socket pair: one end for the handler under test, one for the
/// hostile peer the test drives. (movement's test module has the same helper;
/// both are private to their modules.)
async fn socket_pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port");
    let addr = listener.local_addr().expect("the bound address");
    let writer = TcpStream::connect(addr).await.expect("the client end");
    let (reader, _) = listener.accept().await.expect("the server end");
    (writer, reader)
}

/// A connected pair whose kernel buffers are ~4 KB on both ends, set **before**
/// the handshake: Windows computes the window scale from the receive buffer at
/// connect time and ignores post-connect shrinks, so a 60 KB frame cannot fit
/// and the writer's `write_all` genuinely blocks. The connect is blocking —
/// loopback to an already-listening socket completes immediately — which is
/// fine in a test.
async fn tiny_buffer_pair() -> (TcpStream, TcpStream) {
    use socket2::{Domain, Protocol, SockAddr, Type};
    let listener =
        socket2::Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP)).expect("a socket");
    listener
        .set_recv_buffer_size(4096)
        .expect("SO_RCVBUF before the handshake");
    listener
        .bind(&SockAddr::from(
            "127.0.0.1:0"
                .parse::<std::net::SocketAddr>()
                .expect("an address"),
        ))
        .expect("bind");
    listener.listen(8).expect("listen");
    let bound = listener
        .local_addr()
        .expect("the bound address")
        .as_socket()
        .expect("an IP address");

    let writer =
        socket2::Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP)).expect("a socket");
    writer
        .set_send_buffer_size(4096)
        .expect("SO_SNDBUF before connect");
    writer
        .connect(&SockAddr::from(bound))
        .expect("a loopback connect");

    listener
        .set_nonblocking(true)
        .expect("a nonblocking listener");
    writer.set_nonblocking(true).expect("a nonblocking writer");
    let writer = TcpStream::from_std(std::net::TcpStream::from(writer)).expect("a tokio stream");
    let listener = tokio::net::TcpListener::from_std(std::net::TcpListener::from(listener))
        .expect("a tokio listener");
    let (reader, _) = listener.accept().await.expect("the server end");
    (writer, reader)
}

/// A real-time wait that does **not** let the paused clock jump ahead of
/// in-flight socket I/O: outstanding `spawn_blocking` work counts as runtime
/// activity, so auto-advance stays off until the sleep ends (`tokio::time`
/// clock docs). Used after a write whose delivery the test must observe before
/// arming a virtual sleep.
async fn real_millis(ms: u64) {
    tokio::task::spawn_blocking(move || std::thread::sleep(Duration::from_millis(ms)))
        .await
        .expect("the blocking sleep runs");
}

/// A config with the audit's timings: `SessionTimeouts::default()` carries the
/// 10 s handshake budget the T1/T5 deadlines are measured against. The fields
/// are set directly rather than through `from_env` — the struct deliberately
/// has no `Default`, and no test reads the environment.
fn hardening_config() -> GameConfig {
    GameConfig {
        database_url: "postgres://none:none@127.0.0.1:1/none".to_owned(),
        listen: "127.0.0.1:0".to_owned(),
        link_key: (0x12, 0x34),
        client_compat_version: 1,
        service_area: 0,
        server_unique: 0,
        timeouts: gz_net::session::SessionTimeouts::default(),
        data_root: std::path::PathBuf::from("tests/data-root-unused"),
        debug_log: false,
    }
}

/// The five repositories `handle_client` takes, over one pool that never
/// connects: the T1/T5 paths end before any login, so no row is ever read.
fn lazy_repos() -> (
    PgAccountRepository,
    PgCharacterRepository,
    PgItemRepository,
    PgFriendRepository,
    PgQuestRepository,
) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://none:none@127.0.0.1:1/none")
        .expect("a parseable URL, connected only on first use");
    (
        PgAccountRepository::new(pool.clone()),
        PgCharacterRepository::new(pool.clone()),
        PgItemRepository::new(pool.clone()),
        PgFriendRepository::new(pool.clone()),
        PgQuestRepository::new(pool),
    )
}

/// A registered-session record for the tests below.
fn session_info(char_unique: i16, name: &str) -> SessionInfo {
    SessionInfo {
        char_unique: PlayerCharUnique::try_from_si16(char_unique)
            .expect("test id in the player band"),
        name: name.to_owned(),
        level: 13,
        map_id: 158,
        x: 27,
        y: 87,
        school_id: 0,
        class_kind: 1,
        life: 161,
        move_speed: 12,
        sex: 1,
        gm_level: 0,
    }
}

/// T1 — H2: the handshake budget is an **absolute** window pinned at accept.
/// A peer that dribbles an incomplete frame once a second must still be
/// disconnected at accept + 10 s; re-arming the window from "now" on every
/// read-loop turn (the pre-audit shape) kept such a peer alive forever. The
/// kick must not fire early either: dribbled partial bytes are not a framing
/// error, so nothing may end the session before the deadline.
#[tokio::test(start_paused = true)]
async fn a_dribbling_pre_handshake_peer_is_kicked_at_the_absolute_deadline() {
    let (mut client, server) = socket_pair().await;
    let cfg = hardening_config();
    let (accounts, characters, items, friends, quests) = lazy_repos();
    let registry = Arc::new(Registry::new());
    let monsters = Arc::new(Mutex::new(monsters::MonsterWorld::empty()));
    let player_attacks = Arc::new(PlayerAttacks::default());
    let level_table = Arc::new(gz_data::levelup::LevelUpTable::empty());

    let started = tokio::time::Instant::now();
    let handler = tokio::spawn(async move {
        handle_client(
            server,
            "t1-dribbler",
            &cfg,
            accounts,
            characters,
            items,
            friends,
            quests,
            registry,
            monsters,
            player_attacks,
            level_table,
        )
        .await
    });

    // Ten bytes of a frame whose header declares 256: fed one per second the
    // frame never completes (a completed frame would legitimately establish
    // the session) and the parser never rejects it either.
    let dribble = tokio::spawn(async move {
        for byte in 0..10u8 {
            if client.write_all(&[byte]).await.is_err() {
                return; // the kick closed the socket
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        // Hold the socket open past the deadline: an early EOF here would end
        // the session through the read path and mask the handshake kick.
        tokio::time::sleep(Duration::from_secs(60)).await;
        drop(client);
    });

    let finished = tokio::time::timeout(Duration::from_millis(10_500), handler)
        .await
        .expect("the dribbler is kicked at accept + 10 s, not kept alive by its dribbles");
    assert!(
        finished.expect("the handler task joins").is_ok(),
        "the handshake kick is a clean disconnect"
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_secs(9),
        "the window is not cut short: the session was reaped after {elapsed:?}"
    );
    dribble.await.expect("the dribbler task finishes");
}

/// T2 — H1: `send_raw`'s write is bounded by [`WRITE_TIMEOUT`]. A peer that
/// never reads fills the kernel buffers and the write blocks forever without
/// the bound; with it, the error propagates out of `handle_client`, whose
/// `Registered` guard drop releases the registry entry **and** the account
/// login claim (`registry.remove` retains no claim owned by this key). This
/// drives the handler's exact shape on that path — guard, claim, register,
/// failing write — then checks both releases.
#[tokio::test(start_paused = true)]
async fn a_peer_that_never_reads_is_reaped_by_the_write_timeout_and_frees_its_claim() {
    // Both kernel buffers ~4 KB, set before the connect (Windows fixes the
    // window scale at handshake time and ignores post-connect shrinks).
    let (writer, reader) = tiny_buffer_pair().await;
    // Held open and never read — the stalled peer.
    let _peer = reader;

    let registry = Arc::new(Registry::new());
    let key = Registry::next_key();
    let close = Arc::new(Notify::new());
    let (tx, _rx) = tokio::sync::mpsc::channel(OUTBOUND_CAP);

    let reaped = tokio::spawn({
        let registry = registry.clone();
        let close = close.clone();
        let tx = tx.clone();
        async move {
            // The handler's shape on this path (main.rs's setup, minus the
            // dispatch a stalled peer never reaches).
            let _registered = Registered::new(registry.clone(), key);
            assert!(
                registry
                    .claim_login(
                        42,
                        key,
                        close.clone(),
                        tx.clone(),
                        Outbound {
                            cmd: 0,
                            payload: Vec::new()
                        },
                    )
                    .is_none(),
                "the first claim takes the account"
            );
            registry.register(key, tx.clone(), close, session_info(1471, "Stalled"));
            // A duplicate login for the same account is refused while this
            // session lives: the claim is real, not decorative.
            assert_eq!(
                registry.claim_login(
                    42,
                    Registry::next_key(),
                    Arc::new(Notify::new()),
                    tx,
                    Outbound {
                        cmd: 0,
                        payload: Vec::new()
                    },
                ),
                Some(key),
                "the account is claimed while the session lives"
            );

            let mut writer = writer;
            // Manufacture the stall: one 60 KB frame fits Windows' loopback
            // buffering, so the socket is flooded with raw bytes (framing is
            // irrelevant — the peer never reads) until a write parks. The
            // paused clock auto-advances across the park, so this short
            // timeout fires exactly when the kernel buffers are full.
            let flood = vec![0u8; 16_384];
            loop {
                if tokio::time::timeout(Duration::from_millis(100), writer.write_all(&flood))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            let payload = vec![0xAA; 60_000];
            send_raw(&mut writer, &mut Cipher::new(), 0x1234, &payload).await
        }
    });

    let err = tokio::time::timeout(Duration::from_millis(10_500), reaped)
        .await
        .expect("the stalled write ends within the 10 s write timeout")
        .expect("the handler task joins")
        .expect_err("a peer that never reads cannot be written to forever");
    assert!(
        err.to_string().contains("timed out"),
        "the error names the timeout: {err}"
    );

    // The error path dropped `Registered`: entry and claim both go.
    assert!(
        registry.info(key).is_none(),
        "the registry entry is released"
    );
    let (probe_tx, _probe_rx) = tokio::sync::mpsc::channel(OUTBOUND_CAP);
    assert!(
        registry
            .claim_login(
                42,
                Registry::next_key(),
                Arc::new(Notify::new()),
                probe_tx,
                Outbound {
                    cmd: 0,
                    payload: Vec::new()
                },
            )
            .is_none(),
        "the login claim is released with the session"
    );
}

/// T3 — H1: the per-session outbound queue is bounded at [`OUTBOUND_CAP`].
/// Over the cap a broadcast frame is **dropped** for that session (the fan
/// keeps its non-blocking contract; sessions with room still receive), while
/// a direct reply closes the session — a direct reply is one the peer is
/// waiting on, and a half-answered session is worse than a disconnected one.
/// Either way the queue never grows past the cap.
#[tokio::test]
async fn a_saturated_outbound_queue_drops_broadcasts_and_closes_direct_replies() {
    let registry = Registry::new();
    let key = Registry::next_key();
    let (tx, rx) = tokio::sync::mpsc::channel(OUTBOUND_CAP);
    let close = Arc::new(Notify::new());
    registry.register(key, tx, close.clone(), session_info(1471, "Full"));
    // One session with room, for the fan's other half.
    let (spare_key, mut spare_rx) = registry_join(&registry, 1472, "Spare", 27, 87);

    for i in 0..OUTBOUND_CAP {
        assert!(registry.send_to(key, 1, vec![i as u8]), "queued {i}");
    }
    assert_eq!(rx.len(), OUTBOUND_CAP, "the queue is exactly at its cap");

    // The broadcast fan stays non-blocking: the full session's frame is
    // dropped, the spare's is delivered.
    assert_eq!(registry.broadcast_all(2, Vec::new()), 1);
    assert_eq!(
        rx.len(),
        OUTBOUND_CAP,
        "the dropped broadcast changed nothing"
    );
    assert_eq!(
        spare_rx
            .try_recv()
            .expect("the spare got the broadcast")
            .cmd,
        2
    );

    // A direct reply to a full queue closes the session instead of queueing.
    assert!(!registry.send_to(key, 3, Vec::new()));
    tokio::time::timeout(Duration::from_millis(10), close.notified())
        .await
        .expect("a full direct reply wakes the session's close");
    assert_eq!(
        rx.len(),
        OUTBOUND_CAP,
        "the refused frame was not smuggled in"
    );

    // The neighbour's backpressure never touches a session with room.
    assert!(registry.send_to(spare_key, 4, Vec::new()));
}

/// T4 — H1: the accept loop takes a semaphore permit *before* spawning the
/// handler, so at [`MAX_CONCURRENT_CONNS`] the next socket is dropped rather
/// than queued or silently serviced. `admit` is that gate; the loop spawns
/// only when it returns a permit.
#[test]
fn the_connection_cap_refuses_the_next_admission_and_frees_slots_on_completion() {
    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_CONNS));
    let held: Vec<_> = (0..MAX_CONCURRENT_CONNS)
        .map(|_| admit(&slots).expect("an admission within the cap"))
        .collect();
    assert!(
        admit(&slots).is_none(),
        "one past the cap is refused before any handler is spawned"
    );
    drop(held);
    assert!(admit(&slots).is_some(), "a finished session frees its slot");
}

/// T5 — H2: the decision this locks is **no idle kick for an established
/// session**. Liveness after the handshake is TCP keepalive's job (armed on
/// accept: 60 s idle, 10 s probes), not a read-window timer — so a session
/// that stays silent for three minutes is still alive. A peer whose socket
/// *errors* (an RST, the closest thing a unit test can produce to a
/// keepalive-detected death) is reaped through the read loop's error path.
/// The keepalive *timing* itself is a real-time measurement (audit §9 [R]),
/// not something a paused clock can reach.
#[tokio::test(start_paused = true)]
async fn an_established_session_is_never_idle_kicked_and_a_dead_peer_is_reaped() {
    let (mut client, server) = socket_pair().await;
    let cfg = hardening_config();
    let link_key = cfg.link_key;
    let (accounts, characters, items, friends, quests) = lazy_repos();
    let registry = Arc::new(Registry::new());
    let monsters = Arc::new(Mutex::new(monsters::MonsterWorld::empty()));
    let player_attacks = Arc::new(PlayerAttacks::default());
    let level_table = Arc::new(gz_data::levelup::LevelUpTable::empty());

    let mut handler = tokio::spawn(async move {
        handle_client(
            server,
            "t5-idle",
            &cfg,
            accounts,
            characters,
            items,
            friends,
            quests,
            registry,
            monsters,
            player_attacks,
            level_table,
        )
        .await
    });

    // S8: the greeting carries the session keys, enciphered under the link
    // cipher from configuration — the client's first read.
    let mut link = Cipher::session_cipher(link_key.0, link_key.1);
    let mut head = [0u8; 2];
    client
        .read_exact(&mut head)
        .await
        .expect("the greeting's size field");
    let size = usize::from(u16::from_le_bytes(head));
    assert_eq!(size, HEADER_SIZE + 8, "one CharUnique body");
    let mut wire = vec![0u8; size];
    wire[..2].copy_from_slice(&head);
    client
        .read_exact(&mut wire[2..])
        .await
        .expect("the greeting's body");
    let mut plain = vec![0u8; size];
    assert!(
        link.decode(&wire, &mut plain)
            .expect("the greeting decodes")
    );
    assert_eq!(
        u16::from_le_bytes([plain[3], plain[4]]),
        GameMsg::ResponseClientcharunique as u16,
        "opcode 1"
    );
    let (key1, key2) = (plain[9], plain[10]);

    // One complete frame under the negotiated cipher: the first inbound frame
    // establishes the session, and from here the handshake deadline no longer
    // applies (`gz_net::session`, `establish`).
    let mut session = Cipher::session_cipher(key1, key2);
    let heartbeat = Frame::new(GameMsg::RequestHeartbeat as u16, &[])
        .expect("a 5-byte frame")
        .into_bytes();
    client
        .write_all(&session.encode(&heartbeat).expect("encipher"))
        .await
        .expect("the heartbeat is written");
    // Real time, not virtual: the heartbeat's I/O completion must land (and the
    // handler establish the session) before any virtual sleep arms, or the
    // paused clock would advance straight to the handshake deadline and kick a
    // session that never saw its chance.
    real_millis(50).await;

    // Three idle minutes: nothing may kick it. With no timers armed on the
    // handler side, tokio's paused clock advances straight through.
    tokio::time::sleep(Duration::from_secs(180)).await;
    tokio::time::timeout(Duration::from_millis(50), &mut handler)
        .await
        .expect_err("an established session is never idle-kicked");

    // The peer vanishes without a FIN: SO_LINGER 0 turns the close into an
    // RST, so the server's read fails — the socket-error reap path. The
    // blocking wait again keeps the clock from jumping past the reset's I/O
    // completion.
    socket2::SockRef::from(&client)
        .set_linger(Some(Duration::ZERO))
        .expect("SO_LINGER is settable on loopback");
    drop(client);
    real_millis(50).await;
    let finished = tokio::time::timeout(Duration::from_secs(5), handler)
        .await
        .expect("the dead peer's session is reaped promptly")
        .expect("the handler task joins");
    assert!(finished.is_err(), "an RST surfaces as a read error");
}
