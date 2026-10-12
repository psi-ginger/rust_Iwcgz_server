//! The friend-list family: 6200/6201 add, 6202/6203 accept, and the 1108
//! server-return ack the add fails with when the target is offline.
//!
//! Request/response structs live in `MsgType-Friend.h`, the handlers in
//! `Server-Client\Server-Friend.cpp:19-107` (6200) and `:155-…` (6202), and the
//! general ack in `Server.cpp:5089-5109` + `CommonHeader.h:1342-1351`. Payloads
//! are headerless like every other module here.
//!
//! The port answers 6201 to the *target* (opening its accept dialog) and 6203
//! to the *requester* (adding the accepted friend to its "New Friend" list);
//! the 1108 offline notice goes to the requester, 6209/6210 serve the friend
//! dialog's list, 6219 ships the full list at world entry (`CommonHeader.h:
//! 3504-3528`, sent by `ServerDB-Friend.cpp:67-72`), and 6207/6208 push a
//! friend's login/logout to everyone who has it listed
//! (`ServerDB-Friend.cpp:45-52` login only; the logout push's body is
//! `MsgType-Friend.h:172-181`). The rest of the family (6205+ delete, memos,
//! groups) is not modelled — the tab shows whatever the list holds.

use crate::chat::{MAX_PLAYER_NAME, cut};
use crate::wire::{Reader, WireError, Writer};

/// `sizeof(cltGameMsgRequest_FriendAdd)` — the 6200 body
/// (`MsgType-Friend.h:19-30`): `SI08` + `TCHAR[20]`.
pub const REQUEST_FRIENDADD_SIZE: usize = 21;

/// `sizeof(cltGameMsgResponse_FriendAdd)` — the 6201 body
/// (`MsgType-Friend.h:32-46`): `SI08` + 3 pad + `SI32` + `SI32` + `TCHAR[20]`.
pub const RESPONSE_FRIENDADD_SIZE: usize = 32;

/// `sizeof(cltGameMsgRequest_FriendAddAccept)` — the 6202 body
/// (`MsgType-Friend.h:183-198`): `SI08` + 3 pad + `SI32` + `SI32` + `TCHAR[20]`.
pub const REQUEST_FRIENDADDACCEPT_SIZE: usize = 32;

/// `sizeof(cltGameMsgResponse_FriendAddAccept)` — the 6203 body
/// (`MsgType-Friend.h:200-…`): `SI16` + `SI08` + 1 pad + `SI32` + `SI32` +
/// `TCHAR[20]` + `TCHAR[20]`.
pub const RESPONSE_FRIENDADDACCEPT_SIZE: usize = 52;

/// `sizeof(cltServerResponseValue)` — the 1108 body (`CommonHeader.h:1342-1351`).
pub const SERVER_RESPONSE_VALUE_SIZE: usize = 8;

/// `SRVAL_FRIEND_NOTCONNECTED` (`MsgRval-Define.h:674`) — "the player you are
/// trying to add is not online".
pub const SRVAL_FRIEND_NOTCONNECTED: u16 = 7500;

/// `sizeof(cltGameMsgRequest_FriendList)` — the 6209 body: an empty class, so
/// MSVC sizes it 1 byte (`MsgType-Friend.h:72-78`).
pub const REQUEST_FRIENDLIST_SIZE: usize = 1;

/// `sizeof(cltGameMsgResponse_FriendLogin)` — the 6207 body
/// (`MsgType-Friend.h:154-169`): `SI08` + 3 pad + `SI32` + `SI32` + `TCHAR[20]`.
pub const RESPONSE_FRIENDLOGIN_SIZE: usize = 32;

/// `sizeof(cltGameMsgResponse_FriendLogOut)` — the 6208 body
/// (`MsgType-Friend.h:172-181`): one `SI32`.
pub const RESPONSE_FRIENDLOGOUT_SIZE: usize = 4;

/// `sizeof(cltGameMsgResponse_FriendAllList)` — the 6219 body: the fixed-size
/// record arrays of `cltFriendInfo` (`CommonHeader.h:3504-3528`),
/// 5 * 20 group names + 100 `SI08` groups + 100 `cltSimplePerson` (24 B) +
/// 100 * 20 account ids + 100 `SI32` char ids + 100 `bool` connect statuses.
pub const RESPONSE_FRIENDALLLIST_SIZE: usize = 5_100;

