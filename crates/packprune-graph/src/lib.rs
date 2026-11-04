//! Reference tokens and attachment, the graph per view, the usage model and roots, the prune plan.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::ops::Range;
use std::sync::Arc;

use camino::{Utf8Path, Utf8PathBuf};
use packprune_pack::{
    ClientRules, Digest, Entry, FileKind, Layout, LogicalPath, PackInput, PhysPath, Pointer,
    ResourceId, RunCx, VanillaIndex, ViewSet,
};

mod analysis;
pub mod atlases;
mod facts;
mod glsl;
mod index;
mod json;
mod token_memo;
mod tokens;
mod unused;
mod usages;
mod walk;

#[cfg(test)]
mod tests;

/// What a reference-bearing token is to the client.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    ModelParent,
    TextureSlot,
    Sprite,
    SoundName,
    ShaderImportQuoted,
    ShaderImportAngled,
    Other,
}

/// A token as the file's bytes say it, unresolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawRef {
    pub pointer: Pointer,
    pub span: Range<u32>,
    pub raw: Box<str>,
    pub role: Role,
}

/// A file the tokenizer cannot read, listed invalid and kept as it is.
#[derive(Debug, thiserror::Error)]
#[error("invalid file: {0}")]
pub struct Invalid(pub String);

/// A pure function of the bytes and the kind, so content-keyed and cacheable.
///
/// The tokens are every string the walk reads, in its order, with their JSON pointer and byte span. Besides
/// references, [`Role::Other`] tokens carry what the attachment needs to tell what a reference names: the `type` of a
/// typed object, the events a `sounds.json` defines, and a marker for a container that is an object where the index
/// only reads arrays. `#moj_import` directives are read from any file that has one, since a shader may import any file.
pub fn tokenize(bytes: &[u8], kind: FileKind) -> Result<Vec<RawRef>, Invalid> {
    tokens::tokenize(bytes, kind)
}

/// The tokens of one file, or why it is invalid.
pub type Tokens = Arc<Result<Vec<RawRef>, Box<str>>>;
/// Where tokens come from: computed, or read back from the cache.
pub trait TokenSource: Send + Sync {
    fn tokens(&self, entry: &Entry, kind: FileKind) -> Tokens;
}

/// Tokens computed from the bytes, with no cache.
#[derive(Clone, Copy, Debug, Default)]
pub struct Computed;

impl TokenSource for Computed {
    fn tokens(&self, entry: &Entry, kind: FileKind) -> Tokens {
        Arc::new(tokenize(&entry.blob, kind).map_err(|invalid| invalid.0.into_boxed_str()))
    }
}

pub use token_memo::TokenMemo;

/// What a reference names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Names {
    File {
        logical: LogicalPath,
        folder: Box<str>,
        suffix: Box<str>,
    },
    Sprite {
        sprite: ResourceId,
        atlases: Vec<ResourceId>,
        prefix: Box<str>,
        suffix: Box<str>,
    },
    SpriteName {
        atlas: ResourceId,
        sprite: ResourceId,
        implicit: bool,
    },
    SoundEvent {
        event: ResourceId,
    },
    Beside {
        logical: LogicalPath,
    },
    Text {
        logical: LogicalPath,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reason(pub Box<str>);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    pub file: PhysPath,
    pub pointer: Pointer,
    pub span: Range<u32>,
    pub names: Names,
    pub reason: Reason,
    pub views: ViewSet,
}

/// What a pass's edits changed, for [`ReferenceIndex::update`]. `touched`: every path written, added or removed;
/// `moved`: each file moved, old path to new (a moved file whose bytes changed is also touched at its new path).
#[derive(Clone, Debug, Default)]
pub struct Change {
    pub touched: BTreeSet<PhysPath>,
    pub moved: BTreeMap<PhysPath, PhysPath>,
    pub layout_changed: bool,
}

