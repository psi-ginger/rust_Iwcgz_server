//! Auth + account service — auth chain, session keys, game-auth (blueprint 01 Part A S5).
//!
//! M1: wire the account repository into the auth flow. The service connects
//! to PostgreSQL on startup and exposes account lookup for the login path.

use gz_db::{AccountRepository, PgAccountRepository};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("auth: starting (M1 wiring)");

    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://localhost:5432/goonzu".to_string());

    let repo = PgAccountRepository::connect(&database_url).await?;
    println!("auth: connected to database");

    // Smoke test: look up a known account
    match repo.find_by_username("admin").await {
        Ok(account) => println!(
            "auth: found account {} (id={})",
            account.username, account.account_id
        ),
        Err(gz_db::RepositoryError::NotFound) => println!("auth: no 'admin' account yet"),
        Err(e) => eprintln!("auth: lookup error: {e}"),
    }

    println!("auth: ready");
    Ok(())
}
