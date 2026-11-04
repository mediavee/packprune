use std::collections::BTreeMap;

use packprune_codec::png::{self, Image};
use packprune_pack::{
    Digest, Entry, Layout, PackInput, PackMeta, PhysPath, TextureSize, VanillaIndex,
};

use super::*;

// Expected sizes are worked out by hand from the 26.2 client's `SpriteLoader` and `Stitcher`, not read from this
// code's output.

fn atlas(source: &str, prefix: &str) -> String {
    format!(r#"{{"sources":[{{"type":"directory","source":"{source}","prefix":"{prefix}"}}]}}"#)
}

fn png_bytes(width: u32, height: u32) -> Vec<u8> {
    let rgba = vec![0; width as usize * height as usize * 4];
    png::encode(&Image {
        width,
        height,
        rgba,
    })
    .unwrap()
}

/// A pack stacked over a client jar, built in memory.
struct Fixture {
    pack: BTreeMap<String, Vec<u8>>,
    textures: BTreeMap<String, TextureSize>,
    texture_meta: BTreeMap<String, Vec<u8>>,
    atlases: BTreeMap<String, Vec<u8>>,
    format: &'static str,
}

impl Fixture {
    fn new() -> Self {
        let mut fixture = Self {
            pack: BTreeMap::new(),
            textures: BTreeMap::new(),
            texture_meta: BTreeMap::new(),
            atlases: BTreeMap::new(),
            format: "88.0",
        };
        fixture.pack_file("pack.mcmeta", r#"{"pack":{"pack_format":88}}"#);
        fixture.client_atlas("items", &atlas("item", "item/"));
        fixture.client_atlas("blocks", &atlas("block", "block/"));
        fixture
    }

    fn client_atlas(&mut self, name: &str, json: &str) -> &mut Self {
        self.atlases
            .insert(format!("assets/minecraft/atlases/{name}.json"), json.into());
        self
    }

    fn client_png(&mut self, path: &str, width: u32, height: u32) -> &mut Self {
        self.textures
            .insert(path.into(), TextureSize { width, height });
        self
    }

    fn client_mcmeta(&mut self, path: &str, json: &str) -> &mut Self {
        self.texture_meta.insert(path.into(), json.into());
        self
    }

    fn pack_png(&mut self, path: &str, width: u32, height: u32) -> &mut Self {
        self.pack.insert(path.into(), png_bytes(width, height));
        self
    }

    fn pack_file(&mut self, path: &str, text: &str) -> &mut Self {
        self.pack.insert(path.into(), text.into());
        self
    }

    fn cost(&self) -> Cost {
        self.try_cost().unwrap()
    }

    fn try_cost(&self) -> Result<Cost, CostError> {
        let files = self
            .pack
            .iter()
            .map(|(path, bytes)| {
                let entry = Entry {
                    blob: bytes.as_slice().into(),
                    digest: Digest([0; 32]),
                };
                (PhysPath::new(path).unwrap(), entry)
            })
            .collect();
        let pack = PackInput {
            name: "fixture".into(),
            files,
            digest: Digest([0; 32]),
        };
        let vanilla = VanillaIndex {
            pack_format: self.format.into(),
            atlases: self.atlases.clone(),
            textures: self.textures.clone(),
            texture_meta: self.texture_meta.clone(),
            ..VanillaIndex::default()
        };
        let meta =
            PackMeta::parse(&pack.files[&PhysPath::new("pack.mcmeta").unwrap()].blob).unwrap();
        let layout = Layout::new(&pack, meta, &vanilla);
        Calculator {
            pack: &pack,
            layout: &layout,
            vanilla: &vanilla,
        }
        .client_view()
    }
}

impl Cost {
    fn atlas(&self, id: &str) -> &AtlasCost {
        self.atlases.iter().find(|a| a.id == id).unwrap()
    }

    /// `WxH gpu-bytes` of an atlas.
    fn size(&self, id: &str) -> String {
        let atlas = self.atlas(id);
        format!("{}x{} {}", atlas.width, atlas.height, atlas.gpu)
    }

    fn font(&self, id: &str) -> &FontCost {
        self.fonts.iter().find(|f| f.font == id).unwrap()
    }
}

#[test]
fn a_sprite_that_crosses_a_power_of_two_doubles_the_atlas() {
    // No mipmaps on items: padding 1, so a 16 px sprite is an 18 px holder and the missing sprite is one too.
    let mut fixture = Fixture::new();
    assert_eq!(
        fixture.cost().size("minecraft:items"),
        "32x32 4096",
        "one 18 px holder: 32x32 RGBA"
    );
    fixture.client_png("assets/minecraft/textures/item/a.png", 16, 16);
    assert_eq!(
        fixture.cost().size("minecraft:items"),
        "64x32 8192",
        "a second holder grows x"
    );
    fixture.pack_png("assets/minecraft/textures/item/big.png", 31, 31);
    // The 33 px holder goes first in a 64x64 slot; a and missing fit its spare column without growing again.
    let cost = fixture.cost();
    assert_eq!(cost.size("minecraft:items"), "64x64 16384");
    assert!(cost.complete());
}

#[test]
fn three_sprites_stitch_to_the_size_the_client_reaches_step_by_step() {
    // Three 18 px holders (a, anim, missing): 32x32, then x to 64x32, then y to 64x64.
    let mut fixture = Fixture::new();
    fixture
        .client_png("assets/minecraft/textures/item/a.png", 16, 16)
        .client_png("assets/minecraft/textures/item/b.png", 16, 16);
    assert_eq!(fixture.cost().size("minecraft:items"), "64x64 16384");
}

#[test]
fn an_animated_sprite_occupies_one_frame_while_its_strip_stays_decoded() {
    let mut fixture = Fixture::new();
    fixture
        .client_png("assets/minecraft/textures/item/a.png", 16, 16)
        .pack_png("assets/minecraft/textures/item/anim.png", 16, 64)
        .pack_file(
            "assets/minecraft/textures/item/anim.png.mcmeta",
            r#"{"animation":{}}"#,
        );
    let cost = fixture.cost();
    assert_eq!(cost.size("minecraft:items"), "64x64 16384");
    // a 1024 + strip 16*64*4 = 4096 + missing 1024.
    assert_eq!(cost.atlas("minecraft:items").decoded, 6144);
    let items = cost.atlas("minecraft:items");
    let anim = items
        .largest
        .iter()
        .find(|row| row.id == "minecraft:item/anim")
        .unwrap();
    assert_eq!((anim.width, anim.height, anim.image_height), (16, 16, 64));
    // The strip is the heaviest sprite although its frame is no larger than a's.
    assert_eq!(items.largest_decoded[0].id, "minecraft:item/anim");
    assert_eq!(items.largest_decoded[0].decoded, 4096);
}

#[test]
fn metadata_of_a_pack_below_the_texture_does_not_apply() {
    let mut fixture = Fixture::new();
    fixture
        .client_png("assets/minecraft/textures/item/v.png", 16, 64)
        .client_mcmeta(
            "assets/minecraft/textures/item/v.png.mcmeta",
            r#"{"animation":{}}"#,
        );
    assert_eq!(
        fixture.cost().size("minecraft:items"),
        "64x32 8192",
        "the client's own strip is one 16 px frame"
    );
    // The pack's texture hides the client's .mcmeta: an 18x66 holder first, in a 32x128 slot.
    fixture.pack_png("assets/minecraft/textures/item/v.png", 16, 64);
    assert_eq!(fixture.cost().size("minecraft:items"), "32x128 16384");
    // A pack's own .mcmeta does reach a client texture beneath it.
    let mut meta = Fixture::new();
    meta.client_png("assets/minecraft/textures/item/v.png", 16, 64)
        .pack_file(
            "assets/minecraft/textures/item/v.png.mcmeta",
            r#"{"animation":{"height":32}}"#,
        );
    let cost = meta.cost();
    let v = cost
        .atlas("minecraft:items")
        .largest
        .iter()
        .find(|row| row.id == "minecraft:item/v")
        .unwrap();
    assert_eq!(v.height, 32);
}

#[test]
fn only_an_overlay_active_for_the_client_format_replaces_a_sprite() {
    let mut fixture = Fixture::new();
    fixture.pack_png("assets/minecraft/textures/item/b.png", 16, 16);
    assert_eq!(
        fixture.cost().size("minecraft:items"),
        "64x32 8192",
        "b and missing: two 18 px holders"
    );
    fixture
        .pack_png("on/assets/minecraft/textures/item/b.png", 31, 31)
        .pack_png("off/assets/minecraft/textures/item/b.png", 64, 64)
        .pack_file(
            "pack.mcmeta",
            r#"{"overlays":{"entries":[
                {"directory":"on","min_format":88,"max_format":88},
                {"directory":"off","min_format":89,"max_format":89}]}}"#,
        );
    let cost = fixture.cost();
    assert_eq!(
        cost.size("minecraft:items"),
        "64x64 16384",
        "26.2 is format 88.0: 'on' applies (31 px), 'off' does not (64 px)"
    );
    let b = &cost.atlas("minecraft:items").largest[0];
    assert_eq!(b.id, "minecraft:item/b");
    assert!(b.file.starts_with("on/"), "{}", b.file);
}

