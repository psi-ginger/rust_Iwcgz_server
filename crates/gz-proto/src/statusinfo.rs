//! The character-status-change pair: `GAMEMSG_REQUEST_CHANGESTATUSINFO` (230) →
//! `GAMEMSG_RESPONSE_CHANGESTATUSINFO` (231) (`MsgType.h:268-269`).
//!
//! The client sends 230 when the player toggles the horse auto-food checkbox
//! (`NCharHorse.cpp:471`, `NCharStatus-Horse.cpp:232`) and applies the reply by
//! resolving `siCharUnique` back to a character and walking the `siChangeType`
//! switch (`Client-Person.cpp:1655-1680`).
//!
//! The legacy handler (`Server-Person.cpp:2831-2860`) acts on exactly one change
//! type, `CHANGETYPE_AUTOFOOD` (`CommonHeader.h:1617`): it records the switch and
//! echoes it back as 231. Every other `siChangeType` hits the `default:` and
//! `return`s with **no reply at all** (`:2857-2858`).
//!
//! # The request — 230
//!
//! `cltGameMsgRequest_StatusInfo` (`MsgType-Person.h:905-914`) is just a
//! `cltCharChangeStatusInfo` (`CommonHeader.h:1638-1651`). With `SI32` 4-aligned,
//! the `bool` is followed by three pad bytes:
//!
//! | offset | size | field |
//! |---|---|---|
//! | `0` | 4 | `siChangeType` |
//! | `4` | 1 | `bChangeSwitch` |
//! | `5` | 3 | *(pad)* |
//! | `8` | 4 | `siChageAmount1` |
//! | `12` | 4 | `siChageAmount2` |
//! | `16` | 4 | `siChageAmount3` |
//!
//! so the body is **20 bytes** — the length the shipped client sent live (the
//! triage log recorded `recv opcode 230 (20 bytes)`).
//!
//! # The response — 231
//!
//! `cltGameMsgResponse_StatusInfo` (`MsgType-Person.h:916-928`) prepends the
//! target's `siCharUnique`:
//!
//! | offset | size | field |
//! |---|---|---|
//! | `0` | 2 | `siCharUnique` |
//! | `2` | 2 | *(pad to `cltCharChangeStatusInfo`'s 4-alignment)* |
//! | `4` | 20 | `cltCharChangeStatusInfo` |
//!
//! so the body is **24 bytes**.

use crate::wire::{Reader, WireError};

/// `CHANGETYPE_AUTOFOOD` (`CommonHeader.h:1617`) — the only change type the
/// legacy `230` handler acts on (`Server-Person.cpp:2841`).
pub const CHANGETYPE_AUTOFOOD: i32 = 12;

/// `sizeof(cltGameMsgRequest_StatusInfo)` — the 230 body.
pub const REQUEST_STATUSINFO_SIZE: usize = 20;

/// `sizeof(cltGameMsgResponse_StatusInfo)` — the 231 body.
pub const RESPONSE_STATUSINFO_SIZE: usize = 24;

/// `cltCharChangeStatusInfo` (`CommonHeader.h:1638-1651`), decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangeStatusInfo {
    /// `siChangeType` — one of the `CHANGETYPE_*` values.
    pub change_type: i32,
    /// `bChangeSwitch` — the new value of the named switch.
    pub change_switch: bool,
    /// `siChageAmount1`.
    pub amount1: i32,
    /// `siChageAmount2`.
    pub amount2: i32,
    /// `siChageAmount3`.
    pub amount3: i32,
}

impl ChangeStatusInfo {
    /// Encodes the 231 body, addressing it to `char_unique`.
    #[must_use]
    pub fn encode_response(&self, char_unique: i16) -> [u8; RESPONSE_STATUSINFO_SIZE] {
        let mut out = [0u8; RESPONSE_STATUSINFO_SIZE];
        out[0..2].copy_from_slice(&char_unique.to_le_bytes());
        // 2..4: pad to cltCharChangeStatusInfo's 4-byte alignment.
        out[OFFSET_CHANGE_TYPE..OFFSET_CHANGE_TYPE + 4]
            .copy_from_slice(&self.change_type.to_le_bytes());
        out[OFFSET_CHANGE_SWITCH] = u8::from(self.change_switch);
        // 9..12: pad to siChageAmount1's 4-byte alignment.
        out[OFFSET_AMOUNT1..OFFSET_AMOUNT1 + 4].copy_from_slice(&self.amount1.to_le_bytes());
        out[OFFSET_AMOUNT2..OFFSET_AMOUNT2 + 4].copy_from_slice(&self.amount2.to_le_bytes());
        out[OFFSET_AMOUNT3..OFFSET_AMOUNT3 + 4].copy_from_slice(&self.amount3.to_le_bytes());
        out
    }
}