/// Every reference of a pack with its location, over every view: what the graph follows, from every file a view
/// shows, reached or not. Each file's references are sorted by pointer, then in the order the walk notes them.
#[derive(Clone, Debug, Default)]
pub struct ReferenceIndex {
    by_file: BTreeMap<PhysPath, Vec<Reference>>,
    state: index::State,
}

impl ReferenceIndex {
    pub fn references(&self, file: &PhysPath) -> &[Reference] {
        self.by_file.get(file).map_or(&[], Vec::as_slice)
    }

    /// Every file that has references, with them.
    pub fn all(&self) -> &BTreeMap<PhysPath, Vec<Reference>> {
        &self.by_file
    }

    pub fn build(
        cx: &RunCx,
        pack: &PackInput,
        layout: &Layout,
        rules: &ClientRules,
        tokens: &dyn TokenSource,
    ) -> Self {
        let state = index::State::build(cx, pack, layout, rules, tokens);
        Self {
            by_file: state.references(None),
            state,
        }
    }

    /// Brings the index to what [`Self::build`] gives on `pack`, redoing only what `change` can make stale.
    pub fn update(
        &mut self,
        cx: &RunCx,
        pack: &PackInput,
        layout: &Layout,
        rules: &ClientRules,
        change: &Change,
        tokens: &dyn TokenSource,
    ) {
        match self.state.update(cx, pack, layout, rules, change, tokens) {
            None => self.by_file = self.state.references(None),
            Some(dirty) => {
                for file in &dirty {
                    self.by_file.remove(file);
                }
                self.by_file.extend(self.state.references(Some(&dirty)));
            }
        }
    }
}

/// What the analysis needs. `index` is the [`ReferenceIndex`] of `pack` under `layout`, built with `cx`.
pub struct Inputs<'a> {
    pub cx: &'a RunCx,
    pub pack: &'a PackInput,
    pub layout: &'a Layout,
    pub rules: &'a ClientRules,
    pub vanilla: &'a VanillaIndex,
    pub index: &'a ReferenceIndex,
    pub sources: &'a [SourceUsages],
    /// The declared items that are roots only when a source uses them (the usage level); empty: the orphans level.
    pub selectors: &'a [UnusedSelector],
    pub policy: Policy,
}

/// Which files are reached without a reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// What the game loads by path or the server can name at any time.
    Pack,
    /// The self-test: the client's own assets, rooted at its registries and the files its code loads by name.
    Vanilla,
}

/// What each view reaches from the roots, merged: a file reached in any view is kept, a finding lists the views it
/// occurs in.
#[derive(Clone, Debug, Default)]
pub struct Reached {
    pub files: BTreeMap<PhysPath, FileEntry>,
    pub edges: BTreeSet<Edge>,
    pub missing: Vec<Missing>,
    pub not_stitched: Vec<NotStitched>,
    /// Files whose JSON the walk read and the tokenizer could not, with why.
    pub invalid: BTreeMap<PhysPath, String>,
    pub sources: Vec<SourceInfo>,
    pub source_roots: Vec<SourceRoot>,
    pub unused_items: Vec<UnusedSelection>,
    /// Notes on the analysis, except the one on sources that read server data, which needs the source registry.
    pub warnings: Vec<String>,
}

impl Reached {
    pub fn analyse(inputs: &Inputs<'_>) -> Result<Self, Refused> {
        analysis::analyse(inputs)
    }

    /// The files the client never loads: what a pruned copy leaves out.
    pub fn removable(&self) -> BTreeSet<PhysPath> {
        self.files
            .iter()
            .filter(|(_, file)| file.status.removable())
            .map(|(path, _)| path.clone())
            .collect()
    }

    /// The logical paths a root of the analysis keeps by name, whatever a pass does.
    pub fn kept_names(&self, layout: &Layout) -> BTreeSet<LogicalPath> {
        self.files
            .iter()
            .filter(|(_, file)| file.root.is_some())
            .filter_map(|(path, _)| layout.split(path).1)
            .collect()
    }

