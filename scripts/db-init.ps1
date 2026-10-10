# Applies the goonzu-rs development schema and seed to PostgreSQL.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\db-init.ps1
#
# Override the target with environment variables (or scripts\dev-db.ps1, which
# is sourced below when present and is gitignored):
#   $env:PGHOST / $env:PGPORT / $env:PGUSER / $env:PGPASSWORD / $env:GZ_DB
#
# This is a development bootstrap only; it is NOT invoked by scripts\check.ps1
# (which must stay green without a database).

param(
    [string]$Database = $(if ($env:GZ_DB) { $env:GZ_DB } else { "goonzu" }),
    [string]$Psql     = "C:\Data\98_PostgreSQL\bin\psql.exe"
)

$ErrorActionPreference = "Stop"

# No DB password may live in the repo. This dev box's credentials come from the
# gitignored scripts\dev-db.ps1 when present; otherwise the shell must export
# PGPASSWORD (and PGHOST/PGUSER if they differ from the defaults below).
$devDb = Join-Path $PSScriptRoot "dev-db.ps1"
if (Test-Path $devDb) { . $devDb }

if (-not $env:PGPASSWORD) {
    throw "PGPASSWORD is not set: export it, or create the gitignored scripts\dev-db.ps1 (see the head of this file)"
}
if (-not $env:PGHOST)     { $env:PGHOST = "localhost" }
if (-not $env:PGUSER)     { $env:PGUSER = "postgres" }

if (-not (Test-Path $Psql)) { throw "psql not found at '$Psql' (set -Psql)" }

$root  = Split-Path -Parent $PSScriptRoot
$files = @(
    "0001_schema.sql",
    "0002_seed_dev.sql",
    "0003_character_items.sql",
    "0004_world_items.sql",
    "0005_world_item_handle.sql",
    "0006_character_money.sql",
    "0007_new_character_spawn.sql",
    "0008_character_move_speed.sql",
    "0009_seed_second_account.sql",
    "0010_world_items_cell_unique.sql"
)

foreach ($f in $files) {
    $path = Join-Path $root "crates\gz-db\sql\$f"
    if (-not (Test-Path $path)) { throw "missing SQL file: $path" }
    Write-Host "applying $f -> $Database"
    & $Psql -U $env:PGUSER -h $env:PGHOST -d $Database -v ON_ERROR_STOP=1 -q -f $path
    if ($LASTEXITCODE -ne 0) { throw "psql failed on $f (exit $LASTEXITCODE)" }
}

Write-Host "db-init: done"
