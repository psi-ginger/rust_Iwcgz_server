//! Chat family — 117 `REQUEST_CHAT`, 119 `REQUEST_SECRETCHAT`, 121
//! `REQUEST_HEADCHAT`, and the routing decision tree they share.
//!
//! Handlers do no I/O and no `await`: they resolve the sender/target through the
//! [`Registry`] and queue encoded frames on its per-session outbox, and the
//! connection's read loop writes them. That keeps one reply to this connection
//! and a broadcast to everybody else travelling the same queue in order.

pub(crate) use gz_proto::chat::{
    CHATMODE_GUILD, CHATMODE_HEAD, CHATMODE_HOME, CHATMODE_KING, CHATMODE_NEWSITEM, CHATMODE_PARTY,
    CHATMODE_SECRET, CHATMODE_TOWN, CHATMODE_VILLAGER, ChatRequest, MAX_HEAD_CHAT_TEXT_SIZE,
    MAX_PLAYER_NAME, MakeSecretChatChannelRequest, SECRETCHAT_FAIL_INVALIDNAME,
    SECRETCHAT_FAIL_NOTUSER, SECRETCHAT_SUCCESS, SecretChatRequest, cut, encode_chat_response,
    encode_head_chat_response, encode_makesecret_chat_channel_response, encode_notice,
    encode_secret_chat_result,
};
use gz_proto::opcodes_game::GameMsg;

use crate::logln;
use crate::registry::{Registry, SessionKey};

/// Where one 117 request goes.
///
/// The order of the tests is the legacy's and is not interchangeable: the text
/// is checked for a leading `/` *before* the mode is looked at
/// (`pChatMsg[0] == TEXT('/')` at `:859` runs ahead of the mode chain at
/// `:1018-1184`), so `/t name` typed with some other mode selected is still a
/// whisper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ChatRoute {
    /// `chatbuffer[0] == NULL` (`:762`): nothing to say, nothing sent.
    Ignore,
    /// `/t ` or `/T ` — the 117 whisper, answered with an 118 plus a 120.
    Whisper,
    /// `/p ` or `/P ` — a party invite. The legacy answers with a
    /// `SRVAL_PARTYINVITE_*` server response (`:912-969`) this port does not
    /// carry, so the request is dropped silently.
    Party,
    /// Any other leading `/`: a GM order. `OperateGameMasterOrder` (`:978`) has
    /// no orders modelled, so it always fails and the client always gets the
    /// 112 notice.
    Command,
    /// `CHATMODE_HEAD` — store the text and fan the 122 out.
    Head,
    /// `CHATMODE_HOME` — fan the 118 out over the home village.
    Home,
    /// A mode the legacy gates behind something this port does not carry
    /// (city-hall rank, party, guild, king, news ticket): it returns without
    /// sending anything (`:1018-1042`, `:1069-1138`), and so does this port.
    /// The mode rides along for the log line.
    Dropped(i16),
    /// The default branch (`:1139`) — and the one place this port deliberately
    /// differs from it; see [`handle_chat`].
    Broadcast,
}

/// The legacy's decision tree for one 117 request, with no side effects.
pub(crate) fn route_chat(text: &str, mode: i16) -> ChatRoute {
    let bytes = text.as_bytes();
    // The empty test comes first and is unconditional (`:762`): a request with
    // no text at all is not a command, not a mode, just nothing.
    if bytes.is_empty() {
        return ChatRoute::Ignore;
    }
    if bytes[0] == b'/' {
        // The legacy reads `pChatMsg[1]` and `pChatMsg[2]` as whole characters,
        // and a text too short to hold them lands on the zero-filled tail of
        // `chatbuffer` (`:726`) — so "/t" without a space is *not* a whisper.
        // The length guard reproduces that instead of indexing absent bytes.
        if bytes.len() >= 3 {
            match (bytes[1], bytes[2]) {
                (b't', b' ') | (b'T', b' ') => return ChatRoute::Whisper,
                (b'p', b' ') | (b'P', b' ') => return ChatRoute::Party,
                _ => {}
            }
        }
        return ChatRoute::Command;
    }
    match mode {
        CHATMODE_HEAD => ChatRoute::Head,
        CHATMODE_HOME => ChatRoute::Home,
        CHATMODE_TOWN | CHATMODE_VILLAGER | CHATMODE_PARTY | CHATMODE_GUILD | CHATMODE_KING
        | CHATMODE_NEWSITEM => ChatRoute::Dropped(mode),
        // Anything else is the default branch — including `CHATMODE_SECRET`
        // sent *without* a slash, which the legacy's mode chain at `:1018-1138`
        // does not catch either, so plain text under the whisper mode is
        // broadcast as ordinary chat there and here.
        _ => ChatRoute::Broadcast,
    }
}

