//! Character repository (legacy `tbl_Person` + `tbl_IP`), scoped to what the
//! login char-list path reads.
//!
//! M1 only needs enough to fill a `cltCharListInfo` per slot; the full person
//! record (inventory, skills, quests, position) is world-entry work and lives
//! elsewhere. The trait mirrors [`crate::repository::AccountRepository`] so the
//! game service can take either implementation.

use async_trait::async_trait;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

use crate::repository::RepositoryError;

/// The subset of a character row that a char-list packet carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterSummary {
    /// `cltPBasicInfo.clPerson.siPersonID`.
    pub character_id: i32,
    /// Slot the character occupies (`clBI.uiIndex`).
    pub slot: i16,
    /// `clBI.clPerson.szName[20]`.
    pub name: String,
    /// `clBI.uiKind`.
    pub class_kind: i16,
    /// `clBI.uiSex` (`1` male, `0` female).
    pub gender: i16,
    /// `cltPIPInfo.siLevel`.
    pub level: i16,
    /// `cltPIPInfo.Life`.
    pub max_life: i32,
    /// `cltPIPInfo.Mana`.
    pub max_mana: i32,
    /// `cltPIPInfo.siExp`.
    pub exp: i64,
}

/// The character fields world entry (`GAMEMSG_RESPONSE_LOGIN`'s
/// `clMakeCharInfo`) needs beyond the char-list summary: the save position and
/// home village. Kept separate so the 984-byte char-list path stays lean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterFull {
    /// `clPerson.siPersonID`.
    pub character_id: i32,
    /// `szName[20]`.
    pub name: String,
    /// `clBI.uiKind`.
    pub class_kind: i16,
    /// `clBI.uiSex` (`1` male, `0` female).
    pub gender: i16,
    /// `cltPIPInfo.siLevel`.
    pub level: i16,
    /// `cltPIPInfo.Life`.
    pub max_life: i32,
    /// `cltPIPInfo.Mana`.
    pub max_mana: i32,
    /// `cltPIPInfo.siExp`.
    pub exp: i64,
    /// Last saved map (the `MapData` index sent as `uiMapIndex`).
    pub map_id: i32,
    /// Last saved tile x (`uiX`, 10 bits).
    pub pos_x: i32,
    /// Last saved tile y (`uiY`, 10 bits).
    pub pos_y: i32,
    /// Home village (`siHomeVillageUnique`).
    pub school_id: i16,
}

/// The character storage interface.
#[async_trait]
pub trait CharacterRepository: Send + Sync {
    /// Returns every live character for `account_id`, ordered by slot.
    async fn list_for_account(
        &self,
        account_id: i32,
    ) -> Result<Vec<CharacterSummary>, RepositoryError>;

    /// Returns the live character occupying `slot`, for world entry.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Database`] if the query fails.
    async fn find_by_slot(
        &self,
        account_id: i32,
        slot: i16,
    ) -> Result<Option<CharacterFull>, RepositoryError>;
}

/// PostgreSQL implementation. Cheap to clone (shares the pool).
#[derive(Clone)]
pub struct PgCharacterRepository {
    pool: PgPool,
}

impl PgCharacterRepository {
    /// Wraps an existing pool (e.g. one shared with the account repository).
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

    /// The underlying pool, for callers that need to share it.
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

#[async_trait]
impl CharacterRepository for PgCharacterRepository {
    async fn list_for_account(
        &self,
        account_id: i32,
    ) -> Result<Vec<CharacterSummary>, RepositoryError> {
        type Row = (i32, i16, String, i16, i16, i16, i32, i32, i64);
        let rows = sqlx::query_as::<_, Row>(
            "SELECT c.character_id, c.slot, c.name, c.class_kind, c.gender, \
                    s.level, s.max_life, s.max_mana, s.exp \
             FROM characters c \
             JOIN character_stats s ON s.character_id = c.character_id \
             WHERE c.account_id = $1 AND c.deleted_at IS NULL \
             ORDER BY c.slot",
        )
        .bind(account_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(
                |(character_id, slot, name, class_kind, gender, level, max_life, max_mana, exp)| {
                    CharacterSummary {
                        character_id,
                        slot,
                        name,
                        class_kind,
                        gender,
                        level,
                        max_life,
                        max_mana,
                        exp,
                    }
                },
            )
            .collect())
    }

    async fn find_by_slot(
        &self,
        account_id: i32,
        slot: i16,
    ) -> Result<Option<CharacterFull>, RepositoryError> {
        type Row = (
            i32,
            String,
            i16,
            i16,
            i16,
            i32,
            i32,
            i64,
            i32,
            i32,
            i32,
            i16,
        );
        let row = sqlx::query_as::<_, Row>(
            "SELECT c.character_id, c.name, c.class_kind, c.gender, s.level, \
                    s.max_life, s.max_mana, s.exp, c.map_id, c.pos_x, c.pos_y, c.school_id \
             FROM characters c \
             JOIN character_stats s ON s.character_id = c.character_id \
             WHERE c.account_id = $1 AND c.slot = $2 AND c.deleted_at IS NULL",
        )
        .bind(account_id)
        .bind(slot)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(
            |(
                character_id,
                name,
                class_kind,
                gender,
                level,
                max_life,
                max_mana,
                exp,
                map_id,
                pos_x,
                pos_y,
                school_id,
            )| CharacterFull {
                character_id,
                name,
                class_kind,
                gender,
                level,
                max_life,
                max_mana,
                exp,
                map_id,
                pos_x,
                pos_y,
                school_id,
            },
        ))
    }
}
