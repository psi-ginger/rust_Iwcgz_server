//! The modelled quest chains, as data.
//!
//! One entry per quest: which type and unique it is, which bit of the type's
//! `siQuestState` it clears, the condition the client has to satisfy, the chain
//! link the legacy enforces through `siMustClearQuestUnique`
//! (`Server-Person.cpp:1881-1888`), and what clearing it hands over (see
//! [`super::reward`]).

use gz_proto::quest::{
    QUEST_CLEAR_COND_BONUSZERO, QUEST_CLEAR_COND_EQUIPPOS, QUEST_CLEAR_COND_GOMAP,
    QUEST_CLEAR_COND_OPENCHARSTATUS,
};

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
        reward_item: 0,
        reward_item_num: 0,
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
        reward_item: 0,
        reward_item_num: 0,
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
pub(crate) fn chain_quest(quest_type: i16, unique: i16) -> Option<&'static ChainQuest> {
    BEGINNER2_CHAIN
        .iter()
        .find(|q| q.quest_type == quest_type && q.unique == unique)
}

#[cfg(test)]
mod tests {
    use super::*;

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
