//! Atlas GPU and font memory calculator: what the client allocates for a pack, per view.
//!
//! The atlas model is the 26.2 client's `SpriteLoader` and `Stitcher` and the font model its glyph pages and
//! staging copies. The pack's view is stacked over the client jar as the client's resource manager does; the jar's
//! texture sizes and `.mcmeta` files come from [`packprune_pack::VanillaIndex`].

use packprune_pack::meta::{Format, parse_format};
use packprune_pack::{Layout, PackInput, VanillaIndex, View};
use serde::Serialize;

mod atlases;
mod fonts;
mod read;
mod stack;
mod stitch;
#[cfg(test)]
mod tests;

pub use atlases::{AtlasCost, MAX_TEXTURE_SIZE, SpriteRow};
pub use fonts::{Duplicate, FontCost, PAGE_BYTES};

/// The first format whose client keeps a staging copy of each uploaded font sheet (26.2).
const STAGING_COPY_FORMAT: Format = (88, 0);
/// The first format whose client reads palettes from `textures/palettes/` (26.3).
const PALETTE_DIRECTORY_FORMAT: Format = (97, 1);

#[derive(Debug, thiserror::Error)]
pub enum CostError {
    /// A file the client itself would refuse: `file > field: why`.
    #[error("{0}")]
    Invalid(String),
    #[error("the vanilla index has no client format: it was not built from a client jar")]
    NoClientFormat,
}

/// Client memory of a pack in one view. A part the model cannot size is in `exclusions` and left out of the
/// totals, so the totals are a floor while `complete()` is false.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Cost {
    /// The view's name (`base`, or its overlays joined by `+`).
    pub view: String,
    pub format: Format,
    pub atlases: Vec<AtlasCost>,
    pub fonts: Vec<FontCost>,
    pub duplicates: Vec<Duplicate>,
    pub exclusions: Vec<String>,
}

impl Cost {
    /// Whether every font provider, sprite source and texture was modelled.
    pub fn complete(&self) -> bool {
        self.exclusions.is_empty()
    }

    /// GPU bytes of every atlas texture, mip levels included (`Measured::atlas_gpu_bytes`).
    pub fn atlas_gpu_bytes(&self) -> u64 {
        self.atlases.iter().map(|atlas| atlas.gpu).sum()
    }

    pub fn atlas_decoded_bytes(&self) -> u64 {
        self.atlases.iter().map(|atlas| atlas.decoded).sum()
    }

    /// Client bytes of every font: sheets decoded, staging copies and glyph pages (`Measured::font_bytes`).
    pub fn font_bytes(&self) -> u64 {
        self.fonts.iter().map(|font| font.total).sum()
    }

    /// Bytes decoded again because several providers load the same sheet.
    pub fn redundant_decoded_bytes(&self) -> u64 {
        self.duplicates.iter().map(|d| d.redundant_decoded).sum()
    }
}

/// Insertion-ordered, without repeats.
#[derive(Default)]
pub(crate) struct Exclusions(Vec<String>);

impl Exclusions {
    pub(crate) fn add(&mut self, text: String) {
        if !self.0.contains(&text) {
            self.0.push(text);
        }
    }
}

/// The inputs of a cost: the pack and its layout, and the vanilla index of the client jar.
pub struct Calculator<'a> {
    pub pack: &'a PackInput,
    pub layout: &'a Layout,
    pub vanilla: &'a VanillaIndex,
}

impl Calculator<'_> {
    /// The cost in `view`, a view of `self.layout` or of [`Layout::view_at`].
    pub fn view(&self, view: &View) -> Result<Cost, CostError> {
        let format = view.formats.min;
        let mut exclusions = Exclusions::default();
        let filters: Vec<_> = self
            .layout
            .meta
            .filters
            .iter()
            .map(|filter| (filter.namespace.clone(), filter.path.clone()))
            .collect();
        let invalid = |failure: read::Failure| CostError::Invalid(failure.0);
        let stack = stack::Stack::new(self.pack, view, self.vanilla, &filters, &mut exclusions);
        let fonts = fonts::audit(&stack, format >= STAGING_COPY_FORMAT, &mut exclusions)
            .map_err(invalid)?;
        let atlases = atlases::Atlases::new(
            &stack,
            self.vanilla,
            format >= PALETTE_DIRECTORY_FORMAT,
            &mut exclusions,
        )
        .read()
        .map_err(invalid)?;
        Ok(Cost {
            view: view.name.clone(),
            format,
            atlases,
            fonts: fonts.fonts,
            duplicates: fonts.duplicates,
            exclusions: exclusions.0,
        })
    }

    /// The cost for the client of the vanilla index's jar: the view of its format.
    pub fn client_view(&self) -> Result<Cost, CostError> {
        let format = parse_format(&self.vanilla.pack_format).ok_or(CostError::NoClientFormat)?;
        self.view(&self.layout.view_at(format))
    }
}
