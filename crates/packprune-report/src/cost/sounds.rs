//! OpenAL PCM memory audit for non-streamed sounds in `sounds.json`.
//!
//! Minecraft's `SoundBufferLibrary.getCompleteBuffer` caches each non-streamed sound by path as decoded 16-bit PCM
//! (`2 * channels * frames` bytes). Streamed sounds (`"stream": true`) are decoded in small chunks on demand and do
//! not stay resident in OpenAL RAM.

use std::collections::{BTreeMap, BTreeSet};

use packprune_pack::json;
use serde::Serialize;

use super::read::Key;
use super::stack::Stack;

/// Client OpenAL RAM footprint of the pack's own sounds referenced by `sounds.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SoundCost {
    /// Total 16-bit PCM bytes kept resident in OpenAL buffers for non-streamed pack sounds.
    pub pcm_bytes: u64,
    /// Distinct pack sound paths loaded whole into OpenAL RAM.
    pub buffered: usize,
    /// Distinct pack sound paths marked `"stream": true` everywhere they are referenced.
    pub streamed: usize,
}

/// Scans every `assets/<ns>/sounds.json` in the view and computes the 16-bit PCM memory of all pack `.ogg` files
/// referenced without `"stream": true` (from Ogg headers and final page granule, without full PCM decode).
pub(super) fn audit(stack: &Stack<'_>) -> SoundCost {
    let mut buffered_paths: BTreeSet<String> = BTreeSet::new();
    let mut streamed_paths: BTreeSet<String> = BTreeSet::new();
    for path in stack.pack_paths() {
        let mut parts = path.split('/');
        let (Some("assets"), Some(_ns), Some("sounds.json"), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let Some((_, entry)) = stack.pack_file(path) else {
            continue;
        };
        let Ok(root) = json::parse_lenient(&entry.blob) else {
            continue;
        };
        for event in root.members() {
            for sound in event.node.path("sounds").children() {
                let (name, stream, kind) = if let Some(s) = sound.as_str() {
                    (s, false, "file")
                } else {
                    let kind = sound.path("type").as_str().unwrap_or("file");
                    let name = sound.path("name").as_str().unwrap_or("");
                    let stream = sound.path("stream").as_bool(false);
                    (name, stream, kind)
                };
                if kind != "file" || name.is_empty() {
                    continue;
                }
                let Some((ns, val)) = parse_id(name) else {
                    continue;
                };
                let sound_path = format!("assets/{ns}/sounds/{val}.ogg");
                if stack.pack_file(&sound_path).is_some() {
                    if stream {
                        streamed_paths.insert(sound_path);
                    } else {
                        buffered_paths.insert(sound_path);
                    }
                }
            }
        }
    }
    // A sound referenced both streamed and non-streamed still occupies a complete OpenAL buffer.
    streamed_paths.retain(|p| !buffered_paths.contains(p));
    let mut sizes: BTreeMap<&str, u64> = BTreeMap::new();
    for path in &buffered_paths {
        if let Some((_, entry)) = stack.pack_file(path)
            && let Some(pcm) = ogg_pcm_bytes(&entry.blob)
        {
            sizes.insert(path.as_str(), pcm);
        }
    }
    SoundCost {
        pcm_bytes: sizes.values().sum(),
        buffered: buffered_paths.len(),
        streamed: streamed_paths.len(),
    }
}

fn parse_id(text: &str) -> Option<(&str, &str)> {
    let (ns, val) = match text.split_once(':') {
        Some((ns, val)) => (ns, val),
        None => ("minecraft", text),
    };
    (Key::namespace_ok(ns) && Key::value_ok(val)).then_some((ns, val))
}

/// Reads channel count and final granule from Ogg Vorbis page headers without decoding PCM:
/// `2 * channels * samples` (16-bit PCM bytes in OpenAL).
fn ogg_pcm_bytes(bytes: &[u8]) -> Option<u64> {
    let mut at = 0usize;
    let mut channels = 0u64;
    let mut samples = 0u64;
    while at + 27 <= bytes.len() {
        let header = &bytes[at..at + 27];
        if &header[..4] != b"OggS" {
            return None;
        }
        let segments = usize::from(header[26]);
        let lacing = bytes.get(at + 27..at + 27 + segments)?;
        let size: usize = lacing.iter().map(|&n| usize::from(n)).sum();
        let start = at + 27 + segments;
        let body = bytes.get(start..start + size)?;
        if channels == 0 {
            if body.len() < 23 || &body[..7] != b"\x01vorbis" || body[11] == 0 {
                return None;
            }
            channels = u64::from(body[11]);
        }
        let granule = i64::from_le_bytes(header[6..14].try_into().ok()?);
        if granule >= 0 {
            samples = granule as u64;
        }
        at = start + size;
    }
    (channels > 0 && at == bytes.len())
        .then(|| 2u64.saturating_mul(channels).saturating_mul(samples))
}
