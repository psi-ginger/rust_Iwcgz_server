//! The method surface.
//!
//! Every method that exists in this contract appears in three places, and a
//! test in `tests/contract.rs` asserts they agree exactly:
//!
//! * [`Method`] -- the name alone, used for echoing and for the schema.
//! * [`Call`] -- name plus typed request parameters.
//! * [`ResponseData`] -- name plus typed success payload.
//!
//! Keeping one list per concern makes the generated schema readable (a
//! frontend sees per-method shapes rather than one opaque blob), and the
//! agreement test means adding a method to two of the three is a test failure
//! rather than a silent gap. Same discipline as `codegen --check`.
//!
//! # Phases
//!
//! [`Phase::ReadOnly`] methods only inspect state and may be implemented
//! against the M1 database and the log files without the supervisor existing
//! yet. [`Phase::Reserved`] methods change state and stay
//! [`crate::envelope::OpsErrorCode::NotImplemented`] until M5, when the
//! supervisor can actually act on them. Both are typed and present in
//! `schema.json` so the frontend can be built against the finished contract
//! instead of a subset that later changes underneath it.
//!
//! # `config.set` is in the read-only phase list
//!
//! T-010's card lists `config.set` among the read-only methods. It is not
//! read-only: it writes. It is typed and in the contract anyway, and returns
//! `not_implemented` like the other writers, because there is currently no
//! config file for it to write -- the workspace has no server config at all
//! yet (see the crate README). Typing it now is the point of the card.

use serde::{Deserialize, Serialize};

use crate::logs::LogLevel;

/// The empty parameter object, for methods that take nothing.
///
/// Explicit rather than omitted so every request has a `params` member and the
/// shape does not change when a method later gains an optional argument.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct NoParams {}

/// An empty success payload, for methods whose only effect is the absence of
/// an error.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct Ack {}

/// When a method becomes implementable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Inspects state only. Targetable at M1.
    ReadOnly,
    /// Changes state. Blocked on the supervisor (M5).
    Reserved,
}

/// Every method name in the contract.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
pub enum Method {
    /// Handshake and capability exchange.
    #[serde(rename = "ops.hello")]
    Hello,
    /// Overall cluster identity and health.
    #[serde(rename = "status.get")]
    StatusGet,
    /// Per-service status.
    #[serde(rename = "services.list")]
    ServicesList,
    /// Open a log subscription.
    #[serde(rename = "logs.tail")]
    LogsTail,
    /// Close a log subscription.
    #[serde(rename = "logs.tail.stop")]
    LogsTailStop,
    /// Read configuration.
    #[serde(rename = "config.get")]
    ConfigGet,
    /// Write configuration. Mutating; see the module note.
    #[serde(rename = "config.set")]
    ConfigSet,
    /// Check database reachability.
    #[serde(rename = "db.ping")]
    DbPing,
    /// List characters. Needs the character repository (M2/M3).
    #[serde(rename = "players.list")]
    PlayersList,
    /// Start a service. Mutating.
    #[serde(rename = "service.start")]
    ServiceStart,
    /// Stop a service. Mutating.
    #[serde(rename = "service.stop")]
    ServiceStop,
    /// Restart a service. Mutating.
    #[serde(rename = "service.restart")]
    ServiceRestart,
    /// Disconnect a player's session. Mutating.
    #[serde(rename = "session.kick")]
    SessionKick,
}

