//! font-trim: removes the bitmap providers no glyph is drawn from, and crops each sheet only one provider loads to
//! the texels its glyphs draw, with `height` and `ascent` moved so that every glyph keeps its advance, its scale and
//! the GUI rectangle of each texel.

use std::collections::{BTreeMap, BTreeSet};

use packprune_codec::png::{self, Image};
use packprune_graph::Names;
use packprune_pack::json::{self, Node, Splice, Writer};
use packprune_pack::{LogicalPath, PhysPath};
use packprune_pass::{Edits, Loss, Pass, PassCx, PassError, Stage};
use rayon::prelude::*;

use super::{
    Bitmap, Font, Grid, Readers, actual_width, advance, always, chars, glyph, has_text_shader,
    plural, provider_type, rgba,
};
use crate::textures::{Use, Views, no_options};

const NAME: &str = "font-trim";

/// The widest advance a shadowing glyph may have for the provider it shadows to be removed.
const MAX_SHADOW_ADVANCE: f32 = 32.0;

pub(super) struct FontTrim;

impl Pass for FontTrim {
    fn name(&self) -> &'static str {
        NAME
    }

    fn stage(&self) -> Stage {
        Stage::Fonts
    }

    fn loss(&self) -> Loss {
        Loss::Lossless
    }

    fn options_schema(&self) -> schemars::Schema {
        no_options()
    }

    fn run(&self, cx: &PassCx<'_>) -> Result<Edits, PassError> {
        let mut edits = Edits::default();
        if has_text_shader(cx) {
            edits.changes.push(
                "skipped: the pack replaces a text core shader, which may read any texel of a sheet".into(),
            );
            return Ok(edits);
        }
        let views = Views::new(cx, NAME)?;
        let usage = Use::new(cx, &views);
        let readers = Readers::new(cx, &usage);
        let inlined = inlined(cx);
        let fonts = Font::all(cx);
        let mut kept: BTreeMap<&'static str, usize> = BTreeMap::new();
        let mut plans = Vec::new();
        for font in &fonts {
            if !cx.scope.contains(font.path) || cx.pinned.contains(font.path) {
                continue;
            }
            let removed = removable(cx, font, inlined.contains(&font.logical));
            let mut sheets = Vec::new();
            for (index, provider) in font.providers().iter().enumerate() {
                if removed.contains_key(&index) {
                    continue;
                }
                let Some(bitmap) = Bitmap::read(cx, index, provider) else {
                    continue;
                };
                match candidate(cx, &readers, font, &bitmap) {
                    Ok(sheet) => sheets.push((bitmap, sheet)),
                    Err(why) => *kept.entry(why).or_default() += 1,
                }
            }
            if !removed.is_empty() || !sheets.is_empty() {
                plans.push(Plan {
                    font,
                    removed,
                    sheets,
                });
            }
        }

        let files = &cx.pack.files;
        let jobs: Vec<(&Bitmap, &PhysPath)> = plans
            .iter()
            .flat_map(|plan| plan.sheets.iter().map(|(bitmap, sheet)| (bitmap, *sheet)))
            .collect();
        let outcomes: Vec<Result<Outcome, PassError>> = jobs
            .par_iter()
            .map(|&(bitmap, sheet)| {
                let Ok(image) = png::decode(&files[sheet].blob) else {
                    return Ok(Outcome::Kept("the sheet does not decode"));
                };
                let Some(trimmed) = trim(&image, bitmap) else {
                    return Ok(Outcome::Kept("no texel to trim"));
                };
                let bytes = png::encode(&trimmed.image).map_err(|error| PassError::Failed {
                    pass: NAME,
                    why: format!("{sheet}: {error}"),
                })?;
                Ok(Outcome::Trimmed {
                    before: (image.width, image.height),
                    after: (trimmed.image.width, trimmed.image.height),
                    fields: trimmed.fields,
                    bytes,
                })
            })
            .collect();

        let mut outcomes = outcomes.into_iter();
        let (mut sheets, mut providers, mut texels) = (0, 0, 0u64);
        for plan in &plans {
            let mut changed: BTreeMap<usize, Fields> = BTreeMap::new();
            for (index, why) in &plan.removed {
                edits.changes.push(format!(
                    "{}: provider {index} removed: {}",
                    plan.font.path,
                    why.text()
                ));
                providers += 1;
            }
            for (bitmap, sheet) in &plan.sheets {
                match outcomes.next().expect("one outcome per sheet")? {
                    Outcome::Kept(why) => *kept.entry(why).or_default() += 1,
                    Outcome::Trimmed {
                        before: (width, height),
                        after,
                        fields,
                        bytes,
                    } => {
                        texels += u64::from(width) * u64::from(height)
                            - u64::from(after.0) * u64::from(after.1);
                        edits.changes.push(format!(
                            "{sheet}: {width}x{height} to {}x{}{}",
                            after.0,
                            after.1,
                            fields.describe(bitmap)
                        ));
                        edits.replaced.insert((*sheet).clone(), bytes.into());
                        changed.insert(bitmap.index, fields);
                        sheets += 1;
                    }
                }
            }
            let written = changed
                .values()
                .any(|fields| fields.rows.is_some() || fields.metrics.is_some());
            if !plan.removed.is_empty() || written {
                let bytes = rewrite(plan.font, &plan.removed, &changed);
                if bytes != plan.font.bytes {
                    edits.replaced.insert(plan.font.path.clone(), bytes.into());
                }
            }
        }
        if providers > 0 {
            edits
                .changes
                .push(format!("{} removed", plural(providers, "provider")));
        }
        if sheets > 0 {
            edits.changes.push(format!(
                "{} trimmed, {texels} texels fewer",
                plural(sheets, "sheet")
            ));
        }
        for (why, count) in kept {
            edits
                .changes
                .push(format!("kept {}: {why}", plural(count, "sheet")));
        }
        Ok(edits)
    }
}

