use std::cmp::Reverse;

use packprune_codec::{CodecError, audio, metrics};
use packprune_pack::{Entry, PhysPath, kinds::ResourcePath};
use packprune_pass::{Edits, Loss, LossRecord, Pass, PassCx, PassError, Stage};
use rayon::prelude::*;
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
    /// Vorbis quality, from -2 to 10 (or upper bound when `min_quality` is set).
    #[serde(default = "quality")]
    quality: f32,
    /// Optional lower bound for perceptual quality search when `max_lsd` or `min_snr` is set.
    min_quality: Option<f32>,
    max_lsd: Option<f64>,
    min_snr: Option<f64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RateOptions {
    /// Vorbis quality, from -2 to 10 (or upper bound when `min_quality` is set).
    #[serde(default = "quality")]
    quality: f32,
    /// Optional lower bound for perceptual quality search when `max_lsd` or `min_snr` is set.
    min_quality: Option<f32>,
    #[serde(default = "cap")]
    cap: u32,
    max_lsd: Option<f64>,
    min_snr: Option<f64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct OggOptions {
    /// Vorbis quality, from -2 to 10 (or upper bound when `min_quality` is set).
    #[serde(default = "quality")]
    quality: f32,
    /// Optional lower bound for perceptual quality search when `max_lsd` or `min_snr` is set.
    min_quality: Option<f32>,
    max_lsd: Option<f64>,
    min_snr: Option<f64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ConvertOptions {
    /// Vorbis quality, from -2 to 10 (or upper bound when `min_quality` is set).
    #[serde(default = "quality")]
    quality: f32,
    /// Optional lower bound for perceptual quality search when `max_lsd` or `min_snr` is set.
    min_quality: Option<f32>,
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
    min_quality: Option<f32>,
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
                    min_quality: o.min_quality,
                    cap: None,
                    max_lsd: o.max_lsd,
                    min_snr: o.min_snr,
                }
            }
            Self::Rate => {
                let o: RateOptions = read(table)?;
                Options {
                    quality: o.quality,
                    min_quality: o.min_quality,
                    cap: Some(o.cap),
                    max_lsd: o.max_lsd,
                    min_snr: o.min_snr,
                }
            }
            Self::Ogg => {
                let o: OggOptions = read(table)?;
                Options {
                    quality: o.quality,
                    min_quality: o.min_quality,
                    cap: None,
                    max_lsd: o.max_lsd,
                    min_snr: o.min_snr,
                }
            }
            Self::Convert => {
                let o: ConvertOptions = read(table)?;
                Options {
                    quality: o.quality,
                    min_quality: o.min_quality,
                    cap: None,
                    max_lsd: o.max_lsd,
                    min_snr: o.min_snr,
                }
            }
        };
        if !options.quality.is_finite()
            || !(-2.0..=10.0).contains(&options.quality)
            || options.min_quality.is_some_and(|v| {
                !v.is_finite() || !(-2.0..=10.0).contains(&v) || v > options.quality
            })
            || options.cap == Some(0)
            || options.max_lsd.is_some_and(|v| !v.is_finite() || v < 0.0)
            || options.min_snr.is_some_and(|v| !v.is_finite())
        {
            return Err("quality/min_quality must be in [-2, 10] with min_quality <= quality, cap positive, and loss thresholds finite (max_lsd non-negative)".into());
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

/// A selected sound and how to transcode it; `refused` ones only report why they are kept.
struct Job<'a> {
    path: &'a PhysPath,
    entry: &'a Entry,
    destination: PhysPath,
    convert: bool,
    refused: bool,
    mono: bool,
    options: Options,
}

enum Outcome {
    Kept(String),
    Transcoded {
        output: Vec<u8>,
        change: String,
        loss: Vec<LossRecord>,
    },
}

pub(super) fn transcode(
    cx: &PassCx<'_>,
    selections: &[Selection],
    name: &'static str,
) -> Result<Edits, PassError> {
    let mut planned = Vec::new();
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
            options.min_quality = match (options.min_quality, selection.options.min_quality) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            }
            .map(|v| v.min(options.quality));
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
        let refused = !cx.scope.contains(&destination) || convert && cx.pinned.contains(path);
        planned.push(Job {
            path,
            entry,
            destination,
            convert,
            refused,
            mono,
            options,
        });
    }

    // Largest inputs first (LPT scheduling) so a long track never runs alone at the end; outcomes are put back into
    // `planned` path order so the result does not depend on thread count or schedule.
    let mut order: Vec<usize> = (0..planned.len()).collect();
    order.sort_by_key(|&i| (Reverse(planned[i].entry.blob.len()), planned[i].path));
    let mut indexed: Vec<(usize, Option<Result<Outcome, PassError>>)> = order
        .into_iter()
        .par_bridge()
        .map(|i| {
            let job = &planned[i];
            let collides = job.convert && cx.pack.files.contains_key(&job.destination);
            let outcome = (!collides && !job.refused).then(|| run_cached(job, name, cx));
            (i, outcome)
        })
        .collect();
    indexed.sort_unstable_by_key(|(i, _)| *i);
    let outcomes: Vec<Option<Result<Outcome, PassError>>> =
        indexed.into_iter().map(|(_, outcome)| outcome).collect();

    let mut edits = Edits::default();
    for (job, outcome) in planned.into_iter().zip(outcomes) {
        let path = job.path;
        if job.convert
            && (cx.pack.files.contains_key(&job.destination)
                || edits.added.contains_key(&job.destination))
        {
            edits.changes.push(format!(
                "skipped {path}: {} already exists",
                job.destination
            ));
            continue;
        }
        if job.refused {
            edits.changes.push(format!(
                "kept {path}: conversion destination is outside scope or input is pinned"
            ));
            continue;
        }
        match outcome.expect("a job that is neither refused nor colliding has run")? {
            Outcome::Kept(why) => edits.changes.push(why),
            Outcome::Transcoded {
                output,
                change,
                loss,
            } => {
                edits.changes.push(change);
                edits.loss.extend(loss);
                if job.convert {
                    edits.removed.insert(path.clone());
                    edits.added.insert(job.destination.clone(), output.into());
                    edits.moves.insert(path.clone(), job.destination);
                } else {
                    edits.replaced.insert(path.clone(), output.into());
                }
            }
        }
    }
    Ok(edits)
}

