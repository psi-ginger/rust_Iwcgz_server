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

/// The map a new character starts on: `MAPINDEX_PALACE`, which is 158
/// (`map.h:69`).
///
/// The legacy's new-character path assigns it by name rather than by number
/// (`clbasicinfo.clPos.SetMapIndex(MAPINDEX_PALACE)`, `Server-Login.cpp:375`),
/// and the number is what reaches the client. The client's
/// `Data\mapinfoE.txt` row 173 names MapIndex 158 `Hanyang` — MapUnique 12,
/// which `Data\MapTypeInfo.txt:17` resolves to `MAPTYPE_SHURIPALACE1`. MapIndex 1
/// is the one the dev seed used to be, which `mapinfoE.txt` row 16 ties to
/// `VillageUnique 1`, and `Data\villageinfoE.txt` row 1 names village 1
/// `Ankaran` — which is why the town banner came up wrong.
pub const PALACE_MAP_INDEX: i32 = 158;

/// Where a new character stands: tile `(27, 87)`.
///
/// The legacy draws both axes from `rand()` — `rand()%7 + 24` and `rand()%23 + 68`
/// (`Server-Login.cpp:377-378`) — so its spawn is somewhere in X 24..30, Y
/// 68..90 on every character. `(27, 87)` is inside that box and is the fixed
/// palace point the legacy itself warps to
/// (`cltWarpInfo clWarpInfo(1, MAPINDEX_PALACE, false, false, 27, 87, -1, -1)`,
/// `NPartyDlg.cpp:533`).
///
/// Deviation from the oracle: deterministic where the legacy is not, so that
/// two logins of the same character agree and a capture-based test can name the
/// expected tile.
pub const PALACE_SPAWN_X: i32 = 27;

/// See [`PALACE_SPAWN_X`].
pub const PALACE_SPAWN_Y: i32 = 87;

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
    /// Effective `GetTotalMoveSpeed()` — base class speed plus gear, in the
    /// legacy's accumulation units. The server steps a move off this, one tile
    /// per `5000 / move_speed` ms. `0008_character_move_speed.sql`.
    pub move_speed: i16,
    /// `cltPIPInfo.siExp`.
    pub exp: i64,
    /// `cltPIPInfo.siMoney` — the purse. `0006_character_money.sql`.
    pub money: i64,
    /// Last saved map (the `MapData` index sent as `uiMapIndex`).
    pub map_id: i32,
    /// Last saved tile x (`uiX`, 10 bits).
    pub pos_x: i32,
    /// Last saved tile y (`uiY`, 10 bits).
    pub pos_y: i32,
    /// Character slot in the owning account (`clBI.uiIndex`).
    pub slot: i16,
    /// Home village (`siHomeVillageUnique`).
    pub school_id: i16,
    /// The id the client knows this character by (`MyCharUnique`), or `None`
    /// until it has been taken into a world for the first time.
    pub game_unique: Option<i32>,
    /// `cltPIPInfo.siStr`.
    pub str: i32,
    /// `cltPIPInfo.siDex`.
    pub dex: i32,
    /// `cltPIPInfo.siVit`.
    pub vit: i32,
    /// `cltPIPInfo.siMag`.
    pub mag: i32,
    /// `cltPIPInfo.siHnd`.
    pub hnd: i32,
    /// `cltPIPInfo.siLuk`.
    pub luk: i32,
    /// `cltPIPInfo.siWis`.
    pub wis: i32,
    /// `cltPIPInfo.siBonus` — unspent stat points.
    pub bonus_points: i16,
}

