//! The order pair: `GAMEMSG_REQUEST_USERORDER` (240) →
//! `GAMEMSG_RESPONSE_USERORDER` (241) (`MsgType.h`), plus the
//! `GAMEMSG_RESPONSE_CHARUPDATE` (200) broadcast that follows a move.
//!
//! Both 240 and 241 carry the same `cltOrderData` (`CommonHeader.h:1519-1530`):
//! a `cltOrderDataInfo ODI` (`:1495-1515`) followed by the subject's
//! `SI32 CharUnique`. The client sends exactly that at `Client.cpp:3299`
//! (`sizeof(cltOrderData)`), decodes it in `Client-SystemNPC.cpp:538`, and the
//! server answers with it from `char-order.cpp:169` / `char.cpp:1419,1572`.
//!
//! # Packing
//!
//! `cltOrderDataInfo` is declared outside the `#pragma pack(1)` region, so it
//! uses MSVC default `/Zp8`. With an `SI32` 4-aligned, the leading two bytes and
//! the three `SI16` are each followed by padding:
//!
//! | offset | size | field |
//! |---|---|---|
//! | `0` | 1 | `siOrder` |
//! | `1` | 1 | `siHostMode` |
//! | `2` | 2 | *(pad)* |
//! | `4` | 4 | `siPara1` |
//! | `8` | 2 | `siPara2` |
//! | `10` | 2 | `siPara3` |
//! | `12` | 2 | `siPara4` |
//! | `14` | 2 | *(pad)* |
//! | `16` | 4 | `dwPushedTime` |
//! | `20` | 4 | `CharUnique` |
//!
//! so the body is **24 bytes**. The live session confirms it: the captured 241
//! body is
//! `04 00 00 00 17 00 0E 00 00 00 00 00 00 00 00 00 00 00 00 00 BF 05 00 00`,
//! i.e. `ORDER_MOVE`, `BY_PLAYER`, `siPara1 = 0x000E0017` (x=23, y=14),
//! `siPara2 = 0` and `CharUnique = 1471`.
//!
//! # `siPara1` is the move target
//!
//! `cltOrder::MakeOrder` packs `x | (y << 16)` (`order.cpp:148`) and the
//! `ORDER_MOVE` handler unpackages it with `LOWORD`/`HIWORD`
//! (`char-order.cpp:139-140`). `MakeOrder_MOVE` writes the same
//! `x | (y << 16)` and zeroes the rest (`order.cpp:257-262`).
//!
//! `siPara2` is **not** a coordinate: it is a target's character unique
//! (`cltOrder::MakeOrder:153-156` fills it from `CR[para2]->GetCharUnique()`;
//! `LetOrder` maps it back with `GetIDFromCharUnique`, `CommonLogic-order.cpp:30`).
//! For a move it is 0.

use crate::wire::{Reader, WireError};

/// `ORDER_MOVE` (`CommonHeader.h:174`).
pub const ORDER_MOVE: i8 = 4;

/// `ORDER_ATTACK` (`CommonHeader.h:521`) — a plain swing at the target whose
/// char unique rides in `siPara2` (`MakeOrder_ATTACK`, `order.cpp:314-321`).
///
/// A monster's attack goes out this way *and* as the 268 hit response: the 241
/// is what makes a client play the swing. It is **not** what raises the
/// overhead health bar — that is [`crate::game::encode_jim`], whose frame marks
/// the monster as fought-by a player (`Char-Draw.cpp:2773-2780` gates the bar
/// on `clJim.siJimHostCharUnique ==` the local player). There is no separate
/// "attack animation" opcode.
pub const ORDER_ATTACK: i8 = 5;

/// `ORDER_DIE` (`CommonHeader.h:523`) — the death order, broadcast after a
/// character's life reaches 0 (`char.cpp:9444-9445`,
/// `char-order.cpp:533`).
pub const ORDER_DIE: i8 = 7;

/// `ORDER_SITDOWN` (`CommonHeader.h:533`) — the sit leg of the 7231 toggle.
pub const ORDER_SITDOWN: i8 = 17;

/// `ORDER_FORCESTOP` (`CommonHeader.h:519`) — the stand leg of the 7231 toggle.
pub const ORDER_FORCESTOP: i8 = 3;