#[test]
fn mip_levels_follow_the_smallest_sprite_and_pad_the_holders() {
    // Four mip levels: padding 16, holders 48; x and missing stitch to 128x64, five levels
    // (8192 + 2048 + 512 + 128 + 32 px) * 4 bytes. Each 16 px sprite decodes 341 px over its chain.
    let mut fixture = Fixture::new();
    fixture.client_png("assets/minecraft/textures/block/x.png", 16, 16);
    let four = fixture.cost();
    assert_eq!(four.size("minecraft:blocks"), "128x64 43648");
    assert_eq!(four.atlas("minecraft:blocks").decoded, 2 * 341 * 4);
    assert_eq!(four.atlas("minecraft:blocks").mip_level, 4);
    // A 20 px sprite has lowest set bit 4: level 2, padding 4, holders 28 and 24 -> 64x32, three levels
    // (2048 + 512 + 128 px) * 4 bytes. Decoded: 20 px chain 525 px + missing 336 px.
    let mut fixture = Fixture::new();
    fixture.client_png("assets/minecraft/textures/block/odd.png", 20, 20);
    let two = fixture.cost();
    assert_eq!(two.size("minecraft:blocks"), "64x32 10752");
    assert_eq!(two.atlas("minecraft:blocks").decoded, 861 * 4);
    assert_eq!(two.atlas("minecraft:blocks").mip_level, 2);
}

