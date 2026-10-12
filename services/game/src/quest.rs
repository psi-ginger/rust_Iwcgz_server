//! The quest family — 5700 `REQUEST_SETQUESTGOING`, 5701 `REQUEST_CANCELQUEST`,
//! 5702 `REQUEST_QUESTSTATUS` — and the 225 push that keeps the client's own
//! quest block true (T-019 slice 1).
//!
//! # Why this exists
//!
//! The client will not walk out of the starting map while it believes the
//! newbie chain is unfinished: `MouseOperation.cpp:5500-5511` refuses the move
//! order while `ClearQuest(30005) == false && ClearQuest(30006) == false &&
//! mapindex == MAPINDEX_PALACE (158) && level < 10`, i.e. the exit is guarded by
//! *client-side* quest state that the server has to make true. We used to answer
//! 225 with zeros, so the guard could never lift. See
//! `docs\rust-rewrite\tasks\T-019-npc-quest-hanyang-exit.md`.
//!
//! # Scope (slice 1)
//!
//! * the four quests of Sarah Spencer's `BEGINNER2` chain that carry the
//!   conditions this port can evaluate: `OPENCHARSTATUS`, `BONUSZERO`,
//!   `EQUIPPOS`, `GOMAP` (`New Era Live\Quest\beginner2\3000{0,3,5,6}E.txt`);
//! * accept → in-progress → cleared, per type, persisted per type;
//! * 225 after every change, to the acting session only — the legacy gates it on
//!   `UPDATE_QUEST` and sends it to self (`Char-UpdateChangedData.cpp:247-253`).
//!
//! **Deviation — rewards are logged, not granted.** Each of the four quests
//! hands out money, exp and an item (`30005E.txt:21,25` and its siblings). This
//! port has no exp/level path at all (T-018 slice 6 C is explicitly deferred)
//! and no "grant an item into a free slot" repository call, so granting only the
//! money would be a half-truth and silently inventing an exp curve would be
//! worse. The clear path therefore logs exactly what the legacy would have
//! granted, and the card records this as the open half of slice 1.
//!
//! **Policy — soft-fail.** An unknown quest, a condition the port does not
//! model, a request for a quest that is not in progress, or a failed check is a
//! logged no-op. The session is never closed; the legacy's `CloseSocket()` paths
//! are deliberately not ported (same policy as 250).

use gz_cipher::Cipher;
use gz_db::{CharacterFull, ItemRepository, QuestRepository, RepositoryError};
use gz_proto::opcodes_game::GameMsg;
use gz_proto::quest::{
    CancelQuest, PersonQuest, QUEST_CLEAR_COND_BONUSZERO, QUEST_CLEAR_COND_EQUIPPOS,
    QUEST_CLEAR_COND_GOMAP, QUEST_CLEAR_COND_OPENCHARSTATUS, QUEST_STEP_CLEAR, QuestGoing,
    QuestStatus, decode_cancel_quest, decode_quest_going, decode_quest_status,
};
use tokio::net::TcpStream;

use crate::{AnyError, logln, send_encoded};

/// What the handlers need to read about the acting character.
///
/// A snapshot rather than a borrow: `handle_client` holds the live row behind
/// `LiveFrames`, and taking these three values out first is what lets the
/// handlers borrow the quest block mutably at the same time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QuestActor {
    /// `cltPIPInfo.siLevel`.
    pub level: i16,
    /// `cltPIPInfo.siBonus` — `QUEST_CLEAR_COND_BONUSZERO` wants it at 0
    /// (`Char-Quest.cpp:104-110`).
    pub bonus_points: i16,
    /// The acting character's row id, for the repository calls.
    pub character_id: i32,
}

impl QuestActor {
    /// Reads the three values off the selected character row.
    #[must_use]
    pub(crate) fn from_character(character: &CharacterFull) -> Self {
        Self {
            level: character.level,
            bonus_points: character.bonus_points,
            character_id: character.character_id,
        }
    }
}

