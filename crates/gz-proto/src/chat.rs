//! The chat family: 117/118 general chat, 119/120 whisper, 121/122 head chat
//! and the 112 notice — the wire half of T-018 slice 5.
//!
//! The structs live in `MsgType-System.h:961-1160` (the notice in
//! `MsgType.h:3256-3289`), the handlers in `Server-System.cpp:527-1192`.
//! Payloads are headerless like every other module here: the 5-byte
//! `sPacketHeader` belongs to `gz_net`, and the caller sends a body of exactly
//! `siDataSize` bytes for the variable-length frames — which is what the
//! client's receive loop copies (`cltMsg::GetTotalSize`, `MsgType.h:2979`).
//!
//! # 117 request — `cltGameMsgRequest_Chat` (`MsgType-System.h:1100-1117`)
//!
//! | offset | size | field |
//! |---|---|---|
//! | `0` | 2 | `siDataSize` = 8 + strlen(text) + 1 |
//! | `2` | 2 | `siChatMode` (`CHATMODE_*`) |
//! | `4` | 4 | `siSystemTxt` — text-manager id for system messages, else 0 |
//! | `8` | len+1 | `szChatText` (NUL-terminated) |
//!
//! The struct declares `szChatText[256]` and `StringCchCopy`'s it, so a body
//! can carry at most 255 bytes of text ([`decode_chat_request`] caps the same
//! way); the shipped client sends `siDataSize`, i.e. only the bytes up to the
//! terminator.
//!
//! # 118 response — `cltGameMsgResponse_Chat` (`MsgType-System.h:1123-1160`)
//!
//! | offset | size | field |
//! |---|---|---|
//! | `0` | 2 | `siDataSize` = 20 + name + text + 1 |
//! | `2` | 2 | `siCharUnique` — the sender |
//! | `4` | 2 | `PlayerNameLength` (bytes, no NUL) |
//! | `6` | 2 | `siMode` |
//! | `8` | 1 | `bGameMaster` — always 0 (see the `bGameMaster` skip) |
//! | `9` | 3 | *(pad)* — the legacy stores `sizeof(BOOL)` = 4 over the `bool`, so its 3 trailing bytes were uninitialised stack; this port sends zeros |
//! | `12` | 4 | `siSystemTxt` (echoed from the request) |
//! | `16` | 4 | `siLevel` — **`SI32`, not `SI16`** |
//! | `20` | len+1 | `name | text | NUL` — concatenated, **no NUL between name and text** |
//!
//! The legacy fills the text with `memcpy(..., 100)` (`:1155`), so the text
//! caps at 100 bytes and the name at `MAX_PLAYER_NAME - 1` = 19; the response
//! length is whatever `strlen(name | text)` came out as, which is why this is
//! variable-length rather than the declared `szChatText[256]`.
//!
//! # 119 request — `cltGameMsgSecretChat` (`MsgType-System.h:1024-1037`)
//!
//! | offset | size | field |
//! |---|---|---|
//! | `0` | 2 | `siCharUnique` — the target |
//! | `2` | 20 | `szCharName` (`MAX_PLAYER_NAME`) |
//! | `22` | 100 | `szChatText` (`MAX_SECRET_CHAT_TEXT_LENGTH`) |
//!
//! so the body is **122 bytes**. The legacy handler `memcpy`s 256 bytes out of
//! the 100-byte field and then forces `chatbuffer[99] = NULL`
//! (`Server-System.cpp:533-534`) — the bound copy here decodes the field and
//! caps at 99, which is the observable behaviour without the over-read.
//!
//! # 120 response — `cltGameMsgResponse_SecretChatResult`
//! (`MsgType-System.h:1005-1018`)
//!
//! | offset | size | field |
//! |---|---|---|
//! | `0` | 2 | `siResult` (`SECRETCHAT_*`) |
//! | `2` | 20 | `szName` |
//! | `22` | 2 | *(pad to the `SI32`'s 4-alignment)* |
//! | `24` | 4 | `siCharUnique` |
//!
//! so the body is **28 bytes**, sent as `sizeof(clResult)`.
//!
//! # 121 request — `cltGameMsgRequest_HeadChat` (`MsgType-System.h:961-968`)
//!
//! Just `SI16 siCharUnique` — the **subject** whose stored head text is being
//! asked for, not the asker. Body: **2 bytes**.
//!
//! # 122 response — `cltGameMsgResponse_HeadChat` (`MsgType-System.h:973-992`)
//!
//! | offset | size | field |
//! |---|---|---|
//! | `0` | 2 | `siCharUnique` — the head-chat subject |
//! | `2` | 2 | `siTextSize` = strlen(text) + 1 |
//! | `4` | len+1 | `szText` (the struct declares `[MAX_HEAD_CHAT_TEXT_SIZE]` = 128; the ctor's `memcpy` + forced `[127] = 0` caps it at 127) |
//! | | +1 | *(spare NUL)* — `GetTotalSize` adds 1 on top of `siTextSize` (`:990`), reading one byte past the terminator |
//!
//! so the body is `4 + siTextSize + 1` = **len + 6** bytes, sent as
//! `GetTotalSize()`.
//!
//! # 112 notice — `cltGameMsgResponse_Notice` (`MsgType.h:3256-3289`)
//!
//! | offset | size | field |
//! |---|---|---|
//! | `0` | 4 | `siDataSize` = 12 + name + text |
//! | `4` | 4 | `PlayerNameLength` (`SI32`) — the **name only**: the ctor
//!   measures right after copying the name and *before* appending the text
//!   (`MsgType.h:3278-3282`) |
//! | `8` | 1 | `UseChatBarNotic` |
//! | `9` | 1 | `m_bGMNotice` — always 0 here |
//! | `10` | name+text | `szChatText`: name, then text, **no NUL between** |
//! | | +2 | *(NUL + 1 spare)* — the ctor adds `length + 2` (`:3287`) |
//!
//! so the body is exactly `siDataSize` bytes, sent as `siDataSize` rather than
//! `sizeof`: the legacy passes `clNotice.siDataSize` to `cltMsg`
//! (`Server-System.cpp:982`). The name caps at 19 and the text at 100
//! (`StringCchCopy(..., MAX_PLAYER_NAME, ...)` and
//! `StringCchCopyN(..., 100)`), so a body never exceeds 131 bytes. The fail
//! notice the GM-order path sends — `"FAIL"` + `"Master Order Error"` — is 34
//! bytes, and the test below pins it.
//!
//! # Skips (documented, not implemented)
//!
//! The battle-royal stadium gate, the abuse filter, the chat rate limit (inert:
//! `dwLastChatInputClock` is never written), prison, the mute notice,
//! `bGameMaster` (always false — `uiGMMode` is unmodelled), the whisper-reject
//! flags (default false) and the summon-emote side effect. Routing decisions
//! live in `main.rs`; this module is bytes only.

