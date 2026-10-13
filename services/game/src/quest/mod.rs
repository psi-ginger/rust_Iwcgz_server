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
//! # Scope
//!
//! * the four quests of Sarah Spencer's `BEGINNER2` chain that carry the
//!   conditions this port can evaluate: `OPENCHARSTATUS`, `BONUSZERO`,
//!   `EQUIPPOS`, `GOMAP` (`New Era Live\Quest\beginner2\3000{0,3,5,6}E.txt`);
//! * accept → in-progress → cleared, per type, persisted per type;
//! * **the reward**: clearing hands over the quest's own item and its money, or
//!   refuses the clear when the inventory cannot take the item — see [`reward`];
//! * 225 after every change, to the acting session only — the legacy gates it on
//!   `UPDATE_QUEST` and sends it to self (`Char-UpdateChangedData.cpp:247-253`).
//!
//! **Deviation — exp, skill, fame, NCoin and the move-speed buff are logged, not
//! granted.** This port has no exp or level path at all (T-018 slice 6 C is
//! explicitly deferred), so `grant_reward` reports what the legacy would have
//! granted and grants the rest. Items and money are not affected by that gap.
//!
//! **Policy — soft-fail.** An unknown quest, a condition the port does not
//! model, a request for a quest that is not in progress, or a failed check is a
//! logged no-op. The session is never closed; the legacy's `CloseSocket()` paths
//! are deliberately not ported (same policy as 250).
//!
//! # Layout
//!
//! The family is split by concern rather than by opcode: [`chain`] is the modelled
//! chain as data, [`reward`] is what clearing a quest hands over, and [`handlers`]
//! is everything that answers a request. [`QuestActor`] is the one model they
//! share.

mod chain;
mod handlers;
mod reward;

pub(crate) use chain::{BEGINNER2_CHAIN, ChainQuest};
#[allow(unused_imports)]
pub(crate) use handlers::{
    QuestOutcome, SlotOccupancy, apply_cancel_quest, apply_quest_going, apply_quest_status,
    handle_cancel_quest, handle_quest_status, handle_set_quest_going, push_quest_state,
};
#[allow(unused_imports)]
pub(crate) use reward::{QuestRewardStore, RewardGrant, RewardPlan, RewardStorage, grant_reward};

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
    pub(crate) fn from_character(character: &gz_db::CharacterFull) -> Self {
        Self {
            level: character.level,
            bonus_points: character.bonus_points,
            character_id: character.character_id,
        }
    }
}
