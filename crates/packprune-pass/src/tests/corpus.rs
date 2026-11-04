//! Runs on a real pack named by environment variables, ignored by default
//! (`cargo test -p packprune-pass --release -- --ignored corpus`).
//!
//! - `CORPUS_PACK`: the pack, a folder or zip.
//! - `CORPUS_JAR`, `CORPUS_ASSET_INDEX`: the client jar and the launcher asset index.
//!
//! No usage source is wired here: the analysis keeps what the pack's own roots reach.

use std::env;
use std::time::Instant;

use packprune_pack::Limits;

use super::*;
use crate::{Enabled, Kept, Scope, prove, run};

fn var(name: &str) -> Utf8PathBuf {
    env::var(name)
        .unwrap_or_else(|_| panic!("set {name}"))
        .into()
}

struct Corpus {
    run: RunCx,
    vanilla: VanillaIndex,
    rules: ClientRules,
    jar: PackInput,
    pack: PackInput,
}

impl Corpus {
    fn open() -> Self {
        let limits = Limits::default();
        let vanilla = VanillaIndex::from_jar(&var("CORPUS_JAR"), &limits)
            .unwrap()
            .with_asset_index(&var("CORPUS_ASSET_INDEX"))
            .unwrap();
        Self {
            run: RunCx::default(),
            rules: ClientRules::for_version(&vanilla.version),
            vanilla,
            jar: PackInput::open(&var("CORPUS_JAR"), &limits).unwrap(),
            pack: PackInput::open(&var("CORPUS_PACK"), &limits).unwrap(),
        }
    }

    fn cx(&self) -> Context<'_> {
        Context {
            run: &self.run,
            vanilla: &self.vanilla,
            rules: &self.rules,
            sources: &[],
            selectors: &[],
            cache: &NoCache,
        }
    }
}

/// No pass, and a pass that edits nothing, give the input's entries.
#[test]
#[ignore = "needs CORPUS_PACK, CORPUS_JAR and CORPUS_ASSET_INDEX"]
fn corpus_identity() {
    let corpus = Corpus::open();
    let cx = corpus.cx();
    let state = cx.analyse(corpus.pack.clone()).unwrap();
    let nothing = Edit {
        loss: Loss::Lossless,
        edits: Box::new(|_| Edits::default()),
    };
    let enabled = Enabled {
        pass: &nothing,
        scope: Scope::all(),
        options: toml::Table::new(),
    };

    let none = run(&cx, &Kept::of(&state), &state, &[]).unwrap();
    let empty = run(&cx, &Kept::of(&state), &state, &[enabled]).unwrap();

    assert_eq!(entries(&none.state.pack), entries(&corpus.pack));
    assert_eq!(entries(&empty.state.pack), entries(&corpus.pack));
    assert_eq!(empty.state.pack.digest, corpus.pack.digest);
    eprintln!(
        "{}: {} entries kept as they are",
        corpus.pack.name,
        corpus.pack.files.len()
    );
}

/// Every model no name keeps renamed beside itself: the harness's oracle finds the result the same for the
/// player, and the incremental index equals a rebuild.
#[test]
#[ignore = "needs CORPUS_PACK, CORPUS_JAR and CORPUS_ASSET_INDEX"]
fn corpus_rename_is_proved() {
    let corpus = Corpus::open();
    let cx = corpus.cx();
    let rename = Rename {
        name: "rename-models",
        targets: Targets::Models,
    };
    let enabled = Enabled {
        pass: &rename,
        scope: Scope::all(),
        options: toml::Table::new(),
    };

    let start = Instant::now();
    let proof = prove(&cx, &corpus.jar, corpus.pack.clone(), vec![enabled]).unwrap();

    let comparison = proof.proof.as_ref().expect("the rename changed something");
    eprintln!(
        "{}: pruned {}, renamed {} models, {} files changed, re-pruned {}; oracle {} views, {} keys, {} differ ({:?})",
        corpus.pack.name,
        proof.pruned.len(),
        proof.reports[0].changes.len(),
        proof.reports[0].changed.len(),
        proof.repruned.len(),
        comparison.views,
        comparison.keys,
        comparison.differing_keys,
        start.elapsed(),
    );
    assert!(!proof.reports[0].changes.is_empty());
    assert_eq!(comparison.differing_keys, 0, "{:?}", comparison.differences);
    assert_index_rebuilt(&cx, &proof.output);
}