/// One font's removed providers and the sheets of its other bitmap providers that may be trimmed.
struct Plan<'f, 'a> {
    font: &'f Font<'a>,
    removed: BTreeMap<usize, Removal>,
    sheets: Vec<(Bitmap, &'a PhysPath)>,
}

enum Outcome {
    Kept(&'static str),
    /// The sheet's size before and after, the provider's new fields and the sheet's new bytes.
    Trimmed {
        before: (u32, u32),
        after: (u32, u32),
        fields: Fields,
        bytes: Vec<u8>,
    },
}

/// Why a provider draws no glyph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Removal {
    /// Its grid holds only `\0`, which the client skips.
    Empty,
    /// An earlier provider draws each of its codepoints, so the client never reaches it.
    Shadowed,
}

impl Removal {
    fn text(self) -> &'static str {
        match self {
            Self::Empty => "its grid is only \\0",
            Self::Shadowed => "earlier providers draw each of its codepoints",
        }
    }
}

/// The fonts a reference provider names. Their providers are read again under the referencing provider's filter,
/// where the oracle no longer stops at the first provider that draws a glyph.
fn inlined(cx: &PassCx<'_>) -> BTreeSet<LogicalPath> {
    cx.index
        .all()
        .values()
        .flatten()
        .filter(|reference| &*reference.reason.0 == "font.reference")
        .filter_map(|reference| match &reference.names {
            Names::File { logical, .. } => Some(logical.clone()),
            _ => None,
        })
        .collect()
}

