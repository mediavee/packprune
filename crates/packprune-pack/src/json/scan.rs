//! The tokenizer: Jackson 3.2.3's `UTF8StreamJsonParser` with its default features. Its leniencies are kept on
//! purpose (overlong UTF-8, lone `\u` surrogates in values, several root values), and so are its limits
//! (`StreamReadConstraints` defaults).

use std::borrow::Cow;
use std::ops::Range;

use super::{JsonError, Pointer, Token, TokenKind};

const MAX_DEPTH: usize = 500;
const MAX_NUMBER_LEN: usize = 1000;
const MAX_STRING_LEN: usize = 100_000_000;
const MAX_NAME_LEN: usize = 50_000;

/// Where a token sits: the property names and array indexes leading to it.
pub struct Path {
    pub(super) frames: Vec<Frame>,
}

pub(super) enum Frame {
    Object(Option<String>),
    Array(Option<usize>),
}

impl Path {
    /// The RFC 6901 pointer of the current token, as Jackson's `pathAsPointer`: a container's own pointer on its start
    /// and end tokens, a property's value pointer on its name.
    pub fn pointer(&self) -> Pointer {
        let mut out = String::new();
        for frame in &self.frames {
            match frame {
                Frame::Object(Some(name)) => {
                    out.push('/');
                    for c in name.chars() {
                        match c {
                            '~' => out.push_str("~0"),
                            '/' => out.push_str("~1"),
                            c => out.push(c),
                        }
                    }
                }
                Frame::Array(Some(index)) => {
                    out.push('/');
                    out.push_str(&index.to_string());
                }
                _ => {}
            }
        }
        Pointer(out.into_boxed_str())
    }

    /// Nesting depth of the current token: 0 for a root value.
    pub fn depth(&self) -> usize {
        self.frames.len()
    }
}

pub(super) fn visit<'a>(
    json: &'a [u8],
    visitor: &mut dyn FnMut(&Token<'a>, &Path),
) -> Result<(), JsonError> {
    let start = encoding_start(json)?;
    Scanner {
        bytes: json,
        pos: start,
        path: Path { frames: Vec::new() },
        expect_comma: Vec::new(),
    }
    .run(visitor)
}

/// Jackson's `ByteSourceJsonBootstrapper`: a UTF-8 BOM is skipped when at least four bytes are there; a BOM or zero
/// byte pattern that announces UTF-16 or UTF-32 is refused (Jackson would decode it; the client reads UTF-8 only).
fn encoding_start(json: &[u8]) -> Result<usize, JsonError> {
    let not_utf8 = |offset| Err(JsonError::new(offset, "not UTF-8 encoded"));
    if json.len() >= 4 {
        let quad = u32::from_be_bytes([json[0], json[1], json[2], json[3]]);
        if matches!(quad, 0x0000_FEFF | 0xFFFE_0000 | 0x0000_FFFE | 0xFEFF_0000)
            || matches!(quad >> 16, 0xFEFF | 0xFFFE)
        {
            return not_utf8(0);
        }
        if quad >> 8 == 0xEF_BBBF {
            return Ok(3);
        }
        if quad >> 8 == 0
            || quad & 0x00FF_FFFF == 0
            || quad & !0x00FF_0000 == 0
            || quad & !0x0000_FF00 == 0
            || utf16(quad >> 16)
        {
            return not_utf8(0);
        }
    } else if json.len() >= 2 && utf16(u32::from(json[0]) << 8 | u32::from(json[1])) {
        return not_utf8(0);
    }
    Ok(0)
}

fn utf16(pair: u32) -> bool {
    pair & 0xFF00 == 0 || pair & 0x00FF == 0
}

struct Scanner<'a> {
    bytes: &'a [u8],
    pos: usize,
    path: Path,
    /// Per open container: whether an entry was read, so the next one needs a comma.
    expect_comma: Vec<bool>,
}

