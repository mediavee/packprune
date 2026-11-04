//! One view of one pack, resolved as the 26.2 client bakes it: each key (an item definition,
//! block state, equipment asset, font glyph, sound event, atlas sprite, or a file read whole) to a canonical
//! descriptor. A model resolves to its parent chain's geometry with each drawn face's texels, texture slots, display
//! transforms, ambient occlusion and GUI light; a face to the texel block it samples in every animation frame, its UVs
//! relative to that block, tint and cull. What the descriptors do not hold the client does not draw.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Arc, LazyLock, OnceLock};

use packprune_codec::{glsl, minify};
use regex::Regex;

use packprune_pack::kinds::ResourcePath;
use packprune_pack::meta::{Format, show_format};
use packprune_pack::rules::TextureRule;
use packprune_pack::{
    Blob, ClientRules, FileKind, Interner, Layout, LogicalPath, PackInput, PhysPath, ResourceId,
    RunCx, VanillaIndex, View,
};

use packprune_graph::atlases::{
    AtlasFile, Sprite, SpriteInput, VanillaAtlases, VanillaLayer, ViewAtlases,
};

use crate::VanillaAssets;
use crate::canon::{self, Desc, Digest, Key, canonical, canonical_with, java_order, missing};
use crate::id::{self, DEFAULT_NAMESPACE, Id};
use crate::memo::{Answer, Query, Recorder, Trace};
use crate::shared::{Image, Memo, Shared};
use crate::uvlock::{self, DIRECTIONS};
use packprune_pack::json::{Member, Node, Seg, Value, parse as parse_json};

/// `ItemDisplayContext` names a model's `display` reads.
const DISPLAY: [&str; 9] = [
    "thirdperson_righthand",
    "thirdperson_lefthand",
    "firstperson_righthand",
    "firstperson_lefthand",
    "head",
    "gui",
    "ground",
    "fixed",
    "on_shelf",
];
/// `ItemModelGenerator.LAYERS`.
const LAYERS: usize = 5;
/// Texel coordinates are compared to 1/4096 of a texel: below what nearest sampling can show.
const TEXEL_STEP: f64 = 4096.0;
/// The first format with atlas files (1.19.3). Before it, `ModelBakery` stitched every texture a loaded model names
/// into the blocks atlas (`BlockModel.getMaterials`), each read from `textures/<path>.png`.
const ATLAS_FILES: u32 = 12;

/// `ModelManager.CombinedBlockItemMaterialBaker`: item models look in the items atlas, then the blocks atlas.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Atlas {
    Items,
    Blocks,
}

impl Atlas {
    fn ids(self) -> &'static [&'static str] {
        match self {
            Atlas::Items => &["minecraft:items", "minecraft:blocks"],
            Atlas::Blocks => &["minecraft:blocks"],
        }
    }
}

/// What a comparison shares between its views and sides.
pub(crate) struct Ctx<'a> {
    cx: &'a RunCx,
    pub(crate) vanilla: &'a VanillaIndex,
    assets: &'a dyn VanillaAssets,
    rules: ClientRules,
    pub(crate) shared: Shared,
    vanilla_bytes: Memo<String, Option<(Blob, Digest)>>,
    /// The `named` predicate, evaluated up front for every path a resolution can ask about: it is not `Sync`.
    named: HashSet<String>,
}

impl<'a> Ctx<'a> {
    pub(crate) fn new(
        cx: &'a RunCx,
        vanilla: &'a VanillaIndex,
        assets: &'a dyn VanillaAssets,
        named: HashSet<String>,
    ) -> Self {
        Self {
            cx,
            vanilla,
            assets,
            rules: ClientRules::for_version(&vanilla.version),
            shared: Shared::new(),
            vanilla_bytes: Memo::new(),
            named,
        }
    }
    /// Every logical path a texture, sprite input or font sheet lookup can test against `named`, beyond the
    /// pack files: the textures the jar's atlas files and the packs' atlas files name.
    pub(crate) fn named_candidates(
        interner: &Interner,
        vanilla: &VanillaIndex,
        packs: &[&PackInput],
    ) -> BTreeSet<String> {
        let mut out: BTreeSet<String> = vanilla.names.iter().cloned().collect();
        let mut atlas = |json: &Node| {
            for source in json.path("sources").children() {
                for field in ["resource", "palette_key"] {
                    if let Some(id) = source
                        .get(field)
                        .and_then(Node::as_str)
                        .and_then(|value| Id::parse(interner, value))
                    {
                        out.insert(id.file("textures", ".png"));
                    }
                }
                for texture in source
                    .path("textures")
                    .children()
                    .chain(source.path("permutations").children())
                {
                    if let Some(id) = texture
                        .as_str()
                        .and_then(|value| Id::parse(interner, value))
                    {
                        out.insert(id.file("textures", ".png"));
                    }
                }
            }
        };
        for bytes in vanilla.atlases.values() {
            if let Ok(json) = parse_json(bytes) {
                atlas(&json);
            }
        }
        for pack in packs {
            for (path, entry) in &pack.files {
                if path.as_str().contains("/atlases/")
                    && path.as_str().ends_with(".json")
                    && let Ok(json) = parse_json(&entry.blob)
                {
                    atlas(&json);
                }
            }
        }
        out
    }

    fn named(&self, logical: &str) -> bool {
        self.named.contains(logical)
    }

    fn vanilla_file(&self, logical: &str) -> Option<(Blob, Digest)> {
        self.vanilla_bytes.get(&logical.to_owned(), || {
            let bytes = self.assets.bytes(logical)?;
            let digest = canon::digest_bytes(&bytes);
            Some((bytes, digest))
        })
    }
}

/// One pack of a comparison: its files, layout, the vanilla client under its `filter.block`, and the views of the
/// compared formats.
pub(crate) struct Side<'a> {
    pub(crate) pack: &'a PackInput,
    pub(crate) layout: &'a Layout,
    /// The vanilla client under the pack's `filter.block`; `None` when a pattern does not compile, which the analysis
    /// refuses before any comparison: nothing is hidden then, and no sprite resolves.
    layer: Option<VanillaLayer<'a>>,
    vanilla_atlases: Option<VanillaAtlases>,
    atlas_files: BTreeMap<PhysPath, AtlasFile>,
    /// Each logical path of the pack by its text, so a lookup does not check the path rules again.
    logicals: HashMap<Box<str>, LogicalPath>,
    pub(crate) views: Vec<ViewState>,
}

pub(crate) struct ViewState {
    pub(crate) format: Format,
    pub(crate) view: View,
    atlases: OnceLock<ViewAtlases>,
    /// `view.files` hashed: memo checks look up every file a memoised resolution asked for.
    files: OnceLock<HashMap<LogicalPath, PhysPath>>,
}

impl<'a> Side<'a> {
    pub(crate) fn new(
        cx: &RunCx,
        vanilla: &'a VanillaIndex,
        pack: &'a PackInput,
        layout: &'a Layout,
        formats: &[Format],
    ) -> Self {
        let layer = VanillaLayer::new(vanilla, &layout.meta.filters).ok();
        let vanilla_atlases = layer
            .as_ref()
            .and_then(|layer| VanillaAtlases::with_layer(cx, layer).ok());
        let atlas_files = layout
            .kinds
            .iter()
            .filter(|(_, kind)| **kind == FileKind::Atlas)
            .filter_map(|(path, _)| {
                let entry = pack.files.get(path)?;
                Some((path.clone(), AtlasFile::parse(cx, &entry.blob)?))
            })
            .collect();
        let logicals = pack
            .files
            .keys()
            .filter_map(|path| layout.split(path).1)
            .map(|logical| (logical.as_str().into(), logical))
            .collect();
        Self {
            pack,
            layout,
            layer,
            vanilla_atlases,
            atlas_files,
            logicals,
            views: formats
                .iter()
                .zip(layout.views_at(formats))
                .map(|(&format, view)| ViewState {
                    format,
                    view,
                    atlases: OnceLock::new(),
                    files: OnceLock::new(),
                })
                .collect(),
        }
    }

    /// `VanillaLayer.has`: the vanilla client has `logical` and the pack's filter does not hide it.
    fn has_vanilla(&self, ctx: &Ctx<'_>, logical: &str) -> bool {
        match &self.layer {
            Some(layer) => layer.has(logical),
            None => ctx.vanilla.has(logical),
        }
    }

    fn atlases<'s>(&'s self, ctx: &Ctx<'_>, state: &'s ViewState) -> &'s ViewAtlases {
        state.atlases.get_or_init(|| {
            self.vanilla_atlases
                .as_ref()
                .and_then(|vanilla| {
                    ViewAtlases::build(ctx.cx, vanilla, &state.view, |physical| {
                        self.atlas_files.get(physical)
                    })
                    .ok()
                })
                .unwrap_or_default()
        })
    }
}

fn digest_of(entry: &packprune_pack::Entry) -> Digest {
    entry.digest.0[..16].try_into().expect("16 of 32 bytes")
}

/// `Variant.SimpleModelState`: a variant's rotation in quarter turns, which moves a face's UVs only with `uvlock`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
struct Lock {
    x: i32,
    y: i32,
    z: i32,
}

impl Lock {
    fn of(variant: &Node) -> Self {
        if !variant.path("uvlock").as_bool(false) {
            return Self::default();
        }
        Self {
            x: variant.path("x").as_int(0) / 90,
            y: variant.path("y").as_int(0) / 90,
            z: variant.path("z").as_int(0) / 90,
        }
    }
}

#[derive(Clone, Debug)]
struct Material {
    sprite: String,
    force_translucent: bool,
}

/// One provider of a font, in priority order: a bitmap or space provider with its glyphs, or one whose glyphs are not
/// decoded here (TrueType, unihex, vanilla's own fonts), compared whole; `filter` the options it needs.
#[derive(Clone)]
struct FontEntry {
    glyphs: Rc<HashMap<i32, Desc>>,
    opaque: Option<Desc>,
    filter: Desc,
}

