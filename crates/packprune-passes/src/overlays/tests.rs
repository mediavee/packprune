use std::collections::BTreeSet;

use packprune_pack::PackInput;
use packprune_pass::{Enabled, Kept, Loss, Scope, prove, run};

use super::test_support::{Fixture, empty_pack, entries, logical, phys, put, read_pack, text};
use super::{OverlayFloor, OverlayMerge};

const LANG: &str = "assets/fx/lang/en_us.json";
const OTHER: &str = "assets/fx/lang/fr_fr.json";
fn pack(overlays: &[(&str, u32, u32, &str)], files: &[(&str, &str, &str)]) -> PackInput {
    let mut pack = empty_pack();
    let entries: Vec<_> = overlays
        .iter()
        .map(|(directory, min, max, shape)| {
            let range = match *shape {
                "legacy" => format!(r#""formats":[{min},{max}]"#),
                "modern" => format!(r#""min_format":{min},"max_format":{max}"#),
                "both" => {
                    format!(r#""formats":[{min},{max}],"min_format":{min},"max_format":{max}"#)
                }
                _ => panic!("unknown shape"),
            };
            format!(r#"{{"directory":"{directory}",{range}}}"#)
        })
        .collect();
    put(&mut pack, "pack.mcmeta", format!(r#"{{"pack":{{"min_format":88,"max_format":88,"description":"t"}},"overlays":{{"entries":[{}]}}}}"#, entries.join(",")).as_bytes());
    for (directory, logical, value) in files {
        put(
            &mut pack,
            &format!("{directory}/{logical}"),
            format!(r#"{{"key":"{value}"}}"#).as_bytes(),
        );
    }
    read_pack(&pack)
}
fn overlay_paths(pack: &PackInput) -> BTreeSet<String> {
    pack.files
        .keys()
        .filter(|path| !path.as_str().starts_with("assets/") && !path.as_str().starts_with("pack."))
        .map(ToString::to_string)
        .collect()
}

#[test]
fn adjacent_identical_overlays_merge_with_v1_bytes_and_report() {
    let fixture = Fixture::new();
    let input = pack(
        &[
            ("a", 34, 41, "legacy"),
            ("b", 42, 45, "legacy"),
            ("c", 46, 54, "both"),
            ("d", 55, 62, "legacy"),
        ],
        &[
            ("a", LANG, "X"),
            ("b", LANG, "X"),
            ("c", LANG, "X"),
            ("d", LANG, "Y"),
        ],
    );
    let proof = fixture.prove(input, &[&OverlayMerge]);
    assert_eq!(
        overlay_paths(&proof.output.pack),
        BTreeSet::from([format!("d/{LANG}"), format!("packprune_34_54/{LANG}")])
    );
    assert_eq!(
        text(&proof.output, "pack.mcmeta"),
        r#"{"pack":{"min_format":88,"max_format":88,"description":"t"},"overlays":{"entries":[{"directory":"packprune_34_54","formats":[34,54],"min_format":34,"max_format":54},{"directory":"d","formats":[55,62]}]}}"#
    );
    for directory in ["a", "b", "c"] {
        assert_eq!(
            proof.remap.physical(&phys(&format!("{directory}/{LANG}"))),
            phys(&format!("packprune_34_54/{LANG}"))
        );
    }
    assert_eq!(
        proof.reports[0].changes,
        [
            "2 redundant copies removed in 1 merges, into 1 new overlays",
            "3 overlays left empty and dropped: a, b, c"
        ]
    );
}

#[test]
fn new_overlay_is_inserted_between_competing_winners() {
    let fixture = Fixture::new();
    let input = pack(
        &[
            ("w", 34, 45, "legacy"),
            ("a", 34, 41, "legacy"),
            ("b", 42, 45, "legacy"),
            ("y", 38, 45, "legacy"),
        ],
        &[
            ("w", LANG, "W"),
            ("a", LANG, "X"),
            ("b", LANG, "X"),
            ("y", LANG, "Y"),
        ],
    );
    let proof = fixture.apply(input, &[&OverlayMerge], Kept::default());
    let directories: Vec<_> = proof
        .state
        .layout
        .meta
        .overlays
        .iter()
        .map(|entry| entry.directory.as_str())
        .collect();
    assert_eq!(directories, ["w", "packprune_34_45", "y"]);
}

#[test]
fn impossible_priority_and_range_holes_prevent_a_merge() {
    let fixture = Fixture::new();
    let impossible = pack(
        &[
            ("a", 34, 45, "legacy"),
            ("b", 40, 50, "legacy"),
            ("c", 46, 54, "legacy"),
        ],
        &[("a", LANG, "X"), ("b", LANG, "Y"), ("c", LANG, "X")],
    );
    let hole = pack(
        &[("a", 34, 41, "legacy"), ("c", 46, 54, "legacy")],
        &[("a", LANG, "X"), ("c", LANG, "X")],
    );
    for input in [impossible, hole] {
        let before = entries(&input);
        let result = fixture.apply(input, &[&OverlayMerge], Kept::default());
        assert_eq!(entries(&result.state.pack), before);
    }
}

#[test]
fn legacy_and_modern_only_entries_stay_separate() {
    let fixture = Fixture::new();
    let input = pack(
        &[("a", 34, 41, "legacy"), ("b", 42, 45, "modern")],
        &[("a", LANG, "X"), ("b", LANG, "X")],
    );
    let before = entries(&input);
    let result = fixture.apply(input, &[&OverlayMerge], Kept::default());
    assert_eq!(entries(&result.state.pack), before);
}

#[test]
fn minor_format_and_conflicting_range_shapes_prevent_a_merge() {
    let fixture = Fixture::new();
    for bounds in [
        r#""formats":[34,41],"min_format":[34,3],"max_format":41"#,
        r#""formats":[34,41],"min_format":34,"max_format":[41,3]"#,
        r#""formats":[35,41],"min_format":34,"max_format":41"#,
    ] {
        let mut input = pack(
            &[("a", 34, 41, "both"), ("b", 42, 45, "both")],
            &[("a", LANG, "X"), ("b", LANG, "X")],
        );
        put(&mut input, "pack.mcmeta", format!(
            r#"{{"pack":{{"min_format":88,"max_format":88}},"overlays":{{"entries":[{{"directory":"a",{bounds}}},{{"directory":"b","formats":[42,45],"min_format":42,"max_format":45}}]}}}}"#
        ).as_bytes());
        let before = entries(&input);
        let result = fixture.apply(input, &[&OverlayMerge], Kept::default());
        assert_eq!(entries(&result.state.pack), before, "{bounds}");
    }
}

#[test]
fn multiple_paths_share_the_new_overlay_and_other_files_keep_old_directories() {
    let fixture = Fixture::new();
    let input = pack(
        &[("a", 34, 41, "both"), ("b", 42, 45, "both")],
        &[
            ("a", LANG, "X"),
            ("b", LANG, "X"),
            ("a", OTHER, "F"),
            ("b", OTHER, "F"),
            ("a", "assets/fx/lang/de_de.json", "D"),
        ],
    );
    let proof = fixture.prove(input, &[&OverlayMerge]);
    assert_eq!(
        overlay_paths(&proof.output.pack),
        BTreeSet::from([
            "a/assets/fx/lang/de_de.json".into(),
            format!("packprune_34_45/{LANG}"),
            format!("packprune_34_45/{OTHER}")
        ])
    );
    assert_eq!(
        proof.reports[0].changes[0],
        "2 redundant copies removed in 2 merges, into 1 new overlays"
    );
}

#[test]
fn pinned_copy_stays_while_the_other_copies_merge() {
    let fixture = Fixture::new();
    let input = pack(
        &[
            ("a", 34, 41, "legacy"),
            ("b", 42, 45, "legacy"),
            ("c", 46, 54, "legacy"),
        ],
        &[("a", LANG, "X"), ("b", LANG, "X"), ("c", LANG, "X")],
    );
    let kept = Kept {
        pinned: BTreeSet::from([phys(&format!("a/{LANG}"))]),
        ..Kept::default()
    };
    let result = fixture.apply(input, &[&OverlayMerge], kept);
    assert_eq!(
        overlay_paths(&result.state.pack),
        BTreeSet::from([format!("a/{LANG}"), format!("packprune_42_54/{LANG}")])
    );
}

fn floor_pack() -> PackInput {
    pack(
        &[
            ("old", 34, 41, "legacy"),
            ("straddle", 84, 99, "both"),
            ("current", 88, 96, "modern"),
            ("future", 97, 199, "legacy"),
            ("nodir", 42, 199, "legacy"),
        ],
        &[
            ("old", LANG, "O"),
            ("straddle", LANG, "S"),
            ("current", LANG, "C"),
            ("future", LANG, "F"),
        ],
    )
}
fn options(oldest: u32) -> toml::Table {
    toml::toml! { oldest_format = oldest }
}

#[test]
fn floor_drops_unsupported_entries_and_raises_the_rest() {
    let fixture = Fixture::new();
    let proof = prove(
        &fixture.cx(),
        &fixture.assets,
        floor_pack(),
        vec![Enabled {
            pass: &OverlayFloor,
            scope: Scope::all(),
            options: options(88),
        }],
    )
    .unwrap();
    assert_eq!(
        overlay_paths(&proof.output.pack),
        BTreeSet::from([format!("straddle/{LANG}"), format!("current/{LANG}")])
    );
    assert_eq!(
        text(&proof.output, "pack.mcmeta"),
        r#"{"pack":{"min_format":88,"max_format":88,"description":"t"},"overlays":{"entries":[{"directory":"straddle","formats":[88,99],"min_format":88,"max_format":99},{"directory":"current","min_format":88,"max_format":96}]}}"#
    );
    assert_eq!(proof.reports[0].loss, Loss::Lossy);
    let loss = proof.loss.as_ref().unwrap();
    assert!(loss.differing_keys > 0);
    let before = fixture.cx().analyse(floor_pack()).unwrap();
    let logical = logical(LANG);
    let a = before.layout.view_at((88, 0)).files[&logical].clone();
    let b = proof.output.layout.view_at((88, 0)).files[&logical].clone();
    assert_eq!(before.pack.files[&a].blob, proof.output.pack.files[&b].blob);
    assert_eq!(
        proof.reports[0].changes,
        [
            "3 overlay entries dropped (1 below format 88, 1 above the client's, 1 without a directory), 1 starting at 88",
            "2 files of dropped overlays removed (22 B)"
        ]
    );
}

#[test]
fn floor_below_all_overlays_changes_nothing() {
    let fixture = Fixture::new();
    let input = pack(&[("a", 34, 41, "legacy")], &[("a", LANG, "X")]);
    let before = entries(&input);
    let state = fixture.cx().analyse(input).unwrap();
    let result = run(
        &fixture.cx(),
        &Kept::of(&state),
        &state,
        &[Enabled {
            pass: &OverlayFloor,
            scope: Scope::all(),
            options: options(30),
        }],
    )
    .unwrap();
    assert_eq!(entries(&result.state.pack), before);
}

#[test]
fn floor_keeps_pinned_files_of_dropped_directories() {
    let fixture = Fixture::new();
    let state = fixture.cx().analyse(floor_pack()).unwrap();
    let pinned = phys(&format!("old/{LANG}"));
    let kept = Kept {
        pinned: BTreeSet::from([pinned.clone()]),
        ..Kept::default()
    };
    let result = run(
        &fixture.cx(),
        &kept,
        &state,
        &[Enabled {
            pass: &OverlayFloor,
            scope: Scope::all(),
            options: options(88),
        }],
    )
    .unwrap();
    assert!(result.state.pack.files.contains_key(&pinned));
    assert!(
        !result
            .state
            .layout
            .meta
            .overlays
            .iter()
            .any(|entry| entry.directory == "old")
    );
}
