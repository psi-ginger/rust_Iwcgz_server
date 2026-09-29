# goonzu-rs - Rust rewrite of the GoonZu server suite

Modern Rust rewrite of the legacy GoonZu ("New Era") server suite, kept **wire-compatible** with the existing `New Era.exe` client. This cargo workspace hosts the protocol crates, the service binaries (front / auth / game / logd / supervisor), the dev tools, and (later) the Tauri desktop apps.

Scaffolded per task **T-001 (M0)** of the work packages.

**Living reference documents** (kept outside this repo, in the AutoClaw workspace):
- Architecture & Protocol Blueprint - `C:\Users\Home\.openclaw-autoclaw\workspace\goonzu-server\docs\rewrite-blueprint\` (README + docs 01-05)
- Rewrite plan - `C:\Users\Home\.openclaw-autoclaw\workspace\goonzu-server\docs\rust-rewrite\plan.md`
- Work packages / task board - `C:\Users\Home\.openclaw-autoclaw\workspace\goonzu-server\docs\rust-rewrite\WORKPACKAGES.md` + `tasks\`

## Build & checks

```powershell
cargo build --workspace
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\check.ps1
```

Toolchain: stable (see `rust-toolchain.toml`); edition 2024.
