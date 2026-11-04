use std::fs;

use super::*;
use crate::budgets::Exceeded;
use crate::cost::{AtlasCost, Cost, FontCost};

/// A report of schema 4 with one finding in a subset of the views.
const SAMPLE: &str = r#"{
  "schema": 4,
  "tool": { "name": "packprune", "version": "2.0.0" },
  "inputs": {
    "pack": { "name": "p.zip", "sha256": "aa" },
    "clientJar": { "name": "c.jar", "sha256": "bb" },
    "clientVersion": "26.2",
    "clientRules": "26.2",
    "assetIndex": { "name": "none", "sha256": "" }
  },
  "views": [
    { "id": "base", "format": "88.0", "overlays": [], "verified": true },
    { "id": "o", "format": "89.0", "overlays": ["o"], "verified": false }
  ],
  "summary": {
    "files": 3, "bytes": 30, "reached": 1, "orphans": 1, "orphanBytes": 10,
    "atlasLoadedOrphans": 0, "atlasLoadedOrphanBytes": 0, "unused": 0, "unusedBytes": 0,
    "deadBlockstates": 0, "missing": 1, "notStitched": 0, "otherFormatsMissing": 0,
    "otherFormatsNotStitched": 0, "unknown": 0, "invalid": 0, "unmatchedUsages": 0
  },
  "byKind": [ { "kind": "model", "files": 2, "bytes": 20, "reached": 1, "unreached": 1, "unreachedBytes": 10 } ],
  "orphans": [ { "path": "assets/fx/models/item/orphan.json", "kind": "model", "bytes": 10 } ],
  "atlasLoadedOrphans": [],
  "deadBlockstates": [],
  "removable": ["assets/fx/models/item/orphan.json"],
  "missing": [ { "source": "assets/fx/items/box.json", "target": "fx:item/gone", "reason": "model", "views": ["88.0"] } ],
  "notStitched": [],
  "otherFormats": { "missing": [], "notStitched": [] },
  "unknown": [],
  "invalid": [],
  "sources": [],
  "sourceRoots": [],
  "unusedItems": [],
  "warnings": ["a note"]
}"#;

fn sample() -> Report {
    serde_json::from_str(SAMPLE).unwrap()
}

fn bundle(passes: Option<Passes>, run: Option<RunReport>) -> Bundle {
    Bundle {
        analysis: Analysis {
            report: sample(),
            graph: GraphFile {
                schema: SCHEMA,
                nodes: Vec::new(),
                edges: Vec::new(),
            },
        },
        passes,
        run,
    }
}

fn passes() -> Passes {
    Passes {
        passes: Vec::new(),
        reprune: Vec::new(),
        oracle: None,
        loss: None,
    }
}

#[test]
fn a_finding_in_some_views_names_them_and_one_in_all_does_not() {
    let mut report = sample();
    let markdown = markdown::render(&report);
    assert!(
        markdown.contains("- `fx:item/gone` (model) from `assets/fx/items/box.json` (views 88.0)"),
        "{markdown}"
    );

    report.missing[0].views = ["88.0".to_owned(), "89.0".to_owned()].into();
    let markdown = markdown::render(&report);
    assert!(
        markdown.contains("- `fx:item/gone` (model) from `assets/fx/items/box.json`\n"),
        "{markdown}"
    );
}

#[test]
fn the_views_table_counts_findings_per_format() {
    let markdown = markdown::render(&sample());
    assert!(
        markdown.contains("| 88.0 | yes | none | 1 | 0 |\n| 89.0 | no | o | 0 | 0 |"),
        "{markdown}"
    );
}

#[test]
fn write_stores_the_five_files_and_replaces_those_a_bundle_has_nothing_of() {
    let dir = tempfile::tempdir().unwrap();
    let run = RunReport {
        output: Some(Output {
            files: 2,
            bytes: 20,
            zip_bytes: 7,
            sha1: "00".into(),
        }),
        dropped: vec![Dropped {
            reason: "orphan".into(),
            files: 1,
            bytes: 10,
        }],
        ..RunReport::default()
    };
    write(dir.path(), &bundle(Some(full_run().0), Some(run))).unwrap();
    assert!(read(dir.path(), "passes.json").contains("json-minify"));
    for file in [
        "report.json",
        "report.md",
        "graph.json",
        "passes.json",
        "run.json",
    ] {
        assert!(dir.path().join(file).is_file(), "{file}");
    }

    write(dir.path(), &bundle(None, None)).unwrap();

    assert!(dir.path().join("report.json").is_file());
    assert_eq!(read(dir.path(), "passes.json"), "{\n  \"passes\": [ ]\n}\n");
    assert_eq!(read(dir.path(), "run.json"), "{ }\n");
    let names: Vec<String> = fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(
        names.iter().all(|name| !name.ends_with(".partial")),
        "{names:?}"
    );
}