/// The providers of `font` no glyph is drawn from: bitmap providers whose grid holds only `\0`; and, in a font no
/// reference provider inlines, always-active bitmap providers each of whose codepoints an earlier always-active
/// bitmap or space provider draws, with an advance in `0..=32` and no provider of another type between them. One
/// provider always stays, so the font is still defined.
fn removable(cx: &PassCx<'_>, font: &Font<'_>, inlined: bool) -> BTreeMap<usize, Removal> {
    let providers = font.providers();
    let mut removed = BTreeMap::new();
    // The first provider drawing each codepoint since the last provider of another type, and what each draws once
    // read.
    let mut drawn: BTreeMap<char, usize> = BTreeMap::new();
    let mut loaded: BTreeMap<usize, Option<BTreeMap<char, f32>>> = BTreeMap::new();
    for (index, provider) in providers.iter().enumerate() {
        match provider_type(provider) {
            "bitmap" => {
                let Some(rows) = chars(provider) else {
                    continue;
                };
                let codepoints: BTreeSet<char> = rows
                    .iter()
                    .flatten()
                    .copied()
                    .filter(|&c| c != '\0')
                    .collect();
                if codepoints.is_empty() {
                    removed.insert(index, Removal::Empty);
                    continue;
                }
                if !always(provider) {
                    continue;
                }
                let shadowed = !inlined
                    && codepoints.iter().all(|c| {
                        drawn.get(c).is_some_and(|&by| {
                            loaded
                                .entry(by)
                                .or_insert_with(|| advances(cx, font, by, &providers[by]))
                                .as_ref()
                                .and_then(|advances| advances.get(c))
                                .is_some_and(|advance| (0.0..=MAX_SHADOW_ADVANCE).contains(advance))
                        })
                    });
                if shadowed {
                    removed.insert(index, Removal::Shadowed);
                } else if Bitmap::read(cx, index, provider).is_some() {
                    for c in codepoints {
                        drawn.entry(c).or_insert(index);
                    }
                }
            }
            "space" => {
                if always(provider) {
                    for member in provider.path("advances").members() {
                        if let Some(c) = one_char(&member.name) {
                            drawn.entry(c).or_insert(index);
                        }
                    }
                }
            }
            _ => drawn.clear(),
        }
    }
    if removed.len() == providers.len() {
        removed.pop_first();
    }
    removed
}

fn one_char(text: &str) -> Option<char> {
    let mut chars = text.chars();
    let c = chars.next()?;
    chars.next().is_none().then_some(c)
}

/// The advance of each codepoint provider `index` of `font` draws, when it is a space provider the client's codec
/// reads or a bitmap provider whose sheet is the same pack file in every view that shows the font.
fn advances(
    cx: &PassCx<'_>,
    font: &Font<'_>,
    index: usize,
    provider: &Node,
) -> Option<BTreeMap<char, f32>> {
    match provider_type(provider) {
        "space" => {
            let advances = provider.path("advances");
            if !advances.is_object() {
                return None;
            }
            advances
                .members()
                .iter()
                .map(|member| {
                    let advance = member.node.double_value().filter(|v| v.is_finite())?;
                    Some((one_char(&member.name)?, advance as f32))
                })
                .collect()
        }
        "bitmap" => {
            let bitmap = Bitmap::read(cx, index, provider)?;
            let sheet = font.resolve(cx, &bitmap.texture)?;
            let image = png::decode(&cx.pack.files.get(sheet)?.blob).ok()?;
            let grid = Grid::of(&image, &bitmap)?;
            let scale = grid.scale(&bitmap);
            let mut out = BTreeMap::new();
            for (row, codepoints) in (0..).zip(&bitmap.rows) {
                for (col, &c) in (0..).zip(codepoints) {
                    if c != '\0' {
                        let actual = actual_width(
                            &image,
                            col * grid.cell_w,
                            row * grid.cell_h,
                            grid.cell_w,
                            grid.cell_h,
                        );
                        out.insert(c, advance(actual, scale) as f32);
                    }
                }
            }
            Some(out)
        }
        _ => None,
    }
}

