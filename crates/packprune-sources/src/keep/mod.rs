use camino::Utf8Path;
use packprune_graph::{SourceError, Usage, UsageKind, UsageSource};
use packprune_pack::{Digest, RunCx};

use crate::{
    document::{self, Node, Value},
    refused,
    rules::usage,
    source,
};

pub fn open(path: &Utf8Path, cx: &RunCx) -> Result<Box<dyn UsageSource>, SourceError> {
    Ok(source(path, cx, collect, digest))
}

fn collect(path: &Utf8Path, _cx: &RunCx) -> Result<Vec<Usage>, SourceError> {
    collect_bytes(path, &std::fs::read(path)?)
}

/// Collects keep-list usages from an in-memory document for parser fuzzing.
pub fn yaml_usages(bytes: &[u8], _cx: &RunCx) -> Result<Vec<Usage>, SourceError> {
    collect_bytes(Utf8Path::new("input.yml"), bytes)
}

fn collect_bytes(path: &Utf8Path, bytes: &[u8]) -> Result<Vec<Usage>, SourceError> {
    let docs = document::parse(path.as_str(), bytes, false)?;
    let root = docs
        .first()
        .filter(|_| docs.len() == 1)
        .ok_or_else(|| refused(path, "expected a mapping with version: 1"))?;
    if !root.mapping()
        || !root
            .get("version")
            .is_some_and(|n| matches!(&*n.value, Value::Scalar(s, false) if s == "1"))
    {
        return Err(refused(path, "expected a mapping with version: 1"));
    }
    let Some(Value::List(entries)) = root.get("keep").map(|n| &*n.value) else {
        return Err(refused(path, "expected a keep: list"));
    };
    entries
        .iter()
        .enumerate()
        .map(|(i, node)| entry(path, i, node))
        .collect()
}

fn entry(path: &Utf8Path, index: usize, node: &Node) -> Result<Usage, SourceError> {
    let where_ = format!("{path}: keep entry {}", index + 1);
    if !node.mapping() {
        return Err(refused(&where_, "expected a mapping"));
    }
    let why = node
        .get("why")
        .and_then(Node::string)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| refused(&where_, "every entry needs a why"))?;
    let selectors: Vec<_> = node.entries().iter().filter(|(k, _)| k != "why").collect();
    let [(selector, value)] = selectors[..] else {
        return Err(refused(&where_, SELECTORS));
    };
    let kind = match selector.as_str() {
        "path" => UsageKind::Path,
        "namespace" => UsageKind::Namespace,
        "item_model" => UsageKind::ItemDefinition,
        "model" => UsageKind::Model,
        "texture" => UsageKind::Sprite,
        "font" => UsageKind::Font,
        "equipment" => UsageKind::Equipment,
        "sound_event" => UsageKind::SoundEvent,
        _ => return Err(refused(&where_, SELECTORS)),
    };
    let pattern = value
        .string()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| refused(&where_, "selector must be a non-empty string"))?;
    let file = path.file_name().unwrap_or(path.as_str());
    Ok(usage(
        kind,
        pattern,
        format!("{file}:{}", node.line),
        Some(why.to_owned()),
    ))
}

const SELECTORS: &str = "expected exactly one of [path, namespace, item_model, model, texture, font, equipment, sound_event]";

fn digest(path: &Utf8Path, _cx: &RunCx) -> Result<Digest, SourceError> {
    Ok(Digest(*blake3::hash(&std::fs::read(path)?).as_bytes()))
}
