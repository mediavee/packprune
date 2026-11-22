//! font-dedupe, font-pack, font-trim and font-downscale: bitmap font providers and sheets. [`Font`], [`Bitmap`]
//! and [`Grid`] are what these passes read of a font; [`Readers`] says whether a sheet is that provider's alone.

mod dedupe;
mod downscale;
mod pack;
mod trim;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};

use packprune_codec::png::Image;
use packprune_graph::Names;
use packprune_pack::json::{self, Node};
use packprune_pack::{FileKind, LogicalPath, PhysPath, ResourceId};
use packprune_pass::{Pass, PassCx};
use sha2::{Digest, Sha256};

use crate::textures::{Use, file};

pub fn passes() -> Vec<Box<dyn Pass>> {
    vec![
        Box::new(dedupe::FontDedupe),
        Box::new(pack::FontPack),
        Box::new(trim::FontTrim),
        Box::new(downscale::FontDownscale),
    ]
}

/// What a pack file path holds when it replaces a text core shader: glyphs are then compared cell by cell.
const TEXT_SHADER: &str = "/shaders/core/rendertype_text";

fn has_text_shader(cx: &PassCx<'_>) -> bool {
    cx.pack
        .files
        .keys()
        .any(|path| path.as_str().contains(TEXT_SHADER))
}

/// A font file some view shows, with a tree both of the client's readers agree on.
struct Font<'a> {
    path: &'a PhysPath,
    logical: LogicalPath,
    bytes: &'a [u8],
    root: Node,
    /// The indexes of the views that show this file.
    views: Vec<usize>,
}

impl<'a> Font<'a> {
    /// Every font file a view shows and [`parse`] reads, in path order.
    fn all(cx: &PassCx<'a>) -> Vec<Self> {
        let (pack, layout) = (cx.pack, cx.layout);
        layout
            .kinds
            .iter()
            .filter(|(_, kind)| **kind == FileKind::Font)
            .filter_map(|(path, _)| {
                let entry = pack.files.get(path)?;
                let logical = layout.split(path).1?;
                let views: Vec<usize> = layout
                    .views
                    .iter()
                    .enumerate()
                    .filter(|(_, view)| view.files.get(&logical) == Some(path))
                    .map(|(index, _)| index)
                    .collect();
                if views.is_empty() {
                    return None;
                }
                Some(Self {
                    path,
                    logical,
                    bytes: &entry.blob,
                    root: parse(&entry.blob)?,
                    views,
                })
            })
            .collect()
    }

    fn providers(&self) -> &[Node] {
        self.root.path("providers").items()
    }

    /// The one physical file `logical` is in every view that shows the font.
    fn resolve(&self, cx: &PassCx<'a>, logical: &LogicalPath) -> Option<&'a PhysPath> {
        let layout = cx.layout;
        let mut found = None;
        for &view in &self.views {
            let path = layout.views[view].files.get(logical)?;
            if found.is_some_and(|found| found != path) {
                return None;
            }
            found = Some(path);
        }
        found
    }
}

/// The font's tree when both readers read the same one: valid UTF-8 (Gson's reader replaces what Jackson decodes),
/// no lone surrogate (the lenient grammar refuses it), the strict grammar (the oracle's), and a `providers` array.
fn parse(bytes: &[u8]) -> Option<Node> {
    std::str::from_utf8(bytes).ok()?;
    json::parse_lenient(bytes).ok()?;
    let root = json::parse(bytes).ok()?;
    root.path("providers").is_array().then_some(root)
}

/// A provider's `type`, without a `minecraft:` namespace.
fn provider_type(provider: &Node) -> &str {
    let text = provider.path("type").as_str().unwrap_or("");
    text.strip_prefix("minecraft:").unwrap_or(text)
}

/// No `filter`, or an empty one: the provider is active whatever the options.
fn always(provider: &Node) -> bool {
    provider
        .get("filter")
        .is_none_or(|filter| filter.is_object() && filter.size() == 0)
}

/// A bitmap provider the client loads: its sheet, its codepoint grid, `height` and `ascent` written as integers.
#[derive(Clone, Debug)]
struct Bitmap {
    index: usize,
    texture: LogicalPath,
    rows: Vec<Vec<char>>,
    /// Written, or the default 8.
    height: i32,
    ascent: i32,
}

impl Bitmap {
    /// `None` for anything the client's codec refuses or this module does not edit: no PNG sheet, rows of differing
    /// lengths, `ascent` above `height`, numbers that are not integers.
    fn read(cx: &PassCx<'_>, index: usize, provider: &Node) -> Option<Self> {
        if provider_type(provider) != "bitmap" {
            return None;
        }
        let id = ResourceId::parse(&cx.run.interner, provider.path("file").as_str()?)?;
        let texture = LogicalPath::new(&file(&cx.run.interner, id, "textures", "")).ok()?;
        if !texture.as_str().ends_with(".png") {
            return None;
        }
        let rows = chars(provider)?;
        let cols = rows.first()?.len();
        if cols == 0 || rows.iter().any(|row| row.len() != cols) {
            return None;
        }
        let ascent = provider.path("ascent").int()?;
        let height = match provider.get("height") {
            None => 8,
            Some(node) => node.int()?,
        };
        (height > 0 && ascent <= height).then_some(Self {
            index,
            texture,
            rows,
            height,
            ascent,
        })
    }

