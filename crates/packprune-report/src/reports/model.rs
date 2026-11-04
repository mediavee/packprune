//! The shapes of `report.json` and `graph.json`.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

pub const SCHEMA: u32 = 4;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub schema: u32,
    pub tool: Tool,
    pub inputs: Inputs,
    pub views: Vec<ViewInfo>,
    pub summary: Summary,
    pub by_kind: Vec<KindSummary>,
    pub orphans: Vec<FileRef>,
    pub atlas_loaded_orphans: Vec<AtlasFileRef>,
    pub dead_blockstates: Vec<FileRef>,
    pub removable: Vec<String>,
    pub missing: Vec<Missing>,
    pub not_stitched: Vec<NotStitched>,
    pub other_formats: OtherFormats,
    pub unknown: Vec<FileRef>,
    pub invalid: Vec<Invalid>,
    pub sources: Vec<SourceInfo>,
    pub source_roots: Vec<SourceRoot>,
    pub unused_items: Vec<UnusedSelection>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Inputs {
    pub pack: Input,
    pub client_jar: Input,
    pub client_version: String,
    pub client_rules: String,
    pub asset_index: Input,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Input {
    pub name: String,
    pub sha256: String,
}

/// A file a usage source reads. The digest is the source's own (blake3 over only what it reads), not a SHA-256.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceInput {
    pub name: String,
    pub blake3: String,
}

/// `verified` when the view is the one the client jar's own format selects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewInfo {
    pub id: String,
    pub format: String,
    pub overlays: Vec<String>,
    pub verified: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub files: usize,
    pub bytes: u64,
    pub reached: usize,
    pub orphans: usize,
    pub orphan_bytes: u64,
    pub atlas_loaded_orphans: usize,
    pub atlas_loaded_orphan_bytes: u64,
    pub unused: usize,
    pub unused_bytes: u64,
    pub dead_blockstates: usize,
    pub missing: usize,
    pub not_stitched: usize,
    pub other_formats_missing: usize,
    pub other_formats_not_stitched: usize,
    pub unknown: usize,
    pub invalid: usize,
    pub unmatched_usages: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KindSummary {
    pub kind: String,
    pub files: usize,
    pub bytes: u64,
    pub reached: usize,
    pub unreached: usize,
    pub unreached_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRef {
    pub path: String,
    pub kind: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtlasFileRef {
    pub path: String,
    pub kind: String,
    pub bytes: u64,
    pub atlases: BTreeSet<String>,
}

/// `views` are the formats of the views the reference is broken in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Missing {
    pub source: String,
    pub target: String,
    pub reason: String,
    pub views: BTreeSet<String>,
}

/// A sprite no atlas stitches although `file` exists: the client draws the missing texture.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotStitched {
    pub source: String,
    pub sprite: String,
    pub atlases: String,
    pub file: String,
    pub views: BTreeSet<String>,
}

/// Findings only views for other formats than the client jar's have: listed, not counted for the exit code.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OtherFormats {
    pub missing: Vec<Missing>,
    pub not_stitched: Vec<NotStitched>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invalid {
    pub path: String,
    pub error: String,
}

/// A usage and the source that named it; empty strings for a reason or an atlas it does not have.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageRef {
    pub source: String,
    pub location: String,
    pub reason: String,
    pub kind: String,
    pub id: String,
    pub atlas: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceInfo {
    pub id: String,
    pub inputs: Vec<SourceInput>,
    pub usages: usize,
    pub matched: usize,
    pub vanilla: usize,
    pub unmatched: Vec<UsageRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRoot {
    pub path: String,
    pub usages: Vec<UsageRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnusedSelection {
    pub selector: String,
    pub searched: Vec<String>,
    pub folders: Vec<UnusedFolder>,
    pub unused: Vec<UnusedItem>,
    pub kept: Vec<KeptItem>,
    pub shared: Vec<SharedFile>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnusedFolder {
    pub folder: String,
    pub items: usize,
    pub files: usize,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnusedItem {
    pub declaration: String,
    pub definitions: Vec<String>,
    pub files: Vec<String>,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeptItem {
    pub declaration: String,
    pub definitions: Vec<String>,
    pub used_by: Vec<UsageRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedFile {
    pub path: String,
    pub bytes: u64,
    pub declarations: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphFile {
    pub schema: u32,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

/// `root` is empty when only a reference reaches the file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub path: String,
    pub kind: String,
    pub bytes: u64,
    pub status: String,
    pub root: String,
    pub atlases: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub reason: String,
}