impl FontEntry {
    fn describe(&self, glyph: Option<&Desc>) -> Desc {
        let mut out = Desc::object();
        match glyph {
            Some(glyph) => out.set("glyph", glyph.clone()),
            None => out.set("provider", self.opaque.clone().unwrap_or(Desc::Null)),
        };
        out.set("filter", self.filter.clone());
        out
    }
}

fn filter_is_empty(filter: &Desc) -> bool {
    match filter {
        Desc::Object(members) => members.is_empty(),
        Desc::Array(items) => items.is_empty(),
        _ => true,
    }
}

/// A texture's `.mcmeta`: absent, refused by the client's grammar, or its tree with its bytes.
enum Metadata {
    Absent,
    Unparsable(Digest),
    Parsed(Arc<Node>, Blob, Digest),
}

/// What `SpriteResourceLoader.loadSprite` makes of one texture.
enum Loaded {
    Texture(Texture),
    /// Rejected, so the atlas has no such sprite; or metadata whose outcome the oracle does not model, described by
    /// all of it and every pixel.
    Whole(Desc),
}

fn rejected() -> Desc {
    let mut out = Desc::object();
    out.set("rejected", Desc::Bool(true));
    out
}

/// Why a metadata section has no modelled value: the client's codec certainly fails on it, or the oracle does not
/// know what the codec makes of it.
enum Verdict {
    Rejected,
    Unknown,
}

/// `AnimationMetadataSection.CODEC` decoded through `JsonOps`, for the JSON whose outcome is certain: integers in
/// range, booleans, the documented shapes.
struct Animation {
    frames: Option<Vec<(i32, Option<i32>)>>,
    width: Option<i32>,
    height: Option<i32>,
    frametime: i32,
    interpolate: bool,
}

impl Animation {
    fn read(section: &Node) -> Result<Self, Verdict> {
        if !section.is_object() || !unique_names(section) {
            return Err(Verdict::Unknown);
        }
        let frames = match section.get("frames") {
            None => None,
            Some(list) if list.is_array() => {
                Some(list.children().map(frame).collect::<Result<_, _>>()?)
            }
            Some(_) => return Err(Verdict::Unknown),
        };
        let interpolate = match section.get("interpolate").map(|node| &node.value) {
            None => false,
            Some(Value::Bool(value)) => *value,
            Some(_) => return Err(Verdict::Unknown),
        };
        Ok(Self {
            frames,
            width: int_field(section, "width", 1)?,
            height: int_field(section, "height", 1)?,
            frametime: int_field(section, "frametime", 1)?.unwrap_or(1),
            interpolate,
        })
    }
}

/// `AnimationFrame.CODEC`: a `NON_NEGATIVE_INT` index, or `{index, time}` with a `POSITIVE_INT` time.
fn frame(node: &Node) -> Result<(i32, Option<i32>), Verdict> {
    match &node.value {
        Value::Int(index) if *index >= 0 => Ok((*index, None)),
        Value::Int(_) => Err(Verdict::Rejected),
        Value::Object(_) if unique_names(node) => Ok((
            int_field(node, "index", 0)?.ok_or(Verdict::Rejected)?,
            int_field(node, "time", 1)?,
        )),
        _ => Err(Verdict::Unknown),
    }
}

/// An optional `int` field at least `min`: below it the codec fails.
fn int_field(section: &Node, name: &str, min: i32) -> Result<Option<i32>, Verdict> {
    match section.get(name).map(|node| &node.value) {
        None => Ok(None),
        Some(Value::Int(value)) if *value >= min => Ok(Some(*value)),
        Some(Value::Int(_)) => Err(Verdict::Rejected),
        Some(_) => Err(Verdict::Unknown),
    }
}

/// Every number an `int` literal, which `JsonOps` reads as written: [`canonical`]'s doubles merge numbers the client's
/// codecs tell apart (an `int` past 2^53, a decimal that rounds to an integer, two floats under one double).
fn exact_numbers(node: &Node) -> bool {
    match &node.value {
        Value::Long(_) | Value::Big(_) | Value::Double(_) => false,
        _ => node.children().all(exact_numbers),
    }
}

/// Gson keeps the last of a repeated name where [`Node::get`] finds the first.
fn unique_names(node: &Node) -> bool {
    let Value::Object(members) = &node.value else {
        return true;
    };
    let mut names = HashSet::new();
    members.iter().all(|member| names.insert(&*member.name))
}

/// A decoded texture with its animation: frames as `(index, time)` over a grid of frame-sized cells.
struct Texture {
    digest: Digest,
    image: Arc<Image>,
    frame_width: i32,
    frame_height: i32,
    frames: Vec<(i32, i32)>,
    interpolate: bool,
    meta: Desc,
}

impl Texture {
    /// The digest of each frame in animation order with its time; consecutive equal frames merge when not
    /// interpolated, and a texture whose frames are all equal is one digest.
    fn frames(&self, digest: &dyn Fn(i32, i32) -> Arc<str>) -> Desc {
        let columns = (self.image.width / self.frame_width).max(1);
        let mut digests: Vec<Arc<str>> = Vec::new();
        let mut times: Vec<i32> = Vec::new();
        for &(index, time) in &self.frames {
            let d = digest(
                (index % columns).wrapping_mul(self.frame_width),
                (index / columns).wrapping_mul(self.frame_height),
            );
            if !self.interpolate && digests.last() == Some(&d) {
                let last = times.last_mut().expect("as many times as digests");
                *last = last.wrapping_add(time);
            } else {
                digests.push(d);
                times.push(time);
            }
        }
        if digests.iter().all(|d| *d == digests[0]) {
            return Desc::str(&*digests[0]);
        }
        let frames = digests
            .iter()
            .zip(&times)
            .map(|(d, &time)| {
                let mut frame = Desc::object();
                frame
                    .set("texels", Desc::str(&**d))
                    .set("time", Desc::Int(time.into()));
                frame
            })
            .collect();
        let mut out = Desc::object();
        out.set("interpolate", Desc::Bool(self.interpolate))
            .set("frames", Desc::Array(frames));
        out
    }
}

/// What a model reference was resolved from, to resolve it again where its key differs.
type From = (String, Atlas, Lock);

type Models = HashMap<(ResourceId, Atlas, Lock), (Desc, Option<Trace>)>;
type Fonts = HashMap<String, (Rc<[FontEntry]>, Option<Trace>)>;

pub(crate) struct Resolver<'r> {
    ctx: &'r Ctx<'r>,
    side: &'r Side<'r>,
    state: &'r ViewState,
    models_stitch_their_textures: bool,
    /// Each model with what resolving it asked, when recording.
    models: RefCell<Models>,
    resolved_from: RefCell<HashMap<String, From>>,
    /// Each font with what resolving it asked; `None` while it resolves, or when not recording.
    fonts: RefCell<Fonts>,
    /// Where every query this resolver makes of its view is written down, for the cross-run memo.
    recorder: Option<&'r Recorder>,
    listing: OnceLock<Answer>,
    sprites: OnceLock<Answer>,
}

impl<'r> Resolver<'r> {
    pub(crate) fn new(ctx: &'r Ctx<'r>, side: &'r Side<'r>, view: usize) -> Self {
        let state = &side.views[view];
        Self {
            ctx,
            side,
            state,
            models_stitch_their_textures: state.format.0 < ATLAS_FILES,
            models: RefCell::default(),
            resolved_from: RefCell::default(),
            fonts: RefCell::default(),
            recorder: None,
            listing: OnceLock::new(),
            sprites: OnceLock::new(),
        }
    }

    /// A resolver that writes every query it makes, with its answer, to `recorder`.
    pub(crate) fn recording(
        ctx: &'r Ctx<'r>,
        side: &'r Side<'r>,
        view: usize,
        recorder: &'r Recorder,
    ) -> Self {
        Self {
            recorder: Some(recorder),
            ..Self::new(ctx, side, view)
        }
    }

    fn note(&self, query: impl FnOnce() -> Query, answer: impl FnOnce() -> Answer) {
        if let Some(recorder) = self.recorder {
            recorder.fact(query(), answer());
        }
    }

    /// The answer `query` has in this view now: what the memo checks a recorded answer against.
    pub(crate) fn answer(&self, query: &Query) -> Answer {
        match query {
            Query::File(logical) => self.file_answer(self.lookup_physical(logical)),
            Query::Vanilla(logical) => Answer::of(self.side.has_vanilla(self.ctx, logical)),
            Query::VanillaFile(logical) => Answer::digest(self.ctx.vanilla_file(logical)),
            Query::Named(logical) => Answer::of(self.ctx.named(logical)),
            Query::Sprite(atlas, sprite) => match ResourceId::parse(self.interner(), sprite) {
                Some(id) => {
                    sprite_answer(self.side.atlases(self.ctx, self.state).sprite(atlas, id))
                }
                None => Answer::Absent,
            },
            Query::Root(path) => root_answer(self.root_entry(path)),
            Query::Listing => self.listing(),
            Query::Sprites => self.all_sprites(),
        }
    }

    /// `answer(query) == *answer`, without building the answer of a file lookup, which most facts are.
    pub(crate) fn holds(&self, query: &Query, answer: &Answer) -> bool {
        let Query::File(logical) = query else {
            return self.answer(query) == *answer;
        };
        match (self.lookup_physical(logical), answer) {
            (None, Answer::Absent) => true,
            (
                Some(path),
                Answer::File {
                    physical,
                    content,
                    kind,
                },
            ) => {
                **physical == *path.as_str()
                    && self.side.pack.files.get(path).map(digest_of) == *content
                    && self.side.layout.kinds.get(path) == kind.as_ref()
            }
            _ => false,
        }
    }

