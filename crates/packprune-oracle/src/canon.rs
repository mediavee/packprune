//! Canonical descriptors: trees with sorted keys and every number a double, so two files the client
//! reads alike give equal trees; their 128-bit digests; and where two trees differ. A shared subtree (a resolved
//! model) is stored once and referenced as `{"@model": <digest>}`.

use std::borrow::Cow;
use std::cmp::Ordering;

use packprune_pack::json::{Node, Value, Writer};

pub(crate) const REF: &str = "@model";
/// Lines shown per differing key.
const SHOWN: usize = 120;

/// A descriptor tree; object members keep insertion order.
#[derive(Clone, Debug)]
pub(crate) enum Desc {
    /// An absent value: written as `null`, but shown as absent.
    Missing,
    Null,
    Bool(bool),
    Int(i64),
    Float(f32),
    Double(f64),
    Str(Box<str>),
    Array(Vec<Desc>),
    Object(Vec<(Key, Desc)>),
}

/// A member name: the descriptors' own names are static, only names copied from a file are owned.
pub(crate) type Key = Cow<'static, str>;

impl PartialEq for Desc {
    /// Numbers equal by total order (NaN equals NaN, `0.0` differs from `-0.0`), objects as maps.
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Desc::Missing, Desc::Missing) | (Desc::Null, Desc::Null) => true,
            (Desc::Bool(a), Desc::Bool(b)) => a == b,
            (Desc::Int(a), Desc::Int(b)) => a == b,
            (Desc::Float(a), Desc::Float(b)) => a.total_cmp(b) == Ordering::Equal,
            (Desc::Double(a), Desc::Double(b)) => a.total_cmp(b) == Ordering::Equal,
            (Desc::Str(a), Desc::Str(b)) => a == b,
            (Desc::Array(a), Desc::Array(b)) => a == b,
            (Desc::Object(a), Desc::Object(b)) => {
                a.len() == b.len()
                    && a.iter().all(|(key, value)| {
                        b.iter()
                            .find(|(other, _)| other == key)
                            .is_some_and(|(_, other)| other == value)
                    })
            }
            _ => false,
        }
    }
}

impl Desc {
    pub(crate) fn object() -> Self {
        Desc::Object(Vec::new())
    }

    pub(crate) fn str(text: impl Into<Box<str>>) -> Self {
        Desc::Str(text.into())
    }

    /// Replaces a member in place, or appends it. A no-op on anything but an object.
    pub(crate) fn set(&mut self, key: impl Into<Key>, value: Desc) -> &mut Self {
        if let Desc::Object(members) = self {
            let key = key.into();
            match members.iter_mut().find(|(existing, _)| *existing == key) {
                Some((_, slot)) => *slot = value,
                None => members.push((key, value)),
            }
        }
        self
    }

    pub(crate) fn remove(&mut self, key: &str) {
        if let Desc::Object(members) = self {
            members.retain(|(existing, _)| &**existing != key);
        }
    }

