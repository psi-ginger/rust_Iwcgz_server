@echo off
chcp 65001 > nul
echo ========================================================
echo       GoonZu-rs: Reset DB + Cargo Build + Start Server
echo ========================================================
echo.

cd /d C:\Data\04_Server_Goonzu\goonzu-rs

echo [1/3] Resetting PostgreSQL and Re-seeding...
powershell -NoProfile -ExecutionPolicy Bypass -Command ^
    ". .\scripts\dev-db.ps1; " ^
    "& 'C:\Data\98_PostgreSQL\bin\psql.exe' -U postgres -d goonzu -c 'TRUNCATE TABLE character_items, characters CASCADE;'; " ^
    ".\scripts\db-init.ps1"

if %ERRORLEVEL% NEQ 0 (
    echo [ERROR] Database reset failed!
    pause
    exit /b %ERRORLEVEL%
)

echo.
echo [2/3] Building front and game crates...
cargo build -p game -p front
if %ERRORLEVEL% NEQ 0 (
    echo [ERROR] Cargo build failed!
    pause
    exit /b %ERRORLEVEL%
)

echo.
echo [3/3] Starting GoonZu Server (start-m1.ps1)...
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\start-m1.ps1