    /// The content digest of the pack's file at `physical`, whatever the view resolves: what a memo keyed on the
    /// files a resolution read checks.
    #[cfg(test)]
    pub(crate) fn content(&self, physical: &str) -> Option<Digest> {
        let path = PhysPath::new(physical).ok()?;
        self.side.pack.files.get(&path).map(digest_of)
    }

    fn file_answer(&self, physical: Option<&PhysPath>) -> Answer {
        match physical {
            None => Answer::Absent,
            Some(path) => Answer::File {
                physical: path.as_str().into(),
                content: self.side.pack.files.get(path).map(digest_of),
                kind: self.side.layout.kinds.get(path).copied(),
            },
        }
    }

    /// Every file of the view with its layer and kind: what key enumeration walks.
    fn listing(&self) -> Answer {
        self.listing
            .get_or_init(|| {
                let mut hasher = blake3::Hasher::new();
                for (logical, physical) in &self.view().files {
                    hasher.update(logical.as_str().as_bytes());
                    hasher.update(&[0]);
                    hasher.update(physical.as_str().as_bytes());
                    hasher.update(&[0]);
                    let kind = self
                        .side
                        .layout
                        .kinds
                        .get(physical)
                        .map_or(u8::MAX, |kind| *kind as u8);
                    hasher.update(&[kind]);
                }
                Answer::Digest(canon::truncate(hasher.finalize()))
            })
            .clone()
    }

    /// Every sprite of every atlas of the view: what key enumeration walks.
    fn all_sprites(&self) -> Answer {
        self.sprites
            .get_or_init(|| {
                let atlases = self.side.atlases(self.ctx, self.state);
                let mut hasher = blake3::Hasher::new();
                for atlas in atlases.atlas_ids() {
                    hasher.update(atlas.as_bytes());
                    hasher.update(&[0]);
                    for (id, sprite) in atlases.sprites(atlas, self.interner()) {
                        hasher.update(id.as_bytes());
                        hasher.update(&[0]);
                        hash_sprite(&mut hasher, sprite);
                    }
                }
                Answer::Digest(canon::truncate(hasher.finalize()))
            })
            .clone()
    }

    fn named(&self, logical: &str) -> bool {
        let named = self.ctx.named(logical);
        self.note(|| Query::Named(logical.into()), || Answer::of(named));
        named
    }