/// One quest of a modelled chain.
///
/// `bit` is the quest's row in its group file's `Bit` column, which is the bit
/// `siQuestState` uses (`Quest.h:191-200`); `predecessor` is the quest that must
/// already be cleared for this one to be accepted — the chain the legacy
/// enforces through `siMustClearQuestUnique` (`Server-Person.cpp:1881-1888`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChainQuest {
    /// `QUEST_TYPE_*` index.
    pub quest_type: i16,
    /// `siQuestUnique`.
    pub unique: i16,
    /// Bit index inside the type's `siQuestState`.
    pub bit: u8,
    /// `QUEST_CLEAR_COND_*` the client has to satisfy.
    pub cmd: i16,
    /// `siPara1` of the condition.
    pub para1: i16,
    /// `siPara2` of the condition.
    pub para2: i16,
    /// The quest that must be cleared first, or 0 for the chain's first.
    pub predecessor: i16,
    /// Reward money the legacy grants on clear (`Reward_Other.Money`).
    pub reward_money: i64,
    /// Reward exp the legacy grants (`Reward_Other.Exp`).
    pub reward_exp: i32,
    /// Reward item unique (`Reward_Item.ItemUnique1`), 0 for none.
    pub reward_item: i16,
    /// Reward item count.
    pub reward_item_num: i16,
}

/// Sarah Spencer's `BEGINNER2` chain, in order, as the client's data defines it.
///
/// Sources: `New Era Live\Quest\beginner2.txt:9-19` (bit → unique) and each
/// quest's own `Condition` / `Para1` / `Para2` / reward rows. `30005` and
/// `30006` are the two the client's gate checks
/// (`MouseOperation.cpp:5500-5502`).
pub(crate) const BEGINNER2_CHAIN: &[ChainQuest] = &[
    ChainQuest {
        quest_type: 14, // QUEST_TYPE_BEGINNER2
        unique: 30000,
        bit: 0,
        cmd: QUEST_CLEAR_COND_OPENCHARSTATUS,
        para1: 0,
        para2: 1,
        predecessor: 0,
        reward_money: 100,
        reward_exp: 30,
        reward_item: 1500,
        reward_item_num: 1,
    },
    ChainQuest {
        quest_type: 14,
        unique: 30003,
        bit: 1,
        cmd: QUEST_CLEAR_COND_BONUSZERO,
        para1: 0,
        para2: 1,
        predecessor: 30000,
        reward_money: 110,
        reward_exp: 30,
        reward_item: 20164,
        reward_item_num: 1,
    },
    ChainQuest {
        quest_type: 14,
        unique: 30005,
        bit: 2,
        cmd: QUEST_CLEAR_COND_EQUIPPOS,
        para1: 7,
        para2: 1,
        predecessor: 30003,
        reward_money: 120,
        reward_exp: 30,
        reward_item: 20163,
        reward_item_num: 1,
    },
    ChainQuest {
        quest_type: 14,
        unique: 30006,
        bit: 3,
        cmd: QUEST_CLEAR_COND_GOMAP,
        para1: 0,
        para2: 1,
        predecessor: 30005,
        reward_money: 130,
        reward_exp: 40,
        reward_item: 2565,
        reward_item_num: 20,
    },
];

/// The chain quest named by `(type, unique)`, if this port models it.
fn chain_quest(quest_type: i16, unique: i16) -> Option<&'static ChainQuest> {
    BEGINNER2_CHAIN
        .iter()
        .find(|q| q.quest_type == quest_type && q.unique == unique)
}

/// The one item question a quest condition asks: is `slot` occupied?
///
/// A narrow trait rather than the whole [`ItemRepository`]: `EQUIPPOS` needs one
/// boolean (`Char-Quest.cpp:360-368` reads `clCharItem.clItem[para1].siUnique`),
/// and the blanket implementation below covers every repository, which keeps the
/// handlers testable without a database.
#[async_trait::async_trait]
pub(crate) trait SlotOccupancy: Send + Sync {
    /// Whether `character_id` holds an item in `slot`.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Database`] if the read fails.
    async fn slot_occupied(&self, character_id: i32, slot: u16) -> Result<bool, RepositoryError>;
}

#[async_trait::async_trait]
impl<T: ItemRepository + ?Sized> SlotOccupancy for T {
    async fn slot_occupied(&self, character_id: i32, slot: u16) -> Result<bool, RepositoryError> {
        Ok(self
            .list(character_id)
            .await?
            .into_iter()
            .any(|row| row.slot == slot && row.item_unique != 0))
    }
}

