//! Packs built in memory for the texture and crop passes, over the fixtures' client (`tests/fixtures/vanilla/`), and
//! the harness of `packprune-pass` run on them.

use std::collections::BTreeMap;

use camino::{Utf8Path, Utf8PathBuf};
use packprune_codec::png::{self, Image};
use packprune_graph::{IdGlob, SourceUsages, Usage, UsageKind};
use packprune_pack::{ClientRules, Digest, Entry, PackInput, PhysPath, RunCx, VanillaIndex};
use packprune_pass::{Context, Enabled, NoCache, Pass, Proof, RunError, Scope, prove};

pub(crate) type Files = BTreeMap<String, Vec<u8>>;

pub(crate) const ITEM_MODEL: &str = "assets/fx/models/item/box.json";
pub(crate) const FONT: &str = "assets/fx/font/hud.json";

pub(crate) fn put(files: &mut Files, path: &str, text: &str) {
    files.insert(path.into(), text.as_bytes().to_vec());
}

pub(crate) fn text(files: &Files, path: &str) -> String {
    String::from_utf8(files[path].clone()).unwrap()
}

/// A PNG whose pixel `(x, y)` is `argb(x, y)`, an ARGB int.
pub(crate) fn png(width: u32, height: u32, argb: impl Fn(u32, u32) -> u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let [a, r, g, b] = argb(x, y).to_be_bytes();
            rgba.extend_from_slice(&[r, g, b, a]);
        }
    }
    png::encode(&Image {
        width,
        height,
        rgba,
    })
    .unwrap()
}

/// A PNG whose pixel `(x, y)` is `base + x + 4 * y`, so every texel differs.
pub(crate) fn gradient(width: u32, height: u32, base: u32) -> Vec<u8> {
    png(width, height, |x, y| base + x + 4 * y)
}

pub(crate) fn decode(bytes: &[u8]) -> Image {
    png::decode(bytes).unwrap()
}

/// The ARGB int of pixel `(x, y)`.
pub(crate) fn argb(image: &Image, x: u32, y: u32) -> u32 {
    let at = ((y * image.width + x) * 4) as usize;
    let [r, g, b, a] = image.rgba[at..at + 4].try_into().unwrap();
    u32::from_be_bytes([a, r, g, b])
}

/// An item whose model samples part of a texture through a parent with a display transform,
/// a font, a sound event and a lang file.
pub(crate) fn test_pack() -> Files {
    let mut files = Files::new();
    put(
        &mut files,
        "pack.mcmeta",
        r#"{"pack": {"min_format": 88, "max_format": 88, "description": "t"}}"#,
    );
    put(
        &mut files,
        "assets/fx/items/box.json",
        r#"{"model": {"type": "minecraft:model", "model": "fx:item/box"}}"#,
    );
    put(
        &mut files,
        FONT,
        r#"{"providers": [{"type": "space", "advances": {" ": 4.0}},
 {"type": "bitmap", "file": "fx:font/glyphs.png", "ascent": 7, "chars": [""]}]}"#,
    );
    files.insert(
        "assets/fx/textures/font/glyphs.png".into(),
        gradient(8, 8, 0xFFFF_FF00),
    );
    put(
        &mut files,
        "assets/fx/sounds.json",
        r#"{"boom": {"sounds": ["fx:boom"]}}"#,
    );
    put(&mut files, "assets/fx/sounds/boom.ogg", "not really vorbis");
    put(
        &mut files,
        "assets/fx/lang/en_us.json",
        r#"{"item.fx.box": "Box 16.0"}"#,
    );
    files
}