#[test]
fn report_json_reads_back_as_the_report_that_was_written() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), &bundle(None, None)).unwrap();

    let read: Report =
        serde_json::from_slice(&fs::read(dir.path().join("report.json")).unwrap()).unwrap();

    assert_eq!(read, sample());
    let text = fs::read_to_string(dir.path().join("report.json")).unwrap();
    assert!(text.contains("\"overlays\": [ ]"), "{text}");
    assert!(text.ends_with("}\n"));
}

#[test]
fn render_gives_the_markdown_of_the_report_json() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), &bundle(None, None)).unwrap();

    assert_eq!(render(dir.path()).unwrap(), markdown::render(&sample()));
}

#[test]
fn render_refuses_what_is_not_a_report_of_this_schema() {
    let dir = tempfile::tempdir().unwrap();
    let refuse = |text: &str| {
        fs::write(dir.path().join("report.json"), text).unwrap();
        render(dir.path()).unwrap_err().to_string()
    };

    assert!(refuse(&SAMPLE.replace("\"schema\": 4", "\"schema\": 3")).contains("schema 3"));
    assert!(refuse("{}").contains("no schema field"));
    assert!(refuse("nope").contains("not JSON"));
    assert!(refuse("{\"schema\": 4}").contains("not a packprune report"));
    fs::remove_file(dir.path().join("report.json")).unwrap();
    assert!(
        render(dir.path())
            .unwrap_err()
            .to_string()
            .contains("no such file")
    );
}

#[test]
fn read_report_gives_back_what_write_made() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), &bundle(None, None)).unwrap();

    assert_eq!(
        read_report(&dir.path().join("report.json")).unwrap(),
        sample()
    );
}

#[test]
fn read_report_refuses_what_render_refuses_naming_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("report.json");
    let refuse = |text: &str| {
        fs::write(&file, text).unwrap();
        read_report(&file).unwrap_err().to_string()
    };

    let refused = refuse(&SAMPLE.replace("\"schema\": 4", "\"schema\": 3"));
    assert!(refused.contains("schema 3") && refused.contains("report.json"));
    assert!(refuse("{}").contains("no schema field"));
    assert!(refuse("nope").contains("not JSON"));
    assert!(refuse("{\"schema\": 4}").contains("not a packprune report"));
    fs::remove_file(&file).unwrap();
    assert!(
        read_report(&file)
            .unwrap_err()
            .to_string()
            .contains("no such file")
    );
}

/// Source inputs are `{ name, blake3 }`: a report that hashes them otherwise is refused.
#[test]
fn a_report_whose_source_inputs_have_no_blake3_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("report.json");
    let sources = r#""sources": [ { "id": "keep", "inputs": [ { "name": "keep.yml", "sha256": "aa" } ],
        "usages": 0, "matched": 0, "vanilla": 0, "unmatched": [] } ]"#;
    fs::write(&file, SAMPLE.replace("\"sources\": []", sources)).unwrap();

    let refused = read_report(&file).unwrap_err().to_string();

    assert!(refused.contains("not a packprune report"), "{refused}");
    assert!(refused.contains("blake3"), "{refused}");
    assert!(render(dir.path()).is_err());

    let current = sources.replace("sha256", "blake3");
    fs::write(&file, SAMPLE.replace("\"sources\": []", &current)).unwrap();
    assert_eq!(
        read_report(&file).unwrap().sources[0].inputs[0].blake3,
        "aa"
    );
}

#[test]
fn passes_json_leaves_out_what_did_not_happen() {
    let text = json::to_string(&passes());
    assert_eq!(text, "{\n  \"passes\": [ ]\n}\n");
}

#[test]
fn findings_set_the_exit_class_the_way_1x_does() {
    let mut report = sample();
    assert!(report.has_findings());

    report.summary.missing = 0;
    assert!(report.has_findings(), "an orphan is removable");
    report.summary.orphans = 0;
    assert!(!report.has_findings());
    report.summary.atlas_loaded_orphans = 3;
    report.summary.unknown = 2;
    report.summary.other_formats_missing = 5;
    assert!(
        !report.has_findings(),
        "kept, listed or unverified: not findings"
    );
    report.summary.invalid = 1;
    assert!(report.has_findings());
}

