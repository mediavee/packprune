use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::Arc;

use packprune_codec::png::{self, Image};
use packprune_graph::Names;
use packprune_pack::json::{self, Node, TokenKind};
use packprune_pack::{FileKind, Interner, LogicalPath, PhysPath, ResourceId, View};
use packprune_pass::{Edits, Loss, Pass, PassCx, PassError, Stage};
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};

use super::crop_box::{Axis, CropBox, Span};
use crate::textures::{Views, file, no_options};

/// `ModelManager`: a model face looks in the items atlas, then the blocks atlas.
const FACE_ATLASES: [&str; 2] = ["minecraft:items", "minecraft:blocks"];
/// `AtlasManager.KNOWN_ATLASES`: of those, only blocks has mipmaps.
const MIPMAPPED: &str = "minecraft:blocks";
/// The mip levels the client's option allows at most, the level `SpriteLoader.stitch` starts from.
const MAX_MIP_LEVELS: u32 = 4;
/// The first format with atlas files (1.19.3). Before it, `ModelBakery` stitched every texture a loaded model names
/// into the blocks atlas, each read from `textures/<path>.png`.
const ATLAS_FILES: u32 = 12;
const WHOLE_SOURCES: [&str; 2] = ["atlas.single", "atlas.directory"];
const DIRECTIONS: [&str; 6] = ["down", "up", "north", "south", "west", "east"];
/// `ItemModelGenerator.LAYERS`.
const LAYERS: usize = 5;
/// A face target that is no pack texture this pass could crop: a vanilla sprite, a missing one.
const ELSEWHERE: TextureId = TextureId::MAX;

/// A texture's number in [`Run::textures`].
type TextureId = u32;
/// A face's number in [`Run::faces`].
type FaceId = u32;

/// A texture that only element-model faces sample, cropped to the texels those faces sample, and
/// their UVs rescaled so each face samples exactly the texels it did. A texture something draws whole (a generated
/// item layer, a particle, a font, equipment, an unstitch or palette source, a sprite named from outside the pack or
/// by vanilla) stays as it is, and so does one whose rescaled UVs a float cannot hold exactly. On a mipmapped atlas,
/// every mip level of what is kept stays the same. `uv-crop-particles` also crops what models use as their particle,
/// which then shows only the cropped texels: lossy.
pub(crate) struct UvCrop {
    pub particles: bool,
}

impl Pass for UvCrop {
    fn name(&self) -> &'static str {
        if self.particles {
            "uv-crop-particles"
        } else {
            "uv-crop"
        }
    }

    fn stage(&self) -> Stage {
        Stage::Textures
    }

    fn loss(&self) -> Loss {
        if self.particles {
            Loss::Lossy
        } else {
            Loss::Lossless
        }
    }

    fn options_schema(&self) -> schemars::Schema {
        no_options()
    }

    fn run(&self, cx: &PassCx<'_>) -> Result<Edits, PassError> {
        let views = Views::new(cx, self.name())?;
        let mut run = Run::new(cx, &views, self.particles);
        run.collect();
        run.crop(self.name())
    }
}

/// One face of one element of a model file, wherever the model is used.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Face {
    model: PhysPath,
    element: usize,
    direction: &'static str,
}

impl Face {
    fn uv(&self, i: usize) -> String {
        format!("/elements/{}/faces/{}/uv/{i}", self.element, self.direction)
    }
}

/// A face, its UVs once read, and the textures it samples in any view.
struct FaceSlot {
    face: Face,
    /// What [`Run::read_uvs`] found, which every view asks again.
    uvs: Option<Option<[String; 4]>>,
    /// A set, [`ELSEWHERE`] included.
    targets: Vec<TextureId>,
}

/// What a sprite id names in one view through the atlases a model face looks in.
struct Named {
    /// An atlas holds the sprite.
    found: bool,
    /// Each texture behind it, `None` for one no pack file is.
    textures: Vec<Option<TextureId>>,
}

/// A pack texture file, the faces that sample it, and why it stays whole when it does.
struct Texture {
    physical: PhysPath,
    logical: String,
    faces: Vec<FaceId>,
    /// `faces` as a set, so that each view adding its faces again stays linear.
    sampled: BTreeSet<FaceId>,
    /// The `.mcmeta` each view reads with it, `""` for none.
    metas: BTreeSet<String>,
    /// The mip block its crop is aligned to: `2^levels` of the most mipmapped atlas holding it in any view.
    align: i32,
    particle: bool,
    whole: Option<String>,
}

impl Texture {
    fn whole(&mut self, reason: impl Into<String>) {
        if self.whole.is_none() {
            self.whole = Some(reason.into());
        }
    }

    fn meta(&self) -> &str {
        self.metas.first().map_or("", String::as_str)
    }
}

struct Run<'a> {
    cx: &'a PassCx<'a>,
    views: &'a Views<'a>,
    particles: bool,
    missing: Option<ResourceId>,
    /// By view: the pack textures its mipmapped atlas holds, which set its mip level.
    mipmapped: BTreeMap<usize, BTreeSet<TextureId>>,
    textures: Vec<Texture>,
    texture_ids: BTreeMap<PhysPath, TextureId>,
    /// What [`Run::texture`] already recorded: by view, whether the atlas is the mipmapped one, and the file.
    recorded: BTreeSet<(usize, bool, TextureId)>,
    /// Each logical path text a lookup asked for, checked once: every view asks for the same ones.
    logicals: RefCell<BTreeMap<Box<str>, Option<LogicalPath>>>,
    faces: Vec<FaceSlot>,
    face_ids: BTreeMap<Face, FaceId>,
    /// In the view being collected, by sprite id.
    named: BTreeMap<ResourceId, Rc<Named>>,
    /// By model file.
    numbers: BTreeMap<PhysPath, UvNumbers>,
    /// Parsed JSON by physical path, `None` when it does not parse; shared by the parent chains that name it.
    json: BTreeMap<PhysPath, Option<Arc<Node>>>,
    /// Each model file as a link of a parent chain, `None` when it is no JSON object.
    links: BTreeMap<PhysPath, Option<Arc<Link>>>,
    /// What each parent chain gives, by its links' numbers and whether it leaves the pack: the same in every view.
    shapes: BTreeMap<(Vec<usize>, bool), Arc<Shape>>,
    whole_sprites: BTreeSet<ResourceId>,
    whole_files: BTreeSet<String>,
}

