# goonzu-rs CI check: formatting, lints, tests.
# Run:  powershell -NoProfile -ExecutionPolicy Bypass -File scripts\check.ps1
$ErrorActionPreference = "Stop"
Set-Location (Split-Path -Parent $PSScriptRoot)

Write-Host "[1/3] cargo fmt --check" -ForegroundColor Cyan
cargo fmt --check
if ($LASTEXITCODE -ne 0) { throw "cargo fmt failed" }

Write-Host "[2/3] cargo clippy" -ForegroundColor Cyan
cargo clippy --workspace --all-targets -- -D warnings
if ($LASTEXITCODE -ne 0) { throw "cargo clippy failed" }

Write-Host "[3/3] cargo test" -ForegroundColor Cyan
cargo test --workspace
if ($LASTEXITCODE -ne 0) { throw "cargo test failed" }

Write-Host "All checks passed." -ForegroundColor Green