use crate::wire::{Reader, WireError, Writer};

/// `CHATMODE_NORMAL` (`MsgType-System.h:1085`).
pub const CHATMODE_NORMAL: i16 = 1;
/// `CHATMODE_SECRET` (`MsgType-System.h:1086`) — whisper delivery.
pub const CHATMODE_SECRET: i16 = 2;
/// `CHATMODE_TOWN` (`MsgType-System.h:1087`) — needs a city-hall rank.
pub const CHATMODE_TOWN: i16 = 3;
/// `CHATMODE_HEAD` (`MsgType-System.h:1088`) — head chat.
pub const CHATMODE_HEAD: i16 = 4;
/// `CHATMODE_HOME` (`MsgType-System.h:1089`) — same-home-village chat.
pub const CHATMODE_HOME: i16 = 5;
/// `CHATMODE_ASK` (`MsgType-System.h:1090`).
pub const CHATMODE_ASK: i16 = 6;
/// `CHATMODE_SYSTEM` (`MsgType-System.h:1091`).
pub const CHATMODE_SYSTEM: i16 = 8;
/// `CHATMODE_VILLAGER` (`MsgType-System.h:1092`) — needs a city-hall rank.
pub const CHATMODE_VILLAGER: i16 = 9;
/// `CHATMODE_PARTY` (`MsgType-System.h:1093`) — needs a party.
pub const CHATMODE_PARTY: i16 = 10;
/// `CHATMODE_KING` (`MsgType-System.h:1094`) — needs `GRANK_KING`.
pub const CHATMODE_KING: i16 = 11;
/// `CHATMODE_NEWSITEM` (`MsgType-System.h:1095`) — consumes a news ticket.
pub const CHATMODE_NEWSITEM: i16 = 12;
/// `CHATMODE_GUILD` (`MsgType-System.h:1097`) — needs a guild rank.
pub const CHATMODE_GUILD: i16 = 13;