#[test]
fn sources_stack_from_the_client_up_and_a_filter_removes_what_came_before() {
    let mut fixture = Fixture::new();
    fixture
        .client_png("assets/minecraft/textures/item/keep.png", 16, 16)
        .client_png("assets/minecraft/textures/item/hidden_a.png", 16, 16)
        .pack_png("assets/minecraft/textures/extra/one.png", 16, 16)
        .pack_png("assets/minecraft/textures/sheet.png", 32, 32)
        .pack_png("assets/minecraft/textures/base.png", 16, 16)
        .pack_png("assets/minecraft/textures/key.png", 2, 2)
        .pack_png("assets/minecraft/textures/red.png", 2, 2)
        .pack_file(
            "assets/minecraft/atlases/items.json",
            r#"{"sources":[
                {"type":"filter","pattern":{"path":"^item/hidden"}},
                {"type":"single","resource":"minecraft:extra/one","sprite":"minecraft:item/renamed"},
                {"type":"unstitch","resource":"minecraft:sheet","regions":[
                    {"sprite":"minecraft:item/half","x":0,"y":0,"width":0.5,"height":0.5}]},
                {"type":"paletted_permutations","textures":["minecraft:base"],"palette_key":"minecraft:key",
                 "permutations":{"red":"minecraft:red"}}]}"#,
        );
    let cost = fixture.cost();
    // missingno, keep, renamed, half and base_red: five 18 px holders (hidden_a is gone), which fit 64x64.
    assert_eq!(cost.size("minecraft:items"), "64x64 16384");
    let items = cost.atlas("minecraft:items");
    assert_eq!(items.sprites, 5);
    assert_eq!(
        items.pack_sprites, 3,
        "renamed, half and base_red come from the pack; keep and missingno from the client"
    );
    let ids: Vec<_> = items.largest.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "minecraft:base_red",
            "minecraft:item/half",
            "minecraft:item/keep",
            "minecraft:item/renamed",
            "minecraft:missingno"
        ]
    );
    assert!(cost.complete(), "{:?}", cost.exclusions);
}

#[test]
fn palettes_of_the_newest_client_live_in_their_own_directory() {
    for (directory, resolved) in [("palettes/", true), ("", false)] {
        let mut fixture = Fixture::new();
        fixture.format = "97.1";
        fixture
            .pack_png("assets/minecraft/textures/base.png", 16, 16)
            .pack_file(
                "assets/minecraft/atlases/items.json",
                r#"{"sources":[{"type":"paletted_permutations","textures":["minecraft:base"],
                    "palette_key":"minecraft:key","permutations":{"red":"minecraft:red"}}]}"#,
            )
            .client_png(
                &format!("assets/minecraft/textures/{directory}key.png"),
                2,
                2,
            )
            .client_png(
                &format!("assets/minecraft/textures/{directory}red.png"),
                2,
                2,
            );
        // The jar's own items atlas is stacked underneath and stitches no sprite here.
        fixture
            .atlases
            .remove("assets/minecraft/atlases/blocks.json");
        let cost = fixture.cost();
        // 26.3 finds the palettes only under textures/palettes; the older directory leaves the sprite unresolved.
        assert_eq!(
            cost.complete(),
            resolved,
            "{directory}: {:?}",
            cost.exclusions
        );
        assert_eq!(
            cost.atlas("minecraft:items").sprites,
            if resolved { 2 } else { 1 }
        );
    }
}

