//! The texture and crop passes proved on a real pack named by environment variables, ignored by default
//! (`cargo test -p packprune-passes --release -- --ignored textures::tests::corpus --nocapture`).
//!
//! - `CORPUS_PACK`: the pack, a folder or zip.
//! - `CORPUS_JAR`, `CORPUS_ASSET_INDEX`: the client jar and the launcher asset index.
//!
//! No usage source is wired here: the analysis keeps what the pack's own roots reach.

use std::env;
use std::time::Instant;

use camino::Utf8PathBuf;
use packprune_pack::rules::Glob;
use packprune_pack::{ClientRules, Limits, PackInput, RunCx, VanillaIndex};
use packprune_pass::{Context, Enabled, Loss, NoCache, Pass, Scope, prove};

fn var(name: &str) -> Utf8PathBuf {
    env::var(name)
        .unwrap_or_else(|_| panic!("set {name}"))
        .into()
}

#[test]
#[ignore = "needs CORPUS_PACK, CORPUS_JAR and CORPUS_ASSET_INDEX"]
fn corpus_passes_are_proved() {
    let limits = Limits::default();
    let vanilla = VanillaIndex::from_jar(&var("CORPUS_JAR"), &limits)
        .unwrap()
        .with_asset_index(&var("CORPUS_ASSET_INDEX"))
        .unwrap();
    let run = RunCx::default();
    let rules = ClientRules::for_version(&vanilla.version);
    let jar = PackInput::open(&var("CORPUS_JAR"), &limits).unwrap();
    let pack = PackInput::open(&var("CORPUS_PACK"), &limits).unwrap();
    let cx = Context {
        run: &run,
        vanilla: &vanilla,
        rules: &rules,
        sources: &[],
        selectors: &[],
        cache: &NoCache,
    };
    for pass in crate::textures::passes()
        .into_iter()
        .chain(crate::crop::passes())
    {
        let pass: &dyn Pass = &*pass;
        let scope = if pass.loss() == Loss::Lossy {
            let all = Glob::path("assets/**");
            Scope::matching(move |path| all.matches(path.as_str()))
        } else {
            Scope::all()
        };
        let start = Instant::now();
        let proof = prove(
            &cx,
            &jar,
            pack.clone(),
            vec![Enabled {
                pass,
                scope,
                options: toml::Table::new(),
            }],
        )
        .unwrap_or_else(|error| panic!("{}: {error}", pass.name()));
        let comparison = proof.proof.as_ref().or(proof.loss.as_ref());
        eprintln!(
            "{} {}: pruned {}, {} files changed, re-pruned {}; {}; oracle {} ({:?})",
            pack.name,
            pass.name(),
            proof.pruned.len(),
            proof.reports[0].changed.len(),
            proof.repruned.len(),
            proof.reports[0].changes.join("; "),
            comparison.map_or("not run".into(), |c| format!(
                "{} views, {} keys, {} differ",
                c.views, c.keys, c.differing_keys
            )),
            start.elapsed(),
        );
    }
}
