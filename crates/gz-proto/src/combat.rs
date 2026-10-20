//! The combat frame pair: `GAMEMSG_REQUEST_HIT` (267) →
//! `GAMEMSG_RESPONSE_HIT` (268), plus the `GAMEMSG_RESPONSE_GETEXP` (270) that
//! carries what the hit earned.
//!
//! # Packing
//!
//! None of the three structs sits inside a `#pragma pack(1)` region
//! (`MsgType-System.h`, `MsgType-Person.h`), so MSVC default `/Zp8` applies and
//! every `SI32` is 4-aligned.
//!
//! `cltGameMsgRequest_Hit` (`MsgType-System.h:2792`) is **28 bytes**:
//!
//! | offset | size | field |
//! |---|---|---|
//! | `0` | 4 | `siCharID` — the attacker's `CR` id |
//! | `4` | 24 | `clAttackTarget` |
//!
//! `cltAttackTarget` (`CommonHeader.h:3354-3377`) is itself 24 bytes:
//!
//! | offset | size | field |
//! |---|---|---|
//! | `0` | 4 | `siAttackID` — the target's `CR` id |
//! | `4` | 8 | `clPos` — `cltCharPos` (`CommonHeader.h:1096-1120`), four `SI16` |
//! | `12` | 4 | `siHitRate` |
//! | `16` | 4 | `siDodgeRate` |
//! | `20` | 1 | `bPVPHit` |
//! | `21` | 3 | *(pad)* |
//!
//! `cltGameMsgResponse_Hit` (`MsgType-Person.h:444-476`) is **28 bytes**:
//! `SI16 CharUnique` at `0`, `SI32 Damage` `4`, `SI32 DoubleDamage` `8`,
//! `SI32 Life` `12`, `SI32 Mode` `16`, then `UI08 SpecialAttackSwitch` `20`,
//! `bool bIndure` `21`, `bool bMissHit` `22`, `bool bDodgeHit` `23`,
//! `UI08 uiType` `24`, and three trailing pad bytes.
//!
//! `cltGameMsgResponse_GetExp` (`MsgType-Person.h:418-431`) is **12 bytes**:
//! `SI16 siCharUnique` `0`, `SI16 siReason` `2`, `SI32 siExp` `4`,
//! `SI32 siFameExp` `8`. Note the *declared* order is the wire order, not the
//! constructor's argument order.
//!
//! # What a hit response carries
//!
//! The **target's** char unique, its **resulting life total** rather than a
//! delta, and the target's kind as [`HIT_TARGET_MONSTER`] /
//! [`HIT_TARGET_PLAYER`] (`Server-Attack.cpp:2758-2766`). It goes out with
//! `SendNeighbourMsg(..., true)` from the *target*, so every session near the
//! target — attacker included — receives one frame (`:2775`, `:2782`). That is
//! the whole HP-sync mechanism: there is no separate "set HP" packet.

use crate::chat::MAX_PLAYER_NAME;
use crate::wire::{Reader, WireError};

/// `sizeof(cltGameMsgRequest_Hit)` — the 267 body length.
pub const REQUEST_HIT_LEN: usize = 28;

/// `sizeof(cltGameMsgResponse_Hit)` — the 268 body length.
pub const HIT_RESPONSE_LEN: usize = 28;

/// `sizeof(cltGameMsgResponse_GetExp)` — the 270 body length.
pub const GETEXP_RESPONSE_LEN: usize = 12;

/// `sizeof(cltGameMsgResponse_LevelUp)` — the 7221 body length: `SI16` +
/// `SI08` + `SI08`, which cannot pad between a 2-byte and two 1-byte fields.
pub const LEVELUP_RESPONSE_LEN: usize = 4;

/// `sizeof(cltGameMsgResponse_DeathPenalty)` — the 242 body length: 34 bytes
/// of fields, padded to 40 because the leading `GMONEY` aligns the class to 8
/// under MSVC's default `/Zp8`.
pub const DEATH_PENALTY_RESPONSE_LEN: usize = DEATH_PENALTY_FIELDS_LEN + 6;

