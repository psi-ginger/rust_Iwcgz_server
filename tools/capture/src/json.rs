//! Minimal JSON writer.
//!
//! The workspace has no third-party dependencies and this tool only ever emits
//! fixed-shape objects, so a small writer is cheaper than a dependency that would
//! need auditing. It covers exactly what [`crate::meta::Meta`] needs and nothing
//! more: strings, integers, booleans and pre-formatted fragments.
//!
//! There is deliberately no parser here. Re-reading a capture uses the
//! line-oriented timing sidecar instead, so the tool never needs a JSON parser.

use std::fmt::Write as _;

/// Escape a string for inclusion in a JSON document, per RFC 8259 §7.
///
/// Only `"`, `\` and the C0 control range need escaping; everything else,
/// including non-ASCII, is emitted as UTF-8.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// Builds a flat JSON object: keys in insertion order, no nesting.
#[derive(Debug)]
pub struct Obj {
    buf: String,
    empty: bool,
}

impl Obj {
    /// A new, empty object.
    #[must_use]
    pub fn new() -> Self {
        Self {
            buf: String::from("{"),
            empty: true,
        }
    }

    fn key(&mut self, key: &str) {
        if !self.empty {
            self.buf.push(',');
        }
        self.empty = false;
        self.buf.push('"');
        self.buf.push_str(&escape(key));
        self.buf.push_str("\":");
    }

    /// Add a string field.
    pub fn str(&mut self, key: &str, value: &str) -> &mut Self {
        self.key(key);
        self.buf.push('"');
        self.buf.push_str(&escape(value));
        self.buf.push('"');
        self
    }

    /// Add a signed integer field.
    pub fn int(&mut self, key: &str, value: i64) -> &mut Self {
        self.key(key);
        let _ = write!(self.buf, "{value}");
        self
    }

    /// Add an unsigned integer field.
    pub fn uint(&mut self, key: &str, value: u64) -> &mut Self {
        self.key(key);
        let _ = write!(self.buf, "{value}");
        self
    }

    /// Add a boolean field.
    pub fn bool(&mut self, key: &str, value: bool) -> &mut Self {
        self.key(key);
        self.buf.push_str(if value { "true" } else { "false" });
        self
    }

    /// Add a pre-formatted JSON value, for arrays and nested objects.
    pub fn raw(&mut self, key: &str, value: &str) -> &mut Self {
        self.key(key);
        self.buf.push_str(value);
        self
    }

    /// Close the object and return the document text.
    #[must_use]
    pub fn finish(mut self) -> String {
        self.buf.push('}');
        self.buf
    }
}

impl Default for Obj {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_the_characters_json_requires() {
        assert_eq!(escape(r#"a"b"#), r#"a\"b"#);
        assert_eq!(escape(r"a\b"), r"a\\b");
        assert_eq!(escape("a\nb\rc\td"), r"a\nb\rc\td");
        assert_eq!(escape("\u{8}\u{c}"), r"\b\f");
        // A forward slash needs no escaping, and UTF-8 passes through.
        assert_eq!(escape("a/b"), "a/b");
        assert_eq!(escape("한국어"), "한국어");
    }

    #[test]
    fn remaining_c0_controls_use_the_four_digit_form() {
        assert_eq!(escape("\u{1}"), "\\u0001");
        assert_eq!(escape("\u{1f}"), "\\u001f");
        assert_eq!(escape("\u{0}"), "\\u0000");
        // 0x20 is a space: the first character needing no escaping at all.
        assert_eq!(escape("\u{20}"), " ");
        assert_eq!(escape("\u{7f}"), "\u{7f}");
    }

    #[test]
    fn every_c0_control_produces_a_valid_escape() {
        for code in 0u32..0x20 {
            let ch = char::from_u32(code).expect("c0 range is valid");
            let escaped = escape(&ch.to_string());
            let ok = escaped.starts_with("\\u")
                || escaped == r#"\n"#
                || escaped == r#"\r"#
                || escaped == r#"\t"#
                || escaped == r#"\b"#
                || escaped == r#"\f"#;
            assert!(ok, "U+{code:04X} escaped to {escaped:?}");
        }
    }

    #[test]
    fn writes_fields_in_insertion_order() {
        let mut obj = Obj::new();
        obj.str("b", "two")
            .uint("a", 1)
            .bool("c", true)
            .int("d", -5)
            .raw("e", "[1,2]");
        assert_eq!(
            obj.finish(),
            r#"{"b":"two","a":1,"c":true,"d":-5,"e":[1,2]}"#
        );
    }

    #[test]
    fn an_empty_object_is_valid() {
        assert_eq!(Obj::new().finish(), "{}");
    }

    #[test]
    fn keys_are_escaped_too() {
        let mut obj = Obj::new();
        obj.str("a\"b", "v");
        assert_eq!(obj.finish(), r#"{"a\"b":"v"}"#);
    }
}
