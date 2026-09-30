//! CLI for the capture proxy.
//!
//! Arguments are hand-parsed rather than pulled from a CLI crate: the workspace
//! carries no third-party dependencies and this tool has three options. Unknown
//! flags are a hard error, because silently ignoring a typo like `--lisen` would
//! start a proxy in the wrong mode and quietly record a useless capture.

use std::path::PathBuf;
use std::process::ExitCode;

use capture::recorder::Dir;
use capture::{proxy, verify};

const USAGE: &str = "\
capture - record/replay TCP proxy for the golden packet corpus

USAGE:
    capture --listen <ADDR> --target <ADDR> --out <STEM>
    capture --verify <STEM>
    capture --help

OPTIONS:
    --listen <ADDR>   address to accept client connections on
    --target <ADDR>   legacy server address to forward to
    --out <STEM>      capture file stem, e.g. sessions/login
                      (creates parent directories; appends -2, -3 if taken)
    --verify <STEM>   re-read a capture and check it is internally consistent
    -h, --help        show this message

Each session writes four files: <STEM>.c2s.bin, <STEM>.s2c.bin,
<STEM>.meta.json and <STEM>.timing.jsonl.

EXAMPLES:
    capture --listen 127.0.0.1:5750 --target 127.0.0.1:5700 --out sessions/front-login
    capture --verify sessions/front-login
";

#[derive(Debug)]
enum Action {
    Record {
        listen: String,
        target: String,
        out: PathBuf,
    },
    Verify(PathBuf),
    Help,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let action = match parse(&args) {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("capture: {msg}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    let result = match action {
        Action::Help => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Action::Verify(stem) => run_verify(&stem),
        Action::Record {
            listen,
            target,
            out,
        } => proxy::run(&listen, &target, &out).map_err(|e| e.to_string()),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("capture: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_verify(stem: &std::path::Path) -> Result<(), String> {
    let report = verify::verify(stem).map_err(|e| format!("{}: {e}", stem.display()))?;
    println!("capture: {}", stem.display());
    for dir in [Dir::C2s, Dir::S2c] {
        let name = dir.as_str();
        let file = report.files.get(name).copied().unwrap_or(0);
        let chunks = report.chunks.get(name).copied().unwrap_or(0);
        println!("  {name}: {file} bytes in {chunks} chunk(s) on disk",);
    }
    if report.is_ok() {
        println!("  OK: sidecar chunk lengths sum to the recorded bytes");
        Ok(())
    } else {
        Err(format!(
            "{} problem(s):\n{}",
            report.problems.len(),
            report
                .problems
                .iter()
                .map(|p| format!("  - {p}"))
                .collect::<Vec<_>>()
                .join("\n")
        ))
    }
}

fn parse(args: &[String]) -> Result<Action, String> {
    let mut listen = None;
    let mut target = None;
    let mut out = None;
    let mut verify_stem = None;

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        // Accept both `--flag value` and `--flag=value`.
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) => (n, Some(v.to_owned())),
            None => (arg, None),
        };
        let mut value = || -> Result<String, String> {
            if let Some(v) = inline.clone() {
                return Ok(v);
            }
            i += 1;
            args.get(i)
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match name {
            "--listen" => listen = Some(value()?),
            "--target" => target = Some(value()?),
            "--out" => out = Some(value()?),
            "--verify" => verify_stem = Some(value()?),
            "-h" | "--help" => return Ok(Action::Help),
            other => return Err(format!("unknown argument {other:?}")),
        }
        i += 1;
    }

    if let Some(stem) = verify_stem {
        if listen.is_some() || target.is_some() || out.is_some() {
            return Err("--verify cannot be combined with --listen/--target/--out".into());
        }
        return Ok(Action::Verify(PathBuf::from(stem)));
    }
    match (listen, target, out) {
        (Some(listen), Some(target), Some(out)) => Ok(Action::Record {
            listen,
            target,
            out: PathBuf::from(out),
        }),
        (None, None, None) => {
            Err("nothing to do: pass --listen/--target/--out, or --verify".into())
        }
        _ => Err("--listen, --target and --out must all be given together".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn parses_a_full_record_invocation() {
        let a = parse(&args(&[
            "--listen",
            "127.0.0.1:5750",
            "--target",
            "127.0.0.1:5700",
            "--out",
            "sessions/x",
        ]))
        .expect("should parse");
        match a {
            Action::Record {
                listen,
                target,
                out,
            } => {
                assert_eq!(listen, "127.0.0.1:5750");
                assert_eq!(target, "127.0.0.1:5700");
                assert_eq!(out, PathBuf::from("sessions/x"));
            }
            other => panic!("expected record, got {other:?}"),
        }
    }

    #[test]
    fn accepts_the_equals_form() {
        let a = parse(&args(&[
            "--listen=127.0.0.1:5750",
            "--target=127.0.0.1:5700",
            "--out=s",
        ]))
        .expect("should parse");
        assert!(matches!(a, Action::Record { .. }));
    }

    #[test]
    fn rejects_a_typo_rather_than_ignoring_it() {
        let err = parse(&args(&["--lisen", "x", "--target", "y", "--out", "z"]))
            .expect_err("typo must not be ignored");
        assert!(err.contains("--lisen"), "got {err}");
    }

    #[test]
    fn rejects_a_missing_value() {
        assert!(parse(&args(&["--listen"])).is_err());
        assert!(parse(&args(&["--verify"])).is_err());
    }

    #[test]
    fn requires_all_three_record_flags() {
        assert!(parse(&args(&["--listen", "a", "--target", "b"])).is_err());
        assert!(parse(&args(&[])).is_err());
    }

    #[test]
    fn verify_cannot_be_mixed_with_record_flags() {
        assert!(parse(&args(&["--verify", "x", "--out", "y"])).is_err());
        assert!(matches!(
            parse(&args(&["--verify", "x"])).expect("should parse"),
            Action::Verify(_)
        ));
    }

    #[test]
    fn help_short_circuits() {
        assert!(matches!(
            parse(&args(&["--help"])).expect("should parse"),
            Action::Help
        ));
        assert!(matches!(
            parse(&args(&["-h"])).expect("should parse"),
            Action::Help
        ));
    }
}