/// An accepted stat allocation, ready to write (`GAMEMSG_REQUEST_USEBONUS`, 250).
///
/// The caller has already validated the request against the server's own row;
/// this is the durable half of that decision — the seven abilities, the remaining
/// bonus points and the two derived maxima, rewritten together so a crash cannot
/// leave the invariant half-applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatsUpdate {
    /// `character_stats.str`.
    pub str_: i32,
    /// `character_stats.dex`.
    pub dex: i32,
    /// `character_stats.vit`.
    pub vit: i32,
    /// `character_stats.mag`.
    pub mag: i32,
    /// `character_stats.hnd`.
    pub hnd: i32,
    /// `character_stats.luk`.
    pub luk: i32,
    /// `character_stats.wis`.
    pub wis: i32,
    /// `character_stats.bonus_points` — points still unspent.
    pub bonus_points: i16,
    /// `character_stats.max_life`, recomputed from the post-state vit/level.
    pub max_life: i32,
    /// `character_stats.max_mana`, recomputed from the post-state mag/level.
    pub max_mana: i32,
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

    /// Persist `character_id`'s cell after a move.
    ///
    /// Only `pos_x`/`pos_y` change; the map is left alone — an order stays on one
    /// map, and a map change is a warp, a different message. Called for
    /// `ORDER_MOVE` (`GAMEMSG_REQUEST_USERORDER`, 240).
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Database`] if the update fails.
    async fn update_position(
        &self,
        character_id: i32,
        x: i32,
        y: i32,
    ) -> Result<(), RepositoryError>;

    /// Persists a move **across maps** — the write the gate warp makes, which
    /// [`Self::update_position`] cannot express because the map is part of it.
    /// The legacy does the same thing through `Warp(true)`'s save, and a reload
    /// that kept the old map would drop the character back where it left.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Database`] if the update fails.
    async fn update_map_position(
        &self,
        character_id: i32,
        map_id: i32,
        x: i32,
        y: i32,
    ) -> Result<(), RepositoryError>;

    /// Persist an accepted stat allocation (`GAMEMSG_REQUEST_USEBONUS`, 250).
    ///
    /// Rewrites the seven abilities, the remaining bonus points and the two
    /// derived maxima in one statement. The caller validates the request against
    /// the server's own row first; this only stores the accepted result.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Database`] if the update fails.
    async fn update_stats(
        &self,
        character_id: i32,
        stats: &StatsUpdate,
    ) -> Result<(), RepositoryError>;
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

/// The 24 columns every [`CharacterFull`] needs, in the order [`FULL_SELECT`]
/// lists them.
///
/// A named struct rather than a tuple because sqlx's tuple `FromRow` impls stop
/// at 16 elements. `str_` carries the `str` column, which is a Rust keyword.
#[derive(sqlx::FromRow)]
struct FullRow {
    character_id: i32,
    name: String,
    class_kind: i16,
    gender: i16,
    level: i16,
    max_life: i32,
    max_mana: i32,
    move_speed: i16,
    exp: i64,
    money: i64,
    map_id: i32,
    pos_x: i32,
    pos_y: i32,
    slot: i16,
    school_id: i16,
    game_unique: Option<i32>,
    #[sqlx(rename = "str")]
    str_: i32,
    dex: i32,
    vit: i32,
    mag: i32,
    hnd: i32,
    luk: i32,
    wis: i32,
    bonus_points: i16,
}

const FULL_SELECT: &str = "SELECT c.character_id, c.name, c.class_kind, c.gender, s.level, \
     s.max_life, s.max_mana, s.move_speed, s.exp, c.money, c.map_id, c.pos_x, c.pos_y, c.slot, c.school_id, \
     c.game_unique, \
     s.str, s.dex, s.vit, s.mag, s.hnd, s.luk, s.wis, s.bonus_points \
     FROM characters c JOIN character_stats s ON s.character_id = c.character_id";

