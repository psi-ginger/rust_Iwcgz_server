//! The quest request family: 5700 `REQUEST_SETQUESTGOING`, 5701
//! `REQUEST_CANCELQUEST`, 5702 `REQUEST_QUESTSTATUS`, and the 225 push that keeps
//! the client's own quest block true.
//!
//! The `apply_*` functions are the decisions and take every collaborator as a trait
//! object; the `handle_*` functions are the I/O shells that decode, apply, and send
//! what the outcome asks for. That split is what lets the whole state machine be
//! tested without a socket or a database.

use gz_cipher::Cipher;
use gz_db::{CharacterRepository, ItemRepository, QuestRepository, RepositoryError};
use gz_proto::item::{Item, encode_person_item_info};
use gz_proto::opcodes_game::GameMsg;
use gz_proto::quest::{
    CancelQuest, PersonQuest, QUEST_CLEAR_COND_BONUSZERO, QUEST_CLEAR_COND_EQUIPPOS,
    QUEST_CLEAR_COND_GOMAP, QUEST_CLEAR_COND_OPENCHARSTATUS, QUEST_STEP_CLEAR, QuestGoing,
    QuestStatus, decode_cancel_quest, decode_quest_going, decode_quest_status,
};
use tokio::net::TcpStream;

use super::QuestActor;
use super::chain::{BEGINNER2_CHAIN, chain_quest};
use super::reward::{QuestRewardStore, RewardGrant, RewardPlan, RewardStorage, grant_reward};
use crate::{AnyError, logln, send_encoded};

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
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum QuestOutcome {
    /// The block changed and a reward was paid: persist, push 225, and tell the
    /// client about the item slot the reward landed in.
    Cleared { reward: RewardGrant },
    /// Granting the reward was refused because the inventory has no room: nothing
    /// changed, and the client is told why (`SRVAL_QUEST_FAIL_FULLINV`,
    /// `Server-Person.cpp:1287-1296`).
    RewardRefused,
    /// The block changed: persist and push 225.
    Changed,
    /// **Nothing changed, but push 225 anyway.** The request only makes sense if
    /// the client's own copy of the block disagrees with ours, and the client has
    /// no other way to learn ours: it derives the quest it shows from the 225 the
    /// server sent (`Quest-Manager.cpp:701-723` reads the type's `clUnit`, and
    /// falls back to "lowest unset bit" when the unit is empty), and only a fresh
    /// 225 makes it re-derive (`Client-Person-BigInfo.cpp:254-268`).
    ///
    /// The 2026-10-13 live run is why this exists: the character's block said
    /// 30000 and 30003 were cleared with 30005 in progress, the client still asked
    /// to *start* 30000, and the old silent `NoChange` left the player clicking a
    /// button that could never do anything.
    Resync,
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
    storage: &dyn RewardStorage,
    npc_price_rate: i32,
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
            // The client asked to start a quest we already have cleared. Its copy
            // of the block is provably stale (otherwise it would not ask), and only
            // a fresh 225 can fix that, so answer with the block instead of
            // silence. A *going* step cannot come from the client's own
            // condition-met retry -- that one sends `QUEST_STEP_CLEAR`
            // (`Quest-NInterface.cpp:217`) -- so this cannot echo into a loop.
            logln!(
                "game: quest {} already cleared — resyncing the client's quest block",
                quest.unique
            );
            return Ok(QuestOutcome::Resync);
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
        // The legacy pays before it clears, and refuses the clear when it cannot
        // pay (`Server-Person.cpp:1287-1296`): a reward is never lost to a full
        // inventory. Granting first also keeps our two writes ordered so a failure
        // cannot leave a cleared quest that was never paid.
        let plan = RewardPlan::for_quest(quest, npc_price_rate);
        let reward = match grant_reward(storage, actor, &plan).await? {
            RewardGrant::InventoryFull => {
                logln!(
                    "game: quest {} clear refused — inventory cannot take the reward",
                    quest.unique
                );
                return Ok(QuestOutcome::RewardRefused);
            }
            granted => granted,
        };
        unit.set_clear(quest.bit);
        unit.clear_going();
        quests
            .save(actor.character_id, quest.quest_type, unit)
            .await?;
        logln!(
            "game: quest {} (type {}, bit {}) cleared",
            quest.unique,
            quest.quest_type,
            quest.bit
        );
        return Ok(QuestOutcome::Cleared { reward });
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
    storage: &dyn RewardStorage,
    npc_price_rate: i32,
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
    // Same order as the 5700 clear: pay, or refuse the clear (`Server-Person.cpp`
    // reaches the reward block from both entry points).
    let plan = RewardPlan::for_quest(quest, npc_price_rate);
    let reward = match grant_reward(storage, actor, &plan).await? {
        RewardGrant::InventoryFull => {
            logln!(
                "game: quest {} status clear refused — inventory cannot take the reward",
                quest.unique
            );
            return Ok(QuestOutcome::RewardRefused);
        }
        granted => granted,
    };
    unit.set_clear(quest.bit);
    unit.clear_going();
    quests
        .save(actor.character_id, quest.quest_type, unit)
        .await?;
    logln!(
        "game: quest {} (type {}, bit {}) cleared by the client's report",
        quest.unique,
        quest.quest_type,
        quest.bit
    );
    Ok(QuestOutcome::Cleared { reward })
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
    let body = state.encode();
    // Logged because the block is the client's *only* source for what it shows in
    // the quest windows: when a player reports a quest button doing nothing, the
    // first question is whether a 225 left this session and when.
    logln!("game: pushed 225 quest block ({} bytes)", body.len());
    send_encoded(stream, cipher, GameMsg::ResponsePersonQuest, &body).await
}

