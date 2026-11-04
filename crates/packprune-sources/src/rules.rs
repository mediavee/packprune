use std::collections::HashSet;

use packprune_graph::{IdGlob, SourceError, Usage, UsageKind};
use packprune_pack::{ResourceId, RunCx};
use serde_json::Value;

use crate::{
    document::{self, Node, Value as NodeValue},
    refused,
};

pub(crate) const BLOCKS_ATLAS: &str = "minecraft:blocks";

pub(crate) fn usage(kind: UsageKind, id: &str, location: String, reason: Option<String>) -> Usage {
    Usage {
        kind,
        id: IdGlob(id.into()),
        atlas: None,
        location,
        reason,
        declares: None,
        quiet: false,
    }
}

#[derive(Clone)]
pub(crate) struct Rule {
    pub kind: UsageKind,
    pub atlas: Option<ResourceId>,
    pub id: String,
    pub namespaced: bool,
    pub quiet: bool,
}

impl Rule {
    pub fn simple(kind: UsageKind) -> Self {
        Self {
            kind,
            atlas: None,
            id: "{id}".into(),
            namespaced: false,
            quiet: false,
        }
    }

    pub fn parse(node: &Value, where_: &str, cx: &RunCx) -> Result<Self, SourceError> {
        let name = node["kind"].as_str().unwrap_or("");
        let kind = match name {
            "item_definition" => UsageKind::ItemDefinition,
            "model" => UsageKind::Model,
            "sprite" => UsageKind::Sprite,
            "font" => UsageKind::Font,
            "sound_event" => UsageKind::SoundEvent,
            "equipment" => UsageKind::Equipment,
            "tooltip_style" => UsageKind::TooltipStyle,
            _ => return Err(refused(where_, format!("unknown kind '{name}'"))),
        };
        let atlas = match node.get("atlas") {
            None => None,
            Some(v) if kind == UsageKind::Sprite => Some(
                ResourceId::parse(&cx.interner, v.as_str().unwrap_or(""))
                    .ok_or_else(|| refused(where_, "'atlas' must be an atlas id"))?,
            ),
            _ => return Err(refused(where_, "only a sprite has an atlas")),
        };
        Ok(Self {
            kind,
            atlas,
            id: node["id"].as_str().unwrap_or("{id}").into(),
            namespaced: node["namespaced"].as_bool().unwrap_or(false),
            quiet: node["quiet"].as_bool().unwrap_or(false),
        })
    }

    pub fn apply(
        &self,
        value: &str,
        location: String,
        reason: Option<String>,
        cx: &RunCx,
    ) -> Option<Usage> {
        let text = value.trim();
        if text.is_empty() || self.namespaced && !text.find(':').is_some_and(|p| p > 0) {
            return None;
        }
        let mut written = self.id.clone();
        if ["{id}", "{namespace}", "{path}"]
            .iter()
            .any(|p| written.contains(p))
        {
            let id = ResourceId::parse(&cx.interner, text)?;
            written = written
                .replace("{id}", &id.display(&cx.interner).to_string())
                .replace("{namespace}", cx.interner.resolve(id.ns))
                .replace("{path}", cx.interner.resolve(id.path));
        }
        if written.contains("{value}") {
            if text.contains(['*', '?']) {
                return None;
            }
            written = written.replace("{value}", text);
        }
        ResourceId::parse(&cx.interner, &written.replace(['*', '?'], "x"))?;
        if !written.contains(':') {
            written = format!("minecraft:{written}");
        }
        let mut u = usage(self.kind, &written, location, reason);
        u.atlas = self.atlas;
        u.quiet = self.quiet;
        Some(u)
    }
}

pub(crate) fn unique(usages: Vec<Usage>) -> Vec<Usage> {
    let mut seen = HashSet::new();
    usages
        .into_iter()
        .filter(|u| {
            seen.insert((
                u.kind,
                u.id.clone(),
                u.atlas,
                u.location.clone(),
                u.reason.clone(),
                u.declares.clone(),
                u.quiet,
            ))
        })
        .collect()
}

pub(crate) struct Reference {
    pub rule: Rule,
    pub value: String,
    pub at: String,
}

pub(crate) fn components(node: &Node, at: &str, cx: &RunCx, out: &mut Vec<Reference>) {
    if let NodeValue::List(list) = &*node.value {
        for (i, child) in list.iter().enumerate() {
            components(child, &format!("{at}[{i}]"), cx, out);
        }
    } else if node.mapping() {
        if let Some(font) = node.get("font").and_then(Node::string) {
            out.push(Reference {
                rule: Rule::simple(UsageKind::Font),
                value: font.into(),
                at: format!("{at}.font"),
            });
        }
        if let Some(sprite) = node.get("sprite").and_then(Node::string) {
            let atlas = node
                .get("atlas")
                .and_then(Node::string)
                .unwrap_or(BLOCKS_ATLAS);
            if let Some(atlas) = ResourceId::parse(&cx.interner, atlas) {
                let mut rule = Rule::simple(UsageKind::Sprite);
                rule.atlas = Some(atlas);
                out.push(Reference {
                    rule,
                    value: sprite.into(),
                    at: format!("{at}.sprite"),
                });
            }
        }
        for (key, child) in node.entries() {
            components(child, &format!("{at}.{key}"), cx, out);
        }
    }
}

pub(crate) fn json(text: &str) -> Option<Node> {
    let mut chars = text.chars().peekable();
    let mut normalized = String::new();
    while let Some(c) = chars.next() {
        match c {
            '"' | '\'' => {
                let quote = c;
                normalized.push('"');
                let mut closed = false;
                while let Some(c) = chars.next() {
                    if c == quote {
                        closed = true;
                        break;
                    }
                    if c == '\\' {
                        let next = chars.next()?;
                        if next == '\'' && quote == '\'' {
                            normalized.push('\'');
                        } else {
                            normalized.push('\\');
                            normalized.push(next);
                        }
                    } else {
                        if c == '"' {
                            normalized.push('\\');
                        }
                        normalized.push(c);
                    }
                }
                if !closed {
                    return None;
                }
                normalized.push('"');
            }
            '/' if chars.peek() == Some(&'/') => {
                chars.next();
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
                normalized.push('\n');
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut closed = false;
                while let Some(c) = chars.next() {
                    if c == '*' && chars.peek() == Some(&'/') {
                        chars.next();
                        closed = true;
                        break;
                    }
                }
                if !closed {
                    return None;
                }
                normalized.push(' ');
            }
            ']' | '}' => {
                let end = normalized.trim_end().len();
                if normalized[..end].ends_with(',')
                    && !normalized[..end - 1].trim_end().ends_with(['[', '{'])
                {
                    normalized.truncate(end - 1);
                }
                normalized.push(c);
            }
            _ => normalized.push(c),
        }
    }
    serde_json::from_str::<Value>(&normalized).ok()?;
    document::json(&normalized)
}