/// `MAX_FRIEND_NUMBER` (`CommonHeader.h:3501`) — the friend tables' fixed
/// record count; the client copies every record unconditionally
/// (`Client-Friend.cpp:117-120`).
pub const MAX_FRIEND_NUMBER: usize = 100;

/// `sizeof(cltGameMsgResponse_FriendList)` — the 6210 body. It is the
/// fixed-`MAX_FRIEND_NUMBER` record arrays `SI08` group, `cltSimplePerson`
/// (`SI32` + `TCHAR[20]`), the four `SI32` detail arrays, `cltSimpleRank`
/// (2 B) and `UI08`, so it never changes with the list size
/// (`MsgType-Friend.h:80-123`, `Server-Friend.cpp:269-273`).
pub const RESPONSE_FRIENDLIST_SIZE: usize =
    MAX_FRIEND_NUMBER * (1 + (4 + MAX_PLAYER_NAME) + 4 + 4 + 4 + 4 + 2 + 1);

/// `cltGameMsgRequest_FriendAdd`, decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FriendAddRequest {
    /// `siGroupID` — the group the add goes under, `FRIEND_GROUP_UNKNOWN` from
    /// the right-click menu (`NRightMouseMenuDlg.cpp:261`).
    pub group: u8,
    /// `strFriendName`, capped like the legacy's 20-byte field.
    pub name: String,
}

/// `cltGameMsgRequest_FriendAddAccept`, decoded — the **acceptor** confirming
/// the request. The ids are the original requester's (`Server-Friend.cpp:174`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FriendAddAcceptRequest {
    /// `siGroupID` — the group the friend is stored under.
    pub group: u8,
    /// `siPersonID` — the original requester's person id.
    pub person_id: i32,
    /// `siCharID` — the original requester's charunique.
    pub char_id: i32,
    /// `strCharName` — the original requester's name.
    pub name: String,
}

/// Decodes the 6200 body.
///
/// # Errors
///
/// [`WireError::Truncated`] if fewer than [`REQUEST_FRIENDADD_SIZE`] bytes are
/// supplied.
pub fn decode_friend_add_request(payload: &[u8]) -> Result<FriendAddRequest, WireError> {
    let mut r = Reader::new(payload);
    let group = r.u8()?;
    let mut name = r.fixed_str(MAX_PLAYER_NAME)?;
    cut(&mut name, MAX_PLAYER_NAME - 1);
    Ok(FriendAddRequest { group, name })
}

/// Decodes the 6202 body.
///
/// # Errors
///
/// [`WireError::Truncated`] if fewer than [`REQUEST_FRIENDADDACCEPT_SIZE`]
/// bytes are supplied.
pub fn decode_friend_add_accept_request(
    payload: &[u8],
) -> Result<FriendAddAcceptRequest, WireError> {
    let mut r = Reader::new(payload);
    let group = r.u8()?;
    r.skip(3)?; // siPersonID's 4-alignment
    let person_id = r.i32()?;
    let char_id = r.i32()?;
    let mut name = r.fixed_str(MAX_PLAYER_NAME)?;
    cut(&mut name, MAX_PLAYER_NAME - 1);
    Ok(FriendAddAcceptRequest {
        group,
        person_id,
        char_id,
        name,
    })
}

/// Encodes the 6201 body — the friend-request accept dialog, sent to the
/// **target**. `char_id` and `person_id` describe the requester
/// (`Server-Friend.cpp:102-104`).
#[must_use]
pub fn encode_friend_add_response(group: u8, char_id: i32, person_id: i32, name: &str) -> Vec<u8> {
    let mut name = name.to_owned();
    cut(&mut name, MAX_PLAYER_NAME - 1);
    let mut w = Writer::with_capacity(RESPONSE_FRIENDADD_SIZE);
    w.u8(group); // @ 0
    w.pad(3); // SI32 alignment
    w.i32(char_id); // @ 4
    w.i32(person_id); // @ 8
    w.fixed_str(&name, MAX_PLAYER_NAME); // @ 12
    w.finish()
}

