use std::collections::BTreeMap;
use std::fs;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use camino::{Utf8Path, Utf8PathBuf};
use packprune_graph::{SourceError, SubCache, Usage};
use packprune_pack::RunCx;
use serde_json::{Value, json};

use crate::{PROVIDERS, configs, datapacks, keep, nexo};

fn format(usages: &[Usage], cx: &RunCx) -> String {
    usages
        .iter()
        .map(|u| {
            format!(
                "{} | {} | {} | {} | {}{}\n",
                u.kind.id(),
                u.id.0,
                u.atlas
                    .map(|id| id.display(&cx.interner).to_string())
                    .unwrap_or_default(),
                u.location,
                u.reason.as_deref().unwrap_or(""),
                if u.quiet { " | quiet" } else { "" }
            )
        })
        .collect()
}

#[test]
fn config_goldens() {
    goldens("configs", "configs");
}

#[test]
fn datapack_goldens() {
    goldens("datapacks", "datapacks");
}

fn goldens(module: &str, source: &str) {
    let fixtures = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/sources")
        .join(module);
    let mut cases: Vec<_> = fs::read_dir(&fixtures)
        .unwrap()
        .map(|e| Utf8PathBuf::from_path_buf(e.unwrap().path()).unwrap())
        .filter(|p| p.join("input").is_dir())
        .collect();
    cases.sort();
    for case in cases {
        let cx = RunCx::default();
        let provider = PROVIDERS.iter().find(|p| p.id == source).unwrap();
        let actual = provider.open;
        let actual = actual(&case.join("input"), &cx).unwrap().collect().unwrap();
        assert_eq!(
            format(&actual, &cx),
            fs::read_to_string(case.join("expected/usages.txt")).unwrap(),
            "{case}"
        );
    }
}

#[test]
fn keep_contract() {
    let temp = tempfile::tempdir().unwrap();
    let file = Utf8Path::from_path(temp.path()).unwrap().join("keep.yml");
    fs::write(
        &file,
        "version: 1\nkeep:\n  - texture: 'fx:ui/*'\n    why: Loaded by code\n",
    )
    .unwrap();
    let cx = RunCx::default();
    let source = keep::open(&file, &cx).unwrap();
    assert_eq!(
        format(&source.collect().unwrap(), &cx),
        "sprite | fx:ui/* |  | keep.yml:3 | Loaded by code\n"
    );
    fs::write(&file, "version: 1\nkeep:\n - model: fx:thing\n").unwrap();
    assert!(
        source
            .collect()
            .unwrap_err()
            .to_string()
            .contains("every entry needs a why")
    );
}

#[test]
fn nexo_fixture_usages() {
    let root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/sources/nexo/Nexo");
    let cx = RunCx::default();
    let usages = nexo::open(&root, &cx).unwrap().collect().unwrap();
    let expected: Vec<Value> =
        serde_json::from_str(include_str!("nexo/fixture-usages.json")).unwrap();
    assert_eq!(rows(&usages, &cx), expected);
    let actual = format(&usages, &cx);
    assert!(actual.contains(
        "model | fx:item/ruby_sword |  | items/arms.yml:4 | item ruby_sword Pack.model\n"
    ));
    assert!(actual.contains("item_definition | nexo:ruby_sword |  | items/arms.yml:1 | item ruby_sword: the definition Nexo generates for it\n"));
    assert!(actual.contains("sprite | fx:font/heart |  | glyphs/ui.yml:8 | glyph heart texture\n"));
    assert!(
        usages
            .iter()
            .filter(|u| u.location.starts_with("items/arms.yml:"))
            .all(|u| u.declares.is_some())
    );
}

#[test]
fn malformed_runtime_and_properties_are_refused() {
    let temp = tempfile::tempdir().unwrap();
    let root = Utf8Path::from_path(temp.path()).unwrap();
    let cx = RunCx::default();
    for (name, content) in [
        ("data.json", "{}{}\n"),
        ("menu.properties", "font=\\u12XX\n"),
    ] {
        let file = root.join(name);
        fs::write(&file, content).unwrap();
        assert!(matches!(
            configs::open(&file, &cx).unwrap().collect(),
            Err(SourceError::Refused(_, _))
        ));
        fs::remove_file(file).unwrap();
    }
}

#[test]
fn snakeyaml_flow_closer_indentation() {
    let cx = RunCx::default();
    let usages = configs::yaml_usages(
        b"next:\n    item: {item_model: nexo:gem\n }\n    font: fx:hud\n",
        &cx,
    )
    .unwrap();
    assert_eq!(usages.len(), 2);
    assert_eq!(usages[0].location, "input.yml:2 next.item.item_model");
    assert_eq!(usages[1].location, "input.yml:4 next.font");
}

