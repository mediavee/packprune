//! The equivalence oracle: resolves every key of two packs as the client would and compares the results.

mod canon;
mod id;
pub mod memo;
mod resolver;
mod shared;
mod uvlock;

#[cfg(test)]
mod client_glsl;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;

use packprune_graph::ReferenceIndex;
use packprune_pack::meta::Format;
use packprune_pack::{
    Blob, Layout, LogicalPath, PackInput, PhysPath, ResourceId, RunCx, VanillaIndex, ViewId,
};
use rayon::prelude::*;

use crate::canon::Digest;
use crate::memo::Memo;
use crate::resolver::{Ctx, Resolver, Side};

/// Keys whose differences are located in their descriptors; the others are listed by key.
const LOCATED: usize = 50;

/// Renames and moves passes made, composed across passes, so both sides compare under the new names and
/// the self-check follows a file to where it went. It lives here because `compare` takes it and the oracle cannot
/// depend on the pass API.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Remap {
    files: BTreeMap<LogicalPath, LogicalPath>,
    sprites: BTreeMap<ResourceId, ResourceId>,
    /// One per set of edits, in run order: a move and a rename of the same pass are one step, the move
    /// winning, so two runs with the same renames but different moves stay distinct.
    steps: Vec<Step>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Step {
    files: BTreeMap<LogicalPath, LogicalPath>,
    moves: BTreeMap<PhysPath, PhysPath>,
}

impl Step {
    fn physical(&self, path: &PhysPath) -> PhysPath {
        if let Some(moved) = self.moves.get(path) {
            return moved.clone();
        }
        let text = path.as_str();
        let start = if text.starts_with("assets/") {
            0
        } else {
            text.find("/assets/").map_or(0, |at| at + 1)
        };
        let renamed = LogicalPath::new(&text[start..])
            .ok()
            .and_then(|logical| self.files.get(&logical));
        match renamed {
            Some(logical) => PhysPath::new(&format!("{}{logical}", &text[..start]))
                .expect("an overlay folder and a valid logical path make a valid path"),
            None => path.clone(),
        }
    }
}

impl Remap {
    /// The remaps of one set of edits: its file renames, physical moves and sprite renames.
    pub fn step(
        files: BTreeMap<LogicalPath, LogicalPath>,
        moves: BTreeMap<PhysPath, PhysPath>,
        sprites: BTreeMap<ResourceId, ResourceId>,
    ) -> Remap {
        let steps = if files.is_empty() && moves.is_empty() {
            Vec::new()
        } else {
            vec![Step {
                files: files.clone(),
                moves,
            }]
        };
        Remap {
            files,
            sprites,
            steps,
        }
    }

    /// `self`, then `next`, the remap of a run on this one's output.
    pub fn then(&self, next: &Remap) -> Remap {
        let mut sprites: BTreeMap<ResourceId, ResourceId> = self
            .sprites
            .iter()
            .map(|(from, to)| (*from, next.sprite(to)))
            .collect();
        for (from, to) in &next.sprites {
            sprites.entry(*from).or_insert(*to);
        }
        let mut files: BTreeMap<LogicalPath, LogicalPath> = self
            .files
            .iter()
            .map(|(from, to)| (from.clone(), next.file(to)))
            .collect();
        for (from, to) in &next.files {
            files.entry(from.clone()).or_insert_with(|| to.clone());
        }
        Remap {
            files,
            sprites,
            steps: self.steps.iter().chain(&next.steps).cloned().collect(),
        }
    }

    /// The name `sprite` has in the output.
    pub fn sprite(&self, sprite: &ResourceId) -> ResourceId {
        self.sprites.get(sprite).copied().unwrap_or(*sprite)
    }

    /// The logical path `logical` has in the output.
    pub fn file(&self, logical: &LogicalPath) -> LogicalPath {
        self.files
            .get(logical)
            .cloned()
            .unwrap_or_else(|| logical.clone())
    }

