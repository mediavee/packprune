use std::collections::{BTreeMap, BTreeSet};

use packprune_graph::Names;
use packprune_graph::atlases::Sprite;
use packprune_pack::{FileKind, PhysPath, ResourceId};
use packprune_pass::PassCx;

use super::Views;

const MODEL_TEXTURE: &str = "model.texture";
const ATLAS_SINGLE: &str = "atlas.single";

/// A sprite of one atlas in one view, built from the texture or from several.
#[derive(Clone, Copy)]
pub(crate) struct Placement<'v> {
    pub view: usize,
    pub atlas: &'v str,
    pub id: ResourceId,
    pub sprite: &'v Sprite,
}

impl Placement<'_> {
    pub(crate) fn plain(&self, texture: &str) -> bool {
        self.sprite.inputs.len() == 1
            && &*self.sprite.inputs[0].logical == texture
            && self.sprite.region.is_empty()
    }
}

/// Where the pack's textures are drawn from (the atlas sprites each is an input of, in every view)
/// and the references that name one other than an atlas `single` source or a model's texture slot. A pass that
/// rewrites a texture or merges it into another asks here whether anything else could see the change.
pub(crate) struct Use<'v> {
    cx: &'v PassCx<'v>,
    views: &'v Views<'v>,
    layers: Vec<String>,
    by_texture: BTreeMap<&'v str, Vec<Placement<'v>>>,
    by_id: BTreeMap<ResourceId, Vec<Placement<'v>>>,
    pinned_textures: BTreeSet<&'v str>,
    pinned_ids: BTreeSet<ResourceId>,
    model_ids: BTreeSet<ResourceId>,
}

impl<'v> Use<'v> {
    pub(crate) fn new(cx: &'v PassCx<'v>, views: &'v Views<'v>) -> Self {
        let mut layers = vec![String::new()];
        layers.extend(
            cx.layout
                .meta
                .overlays
                .iter()
                .map(|overlay| format!("{}/", overlay.directory)),
        );
        let mut by_texture: BTreeMap<&str, Vec<Placement<'_>>> = BTreeMap::new();
        let mut by_id: BTreeMap<ResourceId, Vec<Placement<'_>>> = BTreeMap::new();
        for view in 0..views.list.len() {
            for atlas in views.atlases(view).atlas_ids() {
                for (id, sprite) in views.sprites(view, atlas, &cx.run.interner) {
                    let placement = Placement {
                        view,
                        atlas,
                        id,
                        sprite,
                    };
                    by_id.entry(id).or_default().push(placement);
                    for input in &sprite.inputs {
                        by_texture
                            .entry(&input.logical)
                            .or_default()
                            .push(placement);
                    }
                }
            }
        }
        let (mut pinned_textures, mut pinned_ids, mut model_ids) =
            (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
        for reference in cx.index.all().values().flatten() {
            match &reference.names {
                Names::File { logical, .. } => {
                    if FileKind::of(logical.as_str()) == FileKind::Texture
                        && &*reference.reason.0 != ATLAS_SINGLE
                    {
                        pinned_textures.insert(logical.as_str());
                    }
                }
                Names::Text { logical } => {
                    pinned_textures.insert(logical.as_str());
                }
                Names::Sprite { sprite, .. } => {
                    if &*reference.reason.0 == MODEL_TEXTURE {
                        model_ids.insert(*sprite);
                    } else {
                        pinned_ids.insert(*sprite);
                    }
                }
                _ => {}
            }
        }
        Self {
            cx,
            views,
            layers,
            by_texture,
            by_id,
            pinned_textures,
            pinned_ids,
            model_ids,
        }
    }

    /// The physical files of a logical path by layer folder prefix: `""` for the root, `<overlay>/` for an overlay.
    pub(crate) fn layers(&self, logical: &str) -> BTreeMap<&str, PhysPath> {
        self.layers
            .iter()
            .filter_map(|layer| {
                let path = PhysPath::new(&format!("{layer}{logical}")).ok()?;
                self.cx
                    .pack
                    .files
                    .contains_key(&path)
                    .then_some((layer.as_str(), path))
            })
            .collect()
    }

    /// Every texture an atlas of a view loads, by logical path.
    pub(crate) fn stitched(&self) -> impl Iterator<Item = &'v str> + '_ {
        self.by_texture.keys().copied()
    }

    pub(crate) fn placements(&self, texture: &str) -> &[Placement<'v>] {
        self.by_texture.get(texture).map_or(&[], Vec::as_slice)
    }

    /// The one sprite id `texture` has: it is stitched, always alone and whole, under that id, and that id names
    /// nothing else in any atlas of any view, so a reference to the id is a reference to the texture.
    pub(crate) fn sole_id(&self, texture: &str) -> Option<ResourceId> {
        let placements = self.placements(texture);
        if placements.is_empty() || !placements.iter().all(|p| p.plain(texture)) {
            return None;
        }
        let id = placements[0].id;
        let one = placements.iter().all(|p| p.id == id)
            && self.by_id[&id].iter().all(|p| p.plain(texture));
        one.then_some(id)
    }

    /// Each view and atlas that loads `texture`.
    pub(crate) fn slots(&self, texture: &str) -> BTreeSet<(usize, &'v str)> {
        self.placements(texture)
            .iter()
            .map(|p| (p.view, p.atlas))
            .collect()
    }

    /// A font, an equipment layer, a special model, a post effect or other text names the file itself: it is drawn
    /// whole, not as a sprite.
    pub(crate) fn loaded_as_file(&self, texture: &str) -> bool {
        self.pinned_textures.contains(texture)
    }

    /// The vanilla client has the path, so the client or a shader may read it by name.
    pub(crate) fn vanilla(&self, logical: &str) -> bool {
        !self.views.list.is_empty() && self.views.vanilla(logical)
    }

    /// Something other than an atlas `single` source or a model slot names the texture or its sprite.
    pub(crate) fn pinned(&self, texture: &str) -> bool {
        self.pinned_textures.contains(texture)
            || self
                .placements(texture)
                .iter()
                .any(|p| self.pinned_ids.contains(&p.id))
    }

    pub(crate) fn named_by_model(&self, sprite: ResourceId) -> bool {
        self.model_ids.contains(&sprite)
    }
}
