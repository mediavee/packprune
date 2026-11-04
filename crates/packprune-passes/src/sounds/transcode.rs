use packprune_codec::{audio, metrics};
use packprune_pack::{PhysPath, kinds::ResourcePath};
use packprune_pass::{Edits, Loss, LossRecord, Pass, PassCx, PassError, Stage};
use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;

use super::ogg::Ogg;
use super::targets::Selection;

#[derive(Clone, Copy)]
pub(super) enum Audio {
    Mono,
    Rate,
    Ogg,
    Convert,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct MonoOptions {
    /// Vorbis quality, from -2 to 10.
    #[serde(default = "quality")]
    quality: f32,
    max_lsd: Option<f64>,
    min_snr: Option<f64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RateOptions {
    /// Vorbis quality, from -2 to 10.
    #[serde(default = "quality")]
    quality: f32,
    #[serde(default = "cap")]
    cap: u32,
    max_lsd: Option<f64>,
    min_snr: Option<f64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct OggOptions {
    /// Vorbis quality, from -2 to 10.
    #[serde(default = "quality")]
    quality: f32,
    max_lsd: Option<f64>,
    min_snr: Option<f64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ConvertOptions {
    /// Vorbis quality, from -2 to 10.
    #[serde(default = "quality")]
    quality: f32,
    max_lsd: Option<f64>,
    min_snr: Option<f64>,
}

fn quality() -> f32 {
    0.6
}

fn cap() -> u32 {
    48_000
}

fn read<T: DeserializeOwned>(options: &toml::Table) -> Result<T, String> {
    options
        .clone()
        .try_into()
        .map_err(|error: toml::de::Error| error.to_string())
}

#[derive(Clone, Copy)]
pub(super) struct Options {
    quality: f32,
    cap: Option<u32>,
    max_lsd: Option<f64>,
    min_snr: Option<f64>,
}

impl Audio {
    pub(super) fn of(name: &str) -> Option<Self> {
        match name {
            "sound-mono" => Some(Self::Mono),
            "sound-rate" => Some(Self::Rate),
            "ogg-transcode" => Some(Self::Ogg),
            _ => None,
        }
    }

    pub(super) fn options(self, table: &toml::Table) -> Result<Options, String> {
        let options = match self {
            Self::Mono => {
                let o: MonoOptions = read(table)?;
                Options {
                    quality: o.quality,
                    cap: None,
                    max_lsd: o.max_lsd,
                    min_snr: o.min_snr,
                }
            }
            Self::Rate => {
                let o: RateOptions = read(table)?;
                Options {
                    quality: o.quality,
                    cap: Some(o.cap),
                    max_lsd: o.max_lsd,
                    min_snr: o.min_snr,
                }
            }
            Self::Ogg => {
                let o: OggOptions = read(table)?;
                Options {
                    quality: o.quality,
                    cap: None,
                    max_lsd: o.max_lsd,
                    min_snr: o.min_snr,
                }
            }
            Self::Convert => {
                let o: ConvertOptions = read(table)?;
                Options {
                    quality: o.quality,
                    cap: None,
                    max_lsd: o.max_lsd,
                    min_snr: o.min_snr,
                }
            }
        };
        if !options.quality.is_finite()
            || !(-2.0..=10.0).contains(&options.quality)
            || options.cap == Some(0)
            || options.max_lsd.is_some_and(|v| !v.is_finite() || v < 0.0)
            || options.min_snr.is_some_and(|v| !v.is_finite())
        {
            return Err("quality must be in [-2, 10], cap positive, and loss thresholds finite (max_lsd non-negative)".into());
        }
        Ok(options)
    }
}

impl Pass for Audio {
    fn name(&self) -> &'static str {
        match self {
            Self::Mono => "sound-mono",
            Self::Rate => "sound-rate",
            Self::Ogg => "ogg-transcode",
            Self::Convert => "audio-to-ogg",
        }
    }
    fn stage(&self) -> Stage {
        Stage::Lossy
    }
    fn loss(&self) -> Loss {
        Loss::Lossy
    }
    fn options_schema(&self) -> schemars::Schema {
        match self {
            Self::Mono => schemars::schema_for!(MonoOptions),
            Self::Rate => schemars::schema_for!(RateOptions),
            Self::Ogg => schemars::schema_for!(OggOptions),
            Self::Convert => schemars::schema_for!(ConvertOptions),
        }
    }

    fn validate_options(&self, options: &toml::Table) -> Result<(), String> {
        self.options(options).map(drop)
    }

    fn run(&self, cx: &PassCx<'_>) -> Result<Edits, PassError> {
        let options = self.options(cx.options).map_err(|why| PassError::Refused {
            pass: self.name(),
            why,
        })?;
        transcode(
            cx,
            &[Selection {
                audio: *self,
                scope: cx.scope.clone(),
                options,
            }],
            self.name(),
        )
    }
}

pub(super) fn transcode(
    cx: &PassCx<'_>,
    selections: &[Selection],
    name: &'static str,
) -> Result<Edits, PassError> {
    let mut edits = Edits::default();
    for (path, entry) in &cx.pack.files {
        if !cx.scope.contains(path) {
            continue;
        }
        let convert = matches!(selections[0].audio, Audio::Convert);
        let mut selected = Vec::new();
        if convert {
            let Some(logical) = cx.layout.split(path).1 else {
                continue;
            };
            let Some(resource) = ResourcePath::of(logical.as_str()) else {
                continue;
            };
            if resource.folder != "sounds"
                || !audio::DECODABLE_EXTENSIONS
                    .iter()
                    .any(|ext| path.as_str().ends_with(&format!(".{ext}")))
            {
                continue;
            }
            selected.push(&selections[0]);
        } else {
            if !path.as_str().ends_with(".ogg") {
                continue;
            }
            let Some(info) = Ogg::read(&entry.blob) else {
                continue;
            };
            for selection in selections {
                if selection.scope.contains(path)
                    && match selection.audio {
                        Audio::Mono => info.channels == 2,
                        Audio::Rate => {
                            info.rate > selection.options.cap.expect("sound-rate has a cap")
                        }
                        Audio::Ogg => true,
                        Audio::Convert => false,
                    }
                {
                    selected.push(selection);
                }
            }
        }
        let Some(first) = selected.first() else {
            continue;
        };
        let mut options = first.options;
        for selection in selected.iter().skip(1) {
            options.quality = options.quality.max(selection.options.quality);
            options.cap = options.cap.into_iter().chain(selection.options.cap).min();
            options.max_lsd = options
                .max_lsd
                .into_iter()
                .chain(selection.options.max_lsd)
                .reduce(f64::min);
            options.min_snr = options
                .min_snr
                .into_iter()
                .chain(selection.options.min_snr)
                .reduce(f64::max);
        }
        let mono = selected.iter().any(|s| matches!(s.audio, Audio::Mono));
        let destination = if convert {
            PhysPath::new(&format!(
                "{}.ogg",
                path.as_str()
                    .rsplit_once('.')
                    .expect("a converted file has an extension")
                    .0
            ))
            .expect("replacing an extension keeps a valid path")
        } else {
            path.clone()
        };
        if convert
            && (cx.pack.files.contains_key(&destination) || edits.added.contains_key(&destination))
        {
            edits
                .changes
                .push(format!("skipped {path}: {destination} already exists"));
            continue;
        }
        if !cx.scope.contains(&destination) || convert && cx.pinned.contains(path) {
            edits.changes.push(format!(
                "kept {path}: conversion destination is outside scope or input is pinned"
            ));
            continue;
        }
        let before = match audio::decode(&entry.blob) {
            Ok(pcm) => pcm,
            Err(error) => {
                edits.changes.push(format!("kept {path}: {error}"));
                continue;
            }
        };
        let transformed = if mono {
            audio::downmix(&before)
        } else {
            Ok(before.clone())
        }
        .and_then(|pcm| match options.cap {
            Some(cap) => audio::resample(&pcm, cap),
            None => Ok(pcm),
        })
        .map_err(|e| PassError::Failed {
            pass: name,
            why: format!("{path}: {e}"),
        })?;
        let output = audio::encode(&transformed, options.quality / 10.0)
            .and_then(|bytes| audio::optimise(&bytes))
            .map_err(|e| PassError::Failed {
                pass: name,
                why: format!("{path}: {e}"),
            })?;
        let after = audio::decode(&output).map_err(|e| PassError::Failed {
            pass: name,
            why: format!("{path}: {e}"),
        })?;
        let loss = match metrics::sound_loss(&before, &after) {
            Ok(loss) => loss,
            Err(error) => {
                edits
                    .changes
                    .push(format!("kept {path}: loss cannot be measured: {error}"));
                continue;
            }
        };
        if options.max_lsd.is_some_and(|limit| loss.lsd > limit)
            || options
                .min_snr
                .is_some_and(|limit| loss.snr.is_none_or(|snr| snr < limit))
        {
            edits
                .changes
                .push(format!("kept {path}: measured loss exceeds threshold"));
            continue;
        }
        if !convert && !mono && options.cap.is_none() && output.len() >= entry.blob.len() {
            edits
                .changes
                .push(format!("kept {path}: transcoded Ogg would not be smaller"));
            continue;
        }
        edits.changes.push(format!("{path}: {} channels at {} Hz to {} channels at {} Hz, {} to {} B, LSD {:.6} dB, SNR {}", before.channels.len(), before.rate, after.channels.len(), after.rate, entry.blob.len(), output.len(), loss.lsd, loss.snr.map_or_else(|| "unavailable".into(), |v| format!("{v:.6} dB"))));
        for (metric, value) in [
            ("lsd", loss.lsd),
            ("pcm_before", loss.pcm_before as f64),
            ("pcm_after", loss.pcm_after as f64),
        ] {
            edits.loss.push(LossRecord {
                key: path.to_string(),
                metric: metric.into(),
                value,
            });
        }
        if let Some(value) = loss.snr {
            edits.loss.push(LossRecord {
                key: path.to_string(),
                metric: "snr".into(),
                value,
            });
        }
        if convert {
            edits.removed.insert(path.clone());
            edits.added.insert(destination.clone(), output.into());
            edits.moves.insert(path.clone(), destination);
        } else {
            edits.replaced.insert(path.clone(), output.into());
        }
    }
    Ok(edits)
}