/// The `/t` line split the way the legacy splits it.
///
/// `_stscanf(&pChatMsg[1], TEXT("%s %s"), code, szname)` (`:865`) takes the
/// second whitespace-separated token as the target, then forces
/// `szname[MAX_PLAYER_NAME - 1] = NULL` (`:867`) — at most 19 bytes. The
/// message is *not* tokenised: it is recomputed as
/// `&pChatMsg[1 + 1 + 1 + (_tcslen(szname) + 1)]` (`:999`), a fixed
/// `4 + target.len()` bytes from the start of the line. So extra whitespace, a
/// target over the cap, or no message at all all shift or miss exactly as that
/// arithmetic shifts and misses — which is what [`tail_at`] reproduces.
///
/// Returns `(target, message)`.
pub(crate) fn whisper_parts(text: &str) -> (String, String) {
    let mut tokens = text.split_whitespace();
    let _command = tokens.next();
    let mut target = tokens.next().unwrap_or("").to_owned();
    cut(&mut target, MAX_PLAYER_NAME - 1);
    let message = tail_at(text, 4 + target.len()).to_owned();
    (target, message)
}

/// `&text[index..]`, rounded down to a character boundary; `""` past the end.
///
/// The legacy indexes a `TCHAR` buffer that was zero-filled and only ever
/// written by `StringCchCopy` (`:726-728`), so a position past the terminator
/// reads zeros and yields an empty string rather than faulting. Rounding down
/// instead of panicking is this port's half of the same "it just reads nothing"
/// behaviour; the legacy would read a truncated multi-byte character here.
fn tail_at(text: &str, index: usize) -> &str {
    if index >= text.len() {
        return "";
    }
    let mut start = index;
    while start > 0 && !text.is_char_boundary(start) {
        start -= 1;
    }
    &text[start..]
}

