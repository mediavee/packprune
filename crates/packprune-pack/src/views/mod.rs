use std::collections::{BTreeMap, BTreeSet, HashMap};

use smallvec::SmallVec;

use crate::meta::{Format, ViewSpec, overlays_id, parse_format};
use crate::{
    FileKind, FormatRange, Layer, LogicalPath, PackInput, PackMeta, PhysPath, VanillaIndex,
};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ViewId(pub u32);

/// Inline up to 128 views, heap beyond; no limit on the view count.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ViewSet {
    words: SmallVec<[u64; 2]>,
}

/// One view: the layers a client at `formats.min` applies (the root, then the active overlays as `pack.mcmeta` lists
/// them, last wins) and the physical file each logical path under `assets/` resolves to. A view is a set of overlays:
/// `formats` is the single format that selects it (`min == max`), not an interval it covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub id: ViewId,
    /// `base`, the overlays joined by `+`, and `@<format>` when another view with the same overlays is
    /// the verified one.
    pub name: String,
    pub formats: FormatRange,
    pub layers: Vec<Layer>,
    pub files: BTreeMap<LogicalPath, PhysPath>,
    pub verified: bool,
}

/// The files under `assets/` by layer: the root's by logical path, each overlay directory's as listed.
struct Split {
    root: BTreeMap<LogicalPath, PhysPath>,
    overlays: BTreeMap<Box<str>, Vec<(LogicalPath, PhysPath)>>,
}

/// The pack's views and the kind of each of its files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub meta: PackMeta,
    pub views: Vec<View>,
    pub kinds: BTreeMap<PhysPath, FileKind>,
}

/// Old client behaviours a file meets, from the formats of the views that show it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Quirks {
    /// Textures go through `javax.imageio` (before 1.13), whose `getRGB` converts a grey PNG from linear grey to
    /// sRGB: it shows lighter than an RGB one of the same values.
    pub imageio_grey: bool,
    /// `LayeredColorMaskTexture` (before 1.13) draws a banner or shield in the image type `javax.imageio` gives its
    /// base and skips a pattern not of `TYPE_4BYTE_ABGR`, the type of an 8-bit RGBA PNG.
    pub abgr_layers: bool,
    /// `RenderType.eyes` blends with `ONE, ONE` (fixed in 24w39a, MC-235953): a transparent texel's colour adds.
    pub additive_eyes: bool,
    /// Textures go through `javax.imageio` (before 1.13). Without it, stb_image ignores every PNG chunk but the
    /// image data and transparency, and keeps the high byte of a 16-bit sample.
    pub imageio_textures: bool,
}

impl ViewSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, view: ViewId) {
        let (word, bit) = locate(view);
        if self.words.len() <= word {
            self.words.resize(word + 1, 0);
        }
        self.words[word] |= bit;
    }

    pub fn remove(&mut self, view: ViewId) {
        let (word, bit) = locate(view);
        if let Some(slot) = self.words.get_mut(word) {
            *slot &= !bit;
            self.trim();
        }
    }

    pub fn contains(&self, view: ViewId) -> bool {
        let (word, bit) = locate(view);
        self.words.get(word).is_some_and(|slot| slot & bit != 0)
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    pub fn len(&self) -> usize {
        self.words
            .iter()
            .map(|word| word.count_ones() as usize)
            .sum()
    }

    /// The views in ascending order.
    pub fn iter(&self) -> impl Iterator<Item = ViewId> + '_ {
        self.words.iter().enumerate().flat_map(|(index, &word)| {
            (0..64)
                .filter(move |bit| word & (1 << bit) != 0)
                .map(move |bit| ViewId((index * 64 + bit) as u32))
        })
    }

    pub fn union_with(&mut self, other: &Self) {
        if self.words.len() < other.words.len() {
            self.words.resize(other.words.len(), 0);
        }
        for (slot, word) in self.words.iter_mut().zip(&other.words) {
            *slot |= word;
        }
    }

    pub fn intersect_with(&mut self, other: &Self) {
        self.words.truncate(other.words.len());
        for (slot, word) in self.words.iter_mut().zip(&other.words) {
            *slot &= word;
        }
        self.trim();
    }

    /// Trailing zero words dropped, so equal sets compare and hash equal.
    fn trim(&mut self) {
        while self.words.last() == Some(&0) {
            self.words.pop();
        }
    }
}

