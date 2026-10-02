//! Local ops API contract for the Tauri desktop tooling (plan §9).
//!
//! This crate is the **contract only**: framing, message types, and a
//! generated JSON Schema. There is no server, no client and no transport
//! implementation here, by design. It is the thing the control panel, the
//! launcher, the GM tool and the eventual ops server all agree on, so it stays
//! dependency-light -- pulling a database driver or a game-protocol codec into
//! it would make it unconsumable from a plain frontend core crate.
//!
//! # Shape of the protocol
//!
//! Local TCP or Unix-socket stream, frames of a 4-byte big-endian length
//! followed by that many bytes of UTF-8 JSON. See [`framing`] for why it is
//! not newline-delimited.
//!
//! A client sends [`Request`], the server sends back [`Response`]. Long-lived
//! streams additionally push [`EventFrame`]s, distinguishable by the absence
//! of an `ok` member.
//!
//! ```text
//! --> {"id":1,"method":"ops.hello","params":{"client":"control-panel","client_version":"0.1.0","min_api_version":1,"token":"..."}}
//! <-- {"id":1,"ok":true,"result":{"method":"ops.hello","data":{"api_version":1,...}}}
//! --> {"id":2,"method":"logs.tail","params":{"min_level":"warn","replay":200}}
//! <-- {"id":2,"ok":true,"result":{"method":"logs.tail","data":{"subscription":1,"replayed":87}}}
//! <-- {"subscription":1,"event":"logs_line","data":{"seq":88,...}}
//! ```
//!
//! # Versions
//!
//! [`API_VERSION`] is bumped when a method is added or a payload grows an
//! optional member. A newer server must keep older clients working, so
//! additions are the only sanctioned change: a removed method, a removed
//! member, or a changed type is a version bump *and* a `min_api_version`
//! negotiation failure for the affected clients.
//!
//! `ops.hello` is mandatory and must be first. It is what lets the control
//! panel say "this server is too old" instead of sending a request the server
//! will reject with a bare `method_not_found`.
//!
//! # What is *not* decided here
//!
//! * **Transport choice.** [`framing`] is transport-agnostic. The crate README
//!   records the recommendation and the reasoning, but binding a listener is
//!   an implementation decision, not a contract one.
//! * **Where the token lives.** The contract carries it; the README argues
//!   that a Unix socket's `0600` should be the real control.
//! * **The character schema.** [`PlayerSummary`] is intentionally provisional.
//!
//! # Guarding the contract
//!
//! Three mechanisms, all of which fail the build or the test run rather than
//! letting the contract drift silently:
//!
//! * [`methods::Method`], [`methods::Call`] and [`methods::ResponseData`] must
//!   agree on names and arity -- asserted in `tests/contract.rs`.
//! * `schema.json` is a committed artifact that a test re-derives and compares,
//!   the same way `codegen --check` compares the opcode tables.
//! * The method table in the README is generated from the same metadata, so it
//!   cannot describe a method that does not exist.

pub mod envelope;
pub mod framing;
pub mod logs;
pub mod methods;
pub mod schema;

pub use envelope::{EventFrame, OpsError, OpsErrorCode, Request, Response};
pub use framing::{FrameError, MAX_FRAME_BYTES};
pub use logs::{Event, LogLevel, LogLine};
pub use methods::{Call, Method, Phase, ResponseData};

/// Contract version. See the crate docs on compatibility.
pub const API_VERSION: u32 = 1;
