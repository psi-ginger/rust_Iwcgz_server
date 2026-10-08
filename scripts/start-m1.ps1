# start-m1.ps1 - start the M1 front + game services for the New Era.exe
# acceptance run.
#
# Each service gets its own window so its log stays visible; close the windows
# (or Ctrl+C in them) to stop. Both talk to PostgreSQL via DATABASE_URL, which
# defaults to the development instance in the service sources.
#
# This script also supplies the game link-cipher key (GAME_LINK_KEY1/2) and the
# prologue mode (GAME_PROLOGUE). game.exe has no built-in key and will refuse to
# start without them, naming the variable it wanted - that is SEC-014, not a bug.
#
# Prerequisite: `cargo build -p front -p game` has produced target\debug\{front,game}.exe,
# and the database has been initialized with scripts\db-init.ps1.
#
# Stop game.exe before rebuilding it: Windows will not replace a running image
# ("Access is denied. (os error 5)").

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$bin = Join-Path $root "target\debug"

foreach ($exe in @("front.exe", "game.exe")) {
    $path = Join-Path $bin $exe
    if (-not (Test-Path $path)) {
        throw "missing $path - run 'cargo build -p front -p game' first"
    }
}

# Each service writes its stdout/stderr to a file under target\m1-logs so the
# opcode trace survives after the windows are closed (target\ is git-ignored).
$logDir = Join-Path $root "target\m1-logs"
New-Item -ItemType Directory -Force -Path $logDir | Out-Null

# --- SEC-014: the game link-cipher key comes from here, not from Rust source ---
#
# The legacy hard-codes 0x12,0x34 in ITZSession::Init (ITZSession.cpp:56) and the
# shipped New Era.exe carries the same pair in its own .text, so this port has to
# send those bytes to be understood at all. What it must not do is repeat them in
# Rust, where a second copy becomes a second truth (known_legacy_bugs.md SEC-014),
# so game.exe refuses to start unless both variables are set and names the one
# that is missing.
#
# Set them in this script to rotate them: every other value the game service reads
# has the same shape, and any of these can be overridden from the shell that
# launches this script without editing it.
$env:GAME_LINK_KEY1 = if ($env:GAME_LINK_KEY1) { $env:GAME_LINK_KEY1 } else { "12" }
$env:GAME_LINK_KEY2 = if ($env:GAME_LINK_KEY2) { $env:GAME_LINK_KEY2 } else { "34" }

# Where the post-login world bulk comes from. "capture" replays the recorded
# session; "live" allocates char_unique per session but sends no world data yet,
# because generating that is T-017's card. The two cannot be mixed today - 45
# offsets in cap_s2c.bin carry the capture's character id 0x05BF and the client
# drops per-character records that do not match its handshake id. So capture is
# the default, and T-017 flips it to live.
$env:GAME_PROLOGUE = if ($env:GAME_PROLOGUE) { $env:GAME_PROLOGUE } else { "capture" }

# When the ~67 KB world bulk goes out after char-select. "on230" waits for the
# client's first REQUEST_CHANGESTATUSINFO, exactly as captured - but that 230
# rides a 30 s timer anchored at *first* world entry, so on re-entry the bulk
# can land up to 30 s after char-select. "at-selectchar" pushes it with the
# entry prologue instead; set it to time the re-entry stall against the
# 2026-10-08 baseline (25.56 s of client silence) in the handoff.
$env:GAME_WORLD_BULK = if ($env:GAME_WORLD_BULK) { $env:GAME_WORLD_BULK } else { "on230" }

$front = Start-Process -FilePath (Join-Path $bin "front.exe") -WorkingDirectory $root `
    -RedirectStandardOutput (Join-Path $logDir "front.log") `
    -RedirectStandardError (Join-Path $logDir "front.err.log") -PassThru
$game = Start-Process -FilePath (Join-Path $bin "game.exe") -WorkingDirectory $root `
    -RedirectStandardOutput (Join-Path $logDir "game.log") `
    -RedirectStandardError (Join-Path $logDir "game.err.log") -PassThru

Write-Host "front pid $($front.Id) listening on :5700, game pid $($game.Id) on :4010."
Write-Host "Logs: $logDir\front.log, $logDir\game.log (+ .err.log)."
# Every log line is stamped +mm:ss.mmm since that service started, so gaps in
# the trace are readable directly; this is the wall clock that +0:00.000 maps to.
Write-Host "Log clock origin: $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss.fff')."
Write-Host "Link key $($env:GAME_LINK_KEY1)/$($env:GAME_LINK_KEY2), prologue $($env:GAME_PROLOGUE), world bulk $($env:GAME_WORLD_BULK)."
Write-Host "Point the client at 127.0.0.1 and log in as test / test."
