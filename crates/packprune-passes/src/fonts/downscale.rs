//! font-downscale: resamples the cells of bitmap font sheets that hold more texels per GUI pixel than
//! `max_oversample`, by area in premultiplied linear light, when every glyph keeps its advance. The provider's
//! `height` stays, so each glyph keeps its size on screen with fewer texels.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use packprune_codec::metrics;
use packprune_codec::png::{self, Image};
use packprune_pack::PhysPath;
use packprune_pass::{Edits, Loss, LossRecord, Pass, PassCx, PassError, Stage};
use rayon::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;

use super::{Bitmap, Font, Grid, Readers, actual_width, advance, plural, rgba};
use crate::textures::{Use, Views};

const NAME: &str = "font-downscale";

pub(super) struct FontDownscale;

#[derive(Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct Options {
    /// The most texels per GUI pixel a cell keeps: a cell taller than `height` times this is resampled.
    max_oversample: f64,
    min_ssim: Option<f64>,
    max_delta: Option<u8>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_oversample: 2.0,
            min_ssim: None,
            max_delta: None,
        }
    }
}

impl Options {
    fn read(options: &toml::Table) -> Result<Self, String> {
        let options: Self = options
            .clone()
            .try_into()
            .map_err(|error: toml::de::Error| error.to_string())?;
        if !options.max_oversample.is_finite()
            || options.max_oversample <= 0.0
            || options
                .min_ssim
                .is_some_and(|v| !v.is_finite() || !(-1.0..=1.0).contains(&v))
        {
            return Err("max_oversample must be finite and positive, min_ssim in [-1, 1]".into());
        }
        Ok(options)
    }
}

impl Pass for FontDownscale {
    fn name(&self) -> &'static str {
        NAME
    }

    fn stage(&self) -> Stage {
        Stage::Lossy
    }

    fn loss(&self) -> Loss {
        Loss::Lossy
    }

    fn options_schema(&self) -> schemars::Schema {
        schemars::schema_for!(Options)
    }

    fn validate_options(&self, options: &toml::Table) -> Result<(), String> {
        Options::read(options).map(drop)
    }

    fn run(&self, cx: &PassCx<'_>) -> Result<Edits, PassError> {
        let options =
            Options::read(cx.options).map_err(|why| PassError::Refused { pass: NAME, why })?;
        let views = Views::new(cx, NAME)?;
        let usage = Use::new(cx, &views);
        let readers = Readers::new(cx, &usage);
        let fonts = Font::all(cx);
        let mut kept: BTreeMap<&'static str, usize> = BTreeMap::new();
        let mut jobs: Vec<(Bitmap, &PhysPath)> = Vec::new();
        for font in &fonts {
            for (index, provider) in font.providers().iter().enumerate() {
                let Some(bitmap) = Bitmap::read(cx, index, provider) else {
                    continue;
                };
                let Some(sheet) = font.resolve(cx, &bitmap.texture) else {
                    *kept
                        .entry("the sheet is not one pack file in every view that shows the font")
                        .or_default() += 1;
                    continue;
                };
                let why = if !cx.scope.contains(sheet) {
                    // Out of the globs: not this pass's file, not counted.
                    continue;
                } else if cx.pinned.contains(sheet) {
                    Some("a source names the sheet's path")
                } else {
                    readers.shared(cx, font.path, index, &bitmap.texture)
                };
                match why {
                    Some(why) => *kept.entry(why).or_default() += 1,
                    None => jobs.push((bitmap, sheet)),
                }
            }
        }

        let files = &cx.pack.files;
        let outcomes: Vec<Result<Outcome, PassError>> = jobs
            .par_iter()
            .map(|(bitmap, sheet)| {
                let failed = |error: packprune_codec::CodecError| PassError::Failed {
                    pass: NAME,
                    why: format!("{sheet}: {error}"),
                };
                let input = &files[*sheet].blob;
                let Ok(image) = png::decode(input) else {
                    return Ok(Outcome::Kept("the sheet does not decode"));
                };
                let (grid, small) = match shrink(bitmap, &image, options.max_oversample) {
                    Ok(shrunk) => shrunk,
                    Err(why) => return Ok(Outcome::Kept(why)),
                };
                let loss = metrics::image_loss(&image, &small, small.width, small.height)
                    .map_err(failed)?;
                if options
                    .min_ssim
                    .is_some_and(|limit| !loss.ssim.is_finite() || loss.ssim < limit)
                    || options
                        .max_delta
                        .is_some_and(|limit| loss.max_delta > limit)
                {
                    return Ok(Outcome::Kept("measured loss exceeds threshold"));
                }
                let bytes = png::encode(&small).map_err(failed)?;
                let line = format!(
                    "{sheet}: {}x{} to {}x{} (cells {}x{} to {}x{}, height {}), {} to {} B, SSIM {:.4} and max delta {} (nearest sampling at the texel centres of {}x{})",
                    image.width,
                    image.height,
                    small.width,
                    small.height,
                    image.width / grid.cols,
                    image.height / grid.rows,
                    grid.cell_w,
                    grid.cell_h,
                    bitmap.height,
                    input.len(),
                    bytes.len(),
                    loss.ssim,
                    loss.max_delta,
                    small.width,
                    small.height,
                );
                Ok(Outcome::Shrunk {
                    texels: u64::from(image.width) * u64::from(image.height)
                        - u64::from(small.width) * u64::from(small.height),
                    line,
                    loss,
                    bytes,
                })
            })
            .collect();

        let mut edits = Edits::default();
        let (mut sheets, mut texels) = (0, 0);
        for ((_, sheet), outcome) in jobs.iter().zip(outcomes) {
            match outcome? {
                Outcome::Kept(why) => *kept.entry(why).or_default() += 1,
                Outcome::Shrunk {
                    texels: fewer,
                    line,
                    loss,
                    bytes,
                } => {
                    edits.changes.push(line);
                    for (metric, value) in [
                        ("ssim", loss.ssim),
                        ("max_delta", f64::from(loss.max_delta)),
                    ] {
                        edits.loss.push(LossRecord {
                            key: sheet.to_string(),
                            metric: metric.into(),
                            value,
                        });
                    }
                    edits.replaced.insert((*sheet).clone(), bytes.into());
                    sheets += 1;
                    texels += fewer;
                }
            }
        }
        if sheets > 0 {
            edits.changes.push(format!(
                "{} downscaled, {texels} texels fewer",
                plural(sheets, "sheet")
            ));
            edits.changes.push("loss: each sheet before and after, sampled nearest at the texel centres of the new sheet; every glyph keeps its advance and its height on screen".into());
        }
        for (why, count) in kept {
            edits
                .changes
                .push(format!("kept {}: {why}", plural(count, "sheet")));
        }
        Ok(edits)
    }
}