enum Outcome {
    /// `meta`: the rewritten `.mcmeta`, when it names the frame size.
    Cropped {
        png: Vec<u8>,
        meta: Option<(PhysPath, Vec<u8>)>,
        uvs: Vec<(FaceId, [String; 4])>,
        before: u64,
        after: u64,
    },
    /// `inexact`: a crop was possible but no box keeps every UV exact.
    Left { reason: &'static str, inexact: bool },
}

fn left(reason: &'static str) -> Outcome {
    Outcome::Left {
        reason,
        inexact: false,
    }
}

/// A model file in a parent chain: its number among the links, its JSON and the parent it names.
struct Link {
    number: usize,
    json: Arc<Node>,
    parent: Parent,
}

enum Parent {
    None,
    /// A parent that is no resource id.
    Unreadable,
    /// `id` as text, which stops a cycle, and what it names.
    Id {
        id: String,
        next: Next,
    },
}

enum Next {
    /// The path of a built-in model (`builtin/generated`), which ends the chain.
    Builtin(String),
    /// The logical path of the parent model file.
    Model(Arc<str>),
}

/// What a parent chain gives every view that resolves a model to it.
struct Shape {
    /// Each resolved texture slot and its sprite's id.
    slots: BTreeMap<String, Option<ResourceId>>,
    generated: bool,
    /// The model file defining the elements, and the size of its `elements`.
    defining: Option<(PhysPath, usize)>,
    /// When the chain is whole: each drawn face whose texture a slot resolves, with its sprite's id.
    faces: Vec<(FaceId, Option<ResourceId>)>,
}

impl<'a> Run<'a> {
    fn new(cx: &'a PassCx<'a>, views: &'a Views<'a>, particles: bool) -> Self {
        Self {
            cx,
            views,
            particles,
            missing: ResourceId::parse(&cx.run.interner, &cx.rules.missing_sprite),
            mipmapped: BTreeMap::new(),
            textures: Vec::new(),
            texture_ids: BTreeMap::new(),
            recorded: BTreeSet::new(),
            logicals: RefCell::default(),
            faces: Vec::new(),
            face_ids: BTreeMap::new(),
            named: BTreeMap::new(),
            numbers: BTreeMap::new(),
            json: BTreeMap::new(),
            links: BTreeMap::new(),
            shapes: BTreeMap::new(),
            whole_sprites: BTreeSet::new(),
            whole_files: BTreeSet::new(),
        }
    }

    fn face_id(&mut self, face: Face) -> FaceId {
        if let Some(&id) = self.face_ids.get(&face) {
            return id;
        }
        let id = self.faces.len() as FaceId;
        self.faces.push(FaceSlot {
            face: face.clone(),
            uvs: None,
            targets: Vec::new(),
        });
        self.face_ids.insert(face, id);
        id
    }

    fn target(&mut self, face: FaceId, texture: TextureId) {
        let targets = &mut self.faces[face as usize].targets;
        if !targets.contains(&texture) {
            targets.push(texture);
        }
    }

    fn json(&mut self, physical: &PhysPath) -> Option<&Arc<Node>> {
        let files = &self.cx.pack.files;
        self.json
            .entry(physical.clone())
            .or_insert_with(|| {
                files
                    .get(physical)
                    .and_then(|entry| json::parse(&entry.blob).ok())
                    .map(Arc::new)
            })
            .as_ref()
    }

    fn collect(&mut self) {
        // Each model file's JSON and UV numbers are read once, in parallel: the views below only look them up.
        let cx = self.cx;
        let read: Vec<(PhysPath, Option<Arc<Node>>, UvNumbers)> = cx
            .layout
            .kinds
            .par_iter()
            .filter(|(_, kind)| **kind == FileKind::Model)
            .filter_map(|(path, _)| {
                let blob = &cx.pack.files.get(path)?.blob;
                let json = json::parse(blob).ok().map(Arc::new);
                Some((path.clone(), json, uv_numbers(blob)))
            })
            .collect();
        for (path, json, numbers) in read {
            self.json.insert(path.clone(), json);
            self.numbers.insert(path, numbers);
        }
        for reference in self.cx.index.all().values().flatten() {
            match &reference.names {
                Names::File { logical, .. } if &*reference.reason.0 != "atlas.single" => {
                    self.whole_files.insert(logical.as_str().to_owned());
                }
                Names::Sprite { sprite, .. } if &*reference.reason.0 != "model.texture" => {
                    self.whole_sprites.insert(*sprite);
                }
                _ => {}
            }
        }
        let views = self.views;
        for index in 0..views.list.len() {
            let view = &views.list[index];
            self.named.clear();
            if !legacy(view) {
                self.sprites(index);
            }
            let locked = self.uv_locked(view);
            let models: Vec<(LogicalPath, PhysPath)> = view
                .files
                .iter()
                .filter(|(_, physical)| {
                    self.cx.layout.kinds.get(*physical) == Some(&FileKind::Model)
                })
                .map(|(logical, physical)| (logical.clone(), physical.clone()))
                .collect();
            for (logical, _) in models {
                let text = logical.as_str();
                let lock = locked.contains(text)
                    || self.views.vanilla(text)
                        && text.starts_with("assets/minecraft/models/block/");
                self.model(index, text, lock);
            }
        }
        let mipmapped = std::mem::take(&mut self.mipmapped);
        for held in mipmapped.values() {
            let align = 1 << self.mip_level(held);
            for &texture in held {
                let texture = &mut self.textures[texture as usize];
                texture.align = texture.align.max(align);
            }
        }
        self.mipmapped = mipmapped;
        let mut meta_users: BTreeMap<String, usize> = BTreeMap::new();
        for texture in &self.textures {
            for meta in &texture.metas {
                *meta_users.entry(meta.clone()).or_default() += 1;
            }
        }
        for texture in &mut self.textures {
            let logical = LogicalPath::new(&texture.logical).ok();
            if logical.is_some_and(|logical| self.cx.kept_names.contains(&logical)) {
                texture.whole("named from outside the pack");
            } else if self.whole_files.contains(&texture.logical) {
                texture.whole("read whole as a file");
            } else if texture.metas.len() > 1 {
                texture.whole("its .mcmeta differs between views");
            } else if !texture.metas.contains("") && meta_users[texture.meta()] > 1 {
                texture.whole("its .mcmeta serves another texture file too");
            }
        }
        // A face whose UVs serve several textures, or one this pass leaves whole, keeps its UVs: so do they all.
        let mut changed = true;
        while changed {
            changed = false;
            for slot in &self.faces {
                let targets = &slot.targets;
                let shared = targets.len() > 1
                    || targets.contains(&ELSEWHERE)
                    || targets.iter().any(|&target| {
                        target != ELSEWHERE && self.textures[target as usize].whole.is_some()
                    });
                if !shared {
                    continue;
                }
                for &target in targets {
                    if target == ELSEWHERE {
                        continue;
                    }
                    let texture = &mut self.textures[target as usize];
                    if texture.whole.is_none() {
                        texture.whole("a face it shares with another texture");
                        changed = true;
                    }
                }
            }
        }
    }

