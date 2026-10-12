//! Friend repository (`character_friends`), scoped to what the friend family
//! needs: the whole list on world entry, and one insert when a 6202 accept
//! lands.
//!
//! The trait mirrors [`crate::repository::AccountRepository`] and the other
//! repositories so the game service can take either implementation. A
//! friendship is stored per owner with its own group and list slot; the friend
//! name and account id are resolved by join so the loaded records are ready to
//! encode into 6219/6210 without another round trip.

use async_trait::async_trait;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

use crate::repository::RepositoryError;

/// One friend row as loaded for its owner — everything a 6219 slot or 6207
/// push needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedFriend {
    /// `friend_character_id` — the friend's `siPersonID`.
    pub person_id: i32,
    /// The friend's character name (`clFriend[i].szName`).
    pub name: String,
    /// The group the owner stored this friend under (`siGroupID`).
    pub group: u8,
    /// The friend's account id (`szAccountID[i]`).
    pub account_id: String,
    /// The row's slot in the owner's list; load order follows it so the
    /// registry's vec index (what 6203/6219/6210 call the slot) stays stable
    /// across logins.
    pub list_slot: i16,
}

/// The friend storage interface.
#[async_trait]
pub trait FriendRepository: Send + Sync {
    /// Every friendship a character owns, in `list_slot` order.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Database`] if the query fails.
    async fn load_owned(
        &self,
        owner_character_id: i32,
    ) -> Result<Vec<OwnedFriend>, RepositoryError>;

    /// Record one side of an accepted friendship. Idempotent: a duplicate row
    /// (a replayed accept) is left as-is.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Database`] if the insert fails.
    async fn add(
        &self,
        owner_character_id: i32,
        friend_character_id: i32,
        group: u8,
        list_slot: i16,
    ) -> Result<(), RepositoryError>;
}

/// PostgreSQL implementation. Cheap to clone (shares the pool).
#[derive(Clone)]
pub struct PgFriendRepository {
    pool: PgPool,
}

impl PgFriendRepository {
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
}

#[async_trait]
impl FriendRepository for PgFriendRepository {
    async fn load_owned(
        &self,
        owner_character_id: i32,
    ) -> Result<Vec<OwnedFriend>, RepositoryError> {
        let rows = sqlx::query_as::<_, (i16, i16, i32, String, String)>(
            "SELECT f.list_slot, f.group_id, c.character_id, c.name, a.username \
             FROM character_friends f \
             JOIN characters c ON c.character_id = f.friend_character_id \
             JOIN accounts a ON a.account_id = c.account_id \
             WHERE f.owner_character_id = $1 \
             ORDER BY f.list_slot",
        )
        .bind(owner_character_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(list_slot, group, person_id, name, account_id)| OwnedFriend {
                    person_id,
                    name,
                    group: u8::try_from(group.max(0)).unwrap_or(0),
                    account_id,
                    list_slot,
                },
            )
            .collect())
    }

    async fn add(
        &self,
        owner_character_id: i32,
        friend_character_id: i32,
        group: u8,
        list_slot: i16,
    ) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO character_friends \
                 (owner_character_id, friend_character_id, group_id, list_slot) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (owner_character_id, friend_character_id) DO NOTHING",
        )
        .bind(owner_character_id)
        .bind(friend_character_id)
        .bind(i16::from(group))
        .bind(list_slot)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The group, person and slot survive the u8/i16 narrowing the raw columns
    /// force on the wire. Negative columns (a corruption or a legacy value)
    /// clamp to 0 like the character code does.
    #[test]
    fn owned_friend_narrowing_is_lossless_for_valid_rows() {
        let row = (
            7i16,  // list_slot
            3i16,  // group_id
            42i32, // friend_character_id
            "Tester".to_owned(),
            "tester".to_owned(),
        );
        let owned = OwnedFriend {
            person_id: row.2,
            name: row.3.clone(),
            group: u8::try_from(row.1.max(0)).unwrap_or(0),
            account_id: row.4.clone(),
            list_slot: row.0,
        };
        assert_eq!(owned.person_id, 42);
        assert_eq!(owned.group, 3);
        assert_eq!(owned.account_id, "tester");
        assert_eq!(owned.list_slot, 7);
        // A negative group (never written by this port) is clamped, not wrapped.
        let negative = -1i16;
        assert_eq!(u8::try_from(negative.max(0)).unwrap_or(0), 0);
    }
}
