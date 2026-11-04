use std::collections::BTreeMap;

use packprune_pack::json::{self, Pointer, TokenKind};
use packprune_pack::{FileKind, PhysPath, ResourceId};
use packprune_pass::{Edits, Loss, Pass, PassCx, PassError, Stage};
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};

use super::{kind, no_options};

/// Only files the client reads as JSON resources; never lang files, text or `pack.mcmeta`.
const KINDS: [FileKind; 11] = [
    FileKind::ItemDefinition,
    FileKind::Model,
    FileKind::Blockstate,
    FileKind::Atlas,
    FileKind::Equipment,
    FileKind::Font,
    FileKind::SoundsJson,
    FileKind::TextureMeta,
    FileKind::PostEffect,
    FileKind::Particle,
    FileKind::WaypointStyle,
];
const PREFIX: &str = "minecraft:";

/// Every JSON number with a fraction written in its shortest form for the same value (`16.0`
/// becomes `16`), and `minecraft:` dropped from the ids the reference index locates, which the client parses with
/// that default namespace.
pub(crate) struct ShortJson;

impl Pass for ShortJson {
    fn name(&self) -> &'static str {
        "short-json"
    }

    fn stage(&self) -> Stage {
        Stage::Json
    }

    fn loss(&self) -> Loss {
        Loss::Lossless
    }

    fn options_schema(&self) -> schemars::Schema {
        no_options()
    }

    fn run(&self, cx: &PassCx<'_>) -> Result<Edits, PassError> {
        // Each file is read on its own, in parallel; the results are taken in path order.
        let prefixes: BTreeMap<&PhysPath, BTreeMap<Pointer, String>> = cx
            .index
            .all()
            .par_iter()
            .filter_map(|(file, references)| {
                if !KINDS.contains(&kind(cx, file)) {
                    return None;
                }
                let entry = cx.pack.files.get(file)?;
                // A reference's value is the string token at its span; a bound path or an implicit sprite name has
                // none.
                let mut strings = BTreeMap::new();
                json::visit(&entry.blob, |token, path| {
                    if token.kind == TokenKind::String {
                        strings.insert(token.span.start, (path.pointer(), token.text.to_string()));
                    }
                })
                .ok()?;
                let mut dropped = BTreeMap::new();
                for reference in references {
                    let Some((pointer, value)) = strings.get(&(reference.span.start as usize))
                    else {
                        continue;
                    };
                    let Some(path) = value.strip_prefix(PREFIX) else {
                        continue;
                    };
                    let canonical = ResourceId::parse(&cx.run.interner, path)
                        .is_some_and(|id| id.display(&cx.run.interner).to_string() == *value);
                    if canonical {
                        dropped.insert(pointer.clone(), path.to_owned());
                    }
                }
                (!dropped.is_empty()).then_some((file, dropped))
            })
            .collect();

        let none = BTreeMap::new();
        let rewritten: Vec<Result<Rewrite<'_>, PassError>> = cx
            .pack
            .files
            .par_iter()
            .filter(|(path, _)| KINDS.contains(&kind(cx, path)))
            .map(|(path, entry)| {
                let bytes: &[u8] = &entry.blob;
                if !json::parses(bytes) {
                    return Ok(Rewrite::Unreadable(path));
                }
                let mut numbers = 0;
                let shorter = json::rewrite_numbers(bytes, |token, _| {
                    let text = (token.kind == TokenKind::Float)
                        .then(|| shortest(&token.text))
                        .flatten();
                    numbers += usize::from(text.is_some());
                    text
                })
                .map_err(|e| failed(path, e))?;
                let strings = prefixes.get(path).unwrap_or(&none);
                let shorter = if strings.is_empty() {
                    shorter
                } else {
                    json::edit(&shorter, strings, &BTreeMap::new()).map_err(|e| failed(path, e))?
                };
                Ok(Rewrite::Read {
                    path,
                    bytes: (shorter != bytes).then_some(shorter),
                    numbers,
                })
            })
            .collect();
        let mut edits = Edits::default();
        let mut numbers = 0;
        let mut files = 0;
        let mut unreadable = Vec::new();
        for rewrite in rewritten {
            match rewrite? {
                Rewrite::Unreadable(path) => unreadable.push(path.as_str()),
                Rewrite::Read {
                    path,
                    bytes,
                    numbers: shortened,
                } => {
                    numbers += shortened;
                    if let Some(bytes) = bytes {
                        edits.replaced.insert(path.clone(), bytes.into());
                        files += 1;
                    }
                }
            }
        }
        let dropped: usize = prefixes.values().map(BTreeMap::len).sum();
        edits.changes.push(format!("{numbers} numbers shortened"));
        edits
            .changes
            .push(format!("{dropped} minecraft: prefixes dropped"));
        edits.changes.push(format!("{files} files rewritten"));
        if !unreadable.is_empty() {
            let shown: Vec<&str> = unreadable.iter().take(5).copied().collect();
            edits.changes.push(format!(
                "{} files left as they are, not strict JSON: {}{}",
                unreadable.len(),
                shown.join(", "),
                if unreadable.len() > 5 { ", ..." } else { "" }
            ));
        }
        Ok(edits)
    }
}

