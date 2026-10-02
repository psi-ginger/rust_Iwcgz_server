//! Regenerate the committed `schema.json`, `schema-examples.json` and the method
//! table in the README.
//!
//! Run after changing any message type:
//!
//! ```text
//! cargo run -p gz-ops --bin export-schema
//! ```
//!
//! Or check for drift without writing anything, the way `codegen --check` does:
//!
//! ```text
//! cargo run -p gz-ops --bin export-schema -- --check
//! ```
//!
//! Exit codes: 0 up to date, 1 an I/O error, 2 stale (`--check` only).
//!
//! `tests/contract.rs` also fails if a committed file no longer matches, so
//! forgetting to run this shows up as a test failure rather than as a frontend
//! quietly disagreeing with the server.

use gz_ops::{
    Call, Event, EventFrame, LogLevel, LogLine, Method, OpsError, OpsErrorCode, Request, Response,
    ResponseData, methods::*,
};
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// Printed when the committed artifacts are up to date.
const UP_TO_DATE: &str = "gz-ops: schema.json is up to date";

/// Exit code for `--check` when an artifact is stale.
const EXIT_STALE: u8 = 2;

fn main() -> std::process::ExitCode {
    // `--check` reports drift without touching anything, mirroring
    // `codegen --check`. It deliberately does not shell out to git: a
    // git-diff-based check silently passes for any file that is not yet tracked,
    // which is exactly the state a brand-new generated artifact is in -- so such
    // a check would stay green forever while the file was wrong. Comparing the
    // file against what the types derive to right now has no such blind spot.
    let check_only = std::env::args().any(|a| a == "--check");

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let schema_path = root.join("schema.json");
    let examples_path = root.join("schema-examples.json");
    let readme_path = root.join("README.md");

    let mut stale = Vec::new();
    // Short names for the `--check` message; absolute paths are noise when the
    // reader is already looking at the crate.
    let mut stale_rel = Vec::new();

    for (name, path, contents) in [
        (
            "schema.json",
            schema_path.clone(),
            gz_ops::schema::contract_schema_json(),
        ),
        (
            "schema-examples.json",
            examples_path.clone(),
            examples_json(),
        ),
    ] {
        // Compare *before* writing. Checking afterwards would always find the
        // file matching, since we just wrote it, and a regenerated artifact
        // would silently report itself as unchanged.
        if !is_current(&path, &contents) {
            if !check_only && let Err(e) = write(&path, &contents) {
                eprintln!("gz-ops: writing {}: {e}", path.display());
                return std::process::ExitCode::FAILURE;
            }
            stale.push(path.display().to_string());
            stale_rel.push(name.to_string());
        }
    }

    // The README's method table is generated, so it cannot drift from Method::ALL.
    let table = gz_ops::schema::method_table_markdown();
    match std::fs::read_to_string(&readme_path) {
        Ok(existing) => {
            let updated = splice_method_table(&existing, &table);
            if updated != existing {
                if !check_only && let Err(e) = std::fs::write(&readme_path, updated) {
                    eprintln!("gz-ops: writing README.md: {e}");
                    return std::process::ExitCode::FAILURE;
                }
                stale.push(readme_path.display().to_string());
                stale_rel.push("README.md method table".to_string());
            }
        }
        Err(e) => {
            eprintln!("gz-ops: reading README.md: {e}");
            return std::process::ExitCode::FAILURE;
        }
    }

    if stale.is_empty() {
        println!("{UP_TO_DATE}");
        return std::process::ExitCode::SUCCESS;
    }

    if check_only {
        eprintln!(
            "gz-ops: STALE ({}); run: cargo run -p gz-ops --bin export-schema",
            stale_rel.join(", ")
        );
        return std::process::ExitCode::from(EXIT_STALE);
    }

    for p in &stale {
        println!("gz-ops: updated {p}");
    }
    std::process::ExitCode::SUCCESS
}