#[test]
fn snakeyaml_quoted_continuation_indentation() {
    let usages = configs::yaml_usages(
        b"next:\n    title: \"hello\n<font:fx:hud>\"\n",
        &RunCx::default(),
    )
    .unwrap();
    assert_eq!(usages.len(), 1);
    assert_eq!(&*usages[0].id.0, "fx:hud");
    assert_eq!(usages[0].location, "input.yml:2 next.title <font:fx:hud>");
}

pub(super) fn rows(usages: &[Usage], cx: &RunCx) -> Vec<Value> {
    usages
        .iter()
        .map(|u| {
            json!([
                u.kind.id(),
                u.id.0,
                u.atlas
                    .map(|id| id.display(&cx.interner).to_string())
                    .unwrap_or_default(),
                u.location,
                u.reason.as_deref().unwrap_or(""),
                u.declares.as_deref().unwrap_or(""),
                u.quiet
            ])
        })
        .collect()
}

#[test]
#[cfg(unix)]
fn configs_follow_file_symlinks_but_do_not_descend_directory_symlinks() {
    let temp = tempfile::tempdir().unwrap();
    let base = Utf8Path::from_path(temp.path()).unwrap();
    let root = base.join("plugins");
    let target = base.join("outside");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&target).unwrap();
    fs::write(target.join("real.yml"), "font: fx:hud\n").unwrap();
    std::os::unix::fs::symlink(target.join("real.yml"), root.join("linked.yml")).unwrap();
    std::os::unix::fs::symlink(&target, root.join("directory")).unwrap();
    let cx = RunCx::default();
    let usages = configs::open(&root, &cx).unwrap().collect().unwrap();
    assert_eq!(
        format(&usages, &cx),
        "font | fx:hud |  | linked.yml:1 font | \n"
    );
    let usages = configs::open(&root.join("linked.yml"), &cx)
        .unwrap()
        .collect()
        .unwrap();
    assert_eq!(
        format(&usages, &cx),
        "font | fx:hud |  | linked.yml:1 font | \n"
    );
}

#[test]
fn config_digest_tracks_selected_files_and_profiles_only() {
    let temp = tempfile::tempdir().unwrap();
    let root = Utf8Path::from_path(temp.path()).unwrap();
    fs::create_dir(root.join("packprune-profiles")).unwrap();
    fs::create_dir(root.join("Cache")).unwrap();
    fs::create_dir(root.join("Other")).unwrap();
    fs::write(
        root.join("packprune-profiles/custom.json"),
        r#"{"version":1,"name":"custom","exclude":["Cache/**"]}"#,
    )
    .unwrap();
    fs::write(root.join("menu.yml"), "font: fx:first\n").unwrap();
    let cx = RunCx::default();
    let source = configs::open(root, &cx).unwrap();
    let before = source.input_digest(root).unwrap();
    fs::write(root.join("Cache/data.json"), "not JSON").unwrap();
    fs::write(root.join("Other/data.db"), "binary").unwrap();
    assert_eq!(before, source.input_digest(root).unwrap());
    fs::write(root.join("menu.yml"), "font: fx:second\n").unwrap();
    let changed = source.input_digest(root).unwrap();
    assert_ne!(before, changed);
    fs::write(
        root.join("packprune-profiles/custom.json"),
        r#"{"version":1,"name":"custom","exclude":["Cache/**"],"why":"changed"}"#,
    )
    .unwrap();
    assert_ne!(changed, source.input_digest(root).unwrap());
    let single = configs::open(&root.join("menu.yml"), &cx).unwrap();
    let before = single.input_digest(&root.join("menu.yml")).unwrap();
    fs::write(
        root.join("packprune-profiles/custom.json"),
        r#"{"version":1,"name":"custom","exclude":["Cache/**"],"why":"again"}"#,
    )
    .unwrap();
    assert_ne!(before, single.input_digest(&root.join("menu.yml")).unwrap());
}

