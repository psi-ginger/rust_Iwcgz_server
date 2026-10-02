//! Request / response / event envelopes.
//!
//! # Deliberately not JSON-RPC 2.0
//!
//! The shape is familiar (`id`, method name, result, error) but it is not
//! JSON-RPC 2.0, because the `jsonrpc: "2.0"` member buys nothing here: both
//! ends of this channel are Rust processes holding generated types, so there
//! is no third-party client that would benefit from recognising the
//! standard. Dropping it keeps frames smaller on the log-tail path, where the
//! frame rate is the highest thing on this socket.
//!
//! # Frame discrimination
//!
//! A frame is a [`Response`] if and only if it has an `ok` member. Anything
//! else is an [`EventFrame`]. The two can never collide: a response always
//! carries `ok`, and an event never does. That rule is the only thing a
//! receiver needs in order to route a frame, and it is enforced by the
//! round-trip tests in `tests/contract.rs`.
//!
//! # The `ok` invariant
//!
//! `ok == true` if and only if `result` is present. `error` is the mirror.
//! [`Response::ok`], [`Response::error`] and [`Response::result`] constructors
//! exist so callers cannot build a response that breaks this by hand, and
//! `tests/contract.rs` asserts it holds for every response variant.

use serde::{Deserialize, Serialize};

use crate::methods::ResponseData;

/// A request from client to server.
///
/// `id` is client-assigned and is the only correlation the protocol needs:
/// the server echoes it on the response, and uses it as the subscription id
/// for pushed events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Request {
    /// Client-assigned correlation id, echoed verbatim on the response.
    pub id: u64,
    /// Method name plus its typed parameters.
    #[serde(flatten)]
    pub call: crate::methods::Call,
}

/// A response to exactly one [`Request`].
///
/// The schema carries the `ok`/`result`/`error` invariant as an `if`/`then`
/// rather than leaving it to convention. `required` in JSON Schema is
/// unconditional, so the coupling between `ok` and its companions is only
/// expressible conditionally -- and without it the schema accepts
/// `{"ok":true}` with no result, `{"ok":false}` carrying a result, and a frame
/// with both. A frontend generated from that schema would treat all three as
/// possible and would fail on a null deref rather than on a rejected frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(extend(
    "allOf" = [
        {
            "if": { "properties": { "ok": { "const": true } }, "required": ["ok"] },
            "then": { "required": ["result"], "not": { "required": ["error"] } }
        },
        {
            "if": { "properties": { "ok": { "const": false } }, "required": ["ok"] },
            "then": { "required": ["error"], "not": { "required": ["result"] } }
        }
    ]
))]
pub struct Response {
    /// Echo of the request's `id`.
    pub id: u64,
    /// `true` when `result` is present and `error` is absent.
    pub ok: bool,
    /// Method-specific payload. Present iff `ok`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<ResponseData>,
    /// Failure detail. Present iff `!ok`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<OpsError>,
}

impl Response {
    /// Build a success response.
    pub fn ok(id: u64, result: ResponseData) -> Self {
        Self {
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    /// Build a failure response.
    pub fn error(id: u64, error: OpsError) -> Self {
        Self {
            id,
            ok: false,
            result: None,
            error: Some(error),
        }
    }

    /// Whether this frame is a successful one. Mirrors the `ok` member.
    pub fn is_ok(&self) -> bool {
        self.ok
    }

    /// True if the `ok` / `result` / `error` members are mutually consistent.
    ///
    /// Kept public so the round-trip tests -- and any future alternative
    /// constructor -- can assert it rather than assume it.
    pub fn is_well_formed(&self) -> bool {
        matches!(
            (self.ok, &self.result, &self.error),
            (true, Some(_), None) | (false, None, Some(_))
        )
    }
}

/// A server-initiated push. Only ever arrives after a successful `subscribe`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EventFrame {
    /// The `subscription` returned by the method that opened the stream.
    pub subscription: u32,
    /// Event name plus its typed payload.
    #[serde(flatten)]
    pub event: crate::logs::Event,
}

/// Stable machine-readable failure codes.
///
/// These are part of the contract: a frontend may branch on them. The human
/// text goes in [`OpsError::message`] and may change freely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OpsErrorCode {
    /// Token missing or wrong. Always fatal to the connection -- a client that
    /// gets this once should not retry the same token.
    Unauthorized,
    /// The `method` member named something not in [`crate::methods::Method`].
    MethodNotFound,
    /// Parameters were well-formed JSON but not valid for the method, e.g. a
    /// `limit` of zero or a `service` that is not in the registry.
    InvalidParams,
    /// The method exists in this contract but the server has not implemented it
    /// yet. Expected for everything in the reserved phase.
    NotImplemented,
    /// The server is up but the thing the method needs is down, e.g. `db.ping`
    /// when the database is unreachable.
    Unavailable,
    /// The method exceeded its deadline.
    Timeout,
    /// Internal failure. `message` is for logs, not for end users.
    Internal,
}

impl OpsErrorCode {
    /// Whether retrying the identical request could plausibly succeed.
    ///
    /// The frontend uses this to decide between "show a retry button" and
    /// "show an error the operator has to fix". `Unavailable` and `Timeout` are
    /// the only transient ones; retrying `InvalidParams` just wastes the
    /// operator's time.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Unavailable | Self::Timeout)
    }
}

/// A failure. `code` is stable, `message` is not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct OpsError {
    /// Stable machine-readable code.
    pub code: OpsErrorCode,
    /// Human-readable detail. May change between server versions; do not
    /// parse it.
    pub message: String,
}

impl OpsError {
    /// Build an error with a formatted message.
    pub fn new(code: OpsErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Shorthand for the "typed but not built yet" case.
    pub fn not_implemented(method: crate::methods::Method) -> Self {
        Self::new(
            OpsErrorCode::NotImplemented,
            format!("{method} is part of the contract but not implemented by this server"),
        )
    }
}

impl std::fmt::Display for OpsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}

impl std::error::Error for OpsError {}
