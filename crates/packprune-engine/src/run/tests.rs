//! The run on small packs over the fixtures' client (`tests/fixtures/vanilla/`, zipped into a jar).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use camino::{Utf8Path, Utf8PathBuf};
use packprune_codec::png;
use packprune_config::{PassSelection, Profile};
use packprune_pack::{FileKind, Limits, PackInput, PhysPath};
use packprune_pass::{Analysed, Context, FileCx, FileOptimizer, Loss, Optimised, PassError};
use packprune_zip::{EntryClass, WriteOptions};

use super::*;

pub(super) const META: &str =
    r#"{"pack": {"min_format": 88, "max_format": 88, "description": "t"}}"#;
pub(super) const ITEM: &str = "assets/fx/items/box.json";
pub(super) const MODEL: &str = "assets/fx/models/item/box.json";
pub(super) const ORPHAN: &str = "assets/fx/models/item/orphan.json";
pub(super) const README: &str = "README.txt";

fn repo() -> Utf8PathBuf {
    Utf8Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub(super) struct Fixture {
    pub(super) dir: tempfile::TempDir,
    pub(super) jar: Utf8PathBuf,
}

impl Fixture {
    pub(super) fn new() -> Self {
        Self::with_client(&[])
    }

    /// The fixtures' client with `extra` files added.
    fn with_client(extra: &[(&str, &str)]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = repo().join("tests/fixtures/vanilla");
        let client = PackInput::open(&root, &Limits::default()).unwrap();
        let mut entries: BTreeMap<String, (packprune_pack::Blob, EntryClass)> = client
            .files
            .iter()
            .map(|(path, entry)| {
                (
                    path.as_str().to_owned(),
                    (Arc::clone(&entry.blob), EntryClass::Other),
                )
            })
            .collect();
        for (path, text) in extra {
            entries.insert(
                (*path).to_owned(),
                (text.as_bytes().into(), EntryClass::Other),
            );
        }
        let jar = path(&dir, "client.jar");
        packprune_zip::write(&entries, &jar, &WriteOptions::default()).unwrap();
        Self { dir, jar }
    }

    pub(super) fn pack(&self, name: &str, files: &[(&str, &str)]) -> Utf8PathBuf {
        let root = path(&self.dir, name);
        for (file, text) in files {
            let target = root.join(file);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, text).unwrap();
        }
        root
    }

    pub(super) fn request(&self, input: &Utf8Path, output: &str) -> Request {
        Request {
            input: input.to_owned(),
            output: path(&self.dir, output),
            vanilla: VanillaInputs {
                client_jar: self.jar.clone(),
                asset_index: None,
            },
            sources: Vec::new(),
            prune_unused: Vec::new(),
            profile: Profile::default(),
            limits: Default::default(),
            budgets: Default::default(),
            cache: None,
            expect: None,
        }
    }
}

pub(super) fn path(dir: &tempfile::TempDir, name: &str) -> Utf8PathBuf {
    Utf8PathBuf::from_path_buf(dir.path().join(name)).unwrap()
}

/// A pack whose item names a model, with an orphan model, a file no client reads, and JSON the passes and the
/// minifier shorten.
pub(super) fn files() -> Vec<(&'static str, &'static str)> {
    vec![
        ("pack.mcmeta", META),
        (
            ITEM,
            r#"{ "model": { "type": "minecraft:model", "model": "fx:item/box" } }"#,
        ),
        (
            MODEL,
            r##"{ "credit": "made by hand", "textures": { "particle": "minecraft:block/stone" }, "elements": [ { "from": [ 0.0, 0, 0 ], "to": [ 16, 16, 16 ], "faces": { "north": { "texture": "#particle" } } } ] }"##,
        ),
        (
            ORPHAN,
            r#"{ "textures": { "particle": "minecraft:block/stone" } }"#,
        ),
        (README, "not read by any client"),
    ]
}

pub(super) fn entries(zip: &Utf8Path) -> BTreeMap<String, Vec<u8>> {
    PackInput::open(zip, &Limits::default())
        .unwrap()
        .files
        .into_iter()
        .map(|(path, entry)| (path.as_str().to_owned(), entry.blob.to_vec()))
        .collect()
}

fn none() -> Profile {
    Profile {
        passes: Some(PassSelection::Preset("none".into())),
        ..Profile::default()
    }
}

#[test]
fn both_on_one_file_make_one_target() {
    use packprune_codec::{audio, metrics};

    const SOUND: &str = "assets/fx/sounds/high.ogg";
    let original = include_bytes!("../../../../tests/fixtures/sounds/stereo_96k_1s.ogg");
    let fixture = Fixture::new();
    let input = fixture.pack(
        "audio",
        &[
            ("pack.mcmeta", META),
            (
                "assets/fx/sounds.json",
                r#"{"high":{"sounds":["fx:high"]}}"#,
            ),
        ],
    );
    std::fs::create_dir_all(input.join("assets/fx/sounds")).unwrap();
    std::fs::write(input.join(SOUND), original).unwrap();
    let before = audio::decode(original).unwrap();
    let transformed = audio::resample(&audio::downmix(&before).unwrap(), 48_000).unwrap();
    let expected = audio::optimise(&audio::encode(&transformed, 0.06).unwrap()).unwrap();
    for names in [
        vec!["sound-mono", "sound-rate"],
        vec!["ogg-transcode", "sound-mono", "sound-rate"],
    ] {
        let mut config = "version = 2\n[profiles.default]\npasses = 'none'\n".to_owned();
        for name in names {
            config.push_str(&format!(
                "[profiles.default.lossy.{name}]\nglobs = ['**']\nmax_lsd = 1.1333\n"
            ));
        }
        let config = packprune_config::parse_with(
            &config,
            Utf8Path::new("audio.toml"),
            &Registry::default(),
        )
        .unwrap();
        let mut request = fixture.request(&input, "audio.zip");
        request.profile = config.profiles["default"].clone();
        let summary = optimise(&request).unwrap();
        let output = entries(&request.output);
        let after = audio::decode(&output[SOUND]).unwrap();
        let loss = metrics::sound_loss(&before, &after).unwrap();
        assert!(
            loss.lsd <= 1.1333,
            "cumulative LSD {} exceeds the selected threshold",
            loss.lsd
        );
        assert_eq!(after.channels.len(), 1);
        assert_eq!(after.rate, 48_000);
        assert_eq!(output[SOUND], expected, "one encode of the combined target");
        let recorded: Vec<_> = summary
            .passes
            .iter()
            .flat_map(|r| &r.measured)
            .filter(|r| r.metric == "lsd")
            .collect();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].value, loss.lsd);
    }
}

