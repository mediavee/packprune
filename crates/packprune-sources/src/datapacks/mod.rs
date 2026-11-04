use camino::{Utf8Path, Utf8PathBuf};
use packprune_graph::{SourceError, Usage, UsageSource};
use packprune_pack::{Digest, Limits, PackInput, RunCx};
use serde_json::Value;

use crate::{
    document::{Node, Value as NodeValue},
    refused,
    rules::{self, Rule},
    source,
};

pub fn open(path: &Utf8Path, cx: &RunCx) -> Result<Box<dyn UsageSource>, SourceError> {
    Ok(source(path, cx, collect, digest))
}

fn is_pack(path: &Utf8Path) -> bool {
    path.join("data").is_dir() || path.join("pack.mcmeta").is_file()
}

fn packs(path: &Utf8Path) -> Result<Vec<Utf8PathBuf>, SourceError> {
    if !path.is_dir() || is_pack(path) {
        return Ok(vec![path.into()]);
    }
    let mut out = Vec::new();
    for child in std::fs::read_dir(path)? {
        let p = Utf8PathBuf::from_path_buf(child?.path())
            .map_err(|p| refused(p.display(), "not a UTF-8 path"))?;
        if if p.is_dir() {
            is_pack(&p)
        } else {
            p.extension().is_some_and(|s| s.eq_ignore_ascii_case("zip"))
        } {
            out.push(p);
        }
    }
    out.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    Ok(out)
}

fn registry(path: &str) -> Option<&str> {
    let parts: Vec<_> = path.split('/').collect();
    let data = if parts.first() == Some(&"data") {
        0
    } else if parts.get(1) == Some(&"data") {
        1
    } else {
        return None;
    };
    (path.ends_with(".json") && parts.len() >= data + 4).then(|| parts[data + 2])
}

fn collect(path: &Utf8Path, cx: &RunCx) -> Result<Vec<Usage>, SourceError> {
    let rules = load_rules()?;
    let mut out = Vec::new();
    for pack in packs(path)? {
        let data = PackInput::open(&pack, &Limits::default()).map_err(|e| refused(&pack, e))?;
        for (file, entry) in data.files {
            let Some(registry) = registry(file.as_str()) else {
                continue;
            };
            let (text, fields) = selection(&rules, registry);
            if !text && fields.is_none() {
                continue;
            }
            let where_ = format!(
                "{}!{}",
                pack.file_name().unwrap_or(pack.as_str()),
                file.as_str()
            );
            let node = std::str::from_utf8(&entry.blob)
                .ok()
                .and_then(rules::json)
                .ok_or_else(|| refused(&where_, "not valid JSON"))?;
            if text {
                let mut refs = Vec::new();
                rules::components(&node, "", cx, &mut refs);
                for r in refs {
                    if let Some(u) = r.rule.apply(
                        &r.value,
                        format!("{where_} {}", r.at.strip_prefix('.').unwrap_or(&r.at)),
                        None,
                        cx,
                    ) {
                        out.push(u);
                    }
                }
            }
            for field in fields.into_iter().flatten() {
                let rule = Rule::parse(field, "datapack-rules.json", cx)?;
                let path: Vec<_> = field["path"].as_str().unwrap_or("").split('/').collect();
                let mut found = Vec::new();
                values(&node, &path, "", &mut found);
                for (value, trail) in found {
                    if let Some(u) = rule.apply(value, format!("{where_} {trail}"), None, cx) {
                        out.push(u);
                    }
                }
            }
        }
    }
    Ok(rules::unique(out))
}

fn values<'a>(node: &'a Node, path: &[&str], trail: &str, out: &mut Vec<(&'a str, String)>) {
    let Some((step, remaining)) = path.split_first() else {
        if let Some(text) = node.string() {
            out.push((text, trail.into()));
        } else if node.mapping()
            && let Some(text) = node.get("sound_id").and_then(Node::string)
        {
            out.push((text, format!("{trail}.sound_id")));
        }
        return;
    };
    let join = |key: &str| {
        if trail.is_empty() {
            key.to_owned()
        } else {
            format!("{trail}.{key}")
        }
    };
    if *step == "*" {
        match &*node.value {
            NodeValue::List(list) => {
                for (i, child) in list.iter().enumerate() {
                    values(child, remaining, &format!("{trail}[{i}]"), out);
                }
            }
            NodeValue::Map(entries) => {
                for (key, child) in entries {
                    values(child, remaining, &join(key), out);
                }
            }
            _ => {}
        }
    } else if let Some(child) = node
        .entries()
        .iter()
        .find(|(k, _)| k == step)
        .map(|(_, v)| v)
    {
        values(child, remaining, &join(step), out);
    }
}

fn load_rules() -> Result<Value, SourceError> {
    let rules = serde_json::from_str(include_str!("../../data/datapack-rules.json"))
        .map_err(|e| refused("datapack-rules.json", e))?;
    Ok(rules)
}

fn selection<'a>(rules: &'a Value, registry: &str) -> (bool, Option<&'a Vec<Value>>) {
    (
        rules["text_registries"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(registry))),
        rules["registries"][registry].as_array(),
    )
}

fn digest(path: &Utf8Path, _cx: &RunCx) -> Result<Digest, SourceError> {
    let rules = load_rules()?;
    let mut hash = blake3::Hasher::new();
    for pack in packs(path)? {
        let data = PackInput::open(&pack, &Limits::default()).map_err(|e| refused(&pack, e))?;
        // Archive metadata and unselected registries cannot affect collected usages.
        for (file, entry) in data.files {
            if registry(file.as_str()).is_some_and(|r| {
                let (text, fields) = selection(&rules, r);
                text || fields.is_some()
            }) {
                crate::hash_part(
                    &mut hash,
                    pack.file_name().unwrap_or(pack.as_str()).as_bytes(),
                );
                crate::hash_part(&mut hash, file.as_str().as_bytes());
                crate::hash_part(&mut hash, blake3::hash(&entry.blob).as_bytes());
            }
        }
    }
    Ok(Digest(*hash.finalize().as_bytes()))
}