    fn atlas_sprite(&self, atlas: &str, id: ResourceId) -> Option<&'r Sprite> {
        let sprite = self.side.atlases(self.ctx, self.state).sprite(atlas, id);
        self.note(
            || Query::Sprite(atlas.into(), id.display(self.interner()).to_string().into()),
            || sprite_answer(sprite),
        );
        sprite
    }

    fn interner(&self) -> &'r Interner {
        &self.ctx.cx.interner
    }

    fn view(&self) -> &View {
        &self.state.view
    }

    /// The model a reference [`Self::resolve`] returned stands for, resolved again.
    pub(crate) fn model_of(&self, digest: &str) -> Desc {
        let from = self.resolved_from.borrow().get(digest).cloned();
        match from {
            Some((logical, atlas, lock)) => self.model(&logical, atlas, lock),
            None => Desc::Null,
        }
    }

    /// The keys the pack contributes in this view: what it defines or overrides.
    pub(crate) fn keys(&self) -> BTreeSet<String> {
        let mut keys = BTreeSet::new();
        self.note(|| Query::Listing, || self.listing());
        for (logical, physical) in &self.view().files {
            let logical = logical.as_str();
            let Some(path) = ResourcePath::of(logical) else {
                continue;
            };
            let id = |suffix: &str| {
                let rest = path.rest.strip_suffix(suffix).unwrap_or(path.rest);
                format!("{}:{rest}", path.namespace)
            };
            let kind = self
                .side
                .layout
                .kinds
                .get(physical)
                .copied()
                .unwrap_or(FileKind::Unknown);
            match kind {
                FileKind::ItemDefinition => {
                    keys.insert(format!("item {}", id(".json")));
                }
                FileKind::Blockstate => {
                    keys.insert(format!("blockstate {}", id(".json")));
                }
                FileKind::Equipment => {
                    keys.insert(format!("equipment {}", id(".json")));
                }
                // A model something outside the pack names (a source, the keep-list) is drawn by what the
                // server sends, which no item definition of the pack shows.
                FileKind::Model => {
                    if self.vanilla(logical) || self.named(logical) {
                        keys.insert(format!("model {}", id(".json")));
                    }
                }
                FileKind::Font => {
                    let font = id(".json");
                    for entry in self.font_entries(&font).iter() {
                        for &codepoint in entry.glyphs.keys() {
                            keys.insert(glyph_key(&font, codepoint));
                        }
                    }
                    keys.insert(format!("font {font}"));
                }
                FileKind::SoundsJson => {
                    if let Some(json) = self.json(logical) {
                        for member in json.members() {
                            let text = format!("{}:{}", path.namespace, member.name);
                            if Id::parse(self.interner(), &text).is_some_and(|id| id.text() == text)
                            {
                                keys.insert(format!("sound {text}"));
                            }
                        }
                    }
                }
                FileKind::Lang
                | FileKind::Shader
                | FileKind::ShaderInclude
                | FileKind::Text
                | FileKind::Other
                | FileKind::PostEffect
                | FileKind::Particle
                | FileKind::WaypointStyle => {
                    keys.insert(format!("file {logical}"));
                }
                // Read by path, not only through what the pack draws: an entity texture, a vanilla font's sheet.
                FileKind::Texture | FileKind::Sound | FileKind::FontFile => {
                    if self.vanilla(logical) || self.named(logical) {
                        keys.insert(format!("file {logical}"));
                    }
                }
                FileKind::TextureMeta => {
                    let texture = &logical[..logical.len() - ".mcmeta".len()];
                    if self.vanilla(texture) || self.named(texture) {
                        keys.insert(format!("file {texture}"));
                    }
                }
                _ => {}
            }
        }
        let atlases = self.side.atlases(self.ctx, self.state);
        self.note(|| Query::Sprites, || self.all_sprites());
        for atlas in atlases.atlas_ids() {
            for (id, sprite) in atlases.sprites(atlas, &self.ctx.cx.interner) {
                let pack = sprite.origin.is_some()
                    || sprite
                        .inputs
                        .iter()
                        .any(|input| self.physical(&input.logical).is_some());
                if pack && sprite.inputs.iter().any(|input| self.named(&input.logical)) {
                    keys.insert(format!("sprite {atlas} {id}"));
                }
            }
        }
        // The pack list shows the root's icon and description, whatever the view.
        if self.root(PACK_ICON).is_some() {
            keys.insert(PACK_ICON_KEY.into());
        }
        if self.root(PACK_META).is_some() {
            keys.insert(PACK_DESCRIPTION_KEY.into());
        }
        keys
    }

    /// The descriptor of `key` in this view, pack over vanilla; `None` when the view has nothing for it.
    pub(crate) fn resolve(&self, key: &str) -> Option<Desc> {
        let mut parts = key.splitn(3, ' ');
        let kind = parts.next()?;
        let first = parts.next().unwrap_or("");
        let second = parts.next().unwrap_or("");
        match kind {
            "item" => self
                .json(&Id::parse(self.interner(), first)?.file("items", ".json"))
                .map(|json| self.item(&json)),
            "blockstate" => self
                .json(&Id::parse(self.interner(), first)?.file("blockstates", ".json"))
                .map(|json| self.blockstate(&json)),
            "equipment" => self
                .json(&Id::parse(self.interner(), first)?.file("equipment", ".json"))
                .map(|json| self.equipment(&json)),
            "model" => {
                let logical = Id::parse(self.interner(), first)?.file("models", ".json");
                self.exists(&logical).then(|| self.legacy_model(&logical))
            }
            "font" => {
                let entries = self.font_entries(first);
                let opaque = entries
                    .iter()
                    .filter(|entry| entry.opaque.is_some())
                    .map(|entry| entry.describe(None))
                    .collect();
                (!entries.is_empty()).then_some(Desc::Array(opaque))
            }
            "glyph" => {
                let codepoint = i32::from_str_radix(second.get(2..)?, 16).ok()?;
                self.glyph(first, codepoint)
            }
            "sound" => self.sound(&Id::parse(self.interner(), first)?),
            "sprite" => {
                let id = ResourceId::parse(&self.ctx.cx.interner, second)?;
                self.atlas_sprite(first, id)
                    .map(|sprite| self.whole_sprite(sprite))
            }
            "file" => self.file_key(&key["file ".len()..]),
            "pack" => match key {
                PACK_ICON_KEY => self.pack_icon(),
                PACK_DESCRIPTION_KEY => self.pack_description(),
                _ => None,
            },
            _ => None,
        }
    }

    // ---- item definitions, block states, equipment

    fn item(&self, node: &Node) -> Desc {
        match &node.value {
            Value::Object(members) => {
                let kind = id::source_type(self.interner(), node);
                let mut out = Desc::object();
                for (key, value) in sorted(members) {
                    let resolved = match (kind.as_str(), key, value.as_str()) {
                        ("model", "model", Some(model)) | ("special", "base", Some(model)) => {
                            self.model_ref(model, Atlas::Items, Lock::default())
                        }
                        ("special", "model", _) => self.special(value),
                        (_, "type" | "property", Some(text)) => {
                            Desc::str(normalized_id(self.interner(), text))
                        }
                        _ => self.item(value),
                    };
                    out.set(key.to_owned(), resolved);
                }
                out
            }
            Value::Array(items) => Desc::Array(items.iter().map(|item| self.item(item)).collect()),
            _ => canonical(node),
        }
    }

    fn special(&self, special: &Node) -> Desc {
        let mut out = canonical_with(special, &|value| normalized_id(self.interner(), value));
        let kind = match special.path("type").as_str() {
            Some(kind) => id::normalized_type(self.interner(), kind),
            None => String::new(),
        };
        let Some(texture) = special.path("texture").as_str() else {
            return out;
        };
        // Looked up by the type without its namespace while client-rules.json keys carry it: a `minecraft:` type
        // finds no rule and its texture compares as written.
        let Some(rule) = Id::parse(self.interner(), texture).and_then(|id| {
            self.ctx
                .rules
                .special_textures
                .get(&kind)
                .map(|rule| (id, rule))
        }) else {
            return out;
        };
        let resolved = match rule {
            (id, TextureRule::Sprite(sprite)) => {
                let atlases: Vec<&str> = sprite.atlases.iter().map(String::as_str).collect();
                Id::parse(
                    self.interner(),
                    &format!("{}:{}", id.ns, sprite.of(id.path)),
                )
                .and_then(|target| self.lookup(&target, &atlases))
                .map_or_else(missing, |sprite| self.whole_sprite(&sprite))
            }
            (id, TextureRule::File(file)) => self
                .image(&file.file(id.ns, id.path))
                .unwrap_or_else(missing),
        };
        out.set("texture", resolved);
        out
    }

    fn blockstate(&self, node: &Node) -> Desc {
        match &node.value {
            Value::Object(members) => {
                let mut out = Desc::object();
                for (key, value) in sorted(members) {
                    let resolved = match value.as_str() {
                        Some(model) if key == "model" => {
                            self.model_ref(model, Atlas::Blocks, Lock::of(node))
                        }
                        _ => self.blockstate(value),
                    };
                    out.set(key.to_owned(), resolved);
                }
                out
            }
            Value::Array(items) => {
                Desc::Array(items.iter().map(|item| self.blockstate(item)).collect())
            }
            _ => canonical(node),
        }
    }

    fn equipment(&self, equipment: &Node) -> Desc {
        let mut out = canonical(equipment);
        let mut layers = Desc::object();
        let mut names: Vec<&str> = equipment
            .path("layers")
            .members()
            .iter()
            .map(|member| &*member.name)
            .collect();
        names.sort_by(|a, b| java_order(a, b));
        let rule = &self.ctx.rules.equipment_textures;
        for layer in names {
            let mut entries = Desc::Array(Vec::new());
            for entry in equipment.path("layers").path(layer).children() {
                let mut resolved = canonical(entry);
                if let Some(texture) = entry.path("texture").as_str() {
                    let image = Id::parse(self.interner(), texture).and_then(|id| {
                        let folder = format!("{}/{layer}", rule.folder);
                        self.image(&id.file(&folder, &rule.suffix))
                    });
                    resolved.set("texture", image.unwrap_or_else(missing));
                }
                entries.push(resolved);
            }
            layers.set(layer.to_owned(), entries);
        }
        if equipment.has("layers") {
            out.set("layers", layers);
        }
        out
    }

    // ---- models

    /// A resolved model, kept by digest so items sharing it compare it once.
    fn model_ref(&self, value: &str, atlas: Atlas, lock: Lock) -> Desc {
        let Some(id) = Id::parse(self.interner(), value) else {
            return Desc::str(format!("invalid model id {value}"));
        };
        let key = (id.id, atlas, lock);
        if let Some((found, trace)) = self.models.borrow().get(&key) {
            if let (Some(recorder), Some(trace)) = (self.recorder, trace) {
                recorder.reuse(trace);
            }
            return found.clone();
        }
        let logical = id.file("models", ".json");
        if let Some(recorder) = self.recorder {
            recorder.begin();
        }
        let model = self.model(&logical, atlas, lock);
        let trace = self.recorder.map(|recorder| {
            let trace = recorder.end();
            recorder.reuse(&trace);
            trace
        });
        let digest = canon::encode(&canon::digest(&model));
        self.resolved_from
            .borrow_mut()
            .entry(digest.clone())
            .or_insert((logical, atlas, lock));
        let reference = canon::reference(&digest);
        self.models
            .borrow_mut()
            .insert(key, (reference.clone(), trace));
        reference
    }

    /// A model the vanilla client also has: what clients before item definitions reach, overrides included.
    fn legacy_model(&self, logical: &str) -> Desc {
        let mut out = Desc::object();
        let id = ResourcePath::of(logical).map_or_else(String::new, |path| {
            format!(
                "{}:{}",
                path.namespace,
                path.rest.strip_suffix(".json").unwrap_or(path.rest)
            )
        });
        out.set("model", self.model_ref(&id, Atlas::Items, Lock::default()));
        let mut overrides = Desc::Array(Vec::new());
        if let Some(json) = self.json(logical) {
            for over in json.path("overrides").children() {
                let mut resolved = Desc::object();
                resolved.set("predicate", canonical(over.path("predicate")));
                if let Some(model) = over.path("model").as_str() {
                    resolved.set(
                        "model",
                        self.model_ref(model, Atlas::Items, Lock::default()),
                    );
                }
                overrides.push(resolved);
            }
        }
        out.set("overrides", overrides);
        out
    }

    /// `ResolvedModel`: the parent chain, and what the top of it finds first.
    fn model(&self, logical: &str, atlas: Atlas, lock: Lock) -> Desc {
        let mut chain: Vec<Arc<Node>> = Vec::new();
        let mut terminal = String::new();
        let mut seen = HashSet::new();
        let mut current = Some(logical.to_owned());
        while let Some(path) = current.take() {
            let json = match self.json(&path) {
                Some(json) if json.is_object() => json,
                _ => {
                    terminal = "missing".into();
                    break;
                }
            };
            if let Some(parent) = json
                .path("parent")
                .as_str()
                .filter(|parent| !parent.is_empty())
            {
                match Id::parse(self.interner(), parent) {
                    None => terminal = "missing".into(),
                    Some(id)
                        if id.ns == DEFAULT_NAMESPACE
                            && id.path.starts_with(&*self.ctx.rules.builtin_model_prefix) =>
                    {
                        terminal = id.path.to_owned();
                    }
                    Some(id) if !seen.insert(id.text()) => terminal = "loop".into(),
                    Some(id) => current = Some(id.file("models", ".json")),
                }
            }
            chain.push(json);
        }
        let generated = terminal == "builtin/generated";
        let is_missing = terminal == "missing";
        let mut out = Desc::object();
        out.set("terminal", Desc::str(&*terminal));
        let slots = texture_slots(&chain, generated);
        if let Some(model) = chain.iter().find(|model| model.has("elements")) {
            out.set(
                "elements",
                self.elements(model.path("elements"), &slots, atlas, lock),
            );
        } else if generated {
            let mut layers = Desc::Array(Vec::new());
            for layer in (0..LAYERS).map_while(|i| slots.get(&format!("layer{i}"))) {
                layers.push(self.material(layer, atlas));
            }
            out.set("generated", layers);
        }
        let ambient = chain
            .iter()
            .find_map(|model| match &model.path("ambientocclusion").value {
                Value::Bool(value) => Some(value.to_string()),
                _ => None,
            });
        out.set(
            "ambientocclusion",
            Desc::str(
                ambient.unwrap_or_else(|| if is_missing { "missing" } else { "true" }.into()),
            ),
        );
        let light = chain
            .iter()
            .find_map(|model| model.path("gui_light").as_str().map(str::to_owned));
        let light = light.unwrap_or_else(|| {
            if generated {
                "front"
            } else if is_missing {
                "missing"
            } else {
                "side"
            }
            .into()
        });
        out.set("gui_light", Desc::str(light));
        let mut display = Desc::object();
        for context in DISPLAY {
            let transform = chain
                .iter()
                .find_map(|model| transform(model.path("display"), context))
                .unwrap_or_else(|| Desc::str(if is_missing { "missing" } else { "none" }));
            display.set(context, transform);
        }
        out.set("display", display);
        let particle = match slots.get("particle") {
            Some(material) => self.material(material, atlas),
            None => Desc::str("none"),
        };
        out.set("particle", particle);
        out
    }

    fn elements(
        &self,
        elements: &Node,
        slots: &HashMap<String, Material>,
        atlas: Atlas,
        lock: Lock,
    ) -> Desc {
        let mut out = Desc::Array(Vec::new());
        for element in elements.children() {
            let from = floats(element.path("from"), 3);
            let to = floats(element.path("to"), 3);
            // UnbakedCuboidGeometry.bake: a face lying in a flat element is never baked.
            let x = from[0] != to[0];
            let y = from[1] != to[1];
            let z = from[2] != to[2];
            let mut faces = Desc::object();
            for (index, direction) in DIRECTIONS.iter().enumerate() {
                let face = element.path("faces").path(direction);
                let drawn = match *direction {
                    "west" | "east" => y && z,
                    "down" | "up" => x && z,
                    _ => x && y,
                };
                if face.is_object() && drawn {
                    faces.set(
                        *direction,
                        self.face(index, face, &from, &to, slots, atlas, lock),
                    );
                }
            }
            if faces.is_empty_object() {
                continue;
            }
            let mut resolved = Desc::object();
            resolved
                .set("from", float_array(&from))
                .set("to", float_array(&to))
                .set(
                    "rotation",
                    match element.get("rotation") {
                        Some(rotation) => canonical_with(rotation, &str::to_lowercase),
                        None => Desc::Null,
                    },
                )
                .set("shade", Desc::Bool(element.path("shade").as_bool(true)))
                .set(
                    "light_emission",
                    Desc::Int(element.path("light_emission").as_int(0).into()),
                )
                .set("faces", faces);
            out.push(resolved);
        }
        out
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "a face's geometry, slots, atlas and lock, each from a different level of the model"
    )]
    fn face(
        &self,
        direction: usize,
        face: &Node,
        from: &[f32],
        to: &[f32],
        slots: &HashMap<String, Material>,
        atlas: Atlas,
        lock: Lock,
    ) -> Desc {
        let mut out = Desc::object();
        let cull = face.path("cullface").as_string("");
        out.set(
            "cullface",
            Desc::str(if DIRECTIONS.contains(&&*cull) {
                &*cull
            } else {
                ""
            }),
        );
        out.set(
            "tintindex",
            Desc::Int(face.path("tintindex").as_int(-1).into()),
        );
        let uv = face.path("uv");
        let uvs = if uv.is_array() && uv.size() == 4 {
            floats(uv, 4)
        } else {
            default_uvs(direction, from, to)
        };
        let shift = (face.path("rotation").as_int(0) / 90).rem_euclid(4);
        let slot = face.path("texture").as_string("");
        let slot = slot.strip_prefix('#').unwrap_or(&slot);
        let texture = match slots.get(slot) {
            Some(material) => self.sampled(
                material,
                atlas,
                &uvs,
                shift,
                uvlock::map(lock.x, lock.y, lock.z, direction),
            ),
            None => missing(),
        };
        out.set("texture", texture);
        out
    }

    // ---- sprites and texels

    fn lookup(&self, sprite: &Id<'_>, atlases: &[&str]) -> Option<Sprite> {
        atlases
            .iter()
            .find_map(|atlas| self.atlas_sprite(atlas, sprite.id))
            .cloned()
    }

    fn sprite(&self, material: &Material, atlas: Atlas) -> Option<Sprite> {
        let id = Id::parse(self.interner(), &material.sprite)?;
        if Id::parse(self.interner(), &self.ctx.rules.missing_sprite).as_ref() == Some(&id) {
            return None;
        }
        if !self.models_stitch_their_textures {
            return self.lookup(&id, atlas.ids());
        }
        let texture = id.file("textures", ".png");
        self.exists(&texture).then(|| Sprite {
            inputs: vec![SpriteInput {
                logical: texture.into(),
                reason: "model.texture",
            }],
            origin: None,
            region: "".into(),
        })
    }

    /// A whole sprite, as an item model generated from it reads it.
    fn material(&self, material: &Material, atlas: Atlas) -> Desc {
        let mut out = Desc::object();
        let sprite = self
            .sprite(material, atlas)
            .map_or_else(missing, |sprite| self.whole_sprite(&sprite));
        out.set("sprite", sprite)
            .set("force_translucent", Desc::Bool(material.force_translucent));
        out
    }

    /// What a face draws: the texel block its UVs cover in each frame, and its corners' UVs inside that block.
    /// `lock`: [`uvlock::map`] of the face, which `FaceBakery.bakeVertex` applies after the face rotation.
    fn sampled(
        &self,
        material: &Material,
        atlas: Atlas,
        uvs: &[f32],
        shift: i32,
        lock: [[i32; 2]; 2],
    ) -> Desc {
        let mut out = Desc::object();
        out.set("force_translucent", Desc::Bool(material.force_translucent));
        let sprite = self.sprite(material, atlas);
        let texture = sprite
            .as_ref()
            .and_then(|sprite| self.sprite_texture(sprite));
        let Some(Loaded::Texture(t)) = texture else {
            let whole = sprite.map_or_else(missing, |sprite| self.whole_sprite(&sprite));
            let locked = [lock[0][0], lock[0][1], lock[1][0], lock[1][1]]
                .into_iter()
                .map(|value| Desc::Int(value.into()))
                .collect();
            out.set("sprite", whole)
                .set("uv", float_array(uvs))
                .set("rotation", Desc::Int(shift.into()))
                .set("uvlock", Desc::Array(locked));
            return out;
        };
        let mut xs = [0.0f64; 4];
        let mut ys = [0.0f64; 4];
        for i in 0..4 {
            // CuboidFace.getU/getV: each corner takes the UV of the corner the face rotation shifts it to.
            let corner = (i as i32 + shift) % 4;
            let u = f64::from(if corner == 0 || corner == 1 {
                uvs[0]
            } else {
                uvs[2]
            }) - 8.0;
            let v = f64::from(if corner == 0 || corner == 3 {
                uvs[1]
            } else {
                uvs[3]
            }) - 8.0;
            xs[i] = (8.0 + f64::from(lock[0][0]) * u + f64::from(lock[0][1]) * v)
                * f64::from(t.frame_width)
                / 16.0;
            ys[i] = (8.0 + f64::from(lock[1][0]) * u + f64::from(lock[1][1]) * v)
                * f64::from(t.frame_height)
                / 16.0;
        }
        let min = |values: &[f64; 4]| values[0].min(values[1]).min(values[2].min(values[3]));
        let max = |values: &[f64; 4]| values[0].max(values[1]).max(values[2].max(values[3]));
        let x0 = quantize(min(&xs)).floor() as i32;
        let y0 = quantize(min(&ys)).floor() as i32;
        // Wrapping `i32` arithmetic: a UV far outside the texture must not overflow.
        let x1 = x0.wrapping_add(1).max(quantize(max(&xs)).ceil() as i32);
        let y1 = y0.wrapping_add(1).max(quantize(max(&ys)).ceil() as i32);
        let corners = (0..4)
            .map(|i| {
                Desc::Array(vec![
                    Desc::Double(quantize(xs[i] - f64::from(x0))),
                    Desc::Double(quantize(ys[i] - f64::from(y0))),
                ])
            })
            .collect();
        out.set("corners", Desc::Array(corners));
        let bx0 = x0.clamp(0, t.frame_width);
        let by0 = y0.clamp(0, t.frame_height);
        let bx1 = x1.clamp(0, t.frame_width);
        let by1 = y1.clamp(0, t.frame_height);
        let clipped = [
            bx0.wrapping_sub(x0),
            by0.wrapping_sub(y0),
            x1.wrapping_sub(bx1),
            y1.wrapping_sub(by1),
        ]
        .into_iter()
        .map(|value| Desc::Int(value.into()))
        .collect();
        out.set("clipped", Desc::Array(clipped));
        let shared = &self.ctx.shared;
        let texels = t.frames(&|frame_x, frame_y| {
            shared.block(
                t.digest,
                &t.image,
                frame_x.wrapping_add(bx0),
                frame_y.wrapping_add(by0),
                bx1 - bx0,
                by1 - by0,
            )
        });
        out.set("texels", texels).set("meta", t.meta);
        out
    }

    /// A sprite's whole texture: its frames' pixels in animation order, and its metadata.
    fn whole_sprite(&self, sprite: &Sprite) -> Desc {
        let mut out = Desc::object();
        if let Some(loaded) = self.sprite_texture(sprite) {
            let t = match loaded {
                Loaded::Texture(t) => t,
                Loaded::Whole(whole) => return whole,
            };
            let size = vec![
                Desc::Int(t.frame_width.into()),
                Desc::Int(t.frame_height.into()),
            ];
            let shared = &self.ctx.shared;
            let texels = t.frames(&|x, y| {
                shared.block(t.digest, &t.image, x, y, t.frame_width, t.frame_height)
            });
            out.set("size", Desc::Array(size))
                .set("texels", texels)
                .set("meta", t.meta);
            return out;
        }
        // Built from several textures (palette permutations) or cut by an unstitch source: each input's pixels, all the
        // client reads of it (`LazyLoadedImage` and `PalettedPermutations.loadPaletteEntryFromImage`).
        let inputs = sprite
            .inputs
            .iter()
            .map(|input| {
                let mut described = match self.bytes(&input.logical) {
                    Some((bytes, digest)) => self.pixels(&bytes, digest),
                    None => {
                        let mut missing = Desc::object();
                        missing.set("bytes", Desc::str("missing"));
                        missing
                    }
                };
                described.set("reason", Desc::str(input.reason));
                described
            })
            .collect();
        out.set("inputs", Desc::Array(inputs))
            .set("region", Desc::str(&*sprite.region));
        out
    }

    /// A texture file the client loads by path (`TextureContents.load`, `BitmapProvider.Definition.load`): its size
    /// and every pixel, never cut into animation frames, and its whole `.mcmeta`.
    fn image(&self, logical: &str) -> Option<Desc> {
        let (bytes, digest) = self.bytes(logical)?;
        let mut out = self.pixels(&bytes, digest);
        let meta = match self.metadata(logical) {
            Metadata::Absent => Desc::Null,
            Metadata::Unparsable(digest) => Desc::str(canon::encode(&digest)),
            Metadata::Parsed(json, ..) if exact_numbers(&json) => canonical(&json),
            Metadata::Parsed(_, bytes, digest) => json_tokens(&bytes, digest),
        };
        out.set("meta", meta);
        Some(out)
    }

    fn sprite_texture(&self, sprite: &Sprite) -> Option<Loaded> {
        if sprite.inputs.len() != 1 || !sprite.region.is_empty() {
            return None;
        }
        self.texture(&sprite.inputs[0].logical)
    }

    /// A texture's `.mcmeta` as `Resource.metadata()` reads it, for every texture the client loads with one.
    fn metadata(&self, logical: &str) -> Metadata {
        match self.bytes(&format!("{logical}.mcmeta")) {
            None => Metadata::Absent,
            Some((bytes, digest)) => match self.ctx.shared.json(digest, &bytes) {
                Some(json) => Metadata::Parsed(json, bytes, digest),
                None => Metadata::Unparsable(digest),
            },
        }
    }

    /// A texture's decoded pixels, without its metadata.
    fn decoded(&self, logical: &str) -> Option<(Digest, Arc<Image>)> {
        let (bytes, digest) = self.bytes(logical)?;
        Some((digest, self.ctx.shared.image(digest, &bytes)?))
    }

    /// `SpriteResourceLoader.loadSprite`: the sprite its metadata makes of the texture, or that it rejects it.
    /// `None` when the texture is missing or does not decode.
    fn texture(&self, logical: &str) -> Option<Loaded> {
        let (digest, image) = self.decoded(logical)?;
        let (meta, written) = match self.metadata(logical) {
            Metadata::Absent => (Arc::new(Node::new(Value::Object(Vec::new()))), None),
            Metadata::Parsed(json, bytes, digest) if json.is_object() => {
                (json, Some((bytes, digest)))
            }
            // `GsonHelper.parse` reads a `JsonObject` or throws.
            Metadata::Parsed(..) => return Some(Loaded::Whole(rejected())),
            Metadata::Unparsable(meta) => {
                // Kept with its digest: two files the grammar refuses are not known to fail alike in Gson.
                let mut out = rejected();
                out.set("metadata", Desc::str(canon::encode(&meta)));
                return Some(Loaded::Whole(out));
            }
        };
        // The sections beside `animation` stay in the key as canonical JSON, whose doubles must not merge numbers.
        let rest_exact = meta
            .children_at()
            .into_iter()
            .all(|(name, node)| matches!(name, Seg::Name("animation")) || exact_numbers(node));
        let animation = if unique_names(&meta) && rest_exact {
            meta.get("animation").map(Animation::read).transpose()
        } else {
            Err(Verdict::Unknown)
        };
        let animation = match animation {
            Ok(animation) => animation,
            Err(Verdict::Rejected) => return Some(Loaded::Whole(rejected())),
            Err(Verdict::Unknown) => {
                let mut out = Desc::object();
                let size = vec![
                    Desc::Int(image.width.into()),
                    Desc::Int(image.height.into()),
                ];
                let texels = self
                    .ctx
                    .shared
                    .block(digest, &image, 0, 0, image.width, image.height);
                out.set("size", Desc::Array(size))
                    .set("texels", Desc::str(&*texels))
                    .set(
                        "meta",
                        written.map_or(Desc::Null, |(bytes, digest)| json_tokens(&bytes, digest)),
                    );
                return Some(Loaded::Whole(out));
            }
        };
        let mut frame_width = image.width;
        let mut frame_height = image.height;
        let mut frames = vec![(0, 1)];
        let mut interpolate = false;
        if let Some(animation) = animation {
            (frame_width, frame_height) = match (animation.width, animation.height) {
                (Some(width), Some(height)) => (width, height),
                (Some(width), None) => (width, image.height),
                (None, Some(height)) => (image.width, height),
                (None, None) => (image.width.min(image.height), image.width.min(image.height)),
            };
            if image.width % frame_width != 0 || image.height % frame_height != 0 {
                return Some(Loaded::Whole(rejected()));
            }
            let count = (image.width / frame_width).wrapping_mul(image.height / frame_height);
            let time = animation.frametime;
            frames = match animation.frames {
                None => (0..count).map(|index| (index, time)).collect(),
                // `SpriteContents.createAnimatedTexture` drops the frames past the strip.
                Some(listed) => listed
                    .into_iter()
                    .filter(|&(index, _)| index < count)
                    .map(|(index, own)| (index, own.unwrap_or(time)))
                    .collect(),
            };
            if frames.len() < 2 {
                // No animation from fewer than two frames: the sprite is the strip's first cell, whatever cell the
                // list names.
                frames = vec![(0, 1)];
            }
            interpolate = animation.interpolate;
        }
        let mut rest = canonical(&meta);
        rest.remove("animation");
        Some(Loaded::Texture(Texture {
            digest,
            image,
            frame_width,
            frame_height,
            frames,
            interpolate,
            meta: rest,
        }))
    }

    // ---- fonts

    /// `FontManager`: the pack's providers first, then vanilla's for the same id; references inlined.
    fn font_entries(&self, font: &str) -> Rc<[FontEntry]> {
        if let Some((cached, trace)) = self.fonts.borrow().get(font) {
            if let Some(recorder) = self.recorder {
                match trace {
                    Some(trace) => recorder.reuse(trace),
                    // A reference cycle: what this font resolves to depends on which font of the cycle came first.
                    None => recorder.poison(),
                }
            }
            return Rc::clone(cached);
        }
        self.fonts
            .borrow_mut()
            .insert(font.to_owned(), (Rc::default(), None));
        if let Some(recorder) = self.recorder {
            recorder.begin();
        }
        let mut entries = Vec::new();
        let logical = Id::parse(self.interner(), font).map(|id| id.file("font", ".json"));
        if let Some(logical) = &logical
            && self.physical(logical).is_some()
            && let Some(json) = self.json(logical)
        {
            for provider in json.path("providers").children() {
                let filter = provider.path("filter");
                let filter = if filter.is_object() {
                    canonical(filter)
                } else {
                    Desc::object()
                };
                match id::source_type(self.interner(), provider).as_str() {
                    "bitmap" => entries.push(FontEntry {
                        glyphs: Rc::new(self.bitmap(provider)),
                        opaque: None,
                        filter,
                    }),
                    "space" => {
                        let mut glyphs = HashMap::new();
                        for member in provider.path("advances").members() {
                            if let Some(codepoint) = member.name.chars().next() {
                                let mut glyph = Desc::object();
                                glyph.set("space", Desc::Float(member.node.float_value()));
                                glyphs.insert(codepoint as i32, glyph);
                            }
                        }
                        entries.push(FontEntry {
                            glyphs: Rc::new(glyphs),
                            opaque: None,
                            filter,
                        });
                    }
                    "reference" => {
                        if let Some(id) =
                            Id::parse(self.interner(), &provider.path("id").as_string(""))
                        {
                            for entry in self.font_entries(&id.text()).iter() {
                                let filter = if filter_is_empty(&filter) {
                                    entry.filter.clone()
                                } else {
                                    Desc::Array(vec![filter.clone(), entry.filter.clone()])
                                };
                                entries.push(FontEntry {
                                    glyphs: Rc::clone(&entry.glyphs),
                                    opaque: entry.opaque.clone(),
                                    filter,
                                });
                            }
                        }
                    }
                    _ => entries.push(FontEntry {
                        glyphs: Rc::default(),
                        opaque: Some(self.opaque_provider(provider)),
                        filter,
                    }),
                }
            }
        }
        // Vanilla's providers name only vanilla paths, each of which the pack's copy makes a key of its own.
        if logical
            .as_deref()
            .is_some_and(|logical| self.vanilla(logical))
        {
            entries.push(FontEntry {
                glyphs: Rc::default(),
                opaque: Some(Desc::str(format!("vanilla font {font}"))),
                filter: Desc::object(),
            });
        }
        let entries: Rc<[FontEntry]> = Rc::from(entries);
        let trace = self.recorder.map(|recorder| {
            let trace = recorder.end();
            recorder.reuse(&trace);
            trace
        });
        self.fonts
            .borrow_mut()
            .insert(font.to_owned(), (Rc::clone(&entries), trace));
        entries
    }

    /// `BitmapProvider.Definition.load`: cells of the sheet, advance from the last column with alpha.
    fn bitmap(&self, provider: &Node) -> HashMap<i32, Desc> {
        let mut glyphs = HashMap::new();
        // The sheet's pixels only: `BitmapProvider.Definition.load` reads no `.mcmeta`.
        let sheet = Id::parse(self.interner(), &provider.path("file").as_string(""))
            .and_then(|id| self.decoded(&id.file("textures", "")));
        let rows: Vec<Vec<i32>> = provider
            .path("chars")
            .children()
            .map(|row| row.as_string("").chars().map(|c| c as i32).collect())
            .collect();
        let Some((digest, image)) = sheet else {
            return glyphs;
        };
        if rows.is_empty() || rows[0].is_empty() {
            return glyphs;
        }
        let image = &*image;
        let height = provider.path("height").as_int(8);
        let ascent = provider.path("ascent").as_int(0);
        let cell_width = image.width / rows[0].len() as i32;
        let cell_height = image.height / rows.len() as i32;
        let pixel_scale = height as f32 / cell_height as f32;
        for (row, codepoints) in rows.iter().enumerate() {
            for (column, &codepoint) in codepoints.iter().enumerate() {
                if codepoint == 0 {
                    continue;
                }
                let (x, y) = (column as i32 * cell_width, row as i32 * cell_height);
                let actual = actual_width(image, x, y, cell_width, cell_height);
                let mut glyph = Desc::object();
                let cell = self
                    .ctx
                    .shared
                    .block(digest, image, x, y, cell_width, cell_height);
                let advance =
                    ((0.5 + f64::from(actual as f32 * pixel_scale)) as i32).wrapping_add(1);
                glyph
                    .set("cell", Desc::str(&*cell))
                    .set("height", Desc::Int(height.into()))
                    .set("ascent", Desc::Int(ascent.into()))
                    .set("advance", Desc::Int(advance.into()));
                glyphs.insert(codepoint, glyph);
            }
        }
        glyphs
    }

    fn opaque_provider(&self, provider: &Node) -> Desc {
        let mut out = canonical_with(provider, &|value| normalized_id(self.interner(), value));
        out.remove("filter");
        for field in ["file", "hex_file"] {
            if let Some(file) = provider.path(field).as_str() {
                let folder = if id::source_type(self.interner(), provider) == "ttf" {
                    "font"
                } else {
                    ""
                };
                let digest = Id::parse(self.interner(), file)
                    .and_then(|id| self.bytes(&id.file(folder, "")))
                    .map_or_else(
                        || "missing".to_owned(),
                        |(_, digest)| canon::encode(&digest),
                    );
                out.set(field, Desc::str(digest));
            }
        }
        out
    }

    /// `FontSet`: the providers that may draw `codepoint`, in order, up to the first that always does.
    fn glyph(&self, font: &str, codepoint: i32) -> Option<Desc> {
        let entries = self.font_entries(font);
        let mut out = Vec::new();
        let mut any = false;
        for entry in entries.iter() {
            if entry.opaque.is_some() {
                out.push(entry.describe(None));
            } else if let Some(glyph) = entry.glyphs.get(&codepoint) {
                any = true;
                out.push(entry.describe(Some(glyph)));
                if filter_is_empty(&entry.filter) {
                    break;
                }
            }
        }
        any.then_some(Desc::Array(out))
    }

    // ---- sounds and whole files

    fn sound(&self, event: &Id<'_>) -> Option<Desc> {
        let json = self.json(&format!("assets/{}/sounds.json", event.ns))?;
        let definition = json
            .get(event.path)
            .filter(|definition| definition.is_object())?;
        let mut out = Desc::object();
        out.set(
            "replace",
            Desc::Bool(definition.path("replace").as_bool(false)),
        );
        out.set(
            "subtitle",
            definition.get("subtitle").map_or(Desc::Null, canonical),
        );
        let mut sounds = Desc::Array(Vec::new());
        for sound in definition.path("sounds").children() {
            let wrapped;
            let entry = if sound.is_object() {
                sound
            } else {
                wrapped = Node::new(Value::Object(vec![Member::new(
                    "name",
                    Node::new(Value::String(sound.as_string("").into())),
                )]));
                &wrapped
            };
            let kind = entry.path("type").as_string("file");
            let name = entry.path("name").as_string("");
            let mut resolved = Desc::object();
            resolved.set("type", Desc::str(&*kind));
            let shown = match Id::parse(self.interner(), &name) {
                None => Desc::str(format!("invalid {name}")),
                Some(id) if kind == "event" => Desc::str(id.text()),
                Some(id) => {
                    let logical = id.file("sounds", ".ogg");
                    match self.physical(&logical) {
                        Some(physical) => self.side.pack.files.get(physical).map_or_else(
                            || Desc::str(""),
                            |entry| self.heard(&entry.blob, digest_of(entry)),
                        ),
                        None if self.vanilla(&logical) || !self.ctx.vanilla.sound_files => {
                            Desc::str(format!("vanilla {logical}"))
                        }
                        None => Desc::str(format!("missing {logical}")),
                    }
                }
            };
            // SoundEventRegistration defaults: what an entry leaves out is the same as what it writes as default.
            resolved
                .set("name", shown)
                .set("volume", Desc::Double(entry.path("volume").as_double(1.0)))
                .set("pitch", Desc::Double(entry.path("pitch").as_double(1.0)))
                .set("weight", Desc::Int(entry.path("weight").as_int(1).into()))
                .set("stream", Desc::Bool(entry.path("stream").as_bool(false)))
                .set("preload", Desc::Bool(entry.path("preload").as_bool(false)))
                .set(
                    "attenuation_distance",
                    Desc::Int(entry.path("attenuation_distance").as_int(16).into()),
                );
            sounds.push(resolved);
        }
        out.set("sounds", sounds);
        Some(out)
    }

    /// A language file as `Language.loadFromJson` loads it: the strings of its members in file order, up to the first
    /// value the client rejects, which stops the load and keeps what came before. The bytes' digest when the file
    /// does not parse as a JSON object.
    fn translations(&self, logical: &str) -> Option<Desc> {
        let (bytes, digest) = self.bytes(logical)?;
        let json = self.ctx.shared.json_lenient(digest, &bytes);
        let Some(Value::Object(members)) = json.as_deref().map(|node| &node.value) else {
            return Some(Desc::str(canon::encode(&digest)));
        };
        let mut loaded: Vec<_> = members
            .iter()
            .map_while(|member| {
                let text = translation(&member.node, &bytes)?;
                Some((Key::Owned(member.name.to_string()), Desc::str(text)))
            })
            .collect();
        loaded.sort_by(|a, b| java_order(&a.0, &b.0));
        Some(Desc::Object(loaded))
    }

    /// A sound file as the client plays it, streamed or loaded whole: its rate, channels and decoded samples; the
    /// bytes' digest when the client's decoder cannot read it.
    fn heard(&self, bytes: &[u8], digest: Digest) -> Desc {
        let mut out = Desc::object();
        match self.ctx.shared.sound(digest, bytes) {
            Some(sound) => {
                out.set("rate", Desc::Int(sound.rate.into()))
                    .set("channels", Desc::Int(sound.channels as i64))
                    .set("frames", Desc::Int(sound.frames as i64))
                    .set("samples", Desc::str(&*sound.samples));
            }
            None => {
                out.set("bytes", Desc::str(canon::encode(&digest)));
            }
        }
        out
    }

    /// The decoded size and texels, as the client's decoder reads them; the bytes' digest when it cannot.
    fn pixels(&self, bytes: &[u8], digest: Digest) -> Desc {
        let mut out = Desc::object();
        match self.ctx.shared.image(digest, bytes) {
            Some(image) => {
                let size = vec![
                    Desc::Int(image.width.into()),
                    Desc::Int(image.height.into()),
                ];
                let texels = self
                    .ctx
                    .shared
                    .block(digest, &image, 0, 0, image.width, image.height);
                out.set("size", Desc::Array(size))
                    .set("texels", Desc::str(&*texels));
            }
            None => {
                out.set("bytes", Desc::str(canon::encode(&digest)));
            }
        }
        out
    }

    /// A file at the pack's root, outside every view.
    fn root(&self, path: &str) -> Option<&Blob> {
        let entry = self.root_entry(path);
        self.note(|| Query::Root(path.into()), || root_answer(entry));
        entry.map(|entry| &entry.blob)
    }

    fn root_entry(&self, path: &str) -> Option<&'r packprune_pack::Entry> {
        let path = PhysPath::new(path).ok()?;
        self.side.pack.files.get(&path)
    }

    fn pack_icon(&self) -> Option<Desc> {
        let bytes = self.root(PACK_ICON)?;
        Some(self.pixels(bytes, canon::digest_bytes(bytes)))
    }

    /// The description as a text component: what the pack list shows, not how the file spells it.
    fn pack_description(&self) -> Option<Desc> {
        let json = parse_json(self.root(PACK_META)?).ok()?;
        json.path("pack")
            .get("description")
            .map(|description| canonical(&component(description)))
    }

    fn file_key(&self, logical: &str) -> Option<Desc> {
        // The layout's kind: a file the client has at a path no folder rule names is `Other` there, not `Unknown`.
        let kind = self
            .physical(logical)
            .and_then(|physical| self.side.layout.kinds.get(physical).copied())
            .unwrap_or_else(|| FileKind::of(logical));
        match kind {
            FileKind::PostEffect | FileKind::Particle | FileKind::WaypointStyle => self
                .json(logical)
                .map(|json| canonical_with(&json, &|value| normalized_id(self.interner(), value))),
            FileKind::Texture => self.image(logical),
            FileKind::Sound => self
                .bytes(logical)
                .map(|(bytes, digest)| self.heard(&bytes, digest)),
            FileKind::Lang => self.translations(logical),
            FileKind::Shader | FileKind::ShaderInclude => self
                .bytes(logical)
                .map(|(bytes, digest)| glsl_tokens(&bytes, digest)),
            FileKind::Text | FileKind::Other if logical.ends_with(".json") => self
                .bytes(logical)
                .map(|(bytes, digest)| json_tokens(&bytes, digest)),
            _ => self
                .bytes(logical)
                .map(|(_, digest)| Desc::str(canon::encode(&digest))),
        }
    }

    // ---- reading

    fn physical(&self, logical: &str) -> Option<&'r PhysPath> {
        let physical = self.lookup_physical(logical);
        self.note(
            || Query::File(logical.into()),
            || self.file_answer(physical),
        );
        physical
    }

    fn lookup_physical(&self, logical: &str) -> Option<&'r PhysPath> {
        let logical = self.side.logicals.get(logical)?;
        let state = self.state;
        state
            .files
            .get_or_init(|| {
                state
                    .view
                    .files
                    .iter()
                    .map(|(logical, physical)| (logical.clone(), physical.clone()))
                    .collect()
            })
            .get(logical)
    }

    fn vanilla(&self, logical: &str) -> bool {
        let has = self.side.has_vanilla(self.ctx, logical);
        self.note(|| Query::Vanilla(logical.into()), || Answer::of(has));
        has
    }

    fn vanilla_file(&self, logical: &str) -> Option<(Blob, Digest)> {
        let file = self.ctx.vanilla_file(logical);
        self.note(
            || Query::VanillaFile(logical.into()),
            || Answer::digest(file.clone()),
        );
        file
    }

    fn exists(&self, logical: &str) -> bool {
        self.physical(logical).is_some()
            || self.vanilla(logical) && self.vanilla_file(logical).is_some()
    }

    /// A file's bytes and content digest: the pack's under this view, else vanilla's unless the pack hides it.
    fn bytes(&self, logical: &str) -> Option<(Blob, Digest)> {
        if let Some(physical) = self.physical(logical) {
            let entry = self.side.pack.files.get(physical)?;
            return Some((Arc::clone(&entry.blob), digest_of(entry)));
        }
        if self.vanilla(logical) {
            self.vanilla_file(logical)
        } else {
            None
        }
    }

    fn json(&self, logical: &str) -> Option<Arc<Node>> {
        let (bytes, digest) = self.bytes(logical)?;
        self.ctx.shared.json(digest, &bytes)
    }
}

