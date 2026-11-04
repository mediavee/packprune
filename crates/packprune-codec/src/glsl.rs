use crate::{CodecError, invalid};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token<'a> {
    pub bytes: &'a [u8],
    pub directive: bool,
    /// The line the token starts on, from 1: `__LINE__` and `#line` make it part of what the shader computes.
    pub line: usize,
}

/// The line breaks in `bytes`: CR LF, a lone CR and a lone LF each end one line.
fn breaks(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .enumerate()
        .filter(|&(i, &b)| b == b'\n' || b == b'\r' && bytes.get(i + 1) != Some(&b'\n'))
        .count()
}

pub fn tokenize(bytes: &[u8]) -> Result<Vec<Token<'_>>, CodecError> {
    let mut tokens = Vec::new();
    let mut i = 0;
    let mut line_start = 0;
    let mut line_has_token = false;
    let (mut line, mut counted) = (1, 0);
    let mut at = |start: usize| {
        line += breaks(&bytes[counted..start]);
        counted = start;
        line
    };
    if bytes
        .iter()
        .any(|&b| !(b.is_ascii_graphic() || matches!(b, b' ' | b'\t' | b'\n' | b'\r')))
    {
        return Err(invalid("GLSL", "byte outside the GLSL character set"));
    }
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            if matches!(bytes[i], b'\r' | b'\n') {
                line_start = i + 1;
                line_has_token = false;
            }
            i += 1;
            continue;
        }
        if bytes[i..].starts_with(b"//") {
            loop {
                while i < bytes.len() && !matches!(bytes[i], b'\r' | b'\n') {
                    i += 1;
                }
                if i == bytes.len() || bytes[i - 1] != b'\\' {
                    break;
                }
                if bytes.get(i) == Some(&b'\r') {
                    i += 1;
                }
                if bytes.get(i) == Some(&b'\n') {
                    i += 1;
                }
            }
            continue;
        }
        if bytes[i..].starts_with(b"/*") {
            i += 2;
            while i + 1 < bytes.len() && !bytes[i..].starts_with(b"*/") {
                if matches!(bytes[i], b'\r' | b'\n') {
                    line_start = i + 1;
                    line_has_token = false;
                }
                i += 1;
            }
            if i + 1 == bytes.len() || i == bytes.len() {
                return Err(invalid("GLSL", "unterminated block comment"));
            }
            i += 2;
            continue;
        }
        if bytes[i] == b'#' {
            if line_has_token {
                return Err(invalid("GLSL", "directive is not on its own line"));
            }
            let start = line_start;
            loop {
                while i < bytes.len() && !matches!(bytes[i], b'\r' | b'\n') {
                    i += 1;
                }
                let continued = i > 0 && bytes[i - 1] == b'\\';
                if bytes.get(i) == Some(&b'\r') {
                    i += 1;
                }
                if bytes.get(i) == Some(&b'\n') {
                    i += 1;
                }
                if !continued || i == bytes.len() {
                    break;
                }
            }
            let directive = &bytes[start..i];
            let mut rest = directive;
            while let Some(open) = rest.windows(2).position(|w| w == b"/*") {
                let body = &rest[open + 2..];
                let close = body
                    .windows(2)
                    .position(|w| w == b"*/")
                    .ok_or_else(|| invalid("GLSL", "block comment crosses a directive boundary"))?;
                rest = &body[close + 2..];
            }
            tokens.push(Token {
                bytes: directive,
                directive: true,
                line: at(start),
            });
            line_start = i;
            line_has_token = false;
            continue;
        }
        let start = i;
        let b = bytes[i];
        if b.is_ascii_alphabetic() || b == b'_' {
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
        } else if b.is_ascii_digit()
            || b == b'.' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit)
        {
            i += 1;
            while i < bytes.len() {
                let c = bytes[i];
                if c.is_ascii_alphanumeric()
                    || matches!(c, b'_' | b'.')
                    || matches!(c, b'+' | b'-') && matches!(bytes[i - 1], b'e' | b'E' | b'p' | b'P')
                {
                    i += 1;
                } else {
                    break;
                }
            }
        } else {
            let width = [
                b"<<=".as_slice(),
                b">>=",
                b"++",
                b"--",
                b"<<",
                b">>",
                b"<=",
                b">=",
                b"==",
                b"!=",
                b"&&",
                b"||",
                b"^^",
                b"*=",
                b"/=",
                b"+=",
                b"-=",
                b"%=",
                b"&=",
                b"^=",
                b"|=",
            ]
            .into_iter()
            .find(|op| bytes[i..].starts_with(op))
            .map(<[u8]>::len);
            if let Some(width) = width {
                i += width;
            } else if b"[](){}.,;:?~!+-*/%<>=&^|".contains(&b) {
                i += 1;
            } else {
                return Err(invalid("GLSL", format!("unsupported token at byte {i}")));
            }
        }
        tokens.push(Token {
            bytes: &bytes[start..i],
            directive: false,
            line: at(start),
        });
        line_has_token = true;
    }
    Ok(tokens)
}

