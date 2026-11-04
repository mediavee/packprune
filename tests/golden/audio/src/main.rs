//! Compares every Ogg sound of a reference pack with the same path in a candidate pack, decoded as the client
//! decodes them (`packprune_codec::audio::decode`), with the loss metric of `packprune_codec::metrics`:
//!
//!   golden-audio <reference> <candidate> [--input <pack>] [--max-rms-db <dB>] [--max-lsd <dB>]
//!                [--max-frames <n>] [--max-loss-delta <dB>] [--max-median-rms-db <dB>]
//!                [--max-median-loss-delta <dB>]
//!
//! One line per sound: channels, rate, the frame and per-channel RMS level differences, the LSD between the two,
//! and with --input the LSD of each against the input's sound; then the medians over the sounds that differ, which
//! show a shift every sound shares (a quality, a gain) that each one alone keeps within its tolerance. Exits 1 when
//! channels or rates differ, a sound is missing or undecodable, or a difference or median exceeds its tolerance; 2
//! on bad arguments or unreadable packs.

use std::collections::BTreeMap;
use std::env;
use std::process::ExitCode;

use camino::Utf8PathBuf;
use packprune_codec::audio::{Pcm, decode};
use packprune_codec::metrics::sound_loss;
use packprune_pack::input::PackInput;
use packprune_zip::Limits;

#[derive(Default)]
struct Tolerances {
    rms_db: Option<f64>,
    lsd: Option<f64>,
    frames: Option<usize>,
    loss_delta: Option<f64>,
    median_rms_db: Option<f64>,
    median_loss_delta: Option<f64>,
}

struct Args {
    reference: Utf8PathBuf,
    candidate: Utf8PathBuf,
    input: Option<Utf8PathBuf>,
    tolerances: Tolerances,
}

fn args() -> Result<Args, String> {
    let mut positional = Vec::new();
    let mut input = None;
    let mut tolerances = Tolerances::default();
    let mut it = env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        let number = |name: &str, text: String| {
            text.parse::<f64>()
                .map_err(|e| format!("{name} {text}: {e}"))
        };
        match arg.as_str() {
            "--input" => input = Some(Utf8PathBuf::from(value(&arg)?)),
            "--max-rms-db" => tolerances.rms_db = Some(number(&arg, value(&arg)?)?),
            "--max-lsd" => tolerances.lsd = Some(number(&arg, value(&arg)?)?),
            "--max-loss-delta" => tolerances.loss_delta = Some(number(&arg, value(&arg)?)?),
            "--max-median-rms-db" => tolerances.median_rms_db = Some(number(&arg, value(&arg)?)?),
            "--max-median-loss-delta" => {
                tolerances.median_loss_delta = Some(number(&arg, value(&arg)?)?);
            }
            "--max-frames" => {
                let text = value(&arg)?;
                tolerances.frames = Some(
                    text.parse()
                        .map_err(|e| format!("--max-frames {text}: {e}"))?,
                );
            }
            _ if arg.starts_with("--") => return Err(format!("unknown option {arg}")),
            _ => positional.push(Utf8PathBuf::from(arg)),
        }
    }
    let [reference, candidate]: [Utf8PathBuf; 2] = positional
        .try_into()
        .map_err(|_| "expected <reference> <candidate>".to_owned())?;
    Ok(Args {
        reference,
        candidate,
        input,
        tolerances,
    })
}

fn sounds(path: &Utf8PathBuf) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let pack = PackInput::open(path, &Limits::default()).map_err(|e| format!("{path}: {e}"))?;
    Ok(pack
        .files
        .into_iter()
        .filter(|(path, _)| path.as_str().ends_with(".ogg"))
        .map(|(path, entry)| (path.as_str().to_owned(), entry.blob.to_vec()))
        .collect())
}

fn rms_db(samples: &[f32]) -> f64 {
    let power = samples
        .iter()
        .map(|&s| f64::from(s) * f64::from(s))
        .sum::<f64>()
        / samples.len().max(1) as f64;
    10.0 * power.max(1e-20).log10()
}

/// The largest per-channel difference of RMS level, in dB.
fn rms_delta(reference: &Pcm, candidate: &Pcm) -> f64 {
    reference
        .channels
        .iter()
        .zip(&candidate.channels)
        .map(|(r, c)| (rms_db(c) - rms_db(r)).abs())
        .fold(0.0, f64::max)
}