/// What a request did to the quest block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuestOutcome {
    /// The block changed: persist and push 225.
    Changed,
    /// Nothing to persist or send — the request was a logged no-op.
    NoChange,
}

/// Applies a 5700 to the in-memory block.
///
/// The legacy dispatches this request to `DoMsg_GAMEMSG_REQUEST_SETQUESTGOING`
/// (`Server-Person.cpp:1094`), which treats `QUEST_STEP_CLEAR` as "check my
/// completion condition" (`:1125-1128`) and everything else as "start this
/// quest" (`:1864-1912`, gated on the init-condition level, the skill and the
/// must-clear predecessor).
///
/// # Errors
///
/// Propagates repository failures. A *check* that fails is not an error: it is a
/// logged [`QuestOutcome::NoChange`].
#[allow(clippy::too_many_arguments)]
pub(crate) async fn apply_quest_going(
    quests: &dyn QuestRepository,
    slots: &dyn SlotOccupancy,
    actor: &QuestActor,
    state: &mut PersonQuest,
    request: QuestGoing,
) -> Result<QuestOutcome, AnyError> {
    let Some(quest) = chain_quest(request.quest_type, request.quest_unique) else {
        logln!(
            "game: quest going {} type {} not modelled — ignored",
            request.quest_unique,
            request.quest_type
        );
        return Ok(QuestOutcome::NoChange);
    };

    // Every read of the block happens before the mutable borrow below: the
    // accept path needs the predecessor's bit and this quest's own bit, and the
    // clear path needs neither.
    if request.quest_step != QUEST_STEP_CLEAR {
        if quest.predecessor != 0 {
            let cleared = BEGINNER2_CHAIN
                .iter()
                .find(|q| q.unique == quest.predecessor)
                .is_some_and(|prev| {
                    state
                        .unit(prev.quest_type)
                        .is_some_and(|u| u.is_clear(prev.bit))
                });
            if !cleared {
                logln!(
                    "game: quest {} refused — predecessor {} is not cleared",
                    quest.unique,
                    quest.predecessor
                );
                return Ok(QuestOutcome::NoChange);
            }
        }
        if state
            .unit(quest.quest_type)
            .is_some_and(|u| u.is_clear(quest.bit))
        {
            logln!("game: quest {} already cleared — ignored", quest.unique);
            return Ok(QuestOutcome::NoChange);
        }
    }

    let Some(unit) = state.unit_mut(quest.quest_type) else {
        // Unreachable: `chain_quest` only returns types inside the block.
        return Ok(QuestOutcome::NoChange);
    };

    if request.quest_step == QUEST_STEP_CLEAR {
        if unit.cur_unique != quest.unique {
            logln!(
                "game: quest {} clear for a quest that is not in progress (going={}) — ignored",
                quest.unique,
                unit.cur_unique
            );
            return Ok(QuestOutcome::NoChange);
        }
        // The condition itself. Two are real server-side checks, two are the
        // client's own report (the legacy's whitelist at `:1924-2015` treats the
        // para pair as the proof and returns true on a match, `Char-Quest.cpp:547-551`).
        let satisfied = match quest.cmd {
            QUEST_CLEAR_COND_BONUSZERO => actor.bonus_points == 0,
            QUEST_CLEAR_COND_EQUIPPOS => {
                let slot = u16::try_from(quest.para1).unwrap_or(u16::MAX);
                slots.slot_occupied(actor.character_id, slot).await?
            }
            QUEST_CLEAR_COND_OPENCHARSTATUS | QUEST_CLEAR_COND_GOMAP => true,
            other => {
                logln!(
                    "game: quest {} condition {other} not modelled — ignored",
                    quest.unique
                );
                return Ok(QuestOutcome::NoChange);
            }
        };
        if !satisfied {
            logln!(
                "game: quest {} clear refused (condition {} para {}/{})",
                quest.unique,
                quest.cmd,
                quest.para1,
                quest.para2
            );
            return Ok(QuestOutcome::NoChange);
        }
        unit.set_clear(quest.bit);
        unit.clear_going();
        quests
            .save(actor.character_id, quest.quest_type, unit)
            .await?;
        logln!(
            "game: quest {} (type {}, bit {}) cleared — reward not granted (deviation: \
             money {} / exp {} / item {} x{}, no money/exp/item-grant path yet)",
            quest.unique,
            quest.quest_type,
            quest.bit,
            quest.reward_money,
            quest.reward_exp,
            quest.reward_item,
            quest.reward_item_num
        );
        return Ok(QuestOutcome::Changed);
    }

    // Accept / advance. The predecessor and already-cleared checks ran before
    // the mutable borrow above.
    if actor.level < 1 {
        // `InitCondition.Level` is 1 for all four (`30000E.txt:31-33`).
        return Ok(QuestOutcome::NoChange);
    }
    unit.set_going(
        quest.unique,
        request.quest_step,
        quest.cmd,
        quest.para1,
        quest.para2,
    );
    quests
        .save(actor.character_id, quest.quest_type, unit)
        .await?;
    logln!(
        "game: quest {} (type {}) started at step {} — condition {} para {}/{}",
        quest.unique,
        quest.quest_type,
        request.quest_step,
        quest.cmd,
        quest.para1,
        quest.para2
    );
    Ok(QuestOutcome::Changed)
}