    /// Every pack texture an atlas of `view` holds.
    fn sprites(&mut self, view: usize) {
        let (cx, views) = (self.cx, self.views);
        let interner = &cx.run.interner;
        for atlas in views.atlases(view).atlas_ids() {
            for (id, sprite) in views.sprites(view, atlas, interner) {
                for input in &sprite.inputs {
                    let Some(physical) = self.physical(view, &input.logical) else {
                        continue;
                    };
                    let whole = if !WHOLE_SOURCES.contains(&input.reason)
                        || sprite.inputs.len() != 1
                        || !sprite.region.is_empty()
                    {
                        Some(format!("an {} source", input.reason))
                    } else if self.whole_sprites.contains(&id) {
                        Some("a sprite something draws whole".into())
                    } else if self.views.vanilla(&file(interner, id, "textures", ".png")) {
                        Some("a vanilla sprite id".into())
                    } else {
                        None
                    };
                    let texture = self.texture(view, atlas, &input.logical, physical);
                    if let Some(reason) = whole {
                        self.textures[texture as usize].whole(reason);
                    }
                }
            }
        }
    }

    fn physical(&self, view: usize, logical: &str) -> Option<PhysPath> {
        let mut logicals = self.logicals.borrow_mut();
        let logical = match logicals.get(logical) {
            Some(checked) => checked.as_ref()?,
            None => logicals
                .entry(logical.into())
                .or_insert_with(|| LogicalPath::new(logical).ok())
                .as_ref()?,
        };
        self.views.list[view].files.get(logical).cloned()
    }

    /// The pack texture `physical`, held by `atlas` in `view`.
    fn texture(
        &mut self,
        view: usize,
        atlas: &str,
        logical: &str,
        physical: PhysPath,
    ) -> TextureId {
        let id = match self.texture_ids.get(&physical) {
            Some(&id) => id,
            None => {
                let id = self.textures.len() as TextureId;
                self.texture_ids.insert(physical.clone(), id);
                self.textures.push(Texture {
                    physical,
                    logical: logical.to_owned(),
                    faces: Vec::new(),
                    sampled: BTreeSet::new(),
                    metas: BTreeSet::new(),
                    align: 1,
                    particle: false,
                    whole: None,
                });
                id
            }
        };
        // `physical` names `logical` in `view`, so a second call with the same key would change nothing.
        let mipmapped = atlas == MIPMAPPED;
        if !self.recorded.insert((view, mipmapped, id)) {
            return id;
        }
        let meta = self
            .physical(view, &format!("{logical}.mcmeta"))
            .map_or(String::new(), |meta| meta.as_str().to_owned());
        if mipmapped {
            self.mipmapped.entry(view).or_default().insert(id);
        }
        self.textures[id as usize].metas.insert(meta);
        id
    }

    /// The textures `id` is built from in `atlas` of `view`, by logical path; `None` when it has no such sprite.
    fn held(&self, view: usize, atlas: &str, id: ResourceId) -> Option<Vec<Arc<str>>> {
        if !legacy(&self.views.list[view]) {
            return self.views.atlases(view).sprite(atlas, id).map(|sprite| {
                sprite
                    .inputs
                    .iter()
                    .map(|input| Arc::clone(&input.logical))
                    .collect()
            });
        }
        let texture = file(&self.cx.run.interner, id, "textures", ".png");
        (atlas == MIPMAPPED
            && (self.physical(view, &texture).is_some() || self.views.vanilla(&texture)))
        .then(|| vec![texture.into()])
    }

    /// What `id` names in `view`, through the atlases a model face looks in; the same for every face and slot of the
    /// view that names it.
    fn resolve(&mut self, view: usize, id: ResourceId) -> Rc<Named> {
        if let Some(named) = self.named.get(&id) {
            return Rc::clone(named);
        }
        let mut named = Named {
            found: false,
            textures: Vec::new(),
        };
        for atlas in FACE_ATLASES {
            let Some(held) = self.held(view, atlas, id) else {
                continue;
            };
            named.found = true;
            for logical in held {
                let texture = self
                    .physical(view, &logical)
                    .map(|physical| self.texture(view, atlas, &logical, physical));
                named.textures.push(texture);
            }
        }
        let named = Rc::new(named);
        self.named.insert(id, Rc::clone(&named));
        named
    }