/// Encodes the 6203 body — the accepted friend appended to the **requester's**
/// "New Friend" list. `index` is the friend-list slot (`0` is fine for a list
/// this port does not store); `char_id`/`person_id`/`name` are the *friend's*.
#[must_use]
pub fn encode_friend_add_accept_response(
    index: i16,
    group: u8,
    person_id: i32,
    char_id: i32,
    name: &str,
    account_id: &str,
) -> Vec<u8> {
    let mut name = name.to_owned();
    cut(&mut name, MAX_PLAYER_NAME - 1);
    let mut account_id = account_id.to_owned();
    cut(&mut account_id, MAX_PLAYER_NAME - 1);
    let mut w = Writer::with_capacity(RESPONSE_FRIENDADDACCEPT_SIZE);
    w.i16(index); // @ 0
    w.u8(group); // @ 2
    w.pad(1); // SI32 alignment
    w.i32(person_id); // @ 4
    w.i32(char_id); // @ 8
    w.fixed_str(&name, MAX_PLAYER_NAME); // @ 12
    w.fixed_str(&account_id, MAX_PLAYER_NAME); // @ 32
    w.finish()
}

/// Encodes the 1108 body — `cltServerResponseValue`, the general-purpose
/// "something went wrong" ack (`Server.cpp:5089-5109`). `receiver` is 0 from
/// every caller and `value` is an `SRVAL_*`; the client shows a message from
/// its text table keyed by the value.
#[must_use]
pub fn encode_server_return(receiver: i16, value: u16, para1: i16, para2: i16) -> Vec<u8> {
    let mut w = Writer::with_capacity(SERVER_RESPONSE_VALUE_SIZE);
    w.i16(receiver);
    w.u16(value);
    w.i16(para1);
    w.i16(para2);
    w.finish()
}

/// One non-zero slot of the 6210 body — index `i` in the entries slice is list
/// slot `i`, the position the 6203 already told the client (`friend_add`
/// appends in order, so they match).
pub struct FriendListEntry {
    /// `siGroupID` — the group the friend is stored under.
    pub group: u8,
    /// `siPersonID` — the friend's person id; the client hides a slot with a 0.
    pub person_id: i32,
    /// `szName`.
    pub name: String,
    /// `siMapIndex`, `siLevel`, `siCharKind` — online-only details; the client
    /// shows them only for connected friends (`Client-Friend.cpp:151`).
    pub map_index: i32,
    pub level: i32,
    pub char_kind: i32,
}

/// Encodes the 6210 body — `cltGameMsgResponse_FriendList`
/// (`MsgType-Friend.h:80-123`), the answer to the friend tab's 6209 request
/// (`Server-Friend.cpp:228-275`). The record arrays are fixed-size, so the
/// reply is always [`RESPONSE_FRIENDLIST_SIZE`]; empty slots and the
/// `siHomeMapIndex` and rank arrays stay zero (the port has no home maps or
/// guild ranks, and the client renders neither for 0).
#[must_use]
pub fn encode_friend_list_response(entries: &[FriendListEntry]) -> Vec<u8> {
    debug_assert!(entries.len() <= MAX_FRIEND_NUMBER);
    let zero = FriendListEntry {
        group: 0,
        person_id: 0,
        name: String::new(),
        map_index: 0,
        level: 0,
        char_kind: 0,
    };
    let at = |i: usize| entries.get(i).unwrap_or(&zero);
    let mut w = Writer::with_capacity(RESPONSE_FRIENDLIST_SIZE);
    // Column-major like the struct: one array per field.
    for i in 0..MAX_FRIEND_NUMBER {
        w.u8(at(i).group);
    }
    for i in 0..MAX_FRIEND_NUMBER {
        let e = at(i);
        let mut name = e.name.clone();
        cut(&mut name, MAX_PLAYER_NAME - 1);
        w.i32(e.person_id);
        w.fixed_str(&name, MAX_PLAYER_NAME);
    }
    for i in 0..MAX_FRIEND_NUMBER {
        w.i32(at(i).map_index);
    }
    for i in 0..MAX_FRIEND_NUMBER {
        w.i32(at(i).level);
    }
    for _ in 0..MAX_FRIEND_NUMBER {
        w.i32(0); // siHomeMapIndex
    }
    for i in 0..MAX_FRIEND_NUMBER {
        w.i32(at(i).char_kind);
    }
    for _ in 0..MAX_FRIEND_NUMBER {
        w.pad(2); // clCharRank: siType + siVillageUnique
    }
    for _ in 0..MAX_FRIEND_NUMBER {
        w.u8(0); // uiGRank
    }
    w.finish()
}

