use std::collections::BTreeMap;

use packprune_codec::{
    metrics,
    png::{self, Image},
};
use packprune_pack::PhysPath;
use packprune_pass::{Edits, Loss, LossRecord, Pass, PassCx, PassError, Stage};
use rayon::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;

use super::uses::{Faces, Uses};

/// Maximum client mip levels (`SpriteLoader` caps at 4 levels = 16 texels on `minecraft:blocks`).
const MAX_MIP_ALIGN: u32 = 1 << 4;

pub(super) struct TexelDownscale;

#[derive(Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct Options {
    ceiling: f64,
    min_ssim: Option<f64>,
    max_delta: Option<u8>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            ceiling: 4.0,
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
        if !options.ceiling.is_finite()
            || options.ceiling <= 0.0
            || options
                .min_ssim
                .is_some_and(|v| !v.is_finite() || !(-1.0..=1.0).contains(&v))
        {
            return Err("ceiling must be finite and positive, min_ssim in [-1, 1]".into());
        }
        Ok(options)
    }
}

impl Pass for TexelDownscale {
    fn name(&self) -> &'static str {
        "texel-downscale"
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
        let options = Options::read(cx.options).map_err(|why| PassError::Refused {
            pass: self.name(),
            why,
        })?;
        let uses = Uses::of(cx)?;
        let mut kept = BTreeMap::<String, usize>::new();
        let mut edits = Edits::default();
        for why in uses.skipped.into_values() {
            *kept.entry(why).or_default() += 1;
        }
        enum JobOutcome {
            Kept(String),
            Replaced {
                physical: PhysPath,
                smaller: Vec<u8>,
                change: String,
                loss: Vec<LossRecord>,
            },
        }
        let pass_name = self.name();
        let candidates: Vec<(PhysPath, Faces, bool)> = uses
            .candidates
            .into_iter()
            .map(|(physical, faces)| {
                let is_mipmapped = uses.mipmapped.contains(&physical);
                (physical, faces, is_mipmapped)
            })
            .collect();
        let outcomes: Vec<Result<JobOutcome, PassError>> = candidates
            .into_par_iter()
            .map(|(physical, faces, is_mipmapped)| {
                let input = &cx.pack.files[&physical].blob;
                // IHDR bit depth (byte 24) and colour type (byte 25).
                if input.get(24) != Some(&8) && input.get(25) != Some(&3)
                    || !input.get(25).is_some_and(|kind| [2, 3, 6].contains(kind))
                {
                    return Ok(JobOutcome::Kept(
                        "not an 8-bit RGB, RGBA or palette PNG".into(),
                    ));
                }
                let Ok(image) = png::decode(input) else {
                    return Ok(JobOutcome::Kept(
                        "not an 8-bit RGB, RGBA or palette PNG".into(),
                    ));
                };
                // B2 guard: preserve the texture's mip-level contribution on `minecraft:blocks` so downscaling
                // never lowers the global mipmap level of the entire blocks atlas.
                let min_align = if is_mipmapped {
                    image
                        .width
                        .isolate_lowest_one()
                        .min(image.height.isolate_lowest_one())
                        .min(MAX_MIP_ALIGN)
                } else {
                    1
                };
                let factor = factor(&image, faces, options.ceiling, min_align);
                if factor < 2 {
                    return Ok(JobOutcome::Kept(format!(
                        "under {} texels per unit on its tightest face",
                        format(2.0 * options.ceiling)
                    )));
                }
                let small = linear_box_filter(&image, factor);
                let smaller = png::encode(&small).map_err(|e| PassError::Failed {
                    pass: pass_name,
                    why: format!("{physical}: {e}"),
                })?;
                if smaller.len() >= input.len() {
                    return Ok(JobOutcome::Kept("its PNG would not be smaller".into()));
                }
                let width = small
                    .width
                    .min((options.ceiling / faces.u).round().max(1.0) as u32);
                let height = small
                    .height
                    .min((options.ceiling / faces.v).round().max(1.0) as u32);
                let loss = metrics::image_loss(&image, &small, width, height).map_err(|e| {
                    PassError::Failed {
                        pass: pass_name,
                        why: format!("{physical}: {e}"),
                    }
                })?;
                if options.min_ssim.is_some_and(|limit| loss.ssim < limit)
                    || options
                        .max_delta
                        .is_some_and(|limit| loss.max_delta > limit)
                {
                    return Ok(JobOutcome::Kept("measured loss exceeds threshold".into()));
                }
                let change = format!(
                    "{physical}: {}x{} to {}x{} (factor {factor}, tightest face {} texels per unit, ceiling {}), {} to {} B (-{}), -{} atlas texels, SSIM {:.4} and max delta {} (nearest sampling at the texel centres of {width}x{height})",
                    image.width,
                    image.height,
                    small.width,
                    small.height,
                    format(
                        (faces.u * f64::from(image.width)).min(faces.v * f64::from(image.height))
                    ),
                    format(options.ceiling),
                    input.len(),
                    smaller.len(),
                    input.len() - smaller.len(),
                    u64::from(image.width) * u64::from(image.height)
                        - u64::from(small.width) * u64::from(small.height),
                    loss.ssim,
                    loss.max_delta
                );
                let records = [
                    ("ssim", loss.ssim),
                    ("max_delta", f64::from(loss.max_delta)),
                ]
                .into_iter()
                .map(|(metric, value)| LossRecord {
                    key: physical.to_string(),
                    metric: metric.into(),
                    value,
                })
                .collect();
                Ok(JobOutcome::Replaced {
                    physical,
                    smaller,
                    change,
                    loss: records,
                })
            })
            .collect();
        for outcome in outcomes {
            match outcome? {
                JobOutcome::Kept(why) => *kept.entry(why).or_default() += 1,
                JobOutcome::Replaced {
                    physical,
                    smaller,
                    change,
                    loss,
                } => {
                    edits.changes.push(change);
                    edits.loss.extend(loss);
                    edits.replaced.insert(physical, smaller.into());
                }
            }
        }
        for (why, count) in kept {
            edits.changes.push(format!(
                "kept {count} texture{}: {why}",
                if count == 1 { "" } else { "s" }
            ));
        }
        if !edits.replaced.is_empty() {
            edits.changes.push("loss: the texture before and after, sampled nearest (the sampler of atlases without mipmaps) at the texel centres of the size where the tightest face has exactly the ceiling; the mip levels of the blocks atlas are not measured".into());
        }
        Ok(edits)
    }
}