/// Handles 117 `REQUEST_CHAT`.
///
/// The mode chain, the two slash commands that have a wire representation and
/// the GM-order fall-through all live in [`route_chat`]; what is decided here
/// is what each route *sends*.
///
/// **Deviation, the owner's instruction:** the default branch fans out to the
/// whole server instead of the near cells. The legacy sends to the 3×3 cell
/// neighbourhood (`SendNeighbourMsg(&clMsg, true)`, `:1149`); general chat is
/// deliberately global here, modern-server style, with every connected player —
/// the speaker included — receiving it. Whispers, head chat and the home-village
/// branch keep their legacy reach.
pub(crate) fn handle_chat(registry: &Registry, session_key: SessionKey, request: ChatRequest) {
    let Some(sender) = registry.info(session_key) else {
        // The legacy's `IsValidID(id)` guard (`:730`): a character that has not
        // been selected is not in the world and cannot speak.
        return;
    };
    match route_chat(&request.text, request.mode) {
        ChatRoute::Ignore => {}
        ChatRoute::Whisper => {
            let (target, message) = whisper_parts(&request.text);
            match registry.find_by_name(&target) {
                // `GetIDFromName` returned 0 (`:871`): the name resolves to
                // nobody, and the client is told so with
                // `SECRETCHAT_FAIL_INVALIDNAME` — the 120 ctor's `siCharUnique`
                // defaults to 0 (`MsgType-System.h:1011`).
                None => {
                    registry.send_to(
                        session_key,
                        GameMsg::ResponseSecretchatresult as u16,
                        encode_secret_chat_result(SECRETCHAT_FAIL_INVALIDNAME, &target, 0),
                    );
                }
                // The 118 goes to the target with `CHATMODE_SECRET`, level 0
                // and system text 0 — the ctor's defaults at `:1001` — then a
                // `SECRETCHAT_SUCCESS` ack carrying the *target's* name and
                // `siCharUnique` 0 goes to the speaker (`:1008`).
                Some(target_key) => {
                    registry.send_to(
                        target_key,
                        GameMsg::ResponseChat as u16,
                        encode_chat_response(
                            sender.char_unique,
                            &sender.name,
                            0,
                            CHATMODE_SECRET,
                            0,
                            &message,
                        ),
                    );
                    registry.send_to(
                        session_key,
                        GameMsg::ResponseSecretchatresult as u16,
                        encode_secret_chat_result(SECRETCHAT_SUCCESS, &target, 0),
                    );
                }
            }
        }
        ChatRoute::Party => {}
        ChatRoute::Command => {
            // `OperateGameMasterOrder` failed (`:978-984`) — it always does
            // here, no order is modelled — so the speaker gets the 34-byte
            // FAIL notice, and only the speaker.
            registry.send_to(
                session_key,
                GameMsg::ResponseNotice as u16,
                encode_notice("FAIL", "Master Order Error", true),
            );
        }
        ChatRoute::Head => {
            // `SetHeadChat(true, pChatMsg)` (`:1050`), whose `MStrCpy` caps the
            // stored text at 127 bytes (`char.cpp:6628`), then the 122 fan to
            // the near cells *including* the speaker
            // (`SendNeighbourMsg(&clMsg, true)`, `:1055`).
            //
            // The legacy's gate at `:1047` is "not in a personal shop"; that
            // switch is unmodelled and treated as always off, on both this side
            // and 121's.
            let mut text = request.text.clone();
            cut(&mut text, MAX_HEAD_CHAT_TEXT_SIZE - 1);
            registry.set_head_chat(session_key, Some(text.clone()));
            registry.broadcast_near(
                &sender,
                GameMsg::ResponseHeadchat as u16,
                encode_head_chat_response(sender.char_unique, &text),
            );
        }
        ChatRoute::Home => {
            // `if (pclchar->pclCI->clBI.siHomeVillage)` (`:1061`) then
            // `SendAllPersonHomeMsg(..., siHomeVillage)` (`:1066`), which
            // reaches every character of that home village *including* the
            // speaker (`CharManager.cpp:751`).
            if sender.school_id != 0 {
                registry.broadcast_school(
                    sender.school_id,
                    GameMsg::ResponseChat as u16,
                    encode_chat_response(
                        sender.char_unique,
                        &sender.name,
                        0,
                        request.mode,
                        0,
                        &request.text,
                    ),
                );
            }
        }
        ChatRoute::Dropped(mode) => {
            logln!(
                "game: {}: chat mode {mode} dropped (the legacy gates it behind a rank, \
                 party, guild, king or ticket this port does not model)",
                sender.name
            );
        }
        ChatRoute::Broadcast => {
            // The default branch: the head chat is cleared
            // (`SetHeadChat(false, NULL)`, `:1142`) and the message goes out
            // with the request's `siSystemTxt` echoed and level 0 (`:1144`).
            // The summon-emote side effect at `:1151-1181` is a documented
            // skip; summons are not modelled.
            registry.set_head_chat(session_key, None);
            registry.broadcast_all(
                GameMsg::ResponseChat as u16,
                encode_chat_response(
                    sender.char_unique,
                    &sender.name,
                    0,
                    request.mode,
                    request.system_txt,
                    &request.text,
                ),
            );
        }
    }
}

/// Handles 119 `REQUEST_SECRETCHAT` (`Server-System.cpp:527-600`).
///
/// Two lookups, deliberately independent: the target is resolved *by
/// `siCharUnique`* (`:551`) and *again by `szCharName`* (`:561`), and the
/// legacy never compares them — a message carrying Alice's id and Bob's name is
/// delivered to **Alice**, as long as both names exist. That quirk is locked
/// down by a test; nothing here "fixes" it.
///
/// A miss on either lookup answers `SECRETCHAT_FAIL_NOTUSER` with the literal
/// name `"NULL"` and the request's `siCharUnique` echoed (`:554`, `:564`). And
/// there is **no** success ack on this path: the `SECRETCHAT_SUCCESS` 120 is
/// sent only by the 117 `/t` branch (`:1008`).
pub(crate) fn handle_secret_chat(
    registry: &Registry,
    session_key: SessionKey,
    request: SecretChatRequest,
) {
    let Some(sender) = registry.info(session_key) else {
        return;
    };
    let not_user = encode_secret_chat_result(
        SECRETCHAT_FAIL_NOTUSER,
        "NULL",
        i32::from(request.char_unique),
    );
    let target = match registry.find_by_charunique(request.char_unique) {
        Some(target) => target,
        None => {
            registry.send_to(
                session_key,
                GameMsg::ResponseSecretchatresult as u16,
                not_user,
            );
            return;
        }
    };
    if registry.find_by_name(&request.name).is_none() {
        registry.send_to(
            session_key,
            GameMsg::ResponseSecretchatresult as u16,
            not_user,
        );
        return;
    }
    // The delivery goes to the *charunique* target, and it is the only path
    // that fills in 118's `siLevel`: `level = pclCM->CR[id]->pclCI->clIP.
    // GetLevel()` (`:591`), the sender's level.
    registry.send_to(
        target,
        GameMsg::ResponseChat as u16,
        encode_chat_response(
            sender.char_unique,
            &sender.name,
            sender.level,
            CHATMODE_SECRET,
            0,
            &request.text,
        ),
    );
}

