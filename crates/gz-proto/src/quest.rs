//! Quest state and the quest request family (T-019 slice 1).
//!
//! Covers the three client→server requests — `GAMEMSG_REQUEST_SETQUESTGOING`
//! (5700), `…_CANCELQUEST` (5701), `…_QUESTSTATUS` (5702)
//! (`CommonLogic\MsgType.h:1547-1551`) — and the per-character quest block the
//! server pushes as `GAMEMSG_RESPONSE_PERSON_QUEST` (225).
//!
//! # Why this module exists at all
//!
//! The client refuses to walk onto any gate on the starting map while it
//! believes the newbie chain is unfinished: `MouseOperation.cpp:5500-5511`
//! blocks the move order while `ClearQuest(30005) == false &&
//! ClearQuest(30006) == false && mapindex == MAPINDEX_PALACE && level < 10`,
//! and shows "Cannot leave yet. You must complete the quests given by Sarah in
//! Hanyang before you leave." (`TextMgr_Text_ENG.txt:1091-1092`). We were
//! answering 225 with 1536 zeros, so the client believed *nothing* was cleared
//! and the exit stayed dead. See `docs\rust-rewrite\tasks\T-019-*.md`.
//!
//! # The 225 body — `cltPQuestInfo`
//!
//! `Char-UpdateChangedData.cpp:247-253` sends `sizeof(cltPQuestInfo)` to the
//! affected character only, gated by `UPDATE_QUEST`. `Quest.h:906-933` lays the
//! struct out as ten word-assembly flags, three `SI32`s, four single
//! `cltQuestInfoCommon` blocks (special / naming / happybean / remove-unluck),
//! `cltMissionQuestInfo`, and **last** `cltPersonQuestUnitInfo
//! clUnit[MAX_QUEST_TYPE]`.
//!
//! | part | bytes |
//! |---|---|
//! | unmodelled prefix (commons + mission) | 0..592 |
//! | `clUnit` — one 16-byte unit per quest type | 592..1536 |
//!
//! ## How the layout was pinned (do not re-derive it from the tree)
//!
//! The legacy tree in this workspace is a 2010 snapshot whose `Quest.h` ends at
//! `MAX_QUEST_TYPE = 53` (`Quest.h:113`). The shipped client is wider: the
//! reference session's 225 body is **1536** bytes, which 53 units of 16 bytes
//! cannot fill (that arithmetic gives `clUnit` at 688 and a shorter struct).
//! The tree is therefore not an oracle for this struct, and the port already
//! pins the *length* against the capture (`world_entry.rs`).
//!
//! The capture settles the offset too. In
//! `archive/capture/cap_s2c.bin` at **62666** length 1536
//! (`archive/capture/generated_frames.rs:` `(225, 62666, 1536)`) only 24
//! non-zero `u16` words exist. Six of them are `siQuestState` bitmasks at
//! 16-aligned offsets; reading them through `clUnit at 592` gives
//!
//! | offset | type | group file | mask bits | quests in that group |
//! |---|---|---|---|---|
//! | 608 | 1 `BATTLE1` | `battle1.txt` | 13 | 13 |
//! | 816 | 14 `BEGINNER2` | `beginner2.txt` | 11 | 11 |
//! | 848 | 16 `BASIC1` | `basic.txt` | 10 | 10 |
//! | 880 | 18 `MAKEBEGINNER1` | `makebeginner1.txt` | 15 | 15 |
//! | 944 | 22 `HUNTINGGROUND01` | `huntingGround01.txt` | 6 | 23 (in progress) |
//! | 1184 | 37 `BEGINNER1` | `beginner1.txt` | 15 | 15 |
//!
//! Five of the six are *exactly* full: a random offset would eventually claim
//! more cleared quests than a type owns. The phase is fixed by the unit at 944,
//! whose `siCurQuestUnique` is **2060** with `siCurQuestStep` **20**
//! (`QUEST_STEP_MIDDLE`) — 2060 is defined by exactly one group file,
//! `huntingGround01.txt` = type 22, and reading that unit one word earlier or
//! later would put 63 / 24 / 3 in the unique slot, none of which is a real
//! quest unique anywhere in the client data. So `clUnit` starts at 592 and the
//! client's `MAX_QUEST_TYPE` is **59**.
//!
//! [`PersonQuest::decode`] over that captured body is a test in this module, so
//! the constants cannot silently drift.
//!
//! # What is modelled
//!
//! Only `clUnit`. The 592-byte prefix stays zero, which is byte-for-byte what
//! this service sent before the module existed and remains the honest "nothing
//! special is running" state.
//!
//! # Per-unit layout
//!
//! `cltPersonQuestUnitInfo` (`Quest.h:137-161`) is `SI32` + five `SI16` = 14
//! bytes, padded to **16** by the MSVC 4-byte alignment of `SI32`:
//!
//! | offset | size | field |
//! |---|---|---|
//! | 0 | 4 | `siQuestState` — cleared-quest bitmask |
//! | 4 | 2 | `siCurQuestUnique` |
//! | 6 | 2 | `siCurQuestStep` |
//! | 8 | 2 | `siQuestCmd` — the `QUEST_CLEAR_COND_*` the client must satisfy |
//! | 10 | 2 | `siQuestPara1` |
//! | 12 | 2 | `siQuestPara2` |
//! | 14 | 2 | *(pad)* |

