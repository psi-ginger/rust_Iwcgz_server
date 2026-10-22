//! Quest rewards — what a cleared quest actually hands over.
//!
//! # The legacy's clear path
//!
//! `Server-Person.cpp:1247-1461`, in order:
//!
//! 1. **capacity check** — `CanAddInvManyItems(INVMODE_ALL, …)`; when the inventory
//!    cannot take the reward the player is told `SRVAL_QUEST_FAIL_FULLINV` and the
//!    quest is **not** cleared (`:1287-1296`), so a reward can never be lost;
//! 2. **the items** — one `GiveItem` per reward row (`:1310-1371`), which builds the
//!    item, finds its slot through `CanAddInv` and writes it (`Char-Item.cpp:8898`);
//! 3. **money** — `sDBRequest_ChangeMoney(…, CHANGE_MONEY_REASON_QUESTREWARD, …)`
//!    (`:1436`), scaled by the NPC price rate when the reward row asks for it
//!    (`:1427-1431`);
//! 4. exp, skill, fame, NCoin and the move-speed buff (`:1452-1530`) — **not
//!    modelled**: this port has no exp or level path at all (T-018 slice 6 C is
//!    deferred). They are logged here rather than silently dropped.
//!
//! # The port's shape
//!
//! [`RewardStorage`] is the *only* thing this domain needs from storage, and
//! [`QuestRewardStore`] is the adapter the service wires to its two repositories.
//! The reward logic itself never sees SQL, a pool or a socket, which is what lets
//! it be tested against a two-method fake.

use async_trait::async_trait;
use gz_db::{CharacterItem, CharacterRepository, ItemRepository, RepositoryError};

use super::QuestActor;
use super::chain::ChainQuest;
use crate::{AnyError, logln};

/// The parts of a reward this port models, read off the chain table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RewardPlan {
    /// Money, already scaled by the NPC price rate.
    pub(crate) money: i64,
    /// The reward item and how many of it, if the quest has one.
    pub(crate) item: Option<(u16, i32)>,
    /// Exp the legacy would grant — logged, never granted.
    pub(crate) exp: i32,
}

impl RewardPlan {
    /// The plan for `chain`.
    ///
    /// `npc_price_rate` is `clItemStatistic.siNPCPriceRate` as a percentage
    /// (`Server-Person.cpp:1429`). This port has no item-statistics table, so the
    /// service passes the rate it advertises in SERVERINFO — 100, i.e. the identity,
    /// until an operator changes it.
    #[must_use]
    pub(crate) fn for_quest(chain: &ChainQuest, npc_price_rate: i32) -> Self {
        let money = if chain.reward_money == 0 {
            0
        } else {
            chain.reward_money * i64::from(npc_price_rate) / 100
        };
        let item = if chain.reward_item > 0 && chain.reward_item_num > 0 {
            Some((chain.reward_item as u16, i32::from(chain.reward_item_num)))
        } else {
            None
        };
        Self {
            money,
            item,
            exp: chain.reward_exp,
        }
    }
}

/// What granting a reward did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RewardGrant {
    /// The item landed in `slot` — its *resulting* contents, so a topped-up stack
    /// reports the whole stack. Money was added too when the plan asked for it.
    Granted {
        /// The slot row as it now stands, for the `PERSONITEMINFO` frame.
        slot: Option<CharacterItem>,
    },
    /// `GIVEITEM_RESULT_NOINVEN`: nothing was written, and the caller must refuse the
    /// clear rather than clear a quest it cannot pay for.
    InventoryFull,
}

/// What the reward domain needs from storage.
#[async_trait]
pub(crate) trait RewardStorage: Send + Sync {
    /// Stores `count` of `item_unique`, returning the slot row it landed in, or
    /// `None` when there is no room.
    ///
    /// # Errors
    ///
    /// [`RepositoryError`] if the read or write fails.
    async fn store_item(
        &self,
        character_id: i32,
        item_unique: u16,
        count: i32,
    ) -> Result<Option<CharacterItem>, RepositoryError>;

    /// Adds `amount` to the character's purse.
    ///
    /// # Errors
    ///
    /// [`RepositoryError`] if the update fails.
    async fn add_money(&self, character_id: i32, amount: i64) -> Result<(), RepositoryError>;
}

/// The service's adapter: the item and character repositories behind
/// [`RewardStorage`].
pub(crate) struct QuestRewardStore<'a> {
    pub(crate) items: &'a dyn ItemRepository,
    pub(crate) characters: &'a dyn CharacterRepository,
}

#[async_trait]
impl RewardStorage for QuestRewardStore<'_> {
    async fn store_item(
        &self,
        character_id: i32,
        item_unique: u16,
        count: i32,
    ) -> Result<Option<CharacterItem>, RepositoryError> {
        // `store_item` reports the row — the quest counts on the item having
        // landed, not on how much of it did.
        //
        // Placement goes through `items.grant` (-> `grant_slot`), which scans
        // `new_slot_range()` = `INV0..=INV_END` = `15..=34` for the first free
        // slot and merges pileable stacks in ascending slot order. There is no
        // hardcoded slot here: an empty inventory's first reward lands at slot 15
        // (top-left), matching the universal inventory rule. See
        // `gz-db`'s `a_grant_lands_in_the_top_left_slot_of_an_empty_bag`.
        Ok(self
            .items
            .grant(character_id, item_unique, count)
            .await?
            .map(|granted| granted.item))
    }

    async fn add_money(&self, character_id: i32, amount: i64) -> Result<(), RepositoryError> {
        self.characters.add_money(character_id, amount).await
    }
}