    /// Every pack texture `sprite` names in `view`, through the atlases a model face looks in.
    fn named(&mut self, view: usize, id: Option<ResourceId>) -> Vec<TextureId> {
        match id {
            Some(id) => self
                .resolve(view, id)
                .textures
                .iter()
                .flatten()
                .copied()
                .collect(),
            None => Vec::new(),
        }
    }

    fn mark(&mut self, textures: Vec<TextureId>, reason: &str) {
        for texture in textures {
            self.textures[texture as usize].whole(reason);
        }
    }

    /// `SpriteLoader.stitch`: the atlas's mip level drops to the log2 of its smallest frame side, and of the lowest
    /// set bit of any frame side. Vanilla's frames are multiples of 16 and never lower it.
    fn mip_level(&mut self, held: &BTreeSet<TextureId>) -> u32 {
        let mut lowest = 1i32 << MAX_MIP_LEVELS;
        for &texture in held {
            if let Some((width, height)) = self.frame_size(texture) {
                lowest = lowest
                    .min(width.min(height))
                    .min(width.isolate_lowest_one().min(height.isolate_lowest_one()));
            }
        }
        MAX_MIP_LEVELS.min(lowest.max(1).ilog2())
    }

    /// The frame size of a texture from its PNG header and its animation, `None` when the header is unreadable.
    fn frame_size(&mut self, texture: TextureId) -> Option<(i32, i32)> {
        let cx = self.cx;
        let texture = &self.textures[texture as usize];
        let png = &cx.pack.files.get(&texture.physical)?.blob;
        if png.len() < 24 {
            return None;
        }
        let int = |at: usize| i32::from_be_bytes(png[at..at + 4].try_into().expect("four bytes"));
        let (width, height) = (int(16), int(20));
        let meta = texture.metas.iter().find(|meta| !meta.is_empty()).cloned();
        let animation = meta
            .and_then(|meta| PhysPath::new(&meta).ok())
            .and_then(|meta| self.json(&meta))
            .map(|meta| meta.path("animation"));
        let Some(animation) = animation.filter(|node| node.is_object()) else {
            return Some((width, height));
        };
        if width <= 0 || height <= 0 {
            return Some((width, height));
        }
        let frame_width = animation.path("width").as_int(0);
        let frame_height = animation.path("height").as_int(0);
        if frame_width > 0 || frame_height > 0 {
            let pick = |given: i32, whole: i32| if given > 0 { given } else { whole };
            return Some((pick(frame_width, width), pick(frame_height, height)));
        }
        let side = width.min(height);
        Some((side, side))
    }

    /// The models the view's block states turn with `uvlock`, which moves where their faces sample
    /// (`FaceBakery.bakeVertex`). Vanilla's block states are not read: a model at a vanilla block path counts too.
    fn uv_locked(&mut self, view: &View) -> BTreeSet<String> {
        let mut locked = BTreeSet::new();
        let states: Vec<PhysPath> = view
            .files
            .values()
            .filter(|physical| self.cx.layout.kinds.get(*physical) == Some(&FileKind::Blockstate))
            .cloned()
            .collect();
        let cx = self.cx;
        let interner = &cx.run.interner;
        for physical in states {
            if let Some(json) = self.json(&physical) {
                locked_models(json, interner, &mut locked);
            }
        }
        locked
    }

    /// `ResolvedModel`: the parent chain of one model, its texture slots, and what its faces sample. `locked`: a
    /// block state may turn this model with `uvlock`, so its faces and textures stay.
    fn model(&mut self, view: usize, logical: &str, locked: bool) {
        let mut chain: Vec<Arc<Link>> = Vec::new();
        let mut physicals: Vec<PhysPath> = Vec::new();
        let mut generated = false;
        let mut outside = false;
        let mut current: Option<Arc<str>> = None;
        loop {
            let physical = self.physical(view, current.as_deref().unwrap_or(logical));
            let link = physical.as_ref().and_then(|physical| self.link(physical));
            let (Some(physical), Some(link)) = (physical, link) else {
                // A vanilla parent, or a missing one: what it adds is not read here.
                outside = true;
                break;
            };
            physicals.push(physical);
            // The ids met so far are those the links before this one name as their parent.
            let seen = |id: &str| {
                chain
                    .iter()
                    .any(|link| matches!(&link.parent, Parent::Id { id: other, .. } if other == id))
            };
            match &link.parent {
                Parent::None => {}
                Parent::Id { id, next } if !seen(id) => match next {
                    Next::Builtin(path) => generated = path == "builtin/generated",
                    Next::Model(parent) => current = Some(Arc::clone(parent)),
                },
                Parent::Id { .. } | Parent::Unreadable => outside = true,
            }
            let ends = !matches!(
                &link.parent,
                Parent::Id {
                    next: Next::Model(_),
                    ..
                }
            ) || outside;
            chain.push(link);
            if ends {
                break;
            }
        }
        let key = (chain.iter().map(|link| link.number).collect(), outside);
        let shape = match self.shapes.get(&key) {
            Some(shape) => Arc::clone(shape),
            None => {
                let shape = Arc::new(self.shape(&chain, &physicals, generated, outside));
                self.shapes.insert(key, Arc::clone(&shape));
                shape
            }
        };
        if outside {
            // Slots the missing part of the chain maps are unknown: every texture named stays, and so do the UVs.
            for id in shape.slots.values() {
                let named = self.named(view, *id);
                self.mark(named, "a model of its chain is not in the pack");
            }
            if let Some((model, elements)) = &shape.defining {
                for element in 0..*elements {
                    for direction in DIRECTIONS {
                        let face = self.face_id(Face {
                            model: model.clone(),
                            element,
                            direction,
                        });
                        self.target(face, ELSEWHERE);
                    }
                }
            }
            return;
        }
        // Every texture a model names is in the legacy blocks atlas, and sets its mip level.
        for id in shape.slots.values() {
            let named = self.named(view, *id);
            if locked {
                self.mark(named, "a face a uvlock rotation moves");
            }
        }
        if let Some(id) = shape.slots.get("particle") {
            for texture in self.named(view, *id) {
                let texture = &mut self.textures[texture as usize];
                texture.particle = true;
                if !self.particles {
                    texture.whole("a particle");
                }
            }
        }
        if shape.defining.is_none() {
            if shape.generated {
                for layer in (0..LAYERS).map_while(|i| shape.slots.get(&format!("layer{i}"))) {
                    let named = self.named(view, *layer);
                    self.mark(named, "a generated item layer");
                }
            }
            return;
        }
        for &(face, id) in &shape.faces {
            self.face(view, face, id);
            if locked {
                self.target(face, ELSEWHERE);
            }
        }
    }