/// `sizeof(cltGameMsgResponse_Revive)` — the 7226 body length: three `SI16`.
pub const REVIVE_RESPONSE_LEN: usize = 6;

/// `NORMAL` — the `siDieMode` of an ordinary death, and the `siKilledMode` of
/// one that no special war or event map caused (`MsgType-Person.h:1443`,
/// `CharManager.cpp:363`).
pub const DIE_MODE_NORMAL: i16 = 1;

/// `uiType` when the thing that was hit is a monster
/// (`Server-Attack.cpp:2761`, `ATB_MONSTER`).
pub const HIT_TARGET_MONSTER: u8 = 0;

/// `uiType` when the thing that was hit is a player (`:2765`).
pub const HIT_TARGET_PLAYER: u8 = 1;

/// `cltCharPos` (`CommonHeader.h:1096-1120`) — the tile the attack names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CharPos {
    /// `siMapIndex`.
    pub si_map_index: i16,
    /// `siX`.
    pub si_x: i16,
    /// `siY`.
    pub si_y: i16,
    /// `siReserved` — the legacy leaves it 0.
    pub si_reserved: i16,
}

impl CharPos {
    /// Decodes an 8-byte `cltCharPos`.
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`] if fewer than 8 bytes are left.
    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        let mut r = Reader::new(bytes);
        let pos = Self {
            si_map_index: r.i16()?,
            si_x: r.i16()?,
            si_y: r.i16()?,
            si_reserved: r.i16()?,
        };
        Ok(pos)
    }
}

/// `cltAttackTarget` (`CommonHeader.h:3354-3377`) — who is being hit and the
/// odds the client computed for it.
///
/// The hit and dodge rates are the *client's* numbers (`siHitRate`,
/// `siDodgeRate`); the legacy's miss/dodge roll reads them
/// (`Server-Attack.cpp:604-624`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttackTarget {
    /// `siAttackID` — the target's `CR` id. For a field monster that is the
    /// monster's own unique, because the legacy indexes `CR` by it
    /// (`NPC.cpp:972`).
    pub si_attack_id: i32,
    /// `clPos` — the tile the client believes the target is on.
    pub cl_pos: CharPos,
    /// `siHitRate`.
    pub si_hit_rate: i32,
    /// `si_dodge_rate`'s legacy name, `siDodgeRate`.
    pub si_dodge_rate: i32,
    /// `bPVPHit` — the client asking for a PVP hit. Not modelled: this port has
    /// no PVP gate, so the flag is decoded and ignored.
    pub b_pvp_hit: bool,
}

impl AttackTarget {
    /// Decodes a 24-byte `cltAttackTarget`.
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`] if fewer than [`ATTACK_TARGET_LEN`] bytes are
    /// left.
    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        let mut r = Reader::new(bytes);
        let si_attack_id = r.i32()?;
        let cl_pos = CharPos {
            si_map_index: r.i16()?,
            si_x: r.i16()?,
            si_y: r.i16()?,
            si_reserved: r.i16()?,
        };
        let si_hit_rate = r.i32()?;
        let si_dodge_rate = r.i32()?;
        let b_pvp_hit = r.u8()? != 0;
        r.skip(3)?; // pad out to the struct's 4-aligned 24 bytes
        Ok(Self {
            si_attack_id,
            cl_pos,
            si_hit_rate,
            si_dodge_rate,
            b_pvp_hit,
        })
    }
}

/// `sizeof(cltAttackTarget)` — its 24 bytes inside the 267 body.
pub const ATTACK_TARGET_LEN: usize = 24;

/// `cltGameMsgRequest_Hit` (`MsgType-System.h:2792`) — the 267 body.
///
/// The client sends one of these per swing; the server answers with the 268
/// broadcast in [`HitResponse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestHit {
    /// `siCharID` — the attacker's `CR` id.
    pub si_char_id: i32,
    /// `clAttackTarget`.
    pub cl_attack_target: AttackTarget,
}