enum Outcome {
    Kept(&'static str),
    Shrunk {
        texels: u64,
        line: String,
        loss: metrics::ImageLoss,
        bytes: Vec<u8>,
    },
}

/// The sheet's new grid and pixels: each cell `ceil(height * max_oversample)` texels tall, as wide as keeps its
/// aspect, when that is fewer texels than it has and every glyph keeps its advance at the new scale.
pub(super) fn shrink(
    bitmap: &Bitmap,
    image: &Image,
    max_oversample: f64,
) -> Result<(Grid, Image), &'static str> {
    let grid = Grid::of(image, bitmap).ok_or("the sheet does not divide into its grid")?;
    let target = (f64::from(bitmap.height) * max_oversample).ceil();
    if f64::from(grid.cell_h) <= target {
        return Err("at most max_oversample texels per GUI pixel");
    }
    // Below the cell height, so it fits.
    let cell_h = target.max(1.0) as u32;
    let cell_w = ((f64::from(grid.cell_w) * f64::from(cell_h) / f64::from(grid.cell_h))
        .round()
        .max(1.0) as u32)
        .min(grid.cell_w);
    let small = resample(image, &grid, cell_w, cell_h);
    let new_grid = Grid {
        cell_w,
        cell_h,
        ..grid
    };
    let (scale, new_scale) = (grid.scale(bitmap), new_grid.scale(bitmap));
    for (row, codepoints) in (0..).zip(&bitmap.rows) {
        for (col, &c) in (0..).zip(codepoints) {
            if c == '\0' {
                continue;
            }
            let before = actual_width(
                image,
                col * grid.cell_w,
                row * grid.cell_h,
                grid.cell_w,
                grid.cell_h,
            );
            let after = actual_width(&small, col * cell_w, row * cell_h, cell_w, cell_h);
            if advance(before, scale) != advance(after, new_scale) {
                return Err("a glyph's advance would change");
            }
        }
    }
    Ok((new_grid, small))
}

/// Each `grid` cell of `image` resampled to `width` by `height` texels: the area average of the texels it covers,
/// in linear light and weighted by alpha, so the colour under a transparent texel adds nothing.
pub(super) fn resample(image: &Image, grid: &Grid, width: u32, height: u32) -> Image {
    static LINEAR: LazyLock<[f64; 256]> = LazyLock::new(|| {
        std::array::from_fn(|value| {
            let c = value as f64 / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        })
    });
    let (xs, ys) = (weights(grid.cell_w, width), weights(grid.cell_h, height));
    let mut rgba_out =
        Vec::with_capacity((grid.cols * width) as usize * (grid.rows * height) as usize * 4);
    for row in 0..grid.rows {
        for wy in &ys {
            for col in 0..grid.cols {
                for wx in &xs {
                    let (mut sums, mut area) = ([0.0f64; 4], 0.0f64);
                    for &(sy, fy) in wy {
                        for &(sx, fx) in wx {
                            let texel = rgba(image, col * grid.cell_w + sx, row * grid.cell_h + sy);
                            let weight = fx * fy;
                            let alpha = f64::from(texel[3]) / 255.0 * weight;
                            area += weight;
                            sums[3] += alpha;
                            for c in 0..3 {
                                sums[c] += LINEAR[usize::from(texel[c])] * alpha;
                            }
                        }
                    }
                    let alpha = (sums[3] / area * 255.0).round().clamp(0.0, 255.0) as u8;
                    if alpha == 0 {
                        rgba_out.extend_from_slice(&[0; 4]);
                    } else {
                        rgba_out.extend((0..3).map(|c| srgb(sums[c] / sums[3])));
                        rgba_out.push(alpha);
                    }
                }
            }
        }
    }
    Image {
        width: grid.cols * width,
        height: grid.rows * height,
        rgba: rgba_out,
    }
}

/// For each of `to` output texels, the source texels of `from` it covers and how much of each.
fn weights(from: u32, to: u32) -> Vec<Vec<(u32, f64)>> {
    let ratio = f64::from(from) / f64::from(to);
    (0..to)
        .map(|i| {
            let (start, end) = (f64::from(i) * ratio, f64::from(i + 1) * ratio);
            (start.floor() as u32..(end.ceil() as u32).min(from))
                .map(|j| (j, end.min(f64::from(j + 1)) - start.max(f64::from(j))))
                .filter(|&(_, weight)| weight > 0.0)
                .collect()
        })
        .collect()
}

fn srgb(linear: f64) -> u8 {
    let linear = linear.clamp(0.0, 1.0);
    let c = if linear <= 0.003_130_8 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    };
    (c * 255.0).round().clamp(0.0, 255.0) as u8
}