    /// `physical` as a link of a parent chain.
    fn link(&mut self, physical: &PhysPath) -> Option<Arc<Link>> {
        if let Some(link) = self.links.get(physical) {
            return link.clone();
        }
        let cx = self.cx;
        let interner = &cx.run.interner;
        let number = self.links.len();
        let link = self
            .json(physical)
            .filter(|json| json.is_object())
            .cloned()
            .map(|json| {
                let parent = match json
                    .path("parent")
                    .as_str()
                    .filter(|parent| !parent.is_empty())
                {
                    None => Parent::None,
                    Some(parent) => match ResourceId::parse(interner, parent) {
                        None => Parent::Unreadable,
                        Some(id) => {
                            let path = interner.resolve(id.path);
                            let next = if interner.resolve(id.ns) == "minecraft"
                                && path.starts_with(&cx.rules.builtin_model_prefix)
                            {
                                Next::Builtin(path.to_owned())
                            } else {
                                Next::Model(file(interner, id, "models", ".json").into())
                            };
                            Parent::Id {
                                id: id.display(interner).to_string(),
                                next,
                            }
                        }
                    },
                };
                Arc::new(Link {
                    number,
                    json,
                    parent,
                })
            });
        self.links.insert(physical.clone(), link.clone());
        link
    }

    /// What the chain of `physicals` gives: its slots, the model defining its elements and, when the chain is whole,
    /// the faces those elements draw.
    fn shape(
        &mut self,
        chain: &[Arc<Link>],
        physicals: &[PhysPath],
        generated: bool,
        outside: bool,
    ) -> Shape {
        let cx = self.cx;
        let interner = &cx.run.interner;
        let nodes: Vec<&Node> = chain.iter().map(|link| &*link.json).collect();
        let slots = slots(&nodes, generated);
        let defining = nodes.iter().position(|model| model.has("elements"));
        let mut faces = Vec::new();
        if let Some(defining) = defining.filter(|_| !outside) {
            for (index, element) in nodes[defining].path("elements").items().iter().enumerate() {
                for direction in DIRECTIONS {
                    let face = element.path("faces").path(direction);
                    if !face.is_object() || !drawn(element, direction) {
                        continue;
                    }
                    let slot = face.path("texture").as_string("");
                    let Some(sprite) = slots.get(slot.strip_prefix('#').unwrap_or(&slot)) else {
                        continue;
                    };
                    let located = Face {
                        model: physicals[defining].clone(),
                        element: index,
                        direction,
                    };
                    faces.push((self.face_id(located), ResourceId::parse(interner, sprite)));
                }
            }
        }
        Shape {
            slots: slots
                .iter()
                .map(|(name, sprite)| (name.clone(), ResourceId::parse(interner, sprite)))
                .collect(),
            generated,
            defining: defining.map(|defining| {
                (
                    physicals[defining].clone(),
                    nodes[defining].path("elements").size(),
                )
            }),
            faces,
        }
    }

    /// What one face samples in `view`: the pack texture behind each atlas it may look in.
    fn face(&mut self, view: usize, face: FaceId, id: Option<ResourceId>) {
        let id = id.filter(|id| Some(*id) != self.missing);
        let named = id.map(|id| self.resolve(view, id));
        if !named.as_ref().is_some_and(|named| named.found) {
            self.target(face, ELSEWHERE);
        }
        for texture in named.iter().flat_map(|named| &named.textures) {
            self.target(face, texture.unwrap_or(ELSEWHERE));
        }
        let uvs = self.has_uvs(face);
        let Self {
            faces, textures, ..
        } = self;
        for &target in &faces[face as usize].targets {
            if target == ELSEWHERE {
                continue;
            }
            let texture = &mut textures[target as usize];
            if texture.sampled.insert(face) {
                texture.faces.push(face);
            }
            if !uvs {
                texture.whole("a face whose four UVs are not written as numbers");
            }
        }
    }

    /// The four UVs of a face as written, or `None` when it has not four plain numbers the splice can rewrite.
    fn uvs(&mut self, face: FaceId) -> Option<[String; 4]> {
        self.has_uvs(face);
        self.faces[face as usize].uvs.clone().flatten()
    }

    fn has_uvs(&mut self, face: FaceId) -> bool {
        if let Some(uvs) = &self.faces[face as usize].uvs {
            return uvs.is_some();
        }
        let located = self.faces[face as usize].face.clone();
        let uvs = self.read_uvs(&located);
        let has = uvs.is_some();
        self.faces[face as usize].uvs = Some(uvs);
        has
    }

    fn read_uvs(&mut self, face: &Face) -> Option<[String; 4]> {
        let files = &self.cx.pack.files;
        let found = self
            .numbers
            .entry(face.model.clone())
            .or_insert_with(|| uv_numbers(&files[&face.model].blob))
            .as_ref()?;
        let mut uvs: [String; 4] = Default::default();
        for (i, uv) in uvs.iter_mut().enumerate() {
            *uv = found.get(&face.uv(i))?.clone();
        }
        let model = self.json(&face.model)?;
        let array = model
            .path("elements")
            .item(face.element)?
            .path("faces")
            .path(face.direction)
            .path("uv");
        (array.size() == 4).then_some(uvs)
    }

