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
//! the 1108 offline notice goes to the requester. The full list family (6205+
//! delete, 6209/6210 list, memos, groups) is not modelled — the friend dialog
//! simply shows an empty list.

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
}