/// The sheet of `bitmap` when this pass may rewrite it: the same pack file in every view that shows the font, in the
/// scope, not pinned, and read by nothing but this provider.
fn candidate<'a>(
    cx: &PassCx<'a>,
    readers: &Readers<'_>,
    font: &Font<'a>,
    bitmap: &Bitmap,
) -> Result<&'a PhysPath, &'static str> {
    let sheet = font
        .resolve(cx, &bitmap.texture)
        .ok_or("the sheet is not one pack file in every view that shows the font")?;
    if !cx.scope.contains(sheet) {
        return Err("outside the scope");
    }
    if cx.pinned.contains(sheet) {
        return Err("a source names the sheet's path");
    }
    match readers.shared(cx, font.path, bitmap.index, &bitmap.texture) {
        Some(why) => Err(why),
        None => Ok(sheet),
    }
}

/// A sheet cropped to what its glyphs draw, and the provider fields that keep each glyph in place.
#[derive(Debug)]
pub(super) struct Trimmed {
    pub image: Image,
    pub fields: Fields,
}

/// What a trim writes into its provider, and the cell sizes and grid lines it cut for the report.
#[derive(Debug)]
pub(super) struct Fields {
    /// The new `chars`, when rows or columns of `\0` went.
    pub rows: Option<Vec<String>>,
    /// The new `height` and `ascent`, when texel rows of the cells went.
    pub metrics: Option<(i32, i32)>,
    cells: ((u32, u32), (u32, u32)),
    cut: (usize, usize),
}

impl Fields {
    fn describe(&self, bitmap: &Bitmap) -> String {
        let ((w0, h0), (w1, h1)) = self.cells;
        let mut out = format!(" (cells {w0}x{h0} to {w1}x{h1}");
        if self.cut != (0, 0) {
            out.push_str(&format!(
                "; {} and {} of \\0 removed",
                plural(self.cut.0, "row"),
                plural(self.cut.1, "column")
            ));
        }
        if let Some((height, ascent)) = self.metrics {
            out.push_str(&format!(
                "; height {} to {height}, ascent {} to {ascent}",
                bitmap.height, bitmap.ascent
            ));
        }
        out.push(')');
        out
    }
}

