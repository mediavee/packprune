//! The passes `CORPUS_PASSES` lists (comma-separated) proved on a real pack named by environment variables, ignored
//! by default (`cargo test --release -p packprune-passes names::tests::corpus -- --ignored --nocapture`).
//!
//! - `CORPUS_PACK`: the pack, a folder or zip.
//! - `CORPUS_JAR`, `CORPUS_ASSET_INDEX`: the client jar and the launcher asset index.
//! - `CORPUS_OLDEST`: the `oldest_format` of `overlay-floor`, when it is listed.

use std::env;

use super::*;
use packprune_pack::{ClientRules, Limits, RunCx, VanillaIndex};
use packprune_pass::{Enabled, Pass, Scope, order, prove};

fn var(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("set {name}"))
}

struct Corpus {
    fixture: Fixture,
    pack: PackInput,
    passes: Vec<Box<dyn Pass>>,
}
impl Corpus {
    fn open() -> Self {
        let limits = Limits::default();
        let jar = var("CORPUS_JAR");
        let vanilla = VanillaIndex::from_jar(jar.as_str().into(), &limits)
            .unwrap()
            .with_asset_index(var("CORPUS_ASSET_INDEX").as_str().into())
            .unwrap();
        let requested = var("CORPUS_PASSES");
        let names: BTreeSet<_> = requested.split(',').collect();
        let passes: Vec<_> = crate::passes()
            .into_iter()
            .filter(|pass| names.contains(pass.name()))
            .collect();
        assert_eq!(passes.len(), names.len(), "unknown passes: {names:?}");
        Self {
            fixture: Fixture {
                run: RunCx::default(),
                rules: ClientRules::for_version(&vanilla.version),
                vanilla,
                assets: PackInput::open(jar.as_str().into(), &limits).unwrap(),
            },
            pack: PackInput::open(var("CORPUS_PACK").as_str().into(), &limits).unwrap(),
            passes,
        }
    }
    fn enabled(&self) -> Vec<Enabled<'_>> {
        let mut enabled: Vec<_> = self
            .passes
            .iter()
            .map(|pass| Enabled {
                pass: pass.as_ref(),
                scope: Scope::all(),
                options: if pass.name() == "overlay-floor" {
                    let oldest = var("CORPUS_OLDEST").parse::<i64>().unwrap();
                    toml::toml! { oldest_format = oldest }
                } else {
                    toml::Table::new()
                },
            })
            .collect();
        order(&mut enabled);
        enabled
    }
}

#[test]
#[ignore = "needs CORPUS_PACK, CORPUS_JAR, CORPUS_ASSET_INDEX and CORPUS_PASSES"]
fn corpus_strict_proof() {
    let corpus = Corpus::open();
    let proof = prove(
        &corpus.fixture.cx(),
        &corpus.fixture.assets,
        corpus.pack.clone(),
        corpus.enabled(),
    )
    .unwrap();
    if let Some(comparison) = &proof.proof {
        eprintln!(
            "strict oracle: {} views, {} keys, {} differing",
            comparison.views, comparison.keys, comparison.differing_keys
        );
        assert_eq!(comparison.differing_keys, 0);
    }
    if let Some(comparison) = &proof.loss {
        eprintln!(
            "lossy oracle: {} views, {} keys, {} differing",
            comparison.views, comparison.keys, comparison.differing_keys
        );
    }
}