#[test]
fn a_pack_filter_hides_the_client_textures_and_definitions_below_it() {
    let mut fixture = Fixture::new();
    fixture.client_png("assets/minecraft/textures/item/vanilla.png", 16, 16);
    assert_eq!(
        fixture.cost().size("minecraft:items"),
        "64x32 8192",
        "without a filter vanilla and missing stitch to 64x32"
    );
    for path in ["^textures/item/vanilla", r"^atlases/items\\.json$"] {
        fixture.pack_file(
            "pack.mcmeta",
            &format!(r#"{{"filter":{{"block":[{{"namespace":"minecraft","path":"{path}"}}]}}}}"#),
        );
        let cost = fixture.cost();
        // The client still stitches items from its own list: only the missing sprite is left.
        assert_eq!(cost.size("minecraft:items"), "32x32 4096", "{path}");
        assert_eq!(cost.atlas("minecraft:items").sprites, 1, "{path}");
        assert!(cost.complete());
    }
}

/// Client sprites `(name, width, height)` on the items atlas, with the missing sprite (16x16) added by the stitch.
fn items_of(sprites: &[(&str, u32, u32)]) -> String {
    let mut fixture = Fixture::new();
    for &(name, width, height) in sprites {
        fixture.client_png(
            &format!("assets/minecraft/textures/item/{name}.png"),
            width,
            height,
        );
    }
    fixture.cost().size("minecraft:items")
}

#[test]
fn holders_of_one_height_stitch_widest_first() {
    // Heights tie, so the widths order the holders: 33x33, 22x33, 6x17, 6x13, 18x18. Taken in another order the
    // atlas grows to 128x64.
    let sprites = [("s0", 20, 31), ("s1", 31, 31), ("s2", 4, 12), ("s3", 4, 16)];
    assert_eq!(items_of(&sprites), "64x64 16384");
}

#[test]
fn a_region_splits_toward_the_larger_spare_side() {
    // Splitting the other way fits 64x32.
    let sprites = [("s0", 16, 28), ("s1", 8, 16), ("s2", 28, 4)];
    assert_eq!(items_of(&sprites), "64x64 16384");
}

#[test]
fn a_filter_hides_a_path_when_some_entry_matches_its_namespace_and_some_entry_its_path() {
    let mut fixture = Fixture::new();
    fixture.client_png("assets/minecraft/textures/item/vanilla.png", 16, 16);
    let filter = |entries: &str| format!(r#"{{"filter":{{"block":[{entries}]}}}}"#);
    // Neither entry pairs the namespace with the path, yet the client hides the texture.
    fixture.pack_file(
        "pack.mcmeta",
        &filter(
            r#"{"namespace":"other","path":"^textures/item/vanilla"},{"namespace":"minecraft","path":"^nothing"}"#,
        ),
    );
    assert_eq!(fixture.cost().atlas("minecraft:items").sprites, 1);
    // A path matching nothing hides nothing, whatever the namespace.
    fixture.pack_file(
        "pack.mcmeta",
        &filter(r#"{"namespace":"minecraft","path":"^nothing"}"#),
    );
    assert_eq!(fixture.cost().atlas("minecraft:items").sprites, 2);
}

#[test]
fn a_texture_section_the_client_rejects_drops_the_sprite() {
    let mut fixture = Fixture::new();
    fixture
        .pack_png("assets/minecraft/textures/item/s.png", 16, 16)
        .pack_file(
            "assets/minecraft/textures/item/s.png.mcmeta",
            r#"{"texture":{"blur":"invalid"}}"#,
        );
    let rejected = fixture.cost();
    assert!(!rejected.complete());
    assert!(
        rejected.exclusions[0].contains("item/s.png.mcmeta.texture.blur: "),
        "{:?}",
        rejected.exclusions
    );
    assert_eq!(
        rejected.atlas("minecraft:items").sprites,
        1,
        "only missingno: the rejected sprite is not counted"
    );
    fixture.pack_file(
        "assets/minecraft/textures/item/s.png.mcmeta",
        r#"{"texture":{"blur":false,"mipmap_strategy":"cutout","alpha_cutoff_bias":0.1}}"#,
    );
    let accepted = fixture.cost();
    assert!(accepted.complete());
    assert_eq!(accepted.atlas("minecraft:items").sprites, 2);
    fixture.pack_file(
        "assets/minecraft/textures/item/s.png.mcmeta",
        r#"{"texture":{"mipmap_strategy":"nope"}}"#,
    );
    assert!(!fixture.cost().complete());
}

#[test]
fn animation_numbers_are_read_as_jacksons_number_nodes_are() {
    // `intValueExact`: a whole double passes, a fraction or anything past an int does not.
    let animation = |section: &str| {
        let mut fixture = Fixture::new();
        fixture
            .pack_png("assets/minecraft/textures/item/s.png", 16, 16)
            .pack_file(
                "assets/minecraft/textures/item/s.png.mcmeta",
                &format!(r#"{{"animation":{section}}}"#),
            );
        fixture.cost()
    };
    for accepted in [
        r#"{"width":8.0,"height":8}"#,
        r#"{"frametime":2e0,"interpolate":1}"#,
        r#"{"interpolate":true}"#,
    ] {
        let cost = animation(accepted);
        assert!(cost.complete(), "{accepted}: {:?}", cost.exclusions);
    }
    for (rejected, why) in [
        (r#"{"width":8.5}"#, "width: expected a 32-bit integer"),
        (
            r#"{"width":3000000000}"#,
            "width: expected a 32-bit integer",
        ),
        (
            r#"{"height":99999999999999999999}"#,
            "height: expected a 32-bit integer",
        ),
        (r#"{"width":"8"}"#, "width: expected a number"),
        (
            r#"{"interpolate":"yes"}"#,
            "interpolate: expected a boolean",
        ),
    ] {
        let cost = animation(rejected);
        assert!(
            cost.exclusions.iter().any(|line| line.contains(why)),
            "{rejected}: {:?}",
            cost.exclusions
        );
    }
}

#[test]
fn the_gui_section_is_read_only_for_the_gui_atlas() {
    let nine_slice = |border: u32| {
        format!(
            r#"{{"gui":{{"scaling":{{"type":"nine_slice","width":10,"height":10,"border":{border}}}}}}}"#
        )
    };
    let mut fixture = Fixture::new();
    fixture
        .client_atlas("gui", &atlas("gui/sprites", ""))
        .pack_png("assets/minecraft/textures/gui/sprites/frame.png", 16, 16)
        .pack_png("assets/minecraft/textures/item/i.png", 16, 16)
        .pack_file(
            "assets/minecraft/textures/gui/sprites/frame.png.mcmeta",
            &nine_slice(5),
        )
        .pack_file(
            "assets/minecraft/textures/item/i.png.mcmeta",
            &nine_slice(5),
        );
    // Borders of 5 leave no centre slice in a 10 px nine-slice: the client rejects the gui sprite, not the item.
    let rejected = fixture.cost();
    assert!(!rejected.complete());
    assert_eq!(rejected.atlas("minecraft:gui").sprites, 1);
    assert_eq!(rejected.atlas("minecraft:items").sprites, 2);
    fixture.pack_file(
        "assets/minecraft/textures/gui/sprites/frame.png.mcmeta",
        &nine_slice(4),
    );
    let accepted = fixture.cost();
    assert!(accepted.complete());
    assert_eq!(accepted.atlas("minecraft:gui").sprites, 2);
}

#[test]
fn a_truncated_png_is_not_certified() {
    let mut fixture = Fixture::new();
    // Signature, the IHDR chunk's length and type, width and height: nothing else of the file.
    let whole = png_bytes(64, 64);
    fixture.pack.insert(
        "assets/minecraft/textures/item/cut.png".into(),
        whole[..24].to_vec(),
    );
    let cost = fixture.cost();
    assert!(!cost.complete());
    assert!(
        cost.exclusions[0].contains("cut.png"),
        "{:?}",
        cost.exclusions
    );
    assert_eq!(cost.atlas("minecraft:items").sprites, 1);
}

#[test]
fn an_unsupported_source_makes_the_cost_incomplete() {
    let mut fixture = Fixture::new();
    fixture.pack_file(
        "assets/minecraft/atlases/items.json",
        r#"{"sources":[{"type":"minecraft:future"}]}"#,
    );
    let cost = fixture.cost();
    assert!(!cost.complete());
    assert!(cost.exclusions[0].contains("unsupported sprite source minecraft:future"));
    assert_eq!(
        cost.size("minecraft:items"),
        "32x32 4096",
        "what it resolved is still listed"
    );
}

#[test]
fn sprites_that_fit_no_texture_leave_the_atlas_out_and_say_so() {
    let mut fixture = Fixture::new();
    fixture.client_png("assets/minecraft/textures/item/huge.png", 20000, 20000);
    let cost = fixture.cost();
    assert!(cost.atlases.iter().all(|a| a.id != "minecraft:items"));
    assert!(
        cost.exclusions[0].contains("fit no 16384 px texture"),
        "{:?}",
        cost.exclusions
    );
}

#[test]
fn the_gpu_total_counts_every_atlas_with_its_mipmaps() {
    // items: missing alone, 32x32 -> 4096 bytes. blocks: missing alone, holder 48, 64x64 over five levels
    // (4096 + 1024 + 256 + 64 + 16 px) * 4 = 21824 bytes. Total 25920 bytes.
    let cost = Fixture::new().cost();
    assert_eq!(cost.atlas_gpu_bytes(), 25920);
    assert_eq!(cost.atlas_decoded_bytes(), 1024 + 341 * 4);
}

const FONT: &str = "assets/minecraft/font/default.json";

fn font_json(providers: &str) -> String {
    format!(r#"{{"providers":[{providers}]}}"#)
}

fn bitmap(file: &str, rows: &str) -> String {
    format!(r#"{{"type":"bitmap","file":"minecraft:{file}","ascent":7,"chars":{rows}}}"#)
}

#[test]
fn a_font_costs_its_sheet_twice_and_its_glyph_pages() {
    // A 16x16 sheet of four 8x8 cells: all five glyphs (the two built-in ones, 5x8, and the four cells) fit
    // the first 256x256 page. 1024 decoded + 1024 staging + one page of 262144.
    let mut fixture = Fixture::new();
    fixture
        .pack_png("assets/minecraft/textures/font/a.png", 16, 16)
        .pack_file(FONT, &font_json(&bitmap("font/a.png", r#"["ab","cd"]"#)));
    let cost = fixture.cost();
    let font = cost.font("minecraft:default");
    assert_eq!((font.providers, font.glyphs, font.pages), (1, 4, 1));
    assert_eq!(
        (font.decoded, font.staging, font.once_drawn),
        (1024, 1024, 2048)
    );
    assert_eq!(font.total, 2048 + 262_144);
    assert_eq!(cost.font_bytes(), 264_192);
    assert!(cost.complete());
}

#[test]
fn a_client_before_26_2_keeps_no_staging_copy() {
    let mut fixture = Fixture::new();
    fixture.format = "75.0";
    fixture
        .pack_png("assets/minecraft/textures/font/a.png", 16, 16)
        .pack_file(FONT, &font_json(&bitmap("font/a.png", r#"["ab","cd"]"#)));
    let font = fixture.cost().font("minecraft:default").clone();
    assert_eq!(
        (font.decoded, font.staging, font.total),
        (1024, 0, 1024 + 262_144)
    );
}

#[test]
fn cells_of_a_whole_page_each_open_a_page() {
    // 256x256 cells fit no page that already holds the built-in glyphs: each opens its own, plus the first.
    let mut fixture = Fixture::new();
    fixture
        .pack_png("assets/minecraft/textures/font/big.png", 512, 256)
        .pack_file(FONT, &font_json(&bitmap("font/big.png", r#"["ab"]"#)));
    let font = fixture.cost().font("minecraft:default").clone();
    assert_eq!((font.glyphs, font.pages, font.oversized_glyphs), (2, 3, 0));
    assert_eq!(font.total, 2 * 4 * 512 * 256 + 3 * 262_144);
}

#[test]
fn a_glyph_and_its_gutter_must_fit_the_page() {
    // The built-in glyphs take a 5 px column and a 1 px gutter, leaving 250 px: a 251 px cell opens a second page.
    let mut fixture = Fixture::new();
    fixture
        .pack_png("assets/minecraft/textures/font/wide.png", 251, 8)
        .pack_file(FONT, &font_json(&bitmap("font/wide.png", r#"["a"]"#)));
    assert_eq!(fixture.cost().font("minecraft:default").pages, 2);
}

#[test]
fn a_cell_over_256_texels_retains_its_page_and_is_never_uploaded() {
    let mut fixture = Fixture::new();
    fixture
        .pack_png("assets/minecraft/textures/font/huge.png", 514, 257)
        .pack_file(FONT, &font_json(&bitmap("font/huge.png", r#"["ab"]"#)));
    let font = fixture.cost().font("minecraft:default").clone();
    // Each failed insertion still opens a page; nothing uploaded, so no staging copy.
    assert_eq!(
        (font.glyphs, font.pages, font.oversized_glyphs, font.staging),
        (2, 3, 2, 0)
    );
}

#[test]
fn a_sheet_loaded_by_two_providers_is_decoded_twice_and_reported() {
    let rows = bitmap("font/a.png", r#"["ab"]"#);
    let again = bitmap("font/a.png", r#"["cd"]"#);
    let mut fixture = Fixture::new();
    fixture
        .pack_png("assets/minecraft/textures/font/a.png", 16, 8)
        .pack_file(FONT, &font_json(&format!("{rows},{again}")));
    let cost = fixture.cost();
    assert_eq!(cost.font("minecraft:default").decoded, 2 * 4 * 16 * 8);
    let [duplicate] = cost.duplicates.as_slice() else {
        panic!("{:?}", cost.duplicates);
    };
    assert_eq!(
        (duplicate.providers, duplicate.redundant_decoded),
        (2, 4 * 16 * 8)
    );
    assert_eq!(cost.redundant_decoded_bytes(), 512);
}

#[test]
fn a_reference_splices_the_other_font_and_each_font_keeps_its_own_atlas() {
    let mut fixture = Fixture::new();
    fixture
        .pack_png("assets/minecraft/textures/font/a.png", 16, 8)
        .pack_file(
            "assets/minecraft/font/other.json",
            &font_json(&bitmap("font/a.png", r#"["ab"]"#)),
        )
        .pack_file(
            FONT,
            &font_json(r#"{"type":"reference","id":"minecraft:other"}"#),
        );
    let cost = fixture.cost();
    let (default, other) = (cost.font("minecraft:default"), cost.font("minecraft:other"));
    // `default` draws `other`'s glyphs into its own atlas; the sheet and its staging copy belong to `other`.
    assert_eq!(
        (
            default.glyphs,
            default.pages,
            default.decoded,
            default.staging
        ),
        (2, 1, 0, 0)
    );
    assert_eq!(
        (other.glyphs, other.pages, other.decoded, other.staging),
        (2, 1, 512, 512)
    );
    assert!(cost.complete());
}

#[test]
fn a_space_provider_shadows_a_later_bitmap_codepoint() {
    // `a` belongs to the space provider declared first, so only `b` of the bitmap is drawn.
    let mut fixture = Fixture::new();
    fixture
        .pack_png("assets/minecraft/textures/font/a.png", 16, 8)
        .pack_file(
            FONT,
            &font_json(&format!(
                r#"{{"type":"space","advances":{{"a":4}}}},{}"#,
                bitmap("font/a.png", r#"["ab"]"#)
            )),
        );
    let font = fixture.cost().font("minecraft:default").clone();
    assert_eq!((font.glyphs, font.decoded, font.staging), (1, 512, 512));
}

#[test]
fn a_disabled_provider_is_decoded_but_never_drawn() {
    // `uniform: true` asks for an option that is off: the client skips the provider but still loads its sheet.
    let disabled = r#"{"type":"bitmap","file":"minecraft:font/a.png","ascent":7,"chars":["ab"],"filter":{"uniform":true}}"#;
    let mut fixture = Fixture::new();
    fixture
        .pack_png("assets/minecraft/textures/font/a.png", 16, 8)
        .pack_file(FONT, &font_json(disabled));
    let font = fixture.cost().font("minecraft:default").clone();
    assert_eq!(
        (font.providers, font.glyphs, font.decoded, font.staging),
        (1, 0, 512, 0)
    );
}

#[test]
fn an_external_reference_and_a_ttf_provider_make_the_cost_incomplete() {
    let mut fixture = Fixture::new();
    fixture.pack_file(
        FONT,
        &font_json(
            r#"{"type":"reference","id":"minecraft:include/space"},
               {"type":"ttf","file":"minecraft:f.ttf","size":11,"oversample":2}"#,
        ),
    );
    let cost = fixture.cost();
    assert_eq!(cost.exclusions.len(), 2, "{:?}", cost.exclusions);
    assert_eq!(cost.font("minecraft:default").pages, 1);
}

#[test]
fn a_sheet_the_pack_lacks_is_listed_and_not_modelled() {
    let mut fixture = Fixture::new();
    fixture.pack_file(FONT, &font_json(&bitmap("font/gone.png", r#"["a"]"#)));
    let cost = fixture.cost();
    assert!(
        cost.exclusions[0].contains("missing font texture"),
        "{:?}",
        cost.exclusions
    );
    assert_eq!(cost.font("minecraft:default").providers, 0);
}

#[test]
fn a_malformed_font_is_an_error() {
    let mut fixture = Fixture::new();
    fixture.pack_file(FONT, r#"{"providers":[{"type":"nope"}]}"#);
    let error = fixture.try_cost().unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unknown font provider type nope"),
        "{error}"
    );
}

#[test]
fn a_font_the_client_reads_leniently_may_carry_comments() {
    let font = format!(
        "// the hud\n{{\"providers\": [ /* sheet */ {} ]}}",
        bitmap("font/a.png", r#"["ab","cd"]"#)
    );
    let mut fixture = Fixture::new();
    fixture
        .pack_png("assets/minecraft/textures/font/a.png", 16, 16)
        .pack_file(FONT, &font);
    let cost = fixture.cost();
    assert_eq!(cost.font("minecraft:default").glyphs, 4);
    assert!(cost.complete());
}

#[test]
fn a_font_the_client_reads_leniently_may_use_unquoted_names_and_single_quotes() {
    let font = "{providers: [{type: bitmap, file: 'minecraft:font/a.png', ascent: 7, chars: ['ab', 'cd']}]}";
    let mut fixture = Fixture::new();
    fixture
        .pack_png("assets/minecraft/textures/font/a.png", 16, 16)
        .pack_file(FONT, font);
    let cost = fixture.cost();
    assert_eq!(cost.font("minecraft:default").glyphs, 4);
    assert!(cost.complete());
}

#[test]
fn a_comma_before_the_end_of_the_providers_is_a_null_provider_as_gson_reads_it() {
    // Gson lenient turns `[x,]` into `[x, null]`, so the second provider is not an object.
    let font = format!(
        r#"{{"providers":[{},]}}"#,
        bitmap("font/a.png", r#"["ab","cd"]"#)
    );
    let mut fixture = Fixture::new();
    fixture
        .pack_png("assets/minecraft/textures/font/a.png", 16, 16)
        .pack_file(FONT, &font);
    let error = fixture.try_cost().unwrap_err();
    assert!(error.to_string().contains("providers[1]"), "{error}");
}

#[test]
fn an_atlas_definition_is_read_as_strictly_as_the_client_does() {
    let mut fixture = Fixture::new();
    fixture.pack_file(
        "assets/minecraft/atlases/items.json",
        r#"{"sources":[] /* the client refuses this */}"#,
    );
    let error = fixture.try_cost().unwrap_err();
    assert!(error.to_string().contains("invalid JSON"), "{error}");
}

#[test]
fn a_pattern_the_java_subset_refuses_is_an_exclusion_that_matches_nothing() {
    // `(?=x)` is lookaround, which `JavaPattern` refuses: the filter hides nothing and the cost says so.
    let mut fixture = Fixture::new();
    fixture
        .client_png("assets/minecraft/textures/item/vanilla.png", 16, 16)
        .pack_file(
            "pack.mcmeta",
            r#"{"filter":{"block":[{"namespace":"minecraft","path":"(?=textures)"}]}}"#,
        )
        .pack_file(
            "assets/minecraft/atlases/items.json",
            r#"{"sources":[{"type":"filter","pattern":{"path":"a++"}}]}"#,
        );
    let cost = fixture.cost();
    assert!(!cost.complete());
    assert_eq!(cost.exclusions.len(), 2, "{:?}", cost.exclusions);
    assert!(cost.exclusions[0].contains("filter.block[0]"));
    assert_eq!(
        cost.atlas("minecraft:items").sprites,
        2,
        "nothing was hidden or removed"
    );
}

const OVERLAID_META: &str = r#"{"pack":{"supported_formats":[88,88]},
    "overlays":{"entries":[{"formats":[88,88],"directory":"ov"}]}}"#;

#[test]
fn an_overlay_replaces_the_atlas_definition_of_the_root_it_does_not_add_to_it() {
    // The client's `CompositePackResources.getResource` returns the topmost overlay's file, so one pack gives
    // `getResourceStack` one copy: the root's extra source is never read.
    let mut fixture = Fixture::new();
    fixture
        .pack_file("pack.mcmeta", OVERLAID_META)
        .client_png("assets/minecraft/textures/item/a.png", 16, 16)
        .pack_png("assets/minecraft/textures/extra/one.png", 16, 16)
        .pack_file(
            "assets/minecraft/atlases/items.json",
            r#"{"sources":[{"type":"single","resource":"minecraft:extra/one","sprite":"minecraft:item/one"}]}"#,
        )
        .pack_file("ov/assets/minecraft/atlases/items.json", &atlas("item", "item/"));
    let cost = fixture.cost();
    assert_eq!(cost.atlas("minecraft:items").sprites, 2, "missingno and a");
}

#[test]
fn an_overlay_replaces_the_font_definition_of_the_root_it_does_not_add_to_it() {
    let mut fixture = Fixture::new();
    fixture
        .pack_file("pack.mcmeta", OVERLAID_META)
        .pack_png("assets/minecraft/textures/font/a.png", 16, 16)
        .pack_png("assets/minecraft/textures/font/b.png", 16, 16)
        .pack_file(FONT, &font_json(&bitmap("font/a.png", r#"["ab","cd"]"#)))
        .pack_file(
            "ov/assets/minecraft/font/default.json",
            &font_json(&bitmap("font/b.png", r#"["ef"]"#)),
        );
    let cost = fixture.cost();
    assert_eq!(
        cost.font("minecraft:default").glyphs,
        2,
        "only the overlay's"
    );
}
