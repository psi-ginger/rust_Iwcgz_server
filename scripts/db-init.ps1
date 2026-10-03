# Applies the goonzu-rs development schema and seed to PostgreSQL.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\db-init.ps1
#
# Override the target with environment variables:
#   $env:PGHOST / $env:PGPORT / $env:PGUSER / $env:PGPASSWORD / $env:GZ_DB
#
# This is a development bootstrap only; it is NOT invoked by scripts\check.ps1
# (which must stay green without a database).

param(
    [string]$Database = $(if ($env:GZ_DB) { $env:GZ_DB } else { "goonzu" }),
    [string]$Psql     = "C:\Data\98_PostgreSQL\bin\psql.exe"
)

$ErrorActionPreference = "Stop"

if (-not $env:PGPASSWORD) { $env:PGPASSWORD = "Moon@2020" }
if (-not $env:PGHOST)     { $env:PGHOST = "localhost" }
if (-not $env:PGUSER)     { $env:PGUSER = "postgres" }

if (-not (Test-Path $Psql)) { throw "psql not found at '$Psql' (set -Psql)" }

$root  = Split-Path -Parent $PSScriptRoot
$files = @("0001_schema.sql", "0002_seed_dev.sql")

foreach ($f in $files) {
    $path = Join-Path $root "crates\gz-db\sql\$f"
    if (-not (Test-Path $path)) { throw "missing SQL file: $path" }
    Write-Host "applying $f -> $Database"
    & $Psql -U $env:PGUSER -h $env:PGHOST -d $Database -v ON_ERROR_STOP=1 -q -f $path
    if ($LASTEXITCODE -ne 0) { throw "psql failed on $f (exit $LASTEXITCODE)" }
}

Write-Host "db-init: done"
