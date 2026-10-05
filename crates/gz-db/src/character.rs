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
    /// The id the client knows this character by (`MyCharUnique`), or `None`
    /// until it has been taken into a world for the first time.
    pub game_unique: Option<i32>,
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

    /// Returns the live character with this exact name.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Database`] if the query fails.
    async fn find_by_name(&self, name: &str) -> Result<Option<CharacterFull>, RepositoryError>;

    /// Create a new character row for `account_id`.
    ///
    /// `name`, `class_kind`, `gender`, and `slot` come straight from the client's
    /// `GAMEMSG_REQUEST_MAKENEWCHAR` (opcode 7). Returns the generated
    /// `characters.character_id`.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Database`] if the insert fails.
    async fn create_character(
        &self,
        account_id: i32,
        name: &str,
        class_kind: i16,
        gender: i16,
        slot: i16,
    ) -> Result<i32, RepositoryError>;

    /// Permanently remove the character in `slot`, cascading to child tables.
    ///
    /// Used by `GAMEMSG_REQUEST_DELUSERCHAR` (opcode 9), the final confirmation
    /// step in the character-deletion dialog.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Database`] if the query fails.
    async fn delete_character(
        &self,
        account_id: i32,
        slot: i16,
    ) -> Result<Option<i32>, RepositoryError>;

    /// Set or clear the grace-period marker (`characters.deleted_at`).
    ///
    /// This is `GAMEMSG_REQUEST_PREDELUSERCHAR` (opcode 83) semantics in the
    /// legacy: `predeltype = 1` marks the character for deletion, and `0`
    /// restores it. Returns the affected character id when a row was updated.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Database`] if the query fails.
    async fn set_delete_pending(
        &self,
        account_id: i32,
        slot: i16,
        pending: bool,
    ) -> Result<Option<i32>, RepositoryError>;

    /// Returns `character_id`'s `game_unique`, allocating and persisting one the
    /// first time the character enters a world.
    ///
    /// Allocation goes through the `character_game_unique_seq` sequence rather
    /// than `max(game_unique) + 1`, so two simultaneous world entries cannot
    /// pick the same id.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Database`] if the query fails or the sequence is
    /// exhausted.
    async fn resolve_game_unique(&self, character_id: i32) -> Result<i32, RepositoryError>;
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

/// The 13 columns every `CharacterFull` needs, in the order [`FULL_SELECT`]
/// lists them.
type FullRow = (
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
    Option<i32>,
);

const FULL_SELECT: &str = "SELECT c.character_id, c.name, c.class_kind, c.gender, s.level, \
     s.max_life, s.max_mana, s.exp, c.map_id, c.pos_x, c.pos_y, c.school_id, c.game_unique \
     FROM characters c JOIN character_stats s ON s.character_id = c.character_id";

impl From<FullRow> for CharacterFull {
    fn from(
        (
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
            game_unique,
        ): FullRow,
    ) -> Self {
        Self {
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
            game_unique,
        }
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
        let row = sqlx::query_as::<_, FullRow>(&format!(
            "{FULL_SELECT} WHERE c.account_id = $1 AND c.slot = $2 AND c.deleted_at IS NULL"
        ))
        .bind(account_id)
        .bind(slot)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(CharacterFull::from))
    }

    async fn find_by_name(&self, name: &str) -> Result<Option<CharacterFull>, RepositoryError> {
        let row = sqlx::query_as::<_, FullRow>(&format!("{FULL_SELECT} WHERE c.name = $1"))
            .bind(name)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(CharacterFull::from))
    }

    async fn create_character(
        &self,
        account_id: i32,
        name: &str,
        class_kind: i16,
        gender: i16,
        slot: i16,
    ) -> Result<i32, RepositoryError> {
        let character_id: i32 = sqlx::query_scalar(
            "INSERT INTO characters (account_id, slot, name, class_kind, gender) \
             VALUES ($1, $2, $3, $4, $5) \
             RETURNING character_id",
        )
        .bind(account_id)
        .bind(slot)
        .bind(name)
        .bind(class_kind)
        .bind(gender)
        .fetch_one(&self.pool)
        .await?;

        sqlx::query("INSERT INTO character_stats (character_id) VALUES ($1)")
            .bind(character_id)
            .execute(&self.pool)
            .await?;

        Ok(character_id)
    }

    async fn delete_character(
        &self,
        account_id: i32,
        slot: i16,
    ) -> Result<Option<i32>, RepositoryError> {
        let character_id: Option<i32> = sqlx::query_scalar(
            "DELETE FROM characters WHERE account_id = $1 AND slot = $2 \
             RETURNING character_id",
        )
        .bind(account_id)
        .bind(slot)
        .fetch_optional(&self.pool)
        .await?;

        Ok(character_id)
    }

    async fn set_delete_pending(
        &self,
        account_id: i32,
        slot: i16,
        pending: bool,
    ) -> Result<Option<i32>, RepositoryError> {
        let character_id: Option<i32> = sqlx::query_scalar(
            "UPDATE characters SET deleted_at = CASE WHEN $3 THEN now() ELSE NULL END \
             WHERE account_id = $1 AND slot = $2 \
             RETURNING character_id",
        )
        .bind(account_id)
        .bind(slot)
        .bind(pending)
        .fetch_optional(&self.pool)
        .await?;

        Ok(character_id)
    }

    async fn resolve_game_unique(&self, character_id: i32) -> Result<i32, RepositoryError> {
        let existing = sqlx::query_scalar::<_, Option<i32>>(
            "SELECT game_unique FROM characters WHERE character_id = $1",
        )
        .bind(character_id)
        .fetch_optional(&self.pool)
        .await?;
        if let Some(Some(existing)) = existing {
            return Ok(existing);
        }

        // Single statement so a concurrent world entry cannot overwrite a value
        // another connection has just written: the UPDATE only fires while the
        // column is still NULL.
        let allocated = sqlx::query_scalar::<_, i32>(
            "UPDATE characters \
             SET game_unique = nextval('character_game_unique_seq') \
             WHERE character_id = $1 AND game_unique IS NULL \
             RETURNING game_unique",
        )
        .bind(character_id)
        .fetch_optional(&self.pool)
        .await?;

        match allocated {
            Some(id) => Ok(id),
            // Lost the race: the winner's id is now stored, so re-read it.
            None => sqlx::query_scalar::<_, i32>(
                "SELECT game_unique FROM characters WHERE character_id = $1",
            )
            .bind(character_id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(RepositoryError::NotFound),
        }
    }
}
