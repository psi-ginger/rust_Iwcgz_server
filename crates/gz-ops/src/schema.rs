//! JSON Schema generation.
//!
//! The schema is **derived** from the Rust types, never hand-written. A
//! hand-written schema drifts from the code the moment someone adds a field,
//! and a frontend generated against a stale schema fails at runtime instead
//! of at compile time.
//!
//! `schema.json` is committed so the frontend does not need a Rust toolchain
//! to read the contract. `tests/contract.rs` re-derives it and fails if the
//! committed file differs, so drift is caught in CI -- the same arrangement
//! `codegen --check` uses for the opcode tables.

use schemars::Schema;
use schemars::generate::SchemaSettings;
use schemars::transform::{Transform, transform_subschemas};
use serde_json::{Map, Value};

use crate::{
    envelope::{EventFrame, OpsError, Request, Response},
    logs::LogLine,
    methods::{Call, Method, Phase, ResponseData},
};

/// The JSON Schema dialect `schema.json` is written in.
///
/// Pinned explicitly rather than inherited from schemars' default, which its
/// own documentation notes "is liable to change in a future version". This file
/// is a committed artifact that a frontend generates types from, so a silent
/// dialect change under a dependency bump would be a breaking change arriving
/// in a patch release.
///
/// Spelled out as a literal rather than read from `schemars::consts`: the URL
/// is fixed by the specification, so there is nothing to keep in sync, and it
/// avoids depending on schemars' internal module layout.
pub const SCHEMA_DRAFT: &str = "https://json-schema.org/draft/2020-12/schema";

/// Stable identifier for the contract document.
pub const SCHEMA_ID: &str = "https://goonzu.rs/schemas/ops-api.json";

/// Every type that must appear in `$defs`.
///
/// Listed explicitly so that adding a message type without adding it here is a
/// compile error, rather than a schema that quietly stops describing it.
const CONTRACT_TYPES: &[&str] = &[
    "Request",
    "Response",
    "EventFrame",
    "Call",
    "ResponseData",
    "Method",
    "Phase",
    "OpsError",
    "LogLine",
];

/// The contract schema as a JSON value.
pub fn contract_schema_value() -> Value {
    let mut generator = SchemaSettings::draft2020_12()
        // `with_transform` runs after generation, which is what
        // DropNullFromOptionals needs: it edits the finished schema rather
        // than influencing how the schema is built. (`SchemaSettings::transforms`
        // is the during-generation hook.)
        .with_transform(DropNullFromOptionals)
        .into_generator();

    // Registering each root subschema puts its type and everything it reaches
    // into the definitions map.
    let _ = generator.subschema_for::<Request>();
    let _ = generator.subschema_for::<Response>();
    let _ = generator.subschema_for::<EventFrame>();
    let _ = generator.subschema_for::<Call>();
    let _ = generator.subschema_for::<ResponseData>();
    let _ = generator.subschema_for::<Method>();
    let _ = generator.subschema_for::<Phase>();
    let _ = generator.subschema_for::<OpsError>();
    let _ = generator.subschema_for::<LogLine>();

    let defs = generator.take_definitions(true);

    // Every name in CONTRACT_TYPES must have landed in $defs. If schemars ever
    // renames one, this is where it surfaces.
    for name in CONTRACT_TYPES {
        let key = name.to_ascii_lowercase();
        assert!(
            defs.contains_key(&key) || defs.contains_key(*name),
            "contract type {name} is missing from $defs (have: {:?})",
            defs.keys().collect::<Vec<_>>()
        );
    }

    let mut root = Map::new();
    root.insert("$schema".into(), Value::from(SCHEMA_DRAFT));
    root.insert("$id".into(), Value::from(SCHEMA_ID));
    root.insert("title".into(), Value::from("GoonZu local ops API"));
    root.insert("description".into(), Value::from(FORMAT_DESCRIPTION));
    root.insert("x-contract-version".into(), Value::from(crate::API_VERSION));
    root.insert("$defs".into(), Value::Object(defs));

    Value::Object(root)
}