impl Method {
    /// Every method, in declaration order.
    pub const ALL: &'static [Method] = &[
        Method::Hello,
        Method::StatusGet,
        Method::ServicesList,
        Method::LogsTail,
        Method::LogsTailStop,
        Method::ConfigGet,
        Method::ConfigSet,
        Method::DbPing,
        Method::PlayersList,
        Method::ServiceStart,
        Method::ServiceStop,
        Method::ServiceRestart,
        Method::SessionKick,
    ];

    /// The wire name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Method::Hello => "ops.hello",
            Method::StatusGet => "status.get",
            Method::ServicesList => "services.list",
            Method::LogsTail => "logs.tail",
            Method::LogsTailStop => "logs.tail.stop",
            Method::ConfigGet => "config.get",
            Method::ConfigSet => "config.set",
            Method::DbPing => "db.ping",
            Method::PlayersList => "players.list",
            Method::ServiceStart => "service.start",
            Method::ServiceStop => "service.stop",
            Method::ServiceRestart => "service.restart",
            Method::SessionKick => "session.kick",
        }
    }

    /// One-line description, used to generate the method table in the README.
    pub fn description(&self) -> &'static str {
        match self {
            Method::Hello => "Handshake: negotiate API version, exchange a token.",
            Method::StatusGet => "Cluster identity, uptime, service counts.",
            Method::ServicesList => "Per-service state, pid, uptime, cpu, rss, connections.",
            Method::LogsTail => "Open a log subscription; pushes `logs_line` events.",
            Method::LogsTailStop => "Close a log subscription.",
            Method::ConfigGet => "Read configuration entries, with secrets redacted.",
            Method::ConfigSet => "Write configuration entries. Mutating.",
            Method::DbPing => "Database reachability and round-trip latency.",
            Method::PlayersList => "List characters. Needs the M2/M3 character repository.",
            Method::ServiceStart => "Start a service. Mutating.",
            Method::ServiceStop => "Stop a service. Mutating.",
            Method::ServiceRestart => "Restart a service. Mutating.",
            Method::SessionKick => "Disconnect a player's session. Mutating.",
        }
    }

    /// Which phase this method belongs to.
    pub fn phase(&self) -> Phase {
        match self {
            Method::ServiceStart
            | Method::ServiceStop
            | Method::ServiceRestart
            | Method::SessionKick => Phase::Reserved,
            // `config.set` mutates, but the card groups it with the read-only
            // methods and there is no config file for it to write yet. Typed
            // now, implemented when the supervisor owns config.
            _ => Phase::ReadOnly,
        }
    }

    /// Whether the method changes state and so needs a write-capable token.
    pub fn is_mutating(&self) -> bool {
        matches!(
            self,
            Method::ConfigSet
                | Method::ServiceStart
                | Method::ServiceStop
                | Method::ServiceRestart
                | Method::SessionKick
        )
    }
}

impl std::fmt::Display for Method {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A request: method name plus typed parameters.
///
/// `tag = "method"` with `content = "params"` gives the flat wire shape
/// `{"method":"status.get","params":{}}` rather than serde's default
/// nesting, because a frontend reading a log of traffic should not have to
/// know that `method` is a serde implementation detail.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Call {
    #[serde(rename = "ops.hello")]
    Hello(HelloParams),
    #[serde(rename = "status.get")]
    StatusGet(NoParams),
    #[serde(rename = "services.list")]
    ServicesList(NoParams),
    #[serde(rename = "logs.tail")]
    LogsTail(LogsTailParams),
    #[serde(rename = "logs.tail.stop")]
    LogsTailStop(LogsTailStopParams),
    #[serde(rename = "config.get")]
    ConfigGet(ConfigGetParams),
    #[serde(rename = "config.set")]
    ConfigSet(ConfigSetParams),
    #[serde(rename = "db.ping")]
    DbPing(NoParams),
    #[serde(rename = "players.list")]
    PlayersList(PlayersListParams),
    #[serde(rename = "service.start")]
    ServiceStart(ServiceActionParams),
    #[serde(rename = "service.stop")]
    ServiceStop(ServiceActionParams),
    #[serde(rename = "service.restart")]
    ServiceRestart(ServiceActionParams),
    #[serde(rename = "session.kick")]
    SessionKick(SessionKickParams),
}

