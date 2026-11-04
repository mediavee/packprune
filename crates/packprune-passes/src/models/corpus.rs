//! The model passes proved on a real pack named by environment variables, ignored by default
//! (`cargo test -p packprune-passes --release -- --ignored models::corpus`).
//!
//! - `CORPUS_PACK`: the pack, a folder or zip.
//! - `CORPUS_JAR`, `CORPUS_ASSET_INDEX`: the client jar and the launcher asset index.

use std::env;

use packprune_pack::{ClientRules, Limits, PackInput, RunCx, VanillaIndex};
use packprune_pass::{Context, Enabled, NoCache, Pass, Scope, prove};

use super::{DegenerateFaces, IdenticalModels, SharedDisplay};

fn var(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("set {name}"))
}

fn proved(passes: &[&dyn Pass]) {
    let jar = var("CORPUS_JAR");
    let limits = Limits::default();
    let run = RunCx::default();
    let vanilla = VanillaIndex::from_jar(jar.as_str().into(), &limits)
        .unwrap()
        .with_asset_index(var("CORPUS_ASSET_INDEX").as_str().into())
        .unwrap();
    let rules = ClientRules::for_version(&vanilla.version);
    let assets = PackInput::open(jar.as_str().into(), &limits).unwrap();
    let cx = Context {
        run: &run,
        vanilla: &vanilla,
        rules: &rules,
        sources: &[],
        selectors: &[],
        cache: &NoCache,
    };
    let proof = prove(
        &cx,
        &assets,
        PackInput::open(var("CORPUS_PACK").as_str().into(), &limits).unwrap(),
        passes
            .iter()
            .map(|pass| Enabled {
                pass: *pass,
                scope: Scope::all(),
                options: toml::Table::new(),
            })
            .collect(),
    )
    .unwrap();
    let names = passes
        .iter()
        .map(|pass| pass.name())
        .collect::<Vec<_>>()
        .join(",");
    if let Some(comparison) = &proof.proof {
        assert_eq!(comparison.differing_keys, 0, "{:?}", comparison.differences);
        eprintln!(
            "{}: oracle {} views, {} keys, {} differ",
            names, comparison.views, comparison.keys, comparison.differing_keys
        );
    }
    eprintln!(
        "{names}: {:?}",
        proof
            .reports
            .iter()
            .map(|report| &report.changes)
            .collect::<Vec<_>>()
    );
}

#[test]
#[ignore = "needs CORPUS_PACK, CORPUS_JAR and CORPUS_ASSET_INDEX"]
fn corpus_degenerate_faces() {
    proved(&[&DegenerateFaces]);
}

#[test]
#[ignore = "needs CORPUS_PACK, CORPUS_JAR and CORPUS_ASSET_INDEX"]
fn corpus_identical_models() {
    proved(&[&IdenticalModels]);
}

#[test]
#[ignore = "needs CORPUS_PACK, CORPUS_JAR and CORPUS_ASSET_INDEX"]
fn corpus_shared_display() {
    proved(&[&SharedDisplay]);
}

#[test]
#[ignore = "needs CORPUS_PACK, CORPUS_JAR and CORPUS_ASSET_INDEX"]
fn corpus_combined_models() {
    proved(&[&SharedDisplay, &IdenticalModels, &DegenerateFaces]);
}