use crate::wire::{Reader, WireError, Writer};

/// `sizeof(cltPQuestInfo)` — the 225 body. Pinned against the capture; see the
/// module docs.
pub const PERSON_QUEST_LEN: usize = 1536;

/// The client's `MAX_QUEST_TYPE`: how many 16-byte units the body holds.
pub const QUEST_TYPES: usize = 59;

/// `sizeof(cltPersonQuestUnitInfo)` after MSVC padding.
pub const QUEST_UNIT_LEN: usize = 16;

/// Where `clUnit` starts inside the 225 body: `PERSON_QUEST_LEN - QUEST_TYPES *
/// QUEST_UNIT_LEN`.
pub const QUEST_UNITS_OFFSET: usize = PERSON_QUEST_LEN - QUEST_TYPES * QUEST_UNIT_LEN;

/// `QUEST_TYPE_BEGINNER2` (`Quest.h:64`) — Sarah Spencer's chain in Hanyang.
pub const QUEST_TYPE_BEGINNER2: usize = 14;

/// `QUEST_STEP_*` (`Quest.h:34-38`). A unit's `siCurQuestStep` is one of these,
/// which is what makes a captured unit recognisable.
pub const QUEST_STEP_GREETING: i16 = 10;
/// See [`QUEST_STEP_GREETING`].
pub const QUEST_STEP_MIDDLE: i16 = 20;
/// See [`QUEST_STEP_GREETING`].
pub const QUEST_STEP_GOING: i16 = 30;
/// See [`QUEST_STEP_GREETING`].
pub const QUEST_STEP_CLEAR: i16 = 40;
/// See [`QUEST_STEP_GREETING`].
pub const QUEST_STEP_NEXTQUEST: i16 = 50;

/// `QUEST_CLEAR_COND_NOTMEAN` (`Quest.h:335`) — no condition.
pub const QUEST_CLEAR_COND_NOTMEAN: i16 = 0;
/// `QUEST_CLEAR_COND_BONUSZERO` (`Quest.h:337`) — every bonus point spent.
pub const QUEST_CLEAR_COND_BONUSZERO: i16 = 1;
/// `QUEST_CLEAR_COND_OPENCHARSTATUS` (`Quest.h:341`) — status window opened.
pub const QUEST_CLEAR_COND_OPENCHARSTATUS: i16 = 5;
/// `QUEST_CLEAR_COND_GOMAP` (`Quest.h:356`) — stood on map `para1`.
pub const QUEST_CLEAR_COND_GOMAP: i16 = 19;
/// `QUEST_CLEAR_COND_EQUIPPOS` (`Quest.h:396`) — item worn in slot `para1`.
pub const QUEST_CLEAR_COND_EQUIPPOS: i16 = 55;