/// One non-zero slot of the 6219 body — index `i` in the entries slice is list
/// slot `i`, the position 6203 reported. Filled from the character's stored
/// friendships plus who is online right now (`ServerDB-Friend.cpp:21-65`).
pub struct FriendAllListEntry {
    /// `siGroupID[i]`.
    pub group: u8,
    /// `clFriend[i].siPersonID`; the client hides a slot with a 0.
    pub person_id: i32,
    /// `clFriend[i].szName`.
    pub name: String,
    /// `szAccountID[i]` — the friend's account id.
    pub account_id: String,
    /// `siCharID[i]` — the friend's current session character id, 0 offline.
    pub char_id: i32,
    /// `bConnectStatus[i]`.
    pub connect: bool,
}

/// Encodes the 6219 body — `cltGameMsgResponse_FriendAllList`, the friend list
/// shipped at world entry so the client's `pclFriendInfo` knows every stored
/// friend and its connect status before the dialog is ever opened
/// (`Client-Friend.cpp` `DoMsg_GAMEMSG_RESPONSE_FRIENDALLLIST`). Record arrays
/// are fixed-size, so the reply is always [`RESPONSE_FRIENDALLLIST_SIZE`];
/// group names are not modelled (the client shows the numeric group).
#[must_use]
pub fn encode_friend_all_list_response(entries: &[FriendAllListEntry]) -> Vec<u8> {
    debug_assert!(entries.len() <= MAX_FRIEND_NUMBER);
    let zero = FriendAllListEntry {
        group: 0,
        person_id: 0,
        name: String::new(),
        account_id: String::new(),
        char_id: 0,
        connect: false,
    };
    let at = |i: usize| entries.get(i).unwrap_or(&zero);
    let mut w = Writer::with_capacity(RESPONSE_FRIENDALLLIST_SIZE);
    // `szGroupName[5][MAX_FRIEND_GROUPNAME] = 20` — no group names in the port.
    for _ in 0..5 * 20 {
        w.u8(0);
    }
    for i in 0..MAX_FRIEND_NUMBER {
        w.u8(at(i).group);
    }
    for i in 0..MAX_FRIEND_NUMBER {
        let e = at(i);
        let mut name = e.name.clone();
        cut(&mut name, MAX_PLAYER_NAME - 1);
        w.i32(e.person_id);
        w.fixed_str(&name, MAX_PLAYER_NAME);
    }
    for i in 0..MAX_FRIEND_NUMBER {
        let mut account_id = at(i).account_id.clone();
        cut(&mut account_id, MAX_PLAYER_NAME - 1);
        w.fixed_str(&account_id, MAX_PLAYER_NAME);
    }
    for i in 0..MAX_FRIEND_NUMBER {
        w.i32(at(i).char_id);
    }
    for i in 0..MAX_FRIEND_NUMBER {
        w.u8(u8::from(at(i).connect));
    }
    w.finish()
}

/// Encodes the 6207 body — `cltGameMsgResponse_FriendLogin`
/// (`MsgType-Friend.h:154-169`): "friend X is online now". `group` is the
/// group the *sender* stored the friend under (the legacy passes its own
/// list's `siGroupID[i]`, `ServerDB-Friend.cpp:49`), and person/char/account
/// are the sender's own.
#[must_use]
pub fn encode_friend_login_response(
    group: u8,
    person_id: i32,
    char_id: i32,
    account_id: &str,
) -> Vec<u8> {
    let mut account_id = account_id.to_owned();
    cut(&mut account_id, MAX_PLAYER_NAME - 1);
    let mut w = Writer::with_capacity(RESPONSE_FRIENDLOGIN_SIZE);
    w.u8(group); // @ 0
    w.pad(3); // SI32 alignment
    w.i32(person_id); // @ 4
    w.i32(char_id); // @ 8
    w.fixed_str(&account_id, MAX_PLAYER_NAME); // @ 12
    w.finish()
}