impl Call {
    /// Which method this call names.
    pub fn method(&self) -> Method {
        match self {
            Call::Hello(_) => Method::Hello,
            Call::StatusGet(_) => Method::StatusGet,
            Call::ServicesList(_) => Method::ServicesList,
            Call::LogsTail(_) => Method::LogsTail,
            Call::LogsTailStop(_) => Method::LogsTailStop,
            Call::ConfigGet(_) => Method::ConfigGet,
            Call::ConfigSet(_) => Method::ConfigSet,
            Call::DbPing(_) => Method::DbPing,
            Call::PlayersList(_) => Method::PlayersList,
            Call::ServiceStart(_) => Method::ServiceStart,
            Call::ServiceStop(_) => Method::ServiceStop,
            Call::ServiceRestart(_) => Method::ServiceRestart,
            Call::SessionKick(_) => Method::SessionKick,
        }
    }
}

/// A method's success payload, discriminated by method name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "method", content = "data", rename_all = "snake_case")]
pub enum ResponseData {
    #[serde(rename = "ops.hello")]
    Hello(HelloResult),
    #[serde(rename = "status.get")]
    StatusGet(StatusResult),
    #[serde(rename = "services.list")]
    ServicesList(ServicesListResult),
    #[serde(rename = "logs.tail")]
    LogsTail(LogsTailResult),
    #[serde(rename = "logs.tail.stop")]
    LogsTailStop(Ack),
    #[serde(rename = "config.get")]
    ConfigGet(ConfigGetResult),
    #[serde(rename = "config.set")]
    ConfigSet(ConfigSetResult),
    #[serde(rename = "db.ping")]
    DbPing(DbPingResult),
    #[serde(rename = "players.list")]
    PlayersList(PlayersListResult),
    #[serde(rename = "service.start")]
    ServiceStart(ServiceActionResult),
    #[serde(rename = "service.stop")]
    ServiceStop(ServiceActionResult),
    #[serde(rename = "service.restart")]
    ServiceRestart(ServiceActionResult),
    #[serde(rename = "session.kick")]
    SessionKick(SessionKickResult),
}

impl ResponseData {
    /// Which method this payload answers.
    pub fn method(&self) -> Method {
        match self {
            ResponseData::Hello(_) => Method::Hello,
            ResponseData::StatusGet(_) => Method::StatusGet,
            ResponseData::ServicesList(_) => Method::ServicesList,
            ResponseData::LogsTail(_) => Method::LogsTail,
            ResponseData::LogsTailStop(_) => Method::LogsTailStop,
            ResponseData::ConfigGet(_) => Method::ConfigGet,
            ResponseData::ConfigSet(_) => Method::ConfigSet,
            ResponseData::DbPing(_) => Method::DbPing,
            ResponseData::PlayersList(_) => Method::PlayersList,
            ResponseData::ServiceStart(_) => Method::ServiceStart,
            ResponseData::ServiceStop(_) => Method::ServiceStop,
            ResponseData::ServiceRestart(_) => Method::ServiceRestart,
            ResponseData::SessionKick(_) => Method::SessionKick,
        }
    }
}

// ---------------------------------------------------------------------------
// ops.hello
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HelloParams {
    /// Client name, e.g. `control-panel`.
    pub client: String,
    /// Client version, for the operator's support log.
    pub client_version: String,
    /// Oldest API version this client can speak. The server refuses with
    /// `invalid_params` if the server's version is below this.
    ///
    /// There is no upper bound check: a newer server is expected to keep
    /// older methods working, and to add rather than change.
    pub min_api_version: u32,
    /// Shared secret from the token file. Required on TCP; recommended on a
    /// Unix socket, where filesystem permissions are the real control. See
    /// the crate README.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HelloResult {
    /// The contract version this server implements.
    pub api_version: u32,
    /// Server-side identifier, e.g. `goonzu-rs-supervisor`.
    pub server: String,
    /// Build version of the server binary.
    pub server_version: String,
    /// Optional capabilities the client may use, e.g. `config.set`,
    /// `service.restart`. Empty means "read-only server".
    pub features: Vec<String>,
}

// ---------------------------------------------------------------------------
// status.get / services.list
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct StatusResult {
    /// Contract version the server implements.
    pub api_version: u32,
    /// Server build version.
    pub server_version: String,
    /// Seconds since the server started.
    pub uptime_s: u64,
    /// How many registered services are running.
    pub services_running: u32,
    /// How many services are registered in total.
    pub services_total: u32,
}

