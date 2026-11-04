//! The drop-in's failure matrix, case by case, on the run's fixture pack.

use std::collections::{BTreeMap, BTreeSet};

use camino::{Utf8Path, Utf8PathBuf};
use packprune_pack::{FileKind, PhysPath};
use packprune_pass::{
    Analysed, Context, Edits, FileCx, FileOptimizer, Loss, Optimised, Pass, PassCx, PassError,
    Stage,
};

use super::super::SourceRequest;
use super::super::tests::{Fixture, MODEL, ORPHAN, assert_nothing_written, entries, files, path};
use super::*;

fn drop_in(fixture: &Fixture, input: &Utf8Path, output: &str) -> DropIn {
    DropIn {
        input: input.to_owned(),
        output: path(&fixture.dir, output),
        profile: Profile::default(),
        limits: InputLimits::default(),
        quirks: Quirks::default(),
        reports: None,
    }
}

fn prune(
    cx: &Context<'_>,
    state: Analysed,
) -> Result<(Analysed, BTreeSet<PhysPath>), packprune_pass::RunError> {
    cx.prune(state)
}

fn input_files() -> BTreeMap<String, Vec<u8>> {
    files()
        .into_iter()
        .map(|(path, text)| (path.to_owned(), text.as_bytes().to_vec()))
        .collect()
}

/// What prune-only writes: the input less the orphan, every kept file byte for byte.
fn pruned_files() -> BTreeMap<String, Vec<u8>> {
    let mut files = input_files();
    files.remove(ORPHAN);
    files
}

fn run_with(
    fixture: &Fixture,
    registry: &Registry,
    first_prune: Reprune,
    reprune: Reprune,
) -> (Squashed, Utf8PathBuf) {
    let input = fixture.pack("in", &files());
    let drop_in = drop_in(fixture, &input, "out.zip");
    let request = fixture.request(&input, "out.zip");
    let squashed = squash_with(&drop_in, Ok(request), registry, first_prune, reprune).unwrap();
    (squashed, drop_in.output)
}

struct Panics;

impl Pass for Panics {
    fn name(&self) -> &'static str {
        "panics"
    }
    fn stage(&self) -> Stage {
        Stage::Models
    }
    fn loss(&self) -> Loss {
        Loss::Lossless
    }
    fn options_schema(&self) -> schemars::Schema {
        schemars::json_schema!({"type": "object"})
    }
    fn run(&self, _: &PassCx<'_>) -> Result<Edits, PassError> {
        panic!("planted pass panic")
    }
}

/// A JSON optimiser that fails on models, or breaks them so that only the self-check can tell.
struct Json {
    fail: bool,
}

impl FileOptimizer for Json {
    fn name(&self) -> &'static str {
        "json"
    }
    fn loss(&self) -> Loss {
        Loss::Lossless
    }
    fn applies(&self, kind: FileKind, _: &PhysPath) -> bool {
        kind == FileKind::Model
    }
    fn options_schema(&self) -> schemars::Schema {
        schemars::json_schema!({"type": "object"})
    }
    fn optimise(
        &self,
        _: &[u8],
        _: &FileCx,
        _: &toml::Table,
    ) -> Result<Option<Optimised>, PassError> {
        if self.fail {
            return Err(PassError::Failed {
                pass: "json",
                why: "planted optimiser error".into(),
            });
        }
        Ok(Some(Optimised {
            bytes: b"{".to_vec(),
            loss: None,
        }))
    }
}

/// The re-prune drops the model the item draws: the oracle sees it.
fn bad_reprune(
    cx: &Context<'_>,
    state: Analysed,
) -> Result<(Analysed, BTreeSet<PhysPath>), packprune_pass::RunError> {
    let (state, mut dropped) = cx.prune(state)?;
    let mut pack = state.pack;
    let model = PhysPath::new(MODEL).unwrap();
    pack.files.remove(&model);
    dropped.insert(model);
    Ok((cx.analyse(pack)?, dropped))
}

/// An output whose directory cannot take a file is refused before anything is read.
#[test]
fn an_unwritable_output_is_refused_and_nothing_is_written() {
    let fixture = Fixture::new();
    let input = fixture.pack("in", &files());
    let drop_in = drop_in(&fixture, &input, "absent/out.zip");
    let request = fixture.request(&input, "absent/out.zip");

    let error = squash(&drop_in, Ok(request)).unwrap_err();

    assert!(error.is_refusal(), "{error}");
    assert!(error.to_string().contains("output_file_path"), "{error}");
    assert!(!drop_in.output.exists());
}

/// The output inside the pack directory is refused.
#[test]
fn an_output_inside_the_input_is_refused() {
    let fixture = Fixture::new();
    let input = fixture.pack("in", &files());
    let mut drop_in = drop_in(&fixture, &input, "out.zip");
    drop_in.output = input.join("out.zip");

    let error = squash(&drop_in, Err("no config".into())).unwrap_err();

    assert!(error.is_refusal(), "{error}");
    assert!(!drop_in.output.exists());
}

/// An input the reader refuses ships nothing, not even the original, whatever the config.
#[test]
fn a_refused_input_ships_nothing() {
    let fixture = Fixture::new();
    let input = fixture.pack("in", &files());
    std::os::unix::fs::symlink(
        input.join(MODEL),
        input.join("assets/fx/models/item/link.json"),
    )
    .unwrap();
    let drop_in = drop_in(&fixture, &input, "out.zip");

    for full in [
        Err("no config".to_owned()),
        Ok(fixture.request(&input, "out.zip")),
    ] {
        let error = squash(&drop_in, full).unwrap_err();
        assert!(error.is_refusal(), "{error}");
        assert!(error.to_string().contains("link.json"), "{error}");
        assert_nothing_written(&fixture, &drop_in.output);
    }
}