fn cost(gpu: u64, font: u64, exclusions: &[&str]) -> Cost {
    Cost {
        view: "base".into(),
        format: (88, 0),
        atlases: vec![AtlasCost {
            id: "blocks".into(),
            mip_level: 4,
            sprites: 1,
            pack_sprites: 1,
            width: 16,
            height: 16,
            gpu,
            decoded: gpu,
            largest: Vec::new(),
            largest_decoded: Vec::new(),
        }],
        fonts: vec![FontCost {
            font: "default".into(),
            providers: 1,
            glyphs: 1,
            decoded: font,
            staging: 0,
            once_drawn: font,
            pages: 0,
            total: font,
            oversized_glyphs: 0,
            provider_types: Default::default(),
        }],
        duplicates: Vec::new(),
        exclusions: exclusions.iter().map(|text| (*text).into()).collect(),
    }
}

/// A run of every kind of line `report.md` has a section for.
fn full_run() -> (Passes, RunReport) {
    let verdict = |differing_keys| Verdict {
        views: 2,
        keys: 40,
        differing_keys,
        differences: vec!["view base: a/b".into()],
    };
    let passes = Passes {
        passes: vec![PassLine {
            pass: "json-minify".into(),
            lossless: true,
            files_before: 3,
            files_after: 3,
            bytes_before: 30,
            bytes_after: 20,
            changed_files: 2,
            changes: vec!["a.json".into()],
            measured: Vec::new(),
        }],
        reprune: Vec::new(),
        oracle: Some(verdict(0)),
        loss: Some(verdict(1)),
    };
    let run = RunReport {
        output: Some(Output {
            files: 2,
            bytes: 20,
            zip_bytes: 7,
            sha1: "00ff".into(),
        }),
        dropped: vec![Dropped {
            reason: "orphan".into(),
            files: 1,
            bytes: 10,
        }],
        optimisers: vec![OptimiserLine {
            optimiser: "png".into(),
            files: 1,
            bytes_before: 9,
            bytes_after: 5,
        }],
        optimiser_loss: vec![Measure {
            key: "t.png".into(),
            metric: "ssim".into(),
            value: 0.99,
        }],
        on_zip: Some(verdict(0)),
        zip_loss: Some(verdict(2)),
        cost: Some(RunCost {
            input: Some(cost(150 << 20, 3 << 20, &[])),
            output: Some(cost(
                100 << 20,
                2 << 20,
                &["font: a provider", "atlas: palette"],
            )),
        }),
        exceeded: vec![
            (&Exceeded {
                budget: "font_mib",
                limit: 1,
                measured: 2,
            })
                .into(),
        ],
    };
    (passes, run)
}

#[test]
fn render_gives_back_the_markdown_write_made_with_every_section() {
    let dir = tempfile::tempdir().unwrap();
    let (passes, run) = full_run();
    write(dir.path(), &bundle(Some(passes), Some(run))).unwrap();
    let written = fs::read_to_string(dir.path().join("report.md")).unwrap();
    for section in [
        "## Client memory",
        "## Run",
        "### Dropped",
        "### Passes",
        "oracle after the lossless passes",
        "### File optimisers",
        "oracle on the zip as written",
        "### Budgets exceeded",
        "floor: font: a provider; atlas: palette",
    ] {
        assert!(written.contains(section), "{section}\n{written}");
    }

    assert_eq!(render(dir.path()).unwrap(), written);
}

#[test]
fn render_refuses_an_annex_it_cannot_read() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), &bundle(Some(passes()), None)).unwrap();
    fs::write(dir.path().join("passes.json"), "{ \"passes\": 3 }").unwrap();

    let why = render(dir.path()).unwrap_err().to_string();

    assert!(why.contains("passes.json"), "{why}");
}

#[test]
fn staged_files_replace_the_old_ones_only_on_commit() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), &bundle(Some(full_run().0), None)).unwrap();
    let before = fs::read(dir.path().join("report.json")).unwrap();
    let mut changed = bundle(None, None);
    changed.analysis.report.warnings.push("another note".into());

    let staged = stage(dir.path(), &changed).unwrap();

    assert_eq!(fs::read(dir.path().join("report.json")).unwrap(), before);
    assert!(read(dir.path(), "passes.json").contains("json-minify"));
    staged.commit().unwrap();
    assert_ne!(fs::read(dir.path().join("report.json")).unwrap(), before);
    assert_eq!(read(dir.path(), "passes.json"), "{\n  \"passes\": [ ]\n}\n");
}

#[test]
fn staged_files_dropped_without_a_commit_leave_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();

    drop(stage(dir.path(), &bundle(Some(passes()), None)).unwrap());

    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn staging_into_a_file_fails_before_anything_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("out.zip.report");
    fs::write(&blocker, "not a directory").unwrap();

    assert!(stage(&blocker, &bundle(None, None)).is_err());
    assert_eq!(fs::read_to_string(&blocker).unwrap(), "not a directory");
}

