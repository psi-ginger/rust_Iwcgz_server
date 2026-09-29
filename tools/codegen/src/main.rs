//! Generates Rust opcode enums from the legacy C++ headers.
//!
//! ```text
//! cargo run -p codegen -- game            # write crates/gz-proto/src/opcodes_game.rs
//! cargo run -p codegen -- game --check    # fail if the committed file is stale
//! ```
//!
//! The generated files are committed; CI runs `--check` so a header change that
//! was not regenerated cannot land silently. The legacy tree is read-only and is
//! never modified.

mod emit;
mod parse;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use emit::EmitSpec;

/// Legacy source root; override with `GOONZU_LEGACY_ROOT`.
const LEGACY_ROOT_ENV: &str = "GOONZU_LEGACY_ROOT";
const DEFAULT_LEGACY_ROOT: &str =
    r"C:\Users\Home\Documents\Online_work\Goonzu Source Code\Server_Core\GoonZu_Source";

/// One generated module.
struct Target {
    /// Sub-command name.
    name: &'static str,
    /// Path of the legacy header, relative to the legacy root.
    header: &'static str,
    /// Enum tag inside that header.
    tag: &'static str,
    /// Generated file, relative to the workspace root.
    out: &'static str,
    /// Legacy identifier prefix stripped from variant names.
    prefix: &'static str,
    ty: &'static str,
    name_fn: &'static str,
    count_fn: &'static str,
    doc: &'static str,
}

const TARGETS: &[Target] = &[Target {
    name: "game",
    header: r"GoonZuWorld\CommonLogic\MsgType.h",
    tag: "eGameNetworkMsg",
    out: r"crates\gz-proto\src\opcodes_game.rs",
    prefix: "GAMEMSG_",
    ty: "GameMsg",
    name_fn: "name",
    count_fn: "count",
    doc: "Game client <-> game server message opcodes (`GAMEMSG_*`).",
}];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return ExitCode::SUCCESS;
    }

    let check = args.iter().any(|a| a == "--check");
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();

    let target_name = match positional.first() {
        Some(name) => name.as_str(),
        None => {
            eprintln!("error: missing target; expected one of: {}", target_names());
            return ExitCode::FAILURE;
        }
    };

    let Some(target) = TARGETS.iter().find(|t| t.name == target_name) else {
        eprintln!(
            "error: unknown target `{target_name}`; expected one of: {}",
            target_names()
        );
        return ExitCode::FAILURE;
    };

    let legacy_root = std::env::var_os(LEGACY_ROOT_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_LEGACY_ROOT));
    let header = legacy_root.join(target.header);
    let out = workspace_root().join(target.out);

    match run(target, &header, &out, check) {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(target: &Target, header: &Path, out: &Path, check: bool) -> Result<String, String> {
    let source = std::fs::read(header)
        .map_err(|e| format!("cannot read legacy header {}: {e}", header.display()))?;

    let def = parse::parse_enum(&source, target.tag)
        .map_err(|e| format!("{} ({})", e, header.display()))?;

    let rendered = emit::emit(
        &def,
        &EmitSpec {
            tag: target.tag,
            source: target.header,
            prefix: target.prefix,
            ty: target.ty,
            name_fn: target.name_fn,
            count_fn: target.count_fn,
            doc: target.doc,
        },
    )
    .map_err(|e| format!("{} ({})", e, header.display()))?;

    if check {
        let existing = std::fs::read_to_string(out)
            .map_err(|e| format!("cannot read {}: {e}", out.display()))?;
        if normalize(&existing) == normalize(&rendered) {
            return Ok(format!(
                "{}: up to date ({} entries, ids {}-{})",
                out.display(),
                def.members.len(),
                def.members.first().map_or(0, |m| m.value),
                def.members.last().map_or(0, |m| m.value)
            ));
        }
        return Err(format!(
            "{} is stale: re-run `cargo run -p codegen -- {}`",
            out.display(),
            target.name
        ));
    }

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    std::fs::write(out, rendered.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", out.display()))?;

    Ok(format!(
        "wrote {} ({} entries, ids {}-{})",
        out.display(),
        def.members.len(),
        def.members.first().map_or(0, |m| m.value),
        def.members.last().map_or(0, |m| m.value)
    ))
}

/// Workspace root, resolved from this crate's manifest so the tool works from
/// any working directory.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("codegen lives at <workspace>/tools/codegen")
        .to_path_buf()
}

/// Git is configured with `core.autocrlf=true` on this machine, so the committed
/// file may come back with CRLF while the generator always writes LF. Compare
/// line endings insensitively so `--check` is not a false alarm after a clone.
fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n")
}

fn target_names() -> String {
    TARGETS
        .iter()
        .map(|t| t.name)
        .collect::<Vec<_>>()
        .join(", ")
}

fn print_help() {
    println!(
        "codegen - generate Rust opcode enums from the legacy C++ headers\n\
         \n\
         USAGE:\n    \
             cargo run -p codegen -- <TARGET> [--check]\n\
         \n\
         TARGETS:\n    {}\n\
         \n\
         FLAGS:\n    \
             --check   verify the committed file matches the legacy header; \
             write nothing, exit 1 on drift\n\
         \n\
         ENV:\n    \
             {LEGACY_ROOT_ENV}  legacy source root (default: {DEFAULT_LEGACY_ROOT})\n",
        TARGETS
            .iter()
            .map(|t| format!("    {:<6} {} -> {}", t.name, t.header, t.out))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