/// `BY_PLAYER` (`CommonHeader.h:660`).
pub const BY_PLAYER: i8 = 0;

/// `sizeof(cltGameMsgRequest_SitDown)` (`MsgType-Person.h:1418-1428`):
/// `bool bSitDown; SI32 siDirection;` under the default `/Zp8`, so the bool is
/// padded out to the 4-aligned `SI32` — 8 bytes.
pub const SITDOWN_REQUEST_LEN: usize = 8;

/// `sizeof(cltOrderData)`: 20-byte `cltOrderDataInfo` + 4-byte `CharUnique`.
pub const ORDER_DATA_LEN: usize = 24;

/// `sizeof(cltBasicCharUpdateInfo)` (`CommonLogic.h:383-410`): 23 meaningful
/// bytes plus one trailing pad byte the constructor never writes.
pub const BASIC_CHAR_UPDATE_LEN: usize = 24;

/// `cltOrderData` — shared by the 240 request and the 241 response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderData {
    /// `cltOrderDataInfo.siOrder` (`ORDER_MOVE`, `ORDER_FARM`, …).
    pub si_order: i8,
    /// `cltOrderDataInfo.siHostMode`; `BY_PLAYER` on the wire in both directions.
    pub si_host_mode: i8,
    /// `cltOrderDataInfo.siPara1` — for a move, `x | (y << 16)`.
    pub si_para1: i32,
    /// `siPara2` — a character unique (0 for a move).
    pub si_para2: i16,
    /// `siPara3`.
    pub si_para3: i16,
    /// `siPara4`.
    pub si_para4: i16,
    /// `dwPushedTime`.
    pub dw_pushed_time: u32,
    /// The subject character's unique id.
    pub char_unique: i32,
}