/// Applies a 5701 to the in-memory block — `Server-Person.cpp:1011-1087`, which
/// zeroes the type's in-progress fields and leaves the cleared bits alone.
///
/// # Errors
///
/// Propagates repository failures.
pub(crate) async fn apply_cancel_quest(
    quests: &dyn QuestRepository,
    actor: &QuestActor,
    state: &mut PersonQuest,
    request: CancelQuest,
) -> Result<QuestOutcome, AnyError> {
    let Some(quest) = chain_quest(request.quest_type, request.quest_unique) else {
        logln!(
            "game: quest cancel {} type {} not modelled — ignored",
            request.quest_unique,
            request.quest_type
        );
        return Ok(QuestOutcome::NoChange);
    };
    let Some(unit) = state.unit_mut(quest.quest_type) else {
        return Ok(QuestOutcome::NoChange);
    };
    if unit.cur_unique != quest.unique {
        logln!(
            "game: quest cancel {} for a quest that is not in progress (going={}) — ignored",
            quest.unique,
            unit.cur_unique
        );
        return Ok(QuestOutcome::NoChange);
    }
    unit.clear_going();
    quests
        .save(actor.character_id, quest.quest_type, unit)
        .await?;
    logln!("game: quest {} cancelled", quest.unique);
    Ok(QuestOutcome::Changed)
}