/// `SECRETCHAT_FAIL_INVALIDNAME` (`MsgType-System.h:998`) — no such name.
pub const SECRETCHAT_FAIL_INVALIDNAME: i16 = -1;
/// `SECRETCHAT_FAIL` (`MsgType-System.h:999`) — chat refused.
pub const SECRETCHAT_FAIL: i16 = 0;
/// `SECRETCHAT_FAIL_NOTUSER` (`MsgType-System.h:1000`) — target not online.
pub const SECRETCHAT_FAIL_NOTUSER: i16 = -2;
/// `SECRETCHAT_SUCCESS` (`MsgType-System.h:1001`).
pub const SECRETCHAT_SUCCESS: i16 = 1;

/// `MAX_PLAYER_NAME` (`CommonHeader.h:575`) — every name field on the wire is
/// this many bytes, holding at most 19 plus a terminator.
pub const MAX_PLAYER_NAME: usize = 20;

/// `MAX_HEAD_CHAT_TEXT_SIZE` (`CommonHeader.h:592`) — 128 bytes, holding at
/// most 127 plus the terminator the ctor forces.
pub const MAX_HEAD_CHAT_TEXT_SIZE: usize = 128;

/// `MAX_SECRET_CHAT_TEXT_LENGTH` (`MsgType-System.h:1023`) — the 119 text
/// field, and the legacy handler's `chatbuffer[99] = NULL` bound.
pub const MAX_SECRET_CHAT_TEXT_LENGTH: usize = 100;

/// `sizeof(cltGameMsgSecretChat)` — the 119 body.
pub const REQUEST_SECRETCHAT_SIZE: usize = 122;

/// `sizeof(cltGameMsgResponse_SecretChatResult)` — the 120 body.
pub const RESPONSE_SECRETCHATRESULT_SIZE: usize = 28;

/// `sizeof(cltGameMsgRequest_HeadChat)` — the 121 body.
pub const REQUEST_HEADCHAT_SIZE: usize = 2;

/// The fixed part of the 118 response, before `name | text | NUL`.
pub const RESPONSE_CHAT_HEADER: usize = 20;

/// `cltGameMsgRequest_Chat`, decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatRequest {
    /// `siChatMode` — one of the `CHATMODE_*` values.
    pub mode: i16,
    /// `siSystemTxt` — echoed back in the 118 response's default branch.
    pub system_txt: i32,
    /// `szChatText`, up to 255 bytes (`StringCchCopy(..., 256, ...)`).
    pub text: String,
}

/// `cltGameMsgSecretChat`, decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretChatRequest {
    /// `siCharUnique` — the whisper target.
    pub char_unique: i16,
    /// `szCharName` — the target's name, capped where the legacy's
    /// `GetIDFromName` caps it (`StringCchCopy(..., MAX_PLAYER_NAME, ...)`).
    pub name: String,
    /// `szChatText`, capped at 99 like the handler's `chatbuffer`.
    pub text: String,
}