    /// Where the file at `physical` is in the output: each step's move, else its rename of the logical
    /// part, in order.
    pub fn physical(&self, physical: &PhysPath) -> PhysPath {
        self.steps
            .iter()
            .fold(physical.clone(), |path, step| step.physical(&path))
    }
}

pub struct OracleInput<'a> {
    pub pack: &'a PackInput,
    pub layout: &'a Layout,
    pub index: &'a ReferenceIndex,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Difference {
    pub view: ViewId,
    pub key: String,
    pub detail: String,
}

/// The first 50 differences located, the rest counted in `differing_keys`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Comparison {
    pub differences: Vec<Difference>,
    pub differing_keys: u32,
    pub views: u32,
    pub keys: u32,
}

/// The vanilla client's own files, read on demand: what a view resolves to where the pack has nothing.
pub trait VanillaAssets: Sync {
    /// The bytes of `logical` (`assets/...`), `None` when the client does not have it.
    fn bytes(&self, logical: &str) -> Option<Blob>;
}

/// The client jar read whole as a pack.
impl VanillaAssets for PackInput {
    fn bytes(&self, logical: &str) -> Option<Blob> {
        let path = PhysPath::new(logical).ok()?;
        self.files.get(&path).map(|entry| Arc::clone(&entry.blob))
    }
}

/// No vanilla file at all: what [`compare`] resolves over.
struct NoAssets;

impl VanillaAssets for NoAssets {
    fn bytes(&self, _logical: &str) -> Option<Blob> {
        None
    }
}

/// [`compare_with`] without the client's files: a vanilla parent, sheet or sound resolves as absent on both sides,
/// so what a pack file inherits from vanilla is not compared. `VanillaIndex` holds names, not bytes; a caller with
/// the jar uses [`compare_with`].
pub fn compare(
    cx: &RunCx,
    before: &OracleInput<'_>,
    after: &OracleInput<'_>,
    vanilla: &VanillaIndex,
    remap: &Remap,
    named: &dyn Fn(&LogicalPath) -> bool,
) -> Comparison {
    compare_with(cx, before, after, vanilla, &NoAssets, remap, named)
}

/// Two packs are the same for the player when, at every format either declares a view for, every key resolves to
/// the same descriptor. One pack is resolved at a time and only a 128-bit digest per key and view is kept; the keys
/// that differ are resolved again to show where.
///
/// `named`: the files something outside the pack can name: their atlas sprites compare by sprite id and
/// their textures, sounds and font files whole; the others compare through what draws them. Files at vanilla paths
/// always compare whole, since the client loads them by path.
pub fn compare_with(
    cx: &RunCx,
    before: &OracleInput<'_>,
    after: &OracleInput<'_>,
    vanilla: &VanillaIndex,
    assets: &dyn VanillaAssets,
    remap: &Remap,
    named: &dyn Fn(&LogicalPath) -> bool,
) -> Comparison {
    compare_inner(cx, before, after, vanilla, assets, remap, named, None)
}

/// [`compare_with`] reusing the digests `memo` holds from earlier comparisons wherever every query their resolution
/// made has the same answer, and recording the others into it.
#[expect(
    clippy::too_many_arguments,
    reason = "compare_with's inputs and the memo, each borrowed from a different owner"
)]
pub fn compare_memoised(
    cx: &RunCx,
    before: &OracleInput<'_>,
    after: &OracleInput<'_>,
    vanilla: &VanillaIndex,
    assets: &dyn VanillaAssets,
    remap: &Remap,
    named: &dyn Fn(&LogicalPath) -> bool,
    memo: &Memo,
) -> Comparison {
    compare_inner(cx, before, after, vanilla, assets, remap, named, Some(memo))
}