fn format(value: f64) -> String {
    format!("{value:.2}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .into()
}

fn factor(image: &Image, faces: Faces, ceiling: f64, min_align: u32) -> u32 {
    let density = (faces.u * f64::from(image.width)).min(faces.v * f64::from(image.height));
    let limit = ((density / ceiling + 1e-9).floor() as u32).min(image.width.min(image.height));
    (2..=limit)
        .rev()
        .find(|&f| {
            image.width.is_multiple_of(f)
                && image.height.is_multiple_of(f)
                && (image.width / f).isolate_lowest_one() >= min_align
                && (image.height / f).isolate_lowest_one() >= min_align
        })
        .unwrap_or(1)
}

/// Premultiplied linear-light box downsampling (physically accurate sRGB -> linear -> average -> sRGB).
fn linear_box_filter(image: &Image, factor: u32) -> Image {
    let (width, height) = (image.width / factor, image.height / factor);
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    let count = f64::from(factor) * f64::from(factor);
    for y in 0..height {
        for x in 0..width {
            let mut sums = [0.0f64; 3];
            let mut alpha_sum = 0.0f64;
            for j in 0..factor {
                for i in 0..factor {
                    let at = ((y * factor + j) as usize * image.width as usize
                        + (x * factor + i) as usize)
                        * 4;
                    let p = &image.rgba[at..at + 4];
                    let a = f64::from(p[3]) / 255.0;
                    alpha_sum += a;
                    if p[3] > 0 {
                        for c in 0..3 {
                            sums[c] += srgb_to_linear(p[c]) * a;
                        }
                    }
                }
            }
            if alpha_sum == 0.0 {
                rgba.extend_from_slice(&[0; 4]);
            } else {
                for sum in &sums {
                    rgba.push(linear_to_srgb(sum / alpha_sum));
                }
                rgba.push(((alpha_sum / count) * 255.0).round().clamp(0.0, 255.0) as u8);
            }
        }
    }
    Image {
        width,
        height,
        rgba,
    }
}

fn srgb_to_linear(v: u8) -> f64 {
    let s = f64::from(v) / 255.0;
    if s <= 0.04045 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(l: f64) -> u8 {
    let clamped = l.clamp(0.0, 1.0);
    let s = if clamped <= 0.0031308 {
        clamped * 12.92
    } else {
        1.055 * clamped.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round().clamp(0.0, 255.0) as u8
}
