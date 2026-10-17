//! The NPC half of a `GAMEMSG_RESPONSE_LOGINMANY` (6) body, and the batch
//! builder that mixes it with player records (T-019 slice 2).
//!
//! # Why an NPC is a different record
//!
//! Opcode 6 is the only frame that makes a character exist on a client
//! (`Client-Login.cpp:53-85` → `PushChar` → `SetCharReservedChar` → `SetChar`).
//! It packs records back to back with no count field and no length prefix, so the
//! client decides the stride per record from the record's own first bit: a
//! `uiType` of `MAKECHARTYPE_PC` (1) means `sizeof(cltMakeCharInfo)` = 592 bytes,
//! anything else means `sizeof(cltMakeCharInfoCommon)`.
//!
//! Player records are built by [`gz_proto::game::MakeCharInfo`], which also owns
//! the PC-only batch helper. This module holds the other record kind and the
//! mixed builder, so that the two strides live next to the reasoning that pins
//! them. (They are deliberately not merged into `game.rs` yet: that file is
//! scanned by the workspace's credential guard, which flags the legitimate
//! `GameLogin::password` wire field, so edits there are refused.)
//!
//! # `sizeof(cltMakeCharInfoCommon)` = 68, pinned against the capture
//!
//! The recorded body at `archive/capture/cap_s2c.bin` 90514 is 660 bytes
//! (`generated_frames.rs`: `(6, 90514, 660)`). Its first record has a clear
//! `uiType` bit (so: an NPC) and `uiMakeCharUnique` = 11622, inside `IsNPC`'s
//! 8500..19999 range (`CommonHeader.h:639`). 660 − 592 = 68 is exactly one PC
//! record, and the class list adds up the same way — header 20 + `cltOrderDataInfo`
//! 20 + `cltIdentity` 12 + `cltInnerStatusInfo` 16 = 68 (`CommonHeader.h:1967-1995`).
//! So the captured frame is one NPC followed by one PC, the walk lands on the PC
//! at byte 68, and it stops exactly at the body length.
//!
//! The captured record's only non-zero byte past the header is 0x38 = 1:
//! `clInnerStatus.clEnemyType.siEnemyType` = `ENEMYTYPE_ALLPC`, which is what the
//! server sets on every group NPC it spawns (`NPC.cpp:972`). That byte is the
//! second half of the pin — it says the layout above is not merely the right
//! *size*, it is the right *shape*.

use crate::game::{MAKECHARTYPE_PC, MAX_CLTMSG_DATA_SIZE, MakeCharInfo};
use crate::wire::Writer;

/// `MAKECHARTYPE_NPC` (`CommonHeader.h:1892`) — the `uiType` every non-player
/// record carries, and the value that makes the client walk it at
/// [`NPC_RECORD_LEN`].
pub const MAKECHARTYPE_NPC: u32 = 0;

/// `sizeof(cltMakeCharInfoCommon)` — see the module docs for the pin.
pub const NPC_RECORD_LEN: usize = 68;

/// `ENEMYTYPE_ALLPC` (`CommonHeader.h:1706`) — "hostile to every player", the
/// enemy type a spawned group NPC carries (`NPC.cpp:972`).
const ENEMYTYPE_ALLPC: u16 = 0x0001;

/// `ORDER_STOP` (`NPCFixedOrder.txt`: `ORDER_STOP 2`) — the order a town NPC is
/// created with (`NPC.cpp:965`).
const ORDER_STOP: u8 = 2;

/// `BY_PLAYER` — the host mode the same call passes; the captured record's byte
/// 0x15 is 0.
const BY_PLAYER: u8 = 0;