/// The line breaks of `gap` in order, byte for byte: CR LF, a lone CR and a lone LF.
fn gap_breaks(gap: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut i = 0;
    std::iter::from_fn(move || {
        while i < gap.len() {
            let at = i;
            i += 1;
            match gap[at] {
                b'\r' if gap.get(i) == Some(&b'\n') => {
                    i += 1;
                    return Some(&gap[at..i]);
                }
                b'\r' | b'\n' => return Some(&gap[at..i]),
                _ => {}
            }
        }
        None
    })
}

fn ends_with_break(bytes: &[u8]) -> bool {
    matches!(bytes.last(), Some(b'\r' | b'\n'))
}

/// A shader as the 26.2 client and the GLSL compiler can read it: its tokens, every line break byte for byte, and
/// whether it ends with one (`StringUtil.endsWithNewLine`). `None` when anything else of the file can be read, so it
/// is kept and compared byte for byte:
/// - no token: `GlslPreprocessor.processImports` skips an empty import without numbering it (`__FILE__`);
/// - a comment naming a directive: its `moj_import` and `version` patterns match inside comments, a block comment's
///   line included;
/// - a backslash before a line break in a comment: drivers differ on whether it continues the comment;
/// - a stretch between two imports (or before the first, or after the last) holding comments and no token: the
///   client emits a stretch unless it is blank, and the next lines number on from it;
/// - a comment or a continued directive open at the first LF, where `GlDevice.compileShader` has
///   `GlslPreprocessor.injectDefines` insert its defines and `#line 1 0`: they would be read inside it;
/// - a file the lexer cannot read whole.
///
/// The first line, up to that LF, is part of the form byte for byte: the defines follow it.
pub fn canonical(bytes: &[u8]) -> Option<Vec<u8>> {
    let tokens = tokenize(bytes).ok()?;
    if tokens.is_empty() {
        return None;
    }
    let split = first_line_end(bytes);
    if !at_rest(bytes, &tokens, split) {
        return None;
    }
    let mut out = Vec::new();
    let mut put = |tag: u8, part: &[u8]| {
        out.push(tag);
        out.extend((part.len() as u64).to_le_bytes());
        out.extend(part);
    };
    put(b'f', &bytes[..split]);
    let (mut end, mut stretch_tokens, mut stretch_comment) = (0, false, false);
    for token in tokens.iter().map(Some).chain([None]) {
        let start = token.map_or(bytes.len(), |token| offset(bytes, token));
        let gap = &bytes[end..start];
        if names_a_directive(gap)
            || gap
                .windows(2)
                .any(|w| w[0] == b'\\' && matches!(w[1], b'\r' | b'\n'))
        {
            return None;
        }
        stretch_comment |= gap.iter().any(|b| !b.is_ascii_whitespace());
        for line_break in gap_breaks(gap) {
            put(b'n', line_break);
        }
        let import = token.is_some_and(|token| {
            token.directive && token.bytes.windows(10).any(|w| w == b"moj_import")
        });
        if token.is_none() || import {
            if stretch_comment && !stretch_tokens {
                return None;
            }
            (stretch_tokens, stretch_comment) = (false, false);
        } else {
            stretch_tokens = true;
        }
        if let Some(token) = token {
            put(if token.directive { b'd' } else { b't' }, token.bytes);
            end = start + token.bytes.len();
        }
    }
    out.push(u8::from(ends_with_break(bytes)));
    Some(out)
}

/// `#`, spaces, tabs or block comments, then `moj_import` or `version`: the start of what `REGEX_MOJ_IMPORT` and
/// `REGEX_VERSION` of 26.2's `GlslPreprocessor` match, wherever it is.
fn names_a_directive(text: &[u8]) -> bool {
    (0..text.len()).filter(|&at| text[at] == b'#').any(|at| {
        let mut rest = &text[at + 1..];
        loop {
            if let Some(after) = rest.strip_prefix(b" ").or_else(|| rest.strip_prefix(b"\t")) {
                rest = after;
            } else if let Some(body) = rest.strip_prefix(b"/*") {
                match body.windows(2).position(|w| w == b"*/") {
                    Some(close) => rest = &body[close + 2..],
                    None => return false,
                }
            } else {
                return rest.starts_with(b"moj_import") || rest.starts_with(b"version");
            }
        }
    })
}

/// Past the first LF, where `injectDefines` inserts (offsets 9-35: `indexOf(10) + 1`); the whole file without one.
fn first_line_end(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .position(|&b| b == b'\n')
        .map_or(bytes.len(), |at| at + 1)
}

