//! The right-click Add Friend round trip: 6200 request → 6201 accept dialog to
//! the target, and the target's 6202 accept → 6203 "friend added" back to the
//! requester.
//!
//! Handlers do no I/O and no `await`: they resolve the other party through the
//! [`Registry`] and queue encoded frames on its per-session outbox, exactly like
//! the chat handlers. The sender's own replies go through the same queue. The
//! one exception is the 6202 accept, which persists the friendship to
//! `character_friends` before the in-memory push.
//!
//! The client-side flow (`NRightMouseMenuDlg.cpp:249-266`) names the target by
//! character name; the legacy server resolves it against the online hash table
//! and forwards the request so the *target* decides
//! (`Server-Client\Server-Friend.cpp:19-107`). Friendships are stored per
//! character in PostgreSQL, loaded at world entry, and the friend dialog shows
//! them: 6209 is answered with a 6210 list
//! (`Server-Friend.cpp:228-275`) and world entry carries the whole list as a
//! 6219 (`ServerDB-Friend.cpp:67-72`), while the rest of the family (6205+
//! delete, memos, groups) stays unmodelled. The accept dialog's reject button
//! (6204) is dropped unhandled because telling the requester off needs the
//! letter system, which is also not modelled.

use gz_db::FriendRepository;
use gz_proto::friends::{
    FriendAllListEntry, FriendListEntry, SRVAL_FRIEND_NOTCONNECTED,
    decode_friend_add_accept_request, decode_friend_add_request, encode_friend_add_accept_response,
    encode_friend_add_response, encode_friend_all_list_response, encode_friend_list_response,
    encode_server_return,
};
use gz_proto::opcodes_game::GameMsg;
use gz_proto::wire::WireError;

use crate::logln;
use crate::logln_err;
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
        i32::from(sender.char_unique.si16()),
        basic.character_id,
        &sender.name,
    );
    let _ = registry.send_to(target_key, GameMsg::ResponseFriendadd as u16, body);
    Ok(())
}

/// Handles 6202 `REQUEST_FRIENDADDACCEPT` (`Server-Friend.cpp:155-…`).
///
/// The acceptor (this connection) confirms the request, whose ids and name are
/// the original requester's. The server persists the friendship to
/// `character_friends` on both sides, records it in memory, and tells the
/// requester with a 6203 — the message that appends the accepted friend to the
/// requester's "New Friend" dialog (`Client-Friend.cpp:47-59`). The friend-list
/// slot is where the entry landed in the requester's list; the acceptor gets no
/// reply, its dialog just closes. A requester who has logged out in the
/// meantime (or is no longer owned by that name) is silently skipped — there is
/// nobody to add the friend to.
///
/// # Errors
///
/// [`WireError::Truncated`] for a request body shorter than
/// `gz_proto::friends::REQUEST_FRIENDADDACCEPT_SIZE`.
pub(crate) async fn handle_friend_add_accept(
    friends: &dyn FriendRepository,
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
    let Some(requester) = registry.info(requester_key) else {
        return Ok(());
    };
    let Some(requester_basic) = registry.basic_info(requester_key) else {
        return Ok(());
    };
    // Persist both directions first, keyed to the slot each side's list is
    // about to give the friend — the count before the push. A database failure
    // degrades to the in-memory friendship rather than failing the accept
    // (the 6203 still flows; the friendship just does not survive a re-login).
    let requester_slot = registry.friends_of(requester_key).len() as i16;
    let acceptor_slot = registry.friends_of(session_key).len() as i16;
    let saved = friends
        .add(
            requester_basic.character_id,
            acceptor_basic.character_id,
            request.group,
            requester_slot,
        )
        .await
        .and(
            friends
                .add(
                    acceptor_basic.character_id,
                    requester_basic.character_id,
                    request.group,
                    acceptor_slot,
                )
                .await,
        );
    if let Err(e) = saved {
        logln_err!(
            "game: friend DB persist for {} <-> {} failed: {e}",
            acceptor.name,
            requester.name
        );
    }
    // The requester's slot is the 6203's list index; a duplicate accept (already
    // friends) falls back to 0 and still pushes the dialog entry.
    let slot = registry
        .friend_add(
            requester_key,
            FriendEntry {
                person_id: acceptor_basic.character_id,
                group: request.group,
                name: acceptor.name.clone(),
                account_id: acceptor_basic.account_id.clone(),
            },
        )
        .unwrap_or(0);
    registry.friend_add(
        session_key,
        FriendEntry {
            person_id: requester_basic.character_id,
            group: request.group,
            name: requester.name.clone(),
            account_id: requester_basic.account_id.clone(),
        },
    );
    let body = encode_friend_add_accept_response(
        slot as i16,
        request.group,
        acceptor_basic.character_id,
        i32::from(acceptor.char_unique.si16()),
        &acceptor.name,
        &acceptor_basic.account_id,
    );
    registry.send_to(requester_key, GameMsg::ResponseFriendaddaccept as u16, body);
    Ok(())
}

/// Handles 6209 `REQUEST_FRIENDLIST` — the friend dialog's (re)load
/// (`NNewFriendDlg.cpp:819-829`) asking for the list it renders. The request
/// body is an empty 1-byte struct, so it is ignored. The 6210 answer
/// (`Server-Friend.cpp:228-275`) echoes the friends at their 6203-reported
/// slots, with live map/level/kind for whoever is online right now and zeros
/// for the offline; the client only renders slots whose person id is non-zero,
/// so the empty tail stays invisible.
pub(crate) fn handle_friend_list(registry: &Registry, session_key: SessionKey) {
    let friends = registry.friends_of(session_key);
    let mut entries = Vec::with_capacity(friends.len());
    for friend in friends {
        let (map_index, level, char_kind) = registry
            .find_by_character_id(friend.person_id)
            .and_then(|key| registry.info(key))
            .map_or((0, 0, 0), |info| {
                (info.map_id, info.level, i32::from(info.class_kind))
            });
        entries.push(FriendListEntry {
            group: friend.group,
            person_id: friend.person_id,
            name: friend.name,
            map_index,
            level,
            char_kind,
        });
    }
    registry.send_to(
        session_key,
        GameMsg::ResponseFriendlist as u16,
        encode_friend_list_response(&entries),
    );
}

/// The 6219 `RESPONSE_FRIENDALLLIST` body for a session — the whole friend
/// list a freshly-entered character's client needs to draw its friend tab
/// before any 6209: names, accounts, groups, and who is online right now
/// (`cltFriendInfo`, `CommonHeader.h:3504-3528`; built by
/// `ServerDB-Friend.cpp:21-72`). Connect status and the online friend's
/// session charunique are resolved live against the registry, zeros for the
/// offline, so no state has to be kept beyond the friendships themselves.
pub(crate) fn friend_all_list_body(registry: &Registry, session_key: SessionKey) -> Vec<u8> {
    let friends = registry.friends_of(session_key);
    let mut entries = Vec::with_capacity(friends.len());
    for friend in friends {
        let (char_id, connect) = registry
            .find_by_character_id(friend.person_id)
            .and_then(|key| registry.info(key))
            .map_or((0, false), |info| {
                (i32::from(info.char_unique.si16()), true)
            });
        entries.push(FriendAllListEntry {
            group: friend.group,
            person_id: friend.person_id,
            name: friend.name,
            account_id: friend.account_id,
            char_id,
            connect,
        });
    }
    encode_friend_all_list_response(&entries)
}
