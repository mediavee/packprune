//! Gson 2.14's `JsonReader` in `LENIENT` strictness, which is how `FontManager.GSON.fromJson` reads a font (and
//! `Language.loadFromJson` a lang file): the grammar differs from Jackson's strict one in more than comments.
//!
//! Accepted beyond JSON: `//`, `#` and `/* */` comments; single-quoted strings and names; unquoted names and string
//! values (`{type: bitmap}`); `=` and `=>` for `:`; `;` for `,`; an omitted array element reads as `null` (`[1,]` is
//! `[1, null]`); `true`, `false` and `null` in any case; a leading `)]}'` line; a UTF-8 BOM. A number that is not
//! followed by the end of a literal is an unquoted string (`01`, `1.`). Raw control characters are fine inside a
//! string. Still refused: a comma before the `}` of an object, several root values, anything but whitespace after the
//! root value (the trailing check runs after Gson has restored its strictness), nesting beyond 255.

use std::borrow::Cow;

use super::scan::{Frame, Path};
use super::{JsonError, Token, TokenKind};

const MAX_DEPTH: usize = 255;

pub(super) fn visit<'a>(
    json: &'a [u8],
    visitor: &mut dyn FnMut(&Token<'a>, &Path),
) -> Result<(), JsonError> {
    let mut reader = Reader {
        bytes: json,
        pos: if json.starts_with(&[0xEF, 0xBB, 0xBF]) {
            3
        } else {
            0
        },
        path: Path { frames: Vec::new() },
    };
    reader.skip_non_execute_prefix();
    let Some(first) = reader.peek()? else {
        return Ok(());
    };
    reader.value(first, visitor)?;
    while let Some(&c) = reader.bytes.get(reader.pos) {
        if !matches!(c, b' ' | b'\t' | b'\n' | b'\r') {
            return Err(reader.error("unexpected content after the root value"));
        }
        reader.pos += 1;
    }
    Ok(())
}

/// A byte that continues an unquoted literal: everything but the structural characters, whitespace, and the ones
/// that start a comment, an escape or a separator.
fn is_literal(byte: u8) -> bool {
    !matches!(
        byte,
        b'/' | b'\\'
            | b';'
            | b'#'
            | b'='
            | b'{'
            | b'}'
            | b'['
            | b']'
            | b':'
            | b','
            | b' '
            | b'\t'
            | 0x0C
            | b'\r'
            | b'\n'
    )
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    path: Path,
}

impl<'a> Reader<'a> {
    /// `consumeNonExecutePrefix`: the `)]}'` line some servers put before a JSON body.
    fn skip_non_execute_prefix(&mut self) {
        if let Ok(Some(_)) = self.peek()
            && self.bytes[self.pos..].starts_with(b")]}'\n")
        {
            self.pos += 5;
        }
    }

    /// The next byte after whitespace and comments, not consumed; `None` at the end of the input.
    fn peek(&mut self) -> Result<Option<u8>, JsonError> {
        while let Some(&c) = self.bytes.get(self.pos) {
            match c {
                b' ' | b'\t' | b'\n' | b'\r' => self.pos += 1,
                b'#' => self.skip_line(),
                b'/' => match self.bytes.get(self.pos + 1) {
                    Some(b'/') => self.skip_line(),
                    Some(b'*') => match self.bytes[self.pos + 2..]
                        .windows(2)
                        .position(|pair| pair == b"*/")
                    {
                        Some(end) => self.pos += end + 4,
                        None => return Err(self.error("unterminated comment")),
                    },
                    // A lone slash is not a literal character either: the caller refuses it.
                    _ => return Ok(Some(c)),
                },
                _ => return Ok(Some(c)),
            }
        }
        Ok(None)
    }

    /// [`Self::peek`] where the input must go on.
    fn need(&mut self) -> Result<u8, JsonError> {
        self.peek()?
            .ok_or_else(|| self.error("unexpected end of input"))
    }

    fn skip_line(&mut self) {
        while self
            .bytes
            .get(self.pos)
            .is_some_and(|&c| c != b'\n' && c != b'\r')
        {
            self.pos += 1;
        }
    }

