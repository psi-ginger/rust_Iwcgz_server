//! Capture metadata: the `.meta.json` document and the UTC formatter behind it.

use crate::json::Obj;

/// Format a Unix timestamp as `YYYY-MM-DDTHH:MM:SSZ`.
///
/// Implemented directly rather than pulling in a date library: the workspace
/// carries no third-party dependencies, and one conversion is not worth a
/// dependency. This is Howard Hinnant's `civil_from_days`, which is exact for
/// the whole proleptic Gregorian range and needs no lookup tables.
#[must_use]
pub fn iso8601_utc(secs_since_epoch: i64) -> String {
    let days = secs_since_epoch.div_euclid(86_400);
    let secs_of_day = secs_since_epoch.rem_euclid(86_400);

    // Shift the epoch to 0000-03-01 so leap days land at the end of the cycle.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097; // [0, 146096]
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365; // [0, 399]
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100); // [0, 365]
    let mp = (5 * day_of_year + 2) / 153; // [0, 11], March = 0
    let day = day_of_year - (153 * mp + 2) / 5 + 1; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = year + i64::from(month <= 2);

    let (hh, mm, ss) = (
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    );
    format!("{year:04}-{month:02}-{day:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Per-direction summary, as written into `.meta.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectionMeta {
    /// Total bytes recorded in that direction.
    pub bytes: u64,
    /// Number of `read` calls that produced data.
    pub chunks: u64,
    /// Microseconds from session start to the first byte, if any.
    pub first_byte_us: Option<u64>,
    /// Microseconds from session start to the last byte, if any.
    pub last_byte_us: Option<u64>,
}

impl DirectionMeta {
    fn write_into(self, obj: &mut Obj) {
        obj.uint("bytes", self.bytes).uint("chunks", self.chunks);
        match (self.first_byte_us, self.last_byte_us) {
            (Some(first), Some(last)) => {
                obj.uint("first_byte_us", first).uint("last_byte_us", last);
            }
            _ => {
                obj.raw("first_byte_us", "null").raw("last_byte_us", "null");
            }
        }
    }
}

/// Everything recorded about one proxied connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
    /// Tool that produced the capture, for provenance.
    pub tool: String,
    /// Start time, `YYYY-MM-DDTHH:MM:SSZ`.
    pub started_at: String,
    /// Unix seconds at start.
    pub started_unix: i64,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u64,
    /// The address the proxy listened on.
    pub listen: String,
    /// The address the proxy forwarded to.
    pub target: String,
    /// Why the session ended: `client_eof`, `server_eof`, or `error: ...`.
    pub end_reason: String,
    /// Client to server.
    pub c2s: DirectionMeta,
    /// Server to client.
    pub s2c: DirectionMeta,
}

impl Meta {
    /// Render the document text written to `<name>.meta.json`.
    #[must_use]
    pub fn to_json(&self) -> String {
        // Built first, then spliced in as pre-formatted values: the writer is
        // deliberately non-nesting.
        let mut c2s = Obj::new();
        self.c2s.write_into(&mut c2s);
        let mut s2c = Obj::new();
        self.s2c.write_into(&mut s2c);

        let mut obj = Obj::new();
        obj.str("tool", &self.tool)
            .str("started_at", &self.started_at)
            .int("started_unix", self.started_unix)
            .uint("duration_ms", self.duration_ms)
            .str("listen", &self.listen)
            .str("target", &self.target)
            .str("end_reason", &self.end_reason)
            .raw("c2s", &c2s.finish())
            .raw("s2c", &s2c.finish());
        obj.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_known_timestamps() {
        assert_eq!(iso8601_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601_utc(946_684_800), "2000-01-01T00:00:00Z");
        assert_eq!(iso8601_utc(1_234_567_890), "2009-02-13T23:31:30Z");
    }

    #[test]
    fn handles_leap_days_and_century_non_leap_years() {
        // 2020-02-29T00:00:00Z, a leap day in a leap century-adjacent year.
        assert_eq!(iso8601_utc(1_582_934_400), "2020-02-29T00:00:00Z");
        // 1900-03-01T00:00:00Z - 1900 is not a leap year, so Feb 29 never existed.
        assert_eq!(iso8601_utc(-2_203_891_200), "1900-03-01T00:00:00Z");
    }

    #[test]
    fn round_trips_the_last_second_of_a_day() {
        // 23:59:59 must not roll into the next day.
        assert_eq!(iso8601_utc(86_399), "1970-01-01T23:59:59Z");
        assert_eq!(iso8601_utc(86_400), "1970-01-02T00:00:00Z");
    }

    #[test]
    fn renders_nested_direction_objects() {
        let meta = Meta {
            tool: "capture".into(),
            started_at: iso8601_utc(0),
            started_unix: 0,
            duration_ms: 12,
            listen: "127.0.0.1:5750".into(),
            target: "127.0.0.1:5700".into(),
            end_reason: "client_eof".into(),
            c2s: DirectionMeta {
                bytes: 5,
                chunks: 2,
                first_byte_us: Some(100),
                last_byte_us: Some(400),
            },
            s2c: DirectionMeta {
                bytes: 0,
                chunks: 0,
                first_byte_us: None,
                last_byte_us: None,
            },
        };
        assert_eq!(
            meta.to_json(),
            r#"{"tool":"capture","started_at":"1970-01-01T00:00:00Z","started_unix":0,"duration_ms":12,"listen":"127.0.0.1:5750","target":"127.0.0.1:5700","end_reason":"client_eof","c2s":{"bytes":5,"chunks":2,"first_byte_us":100,"last_byte_us":400},"s2c":{"bytes":0,"chunks":0,"first_byte_us":null,"last_byte_us":null}}"#
        );
    }
}