impl From<FullRow> for CharacterFull {
    fn from(row: FullRow) -> Self {
        Self {
            character_id: row.character_id,
            name: row.name,
            class_kind: row.class_kind,
            gender: row.gender,
            level: row.level,
            max_life: row.max_life,
            max_mana: row.max_mana,
            move_speed: row.move_speed,
            exp: row.exp,
            money: row.money,
            map_id: row.map_id,
            pos_x: row.pos_x,
            pos_y: row.pos_y,
            slot: row.slot,
            school_id: row.school_id,
            game_unique: row.game_unique,
            str: row.str_,
            dex: row.dex,
            vit: row.vit,
            mag: row.mag,
            hnd: row.hnd,
            luk: row.luk,
            wis: row.wis,
            bonus_points: row.bonus_points,
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
        let mut tx = self.pool.begin().await?;

        let character_id: i32 = sqlx::query_scalar(
            "INSERT INTO characters \
             (account_id, slot, name, class_kind, gender, map_id, pos_x, pos_y) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             RETURNING character_id",
        )
        .bind(account_id)
        .bind(slot)
        .bind(name)
        .bind(class_kind)
        .bind(gender)
        // The legacy sets these before the row is ever written
        // (`Server-Login.cpp:375-381`); leaving them to the column defaults is
        // what put a character at (0, 0), which is outside every map and renders
        // as a black void.
        .bind(PALACE_MAP_INDEX)
        .bind(PALACE_SPAWN_X)
        .bind(PALACE_SPAWN_Y)
        .fetch_one(&mut *tx)
        .await?;

        // One transaction, matching the legacy's "create the person aggregate"
        // intent (`rsp_PersonInfoCreate` + `rsp_IPCreate`): if the stats row
        // fails, rollback so the character never occupies its (account_id, slot)
        // without being listable.
        sqlx::query("INSERT INTO character_stats (character_id) VALUES ($1)")
            .bind(character_id)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;
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

    async fn update_position(
        &self,
        character_id: i32,
        x: i32,
        y: i32,
    ) -> Result<(), RepositoryError> {
        sqlx::query("UPDATE characters SET pos_x = $2, pos_y = $3 WHERE character_id = $1")
            .bind(character_id)
            .bind(x)
            .bind(y)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn update_map_position(
        &self,
        character_id: i32,
        map_id: i32,
        x: i32,
        y: i32,
    ) -> Result<(), RepositoryError> {
        sqlx::query(
            "UPDATE characters SET map_id = $2, pos_x = $3, pos_y = $4 \
             WHERE character_id = $1",
        )
        .bind(character_id)
        .bind(map_id)
        .bind(x)
        .bind(y)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn update_stats(
        &self,
        character_id: i32,
        stats: &StatsUpdate,
    ) -> Result<(), RepositoryError> {
        sqlx::query(
            "UPDATE character_stats SET \
             str = $2, dex = $3, vit = $4, mag = $5, hnd = $6, luk = $7, wis = $8, \
             bonus_points = $9, max_life = $10, max_mana = $11 \
             WHERE character_id = $1",
        )
        .bind(character_id)
        .bind(stats.str_)
        .bind(stats.dex)
        .bind(stats.vit)
        .bind(stats.mag)
        .bind(stats.hnd)
        .bind(stats.luk)
        .bind(stats.wis)
        .bind(stats.bonus_points)
        .bind(stats.max_life)
        .bind(stats.max_mana)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{PALACE_MAP_INDEX, PALACE_SPAWN_X, PALACE_SPAWN_Y};

    /// The spawn constants are the legacy's, not ours — pinned so nobody
    /// "tidies" them into a nicer-looking number.
    ///
    /// The map is `MAPINDEX_PALACE` (`map.h:69`), which the client's
    /// `Data\mapinfoE.txt` row 173 names `Hanyang`. The tile is inside the box
    /// the legacy's own `rand()%7 + 24` / `rand()%23 + 68`
    /// (`Server-Login.cpp:377-378`) draws from, and equals the fixed palace warp
    /// point in `NPartyDlg.cpp:533`.
    ///
    /// Both halves matter. The bounds assertions catch a spawn point that drifts
    /// outside the palace's walkable box; the equality assertions catch one that
    /// drifts *within* it, which the bounds would happily accept and which would
    /// put the character somewhere the legacy never places one.
    #[test]
    fn the_palace_spawn_is_the_legacy_map_inside_the_legacy_random_box() {
        // `rand()%7 + 24` -> X 24..=30 inclusive.
        assert!((24..=30).contains(&PALACE_SPAWN_X), "x {}", PALACE_SPAWN_X);
        // `rand()%23 + 68` -> Y 68..=90 inclusive.
        assert!((68..=90).contains(&PALACE_SPAWN_Y), "y {}", PALACE_SPAWN_Y);
        // The point `NPartyDlg.cpp:533` warps to.
        assert_eq!(
            (PALACE_MAP_INDEX, PALACE_SPAWN_X, PALACE_SPAWN_Y),
            (158, 27, 87)
        );
    }

    /// Tile 0 of no map is what the client draws as a black void, so nothing in
    /// this module may default to it.
    ///
    /// This is the `(0, 0)` bug restated as an invariant: the columns defaulted to
    /// 0 in `0001_schema.sql` and `create_character` inserted neither of them, so
    /// every character made through the game's own make-character path landed
    /// there. 0007 moves the defaults and corrects the rows.
    #[test]
    fn no_spawn_constant_is_tile_zero() {
        assert_ne!(PALACE_MAP_INDEX, 0);
        assert_ne!(PALACE_SPAWN_X, 0);
        assert_ne!(PALACE_SPAWN_Y, 0);
    }
}