/// Framing rules, embedded in the schema so a consumer reading only
/// `schema.json` still learns them.
const FORMAT_DESCRIPTION: &str = "\
Frame on the wire: a 4-byte big-endian length followed by that many bytes of \
UTF-8 JSON. Frames are NOT newline-delimited: log lines and player names are \
untrusted text that can contain newlines, and splitting on one would \
desynchronise the stream. A frame is a Response if and only if it has an `ok` \
member; anything else is an EventFrame routed by `subscription`. \
`ops.hello` must be the first request. \
\
An absent optional member is OMITTED, never sent as null. \
\
Integer members carry the non-standard `format` values `uint32` and `uint64`, \
which record the Rust source width. They are annotations, not registered JSON \
Schema formats, so a strict validator will warn about them; register them as \
no-ops before compiling. The width matters to a consumer because a u64 above \
2^53 does not survive a round trip through a JavaScript Number.";

/// Removes `null` from every optional property.
///
/// serde is configured so that absent optionals are **omitted** rather than
/// sent as `null` (`skip_serializing_if = "Option::is_none"` on every
/// `Option<_>` in this crate). schemars does not take `skip_serializing_if`
/// into account for type purposes, so it emits `null` as an accepted value and
/// the schema both misdescribes the wire format and, worse, lets a frame
/// through that the contract says cannot exist:
///
/// ```text
/// {"id":1,"ok":true,"result":null}   <- passed validation before this transform
/// ```
///
/// `Response::ok` and `Response::error` make that frame unconstructible in
/// Rust, so a schema that accepts it hands a frontend a type it can never
/// actually receive -- and, worse, one whose failure mode is a null deref in
/// the consumer rather than a rejected frame.
///
/// Two shapes have to be handled, because schemars uses each for a different
/// reason: a nullable primitive comes out as `type: ["integer","null"]`, while
/// a nullable `$ref` comes out as `anyOf: [{$ref}, {"type":"null"}]`.
#[derive(Clone)]
struct DropNullFromOptionals;

impl Transform for DropNullFromOptionals {
    fn transform(&mut self, schema: &mut Schema) {
        // Take the properties out, edit them, and put them back, so the
        // transform does not fight the borrow checker.
        if let Some(Value::Object(properties)) = schema.get("properties").cloned() {
            let mut properties = properties;
            for property in properties.values_mut() {
                drop_null(property);
            }
            schema.insert("properties".to_string(), Value::Object(properties));
        }
        transform_subschemas(self, schema);
    }
}

/// Strip `null` from one schema node, collapsing the wrapper if it becomes
/// redundant. `Vec<Option<T>>` does not occur in this contract; if it ever
/// did, the inner `items` node would be left untouched and `contract.rs` would
/// catch it by finding a reachable `null`.
fn drop_null(node: &mut Value) {
    // Shape 1: `"type": ["integer", "null"]`.
    if let Some(Value::Array(types)) = node.get_mut("type") {
        types.retain(|t| t != "null");
        if types.len() == 1 {
            let only = types.remove(0);
            node.as_object_mut().map(|o| o.insert("type".into(), only));
        }
    }

    // Shape 2: `"anyOf": [{"$ref": ...}, {"type": "null"}]`.
    if let Some(Value::Array(any_of)) = node.get_mut("anyOf") {
        any_of.retain(|alt| !is_null_only(alt));
        match any_of.len() {
            0 => *node = Value::Bool(false),
            1 => {
                let only = any_of.remove(0);
                // Keep this node's description, which sits alongside `anyOf`.
                let description = node.get("description").cloned();
                *node = only;
                if let Some(d) = description {
                    node.as_object_mut()
                        .map(|o| o.entry("description").or_insert(d));
                }
            }
            _ => {}
        }
    }
}

fn is_null_only(value: &Value) -> bool {
    value
        .as_object()
        .is_some_and(|o| o.len() == 1 && o.get("type") == Some(&Value::from("null")))
}

/// The contract schema as pretty-printed JSON, ending in exactly one newline so
/// the committed file diffs cleanly.
pub fn contract_schema_json() -> String {
    let value = contract_schema_value();
    let mut s =
        serde_json::to_string_pretty(&value).expect("contract schema is always serialisable");
    s.push('\n');
    s
}

/// The generated method table, as Markdown.
///
/// Rendered from [`Method::ALL`] plus the metadata on each variant, so the
/// crate README cannot claim a method that does not exist.
pub fn method_table_markdown() -> String {
    let mut s =
        String::from("| method | phase | mutating | description |\n| --- | --- | --- | --- |\n");
    for m in Method::ALL {
        s.push_str(&format!(
            "| `{}` | {} | {} | {} |\n",
            m.as_str(),
            match m.phase() {
                Phase::ReadOnly => "read-only",
                Phase::Reserved => "reserved",
            },
            if m.is_mutating() { "yes" } else { "no" },
            m.description()
        ));
    }
    s
}
