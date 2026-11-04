mod atlases;
mod corpus;
mod index;
mod support;
mod token_memo;

use camino::Utf8Path;
use packprune_pack::{ClientRules, RunCx};
use serde_json::Value;

use crate::{
    IdGlob, Inputs, Policy, Reached, SourceUsages, Status, UnusedSelector, Usage, UsageKind,
};
use support::{
    analyse, differences, fixture_vanilla, golden_report, line_source, open, repo, report,
};

/// What the keep-list source reads from the `keep-entry` fixture's `keep.yml`.
fn keep_entry(path: &Utf8Path) -> SourceUsages {
    let usage = |kind, id: &str, line: u32, why: &str| Usage {
        kind,
        id: IdGlob(id.into()),
        atlas: None,
        location: format!("keep.yml:{line}"),
        reason: Some(why.into()),
        declares: None,
        quiet: false,
    };
    SourceUsages {
        source: "keep".into(),
        path: path.to_owned(),
        usages: vec![
            usage(
                UsageKind::Path,
                "assets/fx/textures/misc/plugin_*.png",
                3,
                "sent by plugin code",
            ),
            usage(
                UsageKind::Model,
                "fx:runtime/*",
                5,
                "models a plugin sets at runtime",
            ),
            usage(UsageKind::Sprite, "fx:nothing/*", 7, "stale"),
        ],
    }
}