/// `siChangeType` inside the 231 body (the change block starts at 4).
const OFFSET_CHANGE_TYPE: usize = 4;
/// `bChangeSwitch` inside the 231 body.
const OFFSET_CHANGE_SWITCH: usize = 8;
/// `siChageAmount1` inside the 231 body.
const OFFSET_AMOUNT1: usize = 12;
/// `siChageAmount2` inside the 231 body.
const OFFSET_AMOUNT2: usize = 16;
/// `siChageAmount3` inside the 231 body.
const OFFSET_AMOUNT3: usize = 20;

/// Decodes the 230 body.
///
/// # Errors
///
/// [`WireError::Truncated`] if fewer than [`REQUEST_STATUSINFO_SIZE`] bytes are
/// supplied.
pub fn decode_change_status_info(payload: &[u8]) -> Result<ChangeStatusInfo, WireError> {
    let mut r = Reader::new(payload);
    let change_type = r.i32()?;
    let change_switch = r.u8()? != 0;
    r.skip(3)?; // pad to siChageAmount1's 4-byte alignment
    let amount1 = r.i32()?;
    let amount2 = r.i32()?;
    let amount3 = r.i32()?;
    Ok(ChangeStatusInfo {
        change_type,
        change_switch,
        amount1,
        amount2,
        amount3,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 230 body toggling auto-food on, with the three amount slots zero — the
    /// shape both client send sites produce (`NCharHorse.cpp:458-471`).
    fn autofood_on() -> [u8; REQUEST_STATUSINFO_SIZE] {
        let mut body = [0u8; REQUEST_STATUSINFO_SIZE];
        body[0..4].copy_from_slice(&CHANGETYPE_AUTOFOOD.to_le_bytes());
        body[4] = 1;
        body
    }

    #[test]
    fn request_is_twenty_bytes_and_reads_the_change_block() {
        let body = autofood_on();
        assert_eq!(body.len(), REQUEST_STATUSINFO_SIZE);
        assert_eq!(
            decode_change_status_info(&body).unwrap(),
            ChangeStatusInfo {
                change_type: CHANGETYPE_AUTOFOOD,
                change_switch: true,
                amount1: 0,
                amount2: 0,
                amount3: 0,
            }
        );
    }

    #[test]
    fn the_change_type_is_the_first_four_bytes() {
        let mut body = [0u8; REQUEST_STATUSINFO_SIZE];
        body[0..4].copy_from_slice(&CHANGETYPE_FROZEN_PROBE.to_le_bytes());
        let decoded = decode_change_status_info(&body).unwrap();
        assert_eq!(decoded.change_type, CHANGETYPE_FROZEN_PROBE);
        assert!(!decoded.change_switch);
    }

    const CHANGETYPE_FROZEN_PROBE: i32 = 15;

    #[test]
    fn a_truncated_request_is_rejected() {
        let body = autofood_on();
        assert!(matches!(
            decode_change_status_info(&body[..19]),
            Err(WireError::Truncated { .. })
        ));
    }

    #[test]
    fn the_response_addresses_the_character_at_the_expected_offsets() {
        let info = ChangeStatusInfo {
            change_type: CHANGETYPE_AUTOFOOD,
            change_switch: true,
            amount1: 0,
            amount2: 0,
            amount3: 0,
        };
        let body = info.encode_response(0x05BF);
        assert_eq!(body.len(), RESPONSE_STATUSINFO_SIZE);
        assert_eq!(&body[0..2], &0x05BFu16.to_le_bytes());
        assert_eq!(body[2..4], [0u8; 2], "pad to the change block");
        assert_eq!(
            &body[OFFSET_CHANGE_TYPE..OFFSET_CHANGE_TYPE + 4],
            &CHANGETYPE_AUTOFOOD.to_le_bytes()
        );
        assert_eq!(body[OFFSET_CHANGE_SWITCH], 1);
        assert_eq!(body[9..12], [0u8; 3], "pad to siChageAmount1");
        assert_eq!(body[OFFSET_AMOUNT1..], [0u8; 12]);
    }

    #[test]
    fn a_toggle_off_round_trips_through_the_response() {
        let mut request = autofood_on();
        request[4] = 0;
        let decoded = decode_change_status_info(&request).unwrap();
        let body = decoded.encode_response(1);
        // The server echoes the request's switch (`Server-Person.cpp:2851-2852`).
        assert_eq!(body[OFFSET_CHANGE_SWITCH], 0);
    }
}