fn sprite_answer(sprite: Option<&Sprite>) -> Answer {
    match sprite {
        Some(sprite) => {
            let mut hasher = blake3::Hasher::new();
            hash_sprite(&mut hasher, sprite);
            Answer::Digest(canon::truncate(hasher.finalize()))
        }
        None => Answer::Absent,
    }
}

fn hash_sprite(hasher: &mut blake3::Hasher, sprite: &Sprite) {
    hasher.update(&(sprite.inputs.len() as u64).to_le_bytes());
    for input in &sprite.inputs {
        for part in [&*input.logical, input.reason] {
            hasher.update(part.as_bytes());
            hasher.update(&[0]);
        }
    }
    match &sprite.origin {
        Some(origin) => hasher.update(&[1]).update(origin.as_str().as_bytes()),
        None => hasher.update(&[2]),
    };
    hasher.update(&[0]);
    hasher.update(sprite.region.as_bytes());
}

fn root_answer(entry: Option<&packprune_pack::Entry>) -> Answer {
    Answer::digest(entry.map(|entry| ((), digest_of(entry))))
}

/// A shader as the client's loader and the GLSL compiler read it (`glsl::canonical`: its tokens, every line break and
/// whether it ends with one), the shader minifier's own check; the bytes' digest when more of the file can be read.
fn glsl_tokens(bytes: &[u8], digest: Digest) -> Desc {
    let mut out = Desc::object();
    match glsl::canonical(bytes) {
        Some(form) => out.set(
            "tokens",
            Desc::str(canon::encode(&canon::digest_bytes(&form))),
        ),
        None => out.set("bytes", Desc::str(canon::encode(&digest))),
    };
    out
}