    /// The physical paths a source names by path (a keep-list `path:`): a pass that moves one loses its protection.
    pub fn pinned_paths(&self) -> BTreeSet<PhysPath> {
        self.source_roots
            .iter()
            .filter(|root| {
                root.usages
                    .iter()
                    .any(|usage| usage.kind == UsageKind::Path.id())
            })
            .map(|root| root.path.clone())
            .collect()
    }
}

/// Which files of an analysis a pruned copy leaves out; writing the copy is the engine's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrunePlan {
    pub dropped: BTreeSet<PhysPath>,
}

impl PrunePlan {
    /// Orphans, dead blockstates and the files of unused items; atlas-loaded orphans and unknown files stay.
    pub fn removable(reached: &Reached) -> Self {
        Self {
            dropped: reached.removable(),
        }
    }
}

/// `removable`: the client never loads the file, so a pruned copy leaves it out and the report flags it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Status {
    #[default]
    Reached,
    Orphan,
    /// Unreached, but an atlas source stitches it: it costs client memory, and the server can name the sprite at any
    /// time (a tooltip style, a text sprite, a data-pack asset id). Listed and kept.
    AtlasLoadedOrphan,
    /// Reached only from item declarations an unused-item selector picks and no source uses.
    Unused,
    /// A blockstate for a block the client does not have.
    DeadBlockstate,
    /// Not a vanilla file kind: kept and listed.
    Unknown,
}

impl Status {
    pub fn removable(self) -> bool {
        matches!(
            self,
            Status::Orphan | Status::Unused | Status::DeadBlockstate
        )
    }

    /// The status in snake case.
    pub fn id(self) -> &'static str {
        match self {
            Status::Reached => "reached",
            Status::Orphan => "orphan",
            Status::AtlasLoadedOrphan => "atlas_loaded_orphan",
            Status::Unused => "unused",
            Status::DeadBlockstate => "dead_blockstate",
            Status::Unknown => "unknown",
        }
    }
}

/// `root`: the reason the file is a root, `None` when only a reference reaches it; `atlases`: the atlases that load
/// it (a `.png.mcmeta` shares its texture's).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub kind: FileKind,
    pub bytes: u64,
    pub status: Status,
    pub root: Option<String>,
    pub atlases: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Edge {
    pub from: PhysPath,
    pub to: PhysPath,
    pub reason: String,
}

/// A target found neither in the view nor in vanilla: a logical path, or the text as written when it is no id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Missing {
    pub source: PhysPath,
    pub target: String,
    pub reason: String,
    pub views: ViewSet,
}

/// A sprite no atlas stitches although `file` exists: the client draws the missing texture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotStitched {
    pub source: PhysPath,
    pub sprite: String,
    pub atlases: String,
    pub file: String,
    pub views: ViewSet,
}

/// A usage and the source that named it; empty strings for a reason or an atlas it does not have.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UsageRef {
    pub source: String,
    pub location: String,
    pub reason: String,
    pub kind: String,
    pub id: String,
    pub atlas: String,
}

/// One source: how many usages it named; `matched` name pack files, `vanilla` only vanilla assets, `unmatched`
/// nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceInfo {
    pub id: String,
    pub usages: usize,
    pub matched: usize,
    pub vanilla: usize,
    pub unmatched: Vec<UsageRef>,
}

/// A file a source's usage roots, with every usage that names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceRoot {
    pub path: PhysPath,
    pub usages: Vec<UsageRef>,
}

/// One unused-item selector: the sources searched for uses, each unused and kept declaration, and the files several
/// unused declarations share.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnusedSelection {
    pub selector: String,
    pub searched: Vec<String>,
    pub folders: Vec<UnusedFolder>,
    pub unused: Vec<UnusedItem>,
    pub kept: Vec<KeptItem>,
    pub shared: Vec<SharedFile>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnusedFolder {
    pub folder: String,
    pub items: usize,
    pub files: usize,
    pub bytes: u64,
}

