use std::collections::{BTreeMap, BTreeSet};

use packprune_codec::audio;
use packprune_graph::Names;
use packprune_pack::{FileKind, Layer, LogicalPath, PhysPath};
use packprune_pass::{Edits, Loss, Pass, PassCx, PassError, Stage};
use rayon::prelude::*;
use sha2::{Digest, Sha256};

use super::ogg::Ogg;

const NAME: &str = "identical-sounds";

type HeaderBucket = Vec<(Layer, u8, u32, u64)>;
type PcmSignature = Vec<(Layer, [u8; 32])>;

/// Sounds with identical decoded PCM across every layer of the pack keep one `.ogg` file and remap all references in
/// `sounds.json` to it. Because Minecraft's `SoundBufferLibrary` caches decoded PCM buffers by path, deduplicating
/// identical sound files saves both archive bytes and resident client OpenAL RAM.
pub(super) struct IdenticalSounds;

impl Pass for IdenticalSounds {
    fn name(&self) -> &'static str {
        NAME
    }

    fn stage(&self) -> Stage {
        Stage::Textures
    }

    fn loss(&self) -> Loss {
        Loss::Lossless
    }

    fn options_schema(&self) -> schemars::Schema {
        schemars::json_schema!({"type": "object", "additionalProperties": false})
    }

    fn run(&self, cx: &PassCx<'_>) -> Result<Edits, PassError> {
        // Collect all logical sound paths and their physical layers.
        let mut by_logical: BTreeMap<LogicalPath, BTreeMap<Layer, PhysPath>> = BTreeMap::new();
        for path in cx.pack.files.keys() {
            if cx.layout.kinds.get(path) != Some(&FileKind::Sound) {
                continue;
            }
            let (layer, Some(logical)) = cx.layout.split(path) else {
                continue;
            };
            by_logical
                .entry(logical)
                .or_default()
                .insert(layer, path.clone());
        }

        // Fast header bucketing: only sounds with identical layer sets and `(channels, rate, samples)` can match PCM.
        let mut buckets: BTreeMap<HeaderBucket, Vec<LogicalPath>> = BTreeMap::new();
        for (logical, layers) in &by_logical {
            let mut key = Vec::with_capacity(layers.len());
            let mut ok = true;
            for (layer, phys) in layers {
                let Some(info) = Ogg::read(&cx.pack.files[phys].blob) else {
                    ok = false;
                    break;
                };
                key.push((layer.clone(), info.channels, info.rate, info.samples));
            }
            if ok && !key.is_empty() {
                buckets.entry(key).or_default().push(logical.clone());
            }
        }

        let candidates: Vec<LogicalPath> = buckets
            .into_values()
            .filter(|group| group.len() > 1)
            .flatten()
            .collect();
        if candidates.is_empty() {
            let mut edits = Edits::default();
            edits
                .changes
                .push("0 sounds merged into an identical one".into());
            return Ok(edits);
        }

        // Paths referenced by text/shader or non-rewritable file references cannot be remapped.
        let mut blocked_text: BTreeSet<LogicalPath> = BTreeSet::new();
        for reference in cx.index.all().values().flatten() {
            if let Names::Text { logical } = &reference.names {
                blocked_text.insert(logical.clone());
            }
        }

        // Decode candidate OGGs in parallel (cached by blob digest) and hash their canonical PCM samples per layer.
        let signatures: Vec<(LogicalPath, Option<PcmSignature>)> = candidates
            .into_par_iter()
            .map(|logical| {
                let layers = &by_logical[&logical];
                let mut sig = Vec::with_capacity(layers.len());
                for (layer, phys) in layers {
                    let entry = &cx.pack.files[phys];
                    let digest: [u8; 32] = if let Some(cached) =
                        cx.cache.get("identical-sounds-pcm", &entry.digest.0)
                        && cached.len() == 32
                    {
                        cached.try_into().expect("32 bytes")
                    } else {
                        let Ok(pcm) = audio::decode(&entry.blob) else {
                            return (logical, None);
                        };
                        let mut hasher = Sha256::new();
                        hasher.update(pcm.rate.to_le_bytes());
                        hasher.update((pcm.channels.len() as u32).to_le_bytes());
                        hasher.update((pcm.frames() as u64).to_le_bytes());
                        for ch in &pcm.channels {
                            for &s in ch {
                                // Canonicalize -0.0 to +0.0.
                                let bits = if s == 0.0 { 0u32 } else { s.to_bits() };
                                hasher.update(bits.to_le_bytes());
                            }
                        }
                        let hash: [u8; 32] = hasher.finalize().into();
                        cx.cache.put("identical-sounds-pcm", &entry.digest.0, &hash);
                        hash
                    };
                    sig.push((layer.clone(), digest));
                }
                (logical, Some(sig))
            })
            .collect();

        let mut groups: BTreeMap<Vec<(Layer, [u8; 32])>, Vec<LogicalPath>> = BTreeMap::new();
        for (logical, sig) in signatures {
            if let Some(sig) = sig {
                groups.entry(sig).or_default().push(logical);
            }
        }

        let removable = |logical: &LogicalPath| -> bool {
            !cx.kept_names.contains(logical)
                && !cx.vanilla.has(logical.as_str())
                && !blocked_text.contains(logical)
                && by_logical[logical]
                    .values()
                    .all(|phys| !cx.pinned.contains(phys) && cx.scope.contains(phys))
        };

        let mut edits = Edits::default();
        let (mut merged, mut bytes) = (0usize, 0usize);
        for mut group in groups.into_values() {
            if group.len() < 2 {
                continue;
            }
            group.sort();
            // Prefer keeping a non-removable path as canonical, otherwise the smallest encoded OGG (tie-break by path).
            let canonical = group
                .iter()
                .find(|logical| !removable(logical))
                .or_else(|| {
                    group.iter().min_by_key(|logical| {
                        let total: usize = by_logical[*logical]
                            .values()
                            .map(|p| cx.pack.files[p].blob.len())
                            .sum();
                        (total, (*logical).clone())
                    })
                })
                .expect("group has at least 2 items")
                .clone();
            for logical in group {
                if logical == canonical || !removable(&logical) {
                    continue;
                }
                for phys in by_logical[&logical].values() {
                    bytes += cx.pack.files[phys].blob.len();
                    edits.removed.insert(phys.clone());
                }
                edits.file_remaps.insert(logical, canonical.clone());
                merged += 1;
            }
        }

        edits
            .changes
            .push(format!("{merged} sounds merged into an identical one"));
        edits.changes.push(format!("{bytes} B of Ogg removed"));
        Ok(edits)
    }
}
