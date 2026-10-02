//! Contract tests.
//!
//! These exist to make drift *loud*. The failure mode being defended against is
//! not a crash — it is a frontend built against a schema that has quietly
//! stopped describing the server.

use gz_ops::{
    Call, Event, EventFrame, Method, OpsErrorCode, Request, Response, ResponseData,
    envelope::OpsError, framing, logs::LogLevel, methods::*,
};

// ---------------------------------------------------------------------------
// Method / Call / ResponseData agreement
// ---------------------------------------------------------------------------

/// Every method has a call, a response payload, and nothing extra in either.
#[test]
fn call_and_response_data_agree_with_method_list() {
    // Build one representative value per variant so both enums are fully
    // covered. Adding a variant to Call without adding it here fails to
    // compile, which is the point.
    let calls: Vec<(Method, Call)> = vec![
        (
            Method::Hello,
            Call::Hello(HelloParams {
                client: "t".into(),
                client_version: "0".into(),
                min_api_version: 1,
                token: None,
            }),
        ),
        (Method::StatusGet, Call::StatusGet(NoParams {})),
        (Method::ServicesList, Call::ServicesList(NoParams {})),
        (
            Method::LogsTail,
            Call::LogsTail(LogsTailParams {
                stream: None,
                min_level: None,
                contains: None,
                replay: None,
            }),
        ),
        (
            Method::LogsTailStop,
            Call::LogsTailStop(LogsTailStopParams { subscription: 1 }),
        ),
        (
            Method::ConfigGet,
            Call::ConfigGet(ConfigGetParams { prefix: None }),
        ),
        (
            Method::ConfigSet,
            Call::ConfigSet(ConfigSetParams {
                entries: vec![ConfigSetEntry {
                    key: "a.b".into(),
                    value: "1".into(),
                }],
            }),
        ),
        (Method::DbPing, Call::DbPing(NoParams {})),
        (
            Method::PlayersList,
            Call::PlayersList(PlayersListParams {
                online_only: None,
                search: None,
                limit: None,
                offset: None,
            }),
        ),
        (
            Method::ServiceStart,
            Call::ServiceStart(ServiceActionParams {
                service: "game".into(),
                timeout_s: None,
            }),
        ),
        (
            Method::ServiceStop,
            Call::ServiceStop(ServiceActionParams {
                service: "game".into(),
                timeout_s: None,
            }),
        ),
        (
            Method::ServiceRestart,
            Call::ServiceRestart(ServiceActionParams {
                service: "game".into(),
                timeout_s: None,
            }),
        ),
        (
            Method::SessionKick,
            Call::SessionKick(SessionKickParams {
                player_id: Some(1),
                account_id: None,
                reason: None,
            }),
        ),
    ];

    let responses: Vec<(Method, ResponseData)> = vec![
        (
            Method::Hello,
            ResponseData::Hello(HelloResult {
                api_version: 1,
                server: "s".into(),
                server_version: "0".into(),
                features: vec![],
            }),
        ),
        (
            Method::StatusGet,
            ResponseData::StatusGet(StatusResult {
                api_version: 1,
                server_version: "0".into(),
                uptime_s: 0,
                services_running: 0,
                services_total: 0,
            }),
        ),
        (
            Method::ServicesList,
            ResponseData::ServicesList(ServicesListResult { services: vec![] }),
        ),
        (
            Method::LogsTail,
            ResponseData::LogsTail(LogsTailResult {
                subscription: 1,
                replayed: 0,
            }),
        ),
        (Method::LogsTailStop, ResponseData::LogsTailStop(Ack {})),
        (
            Method::ConfigGet,
            ResponseData::ConfigGet(ConfigGetResult { entries: vec![] }),
        ),
        (
            Method::ConfigSet,
            ResponseData::ConfigSet(ConfigSetResult {
                applied: 0,
                needs_restart: vec![],
            }),
        ),
        (
            Method::DbPing,
            ResponseData::DbPing(DbPingResult {
                reachable: true,
                latency_ms: Some(1),
                error: None,
                driver: "postgresql".into(),
            }),
        ),
        (
            Method::PlayersList,
            ResponseData::PlayersList(PlayersListResult {
                players: vec![],
                total: 0,
            }),
        ),
        (
            Method::ServiceStart,
            ResponseData::ServiceStart(ServiceActionResult {
                service: "game".into(),
                changed: true,
                state: ServiceState::Running,
            }),
        ),
        (
            Method::ServiceStop,
            ResponseData::ServiceStop(ServiceActionResult {
                service: "game".into(),
                changed: true,
                state: ServiceState::Stopped,
            }),
        ),
        (
            Method::ServiceRestart,
            ResponseData::ServiceRestart(ServiceActionResult {
                service: "game".into(),
                changed: true,
                state: ServiceState::Running,
            }),
        ),
        (
            Method::SessionKick,
            ResponseData::SessionKick(SessionKickResult {
                kicked: 1,
                not_found: 0,
            }),
        ),
    ];

    for (m, c) in &calls {
        assert_eq!(c.method(), *m, "Call reports the wrong method");
    }
    for (m, r) in &responses {
        assert_eq!(r.method(), *m, "ResponseData reports the wrong method");
    }

    let listed: Vec<Method> = Method::ALL.to_vec();
    let call_methods: Vec<Method> = calls.iter().map(|(m, _)| *m).collect();
    let resp_methods: Vec<Method> = responses.iter().map(|(m, _)| *m).collect();

    assert_eq!(
        listed, call_methods,
        "Method::ALL and the Call variants disagree"
    );
    assert_eq!(
        listed, resp_methods,
        "Method::ALL and the ResponseData variants disagree"
    );
}