/// `image`, the sheet of `bitmap`, without what no glyph draws: the grid rows and columns of `\0`, then the cells'
/// fully transparent black (`[0, 0, 0, 0]`) right, top and bottom margins. A top or bottom cut must keep the scale
/// (`height` over the cell height) and give integer `height` and `ascent`, `ascent` not above `height`, and every
/// texel the same GUI rectangle in `f32`. Every glyph is then compared with the oracle's description of it: `None`
/// when one differs or when nothing would change.
pub(super) fn trim(image: &Image, bitmap: &Bitmap) -> Option<Trimmed> {
    let grid = Grid::of(image, bitmap)?;
    let (cell_w, cell_h) = (grid.cell_w, grid.cell_h);
    let mut keep_rows: Vec<u32> = (0..grid.rows)
        .filter(|&row| bitmap.rows[row as usize].iter().any(|&c| c != '\0'))
        .collect();
    let mut keep_cols: Vec<u32> = (0..grid.cols)
        .filter(|&col| bitmap.rows.iter().any(|row| row[col as usize] != '\0'))
        .collect();
    if keep_rows.is_empty() {
        (keep_rows, keep_cols) = (vec![0], vec![0]);
    }
    let cells: Vec<(u32, u32)> = keep_rows
        .iter()
        .flat_map(|&row| keep_cols.iter().map(move |&col| (row, col)))
        .collect();
    let drawn = |row: u32, col: u32| bitmap.rows[row as usize][col as usize] != '\0';
    let filled = |row: u32, col: u32, x: u32, y: u32| {
        rgba(image, col * cell_w + x, row * cell_h + y) != [0; 4]
    };

    let width = cells
        .iter()
        .filter_map(|&(row, col)| {
            (0..cell_w)
                .rev()
                .find(|&x| (0..cell_h).any(|y| filled(row, col, x, y)))
        })
        .max()
        .map_or(1, |x| x + 1);
    let any = |row_filter: &dyn Fn(u32, u32) -> bool| -> Vec<bool> {
        (0..cell_h)
            .map(|y| {
                cells.iter().any(|&(row, col)| {
                    row_filter(row, col) && (0..width).any(|x| filled(row, col, x, y))
                })
            })
            .collect()
    };
    let used = any(&|_, _| true);
    let active = any(&drawn);
    let top_free = used
        .iter()
        .position(|&used| used)
        .map_or(cell_h, |y| y as u32);
    let bottom_free = used
        .iter()
        .rposition(|&used| used)
        .map_or(cell_h, |y| cell_h - 1 - y as u32);
    let (cut_top, cut_bottom, metrics) =
        match vertical(bitmap, cell_h, top_free, bottom_free, &active) {
            Some((top, bottom, height, ascent)) => (top, bottom, Some((height, ascent))),
            None => (0, 0, None),
        };
    let height = cell_h - cut_top - cut_bottom;
    let (rows, cols) = (keep_rows.len() as u32, keep_cols.len() as u32);
    if rows == grid.rows && cols == grid.cols && width == cell_w && metrics.is_none() {
        return None;
    }

    let mut out = Image {
        width: cols * width,
        height: rows * height,
        rgba: Vec::with_capacity(
            cols as usize * width as usize * rows as usize * height as usize * 4,
        ),
    };
    for &row in &keep_rows {
        for y in 0..height {
            let source_y = row * cell_h + cut_top + y;
            for &col in &keep_cols {
                let start =
                    (source_y as usize * image.width as usize + (col * cell_w) as usize) * 4;
                out.rgba
                    .extend_from_slice(&image.rgba[start..start + width as usize * 4]);
            }
        }
    }

    let mut after = bitmap.clone();
    if let Some((height, ascent)) = metrics {
        (after.height, after.ascent) = (height, ascent);
    }
    let new_grid = Grid {
        cols,
        rows,
        cell_w: width,
        cell_h: height,
    };
    let (scale, new_scale) = (grid.scale(bitmap), new_grid.scale(&after));
    if scale.to_bits() != new_scale.to_bits() {
        return None;
    }
    for (new_row, &row) in (0..).zip(&keep_rows) {
        for (new_col, &col) in (0..).zip(&keep_cols) {
            if !drawn(row, col) {
                continue;
            }
            let before = glyph(
                image,
                col * cell_w,
                row * cell_h,
                &grid,
                scale,
                bitmap.top(),
            );
            let now = glyph(
                &out,
                new_col * width,
                new_row * height,
                &new_grid,
                new_scale,
                after.top(),
            );
            if before != now {
                return None;
            }
        }
    }
    let cut = (
        grid.rows as usize - keep_rows.len(),
        grid.cols as usize - keep_cols.len(),
    );
    let rows = (cut != (0, 0)).then(|| {
        keep_rows
            .iter()
            .map(|&row| {
                keep_cols
                    .iter()
                    .map(|&col| bitmap.rows[row as usize][col as usize])
                    .collect()
            })
            .collect()
    });
    Some(Trimmed {
        image: out,
        fields: Fields {
            rows,
            metrics,
            cells: ((cell_w, cell_h), (width, height)),
            cut,
        },
    })
}