/// No token and no comment goes on across `at`.
fn at_rest(bytes: &[u8], tokens: &[Token<'_>], at: usize) -> bool {
    let mut from = 0;
    for token in tokens {
        let start = offset(bytes, token);
        let end = start + token.bytes.len();
        if end <= at {
            from = end;
        } else if start < at {
            return false;
        } else {
            break;
        }
    }
    let gap = &bytes[from..at];
    let mut i = 0;
    while i < gap.len() {
        if gap[i..].starts_with(b"/*") {
            match gap[i + 2..].windows(2).position(|w| w == b"*/") {
                Some(close) => i += close + 4,
                None => return false,
            }
        } else if gap[i..].starts_with(b"//") {
            while i < gap.len() && !matches!(gap[i], b'\r' | b'\n') {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    true
}

fn offset(bytes: &[u8], token: &Token<'_>) -> usize {
    token.bytes.as_ptr() as usize - bytes.as_ptr() as usize
}

/// The first line as it is, then comments and the spaces between tokens dropped and every line break kept byte for
/// byte: the same [`canonical`] form. A file without one is kept as it is.
pub fn minify(bytes: &[u8]) -> Result<Vec<u8>, CodecError> {
    let Some(form) = canonical(bytes) else {
        tokenize(bytes)?;
        return Ok(bytes.to_vec());
    };
    let tokens = tokenize(bytes)?;
    let split = first_line_end(bytes);
    let mut out = bytes[..split].to_vec();
    let mut end = split;
    let mut previous: Option<&Token<'_>> = None;
    for token in tokens.iter().filter(|token| offset(bytes, token) >= split) {
        let start = offset(bytes, token);
        let mut broken = false;
        for line_break in gap_breaks(&bytes[end..start]) {
            out.extend_from_slice(line_break);
            broken = true;
        }
        if let Some(prev) = previous.filter(|prev| !broken && !prev.directive) {
            let mut adjacent = prev.bytes.to_vec();
            adjacent.extend_from_slice(token.bytes);
            let can_join = tokenize(&adjacent).is_ok_and(|pair| {
                pair.len() == 2 && pair[0].bytes == prev.bytes && pair[1].bytes == token.bytes
            });
            if !can_join {
                out.push(b' ');
            }
        }
        out.extend_from_slice(token.bytes);
        end = start + token.bytes.len();
        previous = Some(token);
    }
    for line_break in gap_breaks(&bytes[end..]) {
        out.extend_from_slice(line_break);
    }
    if ends_with_break(&out) && !ends_with_break(bytes) {
        out.push(b' ');
    }
    if canonical(&out).as_ref() != Some(&form) {
        return Err(invalid(
            "GLSL",
            "minification changed what the client reads",
        ));
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

    /// `StringUtil.lineCount` and `endsWithNewLine` of the 26.2 client: the line breaks and how the file ends.
    fn lines(bytes: &[u8]) -> (Vec<&[u8]>, bool) {
        (gap_breaks(bytes).collect(), ends_with_break(bytes))
    }

    /// Minified as `expected`, with the input's exact line breaks and ending.
    fn minifies_to(input: &[u8], expected: &[u8]) {
        let out = minify(input).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out),
            String::from_utf8_lossy(expected)
        );
        assert_eq!(lines(&out), lines(input));
    }

    #[test]
    fn directives_and_continuations_are_byte_exact() {
        let input = b"// header\n  #version 150\r\n #define X(a) a \\\n + 1 // leave this\n#moj_import <fog.glsl>\n void main ( ) { //comment\nfloat x = X( 2 ); }\n";
        minifies_to(
            input,
            b"// header\n  #version 150\r\n #define X(a) a \\\n + 1 // leave this\n#moj_import <fog.glsl>\nvoid main(){\nfloat x=X(2);}\n",
        );
    }

    /// Drivers differ on whether a backslash continues a line comment: such a file is kept as it is.
    #[test]
    fn a_continued_line_comment_keeps_the_file() {
        for newline in ["\n", "\r\n", "\r"] {
            let input = format!(
                "float a = 1.0; // note \\{newline}float b = 2.0;{newline}void main() {{}}{newline}"
            );
            assert_eq!(minify(input.as_bytes()).unwrap(), input.as_bytes());
            assert_eq!(canonical(input.as_bytes()), None);
        }
    }

    #[test]
    fn token_boundaries_cannot_become_operators_or_comments() {
        let input = b"float a = 1e-3; a + + b; a / * b; a < < b; a - - b; int x = 2;";
        let out = minify(input).unwrap();
        assert_eq!(tokenize(&out).unwrap(), tokenize(input).unwrap());
        assert!(out.windows(3).any(|w| w == b"+ +"));
        assert!(out.windows(3).any(|w| w == b"/ *"));
    }

    /// An import's `#define LINE_NUMBER __LINE__` expands on the importer's lines: joining them changes the value.
    #[test]
    fn every_line_break_stays_byte_for_byte() {
        minifies_to(
            b"#version 330 core\n#moj_import <line_number.glsl>\n\nout vec4 fragColor;\n\nvoid main() {\n    fragColor = vec4(float(LINE_NUMBER) / 255.0);\n}\n",
            b"#version 330 core\n#moj_import <line_number.glsl>\n\nout vec4 fragColor;\n\nvoid main(){\nfragColor=vec4(float(LINE_NUMBER)/255.0);\n}\n",
        );
        minifies_to(
            b"a = 1;\r\n// c\r\n\rb = 2; /* x\n y */ c = 3;\n\n\n",
            b"a = 1;\r\n\r\n\rb=2;\nc=3;\n\n\n",
        );
        let token_lines =
            |bytes| -> Vec<usize> { tokenize(bytes).unwrap().iter().map(|t| t.line).collect() };
        assert_eq!(token_lines(b"a\r\nb\rc\n/* x\n */ d"), [1, 2, 3, 5]);
    }

    /// The client appends a line break to an import that does not end with one: the ending is kept.
    #[test]
    fn the_file_ends_as_it_did() {
        minifies_to(b"x = 1; // c", b"x = 1; // c");
        minifies_to(b"x = 1;\ny = 2; // c", b"x = 1;\ny=2;");
        minifies_to(b"x = 1;\ny = 2;\n// c", b"x = 1;\ny=2;\n ");
        minifies_to(b"x = 1;\ny = 2;\n  ", b"x = 1;\ny=2;\n ");
        minifies_to(b"x = 1;\ny = 2;\n\n", b"x = 1;\ny=2;\n\n");
    }

    /// The client skips an empty import without numbering it, and reads `moj_import` and `version` in comments:
    /// such files, and those where a stretch between imports holds only comments, are kept as they are.
    #[test]
    fn what_the_client_reads_beyond_tokens_keeps_the_file() {
        for kept in [
            b"/* intentionally empty\n   multiline include\n*/\n".as_slice(),
            b"\n\n  \n",
            b"",
            b"/*\n#moj_import <a.glsl>\n*/\nfloat x;\n",
            b"// #version 330\n#version 150\nfloat x;\n",
            b"// # /* x */\tmoj_import <a.glsl>\nfloat x;\n",
            b"#moj_import <a.glsl>\n// between\n#moj_import <b.glsl>\nfloat x;\n",
            b"float x;\n#moj_import <a.glsl>\n// after\n",
        ] {
            assert_eq!(canonical(kept), None, "{}", String::from_utf8_lossy(kept));
            assert_eq!(minify(kept).unwrap(), kept);
        }
        minifies_to(
            b"float x;\n// trigger #F0F000\nfloat y;\n",
            b"float x;\n\nfloat y;\n",
        );
        minifies_to(
            b"#moj_import <a.glsl>\n// note\nfloat x;\n#moj_import <b.glsl>\n",
            b"#moj_import <a.glsl>\n\nfloat x;\n#moj_import <b.glsl>\n",
        );
    }

    /// `injectDefines` inserts `#define`s and `#line 1 0` after the first LF: the first line stays as it is, and a file
    /// with a comment or a continued directive open there is kept, since they would hold what is inserted.
    #[test]
    fn the_first_line_is_where_defines_go() {
        minifies_to(
            b"#version 150 // first\nfloat  x; // second\n",
            b"#version 150 // first\nfloat x;\n",
        );
        minifies_to(
            b"/* one line */ float  x;\nfloat  y;\n",
            b"/* one line */ float  x;\nfloat y;\n",
        );
        for kept in [
            b"/* header\n   header continued */\n#version 330 core\nfloat x;\n".as_slice(),
            b"#define A 1 \\\n + 2\nfloat x;\n",
            b"float  x; /* no line feed */ float  y;",
            b"float x; /* lone \r carriage return */\rfloat  y; /* open\n */ float z;\n",
        ] {
            assert_eq!(
                minify(kept).unwrap(),
                kept,
                "{}",
                String::from_utf8_lossy(kept)
            );
        }
        assert_eq!(canonical(b"/* header\n*/\nfloat x;\n"), None);
        assert_eq!(canonical(b"#define A 1 \\\n + 2\nfloat x;\n"), None);
    }

    #[test]
    fn rejects_partial_lexing_and_keeps_line_sensitive_code() {
        for bad in [
            b"/*".as_slice(),
            b"float x; /* unfinished",
            b"void main() {\xff}",
            b"x \\ y",
        ] {
            assert!(minify(bad).is_err());
        }
        minifies_to(b"\n\n int line = __LINE__;\n", b"\n\nint line=__LINE__;\n");
        assert!(minify(b"#define F /* comment\n more */\nfloat x;\n").is_err());
    }
}