// ---------------------------------------------------------------------------
// Envelope invariants
// ---------------------------------------------------------------------------

fn sample_request() -> Request {
    Request {
        id: 7,
        call: Call::LogsTail(LogsTailParams {
            stream: Some("game".into()),
            min_level: Some(LogLevel::Warn),
            contains: None,
            replay: Some(200),
        }),
    }
}

#[test]
fn request_serialises_to_the_documented_flat_shape() {
    let json = serde_json::to_value(sample_request()).unwrap();
    assert_eq!(json["id"], 7);
    assert_eq!(json["method"], "logs.tail");
    assert!(json["params"].is_object(), "params must be an object");
    assert_eq!(json["params"]["min_level"], "warn");
}

#[test]
fn request_round_trips_through_json() {
    let req = sample_request();
    let text = serde_json::to_string(&req).unwrap();
    let back: Request = serde_json::from_str(&text).unwrap();
    assert_eq!(req, back);
}

#[test]
fn absent_optional_params_do_not_reappear_as_nulls() {
    let req = Request {
        id: 1,
        call: Call::StatusGet(NoParams {}),
    };
    let json = serde_json::to_value(&req).unwrap();
    let params = json["params"].as_object().unwrap();
    // No nulls for members that were never set.
    assert!(params.is_empty(), "NoParams must serialise to {{}}");
    assert!(
        json.get("filter").is_none(),
        "absent optionals must be omitted, not null"
    );
}

#[test]
fn ok_and_error_responses_are_well_formed() {
    let ok = Response::ok(
        1,
        ResponseData::DbPing(DbPingResult {
            reachable: true,
            latency_ms: Some(3),
            error: None,
            driver: "postgresql/18.6".into(),
        }),
    );
    assert!(ok.is_well_formed());
    assert!(ok.is_ok());
    let json = serde_json::to_value(&ok).unwrap();
    assert_eq!(json["ok"], true);
    assert!(json.get("error").is_none());

    let bad = Response::error(1, OpsError::new(OpsErrorCode::NotImplemented, "later"));
    assert!(bad.is_well_formed());
    assert!(!bad.is_ok());
    let json = serde_json::to_value(&bad).unwrap();
    assert_eq!(json["ok"], false);
    assert!(json.get("result").is_none());
    assert_eq!(json["error"]["code"], "not_implemented");
}

#[test]
fn response_round_trips_through_json() {
    for resp in [
        Response::ok(
            9,
            ResponseData::StatusGet(StatusResult {
                api_version: 1,
                server_version: "0.1.0".into(),
                uptime_s: 3600,
                services_running: 4,
                services_total: 5,
            }),
        ),
        Response::error(9, OpsError::new(OpsErrorCode::Unauthorized, "bad token")),
    ] {
        let text = serde_json::to_string(&resp).unwrap();
        let back: Response = serde_json::from_str(&text).unwrap();
        assert_eq!(resp, back);
        assert!(back.is_well_formed());
    }
}