/// A JSON file a JSON reader loads: its bytes without the whitespace between tokens (the JSON minifier's own
/// check); the bytes' digest when it does not parse.
fn json_tokens(bytes: &[u8], digest: Digest) -> Desc {
    let mut out = Desc::object();
    match minify::json(bytes) {
        Ok(tokens) => out.set(
            "tokens",
            Desc::str(canon::encode(&canon::digest_bytes(&tokens))),
        ),
        Err(_) => out.set("bytes", Desc::str(canon::encode(&digest))),
    };
    out
}

const PACK_ICON: &str = "pack.png";
const PACK_META: &str = "pack.mcmeta";
const PACK_ICON_KEY: &str = "pack icon";
const PACK_DESCRIPTION_KEY: &str = "pack description";

/// `Language.UNSUPPORTED_FORMAT_PATTERN`: ASCII digits, as Java's `\d`.
static UNSUPPORTED_FORMAT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?-u)%(\d+\$)?[\d.]*[df]").expect("a valid translation format pattern")
});

/// `GsonHelper.convertToString` of a language value, then the client's `%d`/`%f` to `%s` rewrite: a number keeps its
/// spelling (`LazilyParsedNumber`), a boolean its word; `None` for an object, an array or `null`, which it rejects.
fn translation(node: &Node, source: &[u8]) -> Option<String> {
    let text = match &node.value {
        Value::String(text) => text.to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Int(_) | Value::Long(_) | Value::Big(_) | Value::Double(_) => {
            let span = node.span.start as usize..node.span.end as usize;
            String::from_utf8_lossy(&source[span]).into_owned()
        }
        _ => return None,
    };
    Some(UNSUPPORTED_FORMAT.replace_all(&text, "%${1}s").into_owned())
}

