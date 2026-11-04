//! The one `.properties` parser, decoding as `java.util.Properties.load`.

use std::ops::Range;

use crate::{CodecError, invalid};

/// One logical entry, with the byte spans of its key, separator and value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Property {
    pub key: String,
    pub value: String,
    pub key_span: Range<u32>,
    pub separator_span: Range<u32>,
    pub value_span: Range<u32>,
    pub line: u32,
}

/// Malformed escapes or unpaired UTF-16 surrogates yield no entries.
/// Use `parse_checked` to distinguish invalid input from an empty file.
pub fn parse(bytes: &[u8]) -> Vec<Property> {
    parse_checked(bytes).unwrap_or_default()
}

pub fn parse_checked(bytes: &[u8]) -> Result<Vec<Property>, CodecError> {
    if bytes.len() > u32::MAX as usize {
        return Err(invalid("properties", "spans exceed u32"));
    }
    let mut entries = Vec::new();
    let mut at = 0;
    let mut line = 1;
    while at < bytes.len() {
        let first_line = line;
        let mut logical = Vec::new();
        let mut positions = Vec::new();
        let end = loop {
            let mut start = at;
            while at < bytes.len() && !matches!(bytes[at], b'\r' | b'\n') {
                at += 1;
            }
            let end = at;
            if at < bytes.len() {
                let cr = bytes[at] == b'\r';
                at += 1;
                if cr && bytes.get(at) == Some(&b'\n') {
                    at += 1;
                }
                line += 1;
            }
            while start < end && whitespace(bytes[start]) {
                start += 1;
            }
            if logical.is_empty()
                && positions.is_empty()
                && matches!(bytes.get(start), Some(b'#' | b'!'))
            {
                break end;
            }
            let slashes = bytes[start..end]
                .iter()
                .rev()
                .take_while(|&&b| b == b'\\')
                .count();
            let continued = slashes % 2 == 1;
            let kept_end = end - usize::from(continued);
            logical.extend_from_slice(&bytes[start..kept_end]);
            positions.extend(start..kept_end);
            if !continued || at == bytes.len() {
                break kept_end;
            }
        };
        if logical.is_empty() {
            continue;
        }
        let key_start = logical
            .iter()
            .position(|&b| !whitespace(b))
            .unwrap_or(logical.len());
        if key_start == logical.len() {
            continue;
        }
        let mut key_end = key_start;
        while key_end < logical.len() {
            if logical[key_end] == b'\\' {
                key_end = (key_end + 2).min(logical.len());
            } else if whitespace(logical[key_end]) || matches!(logical[key_end], b'=' | b':') {
                break;
            } else {
                key_end += 1;
            }
        }
        let mut value_start = key_end;
        while value_start < logical.len() && whitespace(logical[value_start]) {
            value_start += 1;
        }
        if matches!(logical.get(value_start), Some(b'=' | b':')) {
            value_start += 1;
        }
        while value_start < logical.len() && whitespace(logical[value_start]) {
            value_start += 1;
        }
        let position = |i: usize| positions.get(i).copied().unwrap_or(end) as u32;
        let key_end_position = if key_end > key_start {
            (positions[key_end - 1] + 1) as u32
        } else {
            position(key_start)
        };
        entries.push(Property {
            key: unescape(&logical[key_start..key_end])?,
            value: unescape(&logical[value_start..])?,
            key_span: position(key_start)..key_end_position,
            separator_span: key_end_position..position(value_start),
            value_span: position(value_start)..end as u32,
            line: first_line,
        });
    }
    Ok(entries)
}

fn whitespace(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\x0c')
}

fn unescape(bytes: &[u8]) -> Result<String, CodecError> {
    let mut utf16 = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let mut b = bytes[i];
        i += 1;
        if b == b'\\' {
            b = *bytes
                .get(i)
                .ok_or_else(|| invalid("properties", "dangling escape"))?;
            i += 1;
            b = match b {
                b't' => b'\t',
                b'n' => b'\n',
                b'r' => b'\r',
                b'f' => b'\x0c',
                b'u' => {
                    let digits = bytes
                        .get(i..i + 4)
                        .ok_or_else(|| invalid("properties", "short Unicode escape"))?;
                    let mut unit = 0;
                    for &digit in digits {
                        let hex = (digit as char)
                            .to_digit(16)
                            .ok_or_else(|| invalid("properties", "invalid Unicode escape"))?;
                        unit = (unit << 4) | hex as u16;
                    }
                    utf16.push(unit);
                    i += 4;
                    continue;
                }
                other => other,
            };
        }
        utf16.push(u16::from(b));
    }
    String::from_utf16(&utf16).map_err(|e| invalid("properties", e))
}

pub fn minify(bytes: &[u8]) -> Result<Vec<u8>, CodecError> {
    let before = parse_checked(bytes)?;
    let mut out = Vec::new();
    for entry in &before {
        out.extend_from_slice(&bytes[entry.key_span.start as usize..entry.key_span.end as usize]);
        out.push(b'=');
        out.extend_from_slice(
            &bytes[entry.value_span.start as usize..entry.value_span.end as usize],
        );
        out.push(b'\n');
    }
    let after = parse_checked(&out)?;
    if !before
        .iter()
        .map(|p| (&p.key, &p.value))
        .eq(after.iter().map(|p| (&p.key, &p.value)))
    {
        return Err(invalid("properties", "minification changed entries"));
    }
    Ok(if out.len() < bytes.len() {
        out
    } else {
        bytes.to_vec()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_config_continuation_and_spans() {
        let bytes = include_bytes!(
            "../../../tests/fixtures/sources/configs/keys-properties/input/menu.properties"
        );
        let entries = parse(bytes);
        assert_eq!(entries.len(), 4);
        let sound = &entries[2];
        assert_eq!(
            (&*sound.key, &*sound.value, sound.line),
            ("long.sound", "viki:continued", 4)
        );
        assert_eq!(
            &bytes[sound.key_span.start as usize..sound.key_span.end as usize],
            b"long.sound"
        );
        assert_eq!(
            &bytes[sound.value_span.start as usize..sound.value_span.end as usize],
            b"viki:\\\n   continued"
        );
    }

    #[test]
    fn java_escapes_separators_and_latin1() {
        let entries = parse(b" \t!comment\\\r\n a\\ b\\:c \t:  \\t\\u0041\\q\r\ncontinued=one\\\n \t\\\n two\nkey=\\uD83D\\uDE00\nlast=\xe9\n");
        let pairs: Vec<_> = entries.iter().map(|p| (&*p.key, &*p.value)).collect();
        assert_eq!(
            pairs,
            [
                ("a b:c", "\tAq"),
                ("continued", "onetwo"),
                ("key", "😀"),
                ("last", "é")
            ]
        );
        assert_eq!(entries[1].line, 3);
    }

    #[test]
    fn empty_continued_line_still_allows_a_comment() {
        assert!(parse(b"\\\n#comment\n").is_empty());
        assert_eq!(parse(b"key=\\\n#value\n")[0].value, "#value");
    }

    #[test]
    fn minifier_keeps_raw_keys_and_values_while_removing_separator_space() {
        let input = b" #comment\r\n  a\\:key \t:  \\u0041\\\n  continued  \r\n\nempty\n";
        assert_eq!(
            minify(input).unwrap(),
            b"a\\:key=\\u0041\\\n  continued  \nempty=\n"
        );
        assert!(parse_checked(b"key=\\u12XX\n").is_err());
        assert!(minify(b"key=\\uD800\n").is_err());
    }
}
