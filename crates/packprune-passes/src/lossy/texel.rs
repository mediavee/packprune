use std::collections::BTreeMap;

use packprune_codec::{
    metrics,
    png::{self, Image},
};
use packprune_pass::{Edits, Loss, LossRecord, Pass, PassCx, PassError, Stage};
use schemars::JsonSchema;
use serde::Deserialize;

use super::uses::{Faces, Uses};

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
        for (physical, faces) in uses.candidates {
            let input = &cx.pack.files[&physical].blob;
            // IHDR bit depth (byte 24) and colour type (byte 25).
            if input.get(24) != Some(&8) && input.get(25) != Some(&3)
                || !input.get(25).is_some_and(|kind| [2, 3, 6].contains(kind))
            {
                *kept
                    .entry("not an 8-bit RGB, RGBA or palette PNG".into())
                    .or_default() += 1;
                continue;
            }
            let Ok(image) = png::decode(input) else {
                *kept
                    .entry("not an 8-bit RGB, RGBA or palette PNG".into())
                    .or_default() += 1;
                continue;
            };
            let factor = factor(&image, faces, options.ceiling);
            if factor < 2 {
                *kept
                    .entry(format!(
                        "under {} texels per unit on its tightest face",
                        format(2.0 * options.ceiling)
                    ))
                    .or_default() += 1;
                continue;
            }
            let small = box_filter(&image, factor);
            let smaller = png::encode(&small).map_err(|e| PassError::Failed {
                pass: self.name(),
                why: format!("{physical}: {e}"),
            })?;
            if smaller.len() >= input.len() {
                *kept
                    .entry("its PNG would not be smaller".into())
                    .or_default() += 1;
                continue;
            }
            let width = small
                .width
                .min((options.ceiling / faces.u).round().max(1.0) as u32);
            let height = small
                .height
                .min((options.ceiling / faces.v).round().max(1.0) as u32);
            let loss = metrics::image_loss(&image, &small, width, height).map_err(|e| {
                PassError::Failed {
                    pass: self.name(),
                    why: format!("{physical}: {e}"),
                }
            })?;
            if options.min_ssim.is_some_and(|limit| loss.ssim < limit)
                || options
                    .max_delta
                    .is_some_and(|limit| loss.max_delta > limit)
            {
                *kept
                    .entry("measured loss exceeds threshold".into())
                    .or_default() += 1;
                continue;
            }
            edits.changes.push(format!("{physical}: {}x{} to {}x{} (factor {factor}, tightest face {} texels per unit, ceiling {}), {} to {} B (-{}), -{} atlas texels, SSIM {:.4} and max delta {} (nearest sampling at the texel centres of {width}x{height})", image.width, image.height, small.width, small.height, format((faces.u * f64::from(image.width)).min(faces.v * f64::from(image.height))), format(options.ceiling), input.len(), smaller.len(), input.len() - smaller.len(), u64::from(image.width) * u64::from(image.height) - u64::from(small.width) * u64::from(small.height), loss.ssim, loss.max_delta));
            for (metric, value) in [
                ("ssim", loss.ssim),
                ("max_delta", f64::from(loss.max_delta)),
            ] {
                edits.loss.push(LossRecord {
                    key: physical.to_string(),
                    metric: metric.into(),
                    value,
                });
            }
            edits.replaced.insert(physical, smaller.into());
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

fn factor(image: &Image, faces: Faces, ceiling: f64) -> u32 {
    let density = (faces.u * f64::from(image.width)).min(faces.v * f64::from(image.height));
    let limit = ((density / ceiling + 1e-9).floor() as u32).min(image.width.min(image.height));
    (2..=limit)
        .rev()
        .find(|factor| image.width.is_multiple_of(*factor) && image.height.is_multiple_of(*factor))
        .unwrap_or(1)
}

fn box_filter(image: &Image, factor: u32) -> Image {
    let (width, height) = (image.width / factor, image.height / factor);
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    let count = u64::from(factor).pow(2);
    for y in 0..height {
        for x in 0..width {
            let mut sums = [0u64; 4];
            for j in 0..factor {
                for i in 0..factor {
                    let at = ((y * factor + j) as usize * image.width as usize
                        + (x * factor + i) as usize)
                        * 4;
                    let p = &image.rgba[at..at + 4];
                    let alpha = u64::from(p[3]);
                    sums[3] += alpha;
                    for c in 0..3 {
                        sums[c] += u64::from(p[c]) * alpha;
                    }
                }
            }
            let alpha = sums[3];
            if alpha == 0 {
                rgba.extend_from_slice(&[0; 4]);
            } else {
                rgba.extend((0..3).map(|c| ((sums[c] + alpha / 2) / alpha) as u8));
                rgba.push(((alpha + count / 2) / count) as u8);
            }
        }
    }
    Image {
        width,
        height,
        rgba,
    }
}