/// What one file came to: not strict JSON, or read, with its shorter bytes when they differ.
enum Rewrite<'a> {
    Unreadable(&'a PhysPath),
    Read {
        path: &'a PhysPath,
        bytes: Option<Vec<u8>>,
        numbers: usize,
    },
}

fn failed(path: &PhysPath, error: impl std::fmt::Display) -> PassError {
    PassError::Failed {
        pass: "short-json",
        why: format!("{path}: {error}"),
    }
}

/// The shortest text of the same double that also parses to the same float, as the client reads most numbers;
/// `None` when `text` is already that. A negative zero is left as written: compared bit for bit, `0` is another
/// value, and `-0` an integer to most readers, which drop its sign.
pub(crate) fn shortest(text: &str) -> Option<String> {
    let value: f64 = text.parse().ok()?;
    if !value.is_finite() {
        return None;
    }
    let mut candidates = Vec::new();
    if value == value.round_ties_even() && value.abs() < 1e15 {
        candidates.push((value as i64).to_string());
    }
    if value != 0.0 {
        let (negative, digits, scale) = decimal(&json::java_double(value));
        let sign = if negative { "-" } else { "" };
        candidates.push(format!("{sign}{}", plain(&digits, scale)));
        if scale != 0 {
            candidates.push(format!("{sign}{digits}e{}", -scale));
        }
    }
    let single: f32 = text.parse().ok()?;
    let mut best = text;
    for candidate in &candidates {
        if candidate.len() < best.len()
            && candidate.parse::<f64>().ok().map(f64::to_bits) == Some(value.to_bits())
            && candidate.parse::<f32>().ok().map(f32::to_bits) == Some(single.to_bits())
        {
            best = candidate;
        }
    }
    (best != text).then(|| best.to_owned())
}

/// The sign, unscaled digits without trailing zeros and scale of a non-zero `Double.toString` text.
fn decimal(text: &str) -> (bool, String, i64) {
    let (negative, text) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let (mantissa, exponent) = text
        .split_once('E')
        .map_or((text, 0), |(m, e)| (m, e.parse().expect("Double.toString")));
    let (int, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let mut digits: String = format!("{int}{fraction}").trim_start_matches('0').into();
    let mut scale = fraction.len() as i64 - exponent;
    while digits.len() > 1 && digits.ends_with('0') {
        digits.pop();
        scale -= 1;
    }
    (negative, digits, scale)
}

/// Unscaled `digits` at `scale` as a plain decimal, without its sign.
fn plain(digits: &str, scale: i64) -> String {
    if scale <= 0 {
        format!("{digits}{}", "0".repeat(-scale as usize))
    } else if digits.len() as i64 > scale {
        let point = digits.len() - scale as usize;
        format!("{}.{}", &digits[..point], &digits[point..])
    } else {
        format!("0.{}{digits}", "0".repeat(scale as usize - digits.len()))
    }
}
