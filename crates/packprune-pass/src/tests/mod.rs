//! The framework under three test passes (rename, remove, move), its refusals, and the harness on a planted wrong
//! remap; `corpus` runs it on a real pack.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use camino::{Utf8Path, Utf8PathBuf};
use packprune_graph::{Computed, ReferenceIndex};
use packprune_oracle::VanillaAssets;
use packprune_pack::{
    Blob, ClientRules, Digest, Entry, FileKind, LogicalPath, PackInput, PhysPath, ResourceId,
    RunCx, VanillaIndex,
};

use crate::{Analysed, Context, Edits, Loss, NoCache, Pass, PassCx, PassError, Stage, apply};

mod corpus;
mod framework;

pub(crate) type Files = BTreeMap<String, Vec<u8>>;

fn repo() -> Utf8PathBuf {
    Utf8Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The fixtures' client (`tests/fixtures/vanilla/`): its files as the jar would hold them.
pub(crate) struct Client(Files);

impl Client {
    pub(crate) fn fixture() -> Self {
        let root = repo().join("tests/fixtures/vanilla");
        let mut files = Files::new();
        let mut stack = vec![root.join("assets")];
        while let Some(dir) = stack.pop() {
            for entry in dir.read_dir_utf8().unwrap() {
                let path = entry.unwrap().into_path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    let name = path.strip_prefix(&root).unwrap().as_str().to_owned();
                    files.insert(name, std::fs::read(&path).unwrap());
                }
            }
        }
        Self(files)
    }

    pub(crate) fn index(&self) -> VanillaIndex {
        VanillaIndex {
            version: "26.2".into(),
            pack_format: "88.0".into(),
            names: self.0.keys().cloned().collect(),
            atlases: self
                .0
                .iter()
                .filter(|(path, _)| path.starts_with("assets/minecraft/atlases/"))
                .map(|(path, bytes)| (path.clone(), bytes.clone()))
                .collect(),
            ..VanillaIndex::default()
        }
    }
}

impl VanillaAssets for Client {
    fn bytes(&self, logical: &str) -> Option<Blob> {
        self.0.get(logical).map(|bytes| bytes.as_slice().into())
    }
}

/// One run's fixed inputs over the fixtures' client.
pub(crate) struct Fixture {
    pub run: RunCx,
    pub client: Client,
    pub vanilla: VanillaIndex,
    pub rules: ClientRules,
}

impl Fixture {
    pub(crate) fn new() -> Self {
        let client = Client::fixture();
        let vanilla = client.index();
        Self {
            run: RunCx::default(),
            rules: ClientRules::for_version(&vanilla.version),
            client,
            vanilla,
        }
    }

    pub(crate) fn cx(&self) -> Context<'_> {
        Context {
            run: &self.run,
            vanilla: &self.vanilla,
            rules: &self.rules,
            sources: &[],
            selectors: &[],
            cache: &NoCache,
        }
    }
}

pub(crate) fn pack(files: &Files) -> PackInput {
    let mut pack = PackInput {
        name: "test".into(),
        files: files
            .iter()
            .map(|(path, bytes)| {
                let entry = Entry {
                    digest: Digest(*blake3::hash(bytes).as_bytes()),
                    blob: bytes.as_slice().into(),
                };
                (phys(path), entry)
            })
            .collect(),
        digest: Digest([0; 32]),
    };
    apply::redigest(&mut pack);
    pack
}

pub(crate) fn phys(path: &str) -> PhysPath {
    PhysPath::new(path).unwrap()
}

pub(crate) fn logical(path: &str) -> LogicalPath {
    LogicalPath::new(path).unwrap()
}

pub(crate) fn text(state: &Analysed, path: &str) -> String {
    String::from_utf8(state.pack.files[&phys(path)].blob.to_vec()).unwrap()
}

/// Each path with its digest: what two outputs are compared on.
pub(crate) fn entries(pack: &PackInput) -> Vec<(PhysPath, Digest)> {
    pack.files
        .iter()
        .map(|(path, entry)| (path.clone(), entry.digest))
        .collect()
}

/// The incremental index against one built from scratch on the same pack.
pub(crate) fn assert_index_rebuilt(cx: &Context<'_>, state: &Analysed) {
    let built = ReferenceIndex::build(cx.run, &state.pack, &state.layout, cx.rules, &Computed);
    assert!(
        built.all() == state.index.all(),
        "the updated index differs from a rebuild"
    );
}

/// Renames files by logical path in every layer that holds them, with the remaps that let the framework rewrite
/// what names them. `Models` picks every model no name keeps and gives it a `pp_` name beside it.
pub(crate) struct Rename {
    pub name: &'static str,
    pub targets: Targets,
}

pub(crate) enum Targets {
    Listed {
        files: Vec<(&'static str, &'static str)>,
        sprites: Vec<(&'static str, &'static str)>,
    },
    Models,
}