/// The text component a JSON value decodes to (`ComponentSerialization.CODEC`): a string is a literal, a list is its
/// first component with the others appended to its siblings, and an empty `extra` is no sibling.
fn component(node: &Node) -> Node {
    match &node.value {
        Value::String(text) => Node::new(Value::Object(vec![Member::new(
            "text",
            Node::new(Value::String(text.clone())),
        )])),
        Value::Array(items) if !items.is_empty() => {
            let mut first = component(&items[0]);
            let rest = items[1..].iter().map(component);
            if let Value::Object(members) = &mut first.value {
                match members.iter_mut().find(|member| &*member.name == "extra") {
                    Some(Member {
                        node:
                            Node {
                                value: Value::Array(extra),
                                ..
                            },
                        ..
                    }) => extra.extend(rest),
                    _ => {
                        let rest: Vec<Node> = rest.collect();
                        if !rest.is_empty() {
                            members.push(Member::new("extra", Node::new(Value::Array(rest))));
                        }
                    }
                }
            }
            first
        }
        Value::Object(members) => Node::new(Value::Object(
            members
                .iter()
                .filter_map(|member| match (&*member.name, &member.node.value) {
                    ("extra", Value::Array(extra)) if extra.is_empty() => None,
                    ("extra", Value::Array(extra)) => Some(Member::new(
                        "extra",
                        Node::new(Value::Array(extra.iter().map(component).collect())),
                    )),
                    _ => Some(member.clone()),
                })
                .collect(),
        )),
        _ => node.clone(),
    }
}