#[test]
fn custom_placeholder_patterns_use_java_find_and_refuse_by_name() {
    let temp = tempfile::tempdir().unwrap();
    let root = Utf8Path::from_path(temp.path()).unwrap();
    fs::create_dir(root.join("packprune-profiles")).unwrap();
    let profile = root.join("packprune-profiles/custom.json");
    fs::write(
        &profile,
        r#"{"version":1,"name":"custom","placeholders":["\\bdeferred\\d{2}\\b"]}"#,
    )
    .unwrap();
    fs::write(root.join("menu.yml"), "font: deferred42\n").unwrap();
    let cx = RunCx::default();
    let source = configs::open(root, &cx).unwrap();
    let usages = source.collect().unwrap();
    assert_eq!(
        format(&usages, &cx),
        "font | deferred42 |  | menu.yml:1 font | unresolvable: placeholder\n"
    );
    for pattern in ["[", "(?=deferred)"] {
        fs::write(
            &profile,
            json!({"version":1,"name":"custom","placeholders":[pattern]}).to_string(),
        )
        .unwrap();
        let error = source.collect().unwrap_err();
        assert!(matches!(error, SourceError::Refused(_, _)));
        assert!(error.to_string().contains(profile.as_str()), "{error}");
        assert!(error.to_string().contains(pattern), "{error}");
    }
}

#[test]
fn yaml_duplicates_preserve_config_tuples_and_nexo_last_non_null() {
    let cx = RunCx::default();
    let usages = configs::yaml_usages(b"font: fx:first\nfont: fx:last\n", &cx).unwrap();
    assert_eq!(
        format(&usages, &cx),
        "font | fx:first |  | input.yml:1 font | \nfont | fx:last |  | input.yml:2 font | \n"
    );
    let temp = tempfile::tempdir().unwrap();
    let root = Utf8Path::from_path(temp.path()).unwrap();
    fs::create_dir(root.join("glyphs")).unwrap();
    fs::write(root.join("glyphs/a.yml"), "a:\n  font: fx:first\nb:\n  font: fx:other\na:\n  font: fx:last\n  font: null\n  texture: fx:first\n  texture: fx:last\na: null\n").unwrap();
    let usages = nexo::open(root, &cx).unwrap().collect().unwrap();
    assert_eq!(
        format(&usages, &cx),
        "font | fx:other |  | glyphs/a.yml:4 | glyph b font\nfont | fx:last |  | glyphs/a.yml:6 | glyph a font\nsprite | fx:last |  | glyphs/a.yml:9 | glyph a texture\n"
    );
}