/// Applies a 5702 to the in-memory block.
///
/// The legacy's handler (`Server-Person.cpp:1917-2066`) accepts a whitelist of
/// client-reported conditions, compares the request's para pair against the
/// block's stored pair (`Char-Quest.cpp:547-551`) and then runs
/// `DoQuestCheckAndAction` (`:2064`). Anything else is an explicit `return`.
///
/// # Errors
///
/// Propagates repository failures.
pub(crate) async fn apply_quest_status(
    quests: &dyn QuestRepository,
    slots: &dyn SlotOccupancy,
    actor: &QuestActor,
    state: &mut PersonQuest,
    request: QuestStatus,
) -> Result<QuestOutcome, AnyError> {
    let Some(quest) = chain_quest(request.quest_type, request.quest_unique) else {
        logln!(
            "game: quest status {} type {} not modelled — ignored",
            request.quest_unique,
            request.quest_type
        );
        return Ok(QuestOutcome::NoChange);
    };
    let Some(unit) = state.unit_mut(quest.quest_type) else {
        return Ok(QuestOutcome::NoChange);
    };
    if unit.cur_unique != quest.unique {
        // The reported quest is not the one this type has in progress; the
        // legacy compares the stored command and para pair before acting, so
        // this cannot clear anything.
        logln!(
            "game: quest status {} ignored — in progress is {}",
            quest.unique,
            unit.cur_unique
        );
        return Ok(QuestOutcome::NoChange);
    }
    if request.quest_clear_cond != unit.cmd {
        logln!(
            "game: quest status {} reported condition {} but {} is in progress",
            quest.unique,
            request.quest_clear_cond,
            unit.cmd
        );
        return Ok(QuestOutcome::NoChange);
    }
    if request.para1 != quest.para1 || request.para2 != quest.para2 {
        logln!(
            "game: quest status {} reported para {}/{} but the quest wants {}/{}",
            quest.unique,
            request.para1,
            request.para2,
            quest.para1,
            quest.para2
        );
        return Ok(QuestOutcome::NoChange);
    }
    // Same split as the 5700 clear path: the real checks are re-run here, the
    // client-reported ones are proven by the report itself.
    let satisfied = match quest.cmd {
        QUEST_CLEAR_COND_BONUSZERO => actor.bonus_points == 0,
        QUEST_CLEAR_COND_EQUIPPOS => {
            let slot = u16::try_from(quest.para1).unwrap_or(u16::MAX);
            slots.slot_occupied(actor.character_id, slot).await?
        }
        QUEST_CLEAR_COND_OPENCHARSTATUS | QUEST_CLEAR_COND_GOMAP => true,
        other => {
            logln!("game: quest status condition {other} not modelled — ignored");
            return Ok(QuestOutcome::NoChange);
        }
    };
    if !satisfied {
        logln!(
            "game: quest {} status refused by the condition check",
            quest.unique
        );
        return Ok(QuestOutcome::NoChange);
    }
    unit.set_clear(quest.bit);
    unit.clear_going();
    quests
        .save(actor.character_id, quest.quest_type, unit)
        .await?;
    logln!(
        "game: quest {} (type {}, bit {}) cleared by the client's report — reward not granted \
         (deviation: money {} / exp {} / item {} x{})",
        quest.unique,
        quest.quest_type,
        quest.bit,
        quest.reward_money,
        quest.reward_exp,
        quest.reward_item,
        quest.reward_item_num
    );
    Ok(QuestOutcome::Changed)
}

/// Sends 225 with the whole block, to this session only.
///
/// # Errors
///
/// Propagates socket/cipher failures.
pub(crate) async fn push_quest_state(
    stream: &mut TcpStream,
    cipher: &mut Cipher,
    state: &PersonQuest,
) -> Result<(), AnyError> {
    send_encoded(
        stream,
        cipher,
        GameMsg::ResponsePersonQuest,
        &state.encode(),
    )
    .await
}

/// 5700 as a handler: decode, apply, push.
///
/// # Errors
///
/// A malformed body is a logged no-op, not an error; socket and repository
/// failures propagate.
pub(crate) async fn handle_set_quest_going(
    stream: &mut TcpStream,
    cipher: &mut Cipher,
    quests: &dyn QuestRepository,
    slots: &dyn SlotOccupancy,
    actor: &QuestActor,
    state: &mut PersonQuest,
    payload: &[u8],
) -> Result<(), AnyError> {
    let Ok(request) = decode_quest_going(payload) else {
        logln!("game: malformed 5700 ({} bytes) — ignored", payload.len());
        return Ok(());
    };
    if apply_quest_going(quests, slots, actor, state, request).await? == QuestOutcome::Changed {
        push_quest_state(stream, cipher, state).await?;
    }
    Ok(())
}

/// 5701 as a handler.
///
/// # Errors
///
/// As [`handle_set_quest_going`].
pub(crate) async fn handle_cancel_quest(
    stream: &mut TcpStream,
    cipher: &mut Cipher,
    quests: &dyn QuestRepository,
    actor: &QuestActor,
    state: &mut PersonQuest,
    payload: &[u8],
) -> Result<(), AnyError> {
    let Ok(request) = decode_cancel_quest(payload) else {
        logln!("game: malformed 5701 ({} bytes) — ignored", payload.len());
        return Ok(());
    };
    if apply_cancel_quest(quests, actor, state, request).await? == QuestOutcome::Changed {
        push_quest_state(stream, cipher, state).await?;
    }
    Ok(())
}

