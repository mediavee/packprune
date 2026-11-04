//! The sprites of the client's atlases under one view, each with the texture files it is built from and the pack
//! atlas file that defined it. Sources run in order, vanilla's first, then the pack's, as the client stacks them; a
//! later sprite of the same id replaces an earlier one and `filter` removes what came before it.
//!
//! Public for the oracle, which resolves sprites as the graph does.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use packprune_pack::kinds::ResourcePath;
use packprune_pack::meta::BlockFilter;
use packprune_pack::{
    FileKind, Interner, JavaPattern, LogicalPath, PatternError, PhysPath, ResourceId, RunCx,
    VanillaIndex, View,
};

use crate::facts::intern;
use crate::json::{id_parts, type_name, unescape};
pub use crate::walk::VanillaLayer;
use crate::{RawRef, Role, tokenize};

/// A `filter` pattern: `None` matches everything; a pattern that does not compile fails the atlas that holds it.
type Filter = Option<Result<JavaPattern, PatternError>>;

/// One decoded atlas source.
#[derive(Debug)]
pub(crate) enum Source {
    Directory {
        folder: Box<str>,
        prefix: Box<str>,
    },
    Single {
        sprite: ResourceId,
        texture: Arc<str>,
    },
    Filter {
        namespace: Filter,
        path: Filter,
    },
    /// `regions`: each sprite's region as the source writes it, with the divisors, so a change to one shows.
    Unstitch {
        texture: Arc<str>,
        sprites: Vec<ResourceId>,
        regions: Vec<Box<str>>,
    },
    Paletted {
        bases: Vec<(Box<str>, Box<str>, Arc<str>)>,
        key: Arc<str>,
        permutations: Vec<(Box<str>, Arc<str>)>,
        separator: Box<str>,
    },
}

/// An atlas file decoded once; `invalid` holds the ids that do not parse.
#[derive(Debug, Default)]
pub struct AtlasFile {
    pub(crate) sources: Vec<Source>,
    pub(crate) invalid: Vec<Box<str>>,
}

/// A texture file a sprite is built from, and the source kind that names it (`atlas.single`, `atlas.directory`...).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpriteInput {
    pub logical: Arc<str>,
    pub reason: &'static str,
}

/// An atlas sprite: its inputs, the pack atlas file that defined it (`None` for vanilla's), and the part of its input
/// an unstitch source cuts (empty for the whole texture).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sprite {
    pub inputs: Vec<SpriteInput>,
    pub origin: Option<PhysPath>,
    pub region: Box<str>,
}

/// The tokens of one source of an atlas file, under its `/sources/<seg>` pointer.
pub(crate) struct AtlasSource<'t> {
    pub at: &'t str,
    tokens: Vec<&'t RawRef>,
}

impl<'t> AtlasSource<'t> {
    pub(crate) fn field(&self, name: &str) -> Option<&'t RawRef> {
        let pointer = format!("{}/{name}", self.at);
        self.tokens
            .iter()
            .copied()
            .find(|token| *token.pointer.0 == *pointer)
    }

    pub(crate) fn under(&self, name: &str) -> Vec<&'t RawRef> {
        let prefix = format!("{}/{name}/", self.at);
        self.tokens
            .iter()
            .copied()
            .filter(|token| token.pointer.0.starts_with(&*prefix))
            .collect()
    }

    pub(crate) fn kind(&self) -> &'t str {
        self.field("type").map_or("", |token| type_name(&token.raw))
    }

    pub(crate) fn region_sprites(&self) -> Vec<&'t RawRef> {
        self.under("regions")
            .into_iter()
            .filter(|token| token.pointer.0.ends_with("/sprite"))
            .collect()
    }

    /// `asString("")` of a field the tokenizer kept, empty when it did not.
    fn text(&self, name: &str) -> &'t str {
        self.field(name).map_or("", |token| &token.raw)
    }
}