    fn crop(mut self, pass: &'static str) -> Result<Edits, PassError> {
        let mut edits = Edits::default();
        let mut rewritten: BTreeMap<PhysPath, BTreeMap<String, String>> = BTreeMap::new();
        let mut left_whole: BTreeMap<String, usize> = BTreeMap::new();
        let mut inexact = Vec::new();
        let (mut before, mut after, mut cropped) = (0u64, 0u64, 0usize);
        let (mut particle_before, mut particle_after, mut particle_cropped) = (0u64, 0u64, 0usize);
        // In the order of their paths, which the report's list of inexact textures follows.
        let mut order: Vec<TextureId> = (0..self.textures.len() as TextureId).collect();
        order.sort_by(|a, b| {
            self.textures[*a as usize]
                .physical
                .cmp(&self.textures[*b as usize].physical)
        });
        for id in order {
            let texture = &self.textures[id as usize];
            let physical = texture.physical.clone();
            if texture.faces.is_empty() {
                continue;
            }
            if let Some(reason) = &texture.whole {
                *left_whole.entry(reason.clone()).or_default() += 1;
                continue;
            }
            match self.crop_one(id, pass)? {
                Outcome::Cropped {
                    png,
                    meta,
                    uvs,
                    before: texels_before,
                    after: texels_after,
                } => {
                    edits.replaced.insert(physical.clone(), png.into());
                    if let Some((path, bytes)) = meta {
                        edits.replaced.insert(path, bytes.into());
                    }
                    for (face, values) in uvs {
                        let face = &self.faces[face as usize].face;
                        let model = rewritten.entry(face.model.clone()).or_default();
                        for (i, value) in values.into_iter().enumerate() {
                            model.insert(face.uv(i), value);
                        }
                    }
                    before += texels_before;
                    after += texels_after;
                    cropped += 1;
                    if self.textures[id as usize].particle {
                        particle_before += texels_before;
                        particle_after += texels_after;
                        particle_cropped += 1;
                    }
                }
                Outcome::Left {
                    reason,
                    inexact: no_exact,
                } => {
                    *left_whole.entry(reason.into()).or_default() += 1;
                    if no_exact {
                        inexact.push(self.textures[id as usize].logical.clone());
                    }
                }
            }
        }
        let models = rewritten.len();
        for (model, values) in rewritten {
            let bytes = json::rewrite_numbers(&self.cx.pack.files[&model].blob, |_, path| {
                values.get(&*path.pointer().0).cloned()
            })
            .map_err(|error| PassError::Failed {
                pass,
                why: format!("rewriting the UVs of {model}: {error}"),
            })?;
            edits.replaced.insert(model, bytes.into());
        }
        let share = if before == 0 {
            String::new()
        } else {
            format!(
                " (-{} %)",
                one_decimal(100.0 * (before - after) as f64 / before as f64)
            )
        };
        edits.changes.push(format!(
            "{cropped} textures cropped, {before} -> {after} texels{share}"
        ));
        if self.particles {
            edits.changes.push(format!(
                "{particle_cropped} particle sprites cropped, {particle_before} -> {particle_after} texels: their particles show only the texels the faces sample"
            ));
        }
        edits.changes.push(format!("{models} models' UVs rescaled"));
        if !left_whole.is_empty() {
            let reasons: Vec<String> = left_whole
                .iter()
                .map(|(reason, count)| format!("{count} {reason}"))
                .collect();
            edits
                .changes
                .push(format!("left whole: {}", reasons.join(", ")));
        }
        if !inexact.is_empty() {
            let shown: Vec<&str> = inexact.iter().take(5).map(String::as_str).collect();
            edits.changes.push(format!(
                "{} textures left whole, no crop keeps their UVs exact: {}{}",
                inexact.len(),
                shown.join(", "),
                if inexact.len() > 5 { ", ..." } else { "" }
            ));
        }
        Ok(edits)
    }

