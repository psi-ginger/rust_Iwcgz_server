//! Byte-level parser for legacy C/C++ `enum` blocks.
//!
//! The legacy headers are EUC-KR (CP949) and must never be re-encoded, so this
//! parser works directly on bytes. Only ASCII is ever interpreted, and the
//! parser refuses anything it cannot decode unambiguously.
//!
//! # Why byte-level comment stripping is safe for CP949
//!
//! In CP949 (UHC) a double-byte character has a lead byte in `0x81..=0xFE` and a
//! trail byte in `0x41..=0x5A | 0x61..=0x7A | 0x81..=0xFE`. None of the bytes we
//! treat as syntax (`/` `0x2F`, `*` `0x2A`, `,` `0x2C`, `=` `0x3D`, `;` `0x3B`,
//! `#` `0x23`, whitespace) can appear as a trail byte, so they are never part of
//! a multi-byte character. Stripping comments at the byte level therefore cannot
//! split a character, and non-ASCII bytes are simply dropped as comment or
//! identifier content. `Parser` rejects any identifier that is not pure ASCII,
//! so mojibake can never reach the generated Rust source.

use std::fmt;

/// One enumerator extracted from a legacy enum block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enumerator {
    /// C identifier, verbatim (e.g. `GAMEMSG_REQUEST_CHAT`).
    pub name: String,
    /// Computed value: the explicit `= N` if present, else the previous value + 1.
    pub value: u16,
    /// 1-based line number in the source header (provenance for audits).
    pub line: usize,
}

/// A parsed legacy enum block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumDef {
    /// C enum tag, e.g. `eGameNetworkMsg`.
    pub tag: String,
    /// 1-based line of the `enum` keyword.
    pub start_line: usize,
    /// 1-based line of the closing brace.
    pub end_line: usize,
    /// Enumerators in source order (including any terminator such as `GAMEMSG_END`).
    pub members: Vec<Enumerator>,
}

/// Error raised when a header cannot be parsed without guessing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

impl ParseError {
    fn new(line: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            message: message.into(),
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line == 0 {
            write!(f, "{}", self.message)
        } else {
            write!(f, "line {}: {}", self.line, self.message)
        }
    }
}

impl std::error::Error for ParseError {}

/// Parses the enum tagged `tag` out of a legacy header.
///
/// Enumerators are numbered exactly the way the C compiler would: an explicit
/// `= N` sets the counter, anything else increments it. Values that do not fit
/// in `u16` are rejected, since `usCmd` on the wire is a `UI16`.
pub fn parse_enum(source: &[u8], tag: &str) -> Result<EnumDef, ParseError> {
    let code = strip_comments(source)?;

    let start = code
        .iter()
        .position(|(_, line)| enum_tag(line) == Some(tag))
        .ok_or_else(|| ParseError::new(0, format!("enum `{tag}` not found")))?;

    let start_line = code[start].0;
    let mut members: Vec<Enumerator> = Vec::new();
    // The first enumerator of the block is always explicit in the legacy headers;
    // seeding at -1 means an implicit first value would be 0, as in C.
    let mut counter: i64 = -1;
    let mut end_line = start_line;

    for entry in &code[start + 1..] {
        let (line_no, text) = entry;
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        if text.starts_with('}') {
            end_line = *line_no;
            break;
        }
        if text.starts_with('#') {
            return Err(ParseError::new(
                *line_no,
                "preprocessor directive inside enum block: values cannot be resolved without \
                 knowing which branch is compiled",
            ));
        }

        let decl = strip_trailing_comma(text);
        let (ident, rest) = split_ident(decl).ok_or_else(|| {
            ParseError::new(*line_no, format!("cannot read enumerator: `{decl}`"))
        })?;
        if !ident.is_ascii() || ident.is_empty() {
            return Err(ParseError::new(
                *line_no,
                format!("non-ASCII identifier `{ident}`: refusing to emit mojibake"),
            ));
        }

        let rest = rest.trim_start();
        let value = if rest.is_empty() {
            counter += 1;
            counter
        } else if let Some(after_eq) = rest.strip_prefix('=') {
            let (digits, tail) = split_ident(after_eq.trim_start()).ok_or_else(|| {
                ParseError::new(*line_no, format!("expected a numeric value, got `{rest}`"))
            })?;
            if !tail.trim().is_empty() {
                return Err(ParseError::new(
                    *line_no,
                    format!("trailing tokens after value: `{rest}`"),
                ));
            }
            let parsed: i64 = digits.parse().map_err(|_| {
                ParseError::new(*line_no, format!("`{digits}` is not a decimal value"))
            })?;
            counter = parsed;
            parsed
        } else {
            return Err(ParseError::new(
                *line_no,
                format!("unexpected tokens after `{ident}`: `{rest}`"),
            ));
        };

        if !(0..=u16::MAX as i64).contains(&value) {
            return Err(ParseError::new(
                *line_no,
                format!("value {value} does not fit in the 16-bit `usCmd` field"),
            ));
        }

        members.push(Enumerator {
            name: ident.to_string(),
            value: value as u16,
            line: *line_no,
        });
    }

    if end_line == start_line {
        return Err(ParseError::new(
            start_line,
            format!("enum `{tag}` is not terminated by `}}`"),
        ));
    }
    if members.is_empty() {
        return Err(ParseError::new(
            start_line,
            format!("enum `{tag}` is empty"),
        ));
    }

    Ok(EnumDef {
        tag: tag.to_string(),
        start_line,
        end_line,
        members,
    })
}

