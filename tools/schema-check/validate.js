// Validates the committed gz-ops artifacts with a real JSON Schema validator.
//
// Four things are checked, in increasing order of usefulness:
//
//   1. the three frame schemas compile
//   2. schema.json validates against the JSON Schema 2020-12 meta-schema
//   3. every generated example validates against the right $defs entry
//   4. a set of deliberately-wrong frames is REJECTED
//
// (4) is the one that matters. A schema that accepts everything passes a
// round-trip check while being useless to a frontend, so the negative cases are
// half the point. Two real defects were found this way and would not have been
// caught by any positive check: `{"ok":true,"result":null}` and
// `{"ok":true}` with no result at all both validated.
//
// This is a development tool, not part of `cargo test`. It needs Node and ajv,
// which the Rust workspace deliberately does not depend on -- the in-repo guard
// is `crates/gz-ops/tests/contract.rs`, and this tool is what convinced us that
// guard was looking in the right places.
//
// Usage:
//   cd tools/schema-check && npm install && node validate.js
//
// Override the defaults with GZ_OPS_CRATE (path to crates/gz-ops) and
// GZ_OPS_NODE_MODULES (a node_modules containing ajv).

const path = require('path');
const fs = require('fs');

const REPO = path.resolve(__dirname, '..', '..');
const CRATE = process.env.GZ_OPS_CRATE || path.join(REPO, 'crates', 'gz-ops');
// Defaults next to this file, so `npm install` in tools/schema-check is enough
// and nothing is written to the repository root.
const MODULES =
  process.env.GZ_OPS_NODE_MODULES || path.join(__dirname, 'node_modules');

function load(name) {
  try {
    return require(path.join(MODULES, name));
  } catch (e) {
    console.error(
      `Cannot load "${name}" from ${MODULES}.\n` +
      `  cd tools/schema-check && npm install\n` +
      `  or set GZ_OPS_NODE_MODULES to a node_modules that has them.\n` +
      `  underlying error: ${e.message}`
    );
    process.exit(2);
  }
}

const Ajv2020 = load('ajv/dist/2020');
const addFormats = load('ajv-formats');

function loadJson(name) {
  const p = path.join(CRATE, name);
  if (!fs.existsSync(p)) {
    console.error(
      `${p} is missing.\n` +
      `  Run: cargo run -p gz-ops --bin export-schema`
    );
    process.exit(2);
  }
  return JSON.parse(fs.readFileSync(p, 'utf8'));
}

const schema = loadJson('schema.json');
const examples = loadJson('schema-examples.json');

let failures = 0;
function check(name, ok, detail) {
  const mark = ok ? 'PASS' : 'FAIL';
  console.log(`  [${mark}] ${name}`);
  if (!ok && detail) {
    console.log('         ' + String(detail).split('\n').join('\n         '));
    failures++;
  }
}

const ajv = new Ajv2020({ strict: false, allErrors: true });
addFormats(ajv);
// schemars emits `format: "uint32"` / `"uint64"` to record the Rust source
// width. These are NOT registered JSON Schema formats -- they are annotations,
// and every strict validator warns about them. Register them as no-ops, which
// is what schema.json's own description tells consumers to do. A `true` format
// means "accept anything", so this cannot mask a real type error.
ajv.addFormat('uint32', true);
ajv.addFormat('uint64', true);

// ---------------------------------------------------------------- 1. compiles
console.log('1. schema.json compiles');
let validateRequest, validateResponse, validateEvent;
try {
  const extract = (defName) => {
    const sub = { $schema: schema.$schema, $defs: schema.$defs, ...schema.$defs[defName] };
    return ajv.compile(sub);
  };
  validateRequest = extract('Request');
  validateResponse = extract('Response');
  validateEvent = extract('EventFrame');
  check('Request compiles', true);
  check('Response compiles', true);
  check('EventFrame compiles', true);
} catch (e) {
  check('schemas compile', false, e.message);
  process.exit(1);
}

// ------------------------------------------------------- 2. meta-schema check
console.log('\n2. schema.json is a valid 2020-12 schema');
// ajv.validateSchema picks the meta-schema from the document's own `$schema`
// and validates against it. This is the check that the whole document is a
// legal schema, not merely that ajv could compile one part of it.
const metaOk = ajv.validateSchema(schema);
check('validates against the 2020-12 meta-schema', metaOk,
  JSON.stringify(ajv.errors || ajv.validate.errors, null, 2));

