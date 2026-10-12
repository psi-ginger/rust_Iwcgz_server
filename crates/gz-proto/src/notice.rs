//! `GAMEMSG_RESPONSE_ANOTHERAUTH` (115) — the "another client connected" notice
//! (`MsgType.h:80`, `cltGameMsgResponse_AnotherAuth` at `MsgType-System.h:2441`).
//!
//! The legacy sends this to **both** connections when a login arrives for an
//! account that already has a character online (`Server-Auth.cpp:87-114`):
//!
//! * the session that already holds the account gets `bNewUser = FALSE`, and the
//!   client shows a modal with the newcomer's IP and the time
//!   (`Client-SystemNPC.cpp:2318-2343`);
//! * the newcomer gets `bNewUser = TRUE`, and its client returns to the front
//!   (`:2345-2356`);
//! * the same `IP` (the newcomer's) and `stTime` (server local time) are used for
//!   both, only the flag differs.
//!
//! Both are then told to close. The exact body is
//! `TCHAR szIP[64]` + `SYSTEMTIME stTime` + `BOOL bNewUser`; `TCHAR` is 1 byte
//! here, `SYSTEMTIME` is eight little-endian `u16` (16 bytes) and `BOOL` is 4, so
//! `sizeof` is **84**.

use crate::wire::Writer;

/// `sizeof(cltGameMsgResponse_AnotherAuth)`, headerless.
pub const RESPONSE_ANOTHERAUTH_SIZE: usize = 84;

/// `TCHAR szIP[64]`.
const ANOTHERAUTH_IP_LEN: usize = 64;

/// `sizeof(SYSTEMTIME)` — eight little-endian `u16`s.
const SYSTEMTIME_LEN: usize = 16;

/// Encodes the notice addressed to one side of a duplicate login.
///
/// `system_time` is the 16-byte Win32 `SYSTEMTIME` body; `world_entry`'s
/// `encode_server_time` builds it for `GAMEMSG_RESPONSE_SERVERTIME`, and the
/// struct is the same one embedded here. `new_user` is the legacy `bNewUser`:
/// `false` for the session that already holds the account, `true` for the
/// newcomer.
///
/// # Panics
///
/// Panics if `system_time` is not [`SYSTEMTIME_LEN`] bytes — anything else would
/// shift the `bNewUser` flag the client branches on.
#[must_use]
pub fn encode_another_auth(ip: &str, system_time: &[u8], new_user: bool) -> Vec<u8> {
    assert_eq!(
        system_time.len(),
        SYSTEMTIME_LEN,
        "SYSTEMTIME is eight little-endian u16s"
    );
    let mut w = Writer::with_capacity(RESPONSE_ANOTHERAUTH_SIZE);
    w.fixed_str(ip, ANOTHERAUTH_IP_LEN);
    w.bytes(system_time);
    w.i32(i32::from(new_user));
    w.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Eight distinguishable little-endian `u16`s: `00 00 01 00 02 00 …`.
    fn system_time() -> Vec<u8> {
        (0u16..8).flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn holder_notice_is_the_legacy_layout() {
        let body = encode_another_auth("127.0.0.1", &system_time(), false);
        assert_eq!(body.len(), RESPONSE_ANOTHERAUTH_SIZE);
        assert_eq!(&body[..64], &{
            let mut ip = [0u8; 64];
            ip[..9].copy_from_slice(b"127.0.0.1");
            ip
        });
        assert_eq!(&body[64..80], system_time().as_slice());
        assert_eq!(
            &body[80..84],
            &[0, 0, 0, 0],
            "FALSE bNewUser is a zero BOOL"
        );
    }

    #[test]
    fn newcomer_flag_is_a_nonzero_bool() {
        let body = encode_another_auth("127.0.0.1", &system_time(), true);
        assert_eq!(&body[80..84], &[1, 0, 0, 0]);
    }

    #[test]
    fn an_overlong_ip_fills_the_field_without_writing_past_it() {
        let body = encode_another_auth(&"9".repeat(80), &system_time(), false);
        assert_eq!(body.len(), RESPONSE_ANOTHERAUTH_SIZE);
        assert!(body[..64].iter().all(|b| *b == b'9'));
    }

    #[test]
    #[should_panic(expected = "SYSTEMTIME")]
    fn a_mis_sized_time_is_rejected() {
        let _ = encode_another_auth("127.0.0.1", &[0u8; 15], false);
    }
}
