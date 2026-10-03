# start-m1.ps1 - start the M1 front + game services for the New Era.exe
# acceptance run.
#
# Each service gets its own window so its log stays visible; close the windows
# (or Ctrl+C in them) to stop. Both talk to PostgreSQL via DATABASE_URL, which
# defaults to the development instance in the service sources.
#
# Prerequisite: `cargo build -p front -p game` has produced target\debug\{front,game}.exe,
# and the database has been initialized with scripts\db-init.ps1.

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$bin = Join-Path $root "target\debug"

foreach ($exe in @("front.exe", "game.exe")) {
    $path = Join-Path $bin $exe
    if (-not (Test-Path $path)) {
        throw "missing $path - run 'cargo build -p front -p game' first"
    }
}

Start-Process -FilePath (Join-Path $bin "front.exe") -WorkingDirectory $root
Start-Process -FilePath (Join-Path $bin "game.exe") -WorkingDirectory $root

Write-Host "front listening on :5700, game on :4010."
Write-Host "Point the client at 127.0.0.1 and log in as test / test."