/// Example frames for every method, as the real serializers emit them.
///
/// Committed alongside `schema.json` so a frontend author has something to
/// copy, and so `tests/contract.rs` can check that the documented shapes are
/// the shapes serde actually produces. The alternative -- hand-writing
/// examples -- is how the LZF oracle ended up comparing against a fabricated
/// stream during T-005.
fn examples_json() -> String {
    let requests = vec![
        example_call(Call::Hello(HelloParams {
            client: "control-panel".into(),
            client_version: "0.1.0".into(),
            min_api_version: 1,
            token: Some("<token>".into()),
        })),
        example_call(Call::StatusGet(NoParams {})),
        example_call(Call::ServicesList(NoParams {})),
        example_call(Call::LogsTail(LogsTailParams {
            stream: Some("game".into()),
            min_level: Some(LogLevel::Warn),
            contains: None,
            replay: Some(200),
        })),
        example_call(Call::LogsTailStop(LogsTailStopParams { subscription: 1 })),
        example_call(Call::ConfigGet(ConfigGetParams {
            prefix: Some("server".into()),
        })),
        example_call(Call::ConfigSet(ConfigSetParams {
            entries: vec![ConfigSetEntry {
                key: "server.game.tick_hz".into(),
                value: "30".into(),
            }],
        })),
        example_call(Call::DbPing(NoParams {})),
        example_call(Call::PlayersList(PlayersListParams {
            online_only: Some(true),
            search: None,
            limit: Some(50),
            offset: None,
        })),
        example_call(Call::ServiceStart(ServiceActionParams {
            service: "game".into(),
            timeout_s: Some(30),
        })),
        example_call(Call::ServiceStop(ServiceActionParams {
            service: "game".into(),
            timeout_s: None,
        })),
        example_call(Call::ServiceRestart(ServiceActionParams {
            service: "front".into(),
            timeout_s: None,
        })),
        example_call(Call::SessionKick(SessionKickParams {
            player_id: Some(1042),
            account_id: None,
            reason: Some("cheating".into()),
        })),
    ];

    let responses = vec![
        Response::ok(
            1,
            ResponseData::Hello(HelloResult {
                api_version: 1,
                server: "goonzu-rs-supervisor".into(),
                server_version: "0.1.0".into(),
                features: vec![],
            }),
        ),
        Response::ok(
            2,
            ResponseData::StatusGet(StatusResult {
                api_version: 1,
                server_version: "0.1.0".into(),
                uptime_s: 86_400,
                services_running: 4,
                services_total: 5,
            }),
        ),
        Response::ok(
            3,
            ResponseData::ServicesList(ServicesListResult {
                services: vec![ServiceStatus {
                    name: "game".into(),
                    state: ServiceState::Running,
                    pid: Some(4242),
                    uptime_s: Some(3600),
                    cpu_pct: Some(12.5),
                    rss_bytes: Some(268_435_456),
                    connections: Some(37),
                    since_ms: Some(1_700_000_000_000u64),
                }],
            }),
        ),
        Response::ok(
            4,
            ResponseData::LogsTail(LogsTailResult {
                subscription: 1,
                replayed: 87,
            }),
        ),
        Response::ok(5, ResponseData::LogsTailStop(Ack {})),
        Response::ok(
            6,
            ResponseData::ConfigGet(ConfigGetResult {
                entries: vec![
                    ConfigEntry {
                        key: "db.password".into(),
                        // Never populated for a secret. See ConfigEntry::secret.
                        value: None,
                        kind: ConfigKind::String,
                        secret: true,
                        read_only: false,
                        comment: Some("from the OS keychain".into()),
                    },
                    ConfigEntry {
                        key: "server.game.tick_hz".into(),
                        value: Some("30".into()),
                        kind: ConfigKind::Integer,
                        secret: false,
                        read_only: false,
                        comment: None,
                    },
                ],
            }),
        ),
        Response::ok(
            7,
            ResponseData::ConfigSet(ConfigSetResult {
                applied: 1,
                needs_restart: vec!["game".into()],
            }),
        ),
        Response::ok(
            8,
            ResponseData::DbPing(DbPingResult {
                reachable: true,
                latency_ms: Some(3),
                error: None,
                driver: "postgresql/18.6".into(),
            }),
        ),
        Response::ok(
            9,
            ResponseData::PlayersList(PlayersListResult {
                players: vec![PlayerSummary {
                    player_id: 1042,
                    name: "하늘".into(),
                    online: true,
                }],
                total: 1,
            }),
        ),
        Response::ok(
            10,
            ResponseData::ServiceStart(ServiceActionResult {
                service: "game".into(),
                changed: true,
                state: ServiceState::Running,
            }),
        ),
        Response::ok(
            11,
            ResponseData::ServiceStop(ServiceActionResult {
                service: "game".into(),
                changed: true,
                state: ServiceState::Stopped,
            }),
        ),
        Response::ok(
            12,
            ResponseData::ServiceRestart(ServiceActionResult {
                service: "front".into(),
                changed: true,
                state: ServiceState::Running,
            }),
        ),
        Response::ok(
            13,
            ResponseData::SessionKick(SessionKickResult {
                kicked: 1,
                not_found: 0,
            }),
        ),
        Response::error(
            14,
            OpsError::new(
                OpsErrorCode::NotImplemented,
                "service.kick is part of the contract but not implemented by this server",
            ),
        ),
        Response::error(15, OpsError::new(OpsErrorCode::Unauthorized, "bad token")),
    ];

    let events = vec![EventFrame {
        subscription: 1,
        event: Event::LogsLine(LogLine {
            seq: 88,
            ts_ms: 1_700_000_000_000,
            level: LogLevel::Warn,
            stream: "game".into(),
            target: "GoonZuWorld::Char::Attack".into(),
            // Deliberately contains a newline: this is why framing is
            // length-prefixed rather than newline-delimited.
            message: "damage applied\n  crit: 128".into(),
            truncated: false,
            lossy: true,
        }),
    }];

    // Assembled as a `serde_json::Value` and serialised once, rather than by
    // string concatenation. Hand-assembled JSON is exactly the mistake that
    // made the T-005 LZF oracle compare against a fabricated stream: the
    // output looked plausible and parsed under PowerShell's lenient reader
    // while being invalid JSON. Let serde do the escaping and the quoting.
    let doc = serde_json::json!({
        "_comment": "Generated by `cargo run -p gz-ops --bin export-schema`. \
                     Do not edit. Each entry is the real serde output for one message.",
        "method_names": Method::ALL.iter().map(|m| m.as_str()).collect::<Vec<_>>(),
        "requests": requests,
        "responses": responses,
        "events": events,
    });

    let mut out =
        serde_json::to_string_pretty(&doc).expect("the example document is always serialisable");
    out.push('\n');
    out
}