/// The sources of an atlas file's tokens, in order: one source's tokens are consecutive.
pub(crate) fn atlas_sources<'t>(tokens: &[&'t RawRef]) -> Vec<AtlasSource<'t>> {
    let mut sources: Vec<AtlasSource<'t>> = Vec::new();
    for token in tokens {
        let Some(rest) = token.pointer.0.strip_prefix("/sources/") else {
            continue;
        };
        let end = "/sources/".len() + rest.find('/').unwrap_or(rest.len());
        let at = &token.pointer.0[..end];
        match sources.last_mut() {
            Some(source) if source.at == at => source.tokens.push(token),
            _ => sources.push(AtlasSource {
                at,
                tokens: vec![token],
            }),
        }
    }
    sources
}

impl AtlasFile {
    /// Decodes an atlas file's bytes; `None` when it does not parse as JSON.
    pub fn parse(cx: &RunCx, bytes: &[u8]) -> Option<Self> {
        let tokens = tokenize(bytes, FileKind::Atlas).ok()?;
        let json: Vec<&RawRef> = tokens
            .iter()
            .filter(|token| {
                !matches!(
                    token.role,
                    Role::ShaderImportQuoted | Role::ShaderImportAngled
                )
            })
            .collect();
        Some(Self::from_tokens(&cx.interner, &json))
    }