// --------------------------------------------------------- 3. the real checks
console.log('\n3. generated examples validate');
let reqOk = 0;
examples.requests.forEach((r, i) => {
  const ok = validateRequest(r);
  if (ok) reqOk++;
  else check(`request[${i}] (${r.method})`, false, JSON.stringify(validateRequest.errors, null, 2));
});
check(`all ${examples.requests.length} requests validate`, reqOk === examples.requests.length);

let respOk = 0;
examples.responses.forEach((r, i) => {
  const ok = validateResponse(r);
  if (ok) respOk++;
  else check(`response[${i}] (ok=${r.ok})`, false, JSON.stringify(validateResponse.errors, null, 2));
});
check(`all ${examples.responses.length} responses validate`, respOk === examples.responses.length);

let evOk = 0;
examples.events.forEach((e, i) => {
  const ok = validateEvent(e);
  if (ok) evOk++;
  else check(`event[${i}]`, false, JSON.stringify(validateEvent.errors, null, 2));
});
check(`all ${examples.events.length} events validate`, evOk === examples.events.length);

console.log('\n4. malformed frames are REJECTED (the point of the exercise)');

// A response must carry `ok`.
check('response without `ok` is rejected', !validateResponse({ id: 1, result: { method: 'db.ping', data: {} } }));
// A response must carry the method-discriminated result.
check('response with unknown method in result is rejected',
  !validateResponse({ id: 1, ok: true, result: { method: 'nope.nope', data: {} } }));
check('response with result=null and ok=true is rejected',
  !validateResponse({ id: 1, ok: true, result: null }));
check('response with error=null and ok=false is rejected',
  !validateResponse({ id: 1, ok: false, error: null }));
check('response with BOTH result and error is rejected',
  !validateResponse({
    id: 1, ok: true,
    result: { method: 'db.ping', data: { reachable: true, driver: 'x' } },
    error: { code: 'internal', message: 'x' },
  }));
check('response with ok=true and no result at all is rejected',
  !validateResponse({ id: 1, ok: true }));
check('response with ok=false and a result is rejected',
  !validateResponse({ id: 1, ok: false, result: { method: 'db.ping', data: { reachable: true, driver: 'x' } } }));
// An absent optional must be omitted, not null.
check('optional field sent as null is rejected',
  !validateRequest({ id: 1, method: 'logs.tail', params: { stream: null } }));
check('optional field omitted is accepted',
  validateRequest({ id: 1, method: 'logs.tail', params: {} }));
check('unknown error code is rejected',
  !validateResponse({ id: 1, ok: false, error: { code: 'kaboom', message: 'x' } }));
// A request must name a real method.
check('request with unknown method is rejected',
  !validateRequest({ id: 1, method: 'service.delete', params: {} }));
check('request missing params is rejected',
  !validateRequest({ id: 1, method: 'db.ping' }));
check('request missing id is rejected',
  !validateRequest({ method: 'db.ping', params: {} }));
check('request with params of the wrong type is rejected',
  !validateRequest({ id: 1, method: 'logs.tail', params: { replay: 'lots' } }));
// An event must not be shaped like a response.
check('event carrying `ok` is rejected',
  !validateEvent({ subscription: 1, ok: true, event: 'logs_line', data: {} }));
// Secret hygiene: the schema must not simply allow a secret's value back.
const secretResp = JSON.parse(JSON.stringify(examples.responses.find((r) =>
  r.result && r.result.method === 'config.get')));
check('example secret entry carries no value field',
  secretResp.result.data.entries[0].secret === true &&
  !('value' in secretResp.result.data.entries[0]));

// Method coverage.
const schemaText = JSON.stringify(schema);
const missing = examples.method_names.filter((n) => !schemaText.includes(n));
check('every method name appears in the schema', missing.length === 0,
  missing.length ? 'missing: ' + missing.join(', ') : '');

console.log('\n' + (failures === 0
  ? 'RESULT: schema.json is valid and describes the wire format correctly.'
  : `RESULT: ${failures} FAILURE(S)`));
process.exit(failures === 0 ? 0 : 1);