#[test]
fn combined_audio_targets_respect_scopes_quality_and_strictest_thresholds() {
    use packprune_codec::audio;

    let original = include_bytes!("../../../../tests/fixtures/sounds/stereo_96k_1s.ogg");
    let fixture = Fixture::new();
    let input = fixture.pack(
        "scoped-audio",
        &[
            ("pack.mcmeta", META),
            (
                "assets/fx/sounds.json",
                r#"{"all":{"sounds":["fx:both","fx:mono","fx:rate","fx:plain"]}}"#,
            ),
        ],
    );
    std::fs::create_dir_all(input.join("assets/fx/sounds")).unwrap();
    for name in ["both", "mono", "rate", "plain"] {
        std::fs::write(input.join(format!("assets/fx/sounds/{name}.ogg")), original).unwrap();
    }
    let before = audio::decode(original).unwrap();
    let encode =
        |pcm: &audio::Pcm, quality| audio::optimise(&audio::encode(pcm, quality).unwrap()).unwrap();
    let mono = audio::downmix(&before).unwrap();
    let both = encode(&audio::resample(&mono, 48_000).unwrap(), 0.4);
    let mono = encode(&mono, 0.3);
    let rate = encode(&audio::resample(&before, 48_000).unwrap(), 0.4);
    for limit in ["", "max_lsd = 0", "min_snr = 100"] {
        let text = format!(
            r#"version = 2
[profiles.default]
passes = 'none'
[profiles.default.lossy.sound-mono]
globs = ['**/both.ogg', '**/mono.ogg']
quality = 3
[profiles.default.lossy.sound-rate]
globs = ['**/both.ogg', '**/rate.ogg']
quality = 4
{limit}
[profiles.default.lossy.ogg-transcode]
globs = ['**/never.ogg']
quality = 10
max_lsd = 0
min_snr = 999
"#
        );
        let config = packprune_config::parse_with(
            &text,
            Utf8Path::new("scoped-audio.toml"),
            &Registry::default(),
        )
        .unwrap();
        let mut request = fixture.request(&input, "scoped-audio.zip");
        request.profile = config.profiles["default"].clone();
        let summary = optimise(&request).unwrap();
        let output = entries(&request.output);
        for (name, expected) in [
            (
                "both",
                if limit.is_empty() {
                    &both[..]
                } else {
                    &original[..]
                },
            ),
            ("mono", &mono[..]),
            (
                "rate",
                if limit.is_empty() {
                    &rate[..]
                } else {
                    &original[..]
                },
            ),
            ("plain", &original[..]),
        ] {
            assert_eq!(
                output[&format!("assets/fx/sounds/{name}.ogg")],
                expected,
                "{name}: {limit}"
            );
        }
        let measured = summary
            .passes
            .iter()
            .flat_map(|r| &r.measured)
            .filter(|r| r.metric == "lsd")
            .count();
        assert_eq!(measured, if limit.is_empty() { 3 } else { 1 });
    }
}

/// Without passes: the input less what the analysis finds removable, every kept file byte for byte.
#[test]
fn no_pass_writes_the_pruned_input_as_it_is() {
    let fixture = Fixture::new();
    let input = fixture.pack("in", &files());
    let mut request = fixture.request(&input, "out.zip");
    request.profile = none();

    let summary = optimise(&request).unwrap();

    let mut expected: BTreeMap<String, Vec<u8>> = files()
        .into_iter()
        .map(|(path, text)| (path.to_owned(), text.as_bytes().to_vec()))
        .collect();
    expected.remove(ORPHAN);
    assert_eq!(entries(&request.output), expected);
    assert_eq!(summary.pruned.files, 1);
    assert!(summary.passes.is_empty() && summary.proof.is_none());
}

/// The lossless default: passes and file optimisers ran, the oracle proved the passes, and the run reports them.
#[test]
fn the_lossless_run_is_proved_and_smaller() {
    let fixture = Fixture::new();
    let input = fixture.pack("in", &files());
    let request = fixture.request(&input, "out.zip");

    let summary = optimise(&request).unwrap();

    let written = entries(&request.output);
    assert!(!written.contains_key(README), "drop-unread dropped it");
    assert!(!written.contains_key(ORPHAN));
    assert!(written[MODEL].len() < files()[2].1.len());
    let proof = summary.proof.expect("the lossless passes changed the pack");
    assert_eq!(proof.differing_keys, 0);
    assert!(proof.keys > 0);
    assert!(
        summary
            .optimisers
            .iter()
            .any(|o| o.optimiser == "json" && o.files > 0)
    );
}

/// The re-prune after the passes drops a file the item still draws: the oracle refuses it, nothing is written.
#[test]
fn a_planted_bad_reprune_is_caught_by_the_oracle_before_write() {
    fn bad(
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
    let fixture = Fixture::new();
    let input = fixture.pack("in", &files());
    let request = fixture.request(&input, "out.zip");

    let error = execute(&request, &Registry::default(), bad).unwrap_err();

    let RunError::NotEquivalent { passes, verdict } = &error else {
        panic!("expected the oracle's refusal, got {error}");
    };
    assert!(passes.contains(&"drop-unread"), "{passes:?}");
    assert!(verdict.differing_keys > 0);
    assert!(!error.is_refusal(), "exit 3");
    assert_nothing_written(&fixture, &request.output);
}

/// An optimiser that breaks every model: the written pack, read back, has unreadable files; nothing is published.
#[test]
fn a_self_check_failure_publishes_nothing() {
    struct Break;
    impl FileOptimizer for Break {
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
            Ok(Some(Optimised {
                bytes: b"{".to_vec(),
                loss: None,
            }))
        }
    }
    let fixture = Fixture::new();
    let input = fixture.pack("in", &files());
    let request = fixture.request(&input, "out.zip");
    let registry = Registry {
        passes: packprune_passes::passes(),
        optimisers: vec![Box::new(Break)],
    };

    let error = execute(&request, &registry, |cx, state| cx.prune(state)).unwrap_err();

    let RunError::SelfCheck(differences) = &error else {
        panic!("expected the self-check's refusal, got {error}");
    };
    assert!(
        differences.contains(&format!("unreadable file: {MODEL}")),
        "{differences:?}"
    );
    assert_nothing_written(&fixture, &request.output);
}

/// The report an operator reviewed: `check` of the request, as `report` writes it.
fn reviewed(request: &Request) -> Expected {
    Expected {
        path: "reviewed/report.json".into(),
        report: check(request).unwrap().bundle.analysis.report,
    }
}

/// `--expect-report`: the report of this run's own inputs is the plan, and the run is the same as without it.
#[test]
fn a_run_expecting_the_report_of_its_inputs_writes_what_it_writes_without() {
    let fixture = Fixture::new();
    let input = fixture.pack("in", &files());
    let mut request = fixture.request(&input, "out.zip");
    optimise(&request).unwrap();
    let plain = std::fs::read(&request.output).unwrap();
    request.output = path(&fixture.dir, "expecting.zip");
    request.expect = Some(reviewed(&request));

    optimise(&request).unwrap();

    assert_eq!(std::fs::read(&request.output).unwrap(), plain);
}

#[test]
fn a_run_refuses_a_report_of_other_inputs_before_it_writes() {
    let fixture = Fixture::new();
    let input = fixture.pack("in", &files());
    let mut request = fixture.request(&input, "out.zip");
    request.expect = Some(reviewed(&request));
    std::fs::write(input.join(README), "changed").unwrap();

    let error = optimise(&request).unwrap_err();

    assert!(error.is_refusal(), "exit 2: {error}");
    let message = error.to_string();
    assert!(
        message.contains("reviewed/report.json does not match this run"),
        "{message}"
    );
    assert!(message.contains("pack differs"), "{message}");
    assert_nothing_written(&fixture, &request.output);
}

