//! `report.json` and `graph.json` from one analysis.

use std::collections::{BTreeMap, BTreeSet};

use packprune_graph::{Reached, Status, UsageRef as GraphUsage};
use packprune_pack::meta::show_format;
use packprune_pack::{ClientRules, FileKind, Layer, Layout, VanillaIndex, ViewSet};

use super::model::{
    AtlasFileRef, Edge, FileRef, GraphFile, Input, Inputs, Invalid, KeptItem, KindSummary, Missing,
    Node, NotStitched, OtherFormats, Report, SCHEMA, SharedFile, SourceInfo, SourceInput,
    SourceRoot, Summary, Tool, UnusedFolder, UnusedItem, UnusedSelection, UsageRef, ViewInfo,
};

/// The `report.json` and `graph.json` of one analysis.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Analysis {
    pub report: Report,
    pub graph: GraphFile,
}

/// What a source read, in the order the sources were given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceFacts {
    pub id: String,
    pub inputs: Vec<SourceInput>,
}

/// Everything an analysis is reported from. The hashes are the caller's: they come from the files on disk, which
/// the analysis does not keep.
pub struct Facts<'a> {
    pub pack: Input,
    pub client_jar: Input,
    pub asset_index: Option<Input>,
    pub vanilla: &'a VanillaIndex,
    pub rules: &'a ClientRules,
    pub layout: &'a Layout,
    pub reached: &'a Reached,
    /// One per usage source of the run, as `Reached::sources` lists them.
    pub sources: &'a [SourceFacts],
}

/// The source ids that read data packs or item components: without one, textures only those name look unused.
const SERVER_DATA_SOURCES: [&str; 3] = ["configs", "datapacks", "nexo"];

