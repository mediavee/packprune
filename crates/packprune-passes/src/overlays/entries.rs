use packprune_pack::json::{self, Node, Value};
use packprune_pack::meta::{ANY_MINOR, next_format, overlay_range};
use packprune_pack::{FormatRange, PackMeta};
use packprune_pass::PassError;

use super::json::{object, set, string};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Shape {
    pub legacy: bool,
    pub modern: bool,
}

#[derive(Clone, Debug)]
pub(super) struct Entry {
    pub directory: String,
    pub range: FormatRange,
    pub shape: Shape,
    pub json: Node,
}

pub(super) fn fail(pass: &'static str, why: impl ToString) -> PassError {
    PassError::Failed {
        pass,
        why: why.to_string(),
    }
}

impl Entry {
    pub fn plain(&self) -> bool {
        if self.range.min.1 != 0 || self.range.max.1 != ANY_MINOR {
            return false;
        }
        if !self.shape.legacy || !self.shape.modern {
            return true;
        }
        let without = |fields: &[&str]| {
            let mut node = self.json.clone();
            if let Value::Object(members) = &mut node.value {
                members.retain(|member| !fields.contains(&&*member.name));
            }
            overlay_range(&node).ok()
        };
        without(&["min_format", "max_format"]) == without(&["formats"])
    }

    pub fn from(&self, major: u32) -> Self {
        let mut entry = self.clone();
        if let Value::Object(members) = &mut entry.json.value {
            for member in members {
                if &*member.name == "formats" {
                    let minimum = member.node.path("min_inclusive").as_int(0);
                    match &mut member.node.value {
                        Value::Array(pair)
                            if pair.len() == 2 && pair[0].as_int(0) < major as i32 =>
                        {
                            pair[0] = number(major)
                        }
                        Value::Object(_) if minimum < major as i32 => {
                            set(&mut member.node, "min_inclusive", number(major))
                        }
                        _ => {}
                    }
                }
            }
        }
        if self.shape.modern && self.range.min.0 < major {
            set(&mut entry.json, "min_format", number(major));
        }
        if self.range.min.0 < major {
            entry.range.min = (major, 0);
        }
        entry
    }

    pub fn created(directory: String, range: FormatRange, shape: Shape) -> Self {
        let mut json = object(&[("directory", string(&directory))]);
        if shape.legacy {
            set(
                &mut json,
                "formats",
                Node::new(Value::Array(vec![number(range.min.0), number(range.max.0)])),
            );
        }
        if shape.modern {
            set(&mut json, "min_format", number(range.min.0));
            set(&mut json, "max_format", number(range.max.0));
        }
        Self {
            directory,
            range,
            shape,
            json,
        }
    }
}

fn number(value: u32) -> Node {
    Node::new(Value::Int(value as i32))
}

pub(super) fn read(bytes: &[u8], pass: &'static str) -> Result<Vec<Entry>, PassError> {
    let root = json::parse(bytes).map_err(|error| fail(pass, error))?;
    let meta = PackMeta::parse(bytes).map_err(|error| fail(pass, error))?;
    root.path("overlays")
        .path("entries")
        .children()
        .zip(meta.overlays)
        .map(|(json, entry)| {
            if !json.is_object() {
                return Err(fail(pass, "pack.mcmeta: overlay entry is not an object"));
            }
            Ok(Entry {
                directory: entry.directory,
                range: entry.formats,
                shape: Shape {
                    legacy: json.has("formats"),
                    modern: json.has("min_format") || json.has("max_format"),
                },
                json: json.clone(),
            })
        })
        .collect()
}

pub(super) fn replace(
    bytes: &[u8],
    entries: &[Entry],
    pass: &'static str,
) -> Result<Vec<u8>, PassError> {
    let mut root = json::parse(bytes).map_err(|error| fail(pass, error))?;
    let mut overlays = root.path("overlays").clone();
    set(
        &mut overlays,
        "entries",
        Node::new(Value::Array(
            entries.iter().map(|entry| entry.json.clone()).collect(),
        )),
    );
    set(&mut root, "overlays", overlays);
    Ok(json::write(&root))
}

pub(super) fn touches(a: FormatRange, b: FormatRange) -> bool {
    b.min <= next_format(a.max) && a.min <= next_format(b.max)
}

pub(super) fn union(a: FormatRange, b: FormatRange) -> FormatRange {
    FormatRange {
        min: a.min.min(b.min),
        max: a.max.max(b.max),
    }
}
