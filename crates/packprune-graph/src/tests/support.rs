//! Running the analysis as `report` does, and the parts of its `report.json` the graph decides, so a result compares
//! with a fixture's golden report.

use std::collections::BTreeSet;

use camino::{Utf8Path, Utf8PathBuf};
use packprune_pack::meta::show_format;
use packprune_pack::{
    ClientRules, Layout, Limits, PackInput, PackMeta, ResourceId, RunCx, VanillaIndex, ViewSet,
};
use serde_json::{Value, json};

use crate::{
    Computed, IdGlob, Inputs, Policy, Reached, ReferenceIndex, SourceUsages, Status,
    UnusedSelector, Usage, UsageKind, UsageRef,
};

pub(crate) fn repo() -> Utf8PathBuf {
    Utf8Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The fixtures' client, read from the `tests/fixtures/vanilla/` tree as it would be from a jar.
pub(crate) fn fixture_vanilla() -> VanillaIndex {
    let root = repo().join("tests/fixtures/vanilla");
    let version: Value =
        serde_json::from_slice(&std::fs::read(root.join("version.json")).unwrap()).unwrap();
    let pack = &version["pack_version"];
    let mut index = VanillaIndex {
        version: version["id"].as_str().unwrap().into(),
        pack_format: format!("{}.{}", pack["resource_major"], pack["resource_minor"]),
        ..VanillaIndex::default()
    };
    let mut stack = vec![root.join("assets")];
    while let Some(dir) = stack.pop() {
        for entry in dir.read_dir_utf8().unwrap() {
            let path = entry.unwrap().into_path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = path.strip_prefix(&root).unwrap().as_str().to_owned();
            if name.starts_with("assets/minecraft/atlases/") && name.ends_with(".json") {
                index
                    .atlases
                    .insert(name.clone(), std::fs::read(&path).unwrap());
            }
            index.names.insert(name);
        }
    }
    index
}

/// One usage per line: `<kind> <id> [atlas]` (`lines`), or
/// `<declaration> <kind> <id> [atlas]` (`generator`).
pub(crate) fn line_source(cx: &RunCx, id: &str, file: &Utf8Path, declaring: bool) -> SourceUsages {
    let text = std::fs::read_to_string(file).unwrap();
    let name = file.file_name().unwrap();
    let usages = text
        .lines()
        .enumerate()
        .map(|(index, line)| {
            let mut parts: Vec<&str> = line.split(' ').collect();
            let declares = declaring.then(|| parts.remove(0).to_owned());
            Usage {
                kind: kind(parts[0]),
                id: IdGlob(parts[1].into()),
                atlas: parts
                    .get(2)
                    .map(|atlas| ResourceId::parse(&cx.interner, atlas).unwrap()),
                location: format!("{name}:{}", index + 1),
                reason: None,
                declares,
                quiet: false,
            }
        })
        .collect();
    SourceUsages {
        source: id.into(),
        path: file.to_owned(),
        usages,
    }
}

pub(crate) fn kind(text: &str) -> UsageKind {
    match text {
        "item_definition" => UsageKind::ItemDefinition,
        "model" => UsageKind::Model,
        "sprite" => UsageKind::Sprite,
        "font" => UsageKind::Font,
        "sound_event" => UsageKind::SoundEvent,
        "equipment" => UsageKind::Equipment,
        "tooltip_style" => UsageKind::TooltipStyle,
        "path" => UsageKind::Path,
        "namespace" => UsageKind::Namespace,
        other => panic!("no usage kind {other}"),
    }
}

pub(crate) struct Run {
    pub layout: Layout,
    pub index: ReferenceIndex,
    pub reached: Reached,
}

pub(crate) fn analyse(
    cx: &RunCx,
    pack: &PackInput,
    vanilla: &VanillaIndex,
    sources: &[SourceUsages],
    selectors: &[UnusedSelector],
) -> Run {
    let layout = Layout::new(pack, PackMeta::read(pack).unwrap(), vanilla);
    let rules = ClientRules::for_version(&vanilla.version);
    let index = ReferenceIndex::build(cx, pack, &layout, &rules, &Computed);
    let reached = Reached::analyse(&Inputs {
        cx,
        pack,
        layout: &layout,
        rules: &rules,
        vanilla,
        index: &index,
        sources,
        selectors,
        policy: Policy::Pack,
    })
    .unwrap();
    Run {
        layout,
        index,
        reached,
    }
}

pub(crate) fn open(path: &Utf8Path) -> PackInput {
    PackInput::open(path, &Limits::default()).unwrap()
}

fn views(layout: &Layout, views: &ViewSet) -> Vec<String> {
    let formats: BTreeSet<String> = views
        .iter()
        .map(|view| show_format(layout.views[view.0 as usize].formats.min).to_string())
        .collect();
    formats.into_iter().collect()
}

fn verified(layout: &Layout, set: &ViewSet) -> bool {
    set.iter()
        .any(|view| layout.views[view.0 as usize].verified)
}

fn usage(reference: &UsageRef) -> Value {
    json!({"source": reference.source, "location": reference.location, "reason": reference.reason,
        "kind": reference.kind, "id": reference.id, "atlas": reference.atlas})
}

/// The sections of `report.json` the graph decides; `invalid` and `sources` without what other layers add
/// (Jackson's messages, the inputs' hashes).
pub(crate) fn report(run: &Run) -> Value {
    let (layout, reached) = (&run.layout, &run.reached);
    let files = |status: Status, atlases: bool| -> Value {
        reached
            .files
            .iter()
            .filter(|(_, file)| file.status == status)
            .map(|(path, file)| {
                let mut entry =
                    json!({"path": path.as_str(), "kind": file.kind.id(), "bytes": file.bytes});
                if atlases {
                    entry["atlases"] = json!(file.atlases);
                }
                entry
            })
            .collect()
    };
    let missing = |want: bool| -> Value {
        reached
            .missing
            .iter()
            .filter(|missing| verified(layout, &missing.views) == want)
            .map(|missing| {
                json!({"source": missing.source.as_str(), "target": missing.target,
                "reason": missing.reason, "views": views(layout, &missing.views)})
            })
            .collect()
    };
    let not_stitched = |want: bool| -> Value {
        reached
            .not_stitched
            .iter()
            .filter(|found| verified(layout, &found.views) == want)
            .map(|found| json!({"source": found.source.as_str(), "sprite": found.sprite, "atlases": found.atlases,
                "file": found.file, "views": views(layout, &found.views)}))
            .collect()
    };
    json!({
        "orphans": files(Status::Orphan, false),
        "atlasLoadedOrphans": files(Status::AtlasLoadedOrphan, true),
        "deadBlockstates": files(Status::DeadBlockstate, false),
        "removable": reached.removable().iter().map(|path| path.as_str()).collect::<Vec<_>>(),
        "missing": missing(true),
        "notStitched": not_stitched(true),
        "otherFormats": {"missing": missing(false), "notStitched": not_stitched(false)},
        "unknown": files(Status::Unknown, false),
        "invalid": reached.invalid.keys().map(|path| path.as_str()).collect::<Vec<_>>(),
        "sources": reached.sources.iter().map(|source| json!({"id": source.id, "usages": source.usages,
            "matched": source.matched, "vanilla": source.vanilla,
            "unmatched": source.unmatched.iter().map(usage).collect::<Vec<_>>()})).collect::<Vec<_>>(),
        "sourceRoots": reached.source_roots.iter().map(|root| json!({"path": root.path.as_str(),
            "usages": root.usages.iter().map(usage).collect::<Vec<_>>()})).collect::<Vec<_>>(),
        "unusedItems": reached.unused_items.iter().map(|selection| json!({
            "selector": selection.selector, "searched": selection.searched,
            "folders": selection.folders.iter().map(|folder| json!({"folder": folder.folder, "items": folder.items,
                "files": folder.files, "bytes": folder.bytes})).collect::<Vec<_>>(),
            "unused": selection.unused.iter().map(|item| json!({"declaration": item.declaration,
                "definitions": item.definitions.iter().map(|path| path.as_str()).collect::<Vec<_>>(),
                "files": item.files.iter().map(|path| path.as_str()).collect::<Vec<_>>(),
                "bytes": item.bytes})).collect::<Vec<_>>(),
            "kept": selection.kept.iter().map(|item| json!({"declaration": item.declaration,
                "definitions": item.definitions.iter().map(|path| path.as_str()).collect::<Vec<_>>(),
                "usedBy": item.used_by.iter().map(usage).collect::<Vec<_>>()})).collect::<Vec<_>>(),
            "shared": selection.shared.iter().map(|file| json!({"path": file.path.as_str(), "bytes": file.bytes,
                "declarations": file.declarations})).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

/// A golden `report.json`, cut to [`report`]'s sections.
pub(crate) fn golden_report(report: &Value) -> Value {
    let mut cut = json!({});
    for key in [
        "orphans",
        "atlasLoadedOrphans",
        "deadBlockstates",
        "removable",
        "missing",
        "notStitched",
        "otherFormats",
        "unknown",
        "sourceRoots",
        "unusedItems",
    ] {
        cut[key] = report[key].clone();
    }
    cut["invalid"] = report["invalid"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["path"].clone())
        .collect();
    cut["sources"] = report["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|source| {
            let mut source = source.clone();
            source.as_object_mut().unwrap().remove("inputs");
            source
        })
        .collect();
    cut
}

/// The first differences between two JSON values, by path, at most `limit`.
pub(crate) fn differences(
    at: &str,
    ours: &Value,
    theirs: &Value,
    out: &mut Vec<String>,
    limit: usize,
) {
    if out.len() >= limit || ours == theirs {
        return;
    }
    match (ours, theirs) {
        (Value::Object(a), Value::Object(b)) => {
            let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
            for key in keys {
                let missing = Value::Null;
                differences(
                    &format!("{at}/{key}"),
                    a.get(key).unwrap_or(&missing),
                    b.get(key).unwrap_or(&missing),
                    out,
                    limit,
                );
            }
        }
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
            for (index, (x, y)) in a.iter().zip(b).enumerate() {
                differences(&format!("{at}/{index}"), x, y, out, limit);
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            let shown = |items: &[Value]| -> BTreeSet<String> {
                items.iter().map(Value::to_string).collect()
            };
            let (a, b) = (shown(a), shown(b));
            for only in a.difference(&b).take(limit) {
                out.push(format!("{at}: only ours: {only}"));
            }
            for only in b.difference(&a).take(limit) {
                out.push(format!("{at}: only golden: {only}"));
            }
        }
        _ => out.push(format!("{at}: ours {ours}, golden {theirs}")),
    }
}