#[test]
fn source_digests_track_only_selected_inputs() {
    let temp = tempfile::tempdir().unwrap();
    let root = Utf8Path::from_path(temp.path()).unwrap();
    let cx = RunCx::default();
    let keep = root.join("keep.yml");
    fs::write(&keep, "version: 1\nkeep: []\n").unwrap();
    let source = keep::open(&keep, &cx).unwrap();
    let before = source.input_digest(&keep).unwrap();
    fs::write(&keep, "version: 1\nkeep: []\n# changed\n").unwrap();
    assert_ne!(before, source.input_digest(&keep).unwrap());
    assert!(source.input_digest(&root.join("other")).is_err());

    for dir in [
        "glyphs",
        "items",
        "recipes/shaped",
        "datapack/data/fx/painting_variant",
        "datapack/data/fx/unknown",
    ] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    for dir in ["glyphs", "items", "recipes/shaped"] {
        fs::write(root.join(dir).join("a.yml"), "a: {}\n").unwrap();
    }
    let nexo = nexo::open(root, &cx).unwrap();
    for input in nexo.inputs() {
        let before = nexo.input_digest(input).unwrap();
        fs::write(input.join("ignored.txt"), "ignored").unwrap();
        if input.ends_with("recipes") {
            fs::write(input.join("ignored.yml"), "not in a recipe type").unwrap();
        }
        assert_eq!(before, nexo.input_digest(input).unwrap());
        let selected = if input.ends_with("recipes") {
            input.join("shaped/a.yml")
        } else {
            input.join("a.yml")
        };
        fs::write(&selected, "a: {font: fx:hud}\n").unwrap();
        let changed = nexo.input_digest(input).unwrap();
        assert_ne!(before, changed);
        fs::rename(&selected, selected.with_file_name("b.yml")).unwrap();
        assert_ne!(changed, nexo.input_digest(input).unwrap());
    }
    let pack = root.join("datapack");
    let selected = pack.join("data/fx/painting_variant/a.json");
    fs::write(&selected, r#"{"asset_id":"fx:first"}"#).unwrap();
    let source = datapacks::open(&pack, &cx).unwrap();
    let before = source.input_digest(&pack).unwrap();
    fs::write(pack.join("data/fx/unknown/a.json"), "ignored").unwrap();
    fs::write(pack.join("readme.txt"), "ignored").unwrap();
    assert_eq!(before, source.input_digest(&pack).unwrap());
    fs::write(&selected, r#"{"asset_id":"fx:last"}"#).unwrap();
    let changed = source.input_digest(&pack).unwrap();
    assert_ne!(before, changed);
    fs::rename(&selected, selected.with_file_name("b.json")).unwrap();
    assert_ne!(changed, source.input_digest(&pack).unwrap());
}

#[test]
fn yaml_aliases_keep_anchor_provenance_and_bound_expansion() {
    let cx = RunCx::default();
    let usages = configs::yaml_usages(b"first: &font fx:hud\nfont: *font\nnode: &node\n  font: fx:inside\ncopy: *node\nloop: &loop\n  font: fx:loop\n  self: *loop\n", &cx).unwrap();
    assert_eq!(
        format(&usages, &cx),
        "item_definition | fx:hud |  | input.yml:1 first |  | quiet\nfont | fx:hud |  | input.yml:1 font | \nfont | fx:inside |  | input.yml:4 node.font | \nfont | fx:inside |  | input.yml:4 copy.font | \nfont | fx:loop |  | input.yml:7 loop.font | \n"
    );
    let mut bomb = "a: &a [fx:hud]\n".to_owned();
    for i in 1..8 {
        let previous = if i == 1 {
            "a".into()
        } else {
            format!("a{}", i - 1)
        };
        bomb.push_str(&format!(
            "a{i}: &a{i} [{}]\n",
            vec![format!("*{previous}"); 10].join(",")
        ));
    }
    assert!(configs::yaml_usages(bomb.as_bytes(), &cx).is_err());
    let mut deep = "a0: &a0 []\n".to_owned();
    for i in 1..140 {
        deep.push_str(&format!("a{i}: &a{i} [*a{}]\n", i - 1));
    }
    assert!(configs::yaml_usages(deep.as_bytes(), &cx).is_err());
}

type Entries = BTreeMap<(String, Vec<u8>), Vec<u8>>;

/// A memo kept across the runs of one test, counting what it answers.
#[derive(Default)]
struct Memo {
    entries: Mutex<Entries>,
    hits: AtomicUsize,
}

impl SubCache for Memo {
    fn get(&self, step: &str, inputs: &[u8]) -> Option<Vec<u8>> {
        let value = self
            .entries
            .lock()
            .unwrap()
            .get(&(step.to_owned(), inputs.to_vec()))
            .cloned();
        if value.is_some() {
            self.hits.fetch_add(1, Ordering::Relaxed);
        }
        value
    }

    fn put(&self, step: &str, inputs: &[u8], value: &[u8]) {
        self.entries
            .lock()
            .unwrap()
            .insert((step.to_owned(), inputs.to_vec()), value.to_vec());
    }
}

/// One run of the configs source over `root` with `memo`, each with its own interner as runs have: its usages equal
/// a run without memo. Returns how many files the memo answered.
fn memoised_run(root: &Utf8Path, memo: &Memo) -> usize {
    let before = memo.hits.load(Ordering::Relaxed);
    let cx = RunCx::default();
    let memoised = configs::open(root, &cx)
        .unwrap()
        .collect_memoised(memo)
        .unwrap();
    let fresh_cx = RunCx::default();
    let fresh = configs::open(root, &fresh_cx).unwrap().collect().unwrap();
    assert_eq!(format(&memoised, &cx), format(&fresh, &fresh_cx));
    memo.hits.load(Ordering::Relaxed) - before
}

fn two_configs() -> (tempfile::TempDir, Memo) {
    let temp = tempfile::tempdir().unwrap();
    let root = Utf8Path::from_path(temp.path()).unwrap();
    fs::write(root.join("menu.yml"), "font: fx:menu\n").unwrap();
    fs::write(root.join("shop.yml"), "font: fx:shop\n").unwrap();
    let memo = Memo::default();
    assert_eq!(memoised_run(root, &memo), 0);
    assert_eq!(
        memoised_run(root, &memo),
        2,
        "an unchanged file is not read again"
    );
    (temp, memo)
}

#[test]
fn memoised_configs_read_an_edited_file_again() {
    let (temp, memo) = two_configs();
    let root = Utf8Path::from_path(temp.path()).unwrap();
    fs::write(root.join("menu.yml"), "font: fx:edited\n").unwrap();
    assert_eq!(memoised_run(root, &memo), 1);
}

#[test]
fn memoised_configs_read_an_added_file_even_with_the_bytes_of_another() {
    let (temp, memo) = two_configs();
    let root = Utf8Path::from_path(temp.path()).unwrap();
    // Its usages say where they are: a copy of a known file under a new name is a new entry.
    fs::write(root.join("copy.yml"), "font: fx:shop\n").unwrap();
    assert_eq!(memoised_run(root, &memo), 2);
}

#[test]
fn memoised_configs_drop_a_removed_file() {
    let (temp, memo) = two_configs();
    let root = Utf8Path::from_path(temp.path()).unwrap();
    fs::remove_file(root.join("menu.yml")).unwrap();
    assert_eq!(memoised_run(root, &memo), 1);
}

#[test]
fn memoised_configs_read_every_file_again_after_a_profile_edit() {
    let (temp, memo) = two_configs();
    let root = Utf8Path::from_path(temp.path()).unwrap();
    fs::create_dir(root.join("packprune-profiles")).unwrap();
    fs::write(
        root.join("packprune-profiles/custom.json"),
        r#"{"version":1,"name":"custom","placeholders":["fx:shop"]}"#,
    )
    .unwrap();
    assert_eq!(memoised_run(root, &memo), 0);
}

/// A profile saved while a run reads it: what the run memoises is keyed on the rules it applied, so a later run on
/// the saved profile equals a run without memo.
#[cfg(unix)]
#[test]
fn a_profile_saved_during_a_run_leaves_no_stale_entry() {
    use std::process::Command;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::thread;
    use std::time::Duration;

    let temp = tempfile::tempdir().unwrap();
    let root = Utf8Path::from_path(temp.path()).unwrap();
    fs::create_dir(root.join("packprune-profiles")).unwrap();
    fs::write(root.join("menu.yml"), "chosen: fx:x\n").unwrap();
    let rules = |id: &str| {
        json!({"version": 1, "name": "race", "rules": [{"key": "chosen", "kind": "model", "id": id}]})
            .to_string()
    };
    let profile = root.join("packprune-profiles/race.json");
    // A FIFO hands each opening of the profile the next save: the first one the old rules, every later one the new.
    let made = Command::new("mkfifo").arg(&profile).status().unwrap();
    assert!(made.success());
    let stop = Arc::new(AtomicBool::new(false));
    let writer = thread::spawn({
        let (profile, stop) = (profile.clone(), Arc::clone(&stop));
        let (old, new) = (rules("fx:a"), rules("fx:b"));
        move || {
            for save in 0.. {
                fs::write(&profile, if save == 0 { &old } else { &new }).unwrap();
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                // Reopened at once, the FIFO would hand the reader still draining it two saves in one read.
                thread::sleep(Duration::from_millis(100));
            }
        }
    });
    let memo = Memo::default();
    let cx = RunCx::default();
    configs::open(root, &cx)
        .unwrap()
        .collect_memoised(&memo)
        .unwrap();
    stop.store(true, Ordering::SeqCst);
    fs::read(&profile).unwrap();
    writer.join().unwrap();
    fs::remove_file(&profile).unwrap();
    fs::write(&profile, rules("fx:b")).unwrap();
    memoised_run(root, &memo);
}

/// packprune's own reports and cache under a configs root are never read: the usages, the input digest and the memo
/// keys are those of the same root without them, a report too large to parse as a config included.
#[test]
fn packprune_folders_under_a_configs_root_change_nothing() {
    let (with, without) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let with = Utf8Path::from_path(with.path()).unwrap();
    let without = Utf8Path::from_path(without.path()).unwrap();
    for root in [with, without] {
        fs::write(root.join("menu.yml"), "font: fx:menu\n").unwrap();
    }
    fs::create_dir_all(with.join("reports/latest")).unwrap();
    fs::create_dir_all(with.join("cache/v1/ab")).unwrap();
    fs::write(
        with.join("reports/latest/report.json"),
        r#"{"font": "fx:report"}"#,
    )
    .unwrap();
    // Past the 3 145 728 code points a config document may hold.
    let graph = format!(r#"{{"font": "fx:{}"}}"#, "g".repeat(3_200_000));
    fs::write(with.join("reports/latest/graph.json"), graph).unwrap();
    fs::write(with.join("cache/v1/ab/entry.yml"), "font: fx:cache\n").unwrap();
    let run = |root: &Utf8Path, excluded: &[Utf8PathBuf]| {
        let cx = RunCx::default();
        let memo = Memo::default();
        let mut source = configs::open(root, &cx).unwrap();
        source.exclude(excluded);
        let usages = source.collect_memoised(&memo).unwrap();
        let keys: Vec<(String, Vec<u8>)> = memo.entries.lock().unwrap().keys().cloned().collect();
        (
            format(&usages, &cx),
            source.input_digest(root).unwrap(),
            keys,
        )
    };
    assert_eq!(
        run(with, &[with.join("reports"), with.join("cache")]),
        run(without, &[])
    );
}