fn repo() -> Utf8PathBuf {
    Utf8Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn entry(bytes: &[u8]) -> Entry {
    Entry {
        digest: Digest(*blake3::hash(bytes).as_bytes()),
        blob: bytes.into(),
    }
}

pub(crate) fn pack(files: &Files) -> PackInput {
    let files: BTreeMap<PhysPath, Entry> = files
        .iter()
        .map(|(path, bytes)| (PhysPath::new(path).unwrap(), entry(bytes)))
        .collect();
    let mut digest = blake3::Hasher::new();
    for (path, entry) in &files {
        digest
            .update(path.as_str().as_bytes())
            .update(&[0])
            .update(&entry.digest.0);
    }
    PackInput {
        name: "test".into(),
        files,
        digest: Digest(*digest.finalize().as_bytes()),
    }
}

pub(crate) fn files_of(pack: &PackInput) -> Files {
    pack.files
        .iter()
        .map(|(path, entry)| (path.as_str().to_owned(), entry.blob.to_vec()))
        .collect()
}

/// One run's fixed inputs over the fixtures' client; `client` is its files, what the oracle resolves vanilla to.
pub(crate) struct Fixture {
    pub run: RunCx,
    pub vanilla: VanillaIndex,
    pub rules: ClientRules,
    pub client: PackInput,
}

impl Fixture {
    pub(crate) fn new() -> Self {
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
        let vanilla = VanillaIndex {
            version: "26.2".into(),
            pack_format: "88.0".into(),
            names: files.keys().cloned().collect(),
            atlases: files
                .iter()
                .filter(|(path, _)| path.starts_with("assets/minecraft/atlases/"))
                .map(|(path, bytes)| (path.clone(), bytes.clone()))
                .collect(),
            ..VanillaIndex::default()
        };
        Self {
            run: RunCx::default(),
            rules: ClientRules::for_version(&vanilla.version),
            vanilla,
            client: pack(&files),
        }
    }

    pub(crate) fn cx<'a>(&'a self, sources: &'a [SourceUsages]) -> Context<'a> {
        Context {
            run: &self.run,
            vanilla: &self.vanilla,
            rules: &self.rules,
            sources,
            selectors: &[],
            cache: &NoCache,
        }
    }

    /// The harness on `files` with `passes`, each on `scope`, what `kept` lists named by a usage source: a sprite
    /// id, or a path when it starts with `assets/`.
    pub(crate) fn prove(
        &self,
        files: &Files,
        kept: &[&str],
        passes: &[&dyn Pass],
        scope: Scope,
    ) -> Result<Proof, RunError> {
        let sources = [SourceUsages {
            source: "keep".into(),
            path: "keep.yml".into(),
            usages: kept
                .iter()
                .map(|kept| Usage {
                    kind: if kept.starts_with("assets/") {
                        UsageKind::Path
                    } else {
                        UsageKind::Sprite
                    },
                    id: IdGlob((*kept).into()),
                    atlas: None,
                    location: "keep.yml:1".into(),
                    reason: None,
                    declares: None,
                    quiet: false,
                })
                .collect(),
        }];
        let passes = passes
            .iter()
            .map(|pass| Enabled {
                pass: *pass,
                scope: scope.clone(),
                options: toml::Table::new(),
            })
            .collect();
        prove(&self.cx(&sources), &self.client, pack(files), passes)
    }

    /// [`Self::prove`] of one lossless pass on every path, which the oracle must find the same for the player.
    pub(crate) fn same(&self, files: &Files, kept: &[&str], pass: &dyn Pass) -> Ran {
        let proof = self
            .prove(files, kept, &[pass], Scope::all())
            .unwrap_or_else(|error| panic!("{error}"));
        Ran {
            out: files_of(&proof.output.pack),
            proof,
        }
    }
}

/// What a proved run left: its files, and the proof.
pub(crate) struct Ran {
    pub out: Files,
    pub proof: Proof,
}

impl Ran {
    pub(crate) fn changes(&self) -> &[String] {
        &self.proof.reports[0].changes
    }

    pub(crate) fn changed(&self) -> usize {
        self.proof.reports[0].changed.len()
    }

    pub(crate) fn text(&self, path: &str) -> String {
        text(&self.out, path)
    }

    pub(crate) fn size(&self, path: &str) -> (u32, u32) {
        let image = decode(&self.out[path]);
        (image.width, image.height)
    }
}