/// Every fixture's `expect.txt`, and the sections of its golden `report.json` the graph decides.
#[test]
fn fixtures_match_expectations_and_golden_reports() {
    let root = repo().join("tests/fixtures/packs");
    let mut names: Vec<String> = root
        .read_dir_utf8()
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_owned())
        .collect();
    names.sort();
    assert_eq!(names.len(), 25);
    let mut failures = Vec::new();
    for name in &names {
        let fixture = root.join(name);
        let cx = RunCx::default();
        let mut vanilla = fixture_vanilla();
        let index = fixture.join("assets/indexes/test.json");
        if index.exists() {
            vanilla = vanilla.with_asset_index(&index).unwrap();
        }
        let mut sources = Vec::new();
        if fixture.join("keep.yml").exists() {
            sources.push(keep_entry(&fixture.join("keep.yml")));
        }
        for (id, file, declaring) in [
            ("lines", "usages.txt", false),
            ("generator", "generator.txt", true),
        ] {
            if fixture.join(file).exists() {
                sources.push(line_source(&cx, id, &fixture.join(file), declaring));
            }
        }
        let selectors: Vec<UnusedSelector> =
            std::fs::read_to_string(fixture.join("prune-unused.txt"))
                .map(|text| {
                    text.lines()
                        .map(|line| UnusedSelector::parse(line).unwrap())
                        .collect()
                })
                .unwrap_or_default();
        let pack = open(&fixture.join("pack"));
        let run = analyse(&cx, &pack, &vanilla, &sources, &selectors);
        let ours = report(&run);
        for line in std::fs::read_to_string(fixture.join("expect.txt"))
            .unwrap()
            .lines()
        {
            let (what, argument) = line.split_once(' ').unwrap();
            // That note names the sources that read server data, which only the source registry knows.
            if what == "warning" && argument.starts_with("no source reads data packs") {
                continue;
            }
            let count = |section: &Value, field: &str| {
                section
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|entry| entry[field] == argument)
                    .count()
            };
            let ok = match what {
                "missing" => count(&ours["missing"], "target") == 1,
                "other_missing" => count(&ours["otherFormats"]["missing"], "target") == 1,
                "not_stitched" => count(&ours["notStitched"], "sprite") == 1,
                "warning" => run
                    .reached
                    .warnings
                    .iter()
                    .any(|warning| warning.starts_with(argument)),
                "root" => {
                    let (root, path) = argument.split_once(' ').unwrap();
                    let file = run
                        .reached
                        .files
                        .iter()
                        .find(|(file, _)| file.as_str() == path);
                    file.is_some_and(|(_, file)| file.root.as_deref() == Some(root))
                }
                "usage" => {
                    let parts: Vec<&str> = argument.split(' ').collect();
                    let matched = run
                        .reached
                        .source_roots
                        .iter()
                        .flat_map(|root| &root.usages)
                        .any(|usage| usage.source == parts[0] && usage.location == parts[1]);
                    let unmatched = run
                        .reached
                        .sources
                        .iter()
                        .filter(|source| source.id == parts[0])
                        .flat_map(|source| &source.unmatched)
                        .any(|usage| usage.location == parts[1]);
                    let state = if matched {
                        "matched"
                    } else if unmatched {
                        "unmatched"
                    } else {
                        "vanilla"
                    };
                    state == parts[2]
                }
                status => {
                    let file = run
                        .reached
                        .files
                        .iter()
                        .find(|(file, _)| file.as_str() == argument);
                    file.is_some_and(|(_, file)| file.status.id() == status)
                }
            };
            if !ok {
                failures.push(format!("{name}: {line}"));
            }
        }
        let golden: Value =
            serde_json::from_slice(&std::fs::read(fixture.join("expected/report.json")).unwrap())
                .unwrap();
        let mut found = Vec::new();
        differences("", &ours, &golden_report(&golden), &mut found, 20);
        failures.extend(
            found
                .into_iter()
                .map(|difference| format!("{name}: golden {difference}")),
        );
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The usage level drops what only unused declarations reach, and its fixpoint keeps a declaration another kept
/// one names (`claimed`), from usages built here.
#[test]
fn unused_items_are_dropped_with_what_only_they_reach() {
    let fixture = repo().join("tests/fixtures/packs/unused-items");
    let cx = RunCx::default();
    let sources = [
        line_source(&cx, "lines", &fixture.join("usages.txt"), false),
        line_source(&cx, "generator", &fixture.join("generator.txt"), true),
    ];
    let selectors = [UnusedSelector::parse("generator:items/**").unwrap()];
    let run = analyse(
        &cx,
        &open(&fixture.join("pack")),
        &fixture_vanilla(),
        &sources,
        &selectors,
    );
    let status = |path: &str| {
        run.reached
            .files
            .iter()
            .find(|(file, _)| file.as_str() == path)
            .map(|(_, file)| file.status)
    };
    assert_eq!(status("assets/fx/items/dropped.json"), Some(Status::Unused));
    assert_eq!(
        status("assets/fx/textures/item/dropped_icon.png"),
        Some(Status::Unused)
    );
    assert_eq!(
        status("assets/fx/items/claimed.json"),
        Some(Status::Reached)
    );
    assert_eq!(status("assets/fx/items/kept.json"), Some(Status::Reached));
    assert!(
        run.reached
            .removable()
            .iter()
            .any(|path| path.as_str() == "assets/fx/models/item/twin.json")
    );
    // `claimed` is kept by the usage of a declaration the selector does not pick.
    let kept: Vec<(&str, Vec<&str>)> = run.reached.unused_items[0]
        .kept
        .iter()
        .map(|item| {
            let by = item
                .used_by
                .iter()
                .map(|usage| usage.location.as_str())
                .collect();
            (item.declaration.as_str(), by)
        })
        .collect();
    assert_eq!(
        kept,
        [
            ("generator:items/main.yml:1", vec!["usages.txt:1"]),
            ("generator:items/main.yml:12", vec!["generator.txt:10"])
        ]
    );

    // Without a use, nothing could keep an item: refused.
    let only_generator = [line_source(
        &cx,
        "generator",
        &fixture.join("generator.txt"),
        true,
    )];
    let layout = &run.layout;
    let rules = ClientRules::for_version("26.2");
    let refused = Reached::analyse(&Inputs {
        cx: &cx,
        pack: &open(&fixture.join("pack")),
        layout,
        rules: &rules,
        vanilla: &fixture_vanilla(),
        index: &run.index,
        sources: &only_generator,
        selectors: &selectors,
        policy: Policy::Pack,
    });
    assert!(refused.is_err_and(|refused| {
        refused
            .0
            .starts_with("unused-item selectors need a source that uses items")
    }));
}