/// Grants `plan` to `actor`, in the legacy's order: the item first, and only if it
/// found a slot the money.
///
/// A full inventory returns [`RewardGrant::InventoryFull`] having written *nothing*,
/// which is what makes it safe for the caller to leave the quest in progress
/// (`Server-Person.cpp:1287-1296`).
///
/// # Errors
///
/// [`AnyError`] if storage fails. The caller treats that as a failure of the request,
/// not as a refused reward.
pub(crate) async fn grant_reward(
    storage: &dyn RewardStorage,
    actor: &QuestActor,
    plan: &RewardPlan,
) -> Result<RewardGrant, AnyError> {
    let slot = match plan.item {
        Some((unique, count)) => match storage
            .store_item(actor.character_id, unique, count)
            .await?
        {
            Some(row) => {
                logln!(
                    "game: quest reward: item {unique} x{count} -> slot {} (now {} item(s))",
                    row.slot,
                    row.count
                );
                Some(row)
            }
            None => {
                logln!("game: quest reward refused: no room for item {unique} x{count}");
                return Ok(RewardGrant::InventoryFull);
            }
        },
        None => None,
    };

    if plan.money != 0 {
        storage.add_money(actor.character_id, plan.money).await?;
        logln!("game: quest reward: {} money", plan.money);
    }

    if plan.exp != 0 {
        logln!(
            "game: quest reward: {} exp NOT granted (this port has no exp/level path; T-018 slice 6 C)",
            plan.exp
        );
    }

    Ok(RewardGrant::Granted { slot })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quest::chain::BEGINNER2_CHAIN;
    use std::sync::Mutex;

    /// Records what the reward asked of storage, and answers with the slots it is
    /// told to.
    #[derive(Default)]
    struct MemoryRewardStorage {
        calls: Mutex<Vec<String>>,
        /// What `store_item` answers: `Some(slot)` or `None` for a full inventory.
        room: Option<u16>,
    }

    #[async_trait]
    impl RewardStorage for MemoryRewardStorage {
        async fn store_item(
            &self,
            character_id: i32,
            item_unique: u16,
            count: i32,
        ) -> Result<Option<CharacterItem>, RepositoryError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("item {character_id} {item_unique} x{count}"));
            Ok(self.room.map(|slot| CharacterItem {
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

    fn chain(unique: i16) -> &'static ChainQuest {
        BEGINNER2_CHAIN
            .iter()
            .find(|q| q.unique == unique)
            .expect("the chain has this quest")
    }

    fn actor() -> QuestActor {
        QuestActor {
            level: 5,
            bonus_points: 0,
            character_id: 21,
        }
    }

    /// The plan reads the reward rows of the client's own quest files: the four
    /// modelled quests give money, exp and one item each (`3000{N}E.txt`).
    #[test]
    fn a_plan_mirrors_the_quest_file_rewards() {
        // 30000: money 100, item 1500, one of it, exp 30.
        let plan = RewardPlan::for_quest(chain(30000), 100);
        assert_eq!(plan.money, 100);
        assert_eq!(plan.item, Some((1500, 1)));
        assert_eq!(plan.exp, 30);
        // The NPC price rate scales money only, and a rate of 0 pays nothing.
        assert_eq!(RewardPlan::for_quest(chain(30000), 50).money, 50);
        assert_eq!(RewardPlan::for_quest(chain(30000), 0).money, 0);
    }

    #[tokio::test]
    async fn a_grant_stores_the_item_then_pays_the_money() {
        let storage = MemoryRewardStorage {
            room: Some(16),
            ..Default::default()
        };
        let plan = RewardPlan::for_quest(chain(30005), 100);
        let grant = grant_reward(&storage, &actor(), &plan).await.unwrap();

        match grant {
            RewardGrant::Granted { slot } => {
                let row = slot.expect("the quest has an item");
                assert_eq!(row.slot, 16);
                assert_eq!(row.item_unique, i32::from(plan.item.unwrap().0));
            }
            RewardGrant::InventoryFull => panic!("there was room"),
        }
        // Item first, then money — the order the legacy grants in.
        assert_eq!(
            *storage.calls.lock().unwrap(),
            vec![
                format!("item 21 {} x{}", plan.item.unwrap().0, plan.item.unwrap().1),
                format!("money 21 {}", plan.money),
            ]
        );
    }

    /// A full inventory refuses the **whole** reward: no item, and no money either,
    /// so the caller can leave the quest in progress without half-paying it.
    #[tokio::test]
    async fn a_full_inventory_grants_nothing() {
        let storage = MemoryRewardStorage::default(); // room: None
        let plan = RewardPlan::for_quest(chain(30005), 100);
        let grant = grant_reward(&storage, &actor(), &plan).await.unwrap();

        assert!(matches!(grant, RewardGrant::InventoryFull));
        assert_eq!(
            *storage.calls.lock().unwrap(),
            vec![format!(
                "item 21 {} x{}",
                plan.item.unwrap().0,
                plan.item.unwrap().1
            )],
            "the money must not be paid when the item could not be stored"
        );
    }
}