#[expect(
    clippy::too_many_arguments,
    reason = "the inputs of the three public entry points"
)]
fn compare_inner(
    cx: &RunCx,
    before: &OracleInput<'_>,
    after: &OracleInput<'_>,
    vanilla: &VanillaIndex,
    assets: &dyn VanillaAssets,
    remap: &Remap,
    named: &dyn Fn(&LogicalPath) -> bool,
    memo: Option<&Memo>,
) -> Comparison {
    let formats = formats(before.layout, after.layout);
    let ctx = Ctx::new(
        cx,
        vanilla,
        assets,
        named_paths(cx, vanilla, before, after, named),
    );
    let before_side = Side::new(cx, vanilla, before.pack, before.layout, &formats);
    let after_side = Side::new(cx, vanilla, after.pack, after.layout, &formats);

    let was = digests(&ctx, &before_side, BTreeSet::new(), false, memo);
    let renamed: Vec<(&str, String)> = was
        .keys
        .iter()
        .map(|key| (key.as_str(), renamed(cx, key, remap)))
        .collect();
    let targets: BTreeSet<String> = renamed.iter().map(|(_, to)| to.clone()).collect();
    let is = digests(&ctx, &after_side, targets.clone(), false, memo);
    // Keys only the output has, resolved in the input too: a file it adds may shadow vanilla's.
    let added: BTreeSet<String> = is
        .keys
        .iter()
        .filter(|key| !targets.contains(*key))
        .cloned()
        .collect();
    let was_added = digests(&ctx, &before_side, added.clone(), true, memo);

    let mut differing: BTreeMap<usize, Vec<(String, String)>> = BTreeMap::new();
    let mut keys = 0u32;
    for view in 0..formats.len() {
        let mut compare = |from: &str, to: &str, a: Option<Digest>, b: Option<Digest>| {
            if a.is_none() && b.is_none() {
                return;
            }
            keys += 1;
            if a != b {
                differing
                    .entry(view)
                    .or_default()
                    .push((from.to_owned(), to.to_owned()));
            }
        };
        for (from, to) in &renamed {
            compare(from, to, was.get(view, from), is.get(view, to));
        }
        for key in &added {
            compare(key, key, was_added.get(view, key), is.get(view, key));
        }
    }

    let differing_keys = differing.values().map(Vec::len).sum::<usize>();
    let mut differences = Vec::new();
    let mut located = 0;
    for (view, pairs) in differing {
        let before_resolver = Resolver::new(&ctx, &before_side, view);
        let after_resolver = Resolver::new(&ctx, &after_side, view);
        let id = ViewId(view as u32);
        for (from, to) in pairs {
            let key = if from == to {
                from.clone()
            } else {
                format!("{from} (now {to})")
            };
            located += 1;
            if located > LOCATED {
                differences.push(Difference {
                    view: id,
                    key,
                    detail: String::new(),
                });
                continue;
            }
            match (before_resolver.resolve(&from), after_resolver.resolve(&to)) {
                (Some(a), Some(b)) => {
                    let lines = canon::diff(
                        "",
                        &a,
                        &b,
                        &|digest| before_resolver.model_of(digest),
                        &|digest| after_resolver.model_of(digest),
                    );
                    differences.extend(lines.into_iter().map(|detail| Difference {
                        view: id,
                        key: key.clone(),
                        detail,
                    }));
                }
                (a, _) => differences.push(Difference {
                    view: id,
                    key,
                    detail: if a.is_none() {
                        "only after"
                    } else {
                        "only before"
                    }
                    .into(),
                }),
            }
        }
    }
    Comparison {
        differences,
        differing_keys: differing_keys as u32,
        views: formats.len() as u32,
        keys,
    }
}

/// The name of each view a comparison of these two packs reports, by [`ViewId`]: its format and the overlays each
/// side applies there (`88.0 (base)`, `57.0 (layer -> base)`).
pub fn view_labels(before: &Layout, after: &Layout) -> Vec<String> {
    formats(before, after)
        .into_iter()
        .map(|format| {
            resolver::label(
                format,
                &before.view_name_at(format),
                &after.view_name_at(format),
            )
        })
        .collect()
}