/// Coarse service state. The supervisor maps process state onto this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    /// Not running.
    Stopped,
    /// Spawned but not yet serving.
    Starting,
    /// Serving.
    Running,
    /// Asked to stop, still draining.
    Stopping,
    /// Exited with an error.
    Failed,
    /// The supervisor cannot determine the state.
    Unknown,
}

impl ServiceState {
    /// Whether the service is expected to be accepting clients.
    pub fn is_serving(&self) -> bool {
        matches!(self, Self::Running)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServiceStatus {
    /// Registry name, e.g. `game`. These are the Rust workspace's service
    /// names, not the legacy `.vcproj` names; the mapping is in the README.
    pub name: String,
    /// Coarse state.
    pub state: ServiceState,
    /// OS process id, absent when stopped or unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Seconds since the process started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uptime_s: Option<u64>,
    /// CPU percent since the last sample, or `None` if unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_pct: Option<f64>,
    /// Resident set size in bytes, or `None` if unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rss_bytes: Option<u64>,
    /// Currently connected clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connections: Option<u32>,
    /// Unix epoch ms at which the service entered its current state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServicesListResult {
    /// One entry per registered service, ordered by name.
    pub services: Vec<ServiceStatus>,
}

// ---------------------------------------------------------------------------
// logs.tail / logs.tail.stop
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LogsTailParams {
    /// Restrict to one stream, e.g. `game`. `None` means all streams.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<String>,
    /// Minimum severity. `None` means everything, including `trace`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_level: Option<LogLevel>,
    /// Case-insensitive substring the message must contain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains: Option<String>,
    /// How many recent matching lines to replay before streaming live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LogsTailResult {
    /// Id to pass to `logs.tail.stop`, and the `subscription` on every
    /// pushed event.
    pub subscription: u32,
    /// How many lines were actually replayed. May be fewer than `replay` if
    /// fewer were available, or zero.
    pub replayed: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LogsTailStopParams {
    /// Subscription to close. Closing an unknown id is a no-op, not an error:
    /// the subscription may already have ended.
    pub subscription: u32,
}

// ---------------------------------------------------------------------------
// config.get / config.set
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ConfigGetParams {
    /// Dotted key prefix to restrict to, e.g. `server.game`. `None` is all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ConfigGetResult {
    /// Matching entries, ordered by key.
    pub entries: Vec<ConfigEntry>,
}

/// TOML value type, so the frontend can render an editor rather than a
/// text box with everything as a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConfigKind {
    String,
    Integer,
    Float,
    Boolean,
    /// TOML array. Rendered as a list, not as a TOML inline table.
    List,
    /// Nested table. Rendered as a group, not as a leaf.
    Table,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ConfigEntry {
    /// Dotted path, e.g. `server.game.tick_hz`.
    pub key: String,
    /// Rendered value. **Always `None` when `secret` is true** -- see the
    /// field docs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// TOML type.
    pub kind: ConfigKind,
    /// Value is sensitive (a DB password, a link-cipher key).
    ///
    /// Plan §9 requires DB credentials to live in the OS keychain rather than
    /// in config. So a `secret` entry's `value` is always `None` here no
    /// matter what the server has: the frontend gets a key name and a
    /// "set a new value" affordance, never the current one. Config is
    /// readable by every local process that can reach this socket, and
    /// anything with root on the box can read the keychain anyway -- so
    /// pretending to hide it in the response would only move the leak from a
    /// documented place into an undocumented one.
    pub secret: bool,
    /// Whether the server will accept a write to this key.
    pub read_only: bool,
    /// Doc comment from the config file, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ConfigSetParams {
    /// Entries to write. The server applies them as one transaction: either
    /// every key is valid and written, or none is.
    pub entries: Vec<ConfigSetEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ConfigSetEntry {
    /// Dotted path.
    pub key: String,
    /// New value, as a string. Parsed according to the existing entry's
    /// [`ConfigKind`], or as a string if the key is new.
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ConfigSetResult {
    /// How many entries were written.
    pub applied: u32,
    /// Services that need a restart to pick the change up. The server does
    /// **not** restart them: that is a deliberate, separate action, so an
    /// operator never gets an unexpected restart from an unrelated edit.
    pub needs_restart: Vec<String>,
}

// ---------------------------------------------------------------------------
// db.ping
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DbPingResult {
    /// Whether a trivial query succeeded.
    pub reachable: bool,
    /// Round-trip time of that query.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    /// Why it failed. Operator-facing; carries no credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Driver name and version, e.g. `postgresql/18.6`. Diagnostic.
    pub driver: String,
}

// ---------------------------------------------------------------------------
// players.list
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PlayersListParams {
    /// Only currently connected characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub online_only: Option<bool>,
    /// Case-insensitive substring match on the character name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,
    /// Page size. Server clamps to its own maximum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Rows to skip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
}

/// One row of the character list.
///
/// **Provisional, and deliberately minimal.** The legacy character schema has
/// not been surveyed yet -- that is the M2/M3 character repository work -- so
/// this type carries only the two fields needed to render a list row
/// unambiguously. Adding fields is a breaking change to this type, so they
/// should be added once, when the real columns are known, rather than guessed
/// at now and corrected later. `gz-db` currently models only
/// [`gz_db::repository::Account`]; see the crate README.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PlayerSummary {
    /// Character identifier.
    pub player_id: u32,
    /// Character name, already decoded from CP949 to UTF-8.
    pub name: String,
    /// Whether the character currently has a live session.
    pub online: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PlayersListResult {
    /// Matching characters.
    pub players: Vec<PlayerSummary>,
    /// Total matches before `limit`/`offset`, for paging.
    pub total: u32,
}

// ---------------------------------------------------------------------------
// service.start / service.stop / service.restart
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServiceActionParams {
    /// Registry name from `services.list`.
    pub service: String,
    /// Give up after this many seconds. The server picks its own default when
    /// absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_s: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServiceActionResult {
    /// The service acted on.
    pub service: String,
    /// Whether the requested transition actually happened. `false` with
    /// `state` already correct is success, not failure -- stopping a stopped
    /// service is what the operator asked for.
    pub changed: bool,
    /// State after the action.
    pub state: ServiceState,
}

// ---------------------------------------------------------------------------
// session.kick
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionKickParams {
    /// Kick this character.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub player_id: Option<u32>,
    /// Or every session on this account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<i32>,
    /// Reason shown to the player and written to the log. Strongly
    /// recommended; an unexplained kick is indistinguishable from a bug.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionKickResult {
    /// How many sessions were disconnected.
    pub kicked: u32,
    /// How many targets matched no live session. Not an error: the operator
    /// asked to kick an offline character, and the honest answer is zero.
    pub not_found: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_str_agrees_with_serde_rename() {
        // The wire name exists in two places -- `as_str` and the serde
        // `rename` attribute -- and a mismatch would be invisible in review.
        for m in Method::ALL {
            let value = serde_json::to_value(m).unwrap();
            let via_serde = value.as_str().unwrap();
            assert_eq!(
                via_serde,
                m.as_str(),
                "serde name differs from as_str for {m:?}"
            );
        }
    }

    #[test]
    fn no_duplicate_method_names() {
        let mut seen = std::collections::HashSet::new();
        for m in Method::ALL {
            assert!(seen.insert(m.as_str()), "duplicate name {}", m.as_str());
        }
    }

    #[test]
    fn mutating_methods_are_the_reserved_ones_plus_config_set() {
        for m in Method::ALL {
            let expected = matches!(
                m,
                Method::ConfigSet
                    | Method::ServiceStart
                    | Method::ServiceStop
                    | Method::ServiceRestart
                    | Method::SessionKick
            );
            assert_eq!(m.is_mutating(), expected, "wrong mutating flag for {m}");
        }
    }
}