    pub(crate) fn get(&self, key: &str) -> Option<&Desc> {
        match self {
            Desc::Object(members) => members
                .iter()
                .find(|(existing, _)| &**existing == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    pub(crate) fn push(&mut self, value: Desc) -> &mut Self {
        if let Desc::Array(items) = self {
            items.push(value);
        }
        self
    }

    pub(crate) fn is_empty_object(&self) -> bool {
        matches!(self, Desc::Object(members) if members.is_empty())
    }

    fn is_ref(&self) -> bool {
        matches!(self, Desc::Object(members) if members.len() == 1 && &*members[0].0 == REF)
    }

    /// The digest a `{"@model": ...}` reference holds.
    pub(crate) fn ref_digest(&self) -> Option<&str> {
        match self.get(REF) {
            Some(Desc::Str(digest)) if self.is_ref() => Some(digest),
            _ => None,
        }
    }
}

pub(crate) fn reference(digest: &str) -> Desc {
    let mut out = Desc::object();
    out.set(REF, Desc::str(digest));
    out
}

pub(crate) fn missing() -> Desc {
    let mut out = Desc::object();
    out.set("missing", Desc::Bool(true));
    out
}

/// Keys sorted by UTF-16 code unit, numbers as doubles, each string through `strings`.
pub(crate) fn canonical_with(raw: &Node, strings: &dyn Fn(&str) -> String) -> Desc {
    match &raw.value {
        Value::Object(members) => {
            let mut sorted: Vec<_> = members.iter().collect();
            sorted.sort_by(|a, b| java_order(&a.name, &b.name));
            Desc::Object(
                sorted
                    .into_iter()
                    .map(|member| {
                        (
                            Key::Owned(member.name.to_string()),
                            canonical_with(&member.node, strings),
                        )
                    })
                    .collect(),
            )
        }
        Value::Array(items) => Desc::Array(
            items
                .iter()
                .map(|item| canonical_with(item, strings))
                .collect(),
        ),
        Value::String(text) => Desc::str(strings(text)),
        Value::Bool(value) => Desc::Bool(*value),
        Value::Null => Desc::Null,
        Value::Missing => Desc::Missing,
        _ => Desc::Double(raw.double_value().unwrap_or(f64::NAN)),
    }
}

pub(crate) fn canonical(raw: &Node) -> Desc {
    canonical_with(raw, &str::to_owned)
}

/// Orders strings by UTF-16 code unit, as Java's `String.compareTo`.
pub(crate) fn java_order(a: &str, b: &str) -> Ordering {
    if a.is_ascii() && b.is_ascii() {
        return a.cmp(b);
    }
    a.encode_utf16().cmp(b.encode_utf16())
}

/// A 128-bit digest, written as unpadded base64url of 16 bytes.
pub(crate) type Digest = [u8; 16];

/// Hashes an encoding that two trees share exactly when their compact JSON text is the same: the shortest number
/// text is one per value, so a number's bits stand for it.
pub(crate) fn digest(node: &Desc) -> Digest {
    let mut bytes = Vec::with_capacity(512);
    encode_tree(node, &mut bytes);
    digest_bytes(&bytes)
}

fn encode_tree(node: &Desc, out: &mut Vec<u8>) {
    let text = |text: &str, out: &mut Vec<u8>| {
        out.extend_from_slice(&(text.len() as u64).to_le_bytes());
        out.extend_from_slice(text.as_bytes());
    };
    match node {
        Desc::Missing | Desc::Null => out.push(0),
        Desc::Bool(value) => out.extend_from_slice(&[1, u8::from(*value)]),
        Desc::Int(value) => {
            out.push(2);
            out.extend_from_slice(&value.to_le_bytes());
        }
        Desc::Float(value) => {
            out.push(3);
            out.extend_from_slice(&value.to_bits().to_le_bytes());
        }
        Desc::Double(value) => {
            out.push(4);
            out.extend_from_slice(&value.to_bits().to_le_bytes());
        }
        Desc::Str(value) => {
            out.push(5);
            text(value, out);
        }
        Desc::Array(items) => {
            out.push(6);
            out.extend_from_slice(&(items.len() as u64).to_le_bytes());
            items.iter().for_each(|item| encode_tree(item, out));
        }
        Desc::Object(members) => {
            out.push(7);
            out.extend_from_slice(&(members.len() as u64).to_le_bytes());
            for (key, value) in members {
                text(key, out);
                encode_tree(value, out);
            }
        }
    }
}

pub(crate) fn digest_bytes(bytes: &[u8]) -> Digest {
    truncate(blake3::hash(bytes))
}

pub(crate) fn truncate(hash: blake3::Hash) -> Digest {
    hash.as_bytes()[..16].try_into().expect("16 of 32 bytes")
}

pub(crate) fn encode(digest: &Digest) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(22);
    for chunk in digest.chunks(3) {
        let bits = chunk.iter().enumerate().fold(0u32, |bits, (i, &byte)| {
            bits | u32::from(byte) << (16 - 8 * i)
        });
        for i in 0..=chunk.len() {
            out.push(ALPHABET[(bits >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// `node` as compact JSON, which differences show.
fn write_json(node: &Desc, writer: &mut Writer<'_>) {
    match node {
        Desc::Missing | Desc::Null => writer.null(),
        Desc::Bool(value) => writer.bool(*value),
        Desc::Int(value) => writer.int(*value),
        Desc::Float(value) => writer.float(*value),
        Desc::Double(value) => writer.double(*value),
        Desc::Str(text) => writer.string(text),
        Desc::Array(items) => {
            writer.start_array();
            for item in items {
                write_json(item, writer);
            }
            writer.end_array();
        }
        Desc::Object(members) => {
            writer.start_object();
            for (key, value) in members {
                writer.name(key);
                write_json(value, writer);
            }
            writer.end_object();
        }
    }
}

/// Where `before` and `after` differ, one line per difference under `path`; a pair of model references is followed
/// into each side's table.
pub(crate) fn diff(
    path: &str,
    before: &Desc,
    after: &Desc,
    before_table: &dyn Fn(&str) -> Desc,
    after_table: &dyn Fn(&str) -> Desc,
) -> Vec<String> {
    let mut out = Vec::new();
    diff_into(path, before, after, before_table, after_table, &mut out);
    out
}

fn diff_into(
    path: &str,
    before: &Desc,
    after: &Desc,
    before_table: &dyn Fn(&str) -> Desc,
    after_table: &dyn Fn(&str) -> Desc,
    out: &mut Vec<String>,
) {
    if out.len() >= SHOWN || before == after {
        return;
    }
    match (before, after) {
        (Desc::Object(_), Desc::Object(_)) if before.is_ref() && after.is_ref() => {
            let (Some(a), Some(b)) = (before.ref_digest(), after.ref_digest()) else {
                return;
            };
            diff_into(
                path,
                &before_table(a),
                &after_table(b),
                before_table,
                after_table,
                out,
            );
        }
        (Desc::Object(a), Desc::Object(b)) => {
            let mut keys: Vec<&str> = a.iter().chain(b).map(|(key, _)| &**key).collect();
            keys.sort_by(|x, y| java_order(x, y));
            keys.dedup();
            for key in keys {
                let x = before.get(key).unwrap_or(&Desc::Missing);
                let y = after.get(key).unwrap_or(&Desc::Missing);
                diff_into(
                    &format!("{path}/{key}"),
                    x,
                    y,
                    before_table,
                    after_table,
                    out,
                );
            }
        }
        (Desc::Array(a), Desc::Array(b)) if a.len() == b.len() => {
            for (index, (x, y)) in a.iter().zip(b).enumerate() {
                diff_into(
                    &format!("{path}/{index}"),
                    x,
                    y,
                    before_table,
                    after_table,
                    out,
                );
            }
        }
        _ => out.push(format!("{path}: {} -> {}", show(before), show(after))),
    }
}

fn show(node: &Desc) -> String {
    if matches!(node, Desc::Missing) {
        return "(none)".into();
    }
    let text = match (node.ref_digest(), node) {
        (Some(digest), _) => format!("model {digest}"),
        (None, Desc::Array(items)) => format!("[{} entries] {}", items.len(), to_json(node)),
        _ => to_json(node),
    };
    // The cut counts UTF-16 units.
    if text.encode_utf16().count() <= 160 {
        return text;
    }
    let mut units = 0;
    let cut = text
        .char_indices()
        .find(|(_, c)| {
            units += c.len_utf16();
            units > 157
        })
        .map_or(text.len(), |(at, _)| at);
    format!("{}...", &text[..cut])
}

pub(crate) fn to_json(node: &Desc) -> String {
    let mut out = Vec::new();
    write_json(node, &mut Writer::new(&mut out));
    String::from_utf8(out).expect("UTF-8 in, UTF-8 out")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_of_sixteen_bytes() {
        assert_eq!(encode(&[0; 16]), "AAAAAAAAAAAAAAAAAAAAAA");
        assert_eq!(encode(&[0xff; 16]), "_____________________w");
    }

    #[test]
    fn differences_follow_both_sides_and_show_absence() {
        let mut a = Desc::object();
        a.set("x", Desc::Double(1.0)).set("y", Desc::Int(2));
        let mut b = Desc::object();
        b.set("x", Desc::Double(1.5));
        let none = |_: &str| Desc::Null;
        assert_eq!(
            diff("", &a, &b, &none, &none),
            vec!["/x: 1.0 -> 1.5", "/y: 2 -> (none)"]
        );
    }
}