    /// The first `filter` pattern that does not compile: the client fails on it.
    pub fn check(&self) -> Result<(), PatternError> {
        for source in &self.sources {
            if let Source::Filter { namespace, path } = source {
                for pattern in [namespace, path].into_iter().flatten() {
                    pattern.as_ref().map_err(Clone::clone)?;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn from_tokens(interner: &Interner, tokens: &[&RawRef]) -> Self {
        let mut file = Self::default();
        let texture = |(ns, path): &(String, String)| -> Arc<str> {
            format!("assets/{ns}/textures/{path}.png").into()
        };
        let id = |(ns, path): &(String, String)| intern(interner, ns, path);
        for source in atlas_sources(tokens) {
            let invalid = &mut file.invalid;
            let mut parse = |token: &RawRef| -> Option<(String, String)> {
                let parsed =
                    id_parts(&token.raw).map(|(ns, path)| (ns.to_owned(), path.to_owned()));
                if parsed.is_none() {
                    invalid.push(token.raw.clone());
                }
                parsed
            };
            match source.kind() {
                "directory" => file.sources.push(Source::Directory {
                    folder: source.text("source").into(),
                    prefix: source.text("prefix").into(),
                }),
                "single" => {
                    let Some(resource) = source.field("resource").and_then(&mut parse) else {
                        continue;
                    };
                    let sprite = match source.field("sprite") {
                        Some(sprite) if sprite.role == Role::Sprite => {
                            parse(sprite).unwrap_or_else(|| resource.clone())
                        }
                        _ => resource.clone(),
                    };
                    file.sources.push(Source::Single {
                        sprite: id(&sprite),
                        texture: texture(&resource),
                    });
                }
                "filter" => {
                    let pattern = |name: &str| {
                        source
                            .field(&format!("pattern/{name}"))
                            .map(|token| JavaPattern::new(&token.raw))
                    };
                    file.sources.push(Source::Filter {
                        namespace: pattern("namespace"),
                        path: pattern("path"),
                    });
                }
                "unstitch" => {
                    let Some(resource) = source.field("resource").and_then(&mut parse) else {
                        continue;
                    };
                    let divisors =
                        format!("{},{}", source.text("divisor_x"), source.text("divisor_y"));
                    let mut sprites = Vec::new();
                    let mut regions = Vec::new();
                    for token in source.region_sprites() {
                        let Some(sprite) = parse(token) else {
                            continue;
                        };
                        let pointer = &*token.pointer.0;
                        let region = &pointer[source.at.len() + 1..pointer.len() - "/sprite".len()];
                        let fields = ["x", "y", "width", "height"]
                            .map(|field| source.text(&format!("{region}/{field}")));
                        sprites.push(id(&sprite));
                        regions.push(format!("{divisors} {}", fields.join(",")).into_boxed_str());
                    }
                    file.sources.push(Source::Unstitch {
                        texture: texture(&resource),
                        sprites,
                        regions,
                    });
                }
                "paletted_permutations" => {
                    let bases: Vec<_> = source
                        .under("textures")
                        .into_iter()
                        .filter_map(&mut parse)
                        .map(|base| {
                            (
                                base.0.as_str().into(),
                                base.1.as_str().into(),
                                texture(&base),
                            )
                        })
                        .collect();
                    let mut permutations: Vec<(Box<str>, Arc<str>)> = Vec::new();
                    for token in source.under("permutations") {
                        let Some(palette) = parse(token) else {
                            continue;
                        };
                        let name = unescape(token.pointer.0.rsplit('/').next().unwrap_or_default())
                            .into_boxed_str();
                        // `LinkedHashMap.put`: a repeated name keeps its first place and its last palette.
                        match permutations
                            .iter_mut()
                            .find(|(existing, _)| *existing == name)
                        {
                            Some(slot) => slot.1 = texture(&palette),
                            None => permutations.push((name, texture(&palette))),
                        }
                    }
                    if let Some(key) = source.field("palette_key").and_then(&mut parse) {
                        file.sources.push(Source::Paletted {
                            bases,
                            key: texture(&key),
                            permutations,
                            separator: source
                                .field("separator")
                                .map_or("_".into(), |token| token.raw.clone()),
                        });
                    }
                }
                _ => {}
            }
        }
        file
    }
}

/// The textures directory sources list, sorted by their path under `textures/` so a folder is one range.
#[derive(Debug, Default)]
pub(crate) struct Textures {
    entries: Vec<(Box<str>, Box<str>, Arc<str>)>,
}

impl Textures {
    pub(crate) fn new<'a>(logicals: impl Iterator<Item = &'a str>) -> Self {
        let mut entries: Vec<_> = logicals
            .filter_map(|logical| {
                let path = ResourcePath::of(logical)?;
                Some((path.rest.into(), path.namespace.into(), Arc::from(logical)))
            })
            .collect();
        entries.sort();
        Self { entries }
    }

    /// Textures whose path under `textures/` starts with `inside`.
    fn under<'a>(
        &'a self,
        inside: &'a str,
    ) -> impl Iterator<Item = &'a (Box<str>, Box<str>, Arc<str>)> {
        let start = self.entries.partition_point(|entry| &*entry.0 < inside);
        self.entries[start..]
            .iter()
            .take_while(move |entry| entry.0.starts_with(inside))
    }
}

/// What every view's atlases start from: the vanilla textures the pack's `filter.block` leaves, and each atlas the
/// jar defines with vanilla's file for it when the filter leaves that file. Built once per run.
#[derive(Debug)]
pub struct VanillaAtlases {
    textures: Textures,
    /// Atlas id, its logical path, vanilla's file.
    atlases: Vec<(Box<str>, String, Option<Arc<AtlasFile>>)>,
}

impl VanillaAtlases {
    /// A `filter.block` or vanilla atlas pattern that does not compile is refused.
    pub fn new(
        cx: &RunCx,
        vanilla: &VanillaIndex,
        filters: &[BlockFilter],
    ) -> Result<Self, PatternError> {
        Self::with_layer(cx, &VanillaLayer::new(vanilla, filters)?)
    }

