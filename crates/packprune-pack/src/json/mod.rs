//! JSON: a spanned tokenizer with Jackson's strict default grammar and an in-place splice that changes the named
//! byte ranges and keeps every other byte.
//!
//! The 26.2 client is not more lenient where packs put JSON: models, blockstates, items, atlases, equipment,
//! post effects, particles, `sounds.json` and every `.mcmeta` go through Gson with `Strictness.STRICT`
//! (`net.minecraft.util.StrictJsonParser.parse`, `GsonHelper.parse`/`fromJson`), so comments, trailing commas and
//! unquoted names fail there. Only fonts (`FontManager.GSON.fromJson`) and lang files (`Language.loadFromJson`, a
//! default `Gson`) read leniently, with the grammar of `lenient.rs`. A file the strict grammar refuses is invalid and
//! kept as it is.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::ops::Range;

mod java;
mod lenient;
mod node;
mod scan;
#[cfg(test)]
mod tests;
mod write;

pub use java::{java_double, java_float};
pub use node::{Member, Node, Seg, Value, parse, parse_lenient};
pub use scan::Path;
pub use write::{Writer, write};

/// An RFC 6901 JSON pointer into a file, as the spanned tokenizer reports it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pointer(pub Box<str>);

impl Pointer {
    pub fn new(pointer: &str) -> Self {
        Self(pointer.into())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TokenKind {
    StartObject,
    EndObject,
    StartArray,
    EndArray,
    Name,
    String,
    Int,
    Float,
    True,
    False,
    Null,
}

/// One token: its bytes `[span.start, span.end)`, and its text: decoded for a name or a string, as written for a
/// number, empty otherwise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token<'a> {
    pub kind: TokenKind,
    pub span: Range<usize>,
    pub text: Cow<'a, str>,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid JSON at byte {offset}: {why}")]
pub struct JsonError {
    pub offset: usize,
    pub why: &'static str,
}

impl JsonError {
    fn new(offset: usize, why: &'static str) -> Self {
        Self { offset, why }
    }
}

/// Calls `visitor` with every token of `json` in order and where it sits, until the end or the first grammar
/// error. Several root values are read one after the other.
pub fn visit<'a>(
    json: &'a [u8],
    mut visitor: impl FnMut(&Token<'a>, &Path),
) -> Result<(), JsonError> {
    scan::visit(json, &mut visitor)
}

/// [`visit`] with the grammar of the files the client reads with a lenient Gson (fonts and lang files): Gson 2.14's
/// `JsonReader` in `LENIENT` strictness, see [`lenient`].
pub fn visit_lenient<'a>(
    json: &'a [u8],
    mut visitor: impl FnMut(&Token<'a>, &Path),
) -> Result<(), JsonError> {
    lenient::visit(json, &mut visitor)
}

/// True when `json` is a token stream [`edit`] can edit: zero or more root values.
pub fn parses(json: &[u8]) -> bool {
    visit(json, |_, _| {}).is_ok()
}

/// Whether a pack file is valid JSON (Jackson `readTree`): at most one root value. An empty file passes.
pub fn validate(json: &[u8]) -> Result<(), JsonError> {
    let mut roots = 0;
    let mut extra = None;
    visit(json, |token, path| {
        if path.depth() == 0 && !matches!(token.kind, TokenKind::EndObject | TokenKind::EndArray) {
            roots += 1;
            if roots == 2 && extra.is_none() {
                extra = Some(token.span.start);
            }
        }
    })?;
    match extra {
        Some(offset) => Err(JsonError::new(
            offset,
            "trailing token after the root value",
        )),
        None => Ok(()),
    }
}

/// Bytes that replace `range` of a file; an empty range inserts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Splice {
    pub range: Range<usize>,
    pub bytes: Vec<u8>,
}

/// `json` with each splice applied; every byte outside the ranges stays. Ranges must not overlap.
pub fn splice(json: &[u8], mut splices: Vec<Splice>) -> Vec<u8> {
    splices.sort_by_key(|splice| splice.range.start);
    let mut out = Vec::with_capacity(json.len());
    let mut at = 0;
    for splice in &splices {
        assert!(
            splice.range.start >= at && splice.range.end <= json.len(),
            "splices overlap or leave the file: {:?}",
            splice.range
        );
        out.extend_from_slice(&json[at..splice.range.start]);
        out.extend_from_slice(&splice.bytes);
        at = splice.range.end;
    }
    out.extend_from_slice(&json[at..]);
    out
}

/// A property added at the end of the object at a pointer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub value: String,
}

#[derive(Debug, thiserror::Error)]
pub enum EditError {
    #[error(transparent)]
    Json(#[from] JsonError),
    #[error("not every pointer names a value of its kind: {strings:?} {fields:?}")]
    Unmatched {
        strings: Vec<Pointer>,
        fields: Vec<Pointer>,
    },
}

/// The strings at the pointers of `strings` set to their new values, and the fields of
/// `fields` added to the objects at their pointers. A pointer that names no string, or no object, is an error.
pub fn edit(
    json: &[u8],
    strings: &BTreeMap<Pointer, String>,
    fields: &BTreeMap<Pointer, Vec<Field>>,
) -> Result<Vec<u8>, EditError> {
    let mut splices = Vec::new();
    let mut found = 0;
    // Per open object: whether it has a member, so an added field needs a comma before it.
    let mut members: Vec<bool> = Vec::new();
    visit(json, |token, path| match token.kind {
        TokenKind::Name => {
            if let Some(last) = members.last_mut() {
                *last = true;
            }
        }
        TokenKind::StartObject => members.push(false),
        TokenKind::String if !strings.is_empty() => {
            if let Some(value) = strings.get(&path.pointer()) {
                splices.push(Splice {
                    range: token.span.clone(),
                    bytes: quote(value).into_bytes(),
                });
                found += 1;
            }
        }
        TokenKind::EndObject => {
            let populated = members.pop().unwrap_or(false);
            if let Some(added) = (!fields.is_empty())
                .then(|| fields.get(&path.pointer()))
                .flatten()
            {
                let mut text = String::new();
                for (index, field) in added.iter().enumerate() {
                    if populated || index > 0 {
                        text.push(',');
                    }
                    text.push_str(&quote(&field.name));
                    text.push(':');
                    text.push_str(&quote(&field.value));
                }
                splices.push(Splice {
                    range: token.span.start..token.span.start,
                    bytes: text.into_bytes(),
                });
                found += 1;
            }
        }
        _ => {}
    })?;
    if found != strings.len() + fields.len() {
        return Err(EditError::Unmatched {
            strings: strings.keys().cloned().collect(),
            fields: fields.keys().cloned().collect(),
        });
    }
    Ok(splice(json, splices))
}

/// Each number `rewrite` gives a new text for replaced by it; `None`, or the
/// same text, keeps the token.
pub fn rewrite_numbers(
    json: &[u8],
    mut rewrite: impl FnMut(&Token<'_>, &Path) -> Option<String>,
) -> Result<Vec<u8>, JsonError> {
    let mut splices = Vec::new();
    visit(json, |token, path| {
        if matches!(token.kind, TokenKind::Int | TokenKind::Float)
            && let Some(text) = rewrite(token, path)
            && text != token.text
        {
            splices.push(Splice {
                range: token.span.clone(),
                bytes: text.into_bytes(),
            });
        }
    })?;
    Ok(splice(json, splices))
}

/// A JSON string literal for `value`, escaping quote, backslash and control characters only.
pub fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