/// The plan compared is the plan written: a pack replaced on disk after the comparison does not reach the zip.
#[test]
fn a_pack_replaced_after_the_comparison_does_not_reach_the_zip() {
    static PACK: std::sync::Mutex<Option<Utf8PathBuf>> = std::sync::Mutex::new(None);
    fn replace_then_prune(
        cx: &Context<'_>,
        state: Analysed,
    ) -> Result<(Analysed, BTreeSet<PhysPath>), packprune_pass::RunError> {
        let pack = PACK.lock().unwrap().clone().unwrap();
        std::fs::write(
            pack.join("pack.mcmeta"),
            r#"{"pack": {"min_format": 88, "max_format": 88, "description": "UNREVIEWED"}}"#,
        )
        .unwrap();
        cx.prune(state)
    }
    let fixture = Fixture::new();
    let input = fixture.pack("in", &files());
    *PACK.lock().unwrap() = Some(input.clone());
    let mut request = fixture.request(&input, "out.zip");
    request.profile = none();
    request.expect = Some(reviewed(&request));
    let seams = Seams {
        registry: &Registry::default(),
        prune: replace_then_prune,
        reprune: |cx, state| cx.prune(state),
        quirks: Quirks::default(),
        reports: None,
    };

    execute_staged(&request, &seams, &Progress::default()).unwrap();

    assert_eq!(entries(&request.output)["pack.mcmeta"], META.as_bytes());
}

pub(super) fn assert_nothing_written(fixture: &Fixture, output: &Utf8Path) {
    assert!(!output.exists());
    let left: Vec<String> = std::fs::read_dir(fixture.dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.starts_with(".packprune-"))
        .collect();
    assert!(left.is_empty(), "staging left behind: {left:?}");
}

/// Determinism on one pack: one thread and eight, each cold on its own cache then warm, give the same bytes, and a
/// warm run computes nothing the cache holds.
#[test]
fn cold_and_warm_runs_on_any_thread_count_write_the_same_bytes() {
    let fixture = Fixture::new();
    let input = fixture.pack("in", &files());
    let mut sha1s = BTreeSet::new();
    for threads in [1, 8] {
        let mut request = fixture.request(&input, &format!("out-{threads}.zip"));
        request.profile.threads = Some(threads);
        request.cache = Some(CacheAt {
            dir: path(&fixture.dir, &format!("cache-{threads}")),
            max_bytes: DEFAULT_CACHE_BYTES,
        });
        let cold = optimise(&request).unwrap();
        let warm = optimise(&request).unwrap();
        assert!(cold.cache_misses > 0);
        assert_eq!(warm.cache_misses, 0, "warm run on threads {threads}");
        assert_eq!(warm.cache_hits, cold.cache_hits + cold.cache_misses);
        sha1s.insert(cold.written.sha1);
        sha1s.insert(warm.written.sha1);
    }
    assert_eq!(sha1s.len(), 1);
}

/// A second run reuses every oracle digest the first recorded, and reaches the same verdicts and bytes; a run without
/// a cache records nothing and reaches them too.
#[test]
fn a_warm_run_reuses_the_oracle_memo() {
    let fixture = Fixture::new();
    let mut request = fixture.request(&fixture.pack("in", &files()), "out.zip");
    request.cache = Some(CacheAt {
        dir: path(&fixture.dir, "cache"),
        max_bytes: DEFAULT_CACHE_BYTES,
    });
    let verdicts = |summary: &Summary| {
        [&summary.proof, &summary.on_zip].map(|verdict| {
            verdict
                .as_ref()
                .map(|v| (v.views, v.keys, v.differing_keys, v.differences.clone()))
        })
    };

    let cold = optimise(&request).unwrap();
    let warm = optimise(&request).unwrap();

    assert!(cold.proof.is_some(), "the passes changed the pack");
    assert!(cold.oracle_memo.1 > 0);
    assert_eq!(
        warm.oracle_memo,
        (cold.oracle_memo.0 + cold.oracle_memo.1, 0)
    );
    assert_eq!(verdicts(&warm), verdicts(&cold));
    assert_eq!(warm.written.sha1, cold.written.sha1);

    request.cache = None;
    let uncached = optimise(&request).unwrap();
    assert_eq!(uncached.oracle_memo, (0, 0), "no memo without a cache");
    assert_eq!(verdicts(&uncached), verdicts(&cold));
    assert_eq!(uncached.written.sha1, cold.written.sha1);
}

/// A warm run reads every optimiser output back: no optimiser runs.
#[test]
fn a_warm_run_calls_no_optimiser() {
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    struct Count;
    impl FileOptimizer for Count {
        fn name(&self) -> &'static str {
            "json"
        }
        fn loss(&self) -> Loss {
            Loss::Lossless
        }
        fn applies(&self, _: FileKind, _: &PhysPath) -> bool {
            true
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
            CALLS.fetch_add(1, Ordering::Relaxed);
            Ok(None)
        }
    }
    let fixture = Fixture::new();
    let mut request = fixture.request(&fixture.pack("in", &files()), "out.zip");
    request.cache = Some(CacheAt {
        dir: path(&fixture.dir, "cache"),
        max_bytes: DEFAULT_CACHE_BYTES,
    });
    let registry = Registry {
        passes: packprune_passes::passes(),
        optimisers: vec![Box::new(Count)],
    };
    let reprune: Reprune = |cx, state| cx.prune(state);

    execute(&request, &registry, reprune).unwrap();
    let cold = CALLS.load(Ordering::Relaxed);
    execute(&request, &registry, reprune).unwrap();

    assert!(cold > 0);
    assert_eq!(CALLS.load(Ordering::Relaxed), cold);
}

/// One edited file misses the cache for its own steps only.
#[test]
fn a_changed_file_misses_only_its_own_entries() {
    let fixture = Fixture::new();
    let mut request = fixture.request(&fixture.pack("in", &files()), "out.zip");
    request.cache = Some(CacheAt {
        dir: path(&fixture.dir, "cache"),
        max_bytes: DEFAULT_CACHE_BYTES,
    });
    optimise(&request).unwrap();
    let mut edited = files();
    edited[1].1 = r#"{ "model": { "type": "minecraft:model", "model": "fx:item/box" }, "x": 1 }"#;
    request.input = fixture.pack("edited", &edited);

    let warm = optimise(&request).unwrap();

    // The item: its JSON optimiser step and its zip entry.
    assert_eq!(warm.cache_misses, 2);
}

/// `custom = { force_include = true }` pins a file: drop-unread keeps what it would drop.
#[test]
fn force_include_keeps_a_file_no_client_reads() {
    let fixture = Fixture::new();
    let mut request = fixture.request(&fixture.pack("in", &files()), "out.zip");
    request.profile.overrides = vec![packprune_config::Override {
        glob: README.into(),
        kinds: [(
            "custom".to_owned(),
            [("force_include".to_owned(), serde_json::Value::Bool(true))].into(),
        )]
        .into(),
    }];

    optimise(&request).unwrap();

    assert!(entries(&request.output).contains_key(README));
}

