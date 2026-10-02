//! Log streaming types.
//!
//! The log tail is the only high-rate path on this channel and the only one
//! with a server-to-client push, so it carries more contract detail than the
//! other methods. Two of those details are there because of failures that are
//! easy to hit and hard to notice:
//!
//! * `truncated` -- a single line longer than the per-line budget is cut. A
//!   client that renders it without checking will silently display a partial
//!   line, and the operator will read a sentence that does not end where the
//!   log does. Truncation is reported, never silent.
//! * `lossy` -- log files are CP949/EUC-KR. The supervisor decodes to UTF-8
//!   before emitting. Bytes that are not valid CP949 cannot be represented, so
//!   they are replaced. That replacement is reported too, because a log
//!   showing mojibake is a *configuration* problem (wrong source encoding)
//!   and the operator needs to be told, not left to guess.

use serde::{Deserialize, Serialize};

/// Severity, ordered least to most verbose.
///
/// Deliberately a closed set rather than a passthrough string: a legacy log
/// line uses its own severities, and the supervisor has to map them onto
/// something the frontend can filter on. A closed set makes it a compile-time
/// obligation to handle every legacy severity, instead of a string that
/// silently arrives unfiltered.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    /// Fatal or unrecoverable.
    Error,
    /// Recoverable problem worth attention.
    Warn,
    /// Normal operational message.
    Info,
    /// Diagnostic detail.
    Debug,
    /// Very fine-grained tracing, usually off in production.
    Trace,
}

impl LogLevel {
    /// Parse a legacy severity string, case-insensitively.
    ///
    /// Returns `None` for an unrecognised severity so the caller has to
    /// decide explicitly -- falling back to `Info` by default would let a new
    /// legacy severity pass as routine and be filtered out of an
    /// error-only view.
    pub fn parse_legacy(s: &str) -> Option<Self> {
        match s.trim().to_ascii_uppercase().as_str() {
            "ERR" | "ERROR" | "FATAL" | "CRITICAL" => Some(Self::Error),
            "WARN" | "WARNING" => Some(Self::Warn),
            "INF" | "INFO" | "INFORMATION" => Some(Self::Info),
            "DBG" | "DEBUG" => Some(Self::Debug),
            "TRACE" | "TRC" | "VERBOSE" => Some(Self::Trace),
            _ => None,
        }
    }
}

/// One log line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LogLine {
    /// Monotonic per-subscription sequence number.
    ///
    /// Gaps are meaningful: a sequence that jumps means lines were dropped
    /// because the client fell behind, and the UI must say so rather than
    /// present a hole as if it were the whole story.
    pub seq: u64,
    /// Unix epoch milliseconds, UTC. Always present so the frontend never has
    /// to guess a timezone.
    pub ts_ms: u64,
    /// Severity after mapping from the legacy severity.
    pub level: LogLevel,
    /// Which log stream: `game`, `front`, `auth`, `logd`, `db`, ...
    pub stream: String,
    /// Emitting module, e.g. `GoonZuWorld::Char::Attack`.
    pub target: String,
    /// The message, decoded to UTF-8.
    pub message: String,
    /// The line was cut at the per-line budget.
    pub truncated: bool,
    /// Some bytes could not be decoded from the source encoding.
    pub lossy: bool,
}

/// Server-to-client push payloads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum Event {
    /// A new log line. See [`LogLine`].
    LogsLine(LogLine),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_severities_parse_case_insensitively() {
        assert_eq!(LogLevel::parse_legacy("ERR"), Some(LogLevel::Error));
        assert_eq!(LogLevel::parse_legacy(" warn "), Some(LogLevel::Warn));
        assert_eq!(LogLevel::parse_legacy("Information"), Some(LogLevel::Info));
        assert_eq!(LogLevel::parse_legacy("VERBOSE"), Some(LogLevel::Trace));
    }

    #[test]
    fn unknown_severity_returns_none_rather_than_defaulting() {
        // A silent fallback to Info would let a new legacy severity vanish
        // from an error-only view.
        assert_eq!(LogLevel::parse_legacy("NOTICE"), None);
        assert_eq!(LogLevel::parse_legacy(""), None);
    }

    #[test]
    fn level_ordering_is_verbosity_order() {
        assert!(LogLevel::Error < LogLevel::Warn);
        assert!(LogLevel::Warn < LogLevel::Info);
        assert!(LogLevel::Info < LogLevel::Debug);
        assert!(LogLevel::Debug < LogLevel::Trace);
    }
}
