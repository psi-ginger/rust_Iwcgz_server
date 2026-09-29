# goonzu-rs CI check: formatting, lints, tests, generated-code drift.
# Run:  powershell -NoProfile -ExecutionPolicy Bypass -File scripts\check.ps1
$ErrorActionPreference = "Stop"
Set-Location (Split-Path -Parent $PSScriptRoot)

Write-Host "[1/4] cargo fmt --check" -ForegroundColor Cyan
cargo fmt --check
if ($LASTEXITCODE -ne 0) { throw "cargo fmt failed" }

Write-Host "[2/4] cargo clippy" -ForegroundColor Cyan
cargo clippy --workspace --all-targets -- -D warnings
if ($LASTEXITCODE -ne 0) { throw "cargo clippy failed" }

Write-Host "[3/4] cargo test" -ForegroundColor Cyan
cargo test --workspace
if ($LASTEXITCODE -ne 0) { throw "cargo test failed" }

# The generated opcode tables are committed. Regeneration needs the read-only
# legacy tree; when it is absent (CI checkout, teammate machine) skip with a
# warning rather than failing the build, and rely on the golden-value tests.
$legacyRoot = if ($env:GOONZU_LEGACY_ROOT) {
    $env:GOONZU_LEGACY_ROOT
} else {
    "C:\Users\Home\Documents\Online_work\Goonzu Source Code\Server_Core\GoonZu_Source"
}
Write-Host "[4/4] codegen --check" -ForegroundColor Cyan
if (Test-Path -LiteralPath (Join-Path $legacyRoot "GoonZuWorld\CommonLogic\MsgType.h")) {
    cargo run -q -p codegen -- game --check
    if ($LASTEXITCODE -ne 0) { throw "generated opcodes are stale; re-run cargo run -p codegen -- game" }
} else {
    Write-Host "  legacy tree not found at $legacyRoot - skipping drift check" -ForegroundColor Yellow
}

Write-Host "All checks passed." -ForegroundColor Green