    fn value(
        &mut self,
        c: u8,
        visitor: &mut dyn FnMut(&Token<'a>, &Path),
    ) -> Result<(), JsonError> {
        match c {
            b'"' | b'\'' => {
                let start = self.pos;
                let text = self.quoted(c)?;
                visitor(&token(TokenKind::String, start, self.pos, text), &self.path);
                Ok(())
            }
            b'[' => self.array(visitor),
            b'{' => self.object(visitor),
            _ => self.literal(visitor),
        }
    }

    fn array(&mut self, visitor: &mut dyn FnMut(&Token<'a>, &Path)) -> Result<(), JsonError> {
        self.open(TokenKind::StartArray, Frame::Array(None), visitor)?;
        let mut first = true;
        loop {
            let c = self.need()?;
            if c == b']' && first {
                return self.close(TokenKind::EndArray, visitor);
            }
            if let Some(Frame::Array(index)) = self.path.frames.last_mut() {
                *index = Some(index.map_or(0, |index| index + 1));
            }
            first = false;
            if matches!(c, b']' | b',' | b';') {
                // An omitted element: Gson reads a zero-length literal in an array as `null`.
                let at = self.pos;
                visitor(
                    &token(TokenKind::Null, at, at, Cow::Borrowed("")),
                    &self.path,
                );
            } else {
                self.value(c, visitor)?;
            }
            match self.need()? {
                b']' => return self.close(TokenKind::EndArray, visitor),
                b',' | b';' => self.pos += 1,
                _ => return Err(self.error("unterminated array")),
            }
        }
    }

    fn object(&mut self, visitor: &mut dyn FnMut(&Token<'a>, &Path)) -> Result<(), JsonError> {
        self.open(TokenKind::StartObject, Frame::Object(None), visitor)?;
        let mut first = true;
        loop {
            let mut c = self.need()?;
            if first {
                if c == b'}' {
                    return self.close(TokenKind::EndObject, visitor);
                }
            } else {
                match c {
                    b'}' => return self.close(TokenKind::EndObject, visitor),
                    b',' | b';' => {
                        self.pos += 1;
                        c = self.need()?;
                    }
                    _ => return Err(self.error("unterminated object")),
                }
            }
            first = false;
            let start = self.pos;
            let name = match c {
                b'"' | b'\'' => self.quoted(c)?,
                c if is_literal(c) => self.unquoted(),
                _ => return Err(self.error("expected a property name")),
            };
            if let Some(Frame::Object(current)) = self.path.frames.last_mut() {
                *current = Some(name.to_string());
            }
            visitor(&token(TokenKind::Name, start, self.pos, name), &self.path);
            match self.need()? {
                b':' => self.pos += 1,
                b'=' => {
                    self.pos += 1;
                    if self.bytes.get(self.pos) == Some(&b'>') {
                        self.pos += 1;
                    }
                }
                _ => return Err(self.error("expected a colon")),
            }
            let c = self.need()?;
            self.value(c, visitor)?;
        }
    }

    fn open(
        &mut self,
        kind: TokenKind,
        frame: Frame,
        visitor: &mut dyn FnMut(&Token<'a>, &Path),
    ) -> Result<(), JsonError> {
        if self.path.frames.len() >= MAX_DEPTH {
            return Err(self.error("nesting depth exceeds 255"));
        }
        visitor(
            &token(kind, self.pos, self.pos + 1, Cow::Borrowed("")),
            &self.path,
        );
        self.pos += 1;
        self.path.frames.push(frame);
        Ok(())
    }

    fn close(
        &mut self,
        kind: TokenKind,
        visitor: &mut dyn FnMut(&Token<'a>, &Path),
    ) -> Result<(), JsonError> {
        self.path.frames.pop();
        visitor(
            &token(kind, self.pos, self.pos + 1, Cow::Borrowed("")),
            &self.path,
        );
        self.pos += 1;
        Ok(())
    }

    /// A keyword, a number, or an unquoted string, in `JsonReader.doPeek`'s order.
    fn literal(&mut self, visitor: &mut dyn FnMut(&Token<'a>, &Path)) -> Result<(), JsonError> {
        let start = self.pos;
        if !is_literal(self.bytes[start]) {
            return Err(self.error("expected a value"));
        }
        let ends_literal = |end: usize| self.bytes.get(end).is_none_or(|&next| !is_literal(next));
        let keyword = [
            (&b"true"[..], TokenKind::True),
            (b"false", TokenKind::False),
            (b"null", TokenKind::Null),
        ]
        .into_iter()
        .find(|(word, _)| {
            self.bytes
                .get(start..start + word.len())
                .is_some_and(|text| text.eq_ignore_ascii_case(word))
                && ends_literal(start + word.len())
        });
        let (kind, end, text) = if let Some((word, kind)) = keyword {
            (kind, start + word.len(), Cow::Borrowed(""))
        } else if let Some((kind, end)) = self.number(start).filter(|&(_, end)| ends_literal(end)) {
            let text = std::str::from_utf8(&self.bytes[start..end]).expect("ASCII number");
            (kind, end, Cow::Borrowed(text))
        } else {
            let text = self.unquoted();
            (TokenKind::String, self.pos, text)
        };
        self.pos = end.max(self.pos);
        visitor(&token(kind, start, self.pos, text), &self.path);
        Ok(())
    }

    /// The longest JSON number at `start`, as `peekNumber` reads one.
    fn number(&self, start: usize) -> Option<(TokenKind, usize)> {
        let digits = |from: usize| {
            self.bytes[from.min(self.bytes.len())..]
                .iter()
                .take_while(|c| c.is_ascii_digit())
                .count()
        };
        let mut end = start + usize::from(self.bytes[start] == b'-');
        match self.bytes.get(end)? {
            b'0' => end += 1,
            b'1'..=b'9' => end += digits(end),
            _ => return None,
        }
        let mut kind = TokenKind::Int;
        if self.bytes.get(end) == Some(&b'.') && digits(end + 1) > 0 {
            kind = TokenKind::Float;
            end += 1 + digits(end + 1);
        }
        if matches!(self.bytes.get(end), Some(b'e' | b'E')) {
            let sign = usize::from(matches!(self.bytes.get(end + 1), Some(b'+' | b'-')));
            if digits(end + 1 + sign) > 0 {
                kind = TokenKind::Float;
                end += 1 + sign + digits(end + 1 + sign);
            }
        }
        Some((kind, end))
    }

    /// Literal characters from the cursor on.
    fn unquoted(&mut self) -> Cow<'a, str> {
        let bytes = self.bytes;
        let start = self.pos;
        while bytes.get(self.pos).is_some_and(|&c| is_literal(c)) {
            self.pos += 1;
        }
        String::from_utf8_lossy(&bytes[start..self.pos])
    }

    /// A string or name from its opening `quote`: the decoded text, the cursor after the closing quote.
    fn quoted(&mut self, quote: u8) -> Result<Cow<'a, str>, JsonError> {
        let bytes = self.bytes;
        let mut units: Vec<u16> = Vec::new();
        let mut escaped = false;
        let mut from = self.pos + 1;
        let mut at = from;
        loop {
            let Some(&c) = bytes.get(at) else {
                self.pos = at;
                return Err(self.error("unterminated string"));
            };
            if c == quote {
                let raw = String::from_utf8_lossy(&bytes[from..at]);
                self.pos = at + 1;
                if !escaped {
                    return Ok(raw);
                }
                units.extend(raw.encode_utf16());
                // A lone surrogate has no `String`: replacing it with U+FFFD would merge it with a real U+FFFD name.
                return String::from_utf16(&units)
                    .map(Cow::Owned)
                    .map_err(|_| self.error("lone surrogate in a \\u escape"));
            }
            if c != b'\\' {
                at += 1;
                continue;
            }
            escaped = true;
            units.extend(String::from_utf8_lossy(&bytes[from..at]).encode_utf16());
            self.pos = at + 1;
            units.push(self.escape()?);
            from = self.pos;
            at = from;
        }
    }

    /// `readEscapeCharacter`, the cursor just after the backslash.
    fn escape(&mut self) -> Result<u16, JsonError> {
        let Some(&c) = self.bytes.get(self.pos) else {
            return Err(self.error("unterminated string"));
        };
        self.pos += 1;
        Ok(match c {
            b'b' => 0x08,
            b't' => 0x09,
            b'n' | b'\n' => 0x0A,
            b'f' => 0x0C,
            b'r' => 0x0D,
            b'\'' | b'"' | b'\\' | b'/' => u16::from(c),
            b'u' => {
                let hex = self
                    .bytes
                    .get(self.pos..self.pos + 4)
                    .ok_or_else(|| self.error("unterminated string"))?;
                let mut value = 0_u16;
                for &digit in hex {
                    let digit = char::from(digit)
                        .to_digit(16)
                        .ok_or_else(|| self.error("malformed unicode escape"))?;
                    value = value << 4 | digit as u16;
                }
                self.pos += 4;
                value
            }
            _ => return Err(self.error("invalid escape sequence")),
        })
    }

    fn error(&self, why: &'static str) -> JsonError {
        JsonError::new(self.pos, why)
    }
}

fn token(kind: TokenKind, start: usize, end: usize, text: Cow<'_, str>) -> Token<'_> {
    Token {
        kind,
        span: start..end,
        text,
    }
}