/// One `cltPersonQuestUnitInfo` — the state of a single quest type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QuestUnit {
    /// `siQuestState` — bit `n` set means the type's bit-`n` quest is cleared
    /// (the bit index is the group file's `Bit` column).
    pub state: i32,
    /// `siCurQuestUnique` — the quest in progress, or 0.
    pub cur_unique: i16,
    /// `siCurQuestStep` — one of the `QUEST_STEP_*` values.
    pub cur_step: i16,
    /// `siQuestCmd` — the condition the client has to satisfy.
    pub cmd: i16,
    /// `siQuestPara1`.
    pub para1: i16,
    /// `siQuestPara2`.
    pub para2: i16,
}

impl QuestUnit {
    /// Whether the bit-`bit` quest of this type is cleared.
    ///
    /// The legacy reads it the same way: `SetBit`/`GetBit` (`Quest.h:191-200`)
    /// over `siQuestState`.
    #[must_use]
    pub const fn is_clear(&self, bit: u8) -> bool {
        bit < 32 && (self.state & (1i32 << bit)) != 0
    }

    /// Marks the bit-`bit` quest cleared (idempotent). Bits outside 0..31 are
    /// ignored — `Quest.h` stores the mask in an `SI32`.
    pub fn set_clear(&mut self, bit: u8) {
        if bit < 32 {
            self.state |= 1i32 << bit;
        }
    }

    /// Starts `unique` as the type's in-progress quest, copying the condition
    /// the client must satisfy (`siQuestCmd`/`para1`/`para2`).
    pub fn set_going(&mut self, unique: i16, step: i16, cmd: i16, para1: i16, para2: i16) {
        self.cur_unique = unique;
        self.cur_step = step;
        self.cmd = cmd;
        self.para1 = para1;
        self.para2 = para2;
    }

    /// Ends the in-progress quest, leaving the cleared bits alone — the
    /// `siCurQuestUnique = 0` the legacy writes when a quest is cancelled or
    /// completed (`Server-Person.cpp:1035`, `:1737`).
    pub fn clear_going(&mut self) {
        self.cur_unique = 0;
        self.cur_step = 0;
        self.cmd = 0;
        self.para1 = 0;
        self.para2 = 0;
    }

    /// Encodes the 16-byte unit.
    #[must_use]
    pub fn encode(&self) -> [u8; QUEST_UNIT_LEN] {
        let mut w = Writer::with_capacity(QUEST_UNIT_LEN);
        w.i32(self.state);
        w.i16(self.cur_unique);
        w.i16(self.cur_step);
        w.i16(self.cmd);
        w.i16(self.para1);
        w.i16(self.para2);
        w.pad(2); // tail padding of the SI32-aligned struct
        let bytes = w.finish();
        let mut out = [0u8; QUEST_UNIT_LEN];
        out.copy_from_slice(&bytes);
        out
    }