/// The LSD of `after` against `before`, or none for a sound shorter than its analysis frame.
fn lsd(before: &Pcm, after: &Pcm) -> Option<f64> {
    sound_loss(before, after).ok().map(|loss| loss.lsd)
}

fn median(mut values: Vec<f64>) -> Option<f64> {
    values.sort_by(f64::total_cmp);
    let n = values.len();
    match n {
        0 => None,
        _ if n % 2 == 1 => Some(values[n / 2]),
        _ => Some((values[n / 2 - 1] + values[n / 2]) / 2.0),
    }
}

fn show(value: Option<f64>) -> String {
    value.map_or_else(|| "-".to_owned(), |v| format!("{v:.3}"))
}

fn main() -> ExitCode {
    let args = match args() {
        Ok(args) => args,
        Err(why) => {
            eprintln!("golden-audio: {why}");
            return ExitCode::from(2);
        }
    };
    let packs = (|| -> Result<_, String> {
        let input = args.input.as_ref().map(sounds).transpose()?;
        Ok((sounds(&args.reference)?, sounds(&args.candidate)?, input))
    })();
    let (reference, candidate, input) = match packs {
        Ok(packs) => packs,
        Err(why) => {
            eprintln!("golden-audio: {why}");
            return ExitCode::from(2);
        }
    };
    let t = &args.tolerances;
    let over =
        |value: Option<f64>, max: Option<f64>| matches!((value, max), (Some(v), Some(m)) if v > m);
    let mut failed = false;
    let (mut levels, mut losses) = (Vec::new(), Vec::new());
    println!("sound\tchannels\trate\tframes ref/cand\tRMS delta dB\tLSD dB\tinput LSD ref/cand dB");
    for (path, bytes) in &reference {
        let Some(other) = candidate.get(path) else {
            println!("{path}\tFAILED: not in the candidate");
            failed = true;
            continue;
        };
        if bytes == other {
            continue;
        }
        let (r, c) = match (decode(bytes), decode(other)) {
            (Ok(r), Ok(c)) => (r, c),
            (r, c) => {
                println!(
                    "{path}\tFAILED: undecodable ({:?} / {:?})",
                    r.err(),
                    c.err()
                );
                failed = true;
                continue;
            }
        };
        if r == c {
            continue;
        }
        let mut problems = Vec::new();
        if r.channels.len() != c.channels.len() {
            problems.push("channels");
        }
        if r.rate != c.rate {
            problems.push("rate");
        }
        let frames = r.frames().abs_diff(c.frames());
        if t.frames.is_some_and(|max| frames > max) {
            problems.push("frames");
        }
        let rms = rms_delta(&r, &c);
        levels.push(rms);
        if over(Some(rms), t.rms_db) {
            problems.push("RMS");
        }
        let between = if problems.contains(&"channels") || problems.contains(&"rate") {
            None
        } else {
            lsd(&r, &c)
        };
        if over(between, t.lsd) {
            problems.push("LSD");
        }
        let source = input
            .as_ref()
            .and_then(|input| input.get(path))
            .and_then(|b| decode(b).ok());
        let (in_r, in_c) = source
            .as_ref()
            .map_or((None, None), |s| (lsd(s, &r), lsd(s, &c)));
        let delta = in_r.zip(in_c).map(|(a, b)| (a - b).abs());
        losses.extend(delta);
        if over(delta, t.loss_delta) {
            problems.push("input LSD");
        }
        println!(
            "{path}\t{}/{}\t{}/{}\t{}/{} ({frames})\t{rms:.3}\t{}\t{}/{}{}",
            r.channels.len(),
            c.channels.len(),
            r.rate,
            c.rate,
            r.frames(),
            c.frames(),
            show(between),
            show(in_r),
            show(in_c),
            if problems.is_empty() {
                String::new()
            } else {
                format!("\tFAILED: {}", problems.join(", "))
            }
        );
        failed |= !problems.is_empty();
    }
    let (level, loss) = (median(levels), median(losses));
    let mut problems = Vec::new();
    if over(level, t.median_rms_db) {
        problems.push("median RMS");
    }
    if over(loss, t.median_loss_delta) {
        problems.push("median input LSD");
    }
    println!(
        "median\t\t\t\t{}\t\t{}{}",
        show(level),
        show(loss),
        if problems.is_empty() {
            String::new()
        } else {
            format!("\tFAILED: {}", problems.join(", "))
        }
    );
    failed |= !problems.is_empty();
    if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
