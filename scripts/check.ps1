# goonzu-rs CI check: formatting, lints, tests, generated-code drift, fuzz smoke.
# Run:  powershell -NoProfile -ExecutionPolicy Bypass -File scripts\check.ps1
#
#   -Fuzz              also run the libFuzzer smoke pass (needs nightly + LLVM;
#                      ~$FuzzSeconds per target). Off by default so the normal
#                      check stays fast.
#   -FuzzSeconds <n>   seconds per fuzz target (default 30, ignored without -Fuzz)
param(
    [switch]$Fuzz,
    [int]$FuzzSeconds = 30
)

$ErrorActionPreference = "Stop"
Set-Location (Split-Path -Parent $PSScriptRoot)

Write-Host "[1/5] cargo fmt --check" -ForegroundColor Cyan
cargo fmt --check
if ($LASTEXITCODE -ne 0) { throw "cargo fmt failed" }

Write-Host "[2/5] cargo clippy" -ForegroundColor Cyan
cargo clippy --workspace --all-targets -- -D warnings
if ($LASTEXITCODE -ne 0) { throw "cargo clippy failed" }

Write-Host "[3/5] cargo test" -ForegroundColor Cyan
cargo test --workspace
if ($LASTEXITCODE -ne 0) { throw "cargo test failed" }

# The generated opcode tables are committed. Regeneration needs the read-only
# legacy tree; when it is absent (CI checkout, teammate machine) skip with a
# warning rather than failing the build, and rely on the golden-value tests.
#
# NOTE: this default was stale until 2026-10-02 -- it pointed at the pre-move
# path under C:\Users\Home\Documents\Online_work, so step 4 silently skipped on
# every run and the drift check had effectively never executed. Same defect as
# verify_translations.py's DEF_SRC. If the tree moves again, this needs
# updating; override with $env:GOONZU_LEGACY_ROOT.
$legacyRoot = if ($env:GOONZU_LEGACY_ROOT) {
    $env:GOONZU_LEGACY_ROOT
} else {
    "C:\Data\04_Server_Goonzu\GoonZu Source Code\Server_Core\GoonZu_Source"
}
Write-Host "[4/5] codegen --check" -ForegroundColor Cyan
if (Test-Path -LiteralPath (Join-Path $legacyRoot "GoonZuWorld\CommonLogic\MsgType.h")) {
    Write-Host "  legacy tree: $legacyRoot"
    cargo run -q -p codegen -- all --check
    if ($LASTEXITCODE -ne 0) { throw "generated opcode tables are stale; re-run cargo run -p codegen -- all" }
} else {
    Write-Host "  legacy tree not found at $legacyRoot - skipping drift check" -ForegroundColor Yellow
    Write-Host "  (set GOONZU_LEGACY_ROOT to point at the read-only legacy source)" -ForegroundColor Yellow
}

# --- fuzz smoke (opt-in) --------------------------------------------------------
#
# Step 3 already covers the T-009 invariants: crates/gz-net/tests/property_framing.rs
# runs the same assertions over a 20k-case pseudo-random corpus on stable, with
# no extra toolchain. That is the gate that should always run.
#
# This step is the real libFuzzer pass, which additionally has coverage feedback
# and mutation. It needs two things the base image does not guarantee:
#
#   * a nightly toolchain          -> rustup toolchain install nightly
#   * LLVM's x64 clang_rt runtime  -> winget install LLVM.LLVM
#
# The stock MSVC install ships those runtimes for x86 only, so without LLVM the
# link fails with LNK1104 (missing clang_rt.asan_dynamic_runtime_thunk-x86_64.lib).
# On Linux neither is a problem: rustup's bundled LLVM provides the runtime.
Write-Host "[5/5] fuzz smoke" -ForegroundColor Cyan
if (-not $Fuzz) {
    Write-Host "  skipped (pass -Fuzz to run; invariants are still covered by step 3)" -ForegroundColor Yellow
} else {
    $nightly = (rustup toolchain list 2>&1 | Select-String -Pattern "^nightly")
    if (-not $nightly) {
        throw "-Fuzz needs nightly: run 'rustup toolchain install nightly'"
    }

    # Locate LLVM's x64 clang runtime directory. RUSTFLAGS must carry it because
    # MSVC's LIBPATH env var is only honoured from an initialised VS environment,
    # which CI does not have. The 8.3 short path avoids the space in "Program Files",
    # which RUSTFLAGS cannot quote around.
    $llvmLib = $null
    foreach ($root in @("C:\Program Files\LLVM", "C:\Program Files (x86)\LLVM")) {
        if (Test-Path -LiteralPath $root) {
            $hit = Get-ChildItem -LiteralPath $root -Recurse -Filter "clang_rt.asan_dynamic_runtime_thunk-x86_64.lib" -ErrorAction SilentlyContinue |
                Select-Object -First 1
            if ($hit) { $llvmLib = $hit.Directory.FullName; break }
        }
    }
    if (-not $llvmLib) {
        throw "-Fuzz needs LLVM's x64 clang runtime: run 'winget install LLVM.LLVM' (x86-only by default in MSVC, hence the failure)"
    }
    # The ASan runtime DLL has to be reachable at run time as well as link time.
    $env:PATH = "$llvmLib;$env:PATH"
    $short = (cmd /c "for %I in (`"$llvmLib`") do @echo %~sI").Trim()
    $env:RUSTFLAGS = "-L native=$short"
    Write-Host "  llvm runtime: $llvmLib"

    foreach ($target in @("frame_parser", "cipher_decrypt", "frame_then_decipher")) {
        Write-Host "  running $target for ${FuzzSeconds}s ..." -ForegroundColor Cyan
        cargo +nightly fuzz run $target -- "-max_total_time=$FuzzSeconds"
        if ($LASTEXITCODE -ne 0) { throw "fuzz target '$target' crashed - see fuzz/artifacts/$target" }
    }
    Write-Host "  all targets clean" -ForegroundColor Green
}

Write-Host "All checks passed." -ForegroundColor Green