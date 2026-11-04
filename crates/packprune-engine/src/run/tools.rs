//! `diff` (the oracle as a tool) and `self-test` (the graph over the client's own assets).

use std::collections::BTreeMap;
use std::time::Instant;

use camino::{Utf8Path, Utf8PathBuf};
use packprune_config::InputLimits;
use packprune_graph::{Computed, Inputs, Policy, Reached, ReferenceIndex, Status};
use packprune_oracle::{OracleInput, Remap, compare_with, view_labels};
use packprune_pack::{
    ClientRules, Entry, FileKind, Layout, Limits, PackInput, PackMeta, PhysPath, RunCx,
    VanillaIndex,
};
use packprune_pass::{Context, NoCache};

use super::cache::Cache;
use super::inputs::{self, JarAssets, VanillaInputs};
use super::optimisers::digest;
use super::{RunError, Verdict, limits, verdict};

pub struct DiffRequest {
    pub before: Utf8PathBuf,
    pub after: Utf8PathBuf,
    pub vanilla: VanillaInputs,
    pub limits: InputLimits,
}

pub struct Diff {
    pub verdict: Verdict,
}

/// What changes for the player between two packs, every view of either compared, every file compared whole as a
/// name something outside the pack can use.
pub fn diff(request: &DiffRequest) -> Result<Diff, RunError> {
    let limits = limits(&request.limits);
    let vanilla = inputs::vanilla(&request.vanilla, &limits, &Cache::new(None))?;
    let rules = ClientRules::for_version(&vanilla.version);
    let cx = RunCx::default();
    let read = |path: &Utf8Path| -> Result<(PackInput, Layout, ReferenceIndex), RunError> {
        let pack = PackInput::open(path, &limits)?;
        let meta = PackMeta::read(&pack).map_err(|error| RunError::Refused(error.to_string()))?;
        let layout = Layout::new(&pack, meta, &vanilla);
        let index = ReferenceIndex::build(&cx, &pack, &layout, &rules, &Computed);
        Ok((pack, layout, index))
    };
    let (before_pack, before_layout, before_index) = read(&request.before)?;
    let (after_pack, after_layout, after_index) = read(&request.after)?;
    let assets = JarAssets::new(&request.vanilla.client_jar, &limits);
    let started = Instant::now();
    let comparison = compare_with(
        &cx,
        &OracleInput {
            pack: &before_pack,
            layout: &before_layout,
            index: &before_index,
        },
        &OracleInput {
            pack: &after_pack,
            layout: &after_layout,
            index: &after_index,
        },
        &vanilla,
        assets.load()?,
        &Remap::default(),
        &|_| true,
    );
    let labels = view_labels(&before_layout, &after_layout);
    Ok(Diff {
        verdict: verdict(&comparison, &labels, started),
    })
}

/// Every vanilla model and texture is reached from the client's registries and the files its
/// code loads by name, every code-loaded pattern matches a file, and a pack copy of every vanilla blockstate is a root.
pub struct SelfTest {
    pub models: usize,
    pub textures: usize,
    pub code_loaded: usize,
    /// Models and textures the graph does not reach, by path.
    pub unexplained: Vec<String>,
    /// Code-loaded patterns that root no file.
    pub stale: Vec<String>,
    pub blockstates: usize,
    /// Vanilla blockstates a pack copy of would not root.
    pub unrooted: Vec<String>,
}

impl SelfTest {
    pub fn passed(&self) -> bool {
        self.unexplained.is_empty() && self.stale.is_empty() && self.unrooted.is_empty()
    }
}