/// One format per interval where neither pack's overlays change, among those either pack declares: the bounds of
/// both packs' ranges cut the intervals, so moving a bound in one is seen on both sides of it.
fn formats(before: &Layout, after: &Layout) -> Vec<Format> {
    let mut bounds = before.meta.bounds();
    bounds.extend(after.meta.bounds());
    let mut seen = HashSet::new();
    bounds
        .into_iter()
        .filter(|&format| before.meta.declares(format) || after.meta.declares(format))
        .filter(|&format| {
            let active = |layout: &Layout| -> Vec<String> {
                layout
                    .meta
                    .active(format)
                    .into_iter()
                    .map(str::to_owned)
                    .collect()
            };
            seen.insert((active(before), active(after)))
        })
        .collect()
}

/// The key a renamed sprite or file has in the output.
fn renamed(cx: &RunCx, key: &str, remap: &Remap) -> String {
    if let Some(logical) = key.strip_prefix("file ") {
        return match LogicalPath::new(logical) {
            Ok(logical) => format!("file {}", remap.file(&logical)),
            Err(_) => key.to_owned(),
        };
    }
    let mut parts = key.splitn(3, ' ');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("sprite"), Some(atlas), Some(sprite)) => {
            match ResourceId::parse(&cx.interner, sprite) {
                Some(id) => format!("sprite {atlas} {}", remap.sprite(&id).display(&cx.interner)),
                None => key.to_owned(),
            }
        }
        _ => key.to_owned(),
    }
}

/// `named` evaluated on this thread for every path a resolution may test: it need not be `Sync`, and views resolve
/// in parallel.
fn named_paths(
    cx: &RunCx,
    vanilla: &VanillaIndex,
    before: &OracleInput<'_>,
    after: &OracleInput<'_>,
    named: &dyn Fn(&LogicalPath) -> bool,
) -> HashSet<String> {
    let mut candidates = Ctx::named_candidates(&cx.interner, vanilla, &[before.pack, after.pack]);
    for input in [before, after] {
        for path in input.pack.files.keys() {
            if let (_, Some(logical)) = input.layout.split(path) {
                let logical = logical.as_str();
                if let Some(texture) = logical.strip_suffix(".mcmeta") {
                    candidates.insert(texture.to_owned());
                }
                candidates.insert(logical.to_owned());
            }
        }
    }
    candidates
        .into_iter()
        .filter(|candidate| LogicalPath::new(candidate).is_ok_and(|logical| named(&logical)))
        .collect()
}

/// The digest of each key in every view, `None` where the view has nothing for it.
struct Digests {
    keys: Vec<String>,
    /// By view, then by key index.
    values: Vec<Vec<Option<Digest>>>,
}

impl Digests {
    fn get(&self, view: usize, key: &str) -> Option<Digest> {
        let index = self
            .keys
            .binary_search_by(|probe| probe.as_str().cmp(key))
            .ok()?;
        self.values[view][index]
    }
}

/// The keys any view of the pack contributes and `wanted`, or only `wanted` when `only_wanted`. The views resolve in
/// parallel, each its own resolver over what the comparison shares.
fn digests(
    ctx: &Ctx<'_>,
    side: &Side<'_>,
    wanted: BTreeSet<String>,
    only_wanted: bool,
    memo: Option<&Memo>,
) -> Digests {
    if let Some(memo) = memo {
        let (keys, values) = memo.digests(ctx, side, wanted, only_wanted);
        ctx.shared.forget_parsed();
        return Digests { keys, values };
    }
    let views = side.views.len();
    let mut keys = wanted;
    if !only_wanted {
        let per_view: Vec<BTreeSet<String>> = (0..views)
            .into_par_iter()
            .map(|view| Resolver::new(ctx, side, view).keys())
            .collect();
        per_view.into_iter().for_each(|view| keys.extend(view));
    }
    let keys: Vec<String> = keys.into_iter().collect();
    let values = (0..views)
        .into_par_iter()
        .map(|view| {
            let resolver = Resolver::new(ctx, side, view);
            keys.iter()
                .map(|key| {
                    resolver
                        .resolve(key)
                        .map(|descriptor| canon::digest(&descriptor))
                })
                .collect()
        })
        .collect();
    ctx.shared.forget_parsed();
    Digests { keys, values }
}