impl<'a> Scanner<'a> {
    fn run(mut self, visitor: &mut dyn FnMut(&Token<'a>, &Path)) -> Result<(), JsonError> {
        loop {
            let Some(c) = self.skip_ws()? else {
                if self.path.frames.is_empty() {
                    return Ok(());
                }
                return Err(self.error("unexpected end of input"));
            };
            let at = self.pos;
            if c == b']' || c == b'}' {
                let object = c == b'}';
                match self.path.frames.last() {
                    Some(Frame::Object(_)) if object => {}
                    Some(Frame::Array(_)) if !object => {}
                    _ => return Err(self.error("unexpected close marker")),
                }
                // The comma check comes after: Jackson closes on `]`/`}` before it looks for a separator.
                self.pos += 1;
                self.path.frames.pop();
                self.expect_comma.pop();
                let kind = if object {
                    TokenKind::EndObject
                } else {
                    TokenKind::EndArray
                };
                visitor(&token(kind, at..at + 1, Cow::Borrowed("")), &self.path);
                continue;
            }
            let mut c = c;
            if let Some(expect) = self.expect_comma.last_mut() {
                if *expect {
                    if c != b',' {
                        return Err(self.error("expected a comma between entries"));
                    }
                    self.pos += 1;
                    c = self
                        .skip_ws()?
                        .ok_or_else(|| self.error("unexpected end of input"))?;
                } else {
                    *expect = true;
                }
            }
            if let Some(Frame::Object(_)) = self.path.frames.last() {
                if c != b'"' {
                    return Err(self.error("expected a property name"));
                }
                let start = self.pos;
                let (name, end) = self.name()?;
                if let Some(Frame::Object(current)) = self.path.frames.last_mut() {
                    *current = Some(name.clone());
                }
                visitor(
                    &token(TokenKind::Name, start..end, Cow::Owned(name)),
                    &self.path,
                );
                let colon = self
                    .skip_ws()?
                    .ok_or_else(|| self.error("unexpected end of input"))?;
                if colon != b':' {
                    return Err(self.error("expected a colon"));
                }
                self.pos += 1;
                c = self
                    .skip_ws()?
                    .ok_or_else(|| self.error("unexpected end of input"))?;
            } else if let Some(Frame::Array(index)) = self.path.frames.last_mut() {
                *index = Some(index.map_or(0, |index| index + 1));
            }
            self.value(c, visitor)?;
        }
    }

    fn value(
        &mut self,
        c: u8,
        visitor: &mut dyn FnMut(&Token<'a>, &Path),
    ) -> Result<(), JsonError> {
        let start = self.pos;
        let token = match c {
            b'"' => {
                let (text, end) = self.string()?;
                token(TokenKind::String, start..end, text)
            }
            b'{' | b'[' => {
                if self.path.frames.len() >= MAX_DEPTH {
                    return Err(self.error("nesting depth exceeds 500"));
                }
                self.pos += 1;
                let kind = if c == b'{' {
                    TokenKind::StartObject
                } else {
                    TokenKind::StartArray
                };
                visitor(
                    &token(kind, start..start + 1, Cow::Borrowed("")),
                    &self.path,
                );
                self.path.frames.push(if c == b'{' {
                    Frame::Object(None)
                } else {
                    Frame::Array(None)
                });
                self.expect_comma.push(false);
                return Ok(());
            }
            b't' => self.keyword(b"true", TokenKind::True)?,
            b'f' => self.keyword(b"false", TokenKind::False)?,
            b'n' => self.keyword(b"null", TokenKind::Null)?,
            b'-' | b'0'..=b'9' => self.number()?,
            _ => return Err(self.error("unexpected character")),
        };
        visitor(&token, &self.path);
        Ok(())
    }

    /// Jackson's `_skipWSOrEnd`: space, tab, LF and CR only; comments are off.
    fn skip_ws(&mut self) -> Result<Option<u8>, JsonError> {
        while let Some(&c) = self.bytes.get(self.pos) {
            match c {
                b' ' | b'\t' | b'\n' | b'\r' => self.pos += 1,
                0..0x20 => return Err(self.error("invalid whitespace")),
                _ => return Ok(Some(c)),
            }
        }
        Ok(None)
    }

    fn keyword(&mut self, word: &[u8], kind: TokenKind) -> Result<Token<'a>, JsonError> {
        let start = self.pos;
        let end = start + word.len();
        if self.bytes.get(start..end) != Some(word) {
            return Err(self.error("unrecognized token"));
        }
        self.pos = end;
        // `_matchToken2`: a following identifier character makes it another token; any other byte at or above
        // 0x80 fails later wherever it stands.
        if let Some(&next) = self.bytes.get(end)
            && next >= b'0'
            && next != b']'
            && next != b'}'
            && (next.is_ascii_alphanumeric() || next == b'_' || next >= 0x7F)
        {
            return Err(self.error("unrecognized token"));
        }
        Ok(token(kind, start..end, Cow::Borrowed("")))
    }

    fn number(&mut self) -> Result<Token<'a>, JsonError> {
        let start = self.pos;
        if self.bytes[self.pos] == b'-' {
            self.pos += 1;
        }
        let int_start = self.pos;
        match self.bytes.get(self.pos) {
            Some(b'0') => {
                self.pos += 1;
                if self.bytes.get(self.pos).is_some_and(u8::is_ascii_digit) {
                    return Err(self.error("leading zeroes not allowed"));
                }
            }
            Some(b'1'..=b'9') => self.pos += self.digits(),
            _ => return Err(self.error("invalid number")),
        }
        let int_len = self.pos - int_start;
        let (mut fraction, mut exponent) = (0, 0);
        let mut float = false;
        if self.bytes.get(self.pos) == Some(&b'.') {
            float = true;
            self.pos += 1;
            fraction = self.digits();
            self.pos += fraction;
            if fraction == 0 {
                return Err(self.error("decimal point not followed by a digit"));
            }
            if self.bytes.get(self.pos) == Some(&b'.') {
                return Err(self.error("more than one decimal point"));
            }
        }
        if matches!(self.bytes.get(self.pos), Some(b'e' | b'E')) {
            float = true;
            self.pos += 1;
            if matches!(self.bytes.get(self.pos), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            exponent = self.digits();
            self.pos += exponent;
            if exponent == 0 {
                return Err(self.error("exponent indicator not followed by a digit"));
            }
        }
        if int_len + fraction + exponent > MAX_NUMBER_LEN {
            return Err(self.error("number longer than 1000 digits"));
        }
        if self.path.frames.is_empty()
            && let Some(&next) = self.bytes.get(self.pos)
            && !matches!(next, b' ' | b'\t' | b'\n' | b'\r')
        {
            return Err(self.error("root value not followed by whitespace"));
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos]).expect("ASCII digits");
        let kind = if float {
            TokenKind::Float
        } else {
            TokenKind::Int
        };
        Ok(token(kind, start..self.pos, Cow::Borrowed(text)))
    }

