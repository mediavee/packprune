//! Runs on a real pack, named by environment variables (`cargo test -p packprune-graph --release -- --ignored corpus`).
//!
//! - `PACKPRUNE_CORPUS_PACK`, `_JAR`, `_ASSET_INDEX`: the pack, the client jar, the launcher asset index.
//! - `PACKPRUNE_CORPUS_OUTPUTS`: a directory of edited copies of the pack (zips, such as pass outputs) for the
//!   `update` run.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use camino::Utf8PathBuf;
use packprune_pack::{
    ClientRules, Layout, Limits, PackInput, PackMeta, PhysPath, RunCx, VanillaIndex,
};

use super::support::open;
use crate::{Change, Computed, Inputs, Policy, Reached, ReferenceIndex};

fn var(name: &str) -> Utf8PathBuf {
    std::env::var(name)
        .unwrap_or_else(|_| panic!("set {name}"))
        .into()
}

fn vanilla() -> VanillaIndex {
    VanillaIndex::from_jar(&var("PACKPRUNE_CORPUS_JAR"), &Limits::default())
        .unwrap()
        .with_asset_index(&var("PACKPRUNE_CORPUS_ASSET_INDEX"))
        .unwrap()
}

/// What changed from `before` to `after` as a pass reports it: a path gone and one added with the same bytes is a
/// move, and every other difference is touched.
fn change(before: &PackInput, after: &PackInput) -> Change {
    let mut touched = BTreeSet::new();
    let mut gone: BTreeMap<&[u8], Vec<&PhysPath>> = BTreeMap::new();
    for (path, entry) in &before.files {
        match after.files.get(path) {
            Some(other) if other.blob == entry.blob => {}
            Some(_) => {
                touched.insert(path.clone());
            }
            None => gone.entry(&entry.blob).or_default().push(path),
        }
    }
    let mut moved = BTreeMap::new();
    for (path, entry) in &after.files {
        if before.files.contains_key(path) {
            continue;
        }
        match gone.get_mut(&*entry.blob).and_then(Vec::pop) {
            Some(old) => {
                moved.insert(old.clone(), path.clone());
            }
            None => {
                touched.insert(path.clone());
            }
        }
    }
    touched.extend(gone.into_values().flatten().cloned());
    let meta = |pack: &PackInput| PackMeta::read(pack).unwrap();
    Change {
        layout_changed: meta(before) != meta(after),
        touched,
        moved,
    }
}

/// After each edited copy, `update` of the input's index equals `build` on the copy.
#[test]
#[ignore = "needs a real pack: see the module docs"]
fn corpus_update_matches_build() {
    let cx = RunCx::default();
    let vanilla = vanilla();
    let rules = ClientRules::for_version(&vanilla.version);
    let input = open(&var("PACKPRUNE_CORPUS_PACK"));
    let input_layout = Layout::new(&input, PackMeta::read(&input).unwrap(), &vanilla);
    let built = ReferenceIndex::build(&cx, &input, &input_layout, &rules, &Computed);
    let mut outputs: Vec<Utf8PathBuf> = var("PACKPRUNE_CORPUS_OUTPUTS")
        .read_dir_utf8()
        .unwrap()
        .map(|entry| entry.unwrap().into_path())
        .filter(|path| path.extension() == Some("zip") && !path.as_str().ends_with(".dropped.zip"))
        .collect();
    outputs.sort();
    assert!(!outputs.is_empty());
    let mut failures = Vec::new();
    for output in &outputs {
        let after = open(output);
        let layout = Layout::new(&after, PackMeta::read(&after).unwrap(), &vanilla);
        let change = change(&input, &after);
        let start = Instant::now();
        let mut updated = built.clone();
        updated.update(&cx, &after, &layout, &rules, &change, &Computed);
        let update_time = start.elapsed();
        let start = Instant::now();
        let fresh = ReferenceIndex::build(&cx, &after, &layout, &rules, &Computed);
        eprintln!(
            "{}: {} touched, {} moved, layout changed {}; update {update_time:?}, build {:?}",
            output.file_name().unwrap(),
            change.touched.len(),
            change.moved.len(),
            change.layout_changed,
            start.elapsed()
        );
        if updated.all() != fresh.all() {
            let files: BTreeSet<&PhysPath> =
                updated.all().keys().chain(fresh.all().keys()).collect();
            let differing: Vec<String> = files
                .into_iter()
                .filter(|file| updated.references(file) != fresh.references(file))
                .take(10)
                .map(|file| {
                    format!(
                        "{}: {} file {file}",
                        output.file_name().unwrap(),
                        "update differs from build on"
                    )
                })
                .collect();
            failures.extend(differing);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Wall time of each step on the pack; nothing else is read, so the process's peak RSS is the analysis's.
#[test]
#[ignore = "needs a real pack: see the module docs"]
fn corpus_measure() {
    let cx = RunCx::default();
    let start = Instant::now();
    let vanilla = vanilla();
    let pack = open(&var("PACKPRUNE_CORPUS_PACK"));
    let read = start.elapsed();
    let start = Instant::now();
    let layout = Layout::new(&pack, PackMeta::read(&pack).unwrap(), &vanilla);
    let rules = ClientRules::for_version(&vanilla.version);
    let index = ReferenceIndex::build(&cx, &pack, &layout, &rules, &Computed);
    let built = start.elapsed();
    let start = Instant::now();
    let reached = Reached::analyse(&Inputs {
        cx: &cx,
        pack: &pack,
        layout: &layout,
        rules: &rules,
        vanilla: &vanilla,
        index: &index,
        sources: &[],
        selectors: &[],
        policy: Policy::Pack,
    })
    .unwrap();
    eprintln!(
        "read {read:?}, layout and build {built:?}, graph {:?}: {} files, {} edges",
        start.elapsed(),
        reached.files.len(),
        reached.edges.len()
    );
}
