//! PostgreSQL connectivity spike: prove sqlx can talk to PostgreSQL.
//!
//! Connects using DATABASE_URL env var (or a default), reads server version,
//! creates a test table, inserts/reads a row (simulating GZAccountDB.dbo.tbl_Account),
//! and enumerates tables via information_schema (simulating sys.procedures from T-012).
//!
//! Run with: cargo run --example postgres_spike -p gz-db
//!
//! NOTE: Requires a running PostgreSQL instance. See README.md for
//! connection patterns and setup instructions.

use sqlx::postgres::PgPoolOptions;
use sqlx::Row;

/// Default connection string — used when DATABASE_URL is not set.
/// Format: postgres://user:password@host:port/database
const DEFAULT_DATABASE_URL: &str = "postgres://postgres:Moon@2020@localhost:5432/postgres";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== PostgreSQL connectivity spike (sqlx) ===\n");

    // --- 1. Connect to PostgreSQL ---
    let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        println!("    DATABASE_URL not set, using default: {}", DEFAULT_DATABASE_URL);
        DEFAULT_DATABASE_URL.to_string()
    });

    println!("[1] Connecting to PostgreSQL ...");
    println!("    URL: {}", database_url);

    let pool = match PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
    {
        Ok(pool) => pool,
        Err(e) => {
            eprintln!("    FAILED to connect: {}", e);
            eprintln!("    Ensure PostgreSQL is running and the connection string is correct.");
            return Err(e.into());
        }
    };
    println!("    Connected successfully.\n");

    // --- 2. Read server version ---
    println!("[2] Reading server version ...");
    let row = sqlx::query("SELECT version() AS version")
        .fetch_one(&pool)
        .await?;
    let version: String = row.get("version");
    println!("    Server version: {}\n", version.trim());

    // --- 3. Create test table, insert, and read back ---
    println!("[3] Testing CRUD operations (simulating GZAccountDB.dbo.tbl_Account) ...");

    // Drop if exists for idempotent runs
    sqlx::query("DROP TABLE IF EXISTS spike_test_account")
        .execute(&pool)
        .await?;

    // Create table mimicking tbl_Account structure
    sqlx::query(
        r#"
        CREATE TABLE spike_test_account (
            id SERIAL PRIMARY KEY,
            account_id VARCHAR(50) NOT NULL,
            password VARCHAR(100) NOT NULL,
            login VARCHAR(50) NOT NULL DEFAULT '0',
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
    )
    .execute(&pool)
    .await?;
    println!("    Created table: spike_test_account");

    // Insert a row
    let insert_result = sqlx::query(
        r#"
        INSERT INTO spike_test_account (account_id, password, login)
        VALUES ($1, $2, $3)
        "#,
    )
    .bind("admin")
    .bind("hashed_password_here")
    .bind("1")
    .execute(&pool)
    .await?;
    println!(
        "    Inserted {} row(s)",
        insert_result.rows_affected()
    );

    // Read it back
    let rows = sqlx::query(
        r#"
        SELECT id, account_id, password, login, created_at::text AS created_at
        FROM spike_test_account
        ORDER BY id
        "#,
    )
    .fetch_all(&pool)
    .await?;
    println!("    Read back {} row(s):", rows.len());
    for (i, row) in rows.iter().enumerate() {
        let id: i32 = row.get("id");
        let account_id: String = row.get("account_id");
        let password: String = row.get("password");
        let login: String = row.get("login");
        let created_at: String = row.get("created_at");
        println!(
            "    Row {}: id={}, account_id={}, password={}, login={}, created_at={}",
            i, id, account_id, password, login, created_at
        );
    }
    println!();

    // --- 4. Enumerate tables via information_schema ---
    println!("[4] Enumerating tables via information_schema.tables ...");
    let table_rows = sqlx::query(
        r#"
        SELECT table_schema, table_name, table_type
        FROM information_schema.tables
        WHERE table_schema NOT IN ('pg_catalog', 'information_schema')
        ORDER BY table_schema, table_name
        "#,
    )
    .fetch_all(&pool)
    .await?;
    println!("    Found {} table(s):", table_rows.len());
    for row in &table_rows {
        let schema: String = row.get("table_schema");
        let name: String = row.get("table_name");
        let table_type: String = row.get("table_type");
        println!("    {}.{} ({})", schema, name, table_type);
    }
    println!();

    // --- 5. Enumerate stored procedures/functions via pg_catalog ---
    println!("[5] Enumerating stored procedures via pg_catalog.pg_proc ...");
    let proc_rows = sqlx::query(
        r#"
        SELECT n.nspname AS schema_name,
               p.proname AS procedure_name,
               pg_get_function_identity_arguments(p.oid) AS args
        FROM pg_catalog.pg_proc p
        JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace
        WHERE n.nspname NOT IN ('pg_catalog', 'information_schema')
        ORDER BY n.nspname, p.proname
        "#,
    )
    .fetch_all(&pool)
    .await?;
    println!("    Found {} procedure(s)/function(s):", proc_rows.len());
    for row in &proc_rows {
        let schema: String = row.get("schema_name");
        let name: String = row.get("procedure_name");
        let args: String = row.get("args");
        println!("    {}.{}({})", schema, name, args);
    }
    println!();

    // --- 6. Cleanup ---
    println!("[6] Cleaning up test table ...");
    sqlx::query("DROP TABLE IF EXISTS spike_test_account")
        .execute(&pool)
        .await?;
    println!("    Dropped table: spike_test_account");

    // Close pool
    pool.close().await;

    println!("\n=== Spike complete ===");
    Ok(())
}