pub fn analysis(facts: &Facts<'_>) -> Analysis {
    let Facts {
        layout,
        reached,
        vanilla,
        ..
    } = *facts;
    let formats = |views: &ViewSet| -> BTreeSet<String> {
        views
            .iter()
            .filter_map(|id| layout.views.get(id.0 as usize))
            .map(|view| show_format(view.formats.min).to_string())
            .collect()
    };
    let verified = |views: &ViewSet| {
        views
            .iter()
            .filter_map(|id| layout.views.get(id.0 as usize))
            .any(|view| view.verified)
    };
    // Findings in a verified view set the exit code; those only views for other formats have are listed apart.
    let (missing, other_missing): (Vec<_>, Vec<_>) = reached
        .missing
        .iter()
        .partition(|finding| verified(&finding.views));
    let missing: Vec<Missing> = missing
        .into_iter()
        .map(|finding| missing_of(finding, &formats))
        .collect();
    let other_missing: Vec<Missing> = other_missing
        .into_iter()
        .map(|finding| missing_of(finding, &formats))
        .collect();
    let (not_stitched, other_stitched): (Vec<_>, Vec<_>) = reached
        .not_stitched
        .iter()
        .partition(|finding| verified(&finding.views));
    let not_stitched: Vec<NotStitched> = not_stitched
        .into_iter()
        .map(|finding| not_stitched_of(finding, &formats))
        .collect();
    let other_stitched: Vec<NotStitched> = other_stitched
        .into_iter()
        .map(|finding| not_stitched_of(finding, &formats))
        .collect();

    let with_status = |status: Status| {
        reached
            .files
            .iter()
            .filter(move |(_, file)| file.status == status)
    };
    let file_ref =
        |(path, file): (&packprune_pack::PhysPath, &packprune_graph::FileEntry)| FileRef {
            path: path.to_string(),
            kind: file.kind.id().to_owned(),
            bytes: file.bytes,
        };
    let orphans: Vec<FileRef> = with_status(Status::Orphan).map(file_ref).collect();
    let dead: Vec<FileRef> = with_status(Status::DeadBlockstate).map(file_ref).collect();
    let unknown: Vec<FileRef> = with_status(Status::Unknown).map(file_ref).collect();
    let atlas_loaded: Vec<AtlasFileRef> = with_status(Status::AtlasLoadedOrphan)
        .map(|(path, file)| AtlasFileRef {
            path: path.to_string(),
            kind: file.kind.id().to_owned(),
            bytes: file.bytes,
            atlases: file.atlases.clone(),
        })
        .collect();
    let unused_files: Vec<_> = with_status(Status::Unused).collect();
    let invalid: Vec<Invalid> = reached
        .invalid
        .iter()
        .map(|(path, error)| Invalid {
            path: path.to_string(),
            error: error.clone(),
        })
        .collect();
    let sources = sources(facts);
    let unmatched_usages = sources.iter().map(|source| source.unmatched.len()).sum();

    let summary = Summary {
        files: reached.files.len(),
        bytes: reached.files.values().map(|file| file.bytes).sum(),
        reached: with_status(Status::Reached).count(),
        orphans: orphans.len(),
        orphan_bytes: orphans.iter().map(|file| file.bytes).sum(),
        atlas_loaded_orphans: atlas_loaded.len(),
        atlas_loaded_orphan_bytes: atlas_loaded.iter().map(|file| file.bytes).sum(),
        unused: unused_files.len(),
        unused_bytes: unused_files.iter().map(|(_, file)| file.bytes).sum(),
        dead_blockstates: dead.len(),
        missing: missing.len(),
        not_stitched: not_stitched.len(),
        other_formats_missing: other_missing.len(),
        other_formats_not_stitched: other_stitched.len(),
        unknown: unknown.len(),
        invalid: invalid.len(),
        unmatched_usages,
    };
    let report = Report {
        schema: SCHEMA,
        tool: Tool {
            name: "packprune".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
        inputs: Inputs {
            pack: facts.pack.clone(),
            client_jar: facts.client_jar.clone(),
            client_version: vanilla.version.clone(),
            client_rules: facts.rules.version.clone(),
            asset_index: facts.asset_index.clone().unwrap_or_else(|| Input {
                name: "none".into(),
                sha256: String::new(),
            }),
        },
        views: layout
            .views
            .iter()
            .map(|view| ViewInfo {
                id: view.name.clone(),
                format: show_format(view.formats.min).to_string(),
                overlays: view
                    .layers
                    .iter()
                    .filter_map(|layer| match layer {
                        Layer::Root => None,
                        Layer::Overlay(directory) => Some(directory.to_string()),
                    })
                    .collect(),
                verified: view.verified,
            })
            .collect(),
        summary,
        by_kind: by_kind(reached),
        orphans,
        atlas_loaded_orphans: atlas_loaded,
        dead_blockstates: dead,
        removable: reached
            .removable()
            .iter()
            .map(ToString::to_string)
            .collect(),
        missing,
        not_stitched,
        other_formats: OtherFormats {
            missing: other_missing,
            not_stitched: other_stitched,
        },
        unknown,
        invalid,
        sources,
        source_roots: reached
            .source_roots
            .iter()
            .map(|root| SourceRoot {
                path: root.path.to_string(),
                usages: root.usages.iter().map(usage).collect(),
            })
            .collect(),
        unused_items: reached
            .unused_items
            .iter()
            .map(|selection| UnusedSelection {
                selector: selection.selector.clone(),
                searched: selection.searched.clone(),
                folders: selection
                    .folders
                    .iter()
                    .map(|folder| UnusedFolder {
                        folder: folder.folder.clone(),
                        items: folder.items,
                        files: folder.files,
                        bytes: folder.bytes,
                    })
                    .collect(),
                unused: selection
                    .unused
                    .iter()
                    .map(|item| UnusedItem {
                        declaration: item.declaration.clone(),
                        definitions: item.definitions.iter().map(ToString::to_string).collect(),
                        files: item.files.iter().map(ToString::to_string).collect(),
                        bytes: item.bytes,
                    })
                    .collect(),
                kept: selection
                    .kept
                    .iter()
                    .map(|item| KeptItem {
                        declaration: item.declaration.clone(),
                        definitions: item.definitions.iter().map(ToString::to_string).collect(),
                        used_by: item.used_by.iter().map(usage).collect(),
                    })
                    .collect(),
                shared: selection
                    .shared
                    .iter()
                    .map(|file| SharedFile {
                        path: file.path.to_string(),
                        bytes: file.bytes,
                        declarations: file.declarations.clone(),
                    })
                    .collect(),
            })
            .collect(),
        warnings: warnings(facts),
    };
    let graph = GraphFile {
        schema: SCHEMA,
        nodes: reached
            .files
            .iter()
            .map(|(path, file)| Node {
                path: path.to_string(),
                kind: file.kind.id().to_owned(),
                bytes: file.bytes,
                status: file.status.id().to_owned(),
                root: file.root.clone().unwrap_or_default(),
                atlases: file.atlases.clone(),
            })
            .collect(),
        edges: reached
            .edges
            .iter()
            .map(|edge| Edge {
                from: edge.from.to_string(),
                to: edge.to.to_string(),
                reason: edge.reason.clone(),
            })
            .collect(),
    };
    Analysis { report, graph }
}

fn missing_of(
    finding: &packprune_graph::Missing,
    formats: &dyn Fn(&ViewSet) -> BTreeSet<String>,
) -> Missing {
    Missing {
        source: finding.source.to_string(),
        target: finding.target.clone(),
        reason: finding.reason.clone(),
        views: formats(&finding.views),
    }
}

fn not_stitched_of(
    finding: &packprune_graph::NotStitched,
    formats: &dyn Fn(&ViewSet) -> BTreeSet<String>,
) -> NotStitched {
    NotStitched {
        source: finding.source.to_string(),
        sprite: finding.sprite.clone(),
        atlases: finding.atlases.clone(),
        file: finding.file.clone(),
        views: formats(&finding.views),
    }
}

fn usage(usage: &GraphUsage) -> UsageRef {
    UsageRef {
        source: usage.source.clone(),
        location: usage.location.clone(),
        reason: usage.reason.clone(),
        kind: usage.kind.clone(),
        id: usage.id.clone(),
        atlas: usage.atlas.clone(),
    }
}

fn sources(facts: &Facts<'_>) -> Vec<SourceInfo> {
    facts
        .reached
        .sources
        .iter()
        .enumerate()
        .map(|(index, source)| SourceInfo {
            id: source.id.clone(),
            inputs: facts
                .sources
                .get(index)
                .filter(|read| read.id == source.id)
                .map(|read| read.inputs.clone())
                .unwrap_or_default(),
            usages: source.usages,
            matched: source.matched,
            vanilla: source.vanilla,
            unmatched: source.unmatched.iter().map(usage).collect(),
        })
        .collect()
}

/// A file counts as reached when it is `Reached` or `Unknown` (kept and listed).
fn by_kind(reached: &Reached) -> Vec<KindSummary> {
    let mut counts: BTreeMap<FileKind, [u64; 4]> = BTreeMap::new();
    for file in reached.files.values() {
        let count = counts.entry(file.kind).or_default();
        count[0] += 1;
        count[1] += file.bytes;
        if matches!(file.status, Status::Reached | Status::Unknown) {
            count[2] += 1;
        } else {
            count[3] += file.bytes;
        }
    }
    counts
        .into_iter()
        .map(
            |(kind, [files, bytes, reached, unreached_bytes])| KindSummary {
                kind: kind.id().to_owned(),
                files: files as usize,
                bytes,
                reached: reached as usize,
                unreached: (files - reached) as usize,
                unreached_bytes,
            },
        )
        .collect()
}

/// The graph's notes, with one more when no source reads server data, inserted after the sound-index notes and before
/// those on views.
fn warnings(facts: &Facts<'_>) -> Vec<String> {
    let mut notes = facts.reached.warnings.clone();
    if facts
        .reached
        .sources
        .iter()
        .any(|source| SERVER_DATA_SOURCES.contains(&source.id.as_str()))
    {
        return notes;
    }
    let readers = SERVER_DATA_SOURCES.join(", ");
    let note = format!(
        "no source reads data packs or item components: textures only they name (entity variants, camera overlays, \
         trims, banners) look like orphans here; keep-list them or add --source <id>=<path> with {readers}"
    );
    let at = notes
        .iter()
        .position(|note| {
            note.starts_with("views at formats ") || note.starts_with("the pack declares no view")
        })
        .unwrap_or(notes.len());
    notes.insert(at, note);
    notes
}