/// `oldest_format` enables overlay-floor on every path: the overlay no client from that format reads is gone, and
/// the loss is measured, not refused.
#[test]
fn oldest_format_floors_the_overlays_and_measures_the_loss() {
    let fixture = Fixture::new();
    let meta = r#"{"pack": {"min_format": 9, "max_format": 88, "description": "t"}, "overlays": {"entries": [{"formats": [9, 20], "directory": "old"}]}}"#;
    let mut pack = files();
    pack[0].1 = meta;
    pack.push((
        "old/assets/fx/models/item/box.json",
        r#"{ "textures": { "particle": "minecraft:block/stone" } }"#,
    ));
    let mut request = fixture.request(&fixture.pack("in", &pack), "out.zip");
    request.profile.oldest_format = Some(88);

    let summary = optimise(&request).unwrap();

    let floor = summary
        .passes
        .iter()
        .find(|pass| pass.pass == "overlay-floor")
        .unwrap();
    assert_eq!(floor.loss, Loss::Lossy);
    assert!(!floor.changed.is_empty(), "{floor:?}");
    let written = entries(&request.output);
    assert!(!String::from_utf8_lossy(&written["pack.mcmeta"]).contains("\"old\""));
    assert!(summary.loss.is_some());
}

#[test]
fn an_unknown_pass_is_refused_by_name() {
    let fixture = Fixture::new();
    let mut request = fixture.request(&fixture.pack("in", &files()), "out.zip");
    request.profile.passes = Some(PassSelection::Names(vec!["no-such-pass".into()]));

    let error = optimise(&request).unwrap_err();

    assert!(error.is_refusal());
    assert!(error.to_string().contains("no-such-pass"), "{error}");
}

#[test]
fn the_output_is_never_inside_the_input() {
    let fixture = Fixture::new();
    let input = fixture.pack("in", &files());
    let mut request = fixture.request(&input, "unused");
    request.output = input.join("out.zip");

    let error = optimise(&request).unwrap_err();

    assert!(error.to_string().contains("inside the input"), "{error}");
    assert!(!request.output.exists());
}

/// The config's option tables are checked against what the optimisers declare; a lossy value outside a lossy table is
/// not lossless.
#[test]
fn override_options_are_checked_against_the_optimisers() {
    let check = |text: &str| {
        packprune_config::parse_with(text, Utf8Path::new("packprune.toml"), &Registry::default())
            .map(|_| ())
            .map_err(|error| error.to_string())
    };
    let table =
        |body: &str| format!("version = 2\n[[profiles.default.override]]\nglob = \"**\"\n{body}\n");
    check(&table("png = { effort = 15, alpha = \"keep\" }")).unwrap();
    check(&table("custom = { force_include = true }")).unwrap();
    let unknown = check(&table("png = { colours = 4 }")).unwrap_err();
    assert!(unknown.contains("`colours`"), "{unknown}");
    let lossy = check(&table("png = { alpha = \"rewrite\" }")).unwrap_err();
    assert!(lossy.contains("rewrite"), "{lossy}");
    let custom = check(&table("custom = { force_include = 1 }")).unwrap_err();
    assert!(custom.contains("boolean"), "{custom}");
    let kind = check(&table("gif = { minify = true }")).unwrap_err();
    assert!(kind.contains("expected a file kind"), "{kind}");
}

/// A lossy table the pass cannot read is refused when the config is loaded, at the table, naming what is missing:
/// the same reading the pass runs, so it cannot accept what the run then fails on.
#[test]
fn a_lossy_table_its_pass_cannot_read_is_refused_at_load() {
    let error = packprune_config::parse_with(
        "version = 2\n[profiles.default.lossy.overlay-floor]\nglobs = [\"**\"]\n",
        Utf8Path::new("packprune.toml"),
        &Registry::default(),
    )
    .map(drop)
    .expect_err("refused at load")
    .to_string();
    assert!(error.starts_with("packprune.toml:2:"), "{error}");
    assert!(error.contains("oldest_format"), "{error}");
}