/// Truncate `s` to at most `max` **bytes**, always on a UTF-8 char boundary.
///
/// Every cap in this module is a byte cap inherited from a fixed C buffer, and
/// slicing a `String` at a non-boundary byte would panic (or, done naively,
/// leave a replacement char mid-codepoint).
pub fn cut(s: &mut String, max: usize) {
    if s.len() <= max {
        return;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
}

/// Decodes the 117 body.
///
/// `siDataSize` is read (it must be there) but not enforced: the legacy never
/// cross-checks it either, and the shipped client's exact value depends on the
/// text's terminator, which is what the reader cuts on anyway.
///
/// # Errors
///
/// [`WireError::Truncated`] if fewer than the 8-byte header remains, or if the
/// text field itself is missing bytes the declared layout promises.
pub fn decode_chat_request(payload: &[u8]) -> Result<ChatRequest, WireError> {
    let mut r = Reader::new(payload);
    let _data_size = r.i16()?;
    let mode = r.i16()?;
    let system_txt = r.i32()?;
    // The struct's field is 256 bytes; read at most that, cut at the first
    // NUL, then cap like StringCchCopy's 255-char limit.
    let take = r.remaining().min(256);
    let mut text = r.fixed_str(take)?;
    cut(&mut text, 255);
    Ok(ChatRequest {
        mode,
        system_txt,
        text,
    })
}

/// Decodes the 119 body.
///
/// # Errors
///
/// [`WireError::Truncated`] if fewer than [`REQUEST_SECRETCHAT_SIZE`] bytes
/// are supplied.
pub fn decode_secret_chat_request(payload: &[u8]) -> Result<SecretChatRequest, WireError> {
    let mut r = Reader::new(payload);
    let char_unique = r.i16()?;
    let mut name = r.fixed_str(MAX_PLAYER_NAME)?;
    cut(&mut name, MAX_PLAYER_NAME - 1);
    let mut text = r.fixed_str(MAX_SECRET_CHAT_TEXT_LENGTH)?;
    cut(&mut text, MAX_SECRET_CHAT_TEXT_LENGTH - 1);
    Ok(SecretChatRequest {
        char_unique,
        name,
        text,
    })
}

/// Decodes the 121 body — the subject's `siCharUnique`.
///
/// # Errors
///
/// [`WireError::Truncated`] if fewer than [`REQUEST_HEADCHAT_SIZE`] bytes are
/// supplied.
pub fn decode_head_chat_request(payload: &[u8]) -> Result<i16, WireError> {
    Reader::new(payload).i16()
}

/// Encodes the 118 body — one chat message for one recipient. The bytes are
/// identical for every recipient: the frame describes the *sender*, not the
/// receiver (see the layout table above).
///
/// `level` is the `SI32 siLevel`; 0 for the 117 paths (the legacy's default)
/// and the sender's level for the 119 path (`Server-System.cpp:591-593`).
#[must_use]
pub fn encode_chat_response(
    char_unique: i16,
    name: &str,
    level: i32,
    mode: i16,
    system_txt: i32,
    text: &str,
) -> Vec<u8> {
    let mut name = name.to_owned();
    cut(&mut name, MAX_PLAYER_NAME - 1);
    let mut text = text.to_owned();
    cut(&mut text, 100);
    let name_len = name.len();
    let text_len = text.len();
    let payload_len = RESPONSE_CHAT_HEADER + name_len + text_len + 1;

    let mut w = Writer::with_capacity(payload_len);
    w.i16(payload_len as i16);
    w.i16(char_unique);
    w.i16(name_len as i16);
    w.i16(mode);
    w.u8(0); // bGameMaster — always false (uiGMMode unmodelled)
    w.pad(3); // the legacy's sizeof(BOOL) write left these uninitialised
    w.i32(system_txt);
    w.i32(level);
    w.bytes(name.as_bytes());
    w.bytes(text.as_bytes());
    w.u8(0); // terminator, counted by siDataSize
    w.finish()
}

/// Encodes the 120 body — a whisper ack or failure.
///
/// `char_unique` is the `SI32` field: the 117 `/t` path sends 0 (the ctor's
/// default), the 119 failure path echoes the request's id
/// (`Server-System.cpp:554`).
#[must_use]
pub fn encode_secret_chat_result(result: i16, name: &str, char_unique: i32) -> Vec<u8> {
    let mut name = name.to_owned();
    cut(&mut name, MAX_PLAYER_NAME - 1);
    let mut w = Writer::with_capacity(RESPONSE_SECRETCHATRESULT_SIZE);
    w.i16(result);
    w.fixed_str(&name, MAX_PLAYER_NAME);
    w.pad(2); // SI32 alignment: name ends at 22, siCharUnique at 24
    w.i32(char_unique);
    w.finish()
}

/// Encodes the 122 body — a stored head chat for `char_unique`'s subject.
///
/// The text caps at [`MAX_HEAD_CHAT_TEXT_SIZE`] - 1 = 127, exactly where the
/// legacy ctor forces `szText[127] = NULL`.
#[must_use]
pub fn encode_head_chat_response(char_unique: i16, text: &str) -> Vec<u8> {
    let mut text = text.to_owned();
    cut(&mut text, MAX_HEAD_CHAT_TEXT_SIZE - 1);
    let si_text_size = (text.len() + 1) as i16;
    let mut w = Writer::with_capacity(4 + text.len() + 2);
    w.i16(char_unique);
    w.i16(si_text_size);
    w.bytes(text.as_bytes());
    w.u8(0); // terminator, counted by siTextSize
    w.u8(0); // the GetTotalSize +1 spare (uninitialised stack in legacy)
    w.finish()
}

/// Encodes the 112 body — `name | text` through the chat bar or as a notice.
///
/// `m_bGMNotice` is always 0: the only caller is the failed-GM-order notice
/// (`Server-System.cpp:981`), which passes the ctor's default. The body length
/// *is* `siDataSize`, which is what `cltMsg` is given (`:982`).
#[must_use]
pub fn encode_notice(name: &str, text: &str, use_chat_bar: bool) -> Vec<u8> {
    let mut name = name.to_owned();
    cut(&mut name, MAX_PLAYER_NAME - 1);
    let mut text = text.to_owned();
    cut(&mut text, 100);
    let name_len = name.len();
    let total = name_len + text.len();
    let si_data_size = 12 + total;

    let mut w = Writer::with_capacity(si_data_size);
    w.i32(si_data_size as i32);
    // Measured before the text is appended (MsgType.h:3278-3282), so it is the
    // name's length, not the concatenated string's.
    w.i32(name_len as i32);
    w.u8(u8::from(use_chat_bar));
    w.u8(0); // m_bGMNotice
    w.bytes(name.as_bytes());
    w.bytes(text.as_bytes());
    w.u8(0); // StringCchCopyN's terminator
    w.u8(0); // the ctor's +2 spare byte (uninitialised stack in legacy)
    w.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_request_reads_the_fields_at_the_declared_offsets() {
        // siDataSize, mode, system text, "hi" + NUL.
        let mut body = Vec::new();
        body.extend_from_slice(&11i16.to_le_bytes());
        body.extend_from_slice(&CHATMODE_ASK.to_le_bytes());
        body.extend_from_slice(&42i32.to_le_bytes());
        body.extend_from_slice(b"hi\0");
        assert_eq!(body.len(), 11);

        assert_eq!(
            decode_chat_request(&body).unwrap(),
            ChatRequest {
                mode: CHATMODE_ASK,
                system_txt: 42,
                text: "hi".into(),
            }
        );
    }

    #[test]
    fn a_chat_request_without_a_body_is_empty_not_an_error() {
        let mut body = Vec::new();
        body.extend_from_slice(&9i16.to_le_bytes());
        body.extend_from_slice(&CHATMODE_NORMAL.to_le_bytes());
        body.extend_from_slice(&0i32.to_le_bytes());
        let decoded = decode_chat_request(&body).unwrap();
        assert_eq!(decoded.text, "", "no text field: empty, like the NUL case");
    }

    #[test]
    fn a_truncated_chat_request_is_rejected() {
        let mut body = Vec::new();
        body.extend_from_slice(&9i16.to_le_bytes());
        body.extend_from_slice(&CHATMODE_NORMAL.to_le_bytes());
        assert!(matches!(
            decode_chat_request(&body),
            Err(WireError::Truncated { .. })
        ));
    }

    #[test]
    fn the_chat_response_is_header_plus_name_plus_text_plus_nul() {
        let body = encode_chat_response(0x05BF, "Tester", 34, CHATMODE_NORMAL, 7, "hello");
        // 20 + 6 + 5 + 1 = 32, and siDataSize says the same.
        assert_eq!(body.len(), 32);
        assert_eq!(i16::from_le_bytes([body[0], body[1]]), 32);
        assert_eq!(i16::from_le_bytes([body[2], body[3]]), 0x05BF);
        assert_eq!(i16::from_le_bytes([body[4], body[5]]), 6, "name len");
        assert_eq!(i16::from_le_bytes([body[6], body[7]]), CHATMODE_NORMAL);
        assert_eq!(body[8], 0, "bGameMaster");
        assert_eq!(&body[9..12], &[0, 0, 0], "legacy pad, sent as zeros");
        assert_eq!(i32::from_le_bytes(body[12..16].try_into().unwrap()), 7);
        // siLevel is SI32 — getting this wrong as SI16 shifts every field after.
        assert_eq!(i32::from_le_bytes(body[16..20].try_into().unwrap()), 34);
        assert_eq!(&body[20..26], b"Tester", "name, no NUL in between");
        assert_eq!(&body[26..31], b"hello");
        assert_eq!(body[31], 0, "the counted terminator");
    }

    #[test]
    fn the_chat_response_caps_the_name_at_19_and_the_text_at_100() {
        let body =
            encode_chat_response(1, &"n".repeat(50), 0, CHATMODE_NORMAL, 0, &"t".repeat(300));
        let name_len = i16::from_le_bytes([body[4], body[5]]) as usize;
        assert_eq!(name_len, 19);
        assert_eq!(body.len(), RESPONSE_CHAT_HEADER + 19 + 100 + 1);
    }

    #[test]
    fn a_multibyte_cap_never_splits_a_character() {
        // 2-byte chars: 19 bytes of raw would land mid-char.
        let mut s = "é".repeat(30);
        cut(&mut s, 19);
        assert_eq!(s.len(), 18, "whole characters only");
        assert_eq!(s.chars().count(), 9);
    }

    #[test]
    fn the_secret_chat_request_is_122_bytes_and_caps_both_fields() {
        let mut body = vec![0u8; REQUEST_SECRETCHAT_SIZE];
        body[0..2].copy_from_slice(&9i16.to_le_bytes());
        body[2..8].copy_from_slice(b"Whisp\x00");
        body[22..27].copy_from_slice(b"hello");
        body[121] = 1; // a non-NUL byte after the text must not extend it

        let decoded = decode_secret_chat_request(&body).unwrap();
        assert_eq!(decoded.char_unique, 9);
        assert_eq!(decoded.name, "Whisp");
        assert_eq!(decoded.text, "hello");

        // The legacy forces chatbuffer[MAX_SECRET_CHAT_TEXT_LENGTH - 1] = NULL.
        let mut long = vec![b'a'; REQUEST_SECRETCHAT_SIZE];
        long[0..2].copy_from_slice(&1i16.to_le_bytes());
        let decoded = decode_secret_chat_request(&long).unwrap();
        assert_eq!(decoded.name.len(), 19);
        assert_eq!(decoded.text.len(), 99);
    }

    #[test]
    fn a_truncated_secret_chat_request_is_rejected() {
        let body = vec![0u8; REQUEST_SECRETCHAT_SIZE - 1];
        assert!(matches!(
            decode_secret_chat_request(&body),
            Err(WireError::Truncated { .. })
        ));
    }

    #[test]
    fn the_secret_chat_result_places_its_fields_at_the_zp8_offsets() {
        let body = encode_secret_chat_result(SECRETCHAT_SUCCESS, "Target", 0x05BF);
        assert_eq!(body.len(), RESPONSE_SECRETCHATRESULT_SIZE);
        assert_eq!(i16::from_le_bytes([body[0], body[1]]), SECRETCHAT_SUCCESS);
        assert_eq!(&body[2..8], b"Target");
        assert_eq!(body[8], 0, "NUL-padded name field");
        assert_eq!(&body[22..24], &[0, 0], "pad to the SI32");
        assert_eq!(i32::from_le_bytes(body[24..28].try_into().unwrap()), 0x05BF);
    }

    #[test]
    fn the_head_chat_response_is_len_plus_six() {
        let body = encode_head_chat_response(7, "top");
        assert_eq!(body.len(), 3 + 6);
        assert_eq!(i16::from_le_bytes([body[0], body[1]]), 7);
        assert_eq!(
            i16::from_le_bytes([body[2], body[3]]),
            4,
            "siTextSize = len+1"
        );
        assert_eq!(&body[4..8], b"top\0");
        assert_eq!(body[8], 0, "the GetTotalSize spare");

        // The declared field is 128 bytes with [127] forced to NULL.
        let body = encode_head_chat_response(1, &"h".repeat(500));
        assert_eq!(body.len(), (MAX_HEAD_CHAT_TEXT_SIZE - 1) + 6);
    }

    #[test]
    fn a_head_chat_request_is_two_bytes() {
        let body = 17i16.to_le_bytes();
        assert_eq!(body.len(), REQUEST_HEADCHAT_SIZE);
        assert_eq!(decode_head_chat_request(&body).unwrap(), 17);
        assert!(matches!(
            decode_head_chat_request(&body[..1]),
            Err(WireError::Truncated { .. })
        ));
    }

    /// The GM-order failure notice (`Server-System.cpp:981`) is the one notice
    /// this port sends, and its length is asserted live in `main.rs` too: the
    /// client copies `siDataSize` bytes, so a mismatch is a truncated or
    /// garbage-reading notice rather than a visible error.
    #[test]
    fn the_master_order_error_notice_is_34_bytes() {
        let body = encode_notice("FAIL", "Master Order Error", true);
        assert_eq!(body.len(), 34);
        assert_eq!(i32::from_le_bytes(body[0..4].try_into().unwrap()), 34);
        // PlayerNameLength is the name only — measured before the text is
        // appended (MsgType.h:3278-3282).
        assert_eq!(i32::from_le_bytes(body[4..8].try_into().unwrap()), 4);
        assert_eq!(body[8], 1, "UseChatBarNotic");
        assert_eq!(body[9], 0, "m_bGMNotice");
        assert_eq!(&body[10..14], b"FAIL");
        assert_eq!(&body[14..32], b"Master Order Error");
        assert_eq!(&body[32..34], &[0, 0], "terminator + spare");
    }

    #[test]
    fn the_notice_caps_its_name_and_text_like_the_ctor() {
        let body = encode_notice(&"x".repeat(40), &"y".repeat(400), false);
        // 10-byte header + 19 + 100 + 2.
        assert_eq!(body.len(), 10 + 19 + 100 + 2);
        assert_eq!(i32::from_le_bytes(body[0..4].try_into().unwrap()), 131);
        assert_eq!(
            i32::from_le_bytes(body[4..8].try_into().unwrap()),
            19,
            "name only, not name + text"
        );
    }
}