/// 5700 as a handler: decode, apply, push.
///
/// # Errors
///
/// A malformed body is a logged no-op, not an error; socket and repository
/// failures propagate.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_set_quest_going(
    stream: &mut TcpStream,
    cipher: &mut Cipher,
    quests: &dyn QuestRepository,
    slots: &dyn SlotOccupancy,
    items: &dyn ItemRepository,
    characters: &dyn CharacterRepository,
    npc_price_rate: i32,
    actor: &QuestActor,
    state: &mut PersonQuest,
    payload: &[u8],
) -> Result<(), AnyError> {
    let Ok(request) = decode_quest_going(payload) else {
        logln!("game: malformed 5700 ({} bytes) — ignored", payload.len());
        return Ok(());
    };
    let store = QuestRewardStore { items, characters };
    match apply_quest_going(quests, slots, &store, npc_price_rate, actor, state, request).await? {
        QuestOutcome::Cleared { reward } => {
            push_quest_state(stream, cipher, state).await?;
            announce_reward(stream, cipher, &reward).await?;
        }
        QuestOutcome::RewardRefused => announce_reward_refused(stream, cipher).await?,
        QuestOutcome::Changed | QuestOutcome::Resync => {
            push_quest_state(stream, cipher, state).await?
        }
        QuestOutcome::NoChange => {}
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
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_quest_status(
    stream: &mut TcpStream,
    cipher: &mut Cipher,
    quests: &dyn QuestRepository,
    slots: &dyn SlotOccupancy,
    items: &dyn ItemRepository,
    characters: &dyn CharacterRepository,
    npc_price_rate: i32,
    actor: &QuestActor,
    state: &mut PersonQuest,
    payload: &[u8],
) -> Result<(), AnyError> {
    let Ok(request) = decode_quest_status(payload) else {
        logln!("game: malformed 5702 ({} bytes) — ignored", payload.len());
        return Ok(());
    };
    let store = QuestRewardStore { items, characters };
    match apply_quest_status(quests, slots, &store, npc_price_rate, actor, state, request).await? {
        QuestOutcome::Cleared { reward } => {
            push_quest_state(stream, cipher, state).await?;
            announce_reward(stream, cipher, &reward).await?;
        }
        QuestOutcome::RewardRefused => announce_reward_refused(stream, cipher).await?,
        QuestOutcome::Changed | QuestOutcome::Resync => {
            push_quest_state(stream, cipher, state).await?
        }
        QuestOutcome::NoChange => {}
    }
    Ok(())
}

/// Tells the acting session what a reward did: the slot the item landed in, with
/// its *new* contents.
///
/// This is the frame the legacy's DB layer answers a grant with
/// (`ServerDB-Item.cpp:1374-1420`, `GAMEMSG_RESPONSE_PERSONITEMINFO`), and it is
/// what makes a quest reward appear without a relog.
///
/// # Errors
///
/// Propagates socket failures and an unreadable stored record.
async fn announce_reward(
    stream: &mut TcpStream,
    cipher: &mut Cipher,
    reward: &RewardGrant,
) -> Result<(), AnyError> {
    let RewardGrant::Granted { slot, .. } = reward else {
        return Ok(());
    };
    let Some(row) = slot else {
        return Ok(());
    };
    let item = Item::decode(&row.raw).map_err(|e| {
        format!(
            "reward slot {} holds an unreadable {}-byte record: {e}",
            row.slot,
            row.raw.len()
        )
    })?;
    send_encoded(
        stream,
        cipher,
        GameMsg::ResponsePersoniteminfo,
        &encode_person_item_info(row.slot, &item),
    )
    .await
}

/// Tells the client a reward was withheld because there is no room for it.
///
/// The legacy sends `SRVAL_QUEST_FAIL_FULLINV` (`Server-Person.cpp:1290`), a
/// server-response *id* the client localises. This port has no SRVAL table, so the
/// text travels in a notice instead — same dialog, no shared id space.
///
/// # Errors
///
/// Propagates socket failures.
async fn announce_reward_refused(
    stream: &mut TcpStream,
    cipher: &mut Cipher,
) -> Result<(), AnyError> {
    send_encoded(
        stream,
        cipher,
        GameMsg::ResponseNotice,
        &gz_proto::chat::encode_notice(
            "Quest",
            "Your inventory is too full to receive the reward.",
            true,
        ),
    )
    .await
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

    /// A reward store that records what it was asked to hand over, and can pretend
    /// the inventory is full.
    struct RewardFake {
        calls: Mutex<Vec<String>>,
        /// `Some(slot)` to accept the item, `None` for `GIVEITEM_RESULT_NOINVEN`.
        room: Option<u16>,
    }

    impl Default for RewardFake {
        /// Room in slot 15 — `INV0`, the first carry slot a character holding
        /// nothing has free.
        fn default() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                room: Some(15),
            }
        }
    }

    impl RewardFake {
        fn full() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                room: None,
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl RewardStorage for RewardFake {
        async fn store_item(
            &self,
            character_id: i32,
            item_unique: u16,
            count: i32,
        ) -> Result<Option<gz_db::CharacterItem>, RepositoryError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("item {character_id} {item_unique} x{count}"));
            Ok(self.room.map(|slot| gz_db::CharacterItem {
                slot,
                container: gz_proto::item::container_of(slot),
                item_unique: i32::from(item_unique),
                count,
                raw: vec![0; gz_proto::item::CLT_ITEM_SIZE],
            }))
        }

        async fn add_money(&self, character_id: i32, amount: i64) -> Result<(), RepositoryError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("money {character_id} {amount}"));
            Ok(())
        }
    }

    /// True when the outcome is a clear that carried a grant.
    fn cleared(outcome: &QuestOutcome) -> bool {
        matches!(outcome, QuestOutcome::Cleared { .. })
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
        rewards: &RewardFake,
        actor: &QuestActor,
        state: &mut PersonQuest,
        unique: i16,
    ) -> QuestOutcome {
        apply_quest_going(
            quests,
            slots,
            rewards,
            100,
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
        rewards: &RewardFake,
        actor: &QuestActor,
        state: &mut PersonQuest,
        unique: i16,
    ) -> QuestOutcome {
        let quest = chain_quest(14, unique).expect("modelled quest");
        apply_quest_status(
            quests,
            slots,
            rewards,
            100,
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
        let rewards = RewardFake::default();
        let slots = FixedSlots(vec![7]); // the JP Straw Shoes, equipped
        let actor = newbie(0); // every bonus point spent
        let mut state = PersonQuest::new();

        for unique in [30000, 30003, 30005] {
            assert_eq!(
                accept(&quests, &slots, &rewards, &actor, &mut state, unique).await,
                QuestOutcome::Changed,
                "quest {unique} starts"
            );
            assert!(
                cleared(&report(&quests, &slots, &rewards, &actor, &mut state, unique).await),
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
            accept(&quests, &slots, &rewards, &actor, &mut state, 30006).await,
            QuestOutcome::Changed
        );
        assert!(cleared(
            &report(&quests, &slots, &rewards, &actor, &mut state, 30006).await
        ));
        assert!(state.unit(14).unwrap().is_clear(3));

        // Every change wrote exactly its own type — the legacy's per-type update
        // (`Server-Person.cpp:1905`).
        assert_eq!(quests.saved_types(), vec![14]);
    }

    #[tokio::test]
    async fn a_quest_cannot_start_before_its_predecessor_is_cleared() {
        let quests = MemoryQuests::default();
        let rewards = RewardFake::default();
        let slots = FixedSlots(vec![7]);
        let actor = newbie(0);
        let mut state = PersonQuest::new();

        // 30005 needs 30003, which needs 30000 — nothing is cleared yet.
        assert_eq!(
            accept(&quests, &slots, &rewards, &actor, &mut state, 30005).await,
            QuestOutcome::NoChange
        );
        assert_eq!(state.unit(14).unwrap().cur_unique, 0);
        assert!(quests.saved_types().is_empty(), "a refusal writes nothing");
    }

    #[tokio::test]
    async fn bonuszero_refuses_while_points_are_unspent() {
        let quests = MemoryQuests::default();
        let rewards = RewardFake::default();
        let slots = FixedSlots(vec![]);
        let mut state = PersonQuest::new();

        // Start 30000 and clear it, then start 30003.
        let unspent = newbie(5);
        accept(&quests, &slots, &rewards, &unspent, &mut state, 30000).await;
        report(&quests, &slots, &rewards, &unspent, &mut state, 30000).await;
        accept(&quests, &slots, &rewards, &unspent, &mut state, 30003).await;

        // With 5 points left the condition is not met (`Char-Quest.cpp:104-110`).
        assert_eq!(
            report(&quests, &slots, &rewards, &unspent, &mut state, 30003).await,
            QuestOutcome::NoChange
        );
        assert!(!state.unit(14).unwrap().is_clear(1));

        // The same report after the points are spent clears it.
        let spent = newbie(0);
        assert!(cleared(
            &report(&quests, &slots, &rewards, &spent, &mut state, 30003).await
        ));
        assert!(state.unit(14).unwrap().is_clear(1));
    }

    #[tokio::test]
    async fn equippos_refuses_an_empty_slot() {
        let quests = MemoryQuests::default();
        let rewards = RewardFake::default();
        let actor = newbie(0);
        let mut state = PersonQuest::new();
        let empty = FixedSlots(vec![]);
        let equipped = FixedSlots(vec![7]);

        for unique in [30000, 30003] {
            accept(&quests, &equipped, &rewards, &actor, &mut state, unique).await;
            report(&quests, &equipped, &rewards, &actor, &mut state, unique).await;
        }
        accept(&quests, &equipped, &rewards, &actor, &mut state, 30005).await;

        // Slot 7 empty: `clItem[7].siUnique == 0` (`Char-Quest.cpp:360-368`).
        assert_eq!(
            report(&quests, &empty, &rewards, &actor, &mut state, 30005).await,
            QuestOutcome::NoChange
        );
        assert!(!state.unit(14).unwrap().is_clear(2));

        assert!(cleared(
            &report(&quests, &equipped, &rewards, &actor, &mut state, 30005).await
        ));
        assert!(state.unit(14).unwrap().is_clear(2));
    }

    #[tokio::test]
    async fn an_unmodelled_or_mismatched_request_is_a_logged_no_op() {
        let quests = MemoryQuests::default();
        let rewards = RewardFake::default();
        let slots = FixedSlots(vec![7]);
        let actor = newbie(0);
        let mut state = PersonQuest::new();

        // A quest this port does not model.
        assert_eq!(
            accept(&quests, &slots, &rewards, &actor, &mut state, 30015).await,
            QuestOutcome::NoChange
        );
        // A report whose condition is not the one in progress.
        accept(&quests, &slots, &rewards, &actor, &mut state, 30000).await;
        assert_eq!(
            apply_quest_status(
                &quests,
                &slots,
                &rewards,
                100,
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
            report(&quests, &slots, &rewards, &actor, &mut state, 30003).await,
            QuestOutcome::NoChange
        );
        assert!(!state.unit(14).unwrap().is_clear(0));
        assert!(!state.unit(14).unwrap().is_clear(1));
    }

    #[tokio::test]
    async fn cancelling_clears_the_going_fields_and_not_the_bits() {
        let quests = MemoryQuests::default();
        let rewards = RewardFake::default();
        let slots = FixedSlots(vec![7]);
        let actor = newbie(0);
        let mut state = PersonQuest::new();

        accept(&quests, &slots, &rewards, &actor, &mut state, 30000).await;
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

    /// The acceptance test of T-019's reward work: clearing a quest hands over the
    /// item its own quest file names, then the money — the thing the client needs
    /// before it can equip the shoes and walk out.
    #[tokio::test]
    async fn a_clear_grants_the_quest_files_item_and_money() {
        let quests = MemoryQuests::default();
        let rewards = RewardFake {
            room: Some(16),
            ..Default::default()
        };
        let slots = FixedSlots(vec![]);
        let actor = newbie(0);
        let mut state = PersonQuest::new();

        accept(&quests, &slots, &rewards, &actor, &mut state, 30000).await;
        let outcome = report(&quests, &slots, &rewards, &actor, &mut state, 30000).await;

        // 30000's reward row: item 1500 x1, money 100 (`30000E.txt`).
        match outcome {
            QuestOutcome::Cleared {
                reward: RewardGrant::Granted { slot },
            } => {
                let row = slot.expect("30000 gives an item");
                assert_eq!(row.slot, 16, "the slot the store reported");
                assert_eq!(row.item_unique, 1500);
            }
            other => panic!("expected a paid clear, got {other:?}"),
        }
        assert_eq!(
            rewards.calls(),
            vec!["item 42 1500 x1".to_string(), "money 42 100".to_string()]
        );
        assert!(state.unit(14).unwrap().is_clear(0), "and the bit is set");
    }

    /// A full inventory must leave the quest in progress rather than clear it into
    /// nothing: `Server-Person.cpp:1287-1296` refuses the clear, so the reward is
    /// still owed when there is room again.
    #[tokio::test]
    async fn a_full_inventory_refuses_the_clear_and_keeps_the_quest_going() {
        let quests = MemoryQuests::default();
        let rewards = RewardFake::full();
        let slots = FixedSlots(vec![]);
        let actor = newbie(0);
        let mut state = PersonQuest::new();

        accept(&quests, &slots, &rewards, &actor, &mut state, 30000).await;
        let outcome = report(&quests, &slots, &rewards, &actor, &mut state, 30000).await;

        assert_eq!(outcome, QuestOutcome::RewardRefused);
        let unit = state.unit(14).unwrap();
        assert!(!unit.is_clear(0), "the quest must not clear unpaid");
        assert_eq!(unit.cur_unique, 30000, "it is still in progress");
        // Nothing was written and no money was paid.
        assert_eq!(rewards.calls(), vec!["item 42 1500 x1".to_string()]);
    }

    /// The live failure of 2026-10-13, as a test: the character already has 30000
    /// cleared, the client's own copy says it is not, so the client asks to start
    /// it again. That must produce a resync (push 225) rather than the silent no-op
    /// that left the player clicking a dead button.
    #[tokio::test]
    async fn a_going_request_for_a_cleared_quest_resyncs_instead_of_going_silent() {
        let quests = MemoryQuests::default();
        let rewards = RewardFake::default();
        let slots = FixedSlots(vec![7]);
        let actor = newbie(0);
        let mut state = PersonQuest::new();

        accept(&quests, &slots, &rewards, &actor, &mut state, 30000).await;
        assert!(cleared(
            &report(&quests, &slots, &rewards, &actor, &mut state, 30000).await
        ));

        let outcome = accept(&quests, &slots, &rewards, &actor, &mut state, 30000).await;
        assert_eq!(outcome, QuestOutcome::Resync);
        // The refusal persists nothing and moves nothing: it only re-asserts the
        // block the client should already have.
        let unit = state.unit(14).unwrap();
        assert!(unit.is_clear(0));
        assert_eq!(unit.cur_unique, 0);
    }
}