/// Returns the tag of a line that opens an enum, e.g. `enum eGameNetworkMsg{`.
fn enum_tag(line: &str) -> Option<&str> {
    let rest = line.trim().strip_prefix("enum")?;
    if !rest.starts_with(|c: char| c.is_whitespace()) {
        return None;
    }
    let rest = rest.trim_start();
    let (ident, tail) = split_ident(rest)?;
    if tail.trim_start().starts_with('{') {
        Some(ident)
    } else {
        None
    }
}

/// Splits leading ASCII identifier characters from `s`.
fn split_ident(s: &str) -> Option<(&str, &str)> {
    let end = s
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '_'))
        .map_or(s.len(), |(i, _)| i);
    if end == 0 {
        return None;
    }
    Some((&s[..end], &s[end..]))
}

fn strip_trailing_comma(s: &str) -> &str {
    let s = s.trim_end();
    s.strip_suffix(',').unwrap_or(s).trim_end()
}

/// Replaces every comment with spaces, preserving byte offsets and line numbering.
///
/// Only a trailing newline is dropped, so a file ending in `\n` does not gain a
/// phantom final line.
fn strip_comments(source: &[u8]) -> Result<Vec<(usize, String)>, ParseError> {
    let body = match source.split_last() {
        Some((b'\n', rest)) => rest,
        Some((b'\r', rest)) => rest,
        _ => source,
    };

    let mut out = Vec::new();
    let mut in_block = false;
    for (index, raw) in body.split(|&b| b == b'\n').enumerate() {
        let line_no = index + 1;
        let line = raw.strip_suffix(b"\r").unwrap_or(raw);
        let mut buf: Vec<u8> = Vec::with_capacity(line.len());

        let mut i = 0;
        while i < line.len() {
            if in_block {
                if line[i] == b'*' && line.get(i + 1) == Some(&b'/') {
                    in_block = false;
                    i += 2;
                } else {
                    i += 1;
                }
                continue;
            }
            if line[i] == b'/' {
                match line.get(i + 1) {
                    Some(b'/') => break,
                    Some(b'*') => {
                        in_block = true;
                        i += 2;
                        continue;
                    }
                    _ => {}
                }
            }
            buf.push(line[i]);
            i += 1;
        }

        out.push((
            line_no,
            String::from_utf8(buf)
                .map_err(|_| ParseError::new(line_no, "invalid UTF-8 in code"))?,
        ));
    }

    if in_block {
        return Err(ParseError::new(0, "unterminated block comment"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implicit_sequence_and_rebases() {
        let src = b"enum eGameNetworkMsg{\n\
                      \tGAMEMSG_A\t= 1,\n\
                      \tGAMEMSG_B,\n\
                      \tGAMEMSG_C\t= 10,\n\
                      \tGAMEMSG_D\n\
                   };\n";
        let def = parse_enum(src, "eGameNetworkMsg").unwrap();
        let got: Vec<(&str, u16)> = def
            .members
            .iter()
            .map(|m| (m.name.as_str(), m.value))
            .collect();
        assert_eq!(
            got,
            vec![
                ("GAMEMSG_A", 1),
                ("GAMEMSG_B", 2),
                ("GAMEMSG_C", 10),
                ("GAMEMSG_D", 11),
            ]
        );
    }

    #[test]
    fn line_comments_are_dropped() {
        let src =
            "enum e{\n\tGAMEMSG_A = 1, // \u{B450}\u{AC74}\u{D638}\n\tGAMEMSG_B,\n};\n".as_bytes();
        let def = parse_enum(src, "e").unwrap();
        assert_eq!(def.members.len(), 2);
        assert_eq!(def.members[1].name, "GAMEMSG_B");
        assert_eq!(def.members[1].line, 3);
    }

    #[test]
    fn block_comments_hide_enumerators_without_shifting_values() {
        // Mirrors MsgType.h:1307-1332, where a commented-out block sits between
        // two explicit rebases.
        let src = b"enum e{\n\
                      \tGAMEMSG_A\t= 2900,\n\
                      \t//GAMEMSG_HIDDEN_ONE,\n\
                      \t/*\tGAMEMSG_HIDDEN_TWO,\n\
                      \tGAMEMSG_HIDDEN_THREE,\n\
                      \t*/\n\
                      \tGAMEMSG_B\t= 5000,\n\
                   };\n";
        let def = parse_enum(src, "e").unwrap();
        let names: Vec<&str> = def.members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["GAMEMSG_A", "GAMEMSG_B"]);
        assert_eq!(def.members[1].value, 5000);
    }

    #[test]
    fn block_comment_terminator_on_its_own_line() {
        let src = b"enum e{\n\tGAMEMSG_A = 1,\n\t/* x\n y */\n\tGAMEMSG_B,\n};\n";
        let def = parse_enum(src, "e").unwrap();
        assert_eq!(def.members.len(), 2);
    }

    #[test]
    fn crlf_headers_keep_line_numbers() {
        let src = b"enum e{\r\n\tGAMEMSG_A = 1,\r\n\tGAMEMSG_B,\r\n};\r\n";
        let def = parse_enum(src, "e").unwrap();
        assert_eq!(def.members[0].line, 2);
        assert_eq!(def.members[1].line, 3);
        assert_eq!(def.members[1].value, 2);
    }

    #[test]
    fn multibyte_comments_do_not_hide_syntax() {
        // CP949 trail bytes start at 0x41, so a Hangul syllable can never end in
        // `/` (0x2F). Only the real comment markers may strip anything.
        let src = "enum e{\n\t/* \u{C5D0}\u{B2E8} */\n\tGAMEMSG_A = 1,\n};\n".as_bytes();
        let def = parse_enum(src, "e").unwrap();
        assert_eq!(def.members.len(), 1);
    }

    #[test]
    fn rejects_preprocessor_directive() {
        let src = b"enum e{\n\tGAMEMSG_A = 1,\n#ifdef FOO\n\tGAMEMSG_B,\n#endif\n};\n";
        let err = parse_enum(src, "e").unwrap_err();
        assert!(err.message.contains("preprocessor"), "{err}");
    }

    #[test]
    fn rejects_value_beyond_u16() {
        let src = b"enum e{\n\tGAMEMSG_A = 70000,\n};\n";
        let err = parse_enum(src, "e").unwrap_err();
        assert!(err.message.contains("16-bit"), "{err}");
    }

    #[test]
    fn rejects_non_numeric_rebase() {
        let src = b"enum e{\n\tGAMEMSG_A = BASE + 1,\n};\n";
        assert!(parse_enum(src, "e").is_err());
    }

    #[test]
    fn rejects_garbage_enumerator() {
        let src = b"enum e{\n\tGAMEMSG_A = 1,\n\t?????\n};\n";
        assert!(parse_enum(src, "e").is_err());
    }

    #[test]
    fn missing_tag_is_reported() {
        assert!(parse_enum(b"enum other{}\n", "e").is_err());
    }

    #[test]
    fn unterminated_block_comment_is_reported() {
        assert!(parse_enum(b"enum e{\n/* never closed\n", "e").is_err());
    }

    #[test]
    fn line_without_trailing_newline_is_counted() {
        let def = parse_enum(b"enum e{\n\tGAMEMSG_A = 1,\n}", "e").unwrap();
        assert_eq!(def.members.len(), 1);
        assert_eq!(def.end_line, 3);
    }
}