    /// [`Self::new`] over a layer the caller already built.
    pub fn with_layer(cx: &RunCx, layer: &VanillaLayer<'_>) -> Result<Self, PatternError> {
        let vanilla = layer.index;
        let textures = Textures::new(
            vanilla
                .names
                .iter()
                .map(String::as_str)
                .filter(|name| FileKind::of(name) == FileKind::Texture && layer.has(name)),
        );
        let mut atlases = Vec::new();
        for atlas in vanilla.atlas_ids() {
            let (ns, path) = atlas.split_once(':').unwrap_or(("minecraft", &atlas));
            let logical = format!("assets/{ns}/atlases/{path}.json");
            let file = vanilla
                .atlases
                .get(&logical)
                .filter(|_| layer.has(&logical))
                .and_then(|bytes| AtlasFile::parse(cx, bytes));
            if let Some(file) = &file {
                file.check()?;
            }
            atlases.push((atlas.as_str().into(), logical, file.map(Arc::new)));
        }
        Ok(Self { textures, atlases })
    }

    /// Each atlas id the client loads, with the logical path of its file.
    pub fn atlas_files(&self) -> impl Iterator<Item = (&str, &str)> {
        self.atlases
            .iter()
            .map(|(atlas, logical, _)| (&**atlas, logical.as_str()))
    }
}

/// The atlases of one view.
#[derive(Debug, Default)]
pub struct ViewAtlases {
    sprites: BTreeMap<Box<str>, HashMap<ResourceId, Sprite>>,
}

impl ViewAtlases {
    /// `pack_atlas` decodes the pack's atlas file at a physical path of `view`. A `filter` pattern that does not
    /// compile fails the build, as it fails the client.
    pub fn build<'a>(
        cx: &RunCx,
        vanilla: &VanillaAtlases,
        view: &View,
        pack_atlas: impl Fn(&PhysPath) -> Option<&'a AtlasFile>,
    ) -> Result<Self, PatternError> {
        let textures = Textures::new(
            view.files
                .keys()
                .map(LogicalPath::as_str)
                .filter(|logical| FileKind::of(logical) == FileKind::Texture),
        );
        let mut atlases = Self::default();
        for (atlas, logical, vanilla_file) in &vanilla.atlases {
            let mut sprites = HashMap::new();
            if let Some(file) = vanilla_file {
                run(
                    file,
                    None,
                    &mut sprites,
                    [&vanilla.textures, &textures],
                    &cx.interner,
                );
            }
            let physical = LogicalPath::new(logical)
                .ok()
                .and_then(|logical| view.files.get(&logical));
            if let Some(physical) = physical
                && let Some(file) = pack_atlas(physical)
            {
                file.check()?;
                run(
                    file,
                    Some(physical),
                    &mut sprites,
                    [&vanilla.textures, &textures],
                    &cx.interner,
                );
            }
            atlases.sprites.insert(atlas.clone(), sprites);
        }
        Ok(atlases)
    }

    /// The atlas ids the client loads.
    pub fn atlas_ids(&self) -> impl Iterator<Item = &str> {
        self.sprites.keys().map(|atlas| &**atlas)
    }

    /// Every sprite of `atlas`, vanilla's and the pack's, by id text.
    pub fn sprites(&self, atlas: &str, interner: &Interner) -> Vec<(String, &Sprite)> {
        let mut sprites: Vec<(String, &Sprite)> = self
            .sprites
            .get(atlas)
            .into_iter()
            .flatten()
            .map(|(id, sprite)| (id.display(interner).to_string(), sprite))
            .collect();
        sprites.sort_by(|a, b| a.0.cmp(&b.0));
        sprites
    }

    pub fn sprite(&self, atlas: &str, sprite: ResourceId) -> Option<&Sprite> {
        self.sprites.get(atlas)?.get(&sprite)
    }

    pub(crate) fn lookup(&self, atlas: &str, sprite: ResourceId) -> &[SpriteInput] {
        self.sprite(atlas, sprite)
            .map_or(&[], |sprite| &sprite.inputs)
    }

