use std::collections::BTreeSet;
use std::sync::Arc;

use packprune_pack::{PackInput, PhysPath};
use packprune_pass::Kept;

use crate::overlays::test_support::{Fixture, empty_pack, entries, logical, phys, put, text};

use super::{AtlasDirectory, ShortNames};
use packprune_codec::png::{Image, encode};

mod corpus;

pub(crate) const ATLAS: &str = "assets/minecraft/atlases/items.json";
const MODEL: &str = "00000000-0000-4000-8000-00000000000a";
pub(crate) const A: &str = "00000000-0000-4000-8000-0000000000a1";
const B: &str = "00000000-0000-4000-8000-0000000000b2";
const SOUND: &str = "00000000-0000-4000-8000-0000000000c3";

fn texture(id: &str) -> String {
    format!("assets/fx/textures/{id}.png")
}
fn model() -> String {
    format!("assets/fx/models/{MODEL}.json")
}
fn files() -> PackInput {
    let mut pack = empty_pack();
    for (uuid, color) in [(A, [0, 0, 0, 255]), (B, [128, 128, 0, 255])] {
        let png = encode(&Image {
            width: 4,
            height: 4,
            rgba: color.repeat(16),
        })
        .unwrap();
        put(&mut pack, &texture(uuid), &png);
    }
    put(&mut pack, &model(), format!(r##"{{"textures":{{"a":"fx:{A}","b":"fx:{B}"}},"elements":[{{"from":[0,0,0],"to":[16,16,16],"faces":{{"north":{{"texture":"#a"}},"south":{{"texture":"#b"}}}}}}]}}"##).as_bytes());
    put(
        &mut pack,
        "assets/fx/items/box.json",
        format!(r#"{{"model": {{"type": "minecraft:model", "model": "fx:{MODEL}"}}}}"#).as_bytes(),
    );
    put(&mut pack, ATLAS, format!(r#"{{"sources": [{{"type": "single", "resource": "fx:{A}"}}, {{"type": "single", "resource": "fx:{B}"}}]}}"#).as_bytes());
    put(
        &mut pack,
        "assets/fx/sounds.json",
        format!(r#"{{"boom": {{"sounds": ["fx:{SOUND}"]}}}}"#).as_bytes(),
    );
    put(
        &mut pack,
        &format!("assets/fx/sounds/{SOUND}.ogg"),
        b"not really vorbis",
    );
    pack
}
fn filtered(pattern: &str) -> PackInput {
    let mut pack = files();
    let atlas = String::from_utf8(pack.files[&phys(ATLAS)].blob.to_vec()).unwrap();
    put(
        &mut pack,
        ATLAS,
        atlas
            .replace(
                "}]}",
                &format!(r#"}}, {{"type": "filter", "pattern": {pattern}}}]}}"#),
            )
            .as_bytes(),
    );
    pack
}
fn grouped(sources: &str) -> PackInput {
    let mut pack = files();
    for (uuid, name) in [(A, "a"), (B, "b")] {
        let entry = pack.files.remove(&phys(&texture(uuid))).unwrap();
        pack.files
            .insert(phys(&format!("assets/fx/textures/g/{name}.png")), entry);
    }
    let bytes = String::from_utf8(pack.files[&phys(&model())].blob.to_vec())
        .unwrap()
        .replace(A, "g/a")
        .replace(B, "g/b");
    put(&mut pack, &model(), bytes.as_bytes());
    put(
        &mut pack,
        ATLAS,
        format!(r#"{{"sources": [{sources}]}}"#).as_bytes(),
    );
    pack
}
fn single(path: &str) -> String {
    format!(r#"{{"type": "single", "resource": "fx:{path}"}}"#)
}

#[test]
fn invalid_directory_sprite_keeps_its_texture() {
    let fixture = Fixture::new();
    let mut pack = empty_pack();
    let path = "assets/minecraft/textures/block/not used/andesite_pillar.png";
    let png = encode(&Image {
        width: 1,
        height: 1,
        rgba: vec![128, 128, 128, 255],
    })
    .unwrap();
    put(&mut pack, path, &png);
    let before = entries(&pack);
    let result = fixture.apply(pack, &[&ShortNames], Kept::default());
    assert_eq!(entries(&result.state.pack), before);
}

#[test]
fn unparseable_generated_sprites_keep_candidate_names() {
    let fixture = Fixture::new();
    for source in [
        r#"{"type":"directory","source":"","prefix":"not used/"}"#.to_owned(),
        format!(
            r#"{{"type":"paletted_permutations","textures":["fx:{A}"],"palette_key":"fx:{B}","permutations":{{"not used":"fx:{B}"}}}}"#
        ),
    ] {
        let mut pack = files();
        put(
            &mut pack,
            ATLAS,
            format!(r#"{{"sources":[{}, {}, {source}]}}"#, single(A), single(B)).as_bytes(),
        );
        let result = fixture.apply(pack, &[&ShortNames], Kept::default());
        for uuid in [A, B] {
            assert!(
                result.state.pack.files.contains_key(&phys(&texture(uuid))),
                "{source}: {uuid}"
            );
        }
        assert!(text(&result.state, ATLAS).contains(&source));
    }
}

#[test]
fn atlas_texture_outside_safe_paths_is_not_a_rename_candidate() {
    let fixture = Fixture::new();
    let mut pack = empty_pack();
    put(
        &mut pack,
        ATLAS,
        br#"{"sources":[{"type":"single","resource":"fx:../outside"}]}"#,
    );
    let before = entries(&pack);
    let result = fixture.apply(pack, &[&ShortNames], Kept::default());
    assert_eq!(entries(&result.state.pack), before);
}

#[test]
fn uuid_counters_and_indexed_references_match_v1() {
    let fixture = Fixture::new();
    let pack = files();
    let before = Arc::clone(&pack.files[&phys(&texture(A))].blob);
    let proof = fixture.prove(pack, &[&ShortNames]);
    let output = &proof.output;
    let renamed: BTreeSet<_> = output
        .pack
        .files
        .keys()
        .filter(|path| path.as_str().starts_with("assets/z/"))
        .map(PhysPath::as_str)
        .collect();
    assert_eq!(
        renamed,
        BTreeSet::from([
            "assets/z/models/0.json",
            "assets/z/textures/0/0.png",
            "assets/z/textures/0/1.png",
            "assets/z/sounds/0.ogg"
        ])
    );
    assert_eq!(
        output.pack.files[&phys("assets/z/textures/0/0.png")].blob,
        before
    );
    assert_eq!(
        text(output, "assets/fx/items/box.json"),
        r#"{"model": {"type": "minecraft:model", "model": "z:0"}}"#
    );
    assert_eq!(
        text(output, ATLAS),
        r#"{"sources": [{"type": "single", "resource": "z:0/0"}, {"type": "single", "resource": "z:0/1"}]}"#
    );
    assert_eq!(
        text(output, "assets/fx/sounds.json"),
        r#"{"boom": {"sounds": ["z:0"]}}"#
    );
    assert_eq!(
        proof.reports[0].changes,
        [
            "1 models, 2 textures and 1 sounds renamed into z:",
            "0 UUID-named files kept"
        ]
    );
}

#[test]
fn repeated_runs_are_byte_identical() {
    let fixture = Fixture::new();
    let first = fixture.apply(files(), &[&ShortNames], Kept::default());
    let second = fixture.apply(files(), &[&ShortNames], Kept::default());
    assert_eq!(entries(&first.state.pack), entries(&second.state.pack));
    assert!(
        first
            .state
            .pack
            .files
            .contains_key(&phys("assets/z/models/0.json"))
    );
}

#[test]
fn kept_names_never_move() {
    let fixture = Fixture::new();
    let kept = Kept {
        names: BTreeSet::from([logical(&texture(A)), logical(&model())]),
        ..Kept::default()
    };
    let result = fixture.apply(files(), &[&ShortNames], kept);
    assert!(result.state.pack.files.contains_key(&phys(&texture(A))));
    assert!(result.state.pack.files.contains_key(&phys(&model())));
    assert!(
        result
            .state
            .pack
            .files
            .contains_key(&phys("assets/z/textures/0.png"))
    );
}

#[test]
fn font_sheet_reference_follows_the_texture() {
    let fixture = Fixture::new();
    let mut pack = files();
    put(&mut pack, "assets/fx/font/hud.json", format!(r#"{{"providers":[{{"type":"bitmap","file":"fx:{B}.png","ascent":7,"chars":["\ue000"]}}]}}"#).as_bytes());
    let proof = fixture.prove(pack, &[&ShortNames]);
    assert!(text(&proof.output, "assets/fx/font/hud.json").contains(r#""file":"z:0/1.png""#));
}

#[test]
fn overlay_copies_and_texture_metadata_follow() {
    let fixture = Fixture::new();
    let mut pack = files();
    put(&mut pack, "pack.mcmeta", br#"{"pack":{"min_format":88,"max_format":88},"overlays":{"entries":[{"directory":"ov","min_format":89,"max_format":89}]}}"#);
    let png = Arc::clone(&pack.files[&phys(&texture(A))].blob);
    put(&mut pack, &format!("ov/{}", texture(A)), &png);
    put(
        &mut pack,
        &format!("ov/{}.mcmeta", texture(A)),
        br#"{"texture":{"blur":false}}"#,
    );
    let proof = fixture.prove(pack, &[&ShortNames]);
    assert_eq!(
        text(&proof.output, "ov/assets/z/textures/0/0.png.mcmeta"),
        r#"{"texture":{"blur":false}}"#
    );
    assert!(
        proof
            .output
            .pack
            .files
            .contains_key(&phys("ov/assets/z/textures/0/0.png"))
    );
}

#[test]
fn namespace_filter_selects_the_next_free_namespace() {
    let fixture = Fixture::new();
    let proof = fixture.prove(filtered(r#"{"namespace":"^z$"}"#), &[&ShortNames]);
    assert!(
        proof
            .output
            .pack
            .files
            .contains_key(&phys("assets/zz/textures/0/0.png"))
    );
}

#[test]
fn filter_rejection_recomputes_the_remaining_counters() {
    let fixture = Fixture::new();
    let proof = fixture.prove(filtered(r#"{"path":"^0/0$|^0$"}"#), &[&ShortNames]);
    assert!(proof.output.pack.files.contains_key(&phys(&texture(A))));
    assert!(proof.output.pack.files.contains_key(&phys(&texture(B))));
    assert!(
        proof
            .output
            .pack
            .files
            .contains_key(&phys("assets/z/models/0.json"))
    );
}

#[test]
fn equivalent_filter_allows_the_rename() {
    let fixture = Fixture::new();
    let proof = fixture.prove(filtered(r#"{"path":"^$"}"#), &[&ShortNames]);
    assert!(
        proof
            .output
            .pack
            .files
            .contains_key(&phys("assets/z/textures/0/0.png"))
    );
}

#[test]
fn filters_count_from_the_first_source_naming_the_texture() {
    // A sprite's filters start at the first source whose resource is its texture, here one that names it
    // under another sprite, so the filter after it still reads the renamed id and the rename is refused.
    let fixture = Fixture::new();
    let mut pack = files();
    put(
        &mut pack,
        ATLAS,
        format!(
            r#"{{"sources": [{{"type": "single", "resource": "fx:{A}", "sprite": "fx:other"}}, {{"type": "filter", "pattern": {{"path":"^0/0$|^0$"}}}}, {{"type": "single", "resource": "fx:{A}"}}, {{"type": "single", "resource": "fx:{B}"}}]}}"#
        )
        .as_bytes(),
    );
    let proof = fixture.prove(pack, &[&ShortNames]);
    assert!(proof.output.pack.files.contains_key(&phys(&texture(A))));
}

#[test]
fn occupied_namespace_is_avoided() {
    let fixture = Fixture::new();
    let mut pack = files();
    let png = Arc::clone(&pack.files[&phys(&texture(A))].blob);
    put(&mut pack, "assets/z/textures/0.png", &png);
    let result = fixture.apply(pack, &[&ShortNames], Kept::default());
    assert!(
        result
            .state
            .pack
            .files
            .contains_key(&phys("assets/zz/models/0.json"))
    );
    assert!(
        result
            .state
            .pack
            .files
            .contains_key(&phys("assets/z/textures/0.png"))
    );
}

#[test]
fn atlas_singles_become_the_exact_v1_directory_source() {
    let fixture = Fixture::new();
    let proof = fixture.prove(
        grouped(&format!("{}, {}", single("g/a"), single("g/b"))),
        &[&AtlasDirectory],
    );
    assert_eq!(
        text(&proof.output, ATLAS),
        r#"{"sources":[{"type":"directory","source":"g","prefix":"g/"}]}"#
    );
    assert_eq!(
        proof.reports[0].changes,
        ["2 single sources merged into directory sources, in 1 atlas files"]
    );
}

#[test]
fn extra_folder_texture_prevents_a_directory() {
    let fixture = Fixture::new();
    let mut pack = grouped(&format!("{}, {}", single("g/a"), single("g/b")));
    let png = Arc::clone(&pack.files[&phys("assets/fx/textures/g/a.png")].blob);
    put(&mut pack, "assets/fx/textures/g/unused.png", &png);
    let before = Arc::clone(&pack.files[&phys(ATLAS)].blob);
    let result = fixture.apply(pack, &[&AtlasDirectory], Kept::default());
    assert_eq!(result.state.pack.files[&phys(ATLAS)].blob, before);
}

#[test]
fn vanilla_texture_in_the_same_folder_prevents_a_directory() {
    let mut fixture = Fixture::new();
    fixture
        .vanilla
        .names
        .insert("assets/minecraft/textures/g/stone.png".into());
    let pack = grouped(&format!("{}, {}", single("g/a"), single("g/b")));
    let before = entries(&pack);
    let result = fixture.apply(pack, &[&AtlasDirectory], Kept::default());
    assert_eq!(entries(&result.state.pack), before);
}

#[test]
fn a_texture_missing_in_one_view_prevents_a_directory() {
    let fixture = Fixture::new();
    let mut pack = grouped(&format!("{}, {}", single("g/a"), single("g/b")));
    put(&mut pack, "pack.mcmeta", br#"{"pack":{"min_format":88,"max_format":88},"overlays":{"entries":[{"directory":"ov","min_format":89,"max_format":89}]}}"#);
    let png = pack
        .files
        .remove(&phys("assets/fx/textures/g/b.png"))
        .unwrap();
    pack.files
        .insert(phys("ov/assets/fx/textures/g/b.png"), png);
    let before = Arc::clone(&pack.files[&phys(ATLAS)].blob);
    let result = fixture.apply(pack, &[&AtlasDirectory], Kept::default());
    assert_eq!(result.state.pack.files[&phys(ATLAS)].blob, before);
}

#[test]
fn interleaved_filter_and_duplicate_sprite_keep_source_order() {
    let fixture = Fixture::new();
    for middle in [
        r#"{"type":"filter","pattern":{"path":"^g/a$"}}"#,
        r#"{"type":"single","resource":"fx:h/c","sprite":"fx:g/b"}"#,
    ] {
        let pack = grouped(&format!("{}, {middle}, {}", single("g/a"), single("g/b")));
        let before = Arc::clone(&pack.files[&phys(ATLAS)].blob);
        let result = fixture.apply(pack, &[&AtlasDirectory], Kept::default());
        assert_eq!(result.state.pack.files[&phys(ATLAS)].blob, before);
    }
}

#[test]
fn independent_single_between_members_follows_the_directory() {
    let fixture = Fixture::new();
    let mut pack = grouped(&format!(
        "{}, {}, {}",
        single("g/a"),
        single("h/c"),
        single("g/b")
    ));
    let png = Arc::clone(&pack.files[&phys("assets/fx/textures/g/a.png")].blob);
    put(&mut pack, "assets/fx/textures/h/c.png", &png);
    let proof = fixture.prove(pack, &[&AtlasDirectory]);
    assert_eq!(
        text(&proof.output, ATLAS),
        r#"{"sources":[{"type":"directory","source":"g","prefix":"g/"},{"type":"single","resource":"fx:h/c"}]}"#
    );
}

#[test]
fn framework_orders_short_names_before_atlas_directory() {
    let fixture = Fixture::new();
    let proof = fixture.prove(files(), &[&AtlasDirectory, &ShortNames]);
    assert_eq!(
        text(&proof.output, ATLAS),
        r#"{"sources":[{"type":"directory","source":"0","prefix":"0/"}]}"#
    );
    assert_eq!(
        proof
            .reports
            .iter()
            .map(|report| report.pass)
            .collect::<Vec<_>>(),
        ["short-names", "atlas-directory"]
    );
}