    /// Decodes one unit.
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`] if fewer than [`QUEST_UNIT_LEN`] bytes are
    /// supplied.
    pub fn decode(payload: &[u8]) -> Result<Self, WireError> {
        let mut r = Reader::new(payload);
        let state = r.i32()?;
        let cur_unique = r.i16()?;
        let cur_step = r.i16()?;
        let cmd = r.i16()?;
        let para1 = r.i16()?;
        let para2 = r.i16()?;
        r.skip(2)?; // tail padding
        Ok(Self {
            state,
            cur_unique,
            cur_step,
            cmd,
            para1,
            para2,
        })
    }
}

/// The whole 225 body: `clUnit` for every quest type, plus the zero prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonQuest {
    units: [QuestUnit; QUEST_TYPES],
}

impl Default for PersonQuest {
    /// "Nothing cleared, nothing running" — the state this service sent before
    /// this module existed.
    fn default() -> Self {
        Self {
            units: [QuestUnit::default(); QUEST_TYPES],
        }
    }
}

impl PersonQuest {
    /// An empty quest block.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The unit for `quest_type`, or `None` when the type is out of range —
    /// the same bound the legacy checks (`questtype < 0 || >= MAX_QUEST_TYPE`,
    /// `Server-Person.cpp:1109`).
    #[must_use]
    pub fn unit(&self, quest_type: i16) -> Option<&QuestUnit> {
        usize::try_from(quest_type)
            .ok()
            .and_then(|t| self.units.get(t))
    }

    /// Mutable [`Self::unit`].
    pub fn unit_mut(&mut self, quest_type: i16) -> Option<&mut QuestUnit> {
        usize::try_from(quest_type)
            .ok()
            .and_then(|t| self.units.get_mut(t))
    }

    /// Encodes the 1536-byte body: 592 zero bytes, then the units in type order.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![0u8; QUEST_UNITS_OFFSET];
        for unit in &self.units {
            out.extend_from_slice(&unit.encode());
        }
        debug_assert_eq!(out.len(), PERSON_QUEST_LEN);
        out
    }

    /// Decodes a body.
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`] if fewer than [`PERSON_QUEST_LEN`] bytes are
    /// supplied. A longer body is accepted (the extra bytes are ignored) so a
    /// future client that widens the struct fails loudly in tests rather than
    /// silently here.
    pub fn decode(payload: &[u8]) -> Result<Self, WireError> {
        let mut r = Reader::new(payload);
        r.skip(QUEST_UNITS_OFFSET)?;
        let mut units = [QuestUnit::default(); QUEST_TYPES];
        for unit in &mut units {
            *unit = QuestUnit::decode(r.bytes(QUEST_UNIT_LEN)?)?;
        }
        Ok(Self { units })
    }
}

/// `sizeof(cltGameMsgRequest_QuestGoing)` (`MsgType-Quest.h:44-58`): `SI16`
/// quest type, unique, step, selected-reward index.
pub const REQUEST_QUESTGOING_SIZE: usize = 8;

/// The 5700 body — "start / advance this quest".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuestGoing {
    /// `siQuestType`.
    pub quest_type: i16,
    /// `siQuestUnique`.
    pub quest_unique: i16,
    /// `siQuestStep` — the step the client wants; `QUEST_STEP_CLEAR` asks for
    /// the completion check (`Server-Person.cpp:1128`).
    pub quest_step: i16,
    /// `siSelectItemIndex` — which reward the player picked when the quest
    /// offers a choice (`siSelectItemIndex`, `:1227`).
    pub select_item_index: i16,
}

/// Decodes the 5700 body.
///
/// # Errors
///
/// [`WireError::Truncated`] if fewer than [`REQUEST_QUESTGOING_SIZE`] bytes are
/// supplied.
pub fn decode_quest_going(payload: &[u8]) -> Result<QuestGoing, WireError> {
    let mut r = Reader::new(payload);
    Ok(QuestGoing {
        quest_type: r.i16()?,
        quest_unique: r.i16()?,
        quest_step: r.i16()?,
        select_item_index: r.i16()?,
    })
}

/// `sizeof(cltGameMsgRequest_CancelQuest)` (`MsgType-Quest.h:28-38`).
pub const REQUEST_CANCELQUEST_SIZE: usize = 4;

/// The 5701 body — give up the in-progress quest of this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CancelQuest {
    /// `siQuestType`.
    pub quest_type: i16,
    /// `siQuestUnique`.
    pub quest_unique: i16,
}

/// Decodes the 5701 body.
///
/// # Errors
///
/// [`WireError::Truncated`] if fewer than [`REQUEST_CANCELQUEST_SIZE`] bytes are
/// supplied.
pub fn decode_cancel_quest(payload: &[u8]) -> Result<CancelQuest, WireError> {
    let mut r = Reader::new(payload);
    Ok(CancelQuest {
        quest_type: r.i16()?,
        quest_unique: r.i16()?,
    })
}

