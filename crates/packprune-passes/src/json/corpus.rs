//! Each JSON pass proved on a real pack named by environment variables, ignored by default
//! (`cargo test -p packprune-passes --release -- --ignored json::corpus`).
//!
//! - `CORPUS_PACK`: the pack, a folder or zip.
//! - `CORPUS_JAR`, `CORPUS_ASSET_INDEX`: the client jar and the launcher asset index.
//!
//! No usage source is wired: the analysis keeps what the pack's own roots reach.

use std::env;
use std::time::Instant;

use packprune_pack::{ClientRules, Limits, PackInput, RunCx, VanillaIndex};
use packprune_pass::{Context, Enabled, NoCache, Pass, Scope, prove};

fn var(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("set {name}"))
}

fn proved(pass: Option<&dyn Pass>) {
    let limits = Limits::default();
    let jar = var("CORPUS_JAR");
    let vanilla = VanillaIndex::from_jar(jar.as_str().as_ref(), &limits)
        .unwrap()
        .with_asset_index(var("CORPUS_ASSET_INDEX").as_str().as_ref())
        .unwrap();
    let client = PackInput::open(jar.as_str().as_ref(), &limits).unwrap();
    let pack = PackInput::open(var("CORPUS_PACK").as_str().as_ref(), &limits).unwrap();
    let run = RunCx::default();
    let rules = ClientRules::for_version(&vanilla.version);
    let cx = Context {
        run: &run,
        vanilla: &vanilla,
        rules: &rules,
        sources: &[],
        selectors: &[],
        cache: &NoCache,
    };
    let passes = pass
        .map(|pass| Enabled {
            pass,
            scope: Scope::all(),
            options: toml::Table::new(),
        })
        .into_iter()
        .collect();
    let name = pass.map_or("none", |pass| pass.name());

    let start = Instant::now();
    let proof = prove(&cx, &client, pack.clone(), passes).unwrap();

    let report = proof.reports.first();
    eprintln!(
        "{} {name}: pruned {}, re-pruned {}, {} files changed, {:?}; oracle {}; {} -> {} bytes ({:?})",
        pack.name,
        proof.pruned.len(),
        proof.repruned.len(),
        report.map_or(0, |report| report.changed.len()),
        report.map(|report| &report.changes),
        proof
            .proof
            .as_ref()
            .map_or("not run: nothing changed".into(), |c| format!(
                "{} views, {} keys, {} differ",
                c.views, c.keys, c.differing_keys
            )),
        report.map_or(0, |report| report.bytes_before),
        report.map_or(0, |report| report.bytes_after),
        start.elapsed(),
    );
    if let Some(comparison) = &proof.proof {
        assert_eq!(comparison.differing_keys, 0, "{:?}", comparison.differences);
    }
}

#[test]
#[ignore = "needs CORPUS_PACK, CORPUS_JAR and CORPUS_ASSET_INDEX"]
fn corpus_none() {
    proved(None);
}

#[test]
#[ignore = "needs CORPUS_PACK, CORPUS_JAR and CORPUS_ASSET_INDEX"]
fn corpus_short_json() {
    proved(Some(&super::short::ShortJson));
}

#[test]
#[ignore = "needs CORPUS_PACK, CORPUS_JAR and CORPUS_ASSET_INDEX"]
fn corpus_bloat_keys() {
    proved(Some(&super::bloat::BloatKeys));
}

#[test]
#[ignore = "needs CORPUS_PACK, CORPUS_JAR and CORPUS_ASSET_INDEX"]
fn corpus_drop_unread() {
    proved(Some(&super::unread::DropUnread));
}