/// The discriminator the crate docs promise: a frame is a response iff it has
/// an `ok` member. If this ever stops holding, a client cannot route frames.
#[test]
fn response_and_event_frames_are_unambiguously_distinguishable() {
    let resp = Response::ok(1, ResponseData::LogsTailStop(Ack {}));
    let event = EventFrame {
        subscription: 1,
        event: Event::LogsLine(gz_ops::LogLine {
            seq: 88,
            ts_ms: 1_700_000_000_000,
            level: LogLevel::Warn,
            stream: "game".into(),
            target: "GoonZuWorld::Char".into(),
            message: "a message\nwith a newline in it".into(),
            truncated: false,
            lossy: true,
        }),
    };

    let resp_json = serde_json::to_value(&resp).unwrap();
    let event_json = serde_json::to_value(&event).unwrap();

    assert!(resp_json.get("ok").is_some(), "responses always carry `ok`");
    assert!(
        event_json.get("ok").is_none(),
        "events must never carry `ok`, or routing is ambiguous"
    );
    assert!(event_json.get("subscription").is_some());

    // And both survive the round trip.
    let back: EventFrame = serde_json::from_value(event_json).unwrap();
    assert_eq!(back, event);

    // A log line containing a newline must survive intact -- this is the whole
    // reason framing is length-prefixed.
    match &back.event {
        Event::LogsLine(l) => {
            assert_eq!(l.message, "a message\nwith a newline in it");
            assert!(l.lossy);
        }
    }
}

// ---------------------------------------------------------------------------
// Secret handling
// ---------------------------------------------------------------------------

#[test]
fn secret_config_entries_serialise_without_a_value() {
    let entry = ConfigEntry {
        key: "db.password".into(),
        value: None,
        kind: ConfigKind::String,
        secret: true,
        read_only: false,
        comment: Some("from the OS keychain".into()),
    };
    let json = serde_json::to_value(&entry).unwrap();
    assert_eq!(json["secret"], true);
    assert!(
        json.get("value").is_none(),
        "a secret entry must not put `value` on the wire at all"
    );
    assert_eq!(json["key"], "db.password");
}

// ---------------------------------------------------------------------------
// Framing integration
// ---------------------------------------------------------------------------

#[test]
fn a_request_survives_the_full_frame_path() {
    let req = sample_request();
    let payload = serde_json::to_vec(&req).unwrap();

    let mut wire = framing::encode(&payload);
    assert!(payload.len() < framing::MAX_FRAME_BYTES);

    let got = framing::decode(&mut wire).unwrap().expect("one frame");
    let back: Request = serde_json::from_slice(&got).unwrap();
    assert_eq!(req, back);
    assert!(wire.is_empty());
}

#[test]
fn three_requests_on_one_stream_resynchronise_cleanly() {
    let reqs = vec![
        Request {
            id: 1,
            call: Call::StatusGet(NoParams {}),
        },
        sample_request(),
        Request {
            id: 3,
            call: Call::DbPing(NoParams {}),
        },
    ];

    let mut wire = Vec::new();
    for r in &reqs {
        wire.extend(framing::encode(&serde_json::to_vec(r).unwrap()));
    }

    let mut got = Vec::new();
    while let Some(payload) = framing::decode(&mut wire).unwrap() {
        got.push(serde_json::from_slice::<Request>(&payload).unwrap());
    }

    assert_eq!(reqs, got);
    assert!(wire.is_empty(), "no bytes left over");
}

// ---------------------------------------------------------------------------
// Committed artifacts
// ---------------------------------------------------------------------------

/// `schema.json` must match what the types derive to right now.
#[test]
fn committed_schema_json_is_up_to_date() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/schema.json");
    let committed = std::fs::read_to_string(path).unwrap_or_else(|e| {
        panic!("cannot read {path}: {e}. Run: cargo run -p gz-ops --bin export-schema")
    });
    let derived = gz_ops::schema::contract_schema_json();
    assert_eq!(
        committed, derived,
        "schema.json is stale. Run: cargo run -p gz-ops --bin export-schema"
    );
}

/// The committed schema must actually mention every method by name. A schema
/// that parses but omits methods would satisfy the equality check above while
/// being useless to a frontend.
#[test]
fn committed_schema_names_every_method() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/schema.json");
    let text = std::fs::read_to_string(path).expect("schema.json present");
    for m in Method::ALL {
        assert!(
            text.contains(m.as_str()),
            "schema.json does not mention method {}",
            m.as_str()
        );
    }
}

/// The README method table is generated from `Method::ALL`, so it must match.
#[test]
fn readme_method_table_is_up_to_date() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/README.md");
    let readme = std::fs::read_to_string(path).expect("README present");
    let table = gz_ops::schema::method_table_markdown();

    assert!(
        readme.contains(table.trim_end()),
        "README method table is stale. Run: cargo run -p gz-ops --bin export-schema"
    );
    assert!(
        readme.contains("<!-- BEGIN GENERATED METHOD TABLE -->"),
        "README lost its generated-table delimiters"
    );
}