    fn digits(&self) -> usize {
        self.bytes[self.pos..]
            .iter()
            .take_while(|c| c.is_ascii_digit())
            .count()
    }

    /// A value string from its opening quote: its decoded text and the offset after its closing quote.
    fn string(&mut self) -> Result<(Cow<'a, str>, usize), JsonError> {
        let open = self.pos;
        let body = open + 1;
        let plain = self.bytes[body..]
            .iter()
            .position(|&c| c == b'"' || c == b'\\' || c < 0x20)
            .map(|offset| body + offset);
        // Strict UTF-8 without escapes reads the same in Jackson: borrow it.
        if let Some(close) = plain
            && self.bytes[close] == b'"'
            && let Ok(text) = std::str::from_utf8(&self.bytes[body..close])
        {
            self.pos = close + 1;
            if text.len() > MAX_STRING_LEN && text.encode_utf16().count() > MAX_STRING_LEN {
                return Err(self.error("string longer than 100000000 characters"));
            }
            return Ok((Cow::Borrowed(text), self.pos));
        }
        self.pos = body;
        let mut units = Vec::new();
        loop {
            let c = self.next_byte()?;
            match c {
                b'"' => break,
                b'\\' => units.push(self.escape()?),
                0..0x20 => return Err(self.error("unescaped control character in string")),
                0x20..0x80 => units.push(u16::from(c)),
                _ => self.utf8(c, &mut units)?,
            }
        }
        if units.len() > MAX_STRING_LEN {
            return Err(self.error("string longer than 100000000 characters"));
        }
        Ok((Cow::Owned(String::from_utf16_lossy(&units)), self.pos))
    }

    /// Jackson's `_decodeUtf8_2/3/4` for a value: continuation bytes are checked, overlong forms are not, a 3-byte
    /// surrogate is refused, and a 4-byte sequence is split with Jackson's own arithmetic.
    fn utf8(&mut self, lead: u8, units: &mut Vec<u16>) -> Result<(), JsonError> {
        let lead = i32::from(lead);
        let needed = match lead {
            0xC0..=0xDF => 1,
            0xE0..=0xEF => 2,
            0xF0..=0xF7 => 3,
            _ => return Err(self.error("invalid UTF-8 start byte")),
        };
        let mut value = lead & [0x1F, 0x0F, 0x07][needed - 1];
        for _ in 0..needed {
            let next = self.next_byte()?;
            if next & 0xC0 != 0x80 {
                return Err(self.error("invalid UTF-8 middle byte"));
            }
            value = value << 6 | i32::from(next & 0x3F);
        }
        match needed {
            2 if (0xD800..=0xDFFF).contains(&value) => {
                return Err(self.error("UTF-8 encoded surrogate"));
            }
            3 => {
                let value = value - 0x10000;
                units.push((0xD800 | (value >> 10)) as u16);
                units.push((0xDC00 | (value & 0x3FF)) as u16);
            }
            _ => units.push(value as u16),
        }
        Ok(())
    }