/// `sizeof(cltGameMsgRequest_QuestStatus)` (`MsgType-Quest.h:7-23`).
pub const REQUEST_QUESTSTATUS_SIZE: usize = 10;

/// The 5702 body — "I satisfied this condition".
///
/// The client sends it from a dozen places; the ones that matter for the
/// newbie chain are the map-arrival loop (`Client.cpp:5499-5547`, every 500 ms)
/// and the dialog/UI reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuestStatus {
    /// `siQuestType`.
    pub quest_type: i16,
    /// `siQuestUnique`.
    pub quest_unique: i16,
    /// `siQuestClearCond` — the `QUEST_CLEAR_COND_*` being reported.
    pub quest_clear_cond: i16,
    /// `siQuestPara1`.
    pub para1: i16,
    /// `siQuestPara2`.
    pub para2: i16,
}

/// Decodes the 5702 body.
///
/// # Errors
///
/// [`WireError::Truncated`] if fewer than [`REQUEST_QUESTSTATUS_SIZE`] bytes are
/// supplied.
pub fn decode_quest_status(payload: &[u8]) -> Result<QuestStatus, WireError> {
    let mut r = Reader::new(payload);
    Ok(QuestStatus {
        quest_type: r.i16()?,
        quest_unique: r.i16()?,
        quest_clear_cond: r.i16()?,
        para1: r.i16()?,
        para2: r.i16()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference session's 225 body, straight out of the recorded capture:
    /// `(225, 62666, 1536)` in `archive/capture/generated_frames.rs`, bytes in
    /// `archive/capture/cap_s2c.bin`. Read from the real file rather than typed
    /// out, so this test cannot drift from the evidence.
    fn captured_person_quest() -> Vec<u8> {
        const CAPTURE: &[u8] = include_bytes!("../../../archive/capture/cap_s2c.bin");
        CAPTURE[62666..62666 + PERSON_QUEST_LEN].to_vec()
    }

    /// Quest counts per group file — the second column of the pin table in the
    /// module docs, from `New Era Live\Quest\*.txt`.
    const BATTLE1_QUESTS: u32 = 13;
    const BEGINNER2_QUESTS: u32 = 11;
    const BASIC1_QUESTS: u32 = 10;
    const MAKEBEGINNER1_QUESTS: u32 = 15;
    const BEGINNER1_QUESTS: u32 = 15;

    /// The capture pins the layout: every cleared-quest mask lands on the type
    /// whose group has that many quests, and a wrong offset cannot reproduce it.
    #[test]
    fn the_captured_body_pins_the_unit_layout() {
        let body = captured_person_quest();
        assert_eq!(body.len(), PERSON_QUEST_LEN);
        let quest = PersonQuest::decode(&body).expect("the captured body is exactly 1536 bytes");

        // Type -> (mask, quests in that type's group file). Five of the six are
        // exactly full, which is what makes this a pin and not a coincidence.
        let expected = [
            (1usize, 0x1fffi32, BATTLE1_QUESTS),
            (14, 0x7ff, BEGINNER2_QUESTS),
            (16, 0x3ff, BASIC1_QUESTS),
            (18, 0x7fff, MAKEBEGINNER1_QUESTS),
            (37, 0x7fff, BEGINNER1_QUESTS),
        ];
        for (quest_type, mask, quests) in expected {
            let unit = quest
                .unit(quest_type as i16)
                .expect("quest type is inside QUEST_TYPES");
            assert_eq!(
                unit.state, mask,
                "type {quest_type} mask — a wrong clUnit offset reads another type's bits"
            );
            assert_eq!(
                unit.state.count_ones(),
                quests,
                "type {quest_type} claims exactly the quests its group defines"
            );
        }

        // The in-progress unit: type 22 (HUNTINGGROUND01) with unique 2060 —
        // defined by exactly one group file — at step 20 (middle).
        let hunting = quest.unit(22).unwrap();
        assert_eq!(hunting.state, 0x3f);
        assert_eq!(hunting.cur_unique, 2060);
        assert_eq!(hunting.cur_step, QUEST_STEP_MIDDLE);
        assert_eq!(hunting.cmd, 3);
        assert_eq!((hunting.para1, hunting.para2), (24, 24));

        // Re-encoding reproduces every **unit** byte for byte. The 592-byte prefix
        // is deliberately not modelled (it holds blocks this port has no use for
        // yet), so the comparison starts at `clUnit` — and the capture's prefix is
        // non-zero, which is exactly why byte equality over the whole body would be
        // the wrong assertion.
        let prefix_non_zero = body[..QUEST_UNITS_OFFSET]
            .iter()
            .filter(|b| **b != 0)
            .count();
        assert!(
            prefix_non_zero > 0,
            "the capture has data before clUnit ({prefix_non_zero} bytes); the port zeroes it"
        );
        let reencoded = quest.encode();
        assert_eq!(reencoded.len(), PERSON_QUEST_LEN);
        assert_eq!(
            &reencoded[QUEST_UNITS_OFFSET..],
            &body[QUEST_UNITS_OFFSET..],
            "the unit array must round-trip byte for byte"
        );
    }

    /// The tree's own arithmetic (`MAX_QUEST_TYPE = 53`, `clUnit` at 688) does
    /// **not** decode the capture: it puts the 11-bit BEGINNER2 mask on type 8
    /// (`MAKEARMOUR1`, 15 quests). Kept as a test so nobody "simplifies" the
    /// constants back to the source tree's numbers.
    #[test]
    fn the_source_tree_derived_offset_is_wrong_for_the_shipped_client() {
        let body = captured_person_quest();
        let tree_offset = PERSON_QUEST_LEN - 53 * QUEST_UNIT_LEN;
        assert_eq!(tree_offset, 688);
        let unit_at =
            |offset: usize| QuestUnit::decode(&body[offset..offset + QUEST_UNIT_LEN]).unwrap();
        // 816 is BEGINNER2 under the pinned offset (592) and MAKEARMOUR1 under
        // the tree's 688.
        assert_eq!(
            unit_at(QUEST_UNITS_OFFSET + 14 * QUEST_UNIT_LEN).state,
            0x7ff
        );
        assert_eq!(unit_at(688 + 8 * QUEST_UNIT_LEN).state, 0x7ff);
        assert_ne!(14usize, 8);
    }

    #[test]
    fn the_beginner2_bit_for_quest_30005_lands_where_the_client_reads_it() {
        let mut quest = PersonQuest::new();
        quest
            .unit_mut(QUEST_TYPE_BEGINNER2 as i16)
            .unwrap()
            .set_clear(2); // beginner2.txt: bit 2 = unique 30005
        let body = quest.encode();
        assert_eq!(body.len(), PERSON_QUEST_LEN);
        // Type 14 starts at 592 + 14*16 = 816; siQuestState is the first word.
        assert_eq!(body[816], 0x04);
        assert_eq!(&body[817..820], &[0, 0, 0]);
        // Nothing else in the body moved.
        let non_zero: Vec<usize> = body
            .iter()
            .enumerate()
            .filter(|(_, b)| **b != 0)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(non_zero, vec![816]);
    }

    #[test]
    fn a_unit_round_trips_through_the_wire_form() {
        let unit = QuestUnit {
            state: -1,
            cur_unique: 30005,
            cur_step: QUEST_STEP_CLEAR,
            cmd: QUEST_CLEAR_COND_EQUIPPOS,
            para1: 7,
            para2: 1,
        };
        assert_eq!(QuestUnit::decode(&unit.encode()).unwrap(), unit);
        // The tail padding is part of the pinned size.
        assert_eq!(unit.encode().len(), QUEST_UNIT_LEN);
        assert_eq!(&unit.encode()[14..16], &[0, 0]);
    }

    #[test]
    fn the_whole_block_round_trips() {
        let mut quest = PersonQuest::new();
        quest
            .unit_mut(14)
            .unwrap()
            .set_going(30005, QUEST_STEP_GOING, 55, 7, 1);
        quest.unit_mut(37).unwrap().set_clear(0);
        let decoded = PersonQuest::decode(&quest.encode()).unwrap();
        assert_eq!(decoded, quest);
        assert_eq!(decoded.unit(14).unwrap().cur_unique, 30005);
        assert!(decoded.unit(37).unwrap().is_clear(0));
        assert!(!decoded.unit(37).unwrap().is_clear(1));
    }

    #[test]
    fn an_out_of_range_type_has_no_unit() {
        let quest = PersonQuest::new();
        assert!(quest.unit(-1).is_none());
        assert!(quest.unit(QUEST_TYPES as i16).is_none());
        assert!(quest.unit(QUEST_TYPES as i16 - 1).is_some());
        // Clearing a bit outside the SI32 is a no-op, not a panic.
        let mut unit = QuestUnit::default();
        unit.set_clear(200);
        assert_eq!(unit.state, 0);
    }

    #[test]
    fn truncated_payloads_are_rejected() {
        let body = captured_person_quest();
        assert!(matches!(
            PersonQuest::decode(&body[..PERSON_QUEST_LEN - 1]),
            Err(WireError::Truncated { .. })
        ));
        assert!(matches!(
            QuestUnit::decode(&[0u8; QUEST_UNIT_LEN - 1]),
            Err(WireError::Truncated { .. })
        ));
        assert!(matches!(
            decode_quest_going(&[0u8; REQUEST_QUESTGOING_SIZE - 1]),
            Err(WireError::Truncated { .. })
        ));
        assert!(matches!(
            decode_cancel_quest(&[0u8; REQUEST_CANCELQUEST_SIZE - 1]),
            Err(WireError::Truncated { .. })
        ));
        assert!(matches!(
            decode_quest_status(&[0u8; REQUEST_QUESTSTATUS_SIZE - 1]),
            Err(WireError::Truncated { .. })
        ));
    }

    #[test]
    fn the_request_bodies_decode_into_the_legacy_field_order() {
        let mut going = [0u8; REQUEST_QUESTGOING_SIZE];
        going[0..2].copy_from_slice(&14i16.to_le_bytes());
        going[2..4].copy_from_slice(&30005i16.to_le_bytes());
        going[4..6].copy_from_slice(&QUEST_STEP_CLEAR.to_le_bytes());
        going[6..8].copy_from_slice(&1i16.to_le_bytes());
        assert_eq!(
            decode_quest_going(&going).unwrap(),
            QuestGoing {
                quest_type: 14,
                quest_unique: 30005,
                quest_step: QUEST_STEP_CLEAR,
                select_item_index: 1,
            }
        );

        let mut status = [0u8; REQUEST_QUESTSTATUS_SIZE];
        status[0..2].copy_from_slice(&14i16.to_le_bytes());
        status[2..4].copy_from_slice(&30000i16.to_le_bytes());
        status[4..6].copy_from_slice(&QUEST_CLEAR_COND_OPENCHARSTATUS.to_le_bytes());
        status[6..8].copy_from_slice(&0i16.to_le_bytes());
        status[8..10].copy_from_slice(&1i16.to_le_bytes());
        assert_eq!(
            decode_quest_status(&status).unwrap(),
            QuestStatus {
                quest_type: 14,
                quest_unique: 30000,
                quest_clear_cond: QUEST_CLEAR_COND_OPENCHARSTATUS,
                para1: 0,
                para2: 1,
            }
        );

        let mut cancel = [0u8; REQUEST_CANCELQUEST_SIZE];
        cancel[0..2].copy_from_slice(&14i16.to_le_bytes());
        cancel[2..4].copy_from_slice(&30005i16.to_le_bytes());
        assert_eq!(
            decode_cancel_quest(&cancel).unwrap(),
            CancelQuest {
                quest_type: 14,
                quest_unique: 30005,
            }
        );
    }
}