impl RequestHit {
    /// Decodes a `cltGameMsgRequest_Hit` body.
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`] if fewer than [`REQUEST_HIT_LEN`] bytes are
    /// left.
    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        let mut r = Reader::new(bytes);
        let si_char_id = r.i32()?;
        let cl_attack_target = AttackTarget {
            si_attack_id: r.i32()?,
            cl_pos: CharPos {
                si_map_index: r.i16()?,
                si_x: r.i16()?,
                si_y: r.i16()?,
                si_reserved: r.i16()?,
            },
            si_hit_rate: r.i32()?,
            si_dodge_rate: r.i32()?,
            b_pvp_hit: r.u8()? != 0,
        };
        r.skip(3)?; // the trailing pad inside clAttackTarget
        Ok(Self {
            si_char_id,
            cl_attack_target,
        })
    }
}

/// `cltGameMsgResponse_Hit` (`MsgType-Person.h:444-476`) — the 268 body.
///
/// `Life` is the **resulting total**, not the delta, which is what keeps a
/// client's HP bar from drifting (`Server-Attack.cpp:2773`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HitResponse {
    /// `CharUnique` — the *target's* unique.
    pub char_unique: i16,
    /// `Damage` — the damage actually applied.
    pub damage: i32,
    /// `DoubleDamage` — the extra roll on a critical; 0 otherwise (`:2779`).
    pub double_damage: i32,
    /// `Life` — the target's life after the hit.
    pub life: i32,
    /// `Mode` — the target's hit mode (`siHitMode`). Not modelled: this port has
    /// no mode state, so it is sent as 0.
    pub mode: i32,
    /// `SpecialAttackSwitch` — 0 ordinary, 1 critical, 2 double (`:2770`).
    pub special_attack_switch: u8,
    /// `bIndure` — the indure (super-armour) flag.
    pub b_indure: bool,
    /// `bMissHit` — the miss half of the odds roll (`:610`, `:649`).
    pub b_miss_hit: bool,
    /// `b_dodge_hit`'s legacy name, `bDodgeHit` (`:619`).
    pub b_dodge_hit: bool,
    /// `uiType` — [`HIT_TARGET_MONSTER`] or [`HIT_TARGET_PLAYER`].
    pub ui_type: u8,
}

impl HitResponse {
    /// Encodes a `cltGameMsgResponse_Hit` body. The three trailing pad bytes
    /// are left zero (the legacy ships them uninitialised; the client does not
    /// read them).
    #[must_use]
    pub fn encode(&self) -> [u8; HIT_RESPONSE_LEN] {
        let mut out = [0u8; HIT_RESPONSE_LEN];
        out[0..2].copy_from_slice(&self.char_unique.to_le_bytes());
        out[4..8].copy_from_slice(&self.damage.to_le_bytes());
        out[8..12].copy_from_slice(&self.double_damage.to_le_bytes());
        out[12..16].copy_from_slice(&self.life.to_le_bytes());
        out[16..20].copy_from_slice(&self.mode.to_le_bytes());
        out[20] = self.special_attack_switch;
        out[21] = u8::from(self.b_indure);
        out[22] = u8::from(self.b_miss_hit);
        out[23] = u8::from(self.b_dodge_hit);
        out[24] = self.ui_type;
        out
    }
}

/// `cltGameMsgResponse_GetExp` (`MsgType-Person.h:418-431`) — the 270 body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GetExpResponse {
    /// `siCharUnique` — who earned the experience.
    pub si_char_unique: i16,
    /// `siReason` — the reason code. 0 is the plain combat grant
    /// (`Server-Attack.cpp:4105`).
    pub si_reason: i16,
    /// `siExp` — the amount. The legacy sends it **negated** on the frame that
    /// spends it and positive on a grant (`:4105`); a grant is positive.
    pub si_exp: i32,
    /// `siFameExp` — fame experience. Not modelled, sent as 0.
    pub si_fame_exp: i32,
}

impl GetExpResponse {
    /// Encodes a `cltGameMsgResponse_GetExp` body.
    #[must_use]
    pub fn encode(&self) -> [u8; GETEXP_RESPONSE_LEN] {
        let mut out = [0u8; GETEXP_RESPONSE_LEN];
        out[0..2].copy_from_slice(&self.si_char_unique.to_le_bytes());
        out[2..4].copy_from_slice(&self.si_reason.to_le_bytes());
        out[4..8].copy_from_slice(&self.si_exp.to_le_bytes());
        out[8..12].copy_from_slice(&self.si_fame_exp.to_le_bytes());
        out
    }
}

/// `cltGameMsgResponse_LevelUp` (`MsgType.h:3032-3044`) — the 7221 body, the
/// level-up announcement `LevelUpAction` fans to the neighbourhood including
/// the leveler (`char.cpp:5943-5945`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LevelUpResponse {
    /// `siCharUnique` — who leveled.
    pub si_char_unique: i16,
    /// `siLevel` — the level reached.
    pub si_level: i8,
    /// `siFameLevel` — fame level, not modelled; 0, which is also what the
    /// legacy constructor passes (`char.cpp:5943`).
    pub si_fame_level: i8,
}

impl LevelUpResponse {
    /// Encodes a `cltGameMsgResponse_LevelUp` body.
    #[must_use]
    pub fn encode(&self) -> [u8; LEVELUP_RESPONSE_LEN] {
        let mut out = [0u8; LEVELUP_RESPONSE_LEN];
        out[0..2].copy_from_slice(&self.si_char_unique.to_le_bytes());
        out[2] = self.si_level as u8;
        out[3] = self.si_fame_level as u8;
        out
    }
}

/// Bytes of [`DeathPenaltyResponse`] that are actually **fields**: `GMONEY`
/// `8` + `SI32` `4` + `SI16` `2` + `szKillMeCharName` `20` = 34.
const DEATH_PENALTY_FIELDS_LEN: usize = 34;

/// `cltGameMsgResponse_DeathPenalty` (`MsgType-System.h:1063-1080`) — the 242
/// (`GAMEMSG_RESPONSE_DEALTHPENALTYINFO`) body, and the frame that **opens the
/// client's death dialog**: its handler sets the death dialog's contents from
/// it (`DoMsg_GAMEMSG_RESPONSE_DEALTHPENALTYINFO` → `NDEATH_DLG`,
/// `Client-SystemNPC.cpp:2182-2190`).
///
/// The legacy sends it from `cltCharServer::DeathPenalty()` immediately after a
/// player is killed, paired with `RequestRevive(id, NORMAL)` in the same
/// branch (`CharManager.cpp:362-363`). Together they are the whole reason a
/// dead player has a revive prompt at all — a server that sends neither leaves
/// the body lying on the ground forever, which is exactly what a live run of
/// Test12 against the raccoon reported.
///
/// The penalty **amounts** are not modelled: a monster kill deducts nothing in
/// this port, so `si_money` and `si_exp` go out as 0 and the dialog shows an
/// empty loss.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeathPenaltyResponse {
    /// `GMONEY siMoney` — the money the death took.
    pub si_money: i64,
    /// `SI32 siExp` — the experience the death took.
    pub si_exp: i32,
    /// `SI16 siKilledMode` — a `KILLEDMODE_*` reason code, 0 for an ordinary
    /// field death (the codes run 1..11, `Char-Server.h:25-35`, and
    /// `siKilledMode` is reset to 0 on revive at `char.cpp:5099`).
    pub si_killed_mode: i16,
    /// `TCHAR szKillMeCharName[MAX_PLAYER_NAME]` — who landed the killing blow.
    pub sz_kill_me_char_name: String,
}

impl DeathPenaltyResponse {
    /// Encodes a `cltGameMsgResponse_DeathPenalty` body.
    ///
    /// The struct is 34 bytes of fields followed by 6 bytes of tail padding:
    /// its `GMONEY` member aligns the class to 8, so `sizeof(clInfo)` — the
    /// length the legacy puts on the frame at `char.cpp:4866` — is 40 under
    /// MSVC's default `/Zp8`. The tail goes out zeroed; no reader touches it,
    /// and matching it keeps the frame the length the shipped client expects.
    #[must_use]
    pub fn encode(&self) -> [u8; DEATH_PENALTY_RESPONSE_LEN] {
        let mut out = [0u8; DEATH_PENALTY_RESPONSE_LEN];
        out[0..8].copy_from_slice(&self.si_money.to_le_bytes());
        out[8..12].copy_from_slice(&self.si_exp.to_le_bytes());
        out[12..14].copy_from_slice(&self.si_killed_mode.to_le_bytes());
        // `MStrCpy(..., MAX_PLAYER_NAME)` followed by an explicit NUL in the
        // last byte (`MsgType-System.h:1075-1076`) — 19 characters at most,
        // and the rest is already zero from the array above.
        let name = self.sz_kill_me_char_name.as_bytes();
        let n = name.len().min(MAX_PLAYER_NAME - 1);
        out[14..14 + n].copy_from_slice(&name[..n]);
        out
    }
}

/// `cltGameMsgResponse_Revive` (`MsgType-Person.h:1461-1473`) — the 7226
/// (`GAMEMSG_RESPONSE_REVIVE`) body.
///
/// It answers `GAMEMSG_REQUEST_REVIVE` (7225) but is **also** sent unprompted
/// at the moment of death, in the same `RequestRevive(id, NORMAL)` call
/// (`CharManager.cpp:363`, `char.cpp:4872-4885`). Its handler only flips
/// `bReviveRequestSwitch = true` (`Client-SystemNPC.cpp:2196`), and that flag
/// is what every revive control tests before it will do anything
/// (`NCommonMsgDlg.cpp:172`, `NMenuDlg.cpp:843`, `NStatusBarDlg.cpp:854`,
/// `ToolBarDlg.cpp:467`). So without this frame the death dialog may be on
/// screen while its revive button stays inert — the second half of "the player
/// stays dead indefinitely".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReviveResponse {
    /// `siCharUnique` — the character that died.
    pub si_char_unique: i16,
    /// `siParam` — the legacy always sends `1`.
    pub si_param: i16,
    /// `siDieMode` — [`DIE_MODE_NORMAL`].
    pub si_die_mode: i16,
}

impl ReviveResponse {
    /// Encodes a `cltGameMsgResponse_Revive` body.
    #[must_use]
    pub fn encode(&self) -> [u8; REVIVE_RESPONSE_LEN] {
        let mut out = [0u8; REVIVE_RESPONSE_LEN];
        out[0..2].copy_from_slice(&self.si_char_unique.to_le_bytes());
        out[2..4].copy_from_slice(&self.si_param.to_le_bytes());
        out[4..6].copy_from_slice(&self.si_die_mode.to_le_bytes());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hand-built 267 body: attacker id 1471, target id 9001 at (246, 332) on
    /// map index 0, hit rate 80, dodge rate 20, no PVP.
    fn request_hit_body() -> [u8; REQUEST_HIT_LEN] {
        let mut out = [0u8; REQUEST_HIT_LEN];
        out[0..4].copy_from_slice(&1471i32.to_le_bytes());
        out[4..8].copy_from_slice(&9001i32.to_le_bytes());
        out[8..10].copy_from_slice(&0i16.to_le_bytes()); // siMapIndex
        out[10..12].copy_from_slice(&246i16.to_le_bytes());
        out[12..14].copy_from_slice(&332i16.to_le_bytes());
        out[16..20].copy_from_slice(&80i32.to_le_bytes());
        out[20..24].copy_from_slice(&20i32.to_le_bytes());
        out[24] = 0; // bPVPHit
        out
    }

    #[test]
    fn request_hit_decodes_the_attacker_and_its_target() {
        let hit = RequestHit::decode(&request_hit_body()).expect("decodes");
        assert_eq!(hit.si_char_id, 1471);
        assert_eq!(hit.cl_attack_target.si_attack_id, 9001);
        assert_eq!(hit.cl_attack_target.cl_pos.si_map_index, 0);
        assert_eq!(hit.cl_attack_target.cl_pos.si_x, 246);
        assert_eq!(hit.cl_attack_target.cl_pos.si_y, 332);
        assert_eq!(hit.cl_attack_target.cl_pos.si_reserved, 0);
        assert_eq!(hit.cl_attack_target.si_hit_rate, 80);
        assert_eq!(hit.cl_attack_target.si_dodge_rate, 20);
        assert!(!hit.cl_attack_target.b_pvp_hit);
    }

    #[test]
    fn request_hit_reads_the_pad_byte_as_the_pvp_flag() {
        let mut body = request_hit_body();
        body[24] = 1;
        assert!(
            RequestHit::decode(&body)
                .expect("decodes")
                .cl_attack_target
                .b_pvp_hit
        );
        // The three bytes after it are padding and must not shift anything.
        body[25] = 0xFF;
        body[26] = 0xFF;
        body[27] = 0xFF;
        assert_eq!(
            RequestHit::decode(&body).expect("decodes").cl_attack_target,
            AttackTarget {
                si_attack_id: 9001,
                cl_pos: CharPos {
                    si_map_index: 0,
                    si_x: 246,
                    si_y: 332,
                    si_reserved: 0,
                },
                si_hit_rate: 80,
                si_dodge_rate: 20,
                b_pvp_hit: true,
            }
        );
    }

    #[test]
    fn request_hit_reports_truncation() {
        let short = &request_hit_body()[..REQUEST_HIT_LEN - 1];
        assert!(matches!(
            RequestHit::decode(short),
            Err(WireError::Truncated { .. })
        ));
    }

    #[test]
    fn hit_response_places_every_field_on_its_documented_offset() {
        let body = HitResponse {
            char_unique: 9001,
            damage: 37,
            double_damage: 0,
            life: 3,
            mode: 0,
            special_attack_switch: 0,
            b_indure: false,
            b_miss_hit: false,
            b_dodge_hit: false,
            ui_type: HIT_TARGET_MONSTER,
        }
        .encode();

        assert_eq!(body.len(), HIT_RESPONSE_LEN);
        assert_eq!(i16::from_le_bytes([body[0], body[1]]), 9001);
        assert_eq!(&body[2..4], &[0, 0], "the pad after CharUnique stays clear");
        assert_eq!(i32::from_le_bytes(body[4..8].try_into().unwrap()), 37);
        assert_eq!(i32::from_le_bytes(body[8..12].try_into().unwrap()), 0);
        assert_eq!(i32::from_le_bytes(body[12..16].try_into().unwrap()), 3);
        assert_eq!(i32::from_le_bytes(body[16..20].try_into().unwrap()), 0);
        assert_eq!(body[20], 0);
        assert_eq!(body[21], 0);
        assert_eq!(body[22], 0);
        assert_eq!(body[23], 0);
        assert_eq!(body[24], HIT_TARGET_MONSTER);
        assert_eq!(&body[25..], &[0, 0, 0], "the trailing pad is zero");
    }

    #[test]
    fn hit_response_flags_reach_the_bytes_the_client_reads() {
        let body = HitResponse {
            char_unique: 1471,
            damage: 0,
            double_damage: 41,
            life: 0,
            mode: 2,
            special_attack_switch: 2,
            b_indure: true,
            b_miss_hit: true,
            b_dodge_hit: true,
            ui_type: HIT_TARGET_PLAYER,
        }
        .encode();
        assert_eq!(i32::from_le_bytes(body[8..12].try_into().unwrap()), 41);
        assert_eq!(i32::from_le_bytes(body[16..20].try_into().unwrap()), 2);
        assert_eq!(body[20], 2);
        assert_eq!(body[21], 1);
        assert_eq!(body[22], 1);
        assert_eq!(body[23], 1);
        assert_eq!(body[24], HIT_TARGET_PLAYER);
    }

    #[test]
    fn getexp_is_twelve_bytes_in_declared_field_order() {
        let body = GetExpResponse {
            si_char_unique: 1471,
            si_reason: 0,
            si_exp: 120,
            si_fame_exp: 0,
        }
        .encode();
        assert_eq!(body.len(), GETEXP_RESPONSE_LEN);
        assert_eq!(i16::from_le_bytes([body[0], body[1]]), 1471);
        assert_eq!(i16::from_le_bytes([body[2], body[3]]), 0);
        assert_eq!(i32::from_le_bytes(body[4..8].try_into().unwrap()), 120);
        assert_eq!(i32::from_le_bytes(body[8..12].try_into().unwrap()), 0);
    }

    #[test]
    fn levelup_is_four_bytes_in_declared_field_order() {
        let body = LevelUpResponse {
            si_char_unique: -3,
            si_level: 14,
            si_fame_level: 0,
        }
        .encode();
        assert_eq!(body.len(), LEVELUP_RESPONSE_LEN);
        assert_eq!(i16::from_le_bytes([body[0], body[1]]), -3);
        assert_eq!(body[2], 14, "siLevel follows siCharUnique at offset 2");
        assert_eq!(body[3], 0);
    }

    /// `GMONEY` leads the struct, so the class aligns to 8 and `sizeof` is 40:
    /// the fields end at 34 and the last 6 bytes are the `/Zp8` tail the legacy
    /// puts on the frame (`char.cpp:4866`).
    #[test]
    fn death_penalty_is_thirty_four_bytes_of_fields_padded_to_forty() {
        let body = DeathPenaltyResponse {
            si_money: 12_345_678_901_234,
            si_exp: -700,
            si_killed_mode: 0,
            sz_kill_me_char_name: "Raccoon".to_owned(),
        }
        .encode();
        assert_eq!(body.len(), DEATH_PENALTY_RESPONSE_LEN);
        assert_eq!(
            i64::from_le_bytes(body[0..8].try_into().unwrap()),
            12_345_678_901_234,
            "GMONEY is a full 64-bit field, not a padded 32-bit one"
        );
        assert_eq!(i32::from_le_bytes(body[8..12].try_into().unwrap()), -700);
        assert_eq!(i16::from_le_bytes([body[12], body[13]]), 0);
        assert_eq!(
            &body[14..21],
            b"Raccoon",
            "szKillMeCharName follows the SI16 at 12"
        );
        assert_eq!(body[21], 0, "NUL-terminated inside its 20-byte slot");
        assert!(
            body[DEATH_PENALTY_FIELDS_LEN..].iter().all(|b| *b == 0),
            "the tail is padding and stays zero"
        );
    }

    /// `MStrCpy(..., MAX_PLAYER_NAME)` copies 19 characters and the next line
    /// forces a NUL into the last byte (`MsgType-System.h:1075-1076`), so an
    /// over-long killer name can never run out of its slot into the padding.
    #[test]
    fn a_long_killer_name_stays_inside_its_slot() {
        let body = DeathPenaltyResponse {
            si_money: 0,
            si_exp: 0,
            si_killed_mode: 0,
            sz_kill_me_char_name: "A".repeat(40),
        }
        .encode();
        assert_eq!(&body[14..33], &[b'A'; 19], "19 characters at most");
        assert_eq!(body[33], 0, "the last byte of the name slot is a NUL");
    }

    #[test]
    fn revive_is_three_si16_in_declared_order() {
        let body = ReviveResponse {
            si_char_unique: 1471,
            si_param: 1,
            si_die_mode: DIE_MODE_NORMAL,
        }
        .encode();
        assert_eq!(body.len(), REVIVE_RESPONSE_LEN);
        assert_eq!(i16::from_le_bytes([body[0], body[1]]), 1471);
        assert_eq!(i16::from_le_bytes([body[2], body[3]]), 1);
        assert_eq!(
            i16::from_le_bytes([body[4], body[5]]),
            DIE_MODE_NORMAL,
            "PVP_DIE is 2, so 1 can only be the ordinary-death mode"
        );
        assert_eq!(DIE_MODE_NORMAL, 1, "`NORMAL` at MsgType-Person.h:1443");
    }
}
