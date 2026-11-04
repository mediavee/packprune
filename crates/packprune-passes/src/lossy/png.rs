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
        // The lossless optimisers ran first, so the input is what a candidate must beat.
        let candidate = match self {
            Self::Alpha => png::clear_transparent(&before)
                .map(|image| png::encode(&image))
                .transpose(),
            Self::Quantize => png::quantize(&before, &options.quantization),
            Self::SingleColor => {
                let side = uniform_side(before.width, before.height);
                let uniform = side < before.width.min(before.height)
                    && before
                        .rgba
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .all(|p| p == &before.rgba[..4]);
                uniform
                    .then(|| {
                        png::encode(&png::Image {
                            width: side,
                            height: side,
                            rgba: before.rgba[..4].repeat((side * side) as usize),
                        })
                    })
                    .transpose()
            }
        }
        .map_err(failed)?;
        let Some(candidate) = candidate else {
            return Ok(None);
        };
        let bytes = png::optimise(&candidate, &codec).map_err(failed)?;
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

/// The side of a square that lowers no atlas's mip level: `SpriteLoader` caps it at the log2 of each sprite's
/// smallest `Integer.lowestOneBit` dimension, and a player picks at most 4 levels (16 texels).
fn uniform_side(width: u32, height: u32) -> u32 {
    const MAX_MIP_LEVELS: u32 = 4;
    width
        .isolate_lowest_one()
        .min(height.isolate_lowest_one())
        .min(1 << MAX_MIP_LEVELS)
}