/// Without packprune's config, the input goes out as it is, orphan included.
#[test]
fn no_config_ships_the_original() {
    let fixture = Fixture::new();
    let input = fixture.pack("in", &files());
    let drop_in = drop_in(&fixture, &input, "out.zip");

    let squashed = squash(&drop_in, Err("no packprune.toml".into())).unwrap();

    assert_eq!(squashed.artefact, Artefact::Original);
    assert_eq!(squashed.reasons, ["no packprune.toml"]);
    assert_eq!(entries(&drop_in.output), input_files());
}

/// A usage source refused, or the analysis failing on its client jar, ships the original.
#[test]
fn a_refused_source_or_a_failed_analysis_ships_the_original() {
    let fixture = Fixture::new();
    let input = fixture.pack("in", &files());
    let drop_in = drop_in(&fixture, &input, "out.zip");
    let mut source = fixture.request(&input, "out.zip");
    source.sources.push(SourceRequest {
        id: "nexo".into(),
        path: path(&fixture.dir, "absent-nexo"),
    });
    let jar = path(&fixture.dir, "broken.jar");
    std::fs::write(&jar, b"not a zip").unwrap();
    let mut analysis = fixture.request(&input, "out.zip");
    analysis.vanilla.client_jar = jar;

    for (request, reason) in [(source, "absent-nexo"), (analysis, "broken.jar")] {
        let squashed = squash(&drop_in, Ok(request)).unwrap();

        assert_eq!(squashed.artefact, Artefact::Original);
        assert!(
            squashed.reasons.concat().contains(reason),
            "{:?}",
            squashed.reasons
        );
        assert_eq!(entries(&drop_in.output), input_files());
    }
}

/// A pass that panics ships the pruned pack.
#[test]
fn a_panicking_pass_ships_the_pruned_pack() {
    let fixture = Fixture::new();
    let registry = Registry {
        passes: vec![Box::new(Panics)],
        optimisers: Vec::new(),
    };

    let (squashed, output) = run_with(&fixture, &registry, prune, prune);

    assert_eq!(squashed.artefact, Artefact::Pruned);
    assert!(
        squashed.reasons[0].contains("planted pass panic"),
        "{:?}",
        squashed.reasons
    );
    assert_eq!(entries(&output), pruned_files());
}

/// The oracle refusing the passes ships the pruned pack.
#[test]
fn an_oracle_difference_ships_the_pruned_pack() {
    let fixture = Fixture::new();

    let (squashed, output) = run_with(&fixture, &Registry::default(), prune, bad_reprune);

    assert_eq!(squashed.artefact, Artefact::Pruned);
    assert!(
        squashed.reasons[0].contains("the oracle found"),
        "{:?}",
        squashed.reasons
    );
    assert_eq!(entries(&output), pruned_files());
}

/// A file optimiser's error, or its output failing the self-check, ships the pruned pack.
#[test]
fn a_failed_optimiser_or_self_check_ships_the_pruned_pack() {
    for (fail, reason) in [(true, "planted optimiser error"), (false, "self-check")] {
        let fixture = Fixture::new();
        let registry = Registry {
            passes: Vec::new(),
            optimisers: vec![Box::new(Json { fail })],
        };

        let (squashed, output) = run_with(&fixture, &registry, prune, prune);

        assert_eq!(squashed.artefact, Artefact::Pruned, "{reason}");
        assert!(
            squashed.reasons[0].contains(reason),
            "{:?}",
            squashed.reasons
        );
        assert_eq!(entries(&output), pruned_files());
    }
}

/// When the pruned pack fails its own self-check too, the prune was wrong: the original ships.
#[test]
fn a_pruned_pack_failing_its_self_check_ships_the_original() {
    let fixture = Fixture::new();
    let registry = Registry {
        passes: vec![Box::new(Panics)],
        optimisers: Vec::new(),
    };

    let (squashed, output) = run_with(&fixture, &registry, bad_reprune, prune);

    assert_eq!(squashed.artefact, Artefact::Original);
    assert_eq!(squashed.reasons.len(), 2, "{:?}", squashed.reasons);
    assert!(
        squashed.reasons[1].contains("self-check"),
        "{:?}",
        squashed.reasons
    );
    assert_eq!(entries(&output), input_files());
}

/// A write error fails the drop-in, whichever artefact it was writing, and leaves no staging behind.
#[test]
fn a_write_error_ships_nothing_and_exits_3() {
    let fixture = Fixture::new();
    let input = fixture.pack("in", &files());
    let drop_in = drop_in(&fixture, &input, "out.zip");
    // A directory where the zip goes: every check passes, the final rename fails.
    std::fs::create_dir(&drop_in.output).unwrap();

    for full in [
        Ok(fixture.request(&input, "out.zip")),
        Err("no config".to_owned()),
    ] {
        let error = squash(&drop_in, full).unwrap_err();

        assert!(!error.is_refusal(), "{error}");
        assert!(drop_in.output.is_dir());
        let left: Vec<_> = std::fs::read_dir(fixture.dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.starts_with(".packprune-"))
            .collect();
        assert!(left.is_empty(), "staging left behind: {left:?}");
    }
}

/// The happy path: the optimised run ships, with no reason.
#[test]
fn a_clean_run_ships_the_optimised_pack() {
    let fixture = Fixture::new();

    let (squashed, output) = run_with(&fixture, &Registry::default(), prune, prune);

    assert_eq!(squashed.artefact, Artefact::Optimised);
    assert!(squashed.reasons.is_empty());
    assert!(squashed.summary.unwrap().proof.is_some());
    assert!(!entries(&output).contains_key(ORPHAN));
}
