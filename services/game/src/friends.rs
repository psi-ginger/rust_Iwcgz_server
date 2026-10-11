//! The right-click Add Friend round trip: 6200 request → 6201 accept dialog to
//! the target, and the target's 6202 accept → 6203 "friend added" back to the
//! requester.
//!
//! Handlers do no I/O and no `await`: they resolve the other party through the
//! [`Registry`] and queue encoded frames on its per-session outbox, exactly like
//! the chat handlers. The sender's own replies go through the same queue.
//!
//! The client-side flow (`NRightMouseMenuDlg.cpp:249-266`) names the target by
//! character name; the legacy server resolves it against the online hash table
//! and forwards the request so the *target* decides
//! (`Server-Client\Server-Friend.cpp:19-107`). Friendships live in memory only —
//! there is no friend table in the port's database — and the friend-list family
//! (6205+ delete, 6209/6210 list, memos, groups) is deliberately not modelled:
//! the friend dialog shows an empty list, and the accept dialog's reject button
//! (6204) is dropped unhandled because telling the requester off needs the
//! letter system, which is also not modelled.

use gz_proto::friends::{
    SRVAL_FRIEND_NOTCONNECTED, decode_friend_add_accept_request, decode_friend_add_request,
    encode_friend_add_accept_response, encode_friend_add_response, encode_server_return,
};
use gz_proto::opcodes_game::GameMsg;
use gz_proto::wire::WireError;

use crate::logln;
use crate::registry::{FriendEntry, Registry, SessionKey};

/// Handles 6200 `REQUEST_FRIENDADD` (`Server-Friend.cpp:19-107`).
///
/// The guards are the legacy's: living character, a named target, not yourself
/// (`:43`), not already a friend (`:52-64`). An offline target answers the
/// requester with the 1108 `SRVAL_FRIEND_NOTCONNECTED` notice (`:82`); an online
/// one gets the 6201 accept dialog carrying the requester's group, char id,
/// person id and name (`:101-104`). Nothing is stored yet — the friendship only
/// becomes real when the target accepts via 6202.
///
/// # Errors
///
/// [`WireError::Truncated`] for a request body shorter than
/// `gz_proto::friends::REQUEST_FRIENDADD_SIZE`.
pub(crate) fn handle_friend_add(
    registry: &Registry,
    session_key: SessionKey,
    payload: &[u8],
) -> Result<(), WireError> {
    let request = decode_friend_add_request(payload)?;
    let Some(sender) = registry.info(session_key) else {
        return Ok(());
    };
    if request.name.is_empty() {
        return Ok(());
    }
    if sender.name == request.name {
        return Ok(());
    }
    if registry.is_friend(session_key, &request.name) {
        return Ok(());
    }
    let Some(target_key) = registry.find_by_name(&request.name) else {
        registry.send_to(
            session_key,
            GameMsg::ResponseServerrtn as u16,
            encode_server_return(0, SRVAL_FRIEND_NOTCONNECTED, 0, 0),
        );
        return Ok(());
    };
    let Some(basic) = registry.basic_info(session_key) else {
        return Ok(());
    };
    let body = encode_friend_add_response(
        request.group,
        i32::from(sender.char_unique),
        basic.character_id,
        &sender.name,
    );
    registry.send_to(target_key, GameMsg::ResponseFriendadd as u16, body);
    Ok(())
}

/// Handles 6202 `REQUEST_FRIENDADDACCEPT` (`Server-Friend.cpp:155-…`).
///
/// The acceptor (this connection) confirms the request, whose ids and name are
/// the original requester's. The server records the friendship in memory on
/// both sides and tells the requester with a 6203 — the message that appends
/// the accepted friend to the requester's "New Friend" dialog
/// (`Client-Friend.cpp:47-59`). The friend-list slot is where the entry landed
/// in the requester's list; the acceptor gets no reply, its dialog just closes.
/// A requester who has logged out in the meantime (or is no longer owned by
/// that name) is silently skipped — there is nobody to add the friend to.
///
/// # Errors
///
/// [`WireError::Truncated`] for a request body shorter than
/// `gz_proto::friends::REQUEST_FRIENDADDACCEPT_SIZE`.
pub(crate) fn handle_friend_add_accept(
    registry: &Registry,
    session_key: SessionKey,
    payload: &[u8],
) -> Result<(), WireError> {
    let request = decode_friend_add_accept_request(payload)?;
    if request.name.is_empty() {
        return Ok(());
    }
    let Some(acceptor) = registry.info(session_key) else {
        return Ok(());
    };
    // Parity with the legacy's pre-DB guards (`Server-Friend.cpp:184-205`): the
    // acceptor cannot add itself, and re-accepting a name already on its list is
    // dropped silently — otherwise a crafted duplicate 6202 would push a second
    // 6203 to the requester.
    if acceptor.name == request.name {
        return Ok(());
    }
    if registry.is_friend(session_key, &request.name) {
        return Ok(());
    }
    let Some(acceptor_basic) = registry.basic_info(session_key) else {
        return Ok(());
    };
    let Some(requester_key) = registry.find_by_name(&request.name) else {
        logln!(
            "game: friend add accept {} -> requester {} offline, dropped",
            acceptor.name,
            request.name
        );
        return Ok(());
    };
    // The requester's slot is the 6203's list index; a duplicate accept (already
    // friends) falls back to 0 and still pushes the dialog entry.
    let slot = registry
        .friend_add(
            requester_key,
            FriendEntry {
                char_unique: acceptor.char_unique,
                name: acceptor.name.clone(),
            },
        )
        .unwrap_or(0);
    let requester_info = registry.info(requester_key);
    if let Some(requester) = requester_info {
        registry.friend_add(
            session_key,
            FriendEntry {
                char_unique: requester.char_unique,
                name: requester.name.clone(),
            },
        );
    }
    let body = encode_friend_add_accept_response(
        slot as i16,
        request.group,
        acceptor_basic.character_id,
        i32::from(acceptor.char_unique),
        &acceptor.name,
        &acceptor_basic.account_id,
    );
    registry.send_to(requester_key, GameMsg::ResponseFriendaddaccept as u16, body);
    Ok(())
}