    fn crop_one(&mut self, id: TextureId, pass: &'static str) -> Result<Outcome, PassError> {
        let cx = self.cx;
        let texture = &self.textures[id as usize];
        let physical = &texture.physical.clone();
        let Ok(image) = png::decode(&cx.pack.files[physical].blob) else {
            return Ok(left("not a PNG the client reads"));
        };
        let (align, faces) = (texture.align, texture.faces.clone());
        let meta_path = texture.meta().to_owned();
        let meta_physical = PhysPath::new(&meta_path).ok();
        let meta = match &meta_physical {
            Some(path) => {
                let parses = json::parses(&cx.pack.files[path].blob);
                match self.json(path).cloned() {
                    Some(meta) if parses => Some(meta),
                    _ => return Ok(left("an unreadable .mcmeta")),
                }
            }
            None => None,
        };
        if meta
            .as_ref()
            .is_some_and(|meta| meta.path("texture").path("blur").as_bool(false))
        {
            return Ok(left("blurred, so sampled across the crop edge"));
        }
        // AnimationMetadataSection: frames on a grid of the frame size, square and as small as the image when unset.
        let animation = meta.as_ref().map(|meta| meta.path("animation"));
        let animated = animation.is_some_and(Node::is_object);
        let explicit = |name| {
            animation
                .filter(|_| animated)
                .map_or(0, |animation| animation.path(name).as_int(0))
        };
        let (explicit_width, explicit_height) = (explicit("width"), explicit("height"));
        let (width, height) = (image.width as i32, image.height as i32);
        let (frame_width, frame_height) = if animated && (explicit_width > 0 || explicit_height > 0)
        {
            (
                if explicit_width > 0 {
                    explicit_width
                } else {
                    width
                },
                if explicit_height > 0 {
                    explicit_height
                } else {
                    height
                },
            )
        } else if animated {
            (width.min(height), width.min(height))
        } else {
            (width, height)
        };
        if width % frame_width != 0 || height % frame_height != 0 {
            return Ok(left("frames that do not tile the image"));
        }
        if align > 1 {
            // MipmapGenerator: only the mean strategy builds each mip texel from the 2x2 block under it alone. AUTO is
            // mean unless the whole image has a transparent texel; cutout ones read the whole image (solidify, alpha
            // coverage), so cropping anything changes what is kept.
            let strategy = meta.as_ref().map_or("auto".into(), |meta| {
                meta.path("texture")
                    .path("mipmap_strategy")
                    .as_string("auto")
                    .into_owned()
            });
            if strategy != "mean" && !(strategy == "auto" && !transparent(&image)) {
                return Ok(left("a cutout mipmap strategy on a mipmapped atlas"));
            }
            if (width > frame_width && frame_width % align != 0)
                || (height > frame_height && frame_height % align != 0)
            {
                return Ok(left("frames off the mip block of a mipmapped atlas"));
            }
        }
        let mut values = Vec::with_capacity(faces.len());
        let (mut xs, mut ys) = (Vec::new(), Vec::new());
        for &face in &faces {
            let written = self.uvs(face).expect("a texture with faces has their UVs");
            let mut floats = [0f32; 4];
            for (i, text) in written.iter().enumerate() {
                let (Ok(float), Ok(double)) = (text.parse::<f32>(), text.parse::<f64>()) else {
                    return Ok(left("a UV read differently as a float and as a double"));
                };
                // The oracle reads the double and narrows it; a value both do not read alike is left alone.
                if double as f32 != float || !float.is_finite() {
                    return Ok(left("a UV read differently as a float and as a double"));
                }
                floats[i] = float;
            }
            xs.push(Span {
                from: floats[0],
                to: floats[2],
            });
            ys.push(Span {
                from: floats[1],
                to: floats[3],
            });
            values.push((face, floats));
        }
        let (Some(x_axis), Some(y_axis)) = (
            CropBox::axis(&xs, frame_width),
            CropBox::axis(&ys, frame_height),
        ) else {
            return Ok(left("a face that samples outside its frame"));
        };
        let mut x = CropBox {
            start: 0,
            size: frame_width,
        };
        let mut y = CropBox {
            start: 0,
            size: frame_height,
        };
        if animated && explicit_width == 0 && explicit_height == 0 {
            // The frame size stays the default only while the cropped frame is square.
            if let Some((bx, by)) = square(&x_axis, &y_axis, frame_width, align) {
                (x, y) = (bx, by);
            }
        } else {
            x = x_axis.smallest(1, align).unwrap_or(x);
            y = y_axis.smallest(1, align).unwrap_or(y);
        }
        if x.size == frame_width && y.size == frame_height {
            let tight =
                x_axis.high - x_axis.low == frame_width && y_axis.high - y_axis.low == frame_height;
            return Ok(if tight {
                left("already the texels its faces sample")
            } else if align > 1 {
                left("no exact crop on the mip block of a mipmapped atlas")
            } else {
                Outcome::Left {
                    reason: "no exact crop",
                    inexact: true,
                }
            });
        }
        let columns = width / frame_width;
        let rows = height / frame_height;
        let Some(png) = crop_frames(&image, columns, rows, frame_width, frame_height, x, y) else {
            return Ok(left("not a PNG the encoder writes"));
        };
        let explicit_size = explicit_width > 0 || explicit_height > 0;
        let scope = cx.scope;
        if !scope.contains(physical)
            || !faces
                .iter()
                .all(|&face| scope.contains(&self.faces[face as usize].face.model))
            || (explicit_size
                && !meta_physical
                    .as_ref()
                    .is_some_and(|meta| scope.contains(meta)))
        {
            return Ok(left("a file outside the pass's lossy globs"));
        }
        let meta_edit = match &meta_physical {
            Some(path) if explicit_size => {
                let (new_width, new_height) = (x.size.to_string(), y.size.to_string());
                let bytes = json::rewrite_numbers(&cx.pack.files[path].blob, |_, at| {
                    match &*at.pointer().0 {
                        "/animation/width" => Some(new_width.clone()),
                        "/animation/height" => Some(new_height.clone()),
                        _ => None,
                    }
                })
                .map_err(|error| PassError::Failed {
                    pass,
                    why: format!("rewriting {path}: {error}"),
                })?;
                Some((path.clone(), bytes))
            }
            _ => None,
        };
        let mut uvs = Vec::with_capacity(values.len());
        for (face, floats) in values {
            let mut written: [String; 4] = Default::default();
            for (i, value) in floats.into_iter().enumerate() {
                let (axis, frame) = if i % 2 == 0 {
                    (x, frame_width)
                } else {
                    (y, frame_height)
                };
                let rescaled = axis
                    .rescale(value, frame)
                    .ok_or_else(|| PassError::Failed {
                        pass,
                        why: format!("{value} on {frame} texels is not a float in {axis:?}"),
                    })?;
                written[i] = text(rescaled);
            }
            uvs.push((face, written));
        }
        Ok(Outcome::Cropped {
            png,
            meta: meta_edit,
            uvs,
            before: width as u64 * height as u64,
            after: (columns * x.size) as u64 * (rows * y.size) as u64,
        })
    }
}

/// Every number token's text by pointer that a UV lookup can ask for, `None` when the file does not tokenize.
type UvNumbers = Option<BTreeMap<String, String>>;

fn uv_numbers(blob: &[u8]) -> UvNumbers {
    let mut found = BTreeMap::new();
    json::visit(blob, |token, path| {
        // `/elements/<i>/faces/<direction>/uv/<j>`: only the numbers a UV lookup can ask for.
        if path.depth() == 6 && matches!(token.kind, TokenKind::Int | TokenKind::Float) {
            found.insert(path.pointer().0.into_string(), token.text.to_string());
        }
    })
    .ok()
    .map(|()| found)
}

/// The smallest square box exact on both axes of a square frame, or `None` when only the whole frame is.
fn square(x: &Axis, y: &Axis, frame: i32, align: i32) -> Option<(CropBox, CropBox)> {
    let start = (x.high - x.low).max(y.high - y.low);
    (start..frame).find_map(|size| Some((x.at(size, align)?, y.at(size, align)?)))
}

/// `NativeImage.computeTransparency` over the whole image: a texel with alpha 0.
fn transparent(image: &Image) -> bool {
    image
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .any(|pixel| pixel[3] == 0)
}

/// Each frame cut to the same box, on the same grid.
fn crop_frames(
    image: &Image,
    columns: i32,
    rows: i32,
    frame_width: i32,
    frame_height: i32,
    x: CropBox,
    y: CropBox,
) -> Option<Vec<u8>> {
    let width = (columns * x.size) as usize;
    let height = (rows * y.size) as usize;
    let row_bytes = x.size as usize * 4;
    let mut rgba = vec![0; width * height * 4];
    for row in 0..rows {
        for column in 0..columns {
            for line in 0..y.size {
                let from_y = (row * frame_height + y.start + line) as usize;
                let from_x = (column * frame_width + x.start) as usize;
                let at = (from_y * image.width as usize + from_x) * 4;
                let to_y = (row * y.size + line) as usize;
                let to = (to_y * width + (column * x.size) as usize) * 4;
                rgba[to..to + row_bytes].copy_from_slice(&image.rgba[at..at + row_bytes]);
            }
        }
    }
    png::encode(&Image {
        width: width as u32,
        height: height as u32,
        rgba,
    })
    .ok()
}

/// The shortest text the client reads as `value` and the oracle too, through a double; the exact decimal otherwise,
/// which both read exactly.
pub(crate) fn text(value: f32) -> String {
    let exact = plain(&format!("{:.149}", f64::from(value)));
    let shortest = plain(&format!("{value}"));
    let reads = shortest.parse::<f32>() == Ok(value)
        && shortest.parse::<f64>().map(|double| double as f32) == Ok(value);
    if reads && shortest.len() < exact.len() {
        shortest
    } else {
        exact
    }
}

/// A plain decimal without trailing zeros, `0` for zero.
fn plain(decimal: &str) -> String {
    let trimmed = if decimal.contains('.') {
        decimal.trim_end_matches('0').trim_end_matches('.')
    } else {
        decimal
    };
    match trimmed {
        "-0" | "" => "0".into(),
        other => other.into(),
    }
}

/// `value` to one decimal: its shortest decimal digits, rounded half up.
fn one_decimal(value: f64) -> String {
    let digits = format!("{value}");
    let (whole, fraction) = digits.split_once('.').unwrap_or((&digits, ""));
    let mut tenths: u64 = whole.parse::<u64>().expect("a non-negative share") * 10
        + fraction
            .bytes()
            .next()
            .map_or(0, |digit| u64::from(digit - b'0'));
    if fraction
        .as_bytes()
        .get(1)
        .is_some_and(|digit| *digit >= b'5')
    {
        tenths += 1;
    }
    format!("{}.{}", tenths / 10, tenths % 10)
}

/// `UnbakedCuboidGeometry.bake`: a face lying in a flat element is never baked.
fn drawn(element: &Node, direction: &str) -> bool {
    let spans: Vec<bool> = (0..3)
        .map(|axis| {
            let side = |name| element.path(name).item(axis).map_or(0.0, Node::float_value);
            side("from") != side("to")
        })
        .collect();
    match direction {
        "west" | "east" => spans[1] && spans[2],
        "down" | "up" => spans[0] && spans[2],
        _ => spans[0] && spans[1],
    }
}

/// `TextureSlots.Resolver`: the chain bottom up, each model's slots over its parent's, then `#` references.
fn slots(chain: &[&Node], generated: bool) -> BTreeMap<String, String> {
    let mut resolved: BTreeMap<String, String> = BTreeMap::new();
    let mut unresolved: BTreeMap<String, String> = BTreeMap::new();
    if generated {
        unresolved.insert("particle".into(), "layer0".into());
    }
    for model in chain.iter().rev() {
        for slot in model.path("textures").members() {
            let name = slot.name.to_string();
            match slot.node.as_str().and_then(|value| value.strip_prefix('#')) {
                Some(target) => {
                    resolved.remove(&name);
                    unresolved.insert(name, target.to_owned());
                }
                None => {
                    unresolved.remove(&name);
                    let value = if slot.node.is_object() {
                        slot.node.path("sprite").as_string("").into_owned()
                    } else {
                        slot.node.as_string("").into_owned()
                    };
                    resolved.insert(name, value);
                }
            }
        }
    }
    let mut changed = true;
    while changed {
        changed = false;
        let pending: Vec<(String, String)> = unresolved
            .iter()
            .map(|(name, target)| (name.clone(), target.clone()))
            .collect();
        for (name, target) in pending {
            if let Some(sprite) = resolved.get(&target).cloned() {
                resolved.insert(name.clone(), sprite);
                unresolved.remove(&name);
                changed = true;
            }
        }
    }
    resolved
}

fn locked_models(node: &Node, interner: &Interner, locked: &mut BTreeSet<String>) {
    let turned = |axis| node.path(axis).as_int(0) % 360 != 0;
    if node.is_object()
        && let Some(model) = node.path("model").as_str()
        && node.path("uvlock").as_bool(false)
        && (turned("x") || turned("y") || turned("z"))
        && let Some(id) = ResourceId::parse(interner, model)
    {
        locked.insert(file(interner, id, "models", ".json"));
    }
    for child in node.children() {
        locked_models(child, interner, locked);
    }
}

/// Before atlas files, a model's textures are stitched into the blocks atlas because a model names them.
fn legacy(view: &View) -> bool {
    view.formats.min.0 < ATLAS_FILES
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_written_in_their_shortest_exact_form() {
        assert_eq!(text(16.0), "16");
        assert_eq!(text(0.5), "0.5");
        assert_eq!(text(-0.0), "0");
        assert_eq!(text(0.1), "0.1");
        assert_eq!(text(5.333_333_5), "5.3333335");
        assert_eq!(one_decimal(87.5), "87.5");
        assert_eq!(one_decimal(0.15), "0.2");
        assert_eq!(one_decimal(54.25), "54.3");
        assert_eq!(one_decimal(0.05), "0.1");
        assert_eq!(one_decimal(2.25), "2.3");
        assert_eq!(one_decimal(54.190_118_741_2), "54.2");
    }
}
