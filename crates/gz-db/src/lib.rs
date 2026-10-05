//! SQL repositories — plain SQL, no stored procedures (blueprint 03).
//!
//! The storage engine is behind a trait ([`repository::AccountRepository`])
//! so it can be swapped later (PostgreSQL → MySQL → SQLite) without
//! touching callers. M1 scope: accounts only.

pub mod character;
pub mod item;
pub mod password;
pub mod repository;

pub use character::{CharacterFull, CharacterRepository, CharacterSummary, PgCharacterRepository};
pub use item::{
    CharacterItem, DropLocation, DropOutcome, DropStatus, GroundClaim, ItemRepository, MoveOutcome,
    PgItemRepository, PickupOutcome, PickupStatus, SplitOutcome, SplitStatus, container_of,
};
pub use repository::{Account, AccountRepository, PgAccountRepository, RepositoryError};
