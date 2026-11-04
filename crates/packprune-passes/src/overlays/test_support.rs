use std::collections::BTreeMap;
use std::path::Path;

use packprune_pack::{
    ClientRules, Digest, Entry, Limits, LogicalPath, PackInput, PhysPath, RunCx, VanillaIndex,
};
use packprune_pass::{
    Analysed, Context, Enabled, Kept, NoCache, Outcome, Pass, Proof, Scope, prove, run,
};

pub(crate) fn phys(path: &str) -> PhysPath {
    PhysPath::new(path).unwrap()
}
pub(crate) fn logical(path: &str) -> LogicalPath {
    LogicalPath::new(path).unwrap()
}
pub(crate) fn put(pack: &mut PackInput, path: &str, bytes: &[u8]) {
    pack.files.insert(
        phys(path),
        Entry {
            blob: bytes.into(),
            digest: Digest([0; 32]),
        },
    );
}
pub(crate) fn text(state: &Analysed, path: &str) -> String {
    String::from_utf8(state.pack.files[&phys(path)].blob.to_vec()).unwrap()
}
pub(crate) fn entries(pack: &PackInput) -> BTreeMap<String, Vec<u8>> {
    pack.files
        .iter()
        .map(|(path, entry)| (path.to_string(), entry.blob.to_vec()))
        .collect()
}
pub(crate) fn empty_pack() -> PackInput {
    let mut pack = PackInput {
        name: "test".into(),
        files: BTreeMap::new(),
        digest: Digest([0; 32]),
    };
    put(
        &mut pack,
        "pack.mcmeta",
        br#"{"pack":{"min_format":88,"max_format":88,"description":"t"}}"#,
    );
    pack
}

// Re-read fixtures so their entry digests describe the bytes, as real inputs do.
pub(crate) fn read_pack(pack: &PackInput) -> PackInput {
    let temp = tempfile::tempdir().unwrap();
    for (path, entry) in &pack.files {
        let path = temp.path().join(path.as_str());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, &entry.blob).unwrap();
    }
    PackInput::open(temp.path().to_str().unwrap().into(), &Limits::default()).unwrap()
}

pub(crate) struct Fixture {
    pub run: RunCx,
    pub vanilla: VanillaIndex,
    pub rules: ClientRules,
    pub assets: PackInput,
}
impl Fixture {
    pub fn new() -> Self {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/vanilla");
        let assets = PackInput::open(path.to_str().unwrap().into(), &Limits::default()).unwrap();
        let vanilla = VanillaIndex {
            version: "26.2".into(),
            pack_format: "88.0".into(),
            names: assets.files.keys().map(ToString::to_string).collect(),
            atlases: assets
                .files
                .iter()
                .filter(|(path, _)| path.as_str().starts_with("assets/minecraft/atlases/"))
                .map(|(path, entry)| (path.to_string(), entry.blob.to_vec()))
                .collect(),
            ..VanillaIndex::default()
        };
        Self {
            run: RunCx::default(),
            rules: ClientRules::for_version(&vanilla.version),
            vanilla,
            assets,
        }
    }
    pub fn cx(&self) -> Context<'_> {
        Context {
            run: &self.run,
            vanilla: &self.vanilla,
            rules: &self.rules,
            sources: &[],
            selectors: &[],
            cache: &NoCache,
        }
    }
    pub fn apply(&self, pack: PackInput, passes: &[&dyn Pass], kept: Kept) -> Outcome {
        let cx = self.cx();
        let state = cx.analyse(read_pack(&pack)).unwrap();
        let mut passes: Vec<_> = passes
            .iter()
            .map(|pass| Enabled {
                pass: *pass,
                scope: Scope::all(),
                options: toml::Table::new(),
            })
            .collect();
        packprune_pass::order(&mut passes);
        run(&cx, &kept, &state, &passes).unwrap()
    }
    pub fn prove(&self, pack: PackInput, passes: &[&dyn Pass]) -> Proof {
        let enabled = passes
            .iter()
            .map(|pass| Enabled {
                pass: *pass,
                scope: Scope::all(),
                options: toml::Table::new(),
            })
            .collect();
        let proof = prove(&self.cx(), &self.assets, read_pack(&pack), enabled).unwrap();
        assert_eq!(proof.proof.as_ref().unwrap().differing_keys, 0);
        proof
    }
}