impl OrderData {
    /// Decodes a `cltOrderData` body.
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`] if fewer than [`ORDER_DATA_LEN`] bytes are left.
    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        let mut r = Reader::new(bytes);
        let si_order = r.i8()?;
        let si_host_mode = r.i8()?;
        r.skip(2)?; // pad to the 4-aligned SI32 siPara1
        let si_para1 = r.i32()?;
        let si_para2 = r.i16()?;
        let si_para3 = r.i16()?;
        let si_para4 = r.i16()?;
        r.skip(2)?; // pad to the 4-aligned DWORD dwPushedTime
        let dw_pushed_time = r.u32()?;
        let char_unique = r.i32()?;
        Ok(Self {
            si_order,
            si_host_mode,
            si_para1,
            si_para2,
            si_para3,
            si_para4,
            dw_pushed_time,
            char_unique,
        })
    }

    /// Encodes a `cltOrderData` body.
    #[must_use]
    pub fn encode(&self) -> [u8; ORDER_DATA_LEN] {
        let mut out = [0u8; ORDER_DATA_LEN];
        out[0] = self.si_order as u8;
        out[1] = self.si_host_mode as u8;
        out[4..8].copy_from_slice(&self.si_para1.to_le_bytes());
        out[8..10].copy_from_slice(&self.si_para2.to_le_bytes());
        out[10..12].copy_from_slice(&self.si_para3.to_le_bytes());
        out[12..14].copy_from_slice(&self.si_para4.to_le_bytes());
        out[16..20].copy_from_slice(&self.dw_pushed_time.to_le_bytes());
        out[20..24].copy_from_slice(&self.char_unique.to_le_bytes());
        out
    }

    /// The `(x, y)` packed into `siPara1` — `LOWORD`/`HIWORD`
    /// (`char-order.cpp:139-140`).
    #[must_use]
    pub fn move_target(si_para1: i32) -> (i16, i16) {
        let low = (si_para1 & 0xFFFF) as u16;
        let high = ((si_para1 >> 16) & 0xFFFF) as u16;
        (low as i16, high as i16)
    }

    /// The server's canonical `ORDER_MOVE`, as `MakeOrder_MOVE` builds it
    /// (`order.cpp:257-262`): `x | (y << 16)`, everything else zero, stamped with
    /// the mover's character unique.
    #[must_use]
    pub fn move_order(char_unique: i32, x: i16, y: i16) -> Self {
        let packed = i32::from(x as u16) | (i32::from(y as u16) << 16);
        Self {
            si_order: ORDER_MOVE,
            si_host_mode: BY_PLAYER,
            si_para1: packed,
            si_para2: 0,
            si_para3: 0,
            si_para4: 0,
            dw_pushed_time: 0,
            char_unique,
        }
    }

    /// The server's canonical `ORDER_SITDOWN` (`MakeOrder_SITDOWN`,
    /// `order.cpp:284-290`): `siPara1` carries the 8-way facing the client sent
    /// with the request, and `siPara2` is zero (the legacy puts the cushion item
    /// there, which this port has no items for). The observer's 241
    /// (`char-order.cpp:522-526`, `SendNeighbourMsg(..., true)`).
    #[must_use]
    pub fn sit_order(char_unique: i32, direction: i32) -> Self {
        Self {
            si_order: ORDER_SITDOWN,
            si_host_mode: BY_PLAYER,
            si_para1: direction,
            si_para2: 0,
            si_para3: 0,
            si_para4: 0,
            dw_pushed_time: 0,
            char_unique,
        }
    }

    /// The server's canonical `ORDER_FORCESTOP` (`MakeOrder_FORCESTOP`,
    /// `order.cpp:250-253`) — the stand half of the toggle, everything zero but
    /// the order and the subject (`char-order.cpp:112-119`).
    #[must_use]
    pub fn force_stop_order(char_unique: i32) -> Self {
        Self {
            si_order: ORDER_FORCESTOP,
            si_host_mode: BY_PLAYER,
            si_para1: 0,
            si_para2: 0,
            si_para3: 0,
            si_para4: 0,
            dw_pushed_time: 0,
            char_unique,
        }
    }

    /// The server's canonical `ORDER_ATTACK` (`MakeOrder_ATTACK`,
    /// `order.cpp:314-321`): `siPara1` and `siPara3` are 0, `siPara2` is the
    /// *target's* character unique, and `siPara4` is the reserve-order slot the
    /// legacy passes through (`char-order.cpp:247`) — 0 when nothing is
    /// reserved.
    ///
    /// A client applies this in `DoMsg_GAMEMSG_RESPONSE_USERORDER`
    /// (`Client-SystemNPC.cpp:538`), which is what puts the char into an attack
    /// order. That drives the *swing animation*; it does not raise the overhead
    /// health bar. The bar hangs off `clJim.siJimHostCharUnique`
    /// (`Char-Draw.cpp:2773-2780`), set by the separate 263 `ResponseJim`
    /// (`encode_jim`). The `switch` over `1/2/3/5/9` in that function's caller
    /// is `clIdentity.siIdentity` — `IDENTITY_CASTLE`/`VILLAGEGOD`/
    /// `VILLAGEGOD_SUMMON`/`VILLAGEHUNT`/`MOFUMOFUEVENT`
    /// (`CommonHeader.h:475-483`), not an order and not a `CHARWORKSTEP`.
    #[must_use]
    pub fn attack_order(char_unique: i32, target_unique: i16) -> Self {
        Self {
            si_order: ORDER_ATTACK,
            si_host_mode: BY_PLAYER,
            si_para1: 0,
            si_para2: target_unique,
            si_para3: 0,
            si_para4: 0,
            dw_pushed_time: 0,
            char_unique,
        }
    }

    /// The server's canonical `ORDER_DIE` (`char.cpp:9445`, `clOrder.Make(
    /// ORDER_DIE, BY_PLAYER, 0, 0, 0, 0)`): the order and the dying subject,
    /// everything else zero.
    #[must_use]
    pub fn die_order(char_unique: i32) -> Self {
        Self {
            si_order: ORDER_DIE,
            si_host_mode: BY_PLAYER,
            si_para1: 0,
            si_para2: 0,
            si_para3: 0,
            si_para4: 0,
            dw_pushed_time: 0,
            char_unique,
        }
    }
}

/// `cltGameMsgRequest_SitDown` (`MsgType-Person.h:1418-1428`) — opcode 7231's
/// body, the client's sit/stand toggle.
///
/// The client computes the 8-way facing from the click (`MouseOperation.cpp:4127`,
/// `DecideDirection8`) and sends `GAMEMSG_REQUEST_SITDOWN` here, **not** a 240
/// `ORDER_SITDOWN`; the 240/241 order is only what the server broadcasts to the
/// neighbours afterwards (`Server-Person.cpp:3297-3324`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SitDown {
    /// `bSitDown`: `true` sits, `false` stands back up.
    pub b_sit_down: bool,
    /// `siDirection`: the 8-way facing (`SOUTH`..`EAST`, `CommonHeader.h:388-394`).
    pub direction: i32,
}

