use std::collections::BTreeSet;

use super::*;
use crate::{Enabled, Kept, Proof, Refusal, RunError, Scope, prove, run};

const BOX_ITEM: &str = "assets/fx/items/box.json";
const BOX_MODEL: &str = "assets/fx/models/item/box.json";
const CRATE_MODEL: &str = "assets/fx/models/item/crate.json";
const BASE_MODEL: &str = "assets/fx/models/item/base.json";
const TEXTURE_A: &str = "assets/fx/textures/item/a.png";
const RENAMED_TEXTURE: &str = "assets/fx/textures/item/z0.png";
const RENAMED_MODEL: &str = "assets/fx/models/z/0.json";
const ORPHAN: &str = "assets/fx/models/item/orphan.json";

fn put(files: &mut Files, path: &str, text: &str) {
    files.insert(path.into(), text.as_bytes().to_vec());
}

/// Two items whose models sample the same texture `fx:item/a` through a parent with a display transform, and an
/// orphan model.
fn files() -> Files {
    let mut files = Files::new();
    put(
        &mut files,
        "pack.mcmeta",
        r#"{"pack": {"min_format": 88, "max_format": 88, "description": "t"}}"#,
    );
    put(
        &mut files,
        BOX_ITEM,
        r#"{"model": {"type": "minecraft:model", "model": "fx:item/box"}}"#,
    );
    put(
        &mut files,
        "assets/fx/items/crate.json",
        r#"{"model": {"type": "minecraft:model", "model": "fx:item/crate"}}"#,
    );
    put(
        &mut files,
        BASE_MODEL,
        r#"{"display": {"gui": {"rotation": [30.0, 45.0, 0.0]}}}"#,
    );
    put(
        &mut files,
        BOX_MODEL,
        r##"{"parent": "fx:item/base", "textures": {"a": "fx:item/a", "b": "fx:item/b"},
 "elements": [{"from": [0.0, 0.0, 0.0], "to": [16.0, 16.0, 16.0],
   "faces": {"north": {"uv": [0.0, 0.0, 8.0, 8.0], "texture": "#a"}, "south": {"texture": "#b"}}}]}"##,
    );
    put(
        &mut files,
        CRATE_MODEL,
        r##"{"parent": "fx:item/base", "textures": {"all": "fx:item/a"},
 "elements": [{"from": [0.0, 0.0, 0.0], "to": [16.0, 16.0, 16.0], "faces": {"up": {"texture": "#all"}}}]}"##,
    );
    put(&mut files, TEXTURE_A, "png a");
    put(
        &mut files,
        &format!("{TEXTURE_A}.mcmeta"),
        r#"{"texture": {"blur": false}}"#,
    );
    put(&mut files, "assets/fx/textures/item/b.png", "png b");
    put(&mut files, ORPHAN, r#"{"parent": "fx:item/base"}"#);
    put(
        &mut files,
        "assets/fx/lang/en_us.json",
        r#"{"item.fx.box": "Box"}"#,
    );
    files
}

/// `files()` with an overlay every view applies.
fn with_overlay() -> Files {
    let mut files = files();
    put(
        &mut files,
        "pack.mcmeta",
        r#"{"pack": {"min_format": 88, "max_format": 88, "description": "t"},
 "overlays": {"entries": [{"directory": "over", "formats": [88, 88]}]}}"#,
    );
    files
}

fn enabled(pass: &dyn Pass) -> Enabled<'_> {
    Enabled {
        pass,
        scope: Scope::all(),
        options: toml::Table::new(),
    }
}

fn proved(fx: &Fixture, files: &Files, passes: &[&dyn Pass]) -> Result<Proof, RunError> {
    prove(
        &fx.cx(),
        &fx.client,
        pack(files),
        passes.iter().map(|pass| enabled(*pass)).collect(),
    )
}

fn rename_texture(name: &'static str, with_sprite: bool) -> Rename {
    Rename {
        name,
        targets: Targets::Listed {
            files: vec![(TEXTURE_A, RENAMED_TEXTURE)],
            sprites: if with_sprite {
                vec![("fx:item/a", "fx:item/z0")]
            } else {
                Vec::new()
            },
        },
    }
}