pub(crate) fn glyph_key(font: &str, codepoint: i32) -> String {
    format!("glyph {font} U+{codepoint:04X}")
}

/// The name of a compared view: its format and the overlays each side applies there.
pub(crate) fn label(format: Format, before: &str, after: &str) -> String {
    let ids = if before == after {
        before.to_owned()
    } else {
        format!("{before} -> {after}")
    };
    format!("{} ({ids})", show_format(format))
}

fn sorted(members: &[Member]) -> Vec<(&str, &Node)> {
    let mut sorted: Vec<_> = members
        .iter()
        .map(|member| (&*member.name, &member.node))
        .collect();
    sorted.sort_by(|a, b| java_order(a.0, b.0));
    sorted
}

/// An id written with or without its default namespace is the same id.
fn normalized_id(interner: &Interner, value: &str) -> String {
    match value.strip_prefix("minecraft:") {
        Some(path) if Id::parse(interner, value).is_some_and(|id| id.text() == value) => {
            path.to_owned()
        }
        _ => value.to_owned(),
    }
}

/// `TextureSlots.Resolver`: the chain bottom up, each model's slots over its parent's, then `#` references.
fn texture_slots(chain: &[Arc<Node>], generated: bool) -> HashMap<String, Material> {
    let mut resolved: HashMap<String, Material> = HashMap::new();
    let mut unresolved: HashMap<String, String> = HashMap::new();
    if generated {
        unresolved.insert("particle".into(), "layer0".into());
    }
    for model in chain.iter().rev() {
        for member in model.path("textures").members() {
            let (slot, value): (&str, &Node) = (&member.name, &member.node);
            match value.as_str().and_then(|text| text.strip_prefix('#')) {
                Some(target) => {
                    resolved.remove(slot);
                    unresolved.insert(slot.to_owned(), target.to_owned());
                }
                None => {
                    unresolved.remove(slot);
                    let material = if value.is_object() {
                        Material {
                            sprite: value.path("sprite").as_string("").into_owned(),
                            force_translucent: value.path("force_translucent").as_bool(false),
                        }
                    } else {
                        Material {
                            sprite: value.as_string("").into_owned(),
                            force_translucent: false,
                        }
                    };
                    resolved.insert(slot.to_owned(), material);
                }
            }
        }
    }
    let mut changed = true;
    while changed {
        changed = false;
        let pending: Vec<(String, String)> = unresolved
            .iter()
            .map(|(a, b)| (a.clone(), b.clone()))
            .collect();
        for (slot, target) in pending {
            if let Some(material) = resolved.get(&target).cloned() {
                resolved.insert(slot.clone(), material);
                unresolved.remove(&slot);
                changed = true;
            }
        }
    }
    resolved
}

/// `ItemTransforms.Deserializer`: a left hand without a transform of its own takes the right hand's.
fn transform(display: &Node, context: &str) -> Option<Desc> {
    let mut transform = display.path(context);
    if matches!(transform.value, Value::Missing) && context.ends_with("lefthand") {
        transform = display.path(&context.replace("lefthand", "righthand"));
    }
    if !transform.is_object() {
        return None;
    }
    let mut out = Desc::object();
    out.set(
        "rotation",
        vector(transform.path("rotation"), 0.0, 1.0, f32::INFINITY),
    )
    .set(
        "translation",
        vector(transform.path("translation"), 0.0, 0.0625, 5.0),
    )
    .set("scale", vector(transform.path("scale"), 1.0, 1.0, 4.0));
    Some(out)
}

/// `ItemTransform.Deserializer.getVector3f`, scaled then clamped as it reads them.
fn vector(array: &Node, absent: f32, scale: f32, clamp: f32) -> Desc {
    let values = (0..3)
        .map(|i| {
            let value = if array.is_array() && array.size() == 3 {
                array.item(i).map_or(0.0, Node::float_value) * scale
            } else {
                absent
            };
            Desc::Double(f64::from(value.clamp(-clamp, clamp)))
        })
        .collect();
    Desc::Array(values)
}

/// `FaceBakery.defaultFaceUV`.
fn default_uvs(direction: usize, from: &[f32], to: &[f32]) -> Vec<f32> {
    match DIRECTIONS[direction] {
        "down" => vec![from[0], 16.0 - to[2], to[0], 16.0 - from[2]],
        "up" => vec![from[0], from[2], to[0], to[2]],
        "north" => vec![16.0 - to[0], 16.0 - to[1], 16.0 - from[0], 16.0 - from[1]],
        "south" => vec![from[0], 16.0 - to[1], to[0], 16.0 - from[1]],
        "west" => vec![from[2], 16.0 - to[1], to[2], 16.0 - from[1]],
        _ => vec![16.0 - to[2], 16.0 - to[1], 16.0 - from[2], 16.0 - from[1]],
    }
}

fn quantize(texels: f64) -> f64 {
    (texels * TEXEL_STEP).round_ties_even() / TEXEL_STEP
}

fn actual_width(image: &Image, x0: i32, y0: i32, width: i32, height: i32) -> i32 {
    for x in (0..width).rev() {
        for y in 0..height {
            let (px, py) = (x0 + x, y0 + y);
            if px < image.width && py < image.height && image.alpha(px, py) != 0 {
                return x + 1;
            }
        }
    }
    0
}

fn floats(array: &Node, size: usize) -> Vec<f32> {
    (0..size)
        .map(|i| array.item(i).map_or(0.0, Node::float_value))
        .collect()
}

fn float_array(values: &[f32]) -> Desc {
    Desc::Array(
        values
            .iter()
            .map(|&value| Desc::Double(f64::from(value)))
            .collect(),
    )
}
