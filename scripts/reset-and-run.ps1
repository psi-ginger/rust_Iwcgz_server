# Reset DB + Cargo Build + Start Server in one command
$ErrorActionPreference = "Stop"

Write-Host "=== [1/3] Resetting PostgreSQL & Seeding ===" -ForegroundColor Cyan
. "$PSScriptRoot\dev-db.ps1"
& "C:\Data\98_PostgreSQL\bin\psql.exe" -U postgres -d goonzu -c "TRUNCATE TABLE character_items, characters CASCADE;"
& "$PSScriptRoot\db-init.ps1"

Write-Host "`n=== [2/3] Cargo Building game and front ===" -ForegroundColor Cyan
Set-Location "$PSScriptRoot\.."
cargo build -p game -p front

Write-Host "`n=== [3/3] Starting Server (start-m1.ps1) ===" -ForegroundColor Cyan
& "$PSScriptRoot\start-m1.ps1"