    fn escape(&mut self) -> Result<u16, JsonError> {
        Ok(match self.next_byte()? {
            b'b' => 0x08,
            b't' => 0x09,
            b'n' => 0x0A,
            b'f' => 0x0C,
            b'r' => 0x0D,
            c @ (b'"' | b'/' | b'\\') => u16::from(c),
            b'u' => {
                let mut value = 0_u16;
                for _ in 0..4 {
                    let digit = char::from(self.next_byte()?)
                        .to_digit(16)
                        .ok_or_else(|| self.error("expected a hex digit"))?;
                    value = value << 4 | digit as u16;
                }
                value
            }
            _ => return Err(self.error("unrecognized character escape")),
        })
    }

    /// A property name from its opening quote. Jackson collects its bytes, escapes re-encoded as UTF-8 with
    /// surrogate pairs required, then decodes them (`parseEscapedName`, `addName`).
    fn name(&mut self) -> Result<(String, usize), JsonError> {
        let body = self.pos + 1;
        if let Some(offset) = self.bytes[body..]
            .iter()
            .position(|&c| c == b'"' || c == b'\\' || c < 0x20)
            && self.bytes[body + offset] == b'"'
            && let Ok(text) = std::str::from_utf8(&self.bytes[body..body + offset])
        {
            if offset > MAX_NAME_LEN {
                return Err(self.error("name longer than 50000 bytes"));
            }
            self.pos = body + offset + 1;
            return Ok((text.to_owned(), self.pos));
        }
        self.pos = body;
        let mut raw = Vec::new();
        loop {
            let c = self.next_byte()?;
            match c {
                b'"' => break,
                0..0x20 => return Err(self.error("unescaped control character in name")),
                b'\\' => {
                    let unit = self.escape()?;
                    let code = match unit {
                        0xD800..=0xDBFF => {
                            if self.bytes.get(self.pos) != Some(&b'\\') {
                                return Err(self.error("broken surrogate pair in name"));
                            }
                            self.pos += 1;
                            let low = self.escape()?;
                            if !(0xDC00..=0xDFFF).contains(&low) {
                                return Err(self.error("incomplete surrogate pair"));
                            }
                            (u32::from(unit) << 10) + u32::from(low) - 0x35F_DC00
                        }
                        0xDC00..=0xDFFF => return Err(self.error("unexpected low surrogate")),
                        unit => u32::from(unit),
                    };
                    let mut buffer = [0; 4];
                    let encoded = char::from_u32(code).expect("not a surrogate");
                    raw.extend_from_slice(encoded.encode_utf8(&mut buffer).as_bytes());
                }
                c => raw.push(c),
            }
        }
        if raw.len() > MAX_NAME_LEN {
            return Err(self.error("name longer than 50000 bytes"));
        }
        let units = name_units(&raw).map_err(|why| self.error(why))?;
        Ok((String::from_utf16_lossy(&units), self.pos))
    }

    fn next_byte(&mut self) -> Result<u8, JsonError> {
        let c = *self
            .bytes
            .get(self.pos)
            .ok_or_else(|| self.error("unexpected end of input"))?;
        self.pos += 1;
        Ok(c)
    }

    fn error(&self, why: &'static str) -> JsonError {
        JsonError::new(self.pos, why)
    }
}

/// `addName`'s decoding of a name's bytes, which differs from a value's in the 4-byte arithmetic.
fn name_units(raw: &[u8]) -> Result<Vec<u16>, &'static str> {
    let mut units = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        let lead = i32::from(raw[i]);
        i += 1;
        if lead < 0x80 {
            units.push(lead as u16);
            continue;
        }
        let (mut value, needed) = match lead {
            0xC0..=0xDF => (lead & 0x1F, 1),
            0xE0..=0xEF => (lead & 0x0F, 2),
            0xF0..=0xF7 => (lead & 0x07, 3),
            _ => return Err("invalid UTF-8 start byte in name"),
        };
        if i + needed > raw.len() {
            return Err("unexpected end of name");
        }
        for _ in 0..needed {
            if raw[i] & 0xC0 != 0x80 {
                return Err("invalid UTF-8 middle byte in name");
            }
            value = value << 6 | i32::from(raw[i] & 0x3F);
            i += 1;
        }
        match needed {
            2 if (0xD800..=0xDFFF).contains(&value) => {
                return Err("UTF-8 encoded surrogate in name");
            }
            3 => {
                let value = value - 0x10000;
                units.push((0xD800 + (value >> 10)) as u16);
                units.push((0xDC00 | (value & 0x3FF)) as u16);
            }
            _ => units.push(value as u16),
        }
    }
    Ok(units)
}

fn token(kind: TokenKind, span: Range<usize>, text: Cow<'_, str>) -> Token<'_> {
    Token { kind, span, text }
}
