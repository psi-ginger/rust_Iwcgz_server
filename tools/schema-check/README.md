# schema-check

Validates the committed `crates/gz-ops/schema.json` against the wire format it
claims to describe, using a real JSON Schema validator (ajv).

This is a **development tool, not part of the build**. The Rust workspace has no
Node dependency on purpose — the in-repo guard is
`crates/gz-ops/tests/contract.rs`, which runs under `cargo test`. This tool is
what convinced us that guard was looking in the right places.

## Usage

```
cd tools/schema-check
npm install
node validate.js
```

Exit code 0 means every check passed. Exit code 1 means a check failed. Exit
code 2 means the tool could not run (ajv not installed, or `schema.json`
missing).

## What it checks

| # | Check | Why |
| --- | --- | --- |
| 1 | `Request`, `Response` and `EventFrame` compile | a schema ajv cannot compile is worthless to a consumer |
| 2 | `schema.json` validates against the 2020-12 **meta-schema** | the whole document is a legal schema, not just the parts ajv happened to reach |
| 3 | all 13 requests, 15 responses and 1 event in `schema-examples.json` validate | the committed examples really are instances of the schema |
| 4 | 15 malformed frames are **rejected** | the point of the exercise |

Check 4 is the one that matters. A schema that accepts everything passes every
positive check while being useless to a frontend, so the negative cases carry
half the weight. Among them:

- a response with no `ok` member
- a response whose `result` names a method that does not exist
- `{"ok":true,"result":null}`
- `{"ok":true}` with no result at all
- `{"ok":false}` carrying a result
- a frame carrying both `result` and `error`
- an event that carries `ok`
- an optional field sent as `null` rather than omitted
- an unknown error code

## Two defects this found

Both were invisible to the Rust tests at the time, and both would have shipped
a contract that lied to its consumers.

**1. `{"ok":true,"result":null}` validated.** Every `Option<_>` in gz-ops
carries `skip_serializing_if = "Option::is_none"`, so serde never puts `null`
on the wire — an absent optional is omitted. schemars does not account for
`skip_serializing_if` when working out types, so it emitted `null` as an
accepted value. A frontend generated from that schema carries a `result` it
must null-check, and its failure mode is a null deref rather than a rejected
frame. Fixed by `schema::DropNullFromOptionals`, which strips `null` from
generated schemas. Note that two different shapes have to be handled: a nullable
primitive comes out as `type: ["integer","null"]`, a nullable `$ref` as
`anyOf: [{$ref},{"type":"null"}]`.

**2. `{"ok":true}` with no result at all validated.** JSON Schema's `required`
is unconditional, so "`ok` is true ⇒ `result` is present" is only expressible
as `if`/`then`. Fixed by an `#[schemars(extend(...))]` block on `Response`.

## `uint32` / `uint64` formats

schemars emits `format: "uint32"` and `format: "uint64"` to record the Rust
source width. These are **not registered JSON Schema formats** — they are
annotations, and a strict validator warns about every one of them. This tool
registers them as no-ops before compiling, which is what `schema.json`'s own
`description` tells consumers to do.

Keeping them is deliberate: the width is real information for a frontend,
because a `u64` above 2^53 does not survive a round trip through a JavaScript
`Number`.

## Falsification

The in-repo guards were themselves checked for the ability to fail — a green
test proves nothing until you have watched it go red. Five mutations, each of
which must turn a specific named test red:

| # | Mutation | Must fail |
| --- | --- | --- |
| 1 | remove `DropNullFromOptionals` from the generator settings | `no_reachable_schema_accepts_null` |
| 2 | strip the `#[schemars(extend)]` from `Response` | `ok_true_requires_a_result_object` |
| 3 | add a method to `Method::ALL` with no `Call` variant | `call_and_response_data_agree_with_method_list` |
| 4 | tamper with `schema.json` | `committed_schema_json_is_up_to_date` |
| 4b | tamper with `schema.json`, then run `export-schema --check` | exit code 2, and the file is **not** repaired |
| 4c | same, while the file is still untracked in git | exit code 2 |
| 5 | edit the README's generated method table | `readme_method_table_is_up_to_date` |

All seven were run on 2026-10-02 and all seven produced the expected failure.

Cases 1 and 2 both turning `ok_true_requires_a_result_object` red is correct:
removing the transform makes `Response.result` nullable again, so that test
covers both defects.

### Case 4c exists because of a bug found while writing this

`check.ps1` step 5 originally used `git diff --quiet` to decide whether the
committed artifacts were stale. That silently passes for any file git does not
track — which is exactly the state a *newly generated* artifact is in. The step
would have stayed green forever while `schema.json` was wrong.

`export-schema --check` now compares the files against what the types derive to
right now, which has no blind spot. Case 4c exists to keep that property from
being reintroduced by a future "simplification" back to `git diff`.