impl Pass for Rename {
    fn name(&self) -> &'static str {
        self.name
    }

    fn stage(&self) -> Stage {
        Stage::Names
    }

    fn loss(&self) -> Loss {
        Loss::Lossless
    }

    fn options_schema(&self) -> schemars::Schema {
        schemars::Schema::default()
    }

    fn run(&self, cx: &PassCx<'_>) -> Result<Edits, PassError> {
        let mut edits = Edits::default();
        let (files, sprites): (Vec<(LogicalPath, LogicalPath)>, _) = match &self.targets {
            Targets::Listed { files, sprites } => (
                files
                    .iter()
                    .map(|(from, to)| (logical(from), logical(to)))
                    .collect(),
                sprites.clone(),
            ),
            Targets::Models => (models(cx), Vec::new()),
        };
        for (from, to) in files {
            for path in cx.pack.files.keys() {
                if cx.layout.split(path).1.as_ref() != Some(&from) {
                    continue;
                }
                let folder = &path.as_str()[..path.as_str().len() - from.as_str().len()];
                edits.removed.insert(path.clone());
                edits.added.insert(
                    phys(&format!("{folder}{to}")),
                    Arc::clone(&cx.pack.files[path].blob),
                );
            }
            edits.changes.push(format!("{from} -> {to}"));
            edits.file_remaps.insert(from, to);
        }
        for (from, to) in sprites {
            let parse = |id| ResourceId::parse(&cx.run.interner, id).unwrap();
            edits.sprite_remaps.insert(parse(from), parse(to));
        }
        Ok(edits)
    }
}

/// Every model no name keeps, to a free `pp_` name in its folder.
fn models(cx: &PassCx<'_>) -> Vec<(LogicalPath, LogicalPath)> {
    let logicals: BTreeSet<LogicalPath> = cx
        .pack
        .files
        .keys()
        .filter_map(|path| cx.layout.split(path).1)
        .collect();
    logicals
        .iter()
        .filter(|from| FileKind::of(from.as_str()) == FileKind::Model)
        .filter(|from| !cx.kept_names.contains(*from))
        .filter(|from| {
            !cx.pinned
                .iter()
                .any(|path| cx.layout.split(path).1.as_ref() == Some(*from))
        })
        .filter_map(|from| {
            let text = from.as_str();
            let slash = text.rfind('/').unwrap();
            let to = logical(&format!("{}pp_{}", &text[..=slash], &text[slash + 1..]));
            (!logicals.contains(&to)).then(|| (from.clone(), to))
        })
        .collect()
}

/// Removes the listed physical paths.
pub(crate) struct Remove(pub Vec<&'static str>);

impl Pass for Remove {
    fn name(&self) -> &'static str {
        "remove"
    }

    fn stage(&self) -> Stage {
        Stage::Models
    }

    fn loss(&self) -> Loss {
        Loss::Lossless
    }

    fn options_schema(&self) -> schemars::Schema {
        schemars::Schema::default()
    }

    fn run(&self, _: &PassCx<'_>) -> Result<Edits, PassError> {
        let mut edits = Edits::default();
        edits.removed.extend(self.0.iter().map(|path| phys(path)));
        Ok(edits)
    }
}

/// Moves files from one physical path to another, as they are.
pub(crate) struct Move(pub Vec<(&'static str, &'static str)>);

impl Pass for Move {
    fn name(&self) -> &'static str {
        "move"
    }

    fn stage(&self) -> Stage {
        Stage::Overlays
    }

    fn loss(&self) -> Loss {
        Loss::Lossless
    }

    fn options_schema(&self) -> schemars::Schema {
        schemars::Schema::default()
    }

    fn run(&self, cx: &PassCx<'_>) -> Result<Edits, PassError> {
        let mut edits = Edits::default();
        for (from, to) in &self.0 {
            let (from, to) = (phys(from), phys(to));
            edits.removed.insert(from.clone());
            edits
                .added
                .insert(to.clone(), Arc::clone(&cx.pack.files[&from].blob));
            edits.moves.insert(from, to);
        }
        Ok(edits)
    }
}

/// A pass whose edits a closure writes, for the refusals.
pub(crate) struct Edit {
    pub loss: Loss,
    pub edits: Box<dyn Fn(&PassCx<'_>) -> Edits + Send + Sync>,
}

impl Pass for Edit {
    fn name(&self) -> &'static str {
        "edit"
    }

    fn stage(&self) -> Stage {
        Stage::Json
    }

    fn loss(&self) -> Loss {
        self.loss
    }

    fn options_schema(&self) -> schemars::Schema {
        schemars::Schema::default()
    }

    fn run(&self, cx: &PassCx<'_>) -> Result<Edits, PassError> {
        Ok((self.edits)(cx))
    }
}