/// Encodes the 6208 body — `cltGameMsgResponse_FriendLogOut`
/// (`MsgType-Friend.h:172-181`): friend `person_id` went offline.
#[must_use]
pub fn encode_friend_logout_response(person_id: i32) -> Vec<u8> {
    let mut w = Writer::with_capacity(RESPONSE_FRIENDLOGOUT_SIZE);
    w.i32(person_id);
    w.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_friend_add_request_is_21_bytes_and_reads_both_fields() {
        let mut body = vec![0u8; REQUEST_FRIENDADD_SIZE];
        body[0] = 3;
        body[1..10].copy_from_slice(b"Comrade\0\0");
        assert_eq!(
            decode_friend_add_request(&body).unwrap(),
            FriendAddRequest {
                group: 3,
                name: "Comrade".into(),
            }
        );
        assert!(matches!(
            decode_friend_add_request(&body[..20]),
            Err(WireError::Truncated { .. })
        ));
    }

    #[test]
    fn the_friend_add_response_places_its_fields_at_the_zp8_offsets() {
        let body = encode_friend_add_response(3, 0x05BF, 42, "Comrade");
        assert_eq!(body.len(), RESPONSE_FRIENDADD_SIZE);
        assert_eq!(body[0], 3);
        assert_eq!(&body[1..4], &[0, 0, 0], "pad to the SI32");
        assert_eq!(i32::from_le_bytes(body[4..8].try_into().unwrap()), 0x05BF);
        assert_eq!(i32::from_le_bytes(body[8..12].try_into().unwrap()), 42);
        assert_eq!(&body[12..19], b"Comrade");
        assert_eq!(body[19], 0, "NUL-padded name");
        assert_eq!(body.len(), 32);
    }

    #[test]
    fn the_friend_add_accept_request_reads_all_four_fields() {
        let mut body = vec![0u8; REQUEST_FRIENDADDACCEPT_SIZE];
        body[0] = 9;
        body[4..8].copy_from_slice(&77i32.to_le_bytes());
        body[8..12].copy_from_slice(&0x05BFi32.to_le_bytes());
        body[12..19].copy_from_slice(b"Tester\0");
        assert_eq!(
            decode_friend_add_accept_request(&body).unwrap(),
            FriendAddAcceptRequest {
                group: 9,
                person_id: 77,
                char_id: 0x05BF,
                name: "Tester".into(),
            }
        );
        assert!(matches!(
            decode_friend_add_accept_request(&body[..31]),
            Err(WireError::Truncated { .. })
        ));
    }

    #[test]
    fn the_friend_add_accept_response_places_its_fields_at_the_zp8_offsets() {
        let body = encode_friend_add_accept_response(0, 9, 42, 0x05BF, "Comrade", "tester");
        assert_eq!(body.len(), RESPONSE_FRIENDADDACCEPT_SIZE);
        assert_eq!(i16::from_le_bytes([body[0], body[1]]), 0);
        assert_eq!(body[2], 9);
        assert_eq!(body[3], 0, "pad to the SI32");
        assert_eq!(i32::from_le_bytes(body[4..8].try_into().unwrap()), 42);
        assert_eq!(i32::from_le_bytes(body[8..12].try_into().unwrap()), 0x05BF);
        assert_eq!(&body[12..19], b"Comrade");
        assert_eq!(&body[32..38], b"tester", "szAccountID at 32");
        assert_eq!(body.len(), 52);
    }

    #[test]
    fn the_server_return_ack_is_8_bytes() {
        let body = encode_server_return(0, SRVAL_FRIEND_NOTCONNECTED, 0, 0);
        assert_eq!(body.len(), SERVER_RESPONSE_VALUE_SIZE);
        assert_eq!(i16::from_le_bytes([body[0], body[1]]), 0);
        assert_eq!(u16::from_le_bytes([body[2], body[3]]), 7500);
        assert_eq!(&body[4..8], &[0, 0, 0, 0]);
    }

    #[test]
    fn the_friend_list_response_is_fixed_with_slots_and_online_details() {
        let body = encode_friend_list_response(&[FriendListEntry {
            group: 3,
            person_id: 42,
            name: "Comrade".into(),
            map_index: 158,
            level: 7,
            char_kind: 2,
        }]);
        assert_eq!(body.len(), RESPONSE_FRIENDLIST_SIZE);
        assert_eq!(body[0], 3, "slot 0 group");
        assert_eq!(body[1], 0, "slot 1 group is empty");
        // clSimplePerson[0] at offset 100, clSimplePerson[1] at 124.
        assert_eq!(
            i32::from_le_bytes(body[100..104].try_into().unwrap()),
            42,
            "clSimplePerson[0].siPersonID"
        );
        assert_eq!(&body[104..111], b"Comrade");
        assert_eq!(body[123], 0, "the name's NUL pad");
        assert_eq!(
            i32::from_le_bytes(body[124..128].try_into().unwrap()),
            0,
            "slot 1's empty person hides it from the render"
        );
        // The detail arrays follow the 100 simple persons (offset 2500).
        assert_eq!(
            i32::from_le_bytes(body[2500..2504].try_into().unwrap()),
            158,
            "siMapIndex[0]"
        );
        assert_eq!(
            i32::from_le_bytes(body[2900..2904].try_into().unwrap()),
            7,
            "siLevel[0]"
        );
        assert_eq!(
            i32::from_le_bytes(body[3700..3704].try_into().unwrap()),
            2,
            "siCharKind[0]"
        );
        assert_eq!(
            &body[3300..3700],
            vec![0u8; 400],
            "siHomeMapIndex stays 0 — no home maps in the port"
        );
        assert_eq!(&body[4100..4400], vec![0u8; 300], "the rank arrays stay 0");
    }

    #[test]
    fn the_friend_all_list_response_is_fixed_with_slots_and_connect_status() {
        let body = encode_friend_all_list_response(&[FriendAllListEntry {
            group: 3,
            person_id: 42,
            name: "Comrade".into(),
            account_id: "tester".into(),
            char_id: 0x05BF,
            connect: true,
        }]);
        assert_eq!(body.len(), RESPONSE_FRIENDALLLIST_SIZE);
        assert_eq!(&body[..100], vec![0u8; 100], "szGroupName stays empty");
        assert_eq!(body[100], 3, "slot 0 group");
        assert_eq!(body[101], 0, "slot 1 group is empty");
        // clFriend[0] at 200, clFriend[1] at 224.
        assert_eq!(
            i32::from_le_bytes(body[200..204].try_into().unwrap()),
            42,
            "clFriend[0].siPersonID"
        );
        assert_eq!(&body[204..211], b"Comrade");
        assert_eq!(
            i32::from_le_bytes(body[224..228].try_into().unwrap()),
            0,
            "slot 1's empty person hides it from the render"
        );
        // szAccountID[0] at 2600, siCharID[0] at 4600, bConnectStatus[0] at 5000.
        assert_eq!(&body[2600..2606], b"tester", "szAccountID[0]");
        assert_eq!(
            i32::from_le_bytes(body[4600..4604].try_into().unwrap()),
            0x05BF,
            "siCharID[0] — the online friend's session character id"
        );
        assert_eq!(body[5000], 1, "bConnectStatus[0]");
        assert_eq!(body[5001], 0, "slot 1 is offline");
        assert_eq!(&body[5100..], &[][..], "exactly 5,100 bytes");
    }

    #[test]
    fn the_friend_login_response_is_32_bytes_at_the_zp8_offsets() {
        let body = encode_friend_login_response(9, 42, 0x05BF, "tester");
        assert_eq!(body.len(), RESPONSE_FRIENDLOGIN_SIZE);
        assert_eq!(body[0], 9);
        assert_eq!(&body[1..4], &[0, 0, 0], "pad to the SI32");
        assert_eq!(i32::from_le_bytes(body[4..8].try_into().unwrap()), 42);
        assert_eq!(i32::from_le_bytes(body[8..12].try_into().unwrap()), 0x05BF);
        assert_eq!(&body[12..18], b"tester", "szAccountID at 12");
        assert_eq!(body.len(), 32);
    }

    #[test]
    fn the_friend_logout_response_is_a_lone_person_id() {
        let body = encode_friend_logout_response(42);
        assert_eq!(body.len(), RESPONSE_FRIENDLOGOUT_SIZE);
        assert_eq!(i32::from_le_bytes(body.try_into().unwrap()), 42);
    }
}