#[cfg(unix)]
#[test]
fn a_link_at_a_report_file_name_is_replaced_and_its_target_kept() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let report = dir.path().join("out");
    fs::create_dir(&report).unwrap();
    let mut targets = Vec::new();
    for name in ["report.json", "report.md", "graph.json", "run.json"] {
        let target = outside.path().join(format!("{name}.target"));
        fs::write(&target, "irreplaceable").unwrap();
        std::os::unix::fs::symlink(&target, report.join(name)).unwrap();
        targets.push(target);
    }
    // A link at the name an older design staged under, and a link that leads nowhere.
    let staged_target = outside.path().join("staged.target");
    fs::write(&staged_target, "irreplaceable").unwrap();
    std::os::unix::fs::symlink(&staged_target, report.join(".report.json.partial")).unwrap();
    targets.push(staged_target);
    std::os::unix::fs::symlink(outside.path().join("missing"), report.join("passes.json")).unwrap();
    fs::write(report.join("operator-notes.txt"), "mine").unwrap();

    write(&report, &bundle(Some(passes()), Some(RunReport::default()))).unwrap();

    for target in targets {
        assert_eq!(fs::read_to_string(target).unwrap(), "irreplaceable");
    }
    assert!(!outside.path().join("missing").exists());
    for name in [
        "report.json",
        "report.md",
        "graph.json",
        "run.json",
        "passes.json",
    ] {
        let kind = fs::symlink_metadata(report.join(name)).unwrap().file_type();
        assert!(kind.is_file(), "{name} is a regular file");
    }
    assert_eq!(
        fs::read_to_string(report.join("operator-notes.txt")).unwrap(),
        "mine"
    );
}

#[cfg(unix)]
#[test]
fn replace_swaps_a_link_for_a_file_and_keeps_what_it_led_to() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    fs::write(&target, "irreplaceable").unwrap();
    let path = dir.path().join("report.md");
    std::os::unix::fs::symlink(&target, &path).unwrap();

    replace(&path, "new").unwrap();

    assert_eq!(fs::read_to_string(&target).unwrap(), "irreplaceable");
    assert_eq!(fs::read_to_string(&path).unwrap(), "new");
    assert!(fs::symlink_metadata(&path).unwrap().file_type().is_file());
}

#[test]
fn two_writes_leave_the_same_five_files_with_the_second_ones_content() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), &bundle(Some(full_run().0), None)).unwrap();
    let mut second = bundle(None, None);
    second.analysis.report.warnings.push("second run".into());

    write(dir.path(), &second).unwrap();

    let mut names: Vec<String> = fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "graph.json",
            "passes.json",
            "report.json",
            "report.md",
            "run.json"
        ]
    );
    assert!(!read(dir.path(), "passes.json").contains("json-minify"));
    assert!(report_text(dir.path()).contains("second run"));
}

fn read(dir: &std::path::Path, name: &str) -> String {
    fs::read_to_string(dir.join(name)).unwrap()
}

fn report_text(dir: &std::path::Path) -> String {
    read(dir, "report.json")
}

#[test]
fn a_file_at_the_name_of_an_empty_annex_is_replaced_never_removed() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["passes.json", "run.json"] {
        fs::write(dir.path().join(name), "operator data").unwrap();
    }

    write(dir.path(), &bundle(None, None)).unwrap();

    assert_eq!(read(dir.path(), "passes.json"), "{\n  \"passes\": [ ]\n}\n");
    assert_eq!(read(dir.path(), "run.json"), "{ }\n");
}

/// The report files are only ever replaced by name: no source that writes them can delete a file or a directory.
#[test]
fn no_report_code_removes_a_file() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let crates = manifest.parent().unwrap();
    let mut sources: Vec<std::path::PathBuf> = fs::read_dir(manifest.join("src/reports"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "rs")
                && path.file_name().is_some_and(|name| name != "tests.rs")
        })
        .collect();
    sources.extend(
        [
            "packprune-engine/src/run/report.rs",
            "packprune-engine/src/run/squash.rs",
            "packprune-cli/src/commands/report.rs",
            "packprune-cli/src/commands/check.rs",
        ]
        .map(|file| crates.join(file)),
    );
    // The needles are built here so that this file does not contain them.
    let needles = ["remove_", "unlink", "rmdir", "trash"].map(String::from);
    for source in sources {
        let text = fs::read_to_string(&source).unwrap();
        for needle in &needles {
            assert!(
                !text.contains(needle.as_str()),
                "{} mentions {needle}",
                source.display()
            );
        }
    }
}