/// Handles 121 `REQUEST_HEADCHAT` (`Server-System.cpp:693-720`).
///
/// The request names the **subject**, not the asker, and the reply goes to
/// whoever asked: it carries the subject's stored text and the subject's
/// `siCharUnique`. Nothing is sent when the subject is not online
/// (`GetIDFromCharUnique` fails, `:708`) or when its head-chat switch is off
/// (`:712`), which is `None` in the registry. The personal-shop gate at `:713`
/// is treated as always open, exactly as 117's is.
pub(crate) fn handle_head_chat_request(registry: &Registry, session_key: SessionKey, subject: i16) {
    let Some(subject_key) = registry.find_by_charunique(subject) else {
        return;
    };
    let Some(text) = registry.head_chat(subject_key) else {
        return;
    };
    registry.send_to(
        session_key,
        GameMsg::ResponseHeadchat as u16,
        encode_head_chat_response(subject, &text),
    );
}

/// `BASIC_GAMEMASTER_LEVEL` (`Char-Common.h:219`) — the `>=` gate for the
/// 124 reply's `bGameMaster`.
const BASIC_GAMEMASTER_LEVEL: u8 = 100;

/// Handles 123 `REQUEST_MAKESECRETCHATCHANNEL` (`Server-System.cpp:603-661`).
///
/// The reply goes to the **requester only**: `DoMsg_GAMEMSG_RESPONSE_
/// MAKESECRETCHATCHANNEL` pushes the request into its own private-chat manager
/// (`Client-System.cpp:1424-1429`). The *target's* window opens when the first
/// 119 text reaches it — the 118 handler pushes mode `CHATMODE_SECRET` into the
/// same manager (`Client-System.cpp:1494-1500`) — so there is no 124 echo to
/// both sides in the legacy, and none here.
///
/// The target is resolved by `siCharUnique`, then `siPersonID`, then
/// `szCharName` (`:617-624`); the reply carries `bGameMaster` (level gate only —
/// `uiGMMode` is unmodelled and treated as always showing), the target's level
/// and name, its account id, and the request's own buffer echoed. A miss, or a
/// level outside the legacy's `1..MAX_CHARACTER_LEVEL` (> 0 only here: nothing
/// in this port's data can exceed the cap), answers the `NONAME`/`NONMAE`
/// failure (`:653-658`), which the client pops as its 900/901 notice.
pub(crate) fn handle_makesecret_chat_channel(
    registry: &Registry,
    session_key: SessionKey,
    request: MakeSecretChatChannelRequest,
) {
    let Some(target_key) = resolve_secret_chat_target(registry, &request) else {
        send_secret_chat_failure(registry, session_key);
        return;
    };
    let Some(target) = registry.info(target_key) else {
        return;
    };
    if !(1..).contains(&target.level) {
        send_secret_chat_failure(registry, session_key);
        return;
    }
    let Some(basic) = registry.basic_info(target_key) else {
        return;
    };
    send_secret_chat_channel(
        registry,
        session_key,
        encode_makesecret_chat_channel_response(
            target.gm_level >= BASIC_GAMEMASTER_LEVEL,
            target.char_unique,
            target.level,
            &target.name,
            &basic.account_id,
            &request.chat_buffer,
        ),
    );
}

/// The legacy's three-way target resolution for 123 (`Server-System.cpp:617-624`).
fn resolve_secret_chat_target(
    registry: &Registry,
    request: &MakeSecretChatChannelRequest,
) -> Option<SessionKey> {
    if request.char_unique > 0 {
        return registry.find_by_charunique(request.char_unique);
    }
    if request.person_id > 0
        && let Some(key) = registry.find_by_character_id(request.person_id)
    {
        return Some(key);
    }
    if !request.name.is_empty() {
        return registry.find_by_name(&request.name);
    }
    None
}

/// Queues one 124 to `key`.
fn send_secret_chat_channel(registry: &Registry, key: SessionKey, body: Vec<u8>) {
    registry.send_to(key, GameMsg::ResponseMakesecretchatchannel as u16, body);
}

/// The legacy's `NONAME`/`NONMAE` failure body (`Server-System.cpp:653-658`) —
/// level 0, no char, no GM, the account field carrying the legacy's typo.
fn send_secret_chat_failure(registry: &Registry, key: SessionKey) {
    send_secret_chat_channel(
        registry,
        key,
        encode_makesecret_chat_channel_response(false, 0, 0, "NONAME", "NONMAE", ""),
    );
}