fn example_call(call: Call) -> serde_json::Value {
    let req = Request { id: 1, call };
    serde_json::to_value(req).expect("Request always serialises")
}

fn write(path: &Path, contents: &str) -> std::io::Result<()> {
    let mut f = std::fs::File::create(path)?;
    f.write_all(contents.as_bytes())
}

fn is_current(path: &Path, contents: &str) -> bool {
    std::fs::read_to_string(path).is_ok_and(|c| c == contents)
}

/// Replace the generated method table in `readme`, keeping everything else.
///
/// The table is delimited by HTML comments so the hand-written prose around it
/// is never at risk. The README is required to have the markers: silently
/// doing nothing when they are absent would let the table rot.
fn splice_method_table(readme: &str, table: &str) -> String {
    const START: &str = "<!-- BEGIN GENERATED METHOD TABLE -->";
    const END: &str = "<!-- END GENERATED METHOD TABLE -->";

    let start = readme.find(START).unwrap_or_else(|| {
        panic!("README.md is missing the `{START}` marker; cannot regenerate the method table")
    });
    let end = readme.find(END).unwrap_or_else(|| {
        panic!("README.md is missing the `{END}` marker; cannot regenerate the method table")
    });

    let mut out = String::with_capacity(readme.len() + table.len());
    out.push_str(&readme[..start]);
    out.push_str(START);
    out.push('\n');
    out.push_str(table);
    out.push('\n');
    out.push_str(END);
    out.push_str(&readme[end + END.len()..]);
    out
}
