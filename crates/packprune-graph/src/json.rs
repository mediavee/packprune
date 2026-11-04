//! What the graph's walk adds to the shared JSON tree (`packprune_pack::json`): JSON pointers, resource ids and type
//! names.

pub(crate) use packprune_pack::json::{Node, Seg};

/// `pointer` extended by one segment, escaped as RFC 6901 (and Jackson's `appendProperty`) does.
pub(crate) fn child(pointer: &str, seg: Seg<'_>) -> String {
    let mut out = String::with_capacity(pointer.len() + 8);
    out.push_str(pointer);
    out.push('/');
    match seg {
        Seg::Name(name) => {
            for c in name.chars() {
                match c {
                    '~' => out.push_str("~0"),
                    '/' => out.push_str("~1"),
                    c => out.push(c),
                }
            }
        }
        Seg::Index(index) => out.push_str(&index.to_string()),
    }
    out
}

/// The text of one escaped pointer segment.
pub(crate) fn unescape(segment: &str) -> String {
    segment.replace("~1", "/").replace("~0", "~")
}

/// A resource id's `(namespace, path)`, `minecraft` when the namespace is missing; `None` for an invalid id.
pub(crate) fn id_parts(value: &str) -> Option<(&str, &str)> {
    let (ns, path) = match value.find(':') {
        Some(colon) if colon > 0 => (&value[..colon], &value[colon + 1..]),
        Some(_) => ("minecraft", &value[1..]),
        None => ("minecraft", value),
    };
    let valid = |text: &str, slash: bool| {
        text.bytes().all(|byte| {
            matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-') || slash && byte == b'/'
        })
    };
    (valid(ns, false) && valid(path, true)).then_some((ns, path))
}

/// A type's text: its path alone when it is a `minecraft:` id, else the text as written.
pub(crate) fn type_name(text: &str) -> &str {
    match id_parts(text) {
        Some(("minecraft", path)) => path,
        _ => text,
    }
}

/// The node's `type` string through [`type_name`], empty without one.
pub(crate) fn node_type(node: &Node) -> &str {
    node.string("type")
        .and_then(Node::as_str)
        .map_or("", type_name)
}