impl SitDown {
    /// Decodes a `cltGameMsgRequest_SitDown` body.
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`] if fewer than [`SITDOWN_REQUEST_LEN`] bytes are
    /// left.
    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        let mut r = Reader::new(bytes);
        let b_sit_down = r.i8()? != 0;
        r.skip(3)?; // pad to the 4-aligned SI32 siDirection
        let direction = r.i32()?;
        Ok(Self {
            b_sit_down,
            direction,
        })
    }
}

/// `cltBasicCharUpdateInfo` (`CommonLogic.h:383-410`) — the 200 broadcast.
///
/// Offsets, with the `SI32` fields 4-aligned:
///
/// | offset | size | field |
/// |---|---|---|
/// | `0` | 2 | `siCharUnique` |
/// | `2` | 1 | `siMoveSpeed` |
/// | `3` | 1 | `bUrgentMode` |
/// | `4` | 4 | `siLife` |
/// | `8` | 4 | `siReservedLife` |
/// | `12` | 4 | `siMana` |
/// | `16` | 2 | `siX` |
/// | `18` | 2 | `siY` |
/// | `20` | 1 | `uiGMMode` |
/// | `21` | 1 | `bPCRoom` |
/// | `22` | 1 | `siMakeAniType` |
/// | `23` | 1 | *(pad)* |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BasicCharUpdate {
    /// `siCharUnique`.
    pub si_char_unique: i16,
    /// `siMoveSpeed`.
    pub si_move_speed: i8,
    /// `bUrgentMode`.
    pub b_urgent_mode: bool,
    /// `siLife`.
    pub si_life: i32,
    /// `siReservedLife`.
    pub si_reserved_life: i32,
    /// `siMana`.
    pub si_mana: i32,
    /// `siX`.
    pub si_x: i16,
    /// `siY`.
    pub si_y: i16,
    /// `uiGMMode`.
    pub ui_gm_mode: u8,
    /// `bPCRoom`.
    pub b_pc_room: bool,
    /// `siMakeAniType`.
    pub si_make_ani_type: i8,
}

impl BasicCharUpdate {
    /// Encodes a `cltBasicCharUpdateInfo` body. The trailing pad byte is left
    /// zero (the legacy ships it uninitialised; the client does not read it).
    #[must_use]
    pub fn encode(&self) -> [u8; BASIC_CHAR_UPDATE_LEN] {
        let mut out = [0u8; BASIC_CHAR_UPDATE_LEN];
        out[0..2].copy_from_slice(&self.si_char_unique.to_le_bytes());
        out[2] = self.si_move_speed as u8;
        out[3] = u8::from(self.b_urgent_mode);
        out[4..8].copy_from_slice(&self.si_life.to_le_bytes());
        out[8..12].copy_from_slice(&self.si_reserved_life.to_le_bytes());
        out[12..16].copy_from_slice(&self.si_mana.to_le_bytes());
        out[16..18].copy_from_slice(&self.si_x.to_le_bytes());
        out[18..20].copy_from_slice(&self.si_y.to_le_bytes());
        out[20] = self.ui_gm_mode;
        out[21] = self.b_pc_room as u8;
        out[22] = self.si_make_ani_type as u8;
        out
    }

    /// Decodes a `cltBasicCharUpdateInfo` body — the mirror of [`Self::encode`],
    /// and the test oracle for the observer fan (T-018 slice 6 B1), which asserts
    /// on the fields a bystander's client actually reads.
    ///
    /// The pad byte at offset 23 is not read: the legacy leaves it uninitialised
    /// and the client does not read it, so a decoded body has no value for it.
    ///
    /// # Errors
    ///
    /// A body shorter than [`BASIC_CHAR_UPDATE_LEN`].
    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        if bytes.len() < BASIC_CHAR_UPDATE_LEN {
            return Err(WireError::Truncated {
                needed: BASIC_CHAR_UPDATE_LEN,
                have: bytes.len(),
            });
        }
        Ok(Self {
            si_char_unique: i16::from_le_bytes([bytes[0], bytes[1]]),
            si_move_speed: bytes[2] as i8,
            b_urgent_mode: bytes[3] != 0,
            si_life: i32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            si_reserved_life: i32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            si_mana: i32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
            si_x: i16::from_le_bytes([bytes[16], bytes[17]]),
            si_y: i16::from_le_bytes([bytes[18], bytes[19]]),
            ui_gm_mode: bytes[20],
            b_pc_room: bytes[21] != 0,
            si_make_ani_type: bytes[22] as i8,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The captured 241 body from `login_capture.pcapng` (the live session's only
    /// move), used as the layout oracle.
    const CAPTURED_241: [u8; ORDER_DATA_LEN] = [
        0x04, 0x00, 0x00, 0x00, 0x17, 0x00, 0x0E, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0xBF, 0x05, 0x00, 0x00,
    ];

    #[test]
    fn captured_241_decodes_to_a_move_to_23_14() {
        let order = OrderData::decode(&CAPTURED_241).unwrap();
        assert_eq!(order.si_order, ORDER_MOVE);
        assert_eq!(order.si_host_mode, BY_PLAYER);
        assert_eq!(order.si_para1, 0x000E_0017);
        assert_eq!(OrderData::move_target(order.si_para1), (23, 14));
        assert_eq!(
            order.si_para2, 0,
            "siPara2 is a char unique, zero for a move"
        );
        assert_eq!(order.si_para3, 0);
        assert_eq!(order.si_para4, 0);
        assert_eq!(order.dw_pushed_time, 0);
        assert_eq!(order.char_unique, 1471);
    }

    #[test]
    fn move_order_round_trips_the_captured_bytes() {
        let rebuilt = OrderData::move_order(1471, 23, 14);
        assert_eq!(rebuilt.encode(), CAPTURED_241);
    }

    #[test]
    fn move_target_unpacks_low_and_high_words() {
        assert_eq!(OrderData::move_target(23 | (14 << 16)), (23, 14));
        assert_eq!(OrderData::move_target(0), (0, 0));
        assert_eq!(
            OrderData::move_target(0x3FF | (0x2FF << 16)),
            (0x3FF, 0x2FF)
        );
    }

    /// The two combat orders, so a broadcast body cannot drift from
    /// `MakeOrder_ATTACK` / `char.cpp:9445`.
    #[test]
    fn the_attack_and_die_orders_carry_their_subject_in_the_legacy_fields() {
        let attack = OrderData::attack_order(9001, 1471);
        assert_eq!(attack.si_order, ORDER_ATTACK);
        assert_eq!(attack.si_host_mode, BY_PLAYER);
        assert_eq!(attack.si_para2, 1471, "siPara2 is the target's char unique");
        assert_eq!((attack.si_para1, attack.si_para3), (0, 0));
        assert_eq!(attack.char_unique, 9001);
        assert_eq!(
            OrderData::decode(&attack.encode()).expect("decodes"),
            attack,
            "round-trips"
        );

        let die = OrderData::die_order(9001);
        assert_eq!(die.si_order, ORDER_DIE);
        assert_eq!(die.char_unique, 9001);
        assert_eq!(
            (die.si_para1, die.si_para2, die.si_para3, die.si_para4),
            (0, 0, 0, 0)
        );
    }

    #[test]
    fn decode_reports_truncation() {
        let short = &CAPTURED_241[..ORDER_DATA_LEN - 1];
        assert_eq!(
            OrderData::decode(short),
            Err(WireError::Truncated { needed: 4, have: 3 })
        );
    }

    #[test]
    fn char_update_is_24_bytes_with_a_trailing_pad_byte() {
        let update = BasicCharUpdate {
            si_char_unique: 1471,
            si_move_speed: 14,
            b_urgent_mode: false,
            si_life: 433,
            si_reserved_life: 0,
            si_mana: 20,
            si_x: 23,
            si_y: 14,
            ui_gm_mode: 0,
            b_pc_room: false,
            si_make_ani_type: 0,
        };
        let bytes = update.encode();
        assert_eq!(bytes.len(), BASIC_CHAR_UPDATE_LEN);
        // The captured second 200, less the uninitialised trailing `0x59`.
        assert_eq!(
            &bytes[..],
            &[
                0xBF, 0x05, 0x0E, 0x00, 0xB1, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x14, 0x00,
                0x00, 0x00, 0x17, 0x00, 0x0E, 0x00, 0x00, 0x00, 0x00, 0x00,
            ]
        );
        // The decoder is the observer fan's oracle (slice 6 B1), so it has to read
        // back every field the encoder wrote — including the signed ones, where a
        // `u8 as i8` cast matters.
        assert_eq!(BasicCharUpdate::decode(&bytes).unwrap(), update);
    }

    /// A negative tile and a negative speed survive the round trip: `si_move_speed`
    /// is `SI08` and `siX`/`siY` are signed, and an observer's client compares
    /// tile deltas with `TABS` (`Client-Person-BigInfo.cpp:1187`), so a mis-decoded
    /// sign would put an avatar on the wrong side of the map.
    #[test]
    fn char_update_round_trips_signed_fields() {
        let update = BasicCharUpdate {
            si_char_unique: -2,
            si_move_speed: -3,
            b_urgent_mode: true,
            si_life: -1,
            si_reserved_life: -2,
            si_mana: -3,
            si_x: -4,
            si_y: -5,
            ui_gm_mode: 0,
            b_pc_room: true,
            si_make_ani_type: -6,
        };
        assert_eq!(BasicCharUpdate::decode(&update.encode()).unwrap(), update);
    }

    #[test]
    fn char_update_decode_reports_truncation() {
        let short = [0u8; BASIC_CHAR_UPDATE_LEN - 1];
        assert_eq!(
            BasicCharUpdate::decode(&short),
            Err(WireError::Truncated {
                needed: BASIC_CHAR_UPDATE_LEN,
                have: BASIC_CHAR_UPDATE_LEN - 1,
            })
        );
    }

    /// The 7231 sit request is `bool bSitDown; SI32 siDirection;` padded to the
    /// 4-aligned SI32 under `/Zp8` (`MsgType-Person.h:1418-1428`), i.e. 8 bytes:
    /// the bool at 0, three pad bytes, the facing at 4.
    #[test]
    fn sit_down_request_decodes_the_padded_bool_and_facing() {
        // bSitDown = true, pad 0 0 0, siDirection = SOUTH|EAST (65, = 0x41).
        let sit = SitDown::decode(&[0x01, 0x00, 0x00, 0x00, 0x41, 0x00, 0x00, 0x00]).unwrap();
        assert!(sit.b_sit_down);
        assert_eq!(sit.direction, 65);

        // bSitDown = false (stand), facing WEST (4).
        let stand = SitDown::decode(&[0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00]).unwrap();
        assert!(!stand.b_sit_down);
        assert_eq!(stand.direction, 4);
    }

    #[test]
    fn sit_down_request_reports_truncation() {
        let short = [0u8; SITDOWN_REQUEST_LEN - 1];
        assert_eq!(
            SitDown::decode(&short),
            Err(WireError::Truncated { needed: 4, have: 3 })
        );
    }

    /// The observer's 241 for a sit carries the facing in `siPara1` and nothing
    /// else; the stand 241 is all zero but the order and the subject
    /// (`order.cpp:252`, `:288`).
    #[test]
    fn sit_and_force_stop_orders_encode_the_legacy_fields() {
        let sit = OrderData::sit_order(1471, 4);
        assert_eq!(sit.si_order, ORDER_SITDOWN);
        assert_eq!(sit.si_host_mode, BY_PLAYER);
        assert_eq!(sit.si_para1, 4);
        assert_eq!(sit.si_para2, 0);
        assert_eq!(sit.char_unique, 1471);
        // Offsets: order@0, host@1, para1@4, char_unique@20.
        let bytes = sit.encode();
        assert_eq!(bytes[0], ORDER_SITDOWN as u8);
        assert_eq!(&bytes[4..8], &4i32.to_le_bytes());
        assert_eq!(&bytes[20..24], &1471i32.to_le_bytes());

        let stand = OrderData::force_stop_order(1471);
        assert_eq!(stand.si_order, ORDER_FORCESTOP);
        assert_eq!(stand.si_para1, 0);
        assert_eq!(stand.char_unique, 1471);
    }
}