/// `schema.json` must be valid JSON. Cheapest possible check that the exporter
/// did not write a truncated file.
#[test]
fn committed_schema_is_valid_json() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/schema.json");
    let text = std::fs::read_to_string(path).expect("schema.json present");
    let value: serde_json::Value =
        serde_json::from_str(&text).expect("schema.json must parse as JSON");
    assert!(value.get("$defs").is_some(), "root must carry $defs");
    assert_eq!(value["title"], serde_json::json!("GoonZu local ops API"));
}

/// No reachable part of the schema may accept `null`.
///
/// Every `Option<_>` in this crate carries `skip_serializing_if`, so an absent
/// optional is omitted rather than sent as null. `schema::DropNullFromOptionals`
/// is what makes the derived schema agree; this test is the guard on that
/// transform, because the failure it prevents is invisible -- a schema that
/// accepts null still round-trips every real frame.
///
/// Two shapes count as "accepts null": a `type` array containing `"null"`, and
/// an `anyOf` with a null-only alternative.
#[test]
fn no_reachable_schema_accepts_null() {
    fn walk(node: &serde_json::Value, path: &str, hits: &mut Vec<String>) {
        match node {
            serde_json::Value::Object(map) => {
                // `"type": ["integer", "null"]`
                if let Some(serde_json::Value::Array(types)) = map.get("type")
                    && types.iter().any(|t| t == "null")
                {
                    hits.push(format!("{path}.type"));
                }
                // `"anyOf": [{...}, {"type": "null"}]`
                if let Some(serde_json::Value::Array(any_of)) = map.get("anyOf")
                    && any_of
                        .iter()
                        .any(|alt| alt.get("type") == Some(&serde_json::Value::from("null")))
                {
                    hits.push(format!("{path}.anyOf"));
                }
                for (k, v) in map {
                    walk(v, &format!("{path}.{k}"), hits);
                }
            }
            serde_json::Value::Array(items) => {
                for (i, v) in items.iter().enumerate() {
                    walk(v, &format!("{path}[{i}]"), hits);
                }
            }
            _ => {}
        }
    }

    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/schema.json");
    let schema: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("schema.json"))
            .expect("valid JSON");

    let mut hits = Vec::new();
    walk(&schema, "$", &mut hits);
    assert!(
        hits.is_empty(),
        "these schema nodes accept null, but serde never emits it: {hits:?}"
    );
}

/// The `ok`/`result` invariant must be visible to a schema consumer, not just
/// to Rust callers.
///
/// `{"ok":true,"result":null}` was accepted by the schema before
/// `DropNullFromOptionals` existed, and `{"ok":true}` with no result at all was
/// accepted before the `if`/`then` pair was added to `Response`. A frontend
/// generated from such a schema would carry members it must null-check, and its
/// failure mode would be a null deref rather than a rejected frame.
#[test]
fn ok_true_requires_a_result_object() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/schema.json");
    let schema: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("schema.json"))
            .expect("valid JSON");

    let response = &schema["$defs"]["Response"];

    // No `result` may be null.
    let result = &response["properties"]["result"];
    let rendered = result.to_string();
    assert!(
        !rendered.contains("null"),
        "Response.result must not accept null: {rendered}"
    );

    // The conditional constraint must be present: JSON Schema's `required` is
    // unconditional, so `ok: true => result required` is only expressible as
    // if/then. Assert the shape rather than just the presence of the key, so a
    // half-written branch is caught too.
    let branches = response["allOf"]
        .as_array()
        .expect("Response schema must carry the ok/result if-then constraint");

    let expects = |ok: bool| {
        branches.iter().any(|b| {
            b["if"]["properties"]["ok"]["const"] == serde_json::json!(ok)
                && b["then"]["required"] == serde_json::json!(["result"])
                && b["then"]["not"]["required"] == serde_json::json!(["error"])
        })
    };
    assert!(
        expects(true),
        "missing branch: ok=true requires `result` and forbids `error`. Branches were: {branches:?}"
    );

    let forbids = |ok: bool| {
        branches.iter().any(|b| {
            b["if"]["properties"]["ok"]["const"] == serde_json::json!(ok)
                && b["then"]["required"] == serde_json::json!(["error"])
                && b["then"]["not"]["required"] == serde_json::json!(["result"])
        })
    };
    assert!(
        forbids(false),
        "missing branch: ok=false requires `error` and forbids `result`. Branches were: {branches:?}"
    );
}