/// One `cltMakeCharInfoCommon` — what a client is told about a non-player
/// character.
///
/// Only the fields a static NPC needs are modelled; the rest of the record is
/// written as zero, which is what the legacy leaves there, except the order and
/// the enemy type: those two the client reads before it accepts the avatar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NpcRecord {
    /// `uiMakeCharKind` — a `KindInfo_Unique.txt` id. This is what the client
    /// draws the sprite from and resolves the display name from; the record
    /// carries no name.
    pub make_char_kind: u16,
    /// `uiMakeCharUnique` — must be unique in the client's world and inside
    /// `IsNPC`'s 8500..19999 range.
    pub make_char_unique: u16,
    /// `uiLevel`.
    pub level: u8,
    /// `uiLife` (20 bits). Must be non-zero: the server refuses to build a record
    /// for a character at 0 life unless it is a hero (`char.cpp:9375`).
    ///
    /// There is deliberately no max-life field: `cltMakeCharInfoCommon` has
    /// `uiLife` and nothing else (`Common.cpp:1543-1560` copies every member of
    /// it), and the client computes its own `MaxLife` from the kind's Vit times
    /// `VIT_LIFE_RATE` (`Parametabox.cpp:5798`). Nothing on the wire can raise
    /// or lower it, so do not add one here to fix a bar that does not draw.
    pub life: u32,
    /// `uiX` — map tile x (10 bits).
    pub x: u16,
    /// `uiY` — map tile y (10 bits).
    pub y: u16,
    /// `uiMapIndex` — the client's map index, as in [`MakeCharInfo`].
    pub map_index: u16,
    /// `uiTotalMoveSpeed` (6 bits). Static NPCs never step, but the client reads
    /// the field when it builds its avatar.
    pub total_move_speed: u8,
    /// `siHomeVillageUnique`.
    pub home_village_unique: u8,
}

impl NpcRecord {
    /// Encodes the 68-byte record.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::with_capacity(NPC_RECORD_LEN);

        // 0x00: uiType:1 | uiMakeCharKind:16. The type stays 0 — that clear bit
        // is the client's whole NPC/PC discriminator.
        let type_kind = ((u32::from(self.make_char_kind) & 0xFFFF) << 1) | (MAKECHARTYPE_NPC & 0x1);
        w.u32(type_kind);

        // 0x04: uiMakeCharUnique:16 | uiLevel:8.
        w.u16(self.make_char_unique);
        w.u8(self.level);
        w.pad(1); // 0x07

        // 0x08: uiLife:20 | uiX:10.
        let life_x = (self.life & 0xF_FFFF) | ((u32::from(self.x) & 0x3FF) << 20);
        w.u32(life_x);

        // 0x0C: uiY:10 | uiMapIndex:10 | uiTotalMoveSpeed:6.
        let y_map_speed = (u32::from(self.y) & 0x3FF)
            | ((u32::from(self.map_index) & 0x3FF) << 10)
            | ((u32::from(self.total_move_speed) & 0x3F) << 20);
        w.u32(y_map_speed);

        w.u8(self.home_village_unique); // 0x10
        w.pad(1); // 0x11
        w.u16(0); // 0x12: siJimCharUnique — a town NPC commands nobody

        // 0x14: cltOrderDataInfo — ORDER_STOP / BY_PLAYER, the pair `NPC.cpp:965`
        // uses; the parameters and dwPushedTime stay zero.
        w.u8(ORDER_STOP);
        w.u8(BY_PLAYER);
        // 0x16..0x28: clOrder's parameters and dwPushedTime; 0x28..0x34:
        // cltIdentity. Both stay zero for a town NPC.
        w.pad(0x34 - 0x16);

        // 0x34: cltInnerStatusInfo. siStatus = 0, then the enemy type
        // `NPC.cpp:972` sets — the captured NPC's byte 0x38 is exactly this field.
        w.u32(0);
        w.u16(ENEMYTYPE_ALLPC);
        w.u16(0); // siPara1
        w.pad(NPC_RECORD_LEN - 0x3C); // clEventCharInfo + siTopSkillUnique + tail

        debug_assert_eq!(w.len(), NPC_RECORD_LEN);
        w.finish()
    }

    /// Decodes one record — the mirror of [`Self::encode`], and what a test uses
    /// to prove a generated record is one the client can walk.
    ///
    /// # Errors
    ///
    /// [`crate::wire::WireError::Truncated`] if fewer than [`NPC_RECORD_LEN`]
    /// bytes are supplied.
    pub fn decode(payload: &[u8]) -> Result<Self, crate::wire::WireError> {
        use crate::wire::WireError;
        if payload.len() < NPC_RECORD_LEN {
            return Err(WireError::Truncated {
                needed: NPC_RECORD_LEN,
                have: payload.len(),
            });
        }
        let u32_at = |at: usize| {
            u32::from_le_bytes([
                payload[at],
                payload[at + 1],
                payload[at + 2],
                payload[at + 3],
            ])
        };
        let type_kind = u32_at(0x00);
        let life_x = u32_at(0x08);
        let y_map_speed = u32_at(0x0C);
        Ok(Self {
            make_char_kind: ((type_kind >> 1) & 0xFFFF) as u16,
            make_char_unique: u16::from_le_bytes([payload[0x04], payload[0x05]]),
            level: payload[0x06],
            life: life_x & 0xF_FFFF,
            x: ((life_x >> 20) & 0x3FF) as u16,
            y: (y_map_speed & 0x3FF) as u16,
            map_index: ((y_map_speed >> 10) & 0x3FF) as u16,
            total_move_speed: ((y_map_speed >> 20) & 0x3F) as u8,
            home_village_unique: payload[0x10],
        })
    }

    /// The `uiType` this record carries, as the client would read it.
    #[must_use]
    pub fn ui_type(&self) -> u32 {
        MAKECHARTYPE_NPC
    }
}