impl FromIterator<ViewId> for ViewSet {
    fn from_iter<I: IntoIterator<Item = ViewId>>(iter: I) -> Self {
        let mut set = Self::new();
        iter.into_iter().for_each(|view| set.insert(view));
        set
    }
}

fn locate(view: ViewId) -> (usize, u64) {
    ((view.0 / 64) as usize, 1 << (view.0 % 64))
}

impl Layout {
    /// Every file classified by layer and kind, and the views `meta` declares, the one at the client
    /// jar's format verified (every view when the jar's format is unknown).
    pub fn new(pack: &PackInput, meta: PackMeta, vanilla: &VanillaIndex) -> Self {
        let jar = parse_format(&vanilla.pack_format);
        let specs = meta.views(jar);
        let mut layout = Self {
            meta,
            views: Vec::new(),
            kinds: BTreeMap::new(),
        };
        for path in pack.files.keys() {
            let kind = layout.classify(path, vanilla);
            layout.kinds.insert(path.clone(), kind);
        }
        let split = layout.split_all();
        layout.views = specs
            .iter()
            .enumerate()
            .map(|(index, spec)| layout.view(ViewId(index as u32), spec, &split))
            .collect();
        layout
    }

    /// Brings the layout to what [`Self::new`] gives on `pack` with the same `meta`, after `paths` were added to or
    /// removed from it: only their kinds and the logical paths they name are resolved again.
    pub fn update<'a>(
        &mut self,
        pack: &PackInput,
        paths: impl IntoIterator<Item = &'a PhysPath>,
        vanilla: &VanillaIndex,
    ) {
        let mut logicals = BTreeSet::new();
        for path in paths {
            if pack.files.contains_key(path) {
                let kind = self.classify(path, vanilla);
                self.kinds.insert(path.clone(), kind);
            } else {
                self.kinds.remove(path);
            }
            if let (_, Some(logical)) = self.split(path) {
                logicals.insert(logical);
            }
        }
        let mut text = String::new();
        for logical in &logicals {
            let mut candidates: Vec<(Layer, PhysPath)> = Vec::new();
            let overlays = self.meta.overlays.iter().map(|overlay| &*overlay.directory);
            for directory in std::iter::once(None).chain(overlays.map(Some)) {
                text.clear();
                if let Some(directory) = directory {
                    text.push_str(directory);
                    text.push('/');
                }
                text.push_str(logical.as_str());
                let Some((path, _)) = self.kinds.get_key_value(text.as_str()) else {
                    continue;
                };
                let layer =
                    directory.map_or(Layer::Root, |directory| Layer::Overlay(directory.into()));
                if self.split(path) == (layer.clone(), Some(logical.clone())) {
                    candidates.push((layer, path.clone()));
                }
            }
            for view in &mut self.views {
                // The last listed layer holding it wins: a directory listed twice ranks at its last place.
                let winner = view
                    .layers
                    .iter()
                    .rev()
                    .find_map(|layer| candidates.iter().find(|(held, _)| held == layer));
                match winner {
                    Some((_, path)) => view.files.insert(logical.clone(), path.clone()),
                    None => view.files.remove(logical),
                };
            }
        }
    }

    /// The view a client at `format` sees, verified; its id is past the pack's views.
    pub fn view_at(&self, format: Format) -> View {
        self.views_at(&[format]).remove(0)
    }

    /// [`Self::view_at`] of each format.
    pub fn views_at(&self, formats: &[Format]) -> Vec<View> {
        let split = self.split_all();
        formats
            .iter()
            .map(|&format| {
                let overlays = self.meta.active(format);
                let spec = ViewSpec {
                    id: overlays_id(&overlays),
                    format,
                    overlays: overlays.iter().map(|&overlay| overlay.to_owned()).collect(),
                    verified: true,
                };
                self.view(ViewId(self.views.len() as u32), &spec, &split)
            })
            .collect()
    }

    /// The name of [`Self::view_at`], without building the view.
    pub fn view_name_at(&self, format: Format) -> String {
        overlays_id(&self.meta.active(format))
    }

    /// The layer of a physical file and its logical path (`None` outside `assets/`, and for `pack.mcmeta` and
    /// `pack.png`). Only a directory `pack.mcmeta` lists is an overlay.
    pub fn split(&self, path: &PhysPath) -> (Layer, Option<LogicalPath>) {
        let text = path.as_str();
        if text == "pack.mcmeta" || text == "pack.png" {
            return (Layer::Root, None);
        }
        let (layer, logical) = match text.find('/') {
            Some(slash)
                if slash > 0
                    && self
                        .meta
                        .overlays
                        .iter()
                        .any(|overlay| overlay.directory == text[..slash]) =>
            {
                (Layer::Overlay(text[..slash].into()), &text[slash + 1..])
            }
            _ => (Layer::Root, text),
        };
        let logical = logical
            .starts_with("assets/")
            .then(|| LogicalPath::new(logical).expect("a suffix of a valid path"));
        (layer, logical)
    }

    fn classify(&self, path: &PhysPath, vanilla: &VanillaIndex) -> FileKind {
        let text = path.as_str();
        if text == "pack.mcmeta" || text == "pack.png" {
            return FileKind::PackMeta;
        }
        let Some(logical) = self.split(path).1 else {
            return FileKind::Unknown;
        };
        match FileKind::of(logical.as_str()) {
            FileKind::Unknown if vanilla.has(logical.as_str()) => FileKind::Other,
            kind => kind,
        }
    }

    /// Every file under `assets/` split once into its layer and logical path, for the views built from it.
    fn split_all(&self) -> Split {
        let mut root = Vec::new();
        let mut overlays: BTreeMap<Box<str>, Vec<(LogicalPath, PhysPath)>> = BTreeMap::new();
        for path in self.kinds.keys() {
            match self.split(path) {
                (_, None) => {}
                (Layer::Root, Some(logical)) => root.push((logical, path.clone())),
                (Layer::Overlay(directory), Some(logical)) => overlays
                    .entry(directory)
                    .or_default()
                    .push((logical, path.clone())),
            }
        }
        Split {
            root: root.into_iter().collect(),
            overlays,
        }
    }

    /// Overlays listed later take precedence, and every overlay over the pack root.
    fn view(&self, id: ViewId, spec: &ViewSpec, split: &Split) -> View {
        // A directory listed twice ranks at its last place.
        let mut rank: HashMap<&str, usize> = HashMap::new();
        for (index, overlay) in spec.overlays.iter().enumerate() {
            rank.insert(overlay, index + 1);
        }
        let mut ranked: Vec<(usize, &str)> = rank.into_iter().map(|(dir, at)| (at, dir)).collect();
        ranked.sort_unstable();
        let mut files = split.root.clone();
        for (_, directory) in ranked {
            for (logical, path) in split.overlays.get(directory).into_iter().flatten() {
                files.insert(logical.clone(), path.clone());
            }
        }
        // As `pack.mcmeta` lists them: a directory listed twice appears twice and ranks at its last place.
        let layers = std::iter::once(Layer::Root)
            .chain(
                spec.overlays
                    .iter()
                    .map(|overlay| Layer::Overlay(overlay.as_str().into())),
            )
            .collect();
        View {
            id,
            name: spec.id.clone(),
            formats: FormatRange {
                min: spec.format,
                max: spec.format,
            },
            layers,
            files,
            verified: spec.verified,
        }
    }
}