/// `files` and `bytes`: what only this declaration reached, item definitions included.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnusedItem {
    pub declaration: String,
    pub definitions: Vec<PhysPath>,
    pub files: Vec<PhysPath>,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeptItem {
    pub declaration: String,
    pub definitions: Vec<PhysPath>,
    pub used_by: Vec<UsageRef>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedFile {
    pub path: PhysPath,
    pub bytes: u64,
    pub declarations: Vec<String>,
}

/// A run's input refused by name: exit 2.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Refused(pub String);

/// What one source instance read: its id, the path it was opened on (absolute, normalised) and the usages it named.
#[derive(Clone, Debug)]
pub struct SourceUsages {
    pub source: String,
    pub path: Utf8PathBuf,
    pub usages: Vec<Usage>,
}

/// Which declared items become roots only when a source uses them: the declarations of `source` whose location
/// (`items/example/tools.yml:12`) or its file part matches a glob over paths.
#[derive(Clone, Debug)]
pub struct UnusedSelector {
    pub source: String,
    location: usages::Glob,
}

impl UnusedSelector {
    /// Reads `<source>:<location glob>`.
    pub fn parse(text: &str) -> Result<Self, Refused> {
        match text.find(':') {
            Some(colon) if colon > 0 && colon < text.len() - 1 => Ok(Self {
                source: text[..colon].to_owned(),
                location: usages::Glob::path(&text[colon + 1..]),
            }),
            _ => Err(Refused(format!(
                "unused-item selector {text}: expected <source>:<location glob>"
            ))),
        }
    }

    fn matches(&self, source: &str, declaration: &str) -> bool {
        self.source == source
            && (self.location.matches(declaration)
                || self.location.matches(Self::file(declaration)))
    }

    /// The file a location such as `items/tools.yml:12 Pack.model` is in: the text before its first colon.
    fn file(location: &str) -> &str {
        location.split_once(':').map_or(location, |(file, _)| file)
    }
}

impl fmt::Display for UnusedSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.source, self.location.pattern)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UsageKind {
    ItemDefinition,
    Model,
    Sprite,
    Font,
    SoundEvent,
    Equipment,
    TooltipStyle,
    Path,
    Namespace,
}

/// A resource id or a glob over ids.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IdGlob(pub Box<str>);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Usage {
    pub kind: UsageKind,
    pub id: IdGlob,
    pub atlas: Option<ResourceId>,
    pub location: String,
    pub reason: Option<String>,
    pub declares: Option<String>,
    pub quiet: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    /// Exit 2.
    #[error("{0}: {1}")]
    Refused(String, String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub trait UsageSource: Send + Sync {
    fn inputs(&self) -> &[Utf8PathBuf];
    /// Digest of only what the source reads.
    fn input_digest(&self, input: &Utf8Path) -> Result<Digest, SourceError>;
    fn collect(&self) -> Result<Vec<Usage>, SourceError>;
    /// Folders the source never reads, though they lie under its input: packprune's own reports and cache.
    fn exclude(&mut self, dirs: &[Utf8PathBuf]) {
        let _ = dirs;
    }
    /// [`Self::collect`], reusing from `memo` what earlier runs computed from the same inputs.
    fn collect_memoised(&self, memo: &dyn SubCache) -> Result<Vec<Usage>, SourceError> {
        let _ = memo;
        self.collect()
    }
}

/// Memo for expensive pure sub-steps: of graph passes, and of usage sources across runs.
pub trait SubCache: Send + Sync {
    fn get(&self, step: &str, inputs: &[u8]) -> Option<Vec<u8>>;
    fn put(&self, step: &str, inputs: &[u8], value: &[u8]);
    /// Where the reference index takes each file's tokens from.
    fn tokens(&self) -> &dyn TokenSource {
        &Computed
    }
}

/// No memo: every sub-step is computed.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoCache;

impl SubCache for NoCache {
    fn get(&self, _: &str, _: &[u8]) -> Option<Vec<u8>> {
        None
    }

    fn put(&self, _: &str, _: &[u8], _: &[u8]) {}
}
