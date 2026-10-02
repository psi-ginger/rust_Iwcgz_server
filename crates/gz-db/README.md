# gz-db

SQL Server repositories - plain SQL, no stored procedures (blueprint 03).

Scaffold: M0 T-001. Connectivity spike: T-012 (tiberius/SQL Server). PostgreSQL spike: sqlx.

## Dependencies

- **sqlx 0.8** — async PostgreSQL driver with compile-time checked queries
- **tokio 1** — async runtime

### Previous dependencies (T-012 spike, now removed)

- ~~**tiberius 0.12**~~ — pure-Rust TDS driver (replaced by sqlx)
- ~~**tokio-util 0.7**~~ — compat layer for tiberius (no longer needed)

## Connection patterns

### PostgreSQL (sqlx) — current

```rust
use sqlx::postgres::PgPoolOptions;

let database_url = std::env::var("DATABASE_URL")
    .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5432/goonzu".to_string());

let pool = PgPoolOptions::new()
    .max_connections(5)
    .connect(&database_url)
    .await?;
```

#### Connection string format

```
postgres://user:password@host:port/database
```

| Component | Default | Notes |
|---|---|---|
| `user` | `postgres` | or a dedicated app user |
| `password` | `postgres` | use env var in production |
| `host` | `localhost` | or remote host |
| `port` | `5432` | default PostgreSQL port |
| `database` | `goonzu` | target database |

#### Environment variable

Set `DATABASE_URL` to override the default:

```bash
# Linux/macOS
export DATABASE_URL="postgres://myuser:mypass@db.example.com:5432/goonzu"

# Windows PowerShell
$env:DATABASE_URL = "postgres://myuser:mypass@db.example.com:5432/goonzu"
```

### SQL Server (tiberius) — T-012 spike only

```rust
use tiberius::{Client, Config, AuthMethod};
use tokio::net::TcpStream;
use tokio_util::compat::TokioAsyncWriteCompatExt;

let mut config = Config::new();
config.host("localhost");
config.port(1433);
config.instance_name("SQLEXPRESS");       // named instance resolution
config.authentication(AuthMethod::sql_server("GZServer", "um7aZd/"));
config.trust_cert();                       // equivalent to sqlcmd -C
config.database("GZAccountDB");

let tcp = TcpStream::connect(config.get_addr()).await?;
let mut client = Client::connect(config, tcp.compat_write()).await?;
```

## Spike: `examples/conn_spike.rs` (T-012, SQL Server)

The T-012 spike demonstrates:
1. Connecting to `localhost\SQLEXPRESS`
2. Reading server version (`@@VERSION`)
3. Reading `GZAccountDB.dbo.tbl_Account`
4. Enumerating procedures across all three game databases
5. Verifying the 18 BUG-015 procedure names are absent

Run with:
```bash
cargo run --example conn_spike -p gz-db
```

### Spike results (2026-10-02)

**Status: NOT RUN** — SQL Server is not installed on this machine.

The spike was developed and compiled on a machine without SQL Server.
The task card references `DESKTOP-E0KM6EF\SQLEXPRESS` which is a different
machine. The code is correct and will work when run on a machine with:
- SQL Server running on `localhost\SQLEXPRESS`
- TCP/IP protocol enabled
- SQL Server Browser service running (for named instance resolution)
- `GZServer` login with password `um7aZd/`

**Expected outputs** (based on `sqlcmd` verification 2026-09-30):
- Server version: `Microsoft SQL Server 2025 (RTM) 17.0.1000.7 (X64) ...`
- `tbl_Account`: 1 row (the admin account)
- Procedure counts: GZAccountDB=18, GoonZuAsia=817, GameLog=24
- BUG-015: all 18 names absent (confirmed by census 2026-09-30)

## Spike: `examples/postgres_spike.rs` (PostgreSQL, sqlx)

The PostgreSQL spike demonstrates:
1. Connecting using `DATABASE_URL` env var (or default)
2. Reading server version (`SELECT version()`)
3. Creating a test table, inserting a row, reading it back (simulating `GZAccountDB.dbo.tbl_Account`)
4. Enumerating tables via `information_schema.tables` (simulating `sys.procedures` from T-012)
5. Enumerating stored procedures via `pg_catalog.pg_proc`
6. Cleaning up the test table

Run with:
```bash
cargo run --example postgres_spike -p gz-db
```

### PostgreSQL vs SQL Server differences

| Aspect | SQL Server (tiberius) | PostgreSQL (sqlx) |
|---|---|---|
| Connection | `Config` struct with host/port/instance | Connection string URL |
| Auth | SQL auth or Windows integrated | Password or certificate |
| Version query | `SELECT @@VERSION` | `SELECT version()` |
| Schema | `dbo.tbl_Account` | `public.tbl_account` |
| Procedures | `sys.procedures` | `pg_catalog.pg_proc` |
| Information schema | `INFORMATION_SCHEMA.TABLES` | `information_schema.tables` |
| Parameter marker | `@p1`, `@p2` | `$1`, `$2` |
| Named instances | Yes (via SQL Browser) | No (direct host:port) |
| TLS | `trust_cert()` for self-signed | `sslmode` parameter in URL |

### Setting up PostgreSQL for development

```bash
# Using Docker
docker run --name goonzu-pg \
  -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=goonzu \
  -p 5432:5432 \
  -d postgres:16

# Connect with default
cargo run --example postgres_spike -p gz-db

# Or with custom connection string
DATABASE_URL="postgres://postgres:postgres@localhost:5432/goonzu" \
  cargo run --example postgres_spike -p gz-db
```

## Recommended read-only login for development

### PostgreSQL

```sql
CREATE ROLE gz_reader WITH LOGIN PASSWORD 'read-only-password';
GRANT CONNECT ON DATABASE goonzu TO gz_reader;
GRANT USAGE ON SCHEMA public TO gz_reader;
GRANT SELECT ON ALL TABLES IN SCHEMA public TO gz_reader;
ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT SELECT ON TABLES TO gz_reader;
```

### SQL Server (legacy)

```sql
CREATE LOGIN GZReader WITH PASSWORD = 'read-only-password';
CREATE USER GZReader FOR LOGIN GZReader;
ALTER ROLE db_datareader ADD MEMBER GZReader;
-- Repeat for each database
```

## Scope notes

- **M1 scope:** `GZAccountDB` only (1 table, 21 procedures, 1 row)
- **M2/M3 scope:** `GoonZuAsia` (204 tables, 817 procedures) — connectivity unverified
- **M2/M3 scope:** `GameLog` (21 tables, 24 procedures) — connectivity unverified

The spike deliberately does not depend on `GoonZuAsia` tables per the
2026-09-30 scope change in T-012.