    /// The inputs of every sprite of `atlas` whose id `test` accepts.
    pub(crate) fn matching(
        &self,
        atlas: &str,
        mut test: impl FnMut(ResourceId) -> bool,
    ) -> Vec<&SpriteInput> {
        self.sprites
            .get(atlas)
            .into_iter()
            .flatten()
            .filter(|(id, _)| test(**id))
            .flat_map(|(_, sprite)| &sprite.inputs)
            .collect()
    }

    /// The inputs of the sprites `physical` defined that survive to the final atlas, but directory listings.
    pub(crate) fn named(&self, physical: &PhysPath) -> Vec<&SpriteInput> {
        self.sprites
            .values()
            .flat_map(HashMap::values)
            .filter(|sprite| sprite.origin.as_ref() == Some(physical))
            .flat_map(|sprite| &sprite.inputs)
            .filter(|input| input.reason != "atlas.directory")
            .collect()
    }

    /// Every texture an atlas loads, with the atlases that load it.
    pub(crate) fn loaded(&self) -> Loaded {
        let mut loaded: Loaded = BTreeMap::new();
        for (atlas, sprites) in &self.sprites {
            for input in sprites.values().flat_map(|sprite| &sprite.inputs) {
                loaded
                    .entry(Arc::clone(&input.logical))
                    .or_default()
                    .insert(atlas.clone());
            }
        }
        loaded
    }
}

/// Every texture an atlas loads, with the atlases that load it.
pub(crate) type Loaded = BTreeMap<Arc<str>, BTreeSet<Box<str>>>;

fn run(
    file: &AtlasFile,
    origin: Option<&PhysPath>,
    sprites: &mut HashMap<ResourceId, Sprite>,
    textures: [&Textures; 2],
    interner: &Interner,
) {
    let sprite = |inputs: Vec<SpriteInput>, region: &str| Sprite {
        inputs,
        origin: origin.cloned(),
        region: region.into(),
    };
    let input = |logical: &Arc<str>, reason| SpriteInput {
        logical: Arc::clone(logical),
        reason,
    };
    for source in &file.sources {
        match source {
            Source::Directory { folder, prefix } => {
                let inside = if folder.is_empty() {
                    String::new()
                } else {
                    format!("{folder}/")
                };
                for (rest, namespace, logical) in
                    textures.iter().flat_map(|textures| textures.under(&inside))
                {
                    let name = &rest[inside.len()..rest.len() - ".png".len()];
                    let id = intern(interner, namespace, &format!("{prefix}{name}"));
                    sprites.insert(id, sprite(vec![input(logical, "atlas.directory")], ""));
                }
            }
            Source::Single {
                sprite: id,
                texture,
            } => {
                sprites.insert(*id, sprite(vec![input(texture, "atlas.single")], ""));
            }
            Source::Filter { namespace, path } => {
                // `check` ran before: every pattern here compiled.
                let test = |pattern: &Filter, text: &str| match pattern {
                    None => true,
                    Some(regex) => regex.as_ref().is_ok_and(|regex| regex.find(text)),
                };
                sprites.retain(|id, _| {
                    !(test(namespace, interner.resolve(id.ns))
                        && test(path, interner.resolve(id.path)))
                });
            }
            Source::Unstitch {
                texture,
                sprites: ids,
                regions,
            } => {
                for (id, region) in ids.iter().zip(regions) {
                    sprites.insert(*id, sprite(vec![input(texture, "atlas.unstitch")], region));
                }
            }
            Source::Paletted {
                bases,
                key,
                permutations,
                separator,
            } => {
                for (namespace, path, texture) in bases {
                    for (suffix, palette) in permutations {
                        let id = intern(interner, namespace, &format!("{path}{separator}{suffix}"));
                        let inputs = vec![
                            input(texture, "atlas.paletted_permutations"),
                            input(key, "atlas.palette_key"),
                            input(palette, "atlas.palette"),
                        ];
                        sprites.insert(id, sprite(inputs, ""));
                    }
                }
            }
        }
    }
}