/// 5702 as a handler.
///
/// # Errors
///
/// As [`handle_set_quest_going`].
pub(crate) async fn handle_quest_status(
    stream: &mut TcpStream,
    cipher: &mut Cipher,
    quests: &dyn QuestRepository,
    slots: &dyn SlotOccupancy,
    actor: &QuestActor,
    state: &mut PersonQuest,
    payload: &[u8],
) -> Result<(), AnyError> {
    let Ok(request) = decode_quest_status(payload) else {
        logln!("game: malformed 5702 ({} bytes) — ignored", payload.len());
        return Ok(());
    };
    if apply_quest_status(quests, slots, actor, state, request).await? == QuestOutcome::Changed {
        push_quest_state(stream, cipher, state).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gz_db::StoredQuestType;
    use gz_proto::quest::{QUEST_STEP_GOING, QuestUnit};
    use std::sync::Mutex;

    /// A quest repository that records the types it was asked to write, so a
    /// test can prove a change was persisted (and that a no-op wrote nothing).
    #[derive(Default)]
    struct MemoryQuests {
        saved: Mutex<Vec<(i16, QuestUnit)>>,
    }

    #[async_trait::async_trait]
    impl QuestRepository for MemoryQuests {
        async fn load(&self, _character_id: i32) -> Result<Vec<StoredQuestType>, RepositoryError> {
            Ok(Vec::new())
        }

        async fn save(
            &self,
            _character_id: i32,
            quest_type: i16,
            unit: &QuestUnit,
        ) -> Result<(), RepositoryError> {
            let mut saved = self.saved.lock().unwrap();
            saved.retain(|(t, _)| *t != quest_type);
            saved.push((quest_type, *unit));
            Ok(())
        }
    }

    impl MemoryQuests {
        fn saved_types(&self) -> Vec<i16> {
            self.saved.lock().unwrap().iter().map(|(t, _)| *t).collect()
        }
    }

    /// Occupancy from a fixed set of slots.
    struct FixedSlots(Vec<u16>);

    #[async_trait::async_trait]
    impl SlotOccupancy for FixedSlots {
        async fn slot_occupied(
            &self,
            _character_id: i32,
            slot: u16,
        ) -> Result<bool, RepositoryError> {
            Ok(self.0.contains(&slot))
        }
    }

    fn newbie(bonus_points: i16) -> QuestActor {
        QuestActor {
            level: 1,
            bonus_points,
            character_id: 42,
        }
    }

    async fn accept(
        quests: &MemoryQuests,
        slots: &FixedSlots,
        actor: &QuestActor,
        state: &mut PersonQuest,
        unique: i16,
    ) -> QuestOutcome {
        apply_quest_going(
            quests,
            slots,
            actor,
            state,
            QuestGoing {
                quest_type: 14,
                quest_unique: unique,
                quest_step: QUEST_STEP_GOING,
                select_item_index: 0,
            },
        )
        .await
        .unwrap()
    }

    /// Reports completion using the condition the chain table says the quest
    /// wants — which is what the client sends.
    async fn report(
        quests: &MemoryQuests,
        slots: &FixedSlots,
        actor: &QuestActor,
        state: &mut PersonQuest,
        unique: i16,
    ) -> QuestOutcome {
        let quest = chain_quest(14, unique).expect("modelled quest");
        apply_quest_status(
            quests,
            slots,
            actor,
            state,
            QuestStatus {
                quest_type: 14,
                quest_unique: unique,
                quest_clear_cond: quest.cmd,
                para1: quest.para1,
                para2: quest.para2,
            },
        )
        .await
        .unwrap()
    }

    /// The acceptance test of T-019 slice 1: playing the chain the way a level-1
    /// character does ends with the bit the client's gate check reads set, at the
    /// byte the client reads it from.
    #[tokio::test]
    async fn the_tutorial_chain_reaches_the_bit_that_opens_the_exit() {
        let quests = MemoryQuests::default();
        let slots = FixedSlots(vec![7]); // the JP Straw Shoes, equipped
        let actor = newbie(0); // every bonus point spent
        let mut state = PersonQuest::new();

        for unique in [30000, 30003, 30005] {
            assert_eq!(
                accept(&quests, &slots, &actor, &mut state, unique).await,
                QuestOutcome::Changed,
                "quest {unique} starts"
            );
            assert_eq!(
                report(&quests, &slots, &actor, &mut state, unique).await,
                QuestOutcome::Changed,
                "quest {unique} clears"
            );
        }

        // Exactly the check `MouseOperation.cpp:5500` makes, then the byte it
        // makes it from.
        let unit = state.unit(14).expect("type 14 is inside the block");
        assert!(
            unit.is_clear(2),
            "30005 cleared == ClearQuest(30005) == true"
        );
        assert_eq!(unit.cur_unique, 0, "the chain advanced past it");
        let body = state.encode();
        // Bits 0..2 — 30000, 30003 and 30005 are all cleared by this point, and
        // bit 2 is the one the gate check reads.
        assert_eq!(body[816], 0x07, "type 14's state word: bits 0, 1 and 2 set");
        assert_ne!(body[816] & 0x04, 0, "ClearQuest(30005) reads bit 2");

        // 30006's condition is GOMAP(map 0) — cleared by walking out of Hanyang,
        // which the client reports from `Client.cpp:5499-5523`.
        assert_eq!(
            accept(&quests, &slots, &actor, &mut state, 30006).await,
            QuestOutcome::Changed
        );
        assert_eq!(
            report(&quests, &slots, &actor, &mut state, 30006).await,
            QuestOutcome::Changed
        );
        assert!(state.unit(14).unwrap().is_clear(3));

        // Every change wrote exactly its own type — the legacy's per-type update
        // (`Server-Person.cpp:1905`).
        assert_eq!(quests.saved_types(), vec![14]);
    }

    #[tokio::test]
    async fn a_quest_cannot_start_before_its_predecessor_is_cleared() {
        let quests = MemoryQuests::default();
        let slots = FixedSlots(vec![7]);
        let actor = newbie(0);
        let mut state = PersonQuest::new();

        // 30005 needs 30003, which needs 30000 — nothing is cleared yet.
        assert_eq!(
            accept(&quests, &slots, &actor, &mut state, 30005).await,
            QuestOutcome::NoChange
        );
        assert_eq!(state.unit(14).unwrap().cur_unique, 0);
        assert!(quests.saved_types().is_empty(), "a refusal writes nothing");
    }

    #[tokio::test]
    async fn bonuszero_refuses_while_points_are_unspent() {
        let quests = MemoryQuests::default();
        let slots = FixedSlots(vec![]);
        let mut state = PersonQuest::new();

        // Start 30000 and clear it, then start 30003.
        let unspent = newbie(5);
        accept(&quests, &slots, &unspent, &mut state, 30000).await;
        report(&quests, &slots, &unspent, &mut state, 30000).await;
        accept(&quests, &slots, &unspent, &mut state, 30003).await;

        // With 5 points left the condition is not met (`Char-Quest.cpp:104-110`).
        assert_eq!(
            report(&quests, &slots, &unspent, &mut state, 30003).await,
            QuestOutcome::NoChange
        );
        assert!(!state.unit(14).unwrap().is_clear(1));

        // The same report after the points are spent clears it.
        let spent = newbie(0);
        assert_eq!(
            report(&quests, &slots, &spent, &mut state, 30003).await,
            QuestOutcome::Changed
        );
        assert!(state.unit(14).unwrap().is_clear(1));
    }

    #[tokio::test]
    async fn equippos_refuses_an_empty_slot() {
        let quests = MemoryQuests::default();
        let actor = newbie(0);
        let mut state = PersonQuest::new();
        let empty = FixedSlots(vec![]);
        let equipped = FixedSlots(vec![7]);

        for unique in [30000, 30003] {
            accept(&quests, &equipped, &actor, &mut state, unique).await;
            report(&quests, &equipped, &actor, &mut state, unique).await;
        }
        accept(&quests, &equipped, &actor, &mut state, 30005).await;

        // Slot 7 empty: `clItem[7].siUnique == 0` (`Char-Quest.cpp:360-368`).
        assert_eq!(
            report(&quests, &empty, &actor, &mut state, 30005).await,
            QuestOutcome::NoChange
        );
        assert!(!state.unit(14).unwrap().is_clear(2));

        assert_eq!(
            report(&quests, &equipped, &actor, &mut state, 30005).await,
            QuestOutcome::Changed
        );
        assert!(state.unit(14).unwrap().is_clear(2));
    }

    #[tokio::test]
    async fn an_unmodelled_or_mismatched_request_is_a_logged_no_op() {
        let quests = MemoryQuests::default();
        let slots = FixedSlots(vec![7]);
        let actor = newbie(0);
        let mut state = PersonQuest::new();

        // A quest this port does not model.
        assert_eq!(
            accept(&quests, &slots, &actor, &mut state, 30015).await,
            QuestOutcome::NoChange
        );
        // A report whose condition is not the one in progress.
        accept(&quests, &slots, &actor, &mut state, 30000).await;
        assert_eq!(
            apply_quest_status(
                &quests,
                &slots,
                &actor,
                &mut state,
                QuestStatus {
                    quest_type: 14,
                    quest_unique: 30000,
                    quest_clear_cond: QUEST_CLEAR_COND_EQUIPPOS,
                    para1: 7,
                    para2: 1,
                },
            )
            .await
            .unwrap(),
            QuestOutcome::NoChange
        );
        // A report for a quest that is not in progress.
        assert_eq!(
            report(&quests, &slots, &actor, &mut state, 30003).await,
            QuestOutcome::NoChange
        );
        assert!(!state.unit(14).unwrap().is_clear(0));
        assert!(!state.unit(14).unwrap().is_clear(1));
    }

    #[tokio::test]
    async fn cancelling_clears_the_going_fields_and_not_the_bits() {
        let quests = MemoryQuests::default();
        let slots = FixedSlots(vec![7]);
        let actor = newbie(0);
        let mut state = PersonQuest::new();

        accept(&quests, &slots, &actor, &mut state, 30000).await;
        assert_eq!(state.unit(14).unwrap().cur_unique, 30000);

        let outcome = apply_cancel_quest(
            &quests,
            &actor,
            &mut state,
            CancelQuest {
                quest_type: 14,
                quest_unique: 30000,
            },
        )
        .await
        .unwrap();
        assert_eq!(outcome, QuestOutcome::Changed);
        let unit = state.unit(14).unwrap();
        assert_eq!(unit.cur_unique, 0);
        assert_eq!(unit.cur_step, 0);
        assert_eq!(unit.cmd, 0);
        assert!(!unit.is_clear(0), "cancelling must not clear the quest");

        // Cancelling something that is not in progress changes nothing.
        let again = apply_cancel_quest(
            &quests,
            &actor,
            &mut state,
            CancelQuest {
                quest_type: 14,
                quest_unique: 30000,
            },
        )
        .await
        .unwrap();
        assert_eq!(again, QuestOutcome::NoChange);
    }

    /// The chain table's four entries are the ones the client's gate names, and
    /// each carries the condition its own quest file declares.
    #[test]
    fn the_chain_matches_the_client_data() {
        assert_eq!(BEGINNER2_CHAIN.len(), 4);
        let uniques: Vec<i16> = BEGINNER2_CHAIN.iter().map(|q| q.unique).collect();
        assert_eq!(uniques, vec![30000, 30003, 30005, 30006]);
        assert_eq!(BEGINNER2_CHAIN[0].cmd, QUEST_CLEAR_COND_OPENCHARSTATUS);
        assert_eq!(BEGINNER2_CHAIN[1].cmd, QUEST_CLEAR_COND_BONUSZERO);
        assert_eq!(BEGINNER2_CHAIN[2].cmd, QUEST_CLEAR_COND_EQUIPPOS);
        assert_eq!(BEGINNER2_CHAIN[2].para1, 7, "the shoe slot");
        assert_eq!(BEGINNER2_CHAIN[3].cmd, QUEST_CLEAR_COND_GOMAP);
        assert_eq!(BEGINNER2_CHAIN[3].para1, 0, "map 0, the outdoor field");
        // Bits are the group file's Bit column, and 30005/30006 are exactly the
        // two quests `MouseOperation.cpp:5500-5502` tests.
        assert_eq!(BEGINNER2_CHAIN[2].bit, 2);
        assert_eq!(BEGINNER2_CHAIN[3].bit, 3);
        // The chain is a chain.
        assert_eq!(BEGINNER2_CHAIN[0].predecessor, 0);
        for pair in BEGINNER2_CHAIN.windows(2) {
            assert_eq!(pair[1].predecessor, pair[0].unique);
        }
    }
}
