//! Repository trait + PostgreSQL implementation for account storage.
//!
//! The trait is the seam that lets the storage engine be swapped later
//! (PostgreSQL → MySQL → SQLite) without touching callers. Only the
//! M1 scope (`GZAccountDB` / `tbl_Account`) is implemented; character
//! and inventory repositories come in M2/M3.

use async_trait::async_trait;
use sqlx::Row;
use sqlx::postgres::PgPool;

/// A row from the `accounts` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub account_id: i32,
    pub username: String,
    pub password_hash: String,
    pub gm_level: i32,
    pub gender: i32,
    pub status: i32,
    pub block_mode: i32,
    pub block_reason: Option<String>,
    pub dormancy_state: i32,
    pub playing_time: i32,
    pub created_at: String,
}

/// Errors that can occur in the repository layer.
#[derive(Debug)]
pub enum RepositoryError {
    Database(sqlx::Error),
    NotFound,
    DuplicateUsername,
}

impl std::fmt::Display for RepositoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(e) => write!(f, "database error: {e}"),
            Self::NotFound => write!(f, "account not found"),
            Self::DuplicateUsername => write!(f, "username already exists"),
        }
    }
}

impl std::error::Error for RepositoryError {}

impl From<sqlx::Error> for RepositoryError {
    fn from(e: sqlx::Error) -> Self {
        Self::Database(e)
    }
}

/// The account storage interface. Every method is async; implementations
/// are expected to be cheap to clone (e.g. wrap a connection pool).
#[async_trait]
pub trait AccountRepository: Send + Sync {
    /// Look up an account by username. Returns `NotFound` if absent.
    async fn find_by_username(&self, username: &str) -> Result<Account, RepositoryError>;

    /// Create a new account. Returns `DuplicateUsername` if the name is
    /// already taken.
    async fn create(&self, username: &str, password_hash: &str)
    -> Result<Account, RepositoryError>;

    /// Update the password hash for an existing account.
    async fn update_password(
        &self,
        account_id: i32,
        new_password_hash: &str,
    ) -> Result<(), RepositoryError>;

    /// Update account status (active, blocked, etc.).
    async fn update_status(&self, account_id: i32, status: i32) -> Result<(), RepositoryError>;

    /// Delete an account by id.
    async fn delete(&self, account_id: i32) -> Result<(), RepositoryError>;
}

/// PostgreSQL implementation of [`AccountRepository`].
///
/// Cheap to clone: `sqlx::PgPool` is itself an `Arc` around the connection
/// pool, so cloning shares it. The front service clones one per connection.
#[derive(Clone)]
pub struct PgAccountRepository {
    pool: PgPool,
}

impl PgAccountRepository {
    /// Wrap a connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Build from a connection string. Convenience for tests and tools.
    pub async fn connect(database_url: &str) -> Result<Self, sqlx::Error> {
        let pool = PgPool::connect(database_url).await?;
        Ok(Self::new(pool))
    }
}

#[async_trait]
impl AccountRepository for PgAccountRepository {
    async fn find_by_username(&self, username: &str) -> Result<Account, RepositoryError> {
        let row = sqlx::query(
            r#"
            SELECT account_id, username, password_hash, gm_level, gender,
                   status, block_mode, block_reason, dormancy_state,
                   playing_time, created_at::text AS created_at
            FROM accounts
            WHERE username = $1
            "#,
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(r) => Ok(Account {
                account_id: r.get("account_id"),
                username: r.get("username"),
                password_hash: r.get("password_hash"),
                gm_level: r.get("gm_level"),
                gender: r.get("gender"),
                status: r.get("status"),
                block_mode: r.get("block_mode"),
                block_reason: r.get("block_reason"),
                dormancy_state: r.get("dormancy_state"),
                playing_time: r.get("playing_time"),
                created_at: r.get("created_at"),
            }),
            None => Err(RepositoryError::NotFound),
        }
    }

    async fn create(
        &self,
        username: &str,
        password_hash: &str,
    ) -> Result<Account, RepositoryError> {
        let row = sqlx::query(
            r#"
            INSERT INTO accounts (username, password_hash)
            VALUES ($1, $2)
            RETURNING account_id, username, password_hash, gm_level, gender,
                      status, block_mode, block_reason, dormancy_state,
                      playing_time, created_at::text AS created_at
            "#,
        )
        .bind(username)
        .bind(password_hash)
        .fetch_one(&self.pool)
        .await;

        match row {
            Ok(r) => Ok(Account {
                account_id: r.get("account_id"),
                username: r.get("username"),
                password_hash: r.get("password_hash"),
                gm_level: r.get("gm_level"),
                gender: r.get("gender"),
                status: r.get("status"),
                block_mode: r.get("block_mode"),
                block_reason: r.get("block_reason"),
                dormancy_state: r.get("dormancy_state"),
                playing_time: r.get("playing_time"),
                created_at: r.get("created_at"),
            }),
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                Err(RepositoryError::DuplicateUsername)
            }
            Err(e) => Err(RepositoryError::Database(e)),
        }
    }

    async fn update_password(
        &self,
        account_id: i32,
        new_password_hash: &str,
    ) -> Result<(), RepositoryError> {
        let result = sqlx::query("UPDATE accounts SET password_hash = $1 WHERE account_id = $2")
            .bind(new_password_hash)
            .bind(account_id)
            .execute(&self.pool)
            .await?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }
        Ok(())
    }

    async fn update_status(&self, account_id: i32, status: i32) -> Result<(), RepositoryError> {
        let result = sqlx::query("UPDATE accounts SET status = $1 WHERE account_id = $2")
            .bind(status)
            .bind(account_id)
            .execute(&self.pool)
            .await?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }
        Ok(())
    }

    async fn delete(&self, account_id: i32) -> Result<(), RepositoryError> {
        let result = sqlx::query("DELETE FROM accounts WHERE account_id = $1")
            .bind(account_id)
            .execute(&self.pool)
            .await?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account() -> Account {
        Account {
            account_id: 1,
            username: "testuser".to_string(),
            password_hash: "hash".to_string(),
            gm_level: 0,
            gender: 0,
            status: 1,
            block_mode: 0,
            block_reason: None,
            dormancy_state: 0,
            playing_time: 0,
            created_at: "2026-01-01T00:00:00+00".to_string(),
        }
    }

    #[test]
    fn account_struct_is_clone_and_comparable() {
        let a = account();
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn repository_error_displays() {
        let err = RepositoryError::NotFound;
        assert_eq!(err.to_string(), "account not found");

        let err = RepositoryError::DuplicateUsername;
        assert_eq!(err.to_string(), "username already exists");
    }
}
