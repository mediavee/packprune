//! font-dedupe: merges bitmap font PNG sheets that have bit-identical RGBA pixels (including RGB under alpha = 0)
//! across every layer of the pack, remapping referencing `font/*.json` files cleanly. Safe even when custom
//! `rendertype_text*` core shaders are present, because every pixel byte of every glyph cell is preserved verbatim.

use std::collections::{BTreeMap, BTreeSet};

use packprune_codec::png;
use packprune_graph::Names;
use packprune_pack::{FileKind, Layer, LogicalPath, PhysPath};
use packprune_pass::{Edits, Loss, Pass, PassCx, PassError, Stage};
use rayon::prelude::*;
use sha2::{Digest, Sha256};

use crate::textures::{Use, Views, no_options};

const NAME: &str = "font-dedupe";

type HeaderBucket = (String, Vec<(Layer, u32, u32)>);
type PixelSignature = (String, Vec<(Layer, u32, u32, [u8; 32])>);

pub(super) struct FontDedupe;

impl Pass for FontDedupe {
    fn name(&self) -> &'static str {
        NAME
    }

    fn stage(&self) -> Stage {
        Stage::Fonts
    }

    fn loss(&self) -> Loss {
        Loss::Lossless
    }

    fn options_schema(&self) -> schemars::Schema {
        no_options()
    }

    fn run(&self, cx: &PassCx<'_>) -> Result<Edits, PassError> {
        let views = Views::new(cx, NAME)?;
        let usage = Use::new(cx, &views);

        // Identify all logical PNG paths referenced exclusively by FileKind::Font via Names::File.
        let mut font_referenced: BTreeSet<LogicalPath> = BTreeSet::new();
        let mut blocked: BTreeSet<LogicalPath> = BTreeSet::new();
        for (source, references) in cx.index.all() {
            let is_font = cx.layout.kinds.get(source) == Some(&FileKind::Font);
            for reference in references {
                match &reference.names {
                    Names::File { logical, .. } if logical.as_str().ends_with(".png") => {
                        if is_font {
                            font_referenced.insert(logical.clone());
                        } else {
                            blocked.insert(logical.clone());
                        }
                    }
                    Names::Text { logical } | Names::Beside { logical } => {
                        blocked.insert(logical.clone());
                    }
                    _ => {}
                }
            }
        }

        // Collect all physical layers of candidate font PNGs, excluding any stitched by an atlas or carrying `.mcmeta`.
        let mut by_logical: BTreeMap<LogicalPath, BTreeMap<Layer, PhysPath>> = BTreeMap::new();
        let mut has_meta: BTreeSet<String> = BTreeSet::new();
        for path in cx.pack.files.keys() {
            let (layer, Some(logical)) = cx.layout.split(path) else {
                continue;
            };
            if let Some(stem) = logical.as_str().strip_suffix(".mcmeta") {
                has_meta.insert(stem.to_owned());
                continue;
            }
            if !font_referenced.contains(&logical) || blocked.contains(&logical) {
                continue;
            }
            if !usage.placements(logical.as_str()).is_empty() {
                continue;
            }
            by_logical
                .entry(logical)
                .or_default()
                .insert(layer, path.clone());
        }
        by_logical.retain(|logical, _| !has_meta.contains(logical.as_str()));

        // Fast IHDR bucketing: only PNGs in the same namespace with identical layer sets and (width, height)
        // can merge (keeping per-namespace lossy/PackSquash globs such as `assets/mcore/**` independent).
        let mut buckets: BTreeMap<HeaderBucket, Vec<LogicalPath>> = BTreeMap::new();
        for (logical, layers) in &by_logical {
            let Some(ns) = logical.as_str().split('/').nth(1) else {
                continue;
            };
            let mut key = Vec::with_capacity(layers.len());
            let mut ok = true;
            for (layer, phys) in layers {
                let Some((w, h)) = ihdr_size(&cx.pack.files[phys].blob) else {
                    ok = false;
                    break;
                };
                key.push((layer.clone(), w, h));
            }
            if ok && !key.is_empty() {
                buckets
                    .entry((ns.to_owned(), key))
                    .or_default()
                    .push(logical.clone());
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
                .push("0 font sheets merged into an identical one".into());
            return Ok(edits);
        }

        // Decode candidate PNGs in parallel (cached by blob digest) and hash full RGBA (including RGB under alpha = 0).
        let signatures: Vec<(LogicalPath, Option<PixelSignature>)> = candidates
            .into_par_iter()
            .map(|logical| {
                let Some(ns) = logical.as_str().split('/').nth(1).map(str::to_owned) else {
                    return (logical, None);
                };
                let layers = &by_logical[&logical];
                let mut sig = Vec::with_capacity(layers.len());
                for (layer, phys) in layers {
                    let entry = &cx.pack.files[phys];
                    let Some((w, h)) = ihdr_size(&entry.blob) else {
                        return (logical, None);
                    };
                    let hash: [u8; 32] = if let Some(cached) =
                        cx.cache.get("rgba-sha256", &entry.digest.0)
                        && cached.len() == 32
                    {
                        cached.try_into().expect("32 bytes")
                    } else {
                        let Ok(img) = png::decode(&entry.blob) else {
                            return (logical, None);
                        };
                        let mut hasher = Sha256::new();
                        hasher.update(img.width.to_le_bytes());
                        hasher.update(img.height.to_le_bytes());
                        hasher.update(&img.rgba);
                        let digest: [u8; 32] = hasher.finalize().into();
                        cx.cache.put("rgba-sha256", &entry.digest.0, &digest);
                        digest
                    };
                    sig.push((layer.clone(), w, h, hash));
                }
                (logical, Some((ns, sig)))
            })
            .collect();

        let mut groups: BTreeMap<PixelSignature, Vec<LogicalPath>> = BTreeMap::new();
        for (logical, sig) in signatures {
            if let Some(sig) = sig {
                groups.entry(sig).or_default().push(logical);
            }
        }

        let removable = |logical: &LogicalPath| -> bool {
            !cx.kept_names.contains(logical)
                && !cx.vanilla.has(logical.as_str())
                && !(cx.vanilla.names.is_empty()
                    && logical.as_str().starts_with("assets/minecraft/"))
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
            let canonical = group
                .iter()
                .find(|logical| !removable(logical))
                .or_else(|| {
                    group.iter().min_by_key(|logical| {
                        let total: usize = by_logical[*logical]
                            .values()
                            .map(|p| cx.pack.files[p].blob.len())
                            .sum();
                        (logical.as_str().len(), total, (*logical).clone())
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
            .push(format!("{merged} font sheets merged into an identical one"));
        edits.changes.push(format!("{bytes} B of PNG removed"));
        Ok(edits)
    }
}

fn ihdr_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}