fn run_cached(job: &Job<'_>, name: &'static str, cx: &PassCx<'_>) -> Result<Outcome, PassError> {
    let key = cache_key(job);
    if let Some(cached) = cx.cache.get("audio-transcode", &key)
        && let Some(outcome) = decode_outcome(&cached, job.path)
    {
        return Ok(outcome);
    }
    let outcome = run(job, name)?;
    cx.cache
        .put("audio-transcode", &key, &encode_outcome(&outcome));
    Ok(outcome)
}

fn cache_key(job: &Job<'_>) -> Vec<u8> {
    let mut key = Vec::with_capacity(72);
    key.extend_from_slice(&job.entry.digest.0);
    key.push(u8::from(job.convert));
    key.push(u8::from(job.mono));
    key.extend_from_slice(&job.options.quality.to_bits().to_le_bytes());
    match job.options.min_quality {
        Some(v) => {
            key.push(1);
            key.extend_from_slice(&v.to_bits().to_le_bytes());
        }
        None => key.push(0),
    }
    match job.options.cap {
        Some(cap) => {
            key.push(1);
            key.extend_from_slice(&cap.to_le_bytes());
        }
        None => key.push(0),
    }
    match job.options.max_lsd {
        Some(v) => {
            key.push(1);
            key.extend_from_slice(&v.to_bits().to_le_bytes());
        }
        None => key.push(0),
    }
    match job.options.min_snr {
        Some(v) => {
            key.push(1);
            key.extend_from_slice(&v.to_bits().to_le_bytes());
        }
        None => key.push(0),
    }
    key
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(bytes);
}

fn encode_outcome(outcome: &Outcome) -> Vec<u8> {
    let mut out = Vec::new();
    match outcome {
        Outcome::Kept(why) => {
            out.push(0);
            // Strip the leading path prefix when present so duplicate sounds at different paths share cache entries.
            put_bytes(&mut out, why.as_bytes());
        }
        Outcome::Transcoded {
            output,
            change,
            loss,
        } => {
            out.push(1);
            put_bytes(&mut out, output);
            // Store only the suffix after `": "` so the path is filled in on load.
            let suffix = change.split_once(": ").map_or(change.as_str(), |(_, s)| s);
            put_bytes(&mut out, suffix.as_bytes());
            out.extend_from_slice(&(loss.len() as u64).to_le_bytes());
            for record in loss {
                put_bytes(&mut out, record.metric.as_bytes());
                out.extend_from_slice(&record.value.to_le_bytes());
            }
        }
    }
    out
}

fn take_slice<'a>(rest: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
    let (head, tail) = rest.split_at_checked(n)?;
    *rest = tail;
    Some(head)
}

fn take_field<'a>(rest: &mut &'a [u8]) -> Option<&'a [u8]> {
    let len = u64::from_le_bytes(take_slice(rest, 8)?.try_into().ok()?) as usize;
    take_slice(rest, len)
}

fn decode_outcome(bytes: &[u8], path: &PhysPath) -> Option<Outcome> {
    let mut rest = bytes;
    match take_slice(&mut rest, 1)? {
        [0] => {
            let why = std::str::from_utf8(take_field(&mut rest)?).ok()?;
            if !rest.is_empty() {
                return None;
            }
            // Rewrite `"kept <old>: <reason>"` with this job's path if needed.
            let rebuilt = match why.strip_prefix("kept ").and_then(|s| s.split_once(": ")) {
                Some((_, reason)) => format!("kept {path}: {reason}"),
                None => why.to_owned(),
            };
            Some(Outcome::Kept(rebuilt))
        }
        [1] => {
            let output = take_field(&mut rest)?.to_vec();
            let suffix = std::str::from_utf8(take_field(&mut rest)?).ok()?;
            let count = u64::from_le_bytes(take_slice(&mut rest, 8)?.try_into().ok()?) as usize;
            let mut loss = Vec::with_capacity(count);
            for _ in 0..count {
                let metric = std::str::from_utf8(take_field(&mut rest)?).ok()?.to_owned();
                let value = f64::from_le_bytes(take_slice(&mut rest, 8)?.try_into().ok()?);
                loss.push(LossRecord {
                    key: path.to_string(),
                    metric,
                    value,
                });
            }
            rest.is_empty().then(|| Outcome::Transcoded {
                output,
                change: format!("{path}: {suffix}"),
                loss,
            })
        }
        _ => None,
    }
}