fn refusal(result: Result<Proof, RunError>) -> Refusal {
    match result {
        Err(RunError::Refused { refusal, .. }) => refusal,
        other => panic!(
            "expected a refusal, got {:?}",
            other.map(|proof| proof.reports)
        ),
    }
}

#[test]
fn a_file_rename_rewrites_every_reference_and_the_index_matches_a_rebuild() {
    let fx = Fixture::new();
    let rename = Rename {
        name: "rename",
        targets: Targets::Listed {
            files: vec![(BASE_MODEL, RENAMED_MODEL)],
            sprites: Vec::new(),
        },
    };

    let proof = proved(&fx, &files(), &[&rename]).unwrap();

    let out = &proof.output;
    assert_eq!(proof.pruned, BTreeSet::from([phys(ORPHAN)]));
    assert!(text(out, BOX_MODEL).starts_with(r#"{"parent": "fx:z/0", "textures""#));
    assert!(text(out, CRATE_MODEL).starts_with(r#"{"parent": "fx:z/0", "textures""#));
    assert_eq!(
        out.pack.files[&phys(RENAMED_MODEL)].blob.as_ref(),
        files()[BASE_MODEL].as_slice()
    );
    assert!(!out.pack.files.contains_key(&phys(BASE_MODEL)));
    assert_eq!(
        proof.reports[0].changed,
        [BASE_MODEL, BOX_MODEL, CRATE_MODEL, RENAMED_MODEL].map(phys)
    );
    assert_eq!(proof.proof.unwrap().differing_keys, 0);
    assert_eq!(proof.remap.physical(&phys(BASE_MODEL)), phys(RENAMED_MODEL));
    assert_index_rebuilt(&fx.cx(), out);
}

#[test]
fn a_texture_rename_with_its_sprite_rewrites_both_models_and_takes_its_metadata_along() {
    let fx = Fixture::new();

    let proof = proved(&fx, &files(), &[&rename_texture("rename-texture", true)]).unwrap();

    let out = &proof.output;
    assert!(text(out, BOX_MODEL).contains(r#""a": "fx:item/z0""#));
    assert!(text(out, CRATE_MODEL).contains(r#""all": "fx:item/z0""#));
    assert_eq!(
        text(out, &format!("{RENAMED_TEXTURE}.mcmeta")),
        r#"{"texture": {"blur": false}}"#
    );
    assert!(
        !out.pack
            .files
            .contains_key(&phys(&format!("{TEXTURE_A}.mcmeta")))
    );
    assert_eq!(
        proof.remap.physical(&phys(&format!("{TEXTURE_A}.mcmeta"))),
        phys(&format!("{RENAMED_TEXTURE}.mcmeta")),
        "the remap carries the move"
    );
    let proof_of = proof.proof.unwrap();
    assert_eq!(proof_of.differing_keys, 0, "{:?}", proof_of.differences);
    assert_index_rebuilt(&fx.cx(), out);
}

#[test]
fn a_texture_renamed_without_its_sprite_remap_fails_the_harness_by_name() {
    let fx = Fixture::new();

    let result = proved(&fx, &files(), &[&rename_texture("rename-texture", false)]);

    match result {
        Err(RunError::NotEquivalent { passes, comparison }) => {
            assert_eq!(passes, ["rename-texture"]);
            assert!(comparison.differing_keys > 0);
        }
        other => panic!(
            "expected the oracle to refuse, got {:?}",
            other.map(|proof| proof.proof)
        ),
    }
}

#[test]
fn a_removal_drops_the_file_and_its_references_from_the_index() {
    let fx = Fixture::new();
    let cx = fx.cx();
    let state = cx.analyse(pack(&files())).unwrap();
    let remove = Remove(vec![BASE_MODEL]);

    let outcome = run(&cx, &Kept::of(&state), &state, &[enabled(&remove)]).unwrap();

    assert!(!outcome.state.pack.files.contains_key(&phys(BASE_MODEL)));
    assert_eq!(outcome.reports[0].changed, [phys(BASE_MODEL)]);
    assert_eq!(outcome.reports[0].files_after, files().len() - 1);
    assert!(outcome.state.index.references(&phys(BASE_MODEL)).is_empty());
    assert_index_rebuilt(&cx, &outcome.state);
}

#[test]
fn the_state_the_passes_leave_carries_the_analysis_of_its_own_pack() {
    let fx = Fixture::new();
    let cx = fx.cx();
    let state = cx.analyse(pack(&files())).unwrap();
    let (orphan, base) = (Remove(vec![ORPHAN]), Remove(vec![BASE_MODEL]));

    let outcome = run(
        &cx,
        &Kept::of(&state),
        &state,
        &[enabled(&orphan), enabled(&base)],
    )
    .unwrap();

    let fresh = cx.analyse(outcome.state.pack.clone()).unwrap();
    assert!(
        outcome
            .state
            .reached
            .files
            .keys()
            .eq(fresh.reached.files.keys())
    );
    assert_eq!(outcome.state.reached.removable(), fresh.reached.removable());
}

#[test]
fn a_move_into_an_overlay_follows_the_file_and_proves_the_same() {
    let fx = Fixture::new();
    let over = format!("over/{BASE_MODEL}");
    let moved = Move(vec![(BASE_MODEL, over.clone().leak())]);

    let proof = proved(&fx, &with_overlay(), &[&moved]).unwrap();

    let out = &proof.output;
    assert!(out.pack.files.contains_key(&phys(&over)));
    assert!(!out.pack.files.contains_key(&phys(BASE_MODEL)));
    assert_eq!(proof.reports[0].changed, [phys(BASE_MODEL), phys(&over)]);
    assert_eq!(proof.remap.physical(&phys(BASE_MODEL)), phys(&over));
    assert_eq!(proof.proof.unwrap().differing_keys, 0);
    assert_index_rebuilt(&fx.cx(), out);
}

#[test]
fn no_pass_and_an_empty_pass_leave_every_entry_as_it_was() {
    let fx = Fixture::new();
    let cx = fx.cx();
    let input = pack(&files());
    let state = cx.analyse(input.clone()).unwrap();
    let nothing = Edit {
        loss: Loss::Lossless,
        edits: Box::new(|_| Edits::default()),
    };

    let none = run(&cx, &Kept::of(&state), &state, &[]).unwrap();
    let empty = run(&cx, &Kept::of(&state), &state, &[enabled(&nothing)]).unwrap();

    assert_eq!(entries(&none.state.pack), entries(&input));
    assert_eq!(entries(&empty.state.pack), entries(&input));
    assert_eq!(empty.state.pack.digest, input.digest);
    assert!(empty.reports[0].changed.is_empty());
}

/// Moves, renames (models picked by the pass, a texture with its sprite) and their re-prune, at one thread and at
/// eight: the same edits, bytes, index and reports.
#[test]
fn the_output_does_not_depend_on_the_thread_count() {
    let at = |threads| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap()
            .install(|| {
                let fx = Fixture::new();
                let over = format!("over/{BASE_MODEL}");
                let moved = Move(vec![(BASE_MODEL, over.leak())]);
                let models = Rename {
                    name: "rename-models",
                    targets: Targets::Models,
                };
                let texture = rename_texture("rename-texture", true);
                let proof = proved(&fx, &with_overlay(), &[&texture, &models, &moved]).unwrap();
                assert_eq!(
                    proof.remap.physical(&phys(BASE_MODEL)),
                    phys("over/assets/fx/models/item/pp_base.json")
                );
                assert_eq!(proof.proof.as_ref().unwrap().differing_keys, 0);
                (
                    entries(&proof.output.pack),
                    proof.output.index.all().clone(),
                    proof.reports,
                )
            })
    };
    let one = at(1);
    assert_eq!(
        one.2.iter().map(|report| report.pass).collect::<Vec<_>>(),
        ["move", "rename-models", "rename-texture"]
    );
    assert!(
        one.0
            .iter()
            .any(|(path, _)| path.as_str().ends_with("pp_box.json"))
    );
    assert!(one == at(8));
}

#[test]
fn a_remap_of_a_kept_name_is_refused() {
    let fx = Fixture::new();
    let rename = Edit {
        loss: Loss::Lossless,
        edits: Box::new(|_| {
            let mut edits = Edits::default();
            edits
                .file_remaps
                .insert(logical(BOX_ITEM), logical("assets/fx/items/other.json"));
            edits
        }),
    };

    let refused = refusal(proved(&fx, &files(), &[&rename]));

    assert_eq!(
        refused,
        Refusal::KeptName {
            from: logical(BOX_ITEM)
        }
    );
}

#[test]
fn an_edit_outside_the_scope_is_refused_and_one_inside_is_measured() {
    let fx = Fixture::new();
    let lossy = Edit {
        loss: Loss::Lossy,
        edits: Box::new(|cx| {
            let mut edits = Edits::default();
            let path = phys(BOX_MODEL);
            let bytes = text_of(cx, &path).replace("16.0, 16.0, 16.0", "16.0, 8.0, 16.0");
            edits.replaced.insert(path, bytes.into_bytes().into());
            edits
        }),
    };
    let on = |prefix: &'static str| Enabled {
        pass: &lossy,
        scope: Scope::matching(move |path| path.as_str().starts_with(prefix)),
        options: toml::Table::new(),
    };

    let outside = prove(
        &fx.cx(),
        &fx.client,
        pack(&files()),
        vec![on("assets/fx/textures/")],
    );
    let inside = prove(
        &fx.cx(),
        &fx.client,
        pack(&files()),
        vec![on("assets/fx/models/")],
    );

    assert_eq!(
        refusal(outside),
        Refusal::OutsideScope {
            path: phys(BOX_MODEL)
        }
    );
    let inside = inside.unwrap();
    assert!(inside.proof.is_none());
    assert!(inside.loss.unwrap().differing_keys > 0);
}

fn text_of(cx: &PassCx<'_>, path: &PhysPath) -> String {
    String::from_utf8(cx.pack.files[path].blob.to_vec()).unwrap()
}

#[test]
fn a_pinned_path_cannot_be_removed_or_renamed() {
    let fx = Fixture::new();
    let cx = fx.cx();
    let state = cx.analyse(pack(&files())).unwrap();
    let kept = Kept {
        pinned: BTreeSet::from([phys(BASE_MODEL)]),
        ..Kept::of(&state)
    };
    let remove = Remove(vec![BASE_MODEL]);
    let remap = Edit {
        loss: Loss::Lossless,
        edits: Box::new(|_| {
            let mut edits = Edits::default();
            edits
                .file_remaps
                .insert(logical(BASE_MODEL), logical(RENAMED_MODEL));
            edits
        }),
    };

    let removed = run(&cx, &kept, &state, &[enabled(&remove)]).unwrap_err();
    let renamed = run(&cx, &kept, &state, &[enabled(&remap)]).unwrap_err();

    assert!(matches!(
        removed,
        RunError::Refused {
            refusal: Refusal::Pinned {
                verb: "removes",
                ..
            },
            ..
        }
    ));
    assert!(matches!(
        renamed,
        RunError::Refused {
            refusal: Refusal::PinnedName { .. },
            ..
        }
    ));
}

#[test]
fn edits_the_framework_cannot_apply_are_refused_by_name() {
    type Make = fn(&PassCx<'_>) -> Edits;
    let cases: [(&str, Make); 7] = [
        ("two edits", |cx| {
            let mut edits = Edits::default();
            let path = phys(BOX_MODEL);
            edits
                .replaced
                .insert(path.clone(), Arc::clone(&cx.pack.files[&path].blob));
            edits.removed.insert(path);
            edits
        }),
        ("absent", |_| {
            let mut edits = Edits::default();
            edits.removed.insert(phys("assets/fx/models/none.json"));
            edits
        }),
        ("exists", |cx| {
            let mut edits = Edits::default();
            let path = phys(BOX_MODEL);
            edits
                .added
                .insert(path.clone(), Arc::clone(&cx.pack.files[&path].blob));
            edits
        }),
        ("move not edited", |_| {
            let mut edits = Edits::default();
            edits
                .moves
                .insert(phys(BASE_MODEL), phys(&format!("over/{BASE_MODEL}")));
            edits
        }),
        ("undeclared move", |cx| {
            let mut edits = Edits::default();
            let path = phys(BASE_MODEL);
            edits.removed.insert(path.clone());
            edits.added.insert(
                phys(&format!("over/{BASE_MODEL}")),
                Arc::clone(&cx.pack.files[&path].blob),
            );
            edits
        }),
        ("cycle", |_| {
            let mut edits = Edits::default();
            edits
                .file_remaps
                .insert(logical(BASE_MODEL), logical(RENAMED_MODEL));
            edits
                .file_remaps
                .insert(logical(RENAMED_MODEL), logical(BASE_MODEL));
            edits
        }),
        ("unnameable", |cx| {
            let mut edits = Edits::default();
            let (from, to) = (phys(BASE_MODEL), "assets/fx/textures/base.json");
            edits.removed.insert(from.clone());
            edits
                .added
                .insert(phys(to), Arc::clone(&cx.pack.files[&from].blob));
            edits.file_remaps.insert(logical(BASE_MODEL), logical(to));
            edits
        }),
    ];
    let fx = Fixture::new();
    for (case, make) in cases {
        let pass = Edit {
            loss: Loss::Lossless,
            edits: Box::new(make),
        };
        let refused = refusal(proved(&fx, &with_overlay(), &[&pass]));
        let named = match case {
            "two edits" => matches!(refused, Refusal::TwoEdits { .. }),
            "absent" => matches!(
                refused,
                Refusal::Absent {
                    verb: "removes",
                    ..
                }
            ),
            "exists" => matches!(refused, Refusal::Exists { .. }),
            "move not edited" => matches!(refused, Refusal::MoveNotEdited { .. }),
            "undeclared move" => matches!(refused, Refusal::UndeclaredMove { .. }),
            "cycle" => matches!(refused, Refusal::Cycle { .. }),
            "unnameable" => matches!(refused, Refusal::Unnameable { .. }),
            _ => unreachable!(),
        };
        assert!(named, "{case}: {refused:?}");
    }
}

#[test]
fn a_remap_of_a_file_a_shader_imports_by_text_is_refused() {
    let fx = Fixture::new();
    let mut files = files();
    put(
        &mut files,
        "assets/fx/shaders/core/glow.vsh",
        "#version 150\n#moj_import <fx:helper.glsl>\nvoid main() {}\n",
    );
    put(
        &mut files,
        "assets/fx/shaders/include/helper.glsl",
        "float x;\n",
    );
    let rename = Rename {
        name: "rename",
        targets: Targets::Listed {
            files: vec![(
                "assets/fx/shaders/include/helper.glsl",
                "assets/fx/shaders/include/h.glsl",
            )],
            sprites: Vec::new(),
        },
    };

    let refused = refusal(proved(&fx, &files, &[&rename]));

    assert_eq!(
        refused,
        Refusal::TextName {
            logical: logical("assets/fx/shaders/include/helper.glsl"),
            file: phys("assets/fx/shaders/core/glow.vsh"),
        }
    );
}

#[test]
fn a_sprite_remap_of_a_vanilla_texture_is_refused() {
    let fx = Fixture::new();
    let rename = Rename {
        name: "rename",
        targets: Targets::Listed {
            files: Vec::new(),
            sprites: vec![("minecraft:block/stone", "minecraft:block/rock")],
        },
    };

    let refused = refusal(proved(&fx, &files(), &[&rename]));

    assert_eq!(
        refused,
        Refusal::KeptSprite {
            sprite: "minecraft:block/stone".into(),
            atlas: "minecraft:blocks".into(),
            texture: "assets/minecraft/textures/block/stone.png".into(),
            why: "is vanilla's",
        }
    );
}

/// One 2 by 2 RGBA image, its IDAT stored, then deflated: other bytes, the same pixels.
const ICON_STORED: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 2, 8, 6, 0,
    0, 0, 114, 182, 13, 36, 0, 0, 0, 29, 73, 68, 65, 84, 120, 1, 1, 18, 0, 237, 255, 0, 0, 20, 30,
    255, 10, 20, 30, 255, 0, 100, 20, 30, 255, 110, 20, 30, 255, 42, 66, 5, 161, 79, 204, 53, 188,
    0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];
const ICON_DEFLATED: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 2, 8, 6, 0,
    0, 0, 114, 182, 13, 36, 0, 0, 0, 21, 73, 68, 65, 84, 120, 218, 99, 96, 16, 145, 251, 207, 5,
    196, 12, 41, 64, 34, 15, 136, 1, 42, 66, 5, 161, 52, 157, 126, 52, 0, 0, 0, 0, 73, 69, 78, 68,
    174, 66, 96, 130,
];

fn replace(path: &'static str, bytes: &'static [u8]) -> Edit {
    Edit {
        loss: Loss::Lossless,
        edits: Box::new(move |_| {
            let mut edits = Edits::default();
            edits.replaced.insert(phys(path), bytes.into());
            edits
        }),
    }
}

fn not_equivalent(result: Result<Proof, RunError>) -> bool {
    matches!(result, Err(RunError::NotEquivalent { .. }))
}

/// The orphan model, named by a keep-list: no item of the pack shows it, the server does.
fn named_orphan() -> [packprune_graph::SourceUsages; 1] {
    use packprune_graph::{IdGlob, SourceUsages, Usage, UsageKind};
    [SourceUsages {
        source: "keep".into(),
        path: "keep.yml".into(),
        usages: vec![Usage {
            kind: UsageKind::Model,
            id: IdGlob("fx:item/orphan".into()),
            atlas: None,
            location: "keep.yml:1".into(),
            reason: None,
            declares: None,
            quiet: false,
        }],
    }]
}

#[test]
fn a_lossless_change_to_a_model_only_a_source_names_fails_the_harness() {
    let fx = Fixture::new();
    let sources = named_orphan();
    let cx = Context {
        sources: &sources,
        ..fx.cx()
    };
    let rotate = replace(
        ORPHAN,
        br#"{"parent": "fx:item/base", "display": {"gui": {"rotation": [0, 0, 0]}}}"#,
    );
    let remove = Remove(vec![ORPHAN]);
    let prove_with = |pass: &dyn Pass| prove(&cx, &fx.client, pack(&files()), vec![enabled(pass)]);

    assert!(not_equivalent(prove_with(&rotate)));
    assert!(not_equivalent(prove_with(&remove)));
}

#[test]
fn a_lossless_change_to_what_the_pack_list_shows_fails_the_harness() {
    let fx = Fixture::new();
    let mut files = files();
    files.insert("pack.png".into(), ICON_STORED.to_vec());
    let description = Edit {
        loss: Loss::Lossless,
        edits: Box::new(|cx| {
            let mut edits = Edits::default();
            let path = phys("pack.mcmeta");
            let changed = text_of(cx, &path).replace(r#""t""#, r#""Changed description""#);
            edits.replaced.insert(path, changed.into_bytes().into());
            edits
        }),
    };
    let respelled = replace(
        "pack.mcmeta",
        br#"{"pack":{"description":{"text":"t"},"max_format":88,"min_format":88}}"#,
    );

    assert!(not_equivalent(proved(
        &fx,
        &files,
        &[&Remove(vec!["pack.png"])]
    )));
    assert!(not_equivalent(proved(&fx, &files, &[&description])));
    let reencoded = proved(&fx, &files, &[&replace("pack.png", ICON_DEFLATED)]).unwrap();
    assert_eq!(reencoded.reports[0].changed, [phys("pack.png")]);
    let respelled = proved(&fx, &files, &[&respelled]).unwrap();
    assert_eq!(respelled.reports[0].changed, [phys("pack.mcmeta")]);
}

#[test]
fn a_move_that_changes_the_kind_is_tokenized_again() {
    let fx = Fixture::new();
    let cx = fx.cx();
    let state = cx.analyse(pack(&files())).unwrap();
    let moved = Move(vec![(BOX_MODEL, "assets/fx/particles/moved.json")]);

    let outcome = run(&cx, &Kept::of(&state), &state, &[enabled(&moved)]).unwrap();

    assert_index_rebuilt(&cx, &outcome.state);
}
