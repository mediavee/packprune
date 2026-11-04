//! The atlas resolution the oracle shares with the graph: a view's sprites, and a `filter` pattern outside the
//! supported subset refused by name.

use packprune_pack::{ClientRules, Layout, PackMeta, PhysPath, RunCx};

use super::support::{fixture_vanilla, open, repo};
use crate::atlases::{AtlasFile, SpriteInput, VanillaAtlases, ViewAtlases};
use crate::{Computed, Inputs, Policy, Reached, ReferenceIndex};

/// A sprite as text: id, origin, region, inputs.
type Shown = (String, Option<String>, String, Vec<(String, &'static str)>);

fn sprites(fixture: &str) -> Vec<Shown> {
    let cx = RunCx::default();
    let vanilla = fixture_vanilla();
    let pack = open(
        &repo()
            .join("tests/fixtures/packs")
            .join(fixture)
            .join("pack"),
    );
    let layout = Layout::new(&pack, PackMeta::read(&pack).unwrap(), &vanilla);
    let files: Vec<(PhysPath, AtlasFile)> = pack
        .files
        .iter()
        .filter_map(|(path, entry)| Some((path.clone(), AtlasFile::parse(&cx, &entry.blob)?)))
        .collect();
    let atlases = VanillaAtlases::new(&cx, &vanilla, &layout.meta.filters).unwrap();
    let view = ViewAtlases::build(&cx, &atlases, &layout.views[0], |physical| {
        files
            .iter()
            .find(|(path, _)| path == physical)
            .map(|(_, file)| file)
    })
    .unwrap();
    view.sprites("minecraft:blocks", &cx.interner)
        .into_iter()
        .map(|(id, sprite)| {
            let inputs = sprite
                .inputs
                .iter()
                .map(|SpriteInput { logical, reason }| (logical.to_string(), *reason))
                .collect();
            let origin = sprite
                .origin
                .as_ref()
                .map(|origin| origin.as_str().to_owned());
            (id, origin, sprite.region.to_string(), inputs)
        })
        .collect()
}

#[test]
fn a_view_lists_each_atlas_sprite_with_its_inputs_origin_and_region() {
    let blocks = sprites("unstitch");
    let tile = blocks
        .iter()
        .find(|(id, ..)| id == "fx:block/tile_b")
        .unwrap();
    assert_eq!(
        tile,
        &(
            "fx:block/tile_b".to_owned(),
            Some("assets/minecraft/atlases/blocks.json".to_owned()),
            "2,1 1,0,1,1".to_owned(),
            vec![(
                "assets/fx/textures/sheet/tiles.png".to_owned(),
                "atlas.unstitch"
            )]
        )
    );
    // Vanilla's directory source runs first: its sprites have no pack origin.
    let stone = blocks
        .iter()
        .find(|(id, ..)| id == "minecraft:block/stone")
        .unwrap();
    assert_eq!(
        (stone.1.as_deref(), stone.3[0].1),
        (None, "atlas.directory")
    );
    let ids: Vec<&str> = blocks.iter().map(|(id, ..)| id.as_str()).collect();
    assert!(ids.windows(2).all(|pair| pair[0] < pair[1]), "sorted by id");

    let filtered = sprites("atlas-filter");
    assert!(filtered.iter().any(|(id, ..)| id == "fx:loose/kept"));
    assert!(!filtered.iter().any(|(id, ..)| id == "fx:loose/dropped"));
}

/// A pattern outside the supported subset is refused with its text, never matched to nothing.
#[test]
fn a_filter_pattern_outside_the_subset_is_refused_by_name() {
    let meta = r#"{"pack": {"min_format": 88, "max_format": 88, "description": "t"}}"#;
    let atlas = r#"{"sources": [{"type": "filter", "pattern": {"path": "(?i)stone"}}]}"#;
    let block = r#"{"pack": {"min_format": 88, "max_format": 88, "description": "t"},
        "filter": {"block": [{"path": "a(?=b)"}]}}"#;
    for (files, pattern) in [
        (
            vec![
                ("pack.mcmeta", meta),
                ("assets/minecraft/atlases/blocks.json", atlas),
            ],
            "(?i)stone",
        ),
        (vec![("pack.mcmeta", block)], "a(?=b)"),
    ] {
        let cx = RunCx::default();
        let vanilla = fixture_vanilla();
        let pack = super::index::pack(&files);
        let layout = Layout::new(&pack, PackMeta::read(&pack).unwrap(), &vanilla);
        let rules = ClientRules::for_version("26.2");
        let index = ReferenceIndex::build(&cx, &pack, &layout, &rules, &Computed);
        let refused = Reached::analyse(&Inputs {
            cx: &cx,
            pack: &pack,
            layout: &layout,
            rules: &rules,
            vanilla: &vanilla,
            index: &index,
            sources: &[],
            selectors: &[],
            policy: Policy::Pack,
        })
        .unwrap_err();
        assert!(refused.0.contains(&format!("{pattern:?}")), "{}", refused.0);
    }
}