pub fn self_test(client_jar: &Utf8Path) -> Result<SelfTest, RunError> {
    let limits = Limits::default();
    let index = inputs::vanilla(
        &VanillaInputs {
            client_jar: client_jar.to_owned(),
            asset_index: None,
        },
        &limits,
        &Cache::new(None),
    )?;
    let rules = ClientRules::for_version(&index.version);
    let jar = PackInput::open(client_jar, &limits)?;
    let assets = subset(&jar, |path| path.starts_with("assets/"));
    let cx = RunCx::default();

    // The client's own assets are the pack: no vanilla layer under them, one view, verified.
    let bare = VanillaIndex {
        names: Default::default(),
        pack_format: "unknown".into(),
        ..index.clone()
    };
    let none = PackMeta {
        formats: None,
        overlays: Vec::new(),
        filters: Vec::new(),
    };
    let layout = Layout::new(&assets, none, &bare);
    let references = ReferenceIndex::build(&cx, &assets, &layout, &rules, &Computed);
    let reached = Reached::analyse(&Inputs {
        cx: &cx,
        pack: &assets,
        layout: &layout,
        rules: &rules,
        vanilla: &bare,
        index: &references,
        sources: &[],
        selectors: &[],
        policy: Policy::Vanilla,
    })
    .map_err(|refused| RunError::Refused(refused.0))?;

    let checked: Vec<(&PhysPath, FileKind, Status)> = reached
        .files
        .iter()
        .filter(|(_, file)| matches!(file.kind, FileKind::Model | FileKind::Texture))
        .map(|(path, file)| (path, file.kind, file.status))
        .collect();
    let models = checked
        .iter()
        .filter(|(_, kind, _)| *kind == FileKind::Model)
        .count();
    let unexplained = checked
        .iter()
        .filter(|(_, _, status)| *status != Status::Reached)
        .map(|(path, ..)| path.to_string())
        .collect();
    let roots: Vec<&str> = reached
        .files
        .values()
        .filter_map(|file| file.root.as_deref())
        .collect();
    let code_loaded = roots
        .iter()
        .filter(|root| root.starts_with("code_loaded"))
        .count();
    let stale = rules
        .code_loaded
        .iter()
        .map(|glob| &glob.pattern)
        .filter(|pattern| !roots.contains(&format!("code_loaded {pattern}").as_str()))
        .cloned()
        .collect();

    // The pack-level rule against the jar: every vanilla blockstate, copied into a pack, analysed as a pack.
    let mut blockstates = subset(&jar, |path| {
        index.has(path) && FileKind::of(path) == FileKind::Blockstate
    });
    let count = blockstates.files.len();
    let major = index
        .pack_format
        .split('.')
        .next()
        .and_then(|major| major.parse::<u32>().ok())
        .unwrap_or(0);
    let meta = format!(r#"{{"pack": {{"pack_format": {major}, "description": "self-test"}}}}"#);
    blockstates.files.insert(
        PhysPath::new("pack.mcmeta").expect("a valid path"),
        Entry {
            digest: packprune_pack::Digest(*blake3::hash(meta.as_bytes()).as_bytes()),
            blob: meta.into_bytes().into(),
        },
    );
    blockstates.digest = digest(&blockstates.files);
    let context = Context {
        run: &cx,
        vanilla: &index,
        rules: &rules,
        sources: &[],
        selectors: &[],
        cache: &NoCache,
    };
    let analysed = context.analyse(blockstates)?;
    let unrooted = analysed
        .reached
        .files
        .iter()
        .filter(|(_, file)| file.kind == FileKind::Blockstate && file.root.is_none())
        .map(|(path, _)| path.to_string())
        .collect();

    Ok(SelfTest {
        models,
        textures: checked.len() - models,
        code_loaded,
        unexplained,
        stale,
        blockstates: count,
        unrooted,
    })
}

fn subset(pack: &PackInput, keep: impl Fn(&str) -> bool) -> PackInput {
    let files: BTreeMap<PhysPath, Entry> = pack
        .files
        .iter()
        .filter(|(path, _)| keep(path.as_str()))
        .map(|(path, entry)| (path.clone(), entry.clone()))
        .collect();
    PackInput {
        name: pack.name.clone(),
        digest: digest(&files),
        files,
    }
}