    /// The GUI pixel row of the cell's top edge.
    fn top(&self) -> f32 {
        7.0f32 - self.ascent as f32
    }
}

/// The rows of `chars` when it is an array of strings.
fn chars(provider: &Node) -> Option<Vec<Vec<char>>> {
    let chars = provider.path("chars");
    if !chars.is_array() {
        return None;
    }
    chars
        .items()
        .iter()
        .map(|row| row.as_str().map(|text| text.chars().collect()))
        .collect()
}

/// A sheet cut into whole cells, one per codepoint of the grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Grid {
    cols: u32,
    rows: u32,
    cell_w: u32,
    cell_h: u32,
}

impl Grid {
    fn of(image: &Image, bitmap: &Bitmap) -> Option<Self> {
        let cols = u32::try_from(bitmap.rows[0].len()).ok()?;
        let rows = u32::try_from(bitmap.rows.len()).ok()?;
        let whole = image.width >= cols
            && image.height >= rows
            && image.width.is_multiple_of(cols)
            && image.height.is_multiple_of(rows);
        whole.then_some(Self {
            cols,
            rows,
            cell_w: image.width / cols,
            cell_h: image.height / rows,
        })
    }

    /// The GUI pixels per texel of `bitmap` drawn from this grid.
    fn scale(&self, bitmap: &Bitmap) -> f32 {
        bitmap.height as f32 / self.cell_h as f32
    }
}

fn rgba(image: &Image, x: u32, y: u32) -> [u8; 4] {
    let at = (y as usize * image.width as usize + x as usize) * 4;
    [
        image.rgba[at],
        image.rgba[at + 1],
        image.rgba[at + 2],
        image.rgba[at + 3],
    ]
}

/// `BitmapProvider.getActualGlyphWidth`: one past the last column of the cell with a non-zero alpha.
fn actual_width(image: &Image, x0: u32, y0: u32, width: u32, height: u32) -> u32 {
    (0..width)
        .rev()
        .find(|&x| (0..height).any(|y| rgba(image, x0 + x, y0 + y)[3] != 0))
        .map_or(0, |x| x + 1)
}

/// The advance `BitmapProvider` gives a glyph `actual` texels wide at `scale` GUI pixels per texel.
fn advance(actual: u32, scale: f32) -> i32 {
    ((0.5 + f64::from(actual as f32 * scale)) as i32).wrapping_add(1)
}

/// What the oracle compares of a glyph when no text shader replaces the client's: its advance, and every non-zero
/// texel with its GUI rectangle, here as a digest.
fn glyph(image: &Image, x0: u32, y0: u32, grid: &Grid, scale: f32, top: f32) -> (i32, [u8; 32]) {
    let mut hasher = Sha256::new();
    for py in 0..grid.cell_h {
        let y0_bits = (top + py as f32 * scale).to_bits().to_le_bytes();
        let y1_bits = (top + (py + 1) as f32 * scale).to_bits().to_le_bytes();
        for px in 0..grid.cell_w {
            let texel = rgba(image, x0 + px, y0 + py);
            if texel == [0; 4] {
                continue;
            }
            hasher.update((px as f32 * scale).to_bits().to_le_bytes());
            hasher.update(y0_bits);
            hasher.update(((px + 1) as f32 * scale).to_bits().to_le_bytes());
            hasher.update(y1_bits);
            hasher.update(texel);
        }
    }
    let actual = actual_width(image, x0, y0, grid.cell_w, grid.cell_h);
    (advance(actual, scale), hasher.finalize().into())
}

/// Who may read a texture besides the one bitmap provider a pass would change: every indexed reference that names
/// it by path, by file and pointer, and the atlases that stitch it.
struct Readers<'v> {
    by_texture: BTreeMap<&'v str, BTreeSet<(&'v PhysPath, &'v str)>>,
    usage: &'v Use<'v>,
}

impl<'v> Readers<'v> {
    fn new(cx: &PassCx<'v>, usage: &'v Use<'v>) -> Self {
        let mut by_texture: BTreeMap<&str, BTreeSet<(&PhysPath, &str)>> = BTreeMap::new();
        for (path, references) in cx.index.all() {
            for reference in references {
                let logical = match &reference.names {
                    Names::File { logical, .. }
                    | Names::Text { logical }
                    | Names::Beside { logical } => logical,
                    _ => continue,
                };
                by_texture
                    .entry(logical.as_str())
                    .or_default()
                    .insert((path, &*reference.pointer.0));
            }
        }
        Self { by_texture, usage }
    }

    /// Why provider `index` of `font` may not change `texture`, or `None` when nothing else reads it: no other
    /// reference names its path, no atlas stitches it, and it is neither vanilla nor named outside the pack.
    fn shared(
        &self,
        cx: &PassCx<'_>,
        font: &PhysPath,
        index: usize,
        texture: &LogicalPath,
    ) -> Option<&'static str> {
        let pointer = format!("/providers/{index}/file");
        let others = self
            .by_texture
            .get(texture.as_str())
            .is_some_and(|readers| {
                readers
                    .iter()
                    .any(|(path, at)| *path != font || *at != pointer)
            });
        if others {
            Some("another provider or file loads the sheet")
        } else if !self.usage.placements(texture.as_str()).is_empty() {
            Some("an atlas stitches the sheet")
        } else if cx.vanilla.has(texture.as_str())
            || cx.vanilla.names.is_empty() && texture.as_str().starts_with("assets/minecraft/")
        {
            Some("the sheet is a vanilla path")
        } else if cx.kept_names.contains(texture) {
            Some("a usage source names the sheet")
        } else {
            None
        }
    }
}

/// `count` and its noun, plural past one.
fn plural(count: usize, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
}