/// The most texel rows to cut from the top and bottom of the cells, at most `top_free` and `bottom_free`, with the
/// `height` and `ascent` that keep every `active` row's GUI edges in `f32`: `(top, bottom, height, ascent)`.
fn vertical(
    bitmap: &Bitmap,
    cell_h: u32,
    top_free: u32,
    bottom_free: u32,
    active: &[bool],
) -> Option<(u32, u32, i32, i32)> {
    let (height, ascent, cell) = (
        i64::from(bitmap.height),
        i64::from(bitmap.ascent),
        i64::from(cell_h),
    );
    let scale = bitmap.height as f32 / cell_h as f32;
    let top = bitmap.top();
    let most = (top_free + bottom_free).min(cell_h - 1);
    for total in (1..=most).rev() {
        for cut_top in total.saturating_sub(bottom_free)..=total.min(top_free) {
            let rows = cell - i64::from(total);
            let shift = i64::from(cut_top) * height;
            if (height * rows) % cell != 0 || shift % cell != 0 {
                continue;
            }
            let (Ok(new_height), Ok(new_ascent)) = (
                i32::try_from(height * rows / cell),
                i32::try_from(ascent - shift / cell),
            ) else {
                continue;
            };
            let new_scale = new_height as f32 / rows as f32;
            if new_ascent > new_height || new_scale.to_bits() != scale.to_bits() {
                continue;
            }
            let new_top = 7.0f32 - new_ascent as f32;
            let kept = (0..cell_h).filter(|&y| active[y as usize]).all(|y| {
                [0, 1].iter().all(|&edge| {
                    (new_top + (y - cut_top + edge) as f32 * new_scale).to_bits()
                        == (top + (y + edge) as f32 * scale).to_bits()
                })
            });
            if kept {
                return Some((cut_top, total - cut_top, new_height, new_ascent));
            }
        }
    }
    None
}

/// The font's bytes without the providers of `removed`, and each provider of `changed` with its new fields; every
/// other byte stays as it was.
fn rewrite(
    font: &Font<'_>,
    removed: &BTreeMap<usize, Removal>,
    changed: &BTreeMap<usize, Fields>,
) -> Vec<u8> {
    let array = font.root.path("providers");
    let providers = array.items();
    if removed.is_empty() {
        let splices = changed
            .iter()
            .flat_map(|(&index, changed)| fields(&providers[index], changed, 0))
            .collect();
        return json::splice(font.bytes, splices);
    }
    let mut out = b"[".to_vec();
    for (index, provider) in providers.iter().enumerate() {
        if removed.contains_key(&index) {
            continue;
        }
        if out.len() > 1 {
            out.push(b',');
        }
        let (start, end) = (provider.span.start as usize, provider.span.end as usize);
        let own = &font.bytes[start..end];
        match changed.get(&index) {
            Some(changed) => out.extend(json::splice(own, fields(provider, changed, start))),
            None => out.extend_from_slice(own),
        }
    }
    out.push(b']');
    json::splice(
        font.bytes,
        vec![Splice {
            range: array.span.start as usize..array.span.end as usize,
            bytes: out,
        }],
    )
}

/// The splices that write `changed`'s `chars`, `height` and `ascent` into `provider`, ranges counted from `offset`.
fn fields(provider: &Node, changed: &Fields, offset: usize) -> Vec<Splice> {
    let range = |node: &Node| node.span.start as usize - offset..node.span.end as usize - offset;
    let mut splices = Vec::new();
    if let Some(rows) = &changed.rows {
        let mut bytes = Vec::new();
        let mut writer = Writer::new(&mut bytes);
        writer.start_array();
        for row in rows {
            writer.string(row);
        }
        writer.end_array();
        splices.push(Splice {
            range: range(provider.path("chars")),
            bytes,
        });
    }
    if let Some((height, ascent)) = changed.metrics {
        splices.push(Splice {
            range: range(provider.path("ascent")),
            bytes: ascent.to_string().into_bytes(),
        });
        match provider.get("height") {
            Some(node) => splices.push(Splice {
                range: range(node),
                bytes: height.to_string().into_bytes(),
            }),
            None => {
                let end = provider.span.end as usize - offset - 1;
                splices.push(Splice {
                    range: end..end,
                    bytes: format!(r#","height":{height}"#).into_bytes(),
                });
            }
        }
    }
    splices
}