fn run(job: &Job<'_>, name: &'static str) -> Result<Outcome, PassError> {
    let Job {
        path,
        entry,
        convert,
        mono,
        options,
        ..
    } = *job;
    let failed = |e: CodecError| PassError::Failed {
        pass: name,
        why: format!("{path}: {e}"),
    };
    let before = match audio::decode(&entry.blob) {
        Ok(pcm) => pcm,
        Err(error) => return Ok(Outcome::Kept(format!("kept {path}: {error}"))),
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
    .map_err(failed)?;
    type Candidate = (Vec<u8>, audio::Pcm, metrics::SoundLoss);
    let encode_and_measure = |q: f32| -> Result<Result<Candidate, String>, PassError> {
        let output = audio::encode(&transformed, q / 10.0)
            .and_then(|bytes| audio::optimise(&bytes))
            .map_err(failed)?;
        let after = audio::decode(&output).map_err(failed)?;
        let loss = match metrics::sound_loss(&before, &after) {
            Ok(loss) => loss,
            Err(error) => {
                return Ok(Err(format!(
                    "kept {path}: loss cannot be measured: {error}"
                )));
            }
        };
        Ok(Ok((output, after, loss)))
    };
    let passes_gate = |loss: &metrics::SoundLoss| -> bool {
        !options.max_lsd.is_some_and(|limit| loss.lsd > limit)
            && !options
                .min_snr
                .is_some_and(|limit| loss.snr.is_none_or(|snr| snr < limit))
    };

    let (output, after, loss) = if let Some(min_q) = options.min_quality
        && min_q < options.quality
        && (options.max_lsd.is_some() || options.min_snr.is_some())
    {
        // Perceptual quality search: test `min_q` first; if it passes, we're done with the smallest file!
        // Otherwise binary-search `[min_q, options.quality]` (5 steps -> ~0.1–0.2 Vorbis quality granularity)
        // to find the lowest Vorbis quality that meets `max_lsd` and `min_snr`.
        let low_try = match encode_and_measure(min_q)? {
            Ok(v) => v,
            Err(msg) => return Ok(Outcome::Kept(msg)),
        };
        if passes_gate(&low_try.2) {
            low_try
        } else {
            let high_try = match encode_and_measure(options.quality)? {
                Ok(v) => v,
                Err(msg) => return Ok(Outcome::Kept(msg)),
            };
            if !passes_gate(&high_try.2) {
                return Ok(Outcome::Kept(format!(
                    "kept {path}: measured loss exceeds threshold"
                )));
            }
            let (mut lo, mut hi) = (min_q, options.quality);
            let mut best = high_try;
            for _ in 0..5 {
                let mid = ((lo + hi) * 0.5 * 20.0).round() / 20.0;
                if mid <= lo || mid >= hi {
                    break;
                }
                match encode_and_measure(mid)? {
                    Ok(cand) if passes_gate(&cand.2) => {
                        if cand.0.len() < best.0.len() {
                            best = cand;
                        }
                        hi = mid;
                    }
                    Ok(_) => lo = mid,
                    Err(_) => lo = mid,
                }
            }
            best
        }
    } else {
        let single = match encode_and_measure(options.quality)? {
            Ok(v) => v,
            Err(msg) => return Ok(Outcome::Kept(msg)),
        };
        if !passes_gate(&single.2) {
            return Ok(Outcome::Kept(format!(
                "kept {path}: measured loss exceeds threshold"
            )));
        }
        single
    };
    if !convert && !mono && options.cap.is_none() && output.len() >= entry.blob.len() {
        return Ok(Outcome::Kept(format!(
            "kept {path}: transcoded Ogg would not be smaller"
        )));
    }
    let change = format!(
        "{path}: {} channels at {} Hz to {} channels at {} Hz, {} to {} B, LSD {:.6} dB, SNR {}",
        before.channels.len(),
        before.rate,
        after.channels.len(),
        after.rate,
        entry.blob.len(),
        output.len(),
        loss.lsd,
        loss.snr
            .map_or_else(|| "unavailable".into(), |v| format!("{v:.6} dB"))
    );
    let records = [
        ("lsd", loss.lsd),
        ("pcm_before", loss.pcm_before as f64),
        ("pcm_after", loss.pcm_after as f64),
    ]
    .into_iter()
    .chain(loss.snr.map(|value| ("snr", value)))
    .map(|(metric, value)| LossRecord {
        key: path.to_string(),
        metric: metric.into(),
        value,
    })
    .collect();
    Ok(Outcome::Transcoded {
        output,
        change,
        loss: records,
    })
}
