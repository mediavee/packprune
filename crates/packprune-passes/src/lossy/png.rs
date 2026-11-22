use packprune_codec::{DEFAULT_EFFORT, metrics, png};
use packprune_pack::{FileKind, PhysPath};
use packprune_pass::{FileCx, FileOptimizer, Loss, LossRecord, Optimised, PassError};
use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::encode::png_layers;

pub(super) enum Png {
    Alpha,
    Quantize,
    SingleColor,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AlphaOptions {
    #[serde(default = "effort")]
    effort: u8,
    min_ssim: Option<f64>,
    max_delta: Option<u8>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct QuantizeOptions {
    #[serde(default = "effort")]
    effort: u8,
    #[serde(default = "max_colors")]
    max_colors: u16,
    #[serde(default = "quality")]
    quality: u8,
    #[serde(default = "dithering")]
    dithering: f32,
    min_ssim: Option<f64>,
    max_delta: Option<u8>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SingleColorOptions {
    #[serde(default = "effort")]
    effort: u8,
    min_ssim: Option<f64>,
    max_delta: Option<u8>,
}

fn effort() -> u8 {
    DEFAULT_EFFORT
}
fn max_colors() -> u16 {
    256
}
fn quality() -> u8 {
    100
}
fn dithering() -> f32 {
    1.0
}

fn read<T: DeserializeOwned>(options: &toml::Table) -> Result<T, String> {
    options
        .clone()
        .try_into()
        .map_err(|error: toml::de::Error| error.to_string())
}

struct Options {
    effort: u8,
    quantization: png::Quantization,
    min_ssim: Option<f64>,
    max_delta: Option<u8>,
}

impl Png {
    fn options(&self, table: &toml::Table) -> Result<Options, String> {
        let options = match self {
            Self::Alpha => {
                let o: AlphaOptions = read(table)?;
                Options {
                    effort: o.effort,
                    quantization: png::Quantization {
                        max_colors: max_colors(),
                        quality: quality(),
                        dithering: dithering(),
                    },
                    min_ssim: o.min_ssim,
                    max_delta: o.max_delta,
                }
            }
            Self::Quantize => {
                let o: QuantizeOptions = read(table)?;
                Options {
                    effort: o.effort,
                    quantization: png::Quantization {
                        max_colors: o.max_colors,
                        quality: o.quality,
                        dithering: o.dithering,
                    },
                    min_ssim: o.min_ssim,
                    max_delta: o.max_delta,
                }
            }
            Self::SingleColor => {
                let o: SingleColorOptions = read(table)?;
                Options {
                    effort: o.effort,
                    quantization: png::Quantization {
                        max_colors: max_colors(),
                        quality: quality(),
                        dithering: dithering(),
                    },
                    min_ssim: o.min_ssim,
                    max_delta: o.max_delta,
                }
            }
        };
        if !(2..=256).contains(&options.quantization.max_colors)
            || options.quantization.quality > 100
            || !options.quantization.dithering.is_finite()
            || !(0.0..=1.0).contains(&options.quantization.dithering)
            || options
                .min_ssim
                .is_some_and(|v| !v.is_finite() || !(-1.0..=1.0).contains(&v))
        {
            return Err("max_colors must be in [2, 256], quality in [0, 100], dithering in [0, 1], min_ssim in [-1, 1]".into());
        }
        Ok(options)
    }
}

impl FileOptimizer for Png {
    fn name(&self) -> &'static str {
        match self {
            Self::Alpha => "png-alpha",
            Self::Quantize => "png-quantize",
            Self::SingleColor => "png-single-color",
        }
    }
    fn loss(&self) -> Loss {
        Loss::Lossy
    }
    fn applies(&self, kind: FileKind, path: &PhysPath) -> bool {
        matches!(kind, FileKind::Texture) || path.as_str() == "pack.png"
    }
    fn options_schema(&self) -> schemars::Schema {
        match self {
            Self::Alpha => schemars::schema_for!(AlphaOptions),
            Self::Quantize => schemars::schema_for!(QuantizeOptions),
            Self::SingleColor => schemars::schema_for!(SingleColorOptions),
        }
    }

    fn validate_options(&self, options: &toml::Table) -> Result<(), String> {
        self.options(options).map(drop)
    }

    fn optimise(
        &self,
        input: &[u8],
        cx: &FileCx,
        options: &toml::Table,
    ) -> Result<Option<Optimised>, PassError> {
        let options = self.options(options).map_err(|why| PassError::Refused {
            pass: self.name(),
            why,
        })?;
        let layers = png_layers(&cx.path);
        let codec = png::Options {
            effort: options.effort,
            keep_color_type: cx.quirks.abgr_layers && layers.banner,
            keep_grayscale: cx.quirks.imageio_grey,
            imageio: cx.quirks.imageio_textures,
            ..Default::default()
        };
        let failed = |e| PassError::Failed {
            pass: self.name(),
            why: format!("{}: {e}", cx.path),
        };
        // Additive eyes show the colour under transparent texels, and effect textures are shader data: a palette or
        // an alpha rewrite changes both. A banner layer under its quirk must keep its colour type.
        if cx.quirks.additive_eyes && layers.eyes || layers.effect || codec.keep_color_type {
            return Ok(None);
        }
        // An input the codec refuses (a corrupt chunk, say) stays as it is, as the lossless optimiser leaves it.
        let Ok(before) = png::decode(input) else {
            return Ok(None);
        };
        let pixels;
        // The lossless optimisers ran first, so the input is what a candidate must beat.
        let fast = match self {
            Self::Alpha => {
                let Some(cleared) = png::clear_transparent(&before) else {
                    return Ok(None);
                };
                pixels = cleared.width as usize * cleared.height as usize;
                png::optimise_raw_fast(&cleared, &codec).map_err(failed)?
            }
            Self::Quantize => {
                let Some(candidate) =
                    png::quantize(&before, &options.quantization).map_err(failed)?
                else {
                    return Ok(None);
                };
                pixels = before.width as usize * before.height as usize;
                png::optimise_fast(&candidate, &codec).map_err(failed)?.0
            }
            Self::SingleColor => {
                // B1 guard: colormaps (256-wide LUTs), font sheets (cell size sets glyph advance & height), and
                // palette textures (pixel count must match palette_key) must never be resized.
                if is_dimension_sensitive_texture(&cx.path) {
                    return Ok(None);
                }
                let side = uniform_side(before.width, before.height);
                let uniform = side < before.width.min(before.height)
                    && before
                        .rgba
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .all(|p| p == &before.rgba[..4]);
                let resized = if uniform {
                    png::Image {
                        width: side,
                        height: side,
                        rgba: before.rgba[..4].repeat((side * side) as usize),
                    }
                } else if let Some(downscaled) = exact_block_downscale(&before, side) {
                    downscaled
                } else {
                    return Ok(None);
                };
                pixels = resized.width as usize * resized.height as usize;
                png::optimise_raw_fast(&resized, &codec).map_err(failed)?
            }
        };
        // Fast reject: if libdeflate-12 isn't even within 5% of the input, Zopfli (which typically saves ~0.5-1.5%
        // over libdeflate-12) cannot beat the input either, so skip the expensive Zopfli pass.
        if options.effort > 0 && fast.len() * 95 >= input.len() * 100 {
            return Ok(None);
        }
        let bytes = png::zopfli_idat(&fast, pixels, options.effort).map_err(failed)?;
        if bytes.len() >= input.len() {
            return Ok(None);
        }
        let after = png::decode(&bytes).map_err(failed)?;
        let loss =
            metrics::image_loss(&before, &after, after.width, after.height).map_err(failed)?;
        let delta = if before.width == after.width && before.height == after.height {
            before
                .rgba
                .iter()
                .zip(&after.rgba)
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap_or(0)
                .max(loss.max_delta)
        } else {
            loss.max_delta
        };
        let accepted = options.min_ssim.is_none_or(|limit| loss.ssim >= limit)
            && options.max_delta.is_none_or(|limit| delta <= limit);
        if !accepted {
            return Ok(None);
        }
        let record = (before != after).then(|| LossRecord {
            key: cx.path.to_string(),
            metric: if matches!(self, Self::Alpha) {
                "max_delta"
            } else {
                "ssim"
            }
            .into(),
            value: if matches!(self, Self::Alpha) {
                f64::from(delta)
            } else {
                loss.ssim
            },
        });
        Ok(Some(Optimised {
            bytes,
            loss: record,
        }))
    }
}

/// Textures whose pixel dimensions carry semantic meaning in the client: colormaps (256-wide LUTs), font sheets
/// (cell height/width set glyph size and advance), and palette strips (pixel count must equal palette_key).
fn is_dimension_sensitive_texture(path: &PhysPath) -> bool {
    let Some(logical) = crate::encode::logical(path) else {
        return false;
    };
    let Some((_, rest)) = logical
        .strip_prefix("assets/")
        .and_then(|s| s.split_once('/'))
    else {
        return false;
    };
    rest.starts_with("textures/colormap/")
        || rest.starts_with("textures/font/")
        || rest.starts_with("textures/palettes/")
        || rest.starts_with("textures/trims/color_palettes/")
}

/// Detects if `image` is an exact `k x k` nearest-neighbor integer upscale (every `k x k` block is bit-for-bit
/// identical) and returns the largest mip-safe downscale (`new_w.isolate_lowest_one() >= min_align` and
/// `new_h.isolate_lowest_one() >= min_align`). Gives 4x-64x GPU VRAM savings with SSIM = 1.0 and max_delta = 0.
fn exact_block_downscale(image: &png::Image, min_align: u32) -> Option<png::Image> {
    let min_side = image.width.min(image.height);
    if min_side < min_align.saturating_mul(2) {
        return None;
    }
    let max_k = min_side / min_align.max(1);
    let mut best_k = 1u32;
    let mut k = 2u32;
    while k <= max_k {
        if image.width.is_multiple_of(k)
            && image.height.is_multiple_of(k)
            && (image.width / k).isolate_lowest_one() >= min_align
            && (image.height / k).isolate_lowest_one() >= min_align
            && is_exact_k_upscale(image, k)
        {
            best_k = k;
        }
        k = k.saturating_mul(2);
    }
    if best_k < 2 {
        return None;
    }
    let new_w = image.width / best_k;
    let new_h = image.height / best_k;
    let mut rgba = Vec::with_capacity(new_w as usize * new_h as usize * 4);
    for y in 0..new_h {
        let sy = (y * best_k) as usize * image.width as usize;
        for x in 0..new_w {
            let at = (sy + (x * best_k) as usize) * 4;
            rgba.extend_from_slice(&image.rgba[at..at + 4]);
        }
    }
    Some(png::Image {
        width: new_w,
        height: new_h,
        rgba,
    })
}

fn is_exact_k_upscale(image: &png::Image, k: u32) -> bool {
    let w = image.width as usize;
    let ku = k as usize;
    let blocks_x = w / ku;
    let blocks_y = image.height as usize / ku;
    for by in 0..blocks_y {
        let base_y = by * ku;
        for bx in 0..blocks_x {
            let base_x = bx * ku;
            let first = (base_y * w + base_x) * 4;
            let color = &image.rgba[first..first + 4];
            for dy in 0..ku {
                let row = (base_y + dy) * w + base_x;
                for dx in 0..ku {
                    let at = (row + dx) * 4;
                    if &image.rgba[at..at + 4] != color {
                        return false;
                    }
                }
            }
        }
    }
    true
}

/// The side of a square that lowers no atlas's mip level: `SpriteLoader` caps it at the log2 of each sprite's
/// smallest `Integer.lowestOneBit` dimension, and a player picks at most 4 levels (16 texels).
fn uniform_side(width: u32, height: u32) -> u32 {
    const MAX_MIP_LEVELS: u32 = 4;
    width
        .isolate_lowest_one()
        .min(height.isolate_lowest_one())
        .min(1 << MAX_MIP_LEVELS)
}