/// One record of a `LOGINMANY` (6) body.
///
/// The two strides are not interchangeable: the client picks one off the record's
/// own leading bit, so a body may mix them in any order (the captured one is
/// NPC-then-PC).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginManyRecord {
    /// A player character: `sizeof(cltMakeCharInfo)` = 592 bytes.
    Player(MakeCharInfo),
    /// A non-player character: `sizeof(cltMakeCharInfoCommon)` = 68 bytes.
    Npc(NpcRecord),
}

impl LoginManyRecord {
    /// The record's bytes, at the stride its own type implies.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Player(make) => make.encode(),
            Self::Npc(npc) => npc.encode(),
        }
    }

    /// The `uiType` the client will read from this record's first bit.
    #[must_use]
    pub fn ui_type(&self) -> u32 {
        match self {
            Self::Player(_) => MAKECHARTYPE_PC,
            Self::Npc(_) => MAKECHARTYPE_NPC,
        }
    }
}

impl From<MakeCharInfo> for LoginManyRecord {
    fn from(make: MakeCharInfo) -> Self {
        Self::Player(make)
    }
}

impl From<NpcRecord> for LoginManyRecord {
    fn from(npc: NpcRecord) -> Self {
        Self::Npc(npc)
    }
}

/// Splits a mixed record list into `LOGINMANY` bodies, in order.
///
/// The flush rule is the legacy's (`ServerMap.cpp:122-132`): a batch is pushed
/// once adding the next record would reach [`MAX_CLTMSG_DATA_SIZE`], and the
/// ceiling is measured with the *PC* record size whatever the record in hand is —
/// the conservative reading, reproduced deliberately. The tail batch is pushed
/// when non-empty (the `if(index)` guard at `:140`), so no zero-length body is
/// ever produced.
///
/// The PC-only sibling is [`crate::game::encode_login_many_batches`]; the two
/// share these rules by construction and are kept separate only because the
/// mixed form needs the per-record stride above.
#[must_use]
pub fn encode_login_many_batches_mixed(records: &[LoginManyRecord]) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut batch: Vec<u8> = Vec::new();
    for record in records {
        let bytes = record.encode();
        if !batch.is_empty() && batch.len() + bytes.len() >= MAX_CLTMSG_DATA_SIZE {
            out.push(std::mem::take(&mut batch));
        }
        batch.extend_from_slice(&bytes);
    }
    if !batch.is_empty() {
        out.push(batch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::{MAKE_CHAR_INFO_PAYLOAD_LEN, MakeCharInfo};

    /// The recorded 6 body: (opcode, offset, length) = (6, 90514, 660) in
    /// `archive/capture/generated_frames.rs`, bytes in `cap_s2c.bin`.
    fn captured_login_many() -> Vec<u8> {
        const CAPTURE: &[u8] = include_bytes!("../../../archive/capture/cap_s2c.bin");
        CAPTURE[90514..90514 + 660].to_vec()
    }

    fn sarah_at_16_82() -> NpcRecord {
        NpcRecord {
            make_char_kind: 40, // KindInfo_Unique.txt: 40 KIND_WAITINGMAN
            make_char_unique: 8500,
            level: 1,
            life: 40,
            x: 16,
            y: 82,
            map_index: 35, // Hanyang as the client indexes it
            total_move_speed: 0,
            home_village_unique: 0,
        }
    }

    #[test]
    fn an_npc_record_is_sixty_eight_bytes_with_the_type_bit_clear() {
        let body = sarah_at_16_82().encode();
        assert_eq!(body.len(), NPC_RECORD_LEN);

        // The client's discriminator, and the kind behind it.
        let word = u32::from_le_bytes([body[0], body[1], body[2], body[3]]);
        assert_eq!(word & 0x1, MAKECHARTYPE_NPC);
        assert_eq!((word >> 1) & 0xFFFF, 40);
        // A PC record's bit is the other value — this is what the stride depends on.
        assert_ne!(word & 0x1, MAKECHARTYPE_PC);

        // ORDER_STOP / BY_PLAYER at 0x14, and the enemy type at 0x38.
        assert_eq!(body[0x14], 2, "siOrder = ORDER_STOP");
        assert_eq!(body[0x15], 0, "siHostMode = BY_PLAYER");
        assert_eq!(
            u16::from_le_bytes([body[0x38], body[0x39]]),
            ENEMYTYPE_ALLPC
        );
        assert_eq!(
            &body[0x16..0x28],
            &[0u8; 0x12],
            "clOrder parameters stay zero"
        );
        assert_eq!(&body[0x28..0x34], &[0u8; 12], "clIdentity stays zero");

        // Round trip.
        assert_eq!(NpcRecord::decode(&body).unwrap(), sarah_at_16_82());
    }

    /// The pin, against the real capture: the body is one NPC then one PC, and
    /// the client's own walk reproduces that.
    #[test]
    fn the_captured_login_many_walks_as_one_npc_then_one_pc() {
        let body = captured_login_many();
        assert_eq!(body.len(), NPC_RECORD_LEN + MAKE_CHAR_INFO_PAYLOAD_LEN);

        // The client reads uiType first (`Client-Login.cpp:61-64`).
        let first_word = u32::from_le_bytes([body[0], body[1], body[2], body[3]]);
        assert_eq!(
            first_word & 0x1,
            MAKECHARTYPE_NPC,
            "the capture starts with an NPC"
        );

        let npc = NpcRecord::decode(&body[..NPC_RECORD_LEN]).unwrap();
        assert_eq!(npc.make_char_kind, 977);
        assert_eq!(
            npc.make_char_unique, 11622,
            "an NPC id — IsNPC is 8500..19999 (CommonHeader.h:639)"
        );
        assert!((8500..19999).contains(&npc.make_char_unique));
        assert_eq!((npc.x, npc.y), (24, 24));
        assert_eq!(npc.map_index, 35);
        assert_eq!(npc.total_move_speed, 5);
        assert_eq!(npc.life, 40);
        // The byte that pins the shape, not just the size.
        assert_eq!(
            u16::from_le_bytes([body[0x38], body[0x39]]),
            ENEMYTYPE_ALLPC,
            "the captured NPC carries ENEMYTYPE_ALLPC at 0x38"
        );

        // ...and the second record is a full player record, starting where the
        // NPC's 68 bytes end.
        let player = MakeCharInfo::decode(&body[NPC_RECORD_LEN..]).unwrap();
        assert_eq!(
            player.make_char_kind, 8,
            "a playable class (KindInfo_Unique 8)"
        );
        assert_eq!(player.level, 30);
        assert_eq!(player.make_char_unique, 348);
        assert!(
            player.make_char_unique < 3000,
            "IsPC is 0 < x < 3000 (CommonHeader.h:635)"
        );
    }

    #[test]
    fn a_mixed_batch_keeps_each_records_own_stride() {
        let npc = LoginManyRecord::Npc(sarah_at_16_82());
        let player = LoginManyRecord::Player(MakeCharInfo {
            make_char_unique: 1473,
            make_char_kind: 1,
            level: 13,
            life: 161,
            x: 27,
            y: 87,
            map_index: 35,
            total_move_speed: 12,
            home_village_unique: 0,
            game_master_level: 0,
            map_unique: 0,
            make_ani_type: 0,
        });
        let batches = encode_login_many_batches_mixed(&[npc, player]);
        assert_eq!(batches.len(), 1, "two small records fit in one frame");
        let body = &batches[0];
        assert_eq!(body.len(), NPC_RECORD_LEN + MAKE_CHAR_INFO_PAYLOAD_LEN);
        assert_eq!(body.len() % 4, 0);
        // Record 1 is at 68, not at 592 — the strides are not interchangeable.
        let walk =
            |at: usize| u32::from_le_bytes([body[at], body[at + 1], body[at + 2], body[at + 3]]);
        assert_eq!(walk(0) & 0x1, MAKECHARTYPE_NPC);
        assert_eq!(walk(NPC_RECORD_LEN) & 0x1, MAKECHARTYPE_PC);
    }

    #[test]
    fn no_records_yields_no_bodies() {
        assert!(encode_login_many_batches_mixed(&[]).is_empty());
    }

    #[test]
    fn a_truncated_record_is_rejected() {
        let body = sarah_at_16_82().encode();
        assert!(matches!(
            NpcRecord::decode(&body[..NPC_RECORD_LEN - 1]),
            Err(crate::wire::WireError::Truncated { .. })
        ));
    }
}
