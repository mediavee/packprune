use crate::{CodecError, invalid};

pub fn json(bytes: &[u8]) -> Result<Vec<u8>, CodecError> {
    let before: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| invalid("JSON", e))?;
    let mut out = Vec::with_capacity(bytes.len());
    let mut in_string = false;
    let mut escaped = false;
    for &b in bytes {
        if in_string || !b.is_ascii_whitespace() {
            out.push(b);
        }
        if escaped {
            escaped = false;
        } else if in_string && b == b'\\' {
            escaped = true;
        } else if b == b'"' {
            in_string = !in_string;
        }
    }
    let after: serde_json::Value = serde_json::from_slice(&out).map_err(|e| invalid("JSON", e))?;
    if before != after {
        return Err(invalid("JSON", "minification changed the value"));
    }
    Ok(out)
}

pub fn legacy_lang(bytes: &[u8], strip_bom: bool) -> Result<Vec<u8>, CodecError> {
    let bytes = if strip_bom {
        bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes)
    } else {
        bytes
    };
    strip_lines(bytes, false)
}

pub fn mcfunction(bytes: &[u8]) -> Result<Vec<u8>, CodecError> {
    strip_lines(bytes, true)
}

fn strip_lines(bytes: &[u8], continued_commands: bool) -> Result<Vec<u8>, CodecError> {
    let text = std::str::from_utf8(bytes).map_err(|e| invalid("text", e))?;
    let mut out = Vec::with_capacity(bytes.len());
    let mut continued = false;
    let mut discard = false;
    let mut at = 0;
    while at < bytes.len() {
        let start = at;
        while at < bytes.len() && !matches!(bytes[at], b'\r' | b'\n') {
            at += 1;
        }
        if bytes.get(at) == Some(&b'\r') {
            at += 1;
        }
        if bytes.get(at) == Some(&b'\n') {
            at += 1;
        }
        let line = &text[start..at];
        let trimmed = line.trim_matches([' ', '\t', '\r', '\n']);
        let comment = if continued_commands {
            trimmed.starts_with('#')
        } else {
            line.starts_with('#')
        };
        if !continued {
            discard = comment || trimmed.is_empty();
        }
        if !discard {
            out.extend_from_slice(line.as_bytes());
        }
        continued = continued_commands && trimmed.ends_with('\\');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_keeps_number_key_order_duplicates_and_string_spellings() {
        let input = br#" { "b" : 1.00e+02, "a" : "space \\" , "b": "\u0061" } "#;
        assert_eq!(
            json(input).unwrap(),
            br#"{"b":1.00e+02,"a":"space \\","b":"\u0061"}"#
        );
    }

    #[test]
    fn json_refuses_extensions() {
        for bytes in [b"{/* comment */}".as_slice(), b"[1,]", b"{broken}"] {
            assert!(json(bytes).is_err());
        }
    }

    #[test]
    fn cr_only_and_crlf_continuations_keep_real_entries() {
        assert_eq!(
            legacy_lang(b"# comment\rkey=value\r", false).unwrap(),
            b"key=value\r"
        );
        assert_eq!(
            legacy_lang(b" #real=key\r", false).unwrap(),
            b" #real=key\r"
        );
        assert_eq!(mcfunction(b"#comment\rsay hi\r").unwrap(), b"say hi\r");
        assert_eq!(
            mcfunction(b"say hi \\\r\n #part of command\r\n").unwrap(),
            b"say hi \\\r\n #part of command\r\n"
        );
    }

    #[test]
    fn comment_continuations_are_removed_as_one_logical_line() {
        for newline in ["\n", "\r\n", "\r"] {
            let input = format!(
                " #execute as @a \\{newline}    run kill @s \\{newline}say hidden{newline}say ok{newline}"
            );
            assert_eq!(
                mcfunction(input.as_bytes()).unwrap(),
                format!("say ok{newline}").as_bytes()
            );
            let input = format!("say live \\{newline}# still command \\{newline}tail{newline}");
            assert_eq!(mcfunction(input.as_bytes()).unwrap(), input.as_bytes());
        }
    }

    #[test]
    fn text_preserves_values_and_command_continuations() {
        assert_eq!(
            legacy_lang(b"\xef\xbb\xbf# header\n\nkey= value  \r\n", true).unwrap(),
            b"key= value  \r\n"
        );
        assert_eq!(
            mcfunction(b"# header\n\nsay hi \\\n #part of command\n\nsay # kept\n").unwrap(),
            b"say hi \\\n #part of command\nsay # kept\n"
        );
    }
}