/// `force_include` applies to files of no known kind only: a model it matches is
/// pruned when nothing uses it, with its parent, and the run says so: the path, the glob, why it went and how to keep
/// it, in the report and in the warnings. A matched file the run keeps is not mentioned.
#[test]
fn force_include_does_not_pin_a_file_of_a_known_kind() {
    const SERVER: &str = "assets/fx/models/item/server.json";
    const BASE: &str = "assets/fx/models/item/base.json";
    let fixture = Fixture::new();
    let mut pack = files();
    pack.push((SERVER, r#"{"parent": "fx:item/base"}"#));
    pack.push((
        BASE,
        r#"{"textures": {"particle": "minecraft:block/stone"}}"#,
    ));
    let mut request = fixture.request(&fixture.pack("in", &pack), "out.zip");
    request.profile.overrides = vec![force_include(SERVER), force_include(MODEL)];

    let summary = optimise(&request).unwrap();

    let written = entries(&request.output);
    assert!(!written.contains_key(SERVER), "{:?}", written.keys());
    assert!(!written.contains_key(BASE), "{:?}", written.keys());
    assert!(written.contains_key(MODEL), "a referenced model stays");
    let notes: Vec<&String> = summary
        .warnings
        .iter()
        .filter(|warning| warning.contains("force_include"))
        .collect();
    assert_eq!(notes.len(), 1, "{:?}", summary.warnings);
    for expected in [SERVER, "known kind", "usage source", "keep-list"] {
        assert!(notes[0].contains(expected), "{expected}: {}", notes[0]);
    }
    assert!(!notes[0].contains(MODEL), "{}", notes[0]);
    assert!(summary.analysis.report.warnings.contains(notes[0]));
    let report = std::fs::read_to_string(report_dir(&request.output).join("report.json")).unwrap();
    assert!(report.contains(SERVER), "{report}");
}

/// A file an optimiser reads is of a known kind, whatever the layout calls it: its overlay copies merge, whether the
/// optimiser reads it by its suffix (a legacy `.lang`) or by its extension alone (a shader's JSON).
#[test]
fn force_include_leaves_the_files_an_optimiser_reads_to_overlay_merge() {
    const LANG: &str = "assets/fx/lang/en_us.lang";
    const SHADER: &str = "assets/minecraft/shaders/core/rendertype_text.json";
    const OVERLAYS: &str = r#"{"pack": {"min_format": 88, "max_format": 88, "description": "t"}, "overlays": {"entries": [{"directory": "nexo_a", "formats": [34, 41]}, {"directory": "nexo_b", "formats": [42, 45]}]}}"#;
    let fixture = Fixture::new();
    let copies = [
        ("nexo_a/assets/fx/lang/en_us.lang", "key=X\n"),
        (
            "nexo_a/assets/minecraft/shaders/core/rendertype_text.json",
            r#"{"vertex": "rendertype_text"}"#,
        ),
        ("nexo_b/assets/fx/lang/en_us.lang", "key=X\n"),
        (
            "nexo_b/assets/minecraft/shaders/core/rendertype_text.json",
            r#"{"vertex": "rendertype_text"}"#,
        ),
    ];
    let mut pack = files();
    pack[0].1 = OVERLAYS;
    pack.extend(copies);
    let mut request = fixture.request(&fixture.pack("in", &pack), "out.zip");
    request.profile.overrides = vec![force_include("*_*/**/*?.*")];

    optimise(&request).unwrap();

    let written: Vec<String> = entries(&request.output).into_keys().collect();
    for file in [LANG, SHADER] {
        let found: Vec<&String> = written.iter().filter(|path| path.ends_with(file)).collect();
        assert_eq!(found.len(), 1, "{file}: {found:?}");
        assert!(found[0].starts_with("packprune_34_45/"), "{found:?}");
    }
}

fn force_include(glob: &str) -> packprune_config::Override {
    packprune_config::Override {
        glob: glob.into(),
        kinds: [(
            "custom".to_owned(),
            [("force_include".to_owned(), serde_json::Value::Bool(true))].into(),
        )]
        .into(),
    }
}

/// Only the files `force_include` matches survive `drop-unread`, byte for byte, among those of no known kind.
#[test]
fn force_include_keeps_the_unknown_files_it_matches_and_drops_the_others() {
    const KEPT: &str = "assets/fx/models/item/kept.bbmodel";
    const DROPPED: &str = "assets/fx/models/item/dropped.bbmodel";
    const BYTES: &str = "{\"editor\":   \"project\"}\n";
    let fixture = Fixture::new();
    let mut pack = files();
    pack.push((KEPT, BYTES));
    pack.push((DROPPED, BYTES));
    let mut request = fixture.request(&fixture.pack("in", &pack), "out.zip");
    request.profile.overrides = vec![force_include(KEPT)];

    optimise(&request).unwrap();

    let written = entries(&request.output);
    assert_eq!(written[KEPT], BYTES.as_bytes());
    assert!(!written.contains_key(DROPPED), "{:?}", written.keys());
}

/// Overlay copies a `force_include` glob such as `*_*/**/*?.*` covers are of a known kind: overlay-merge merges them.
#[test]
fn force_include_leaves_the_overlay_copies_of_a_known_kind_to_overlay_merge() {
    const LANG: &str = "assets/fx/lang/en_us.json";
    const OVERLAYS: &str = r#"{"pack": {"min_format": 88, "max_format": 88, "description": "t"}, "overlays": {"entries": [{"directory": "nexo_a", "formats": [34, 41]}, {"directory": "nexo_b", "formats": [42, 45]}]}}"#;
    let fixture = Fixture::new();
    let mut pack = files();
    pack[0].1 = OVERLAYS;
    pack.push(("nexo_a/assets/fx/lang/en_us.json", r#"{"key": "X"}"#));
    pack.push(("nexo_b/assets/fx/lang/en_us.json", r#"{"key": "X"}"#));
    let mut request = fixture.request(&fixture.pack("in", &pack), "out.zip");
    request.profile.overrides = vec![force_include("*_*/**/*?.*")];

    optimise(&request).unwrap();

    let written: Vec<_> = entries(&request.output)
        .into_keys()
        .filter(|path| path.ends_with(LANG))
        .collect();
    assert_eq!(written, [format!("packprune_34_45/{LANG}")]);
}

/// A warm vanilla index is read under the limits that built it: a jar the limits refuse cold is refused warm.
#[test]
fn a_warm_client_index_does_not_bypass_the_read_limits() {
    let fixture = Fixture::new();
    let pack = fixture.pack(
        "in",
        &[
            ("pack.mcmeta", META),
            (ITEM, files()[1].1),
            (MODEL, files()[2].1),
        ],
    );
    let mut request = fixture.request(&pack, "out.zip");
    request.profile = none();
    request.cache = Some(CacheAt {
        dir: path(&fixture.dir, "cache"),
        max_bytes: DEFAULT_CACHE_BYTES,
    });
    optimise(&request).unwrap();
    request.limits.max_entries = Some(3);
    request.output = path(&fixture.dir, "strict.zip");

    let error = optimise(&request).unwrap_err();

    assert!(error.is_refusal(), "{error}");
    assert!(error.to_string().contains("client.jar"), "{error}");
    assert!(!request.output.exists());
}

/// Properties files are minified only with OptiFine's support kept: without it the optimiser does not run.
#[test]
fn properties_are_left_alone_without_optifine() {
    const PROPERTIES: &str = "assets/fx/optifine/example.properties";
    const TEXT: &str = "# keep this comment\nkey = value\n";
    let fixture = Fixture::new();
    let mut pack = files();
    pack.push((PROPERTIES, TEXT));
    let input = fixture.pack("in", &pack);
    let request = fixture.request(&input, "plain.zip");
    let mut optifine = fixture.request(&input, "optifine.zip");
    optifine.profile.keep_mod_files = vec!["optifine".into()];

    optimise(&request).unwrap();
    optimise(&optifine).unwrap();

    assert_eq!(entries(&request.output)[PROPERTIES], TEXT.as_bytes());
    assert!(
        entries(&optifine.output)[PROPERTIES].len() < TEXT.len(),
        "control"
    );
}

/// An entry written by another build is never read: the build is part of every key.
#[test]
fn a_cache_entry_of_another_build_is_a_miss() {
    let dir = tempfile::tempdir().unwrap();
    let store = || {
        Some(packprune_cache::Store {
            dir: Utf8PathBuf::from_path_buf(dir.path().to_owned()).unwrap(),
            max_bytes: DEFAULT_CACHE_BYTES,
        })
    };
    let a = cache::Cache::with_build(store(), "a".into());
    a.put("step", b"options", &[[1; 32]], b"a's output");
    let b = cache::Cache::with_build(store(), "b".into());

    assert_eq!(
        b.get("step", b"options", &[[1; 32]], |v| Some(v.to_vec())),
        None
    );
    assert_eq!(
        a.get("step", b"options", &[[1; 32]], |v| Some(v.to_vec())),
        Some(b"a's output".to_vec())
    );
}

const UI_TEXTURE: &str = "assets/ui/textures/0d6a9e2c-1f3b-4c5d-8e7f-a1b2c3d4e5f6.png";
const FX_TEXTURE: &str = "assets/fx/textures/6f5e4d3c-2b1a-4f0e-9d8c-7b6a5f4e3d2c.png";
/// A lossy scope: every PNG under `assets/` but the `assets/ui` tree.
const BUT_UI: &str = "assets/{[!u]*,u,u[!i]*,ui?*}/**/*.png";

/// A 4×4 texture whose transparent texels keep a colour: what the alpha rewrite changes.
fn hidden_rgb() -> packprune_codec::png::Image {
    let rgba = (0..16)
        .flat_map(|i| {
            if i % 2 == 0 {
                [200, 30, 90, 255]
            } else {
                [10 * i as u8, 250, 5, 0]
            }
        })
        .collect();
    packprune_codec::png::Image {
        width: 4,
        height: 4,
        rgba,
    }
}

/// Two fonts drawing UUID-named textures, one under `assets/ui`: short-names renames both into another namespace.
fn font_pack(fixture: &Fixture) -> Utf8PathBuf {
    let font = |texture: &str| {
        let id = texture
            .trim_start_matches("assets/")
            .replacen("/textures/", ":", 1);
        format!(
            r#"{{"providers": [{{"type": "bitmap", "file": "{id}", "ascent": 7, "chars": ["a"]}}]}}"#
        )
    };
    let (ui, fx) = (font(UI_TEXTURE), font(FX_TEXTURE));
    let root = fixture.pack(
        "fonts",
        &[
            ("pack.mcmeta", META),
            ("assets/ui/font/default.json", &ui),
            ("assets/fx/font/default.json", &fx),
        ],
    );
    let png = packprune_codec::png::encode(&hidden_rgb()).unwrap();
    for texture in [UI_TEXTURE, FX_TEXTURE] {
        std::fs::create_dir_all(root.join(texture).parent().unwrap()).unwrap();
        std::fs::write(root.join(texture), &png).unwrap();
    }
    root
}

/// The pixels of the texture `font` draws in the written pack, wherever short-names put it.
fn drawn_by(written: &BTreeMap<String, Vec<u8>>, font: &str) -> packprune_codec::png::Image {
    let json = String::from_utf8_lossy(&written[font]).into_owned();
    let file = json
        .split("\"file\":")
        .nth(1)
        .and_then(|rest| rest.split('"').nth(1))
        .unwrap_or_else(|| panic!("{json}"));
    let (namespace, path) = file.split_once(':').unwrap_or(("minecraft", file));
    let texture = format!("assets/{namespace}/textures/{path}");
    packprune_codec::png::decode(&written[&texture]).unwrap()
}

/// A texture a lossy scope leaves out by path keeps its pixels after short-names renamed it, while the
/// same texture outside the excluded tree is rewritten.
#[test]
fn a_lossy_scope_matches_the_input_path_of_a_renamed_file() {
    let fixture = Fixture::new();
    let mut request = fixture.request(&font_pack(&fixture), "out.zip");
    request.profile.lossy = [(
        "png-alpha".to_owned(),
        packprune_config::Lossy {
            globs: vec![BUT_UI.into()],
            options: Default::default(),
        },
    )]
    .into();

    let summary = optimise(&request).unwrap();

    let short = summary
        .passes
        .iter()
        .find(|pass| pass.pass == "short-names")
        .unwrap();
    assert!(
        short.changed.iter().any(|path| path.as_str() == UI_TEXTURE),
        "short-names renamed the ui texture: {short:?}"
    );
    let written = entries(&request.output);
    assert!(!written.contains_key(UI_TEXTURE), "renamed");
    assert_eq!(
        drawn_by(&written, "assets/ui/font/default.json").rgba,
        hidden_rgb().rgba,
        "excluded by its input path"
    );
    assert_ne!(
        drawn_by(&written, "assets/fx/font/default.json").rgba,
        hidden_rgb().rgba,
        "control: in scope, rewritten"
    );
}

/// A per-file override names the input path: a model short-names renamed keeps the options its old path had.
#[test]
fn an_override_matches_the_input_path_of_a_renamed_file() {
    const MODEL: &str = "assets/ui/models/0d6a9e2c-1f3b-4c5d-8e7f-a1b2c3d4e5f6.json";
    const TEXT: &str = "{ \"textures\": { \"particle\": \"block/stone\" } }";
    let fixture = Fixture::new();
    let input = fixture.pack(
        "in",
        &[
            ("pack.mcmeta", META),
            (
                ITEM,
                r#"{"model": {"type": "minecraft:model", "model": "ui:0d6a9e2c-1f3b-4c5d-8e7f-a1b2c3d4e5f6"}}"#,
            ),
            (MODEL, TEXT),
        ],
    );
    let mut request = fixture.request(&input, "out.zip");
    request.profile.overrides = vec![packprune_config::Override {
        glob: "assets/ui/**".into(),
        kinds: [(
            "json".to_owned(),
            [("minify".to_owned(), serde_json::Value::Bool(false))].into(),
        )]
        .into(),
    }];

    optimise(&request).unwrap();

    let written = entries(&request.output);
    assert!(
        !written.contains_key(MODEL),
        "renamed: {:?}",
        written.keys()
    );
    let model = written
        .iter()
        .find(|(path, _)| path.contains("/models/"))
        .map(|(_, bytes)| bytes)
        .unwrap();
    assert_eq!(model, TEXT.as_bytes(), "minify = false kept");
}

/// A file several input files became is in a scope only if each of them is; an added file is known by its path.
#[test]
fn origins_of_a_merged_file_must_all_match() {
    let phys = |path: &str| PhysPath::new(path).unwrap();
    let logical = |path: &str| packprune_pack::LogicalPath::new(path).unwrap();
    let start = [
        phys("assets/ui/textures/a.png"),
        phys("assets/fx/textures/b.png"),
    ];
    let merge = packprune_oracle::Remap::step(
        [(
            logical("assets/ui/textures/a.png"),
            logical("assets/fx/textures/b.png"),
        )]
        .into(),
        Default::default(),
        Default::default(),
    );
    let origins = origins::Origins::new(&start, &merge);
    let but_ui = |path: &str| !path.starts_with("assets/ui/");

    assert!(!origins.all(&phys("assets/fx/textures/b.png"), but_ui));
    assert!(origins.all(&phys("assets/fx/textures/new.png"), but_ui));
    assert!(!origins.all(&phys("assets/ui/textures/new.png"), but_ui));
}

/// identical-models merges a model whose override says `minify = false` with one that has none: the file is both,
/// no single table is the server's, so the optimiser leaves it as it is.
#[test]
fn a_merged_file_whose_overrides_disagree_is_left_alone() {
    const TEXT: &str = "{ \"textures\": { \"particle\": \"block/stone\" } }";
    let item =
        |model: &str| format!(r#"{{"model": {{"type": "minecraft:model", "model": "{model}"}}}}"#);
    let (ui, fx) = (item("ui:item/box"), item("fx:item/box"));
    let fixture = Fixture::new();
    let input = fixture.pack(
        "in",
        &[
            ("pack.mcmeta", META),
            ("assets/ui/items/box.json", &ui),
            ("assets/fx/items/box.json", &fx),
            ("assets/ui/models/item/box.json", TEXT),
            ("assets/fx/models/item/box.json", TEXT),
        ],
    );
    let mut request = fixture.request(&input, "out.zip");
    request.profile.passes = Some(PassSelection::Names(vec![
        "identical-models".into(),
        "json".into(),
    ]));
    request.profile.overrides = vec![packprune_config::Override {
        glob: "assets/ui/**".into(),
        kinds: [(
            "json".to_owned(),
            [("minify".to_owned(), serde_json::Value::Bool(false))].into(),
        )]
        .into(),
    }];

    let summary = optimise(&request).unwrap();

    let merged = summary
        .passes
        .iter()
        .find(|pass| pass.pass == "identical-models")
        .unwrap();
    assert_eq!(merged.files_after, merged.files_before - 1, "{merged:?}");
    let written = entries(&request.output);
    let models: Vec<&Vec<u8>> = written
        .iter()
        .filter(|(path, _)| path.contains("/models/"))
        .map(|(_, bytes)| bytes)
        .collect();
    assert_eq!(models, [&TEXT.as_bytes().to_vec()]);
}

/// A lossy graph pass's scope is checked against the input path of a file the lossless passes renamed.
#[test]
fn a_lossy_pass_scope_follows_the_lossless_renames() {
    let phys = |path: &str| PhysPath::new(path).unwrap();
    let logical = |path: &str| packprune_pack::LogicalPath::new(path).unwrap();
    let registry = Registry::default();
    let mut profile = none();
    profile.lossy = [(
        "texel-downscale".to_owned(),
        packprune_config::Lossy {
            globs: vec![BUT_UI.into()],
            options: Default::default(),
        },
    )]
    .into();
    let selection = registry.select(&profile).unwrap();
    let start = [phys(UI_TEXTURE), phys(FX_TEXTURE)];
    let short_names = packprune_oracle::Remap::step(
        [
            (logical(UI_TEXTURE), logical("assets/z/textures/0.png")),
            (logical(FX_TEXTURE), logical("assets/z/textures/1.png")),
        ]
        .into(),
        Default::default(),
        Default::default(),
    );
    let origins = std::sync::Arc::new(origins::Origins::new(&start, &short_names));

    let passes = selection.lossy_passes(&origins);

    let [texel] = passes.as_slice() else {
        panic!("one lossy pass")
    };
    assert!(
        !texel.scope.contains(&phys("assets/z/textures/0.png")),
        "was ui"
    );
    assert!(
        texel.scope.contains(&phys("assets/z/textures/1.png")),
        "was fx"
    );
}

/// A PackSquash settings file's per-glob rules as file optimiser overrides, without `delete_bloat_keys` and
/// `maximum_width_and_height`, which no file optimiser takes.
fn packsquash_overrides(settings: &str) -> Vec<packprune_config::Override> {
    let text = format!("pack_directory = 'pack'\n{settings}");
    let mut overrides =
        packprune_config::packsquash::parse(&text, Utf8Path::new("packsquash.toml"), &[])
            .unwrap()
            .profile
            .overrides;
    for rule in &mut overrides {
        rule.kinds.retain(|kind, options| {
            options.remove("delete_bloat_keys");
            options.remove("maximum_width_and_height");
            kind != "custom"
        });
    }
    overrides
}

/// A font sheet renamed out of `assets/ui` by short-names keeps the PackSquash rule written on its input path, so
/// its RGB under alpha 0, which a text shader may read, is not rewritten.
#[test]
fn a_ui_font_sheet_keeps_rgb_under_alpha_0_after_short_names() {
    let fixture = Fixture::new();
    let mut request = fixture.request(&font_pack(&fixture), "out.zip");
    request.profile.overrides =
        packsquash_overrides("['assets/ui/**/*.png']\nskip_alpha_optimizations = true\n");

    optimise(&request).unwrap();

    let written = entries(&request.output);
    assert!(!written.contains_key(UI_TEXTURE), "renamed");
    assert_eq!(
        drawn_by(&written, "assets/ui/font/default.json").rgba,
        hidden_rgb().rgba
    );
}

/// A lossless file optimiser that rewrites RGB under alpha 0 changes what a shader reads; the oracle on the zip
/// as written sees it and nothing is published.
#[test]
fn a_lossless_optimiser_rewriting_rgb_under_alpha_0_is_refused() {
    struct AlphaAsLossless;
    impl FileOptimizer for AlphaAsLossless {
        fn name(&self) -> &'static str {
            "png"
        }
        fn loss(&self) -> Loss {
            Loss::Lossless
        }
        fn applies(&self, _: FileKind, path: &PhysPath) -> bool {
            path.as_str().ends_with(".png")
        }
        fn options_schema(&self) -> schemars::Schema {
            schemars::json_schema!({"type": "object"})
        }
        fn optimise(
            &self,
            input: &[u8],
            _: &FileCx,
            _: &toml::Table,
        ) -> Result<Option<Optimised>, PassError> {
            let image = png::decode(input).unwrap();
            let cleared = png::clear_transparent(&image).unwrap_or(image);
            let bytes =
                png::optimise(&png::encode(&cleared).unwrap(), &Default::default()).unwrap();
            Ok(Some(Optimised { bytes, loss: None }))
        }
    }
    let fixture = Fixture::new();
    let request = fixture.request(&font_pack(&fixture), "out.zip");
    let registry = Registry {
        passes: packprune_passes::passes(),
        optimisers: vec![Box::new(AlphaAsLossless)],
    };

    let error = execute(&request, &registry, |cx, state| cx.prune(state)).unwrap_err();

    let RunError::NotEquivalent { passes, verdict } = &error else {
        panic!("expected the oracle's refusal, got {error}");
    };
    assert_eq!(passes, &["png"]);
    assert_eq!(verdict.differing_keys, 2, "{:?}", verdict.differences);
    assert!(!error.is_refusal(), "exit 3");
    assert_nothing_written(&fixture, &request.output);
}

/// The same rewrite by the lossy alpha optimiser, opted in: the run publishes and reports it as loss.
#[test]
fn the_opted_in_alpha_rewrite_is_measured_on_the_written_zip() {
    let fixture = Fixture::new();
    let mut request = fixture.request(&font_pack(&fixture), "out.zip");
    request.profile.lossy = [(
        "png-alpha".to_owned(),
        packprune_config::Lossy {
            globs: vec!["**/*.png".into()],
            options: Default::default(),
        },
    )]
    .into();

    let summary = optimise(&request).unwrap();

    let loss = summary.zip_loss.expect("the lossy optimiser changed bytes");
    assert_eq!(loss.differing_keys, 2, "{:?}", loss.differences);
    assert!(request.output.exists());
}

/// In mixed mode, an opted-in lossy rewrite of one file does not cover a lossless optimiser's error on another.
#[test]
fn a_lossless_error_is_refused_beside_an_opted_in_lossy_rewrite() {
    struct WrongOnUi;
    impl FileOptimizer for WrongOnUi {
        fn name(&self) -> &'static str {
            "png"
        }
        fn loss(&self) -> Loss {
            Loss::Lossless
        }
        fn applies(&self, _: FileKind, path: &PhysPath) -> bool {
            path.as_str() == UI_TEXTURE
        }
        fn options_schema(&self) -> schemars::Schema {
            schemars::json_schema!({"type": "object"})
        }
        fn optimise(
            &self,
            input: &[u8],
            _: &FileCx,
            _: &toml::Table,
        ) -> Result<Option<Optimised>, PassError> {
            let image = png::decode(input).unwrap();
            let cleared = png::clear_transparent(&image).unwrap_or(image);
            let bytes =
                png::optimise(&png::encode(&cleared).unwrap(), &Default::default()).unwrap();
            Ok(Some(Optimised { bytes, loss: None }))
        }
    }
    let fixture = Fixture::new();
    let mut request = fixture.request(&font_pack(&fixture), "out.zip");
    request.profile.passes = Some(PassSelection::Names(vec!["png".into()]));
    request.profile.lossy = [(
        "png-alpha".to_owned(),
        packprune_config::Lossy {
            globs: vec!["assets/fx/**/*.png".into()],
            options: Default::default(),
        },
    )]
    .into();
    let mut registry = Registry {
        passes: Vec::new(),
        optimisers: packprune_passes::optimisers(),
    };
    registry.optimisers.retain(|o| o.name() == "png-alpha");
    registry.optimisers.insert(0, Box::new(WrongOnUi));

    let error = execute(&request, &registry, |cx, state| cx.prune(state)).unwrap_err();

    let RunError::NotEquivalent { passes, verdict } = &error else {
        panic!("expected the oracle's refusal, got {error}");
    };
    assert_eq!(passes, &["png"]);
    assert_eq!(verdict.differing_keys, 1, "{:?}", verdict.differences);
    assert!(
        verdict.differences[0].contains("ui:default"),
        "{:?}",
        verdict.differences
    );
    assert_nothing_written(&fixture, &request.output);
}

/// The client skips an empty import without numbering it: a comment-only import is written as it is, and the
/// others keep every line break.
#[test]
fn a_comment_only_import_is_written_as_it_is() {
    const SHADER: &str = "assets/minecraft/shaders/core/position_color.fsh";
    const EMPTY: &str = "assets/minecraft/shaders/include/empty.glsl";
    const SOURCE: &str = "assets/minecraft/shaders/include/source.glsl";
    const COMMENT: &str = "/* intentionally empty\n   multiline include\n*/\n";
    let fixture = Fixture::new();
    let input = fixture.pack(
        "in",
        &[
            ("pack.mcmeta", META),
            (
                SHADER,
                "#version 330 core\n#moj_import <empty.glsl>\n#moj_import <source.glsl>\nout vec4 fragColor;\nvoid main() {\n    fragColor = vec4(float(IMPORT_ID) / 255.0);\n}\n",
            ),
            (EMPTY, COMMENT),
            (
                SOURCE,
                "const int IMPORT_ID = __FILE__;   \nconst  int  OTHER = 1;   \n",
            ),
        ],
    );
    let request = fixture.request(&input, "out.zip");

    let summary = optimise(&request).unwrap();

    let written = entries(&request.output);
    assert_eq!(written[EMPTY], COMMENT.as_bytes());
    assert_eq!(
        written[SOURCE],
        b"const int IMPORT_ID = __FILE__;   \nconst int OTHER=1;\n"
    );
    assert_eq!(
        written[SHADER],
        b"#version 330 core\n#moj_import <empty.glsl>\n#moj_import <source.glsl>\nout vec4 fragColor;\nvoid main(){\nfragColor=vec4(float(IMPORT_ID)/255.0);\n}\n"
    );
    assert_eq!(summary.on_zip.expect("bytes changed").differing_keys, 0);
}

/// A file the client has at a path no folder rule names (`Other`) compares as JSON: the minifier's whitespace is free.
#[test]
fn a_minified_vanilla_json_of_kind_other_is_the_same() {
    const WARNLIST: &str = "assets/minecraft/gpu_warnlist.json";
    // 26.2's own file, indented as the client jar has it.
    const TEXT: &str = "{\n  \"renderer\": [],\n  \"version\": [\n    \"\\\\bMetal\\\\b\"\n  ],\n  \"vendor\": []\n}\n";
    let fixture = Fixture::with_client(&[(WARNLIST, TEXT)]);
    let input = fixture.pack("in", &[("pack.mcmeta", META), (WARNLIST, TEXT)]);
    let request = fixture.request(&input, "out.zip");

    let summary = optimise(&request).unwrap();

    let written = entries(&request.output);
    assert!(written[WARNLIST].len() < TEXT.len(), "minified");
    assert_eq!(summary.on_zip.expect("bytes changed").differing_keys, 0);
}

/// A PackSquash per-glob setting names the input path, like the profile's own overrides.
#[test]
fn a_packsquash_glob_setting_matches_the_input_path_of_a_renamed_file() {
    const MODEL: &str = "assets/ui/models/0d6a9e2c-1f3b-4c5d-8e7f-a1b2c3d4e5f6.json";
    const TEXT: &str = "{ \"textures\": { \"particle\": \"block/stone\" } }";
    let fixture = Fixture::new();
    let input = fixture.pack(
        "in",
        &[
            ("pack.mcmeta", META),
            (
                ITEM,
                r#"{"model": {"type": "minecraft:model", "model": "ui:0d6a9e2c-1f3b-4c5d-8e7f-a1b2c3d4e5f6"}}"#,
            ),
            (MODEL, TEXT),
        ],
    );
    let mut request = fixture.request(&input, "out.zip");
    request.profile.overrides = packsquash_overrides("['assets/ui/**']\nminify_json = false\n");

    optimise(&request).unwrap();

    let written = entries(&request.output);
    assert!(!written.contains_key(MODEL), "renamed");
    let models: Vec<&Vec<u8>> = written
        .iter()
        .filter(|(path, _)| path.contains("/models/"))
        .map(|(_, bytes)| bytes)
        .collect();
    assert_eq!(models, [&TEXT.as_bytes().to_vec()]);
}

/// One second of 16-bit stereo PCM at 48 kHz, a different tone per channel.
fn stereo_wav() -> Vec<u8> {
    let frames = 48_000u32;
    let mut bytes = b"RIFF".to_vec();
    bytes.extend((36 + frames * 4).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(2u16.to_le_bytes());
    bytes.extend(48_000u32.to_le_bytes());
    bytes.extend((48_000u32 * 4).to_le_bytes());
    bytes.extend(4u16.to_le_bytes());
    bytes.extend(16u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend((frames * 4).to_le_bytes());
    for i in 0..frames {
        for hz in [440.0, 660.0] {
            let sample = (i as f32 * hz * std::f32::consts::TAU / 48_000.0).sin() * 10_000.0;
            bytes.extend((sample as i16).to_le_bytes());
        }
    }
    bytes
}

/// audio-to-ogg moves a `.wav` to `.ogg` during the lossy stage: a later pass's `**/*.ogg` scope does not name the
/// file, whose input path is the `.wav`.
#[test]
fn a_later_lossy_scope_matches_the_input_path_of_a_converted_sound() {
    let fixture = Fixture::new();
    let input = fixture.pack(
        "wav",
        &[
            ("pack.mcmeta", META),
            (
                "assets/fx/sounds.json",
                r#"{"tone":{"sounds":["fx:tone"]}}"#,
            ),
        ],
    );
    std::fs::create_dir_all(input.join("assets/fx/sounds")).unwrap();
    std::fs::write(input.join("assets/fx/sounds/tone.wav"), stereo_wav()).unwrap();
    let config = "version = 2\n[profiles.default]\npasses = 'none'\n\
        [profiles.default.lossy.audio-to-ogg]\nglobs = ['assets/fx/sounds/**']\n\
        [profiles.default.lossy.sound-mono]\nglobs = ['**/*.ogg']\n";
    let config =
        packprune_config::parse_with(config, Utf8Path::new("wav.toml"), &Registry::default())
            .unwrap();
    let mut request = fixture.request(&input, "wav.zip");
    request.profile = config.profiles["default"].clone();

    optimise(&request).unwrap();

    let written = entries(&request.output);
    let sound = packprune_codec::audio::decode(&written["assets/fx/sounds/tone.ogg"]).unwrap();
    assert_eq!(sound.channels.len(), 2, "sound-mono's scope left it out");
}

/// A texture's `.mcmeta` follows the texture short-names renames: an override on the input path still applies to it.
#[test]
fn an_override_matches_the_input_path_of_metadata_that_followed_its_texture() {
    const METADATA: &str = "{ \"texture\": { \"blur\": false, \"clamp\": false } }\n";
    let fixture = Fixture::new();
    let input = font_pack(&fixture);
    std::fs::write(input.join(format!("{UI_TEXTURE}.mcmeta")), METADATA).unwrap();
    let mut request = fixture.request(&input, "out.zip");
    request.profile.passes = Some(PassSelection::Names(vec![
        "short-names".into(),
        "json".into(),
    ]));
    request.profile.overrides = vec![packprune_config::Override {
        glob: "assets/ui/**".into(),
        kinds: [(
            "json".to_owned(),
            [("minify".to_owned(), serde_json::Value::Bool(false))].into(),
        )]
        .into(),
    }];

    optimise(&request).unwrap();

    let written = entries(&request.output);
    let metadata: Vec<(&String, &Vec<u8>)> = written
        .iter()
        .filter(|(path, _)| path.ends_with(".png.mcmeta"))
        .collect();
    let [(path, bytes)] = metadata.as_slice() else {
        panic!("{metadata:?}")
    };
    assert!(!path.starts_with("assets/ui/"), "moved: {path}");
    assert_eq!(bytes.as_slice(), METADATA.as_bytes(), "minify = false kept");
}
