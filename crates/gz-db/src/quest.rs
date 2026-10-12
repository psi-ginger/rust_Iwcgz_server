//! Quest-state repository (`character_quests`) — one row per character × quest
//! type, written per type like the legacy's `rsp_QuestInfoSet`
//! (`Server-Person.cpp:1905`, `DBMsgQuest.cpp:8-33`).
//!
//! The stored columns are the six fields of `cltPersonQuestUnitInfo`
//! (`Quest.h:137-161`); the wire layout and its pinning evidence live in
//! [`gz_proto::quest`]. A character with no row for a type has an all-zero unit
//! for it, which is why [`QuestRepository::load`] returns only the rows that
//! exist and the game service starts from a zeroed block.

use async_trait::async_trait;
use gz_proto::quest::QuestUnit;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

use crate::repository::RepositoryError;

/// One stored quest type, as loaded for its owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoredQuestType {
    /// `character_quests.quest_type` — a `QUEST_TYPE_*` index.
    pub quest_type: i16,
    /// The unit's six fields.
    pub unit: QuestUnit,
}

/// The quest-state storage interface.
#[async_trait]
pub trait QuestRepository: Send + Sync {
    /// Every quest type this character has a row for, in `quest_type` order.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Database`] if the query fails.
    async fn load(&self, character_id: i32) -> Result<Vec<StoredQuestType>, RepositoryError>;

    /// Writes one type's unit, inserting the row if it does not exist yet.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Database`] if the upsert fails.
    async fn save(
        &self,
        character_id: i32,
        quest_type: i16,
        unit: &QuestUnit,
    ) -> Result<(), RepositoryError>;
}

/// PostgreSQL implementation. Cheap to clone (shares the pool).
#[derive(Clone)]
pub struct PgQuestRepository {
    pool: PgPool,
}

impl PgQuestRepository {
    /// Wraps an existing pool (e.g. one shared with the character repository).
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Opens a fresh pool for `database_url`.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Database`] if the pool cannot connect.
    pub async fn connect(database_url: &str) -> Result<Self, RepositoryError> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await?;
        Ok(Self { pool })
    }
}

/// The columns a row maps to, in the order every query below selects them.
type QuestRow = (i16, i32, i16, i16, i16, i16, i16);

fn unit_from_row(row: QuestRow) -> StoredQuestType {
    let (quest_type, state, cur_unique, cur_step, cmd, para1, para2) = row;
    StoredQuestType {
        quest_type,
        unit: QuestUnit {
            state,
            cur_unique,
            cur_step,
            cmd,
            para1,
            para2,
        },
    }
}

#[async_trait]
impl QuestRepository for PgQuestRepository {
    async fn load(&self, character_id: i32) -> Result<Vec<StoredQuestType>, RepositoryError> {
        let rows = sqlx::query_as::<_, QuestRow>(
            "SELECT quest_type, state, cur_unique, cur_step, cur_cmd, para1, para2 \
             FROM character_quests \
             WHERE character_id = $1 \
             ORDER BY quest_type",
        )
        .bind(character_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(unit_from_row).collect())
    }

    async fn save(
        &self,
        character_id: i32,
        quest_type: i16,
        unit: &QuestUnit,
    ) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO character_quests \
                 (character_id, quest_type, state, cur_unique, cur_step, cur_cmd, para1, para2) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             ON CONFLICT (character_id, quest_type) DO UPDATE SET \
                 state = EXCLUDED.state, cur_unique = EXCLUDED.cur_unique, \
                 cur_step = EXCLUDED.cur_step, cur_cmd = EXCLUDED.cur_cmd, \
                 para1 = EXCLUDED.para1, para2 = EXCLUDED.para2, \
                 updated_at = now()",
        )
        .bind(character_id)
        .bind(quest_type)
        .bind(unit.state)
        .bind(unit.cur_unique)
        .bind(unit.cur_step)
        .bind(unit.cmd)
        .bind(unit.para1)
        .bind(unit.para2)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The row mapping is column order, and it has to survive the whole range of
    /// each field — a unit is a bitmask plus signed 16-bit values, and the
    /// in-progress unique of a real chain is nowhere near the low end.
    #[test]
    fn a_row_maps_to_the_unit_field_for_field() {
        let row: QuestRow = (14, -1, 30006, 20, 19, 0, 1);
        let stored = unit_from_row(row);
        assert_eq!(stored.quest_type, 14);
        assert_eq!(
            stored.unit,
            QuestUnit {
                state: -1,
                cur_unique: 30006,
                cur_step: 20,
                cmd: 19,
                para1: 0,
                para2: 1,
            }
        );
    }

    /// A cleared bitmask must round-trip a negative `SI32` unchanged: the client
    /// reads the same four bytes back, so masking the sign off here would drop
    /// bit 31 of a fully-cleared type.
    #[test]
    fn the_signed_state_is_not_masked_off() {
        let stored = unit_from_row((1, i32::MIN, 0, 0, 0, 0, 0));
        assert_eq!(stored.unit.state, i32::MIN);
        assert!(stored.unit.is_clear(31));
    }
}
