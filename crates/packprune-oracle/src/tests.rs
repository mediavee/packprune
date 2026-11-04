//! Oracle and remap cases over an in-memory pack and vanilla client, then planted faults that must come out as
//! located differences.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use packprune_codec::{audio, png};
use packprune_graph::ReferenceIndex;
use packprune_pack::{
    Blob, Digest, Entry, Layout, Limits, LogicalPath, PackInput, PackMeta, PhysPath, ResourceId,
    RunCx, VanillaIndex,
};

use super::*;

const ITEM_MODEL: &str = "assets/fx/models/item/box.json";
const PARENT_MODEL: &str = "assets/fx/models/item/base.json";
const TEXTURE_A: &str = "assets/fx/textures/item/a.png";
const TEXTURE_B: &str = "assets/fx/textures/item/b.png";
const FONT: &str = "assets/fx/font/hud.json";
const ASCII: &str = "assets/minecraft/textures/font/ascii.png";

type Files = BTreeMap<String, Vec<u8>>;

/// A minimal vanilla client: its atlases, and the other files by name only, all empty but `paper.json`.
fn client(extra: &Files) -> Files {
    let mut files = Files::new();
    let directory = |source: &str, prefix: &str| {
        format!(r#"{{"sources": [{{"type": "minecraft:directory", "source": "{source}", "prefix": "{prefix}"}}]}}"#).into_bytes()
    };
    for (atlas, source, prefix) in [
        ("blocks", "block", "block/"),
        ("chests", "entity/chest", "entity/chest/"),
        ("gui", "gui/sprites", ""),
        ("items", "item", "item/"),
        ("paintings", "painting", ""),
        ("particles", "particle", ""),
        ("shulker_boxes", "entity/shulker", "entity/shulker/"),
    ] {
        files.insert(
            format!("assets/minecraft/atlases/{atlas}.json"),
            directory(source, prefix),
        );
    }
    for empty in [
        "assets/minecraft/blockstates/stone.json",
        "assets/minecraft/blockstates/tripwire.json",
        "assets/minecraft/font/default.json",
        "assets/minecraft/items/red_mushroom.json",
        "assets/minecraft/items/stone.json",
        "assets/minecraft/models/block/cube_all.json",
        "assets/minecraft/models/item/generated.json",
        "assets/minecraft/shaders/core/entity.vsh",
        "assets/minecraft/shaders/core/rendertype_text.vsh",
        "assets/minecraft/textures/block/stone.png",
    ] {
        files.insert(empty.into(), Vec::new());
    }
    files.insert(
        "assets/minecraft/models/item/paper.json".into(),
        br#"{"parent": "minecraft:item/generated"}"#.to_vec(),
    );
    files.insert(
        "assets/minecraft/textures/gui/sprites/hud/hotbar.png".into(),
        b"png\n".to_vec(),
    );
    files.extend(
        extra
            .iter()
            .map(|(path, bytes)| (path.clone(), bytes.clone())),
    );
    files
}

fn vanilla_index(client: &Files) -> VanillaIndex {
    VanillaIndex {
        version: "26.2".into(),
        pack_format: "88.0".into(),
        names: client.keys().cloned().collect(),
        atlases: client
            .iter()
            .filter(|(path, _)| path.starts_with("assets/minecraft/atlases/"))
            .map(|(path, bytes)| (path.clone(), bytes.clone()))
            .collect(),
        ..VanillaIndex::default()
    }
}

struct Client(Files);

impl VanillaAssets for Client {
    fn bytes(&self, logical: &str) -> Option<Blob> {
        self.0.get(logical).map(|bytes| bytes.as_slice().into())
    }
}

fn text(files: &mut Files, path: &str, text: &str) {
    files.insert(path.into(), text.as_bytes().to_vec());
}

fn read(files: &Files, path: &str) -> String {
    String::from_utf8(files[path].clone()).expect("UTF-8")
}

/// A `width` by `height` PNG whose pixel `(x, y)` is `base + x + 4 * y` in ARGB: every texel differs.
fn png(width: u32, height: u32, base: u32) -> Vec<u8> {
    let mut rgba = Vec::new();
    for y in 0..height {
        for x in 0..width {
            rgba.extend(argb_to_rgba(base + x + 4 * y));
        }
    }
    encode(width, height, rgba)
}

fn argb_to_rgba(argb: u32) -> [u8; 4] {
    let [a, r, g, b] = argb.to_be_bytes();
    [r, g, b, a]
}

fn encode(width: u32, height: u32, rgba: Vec<u8>) -> Vec<u8> {
    png::encode(&png::Image {
        width,
        height,
        rgba,
    })
    .expect("encodes")
}

fn with_pixel(bytes: &[u8], x: u32, y: u32, argb: u32) -> Vec<u8> {
    let mut image = png::decode(bytes).expect("decodes");
    let at = ((y * image.width + x) * 4) as usize;
    image.rgba[at..at + 4].copy_from_slice(&argb_to_rgba(argb));
    encode(image.width, image.height, image.rgba)
}

/// The top-left `width` by `height` block of `bytes`.
fn cropped(bytes: &[u8], width: u32, height: u32) -> Vec<u8> {
    let image = png::decode(bytes).expect("decodes");
    let mut rgba = Vec::new();
    for y in 0..height {
        let start = ((y * image.width) * 4) as usize;
        rgba.extend_from_slice(&image.rgba[start..start + (width * 4) as usize]);
    }
    encode(width, height, rgba)
}

/// An item whose model samples part of a texture through a parent with a display transform,
/// a font, a sound event.
fn files() -> Files {
    let mut files = Files::new();
    text(
        &mut files,
        "pack.mcmeta",
        r#"{"pack": {"min_format": 88, "max_format": 88, "description": "t"}}"#,
    );
    text(
        &mut files,
        "assets/fx/items/box.json",
        r#"{"model": {"type": "minecraft:model", "model": "fx:item/box"}}"#,
    );
    text(
        &mut files,
        PARENT_MODEL,
        r#"{"display": {"gui": {"rotation": [30.0, 45.0, 0.0]}}}"#,
    );
    text(
        &mut files,
        ITEM_MODEL,
        r##"{"parent": "fx:item/base", "textures": {"a": "fx:item/a", "b": "fx:item/b"},
 "elements": [{"from": [0.0, 0.0, 0.0], "to": [16.0, 16.0, 16.0], "rotation": {"origin": [8.0, 8.0, 8.0], "axis": "y", "angle": 22.5},
   "faces": {"north": {"uv": [0.0, 0.0, 8.0, 8.0], "texture": "#a"}, "south": {"texture": "#b"}}}]}"##,
    );
    files.insert(TEXTURE_A.into(), png(4, 4, 0xFF00_0000));
    files.insert(TEXTURE_B.into(), png(4, 4, 0xFF80_8000));
    text(
        &mut files,
        FONT,
        r#"{"providers": [{"type": "space", "advances": {" ": 4.0}},
 {"type": "bitmap", "file": "fx:font/glyphs.png", "ascent": 7, "chars": [""]}]}"#,
    );
    files.insert(
        "assets/fx/textures/font/glyphs.png".into(),
        png(8, 8, 0xFFFF_FF00),
    );
    text(
        &mut files,
        "assets/fx/sounds.json",
        r#"{"boom": {"sounds": ["fx:boom"]}}"#,
    );
    files.insert(
        "assets/fx/sounds/boom.ogg".into(),
        b"not really vorbis".to_vec(),
    );
    text(
        &mut files,
        "assets/fx/lang/en_us.json",
        r#"{"item.fx.box": "Box 16.0"}"#,
    );
    files
}

fn pack(files: &Files) -> PackInput {
    PackInput {
        name: "test".into(),
        files: files
            .iter()
            .map(|(path, bytes)| {
                let entry = Entry {
                    digest: Digest(*blake3::hash(bytes).as_bytes()),
                    blob: bytes.as_slice().into(),
                };
                (PhysPath::new(path).expect("a valid path"), entry)
            })
            .collect(),
        digest: Digest([0; 32]),
    }
}

/// One comparison's result, one line per difference: `view <format> (<overlays>): <key> <detail>`.
struct Result {
    comparison: Comparison,
    lines: Vec<String>,
}

impl Result {
    fn same(&self) -> bool {
        self.comparison.differing_keys == 0
    }
}

fn compare_over(
    client_extra: &Files,
    before: &Files,
    after: &Files,
    remap: &Remap,
    named: bool,
) -> Result {
    let cx = RunCx::default();
    let client = client(client_extra);
    let vanilla = vanilla_index(&client);
    let (before_pack, after_pack) = (pack(before), pack(after));
    let layout = |pack: &PackInput| {
        let meta = PackMeta::read(pack).expect("a pack.mcmeta");
        Layout::new(pack, meta, &vanilla)
    };
    let (before_layout, after_layout) = (layout(&before_pack), layout(&after_pack));
    let index = ReferenceIndex::default();
    let input = |pack, layout| OracleInput {
        pack,
        layout,
        index: &index,
    };
    let comparison = compare_with(
        &cx,
        &input(&before_pack, &before_layout),
        &input(&after_pack, &after_layout),
        &vanilla,
        &Client(client),
        remap,
        &|_: &LogicalPath| named,
    );
    let labels = view_labels(&before_layout, &after_layout);
    let lines = comparison
        .differences
        .iter()
        .map(|difference| {
            let detail = if difference.detail.is_empty() {
                String::new()
            } else {
                format!(" {}", difference.detail)
            };
            format!(
                "view {}: {}{detail}",
                labels[difference.view.0 as usize], difference.key
            )
        })
        .collect();
    Result { comparison, lines }
}

fn compare_to(after: &Files) -> Result {
    compare_over(&Files::new(), &files(), after, &Remap::default(), false)
}

#[test]
fn the_same_pack_and_a_lossless_rewrite_of_it_are_the_same() {
    let mut rewritten = files();
    text(
        &mut rewritten,
        ITEM_MODEL,
        r##"{"elements":[{"faces":{"south":{"texture":"#b"},"north":{"texture":"#a","uv":[0,0,8,8]}},"to":[16,16,16],"from":[0,0,0],
   "rotation":{"angle":22.5,"axis":"Y","origin":[8,8,8]}}],
 "textures":{"b":"fx:item/b","a":"fx:item/a"},"parent":"fx:item/base"}"##,
    );
    text(
        &mut rewritten,
        "assets/fx/items/box.json",
        r#"{"model":{"model":"fx:item/box","type":"model"}}"#,
    );

    assert!(compare_to(&files()).same());
    let result = compare_to(&rewritten);

    assert!(result.same(), "{:?}", result.lines);
    assert!(result.comparison.keys > 0);
}

/// A shader can read the colour of a transparent texel, so RGB under alpha 0 is part of what a face and a glyph
/// show.
#[test]
fn rgb_under_alpha_0_differs() {
    const GLYPHS: &str = "assets/fx/textures/font/glyphs.png";
    let mut before = files();
    before.insert(
        TEXTURE_A.into(),
        with_pixel(&before[TEXTURE_A], 1, 1, 0x0012_3456),
    );
    before.insert(
        GLYPHS.into(),
        with_pixel(&before[GLYPHS], 0, 0, 0x0012_3456),
    );
    let mut after = before.clone();
    after.insert(
        TEXTURE_A.into(),
        with_pixel(&before[TEXTURE_A], 1, 1, 0x0000_0000),
    );
    after.insert(
        GLYPHS.into(),
        with_pixel(&before[GLYPHS], 0, 0, 0x0000_0000),
    );

    let result = compare_over(&Files::new(), &before, &after, &Remap::default(), false);

    assert_eq!(result.comparison.differing_keys, 2, "{:?}", result.lines);
}

/// A shader and a JSON text compare as their lossless minifiers prove them: comments and whitespace between tokens
/// are free, a token and a shader token's line are not.
#[test]
fn shaders_and_json_texts_compare_by_tokens() {
    const SHADER: &str = "assets/fx/shaders/core/glow.fsh";
    const LINES: &str = "assets/fx/shaders/include/lines.glsl";
    const CREDITS: &str = "assets/minecraft/texts/credits.json";
    let mut before = files();
    text(
        &mut before,
        SHADER,
        "#version 150\n// glow\nvoid main() {\n    gl_FragColor = vec4(1.0);\n}\n",
    );
    text(&mut before, LINES, "int a = 1;\n\nint b = __LINE__;\n");
    text(&mut before, CREDITS, "[ { \"section\": \"A B\" } ]\n");
    let mut minified = before.clone();
    text(
        &mut minified,
        SHADER,
        "#version 150\n\nvoid main(){\ngl_FragColor=vec4(1.0);\n}\n",
    );
    text(&mut minified, CREDITS, "[{\"section\":\"A B\"}]");
    let mut changed = minified.clone();
    text(
        &mut changed,
        SHADER,
        "#version 150\n\nvoid main(){\ngl_FragColor=vec4(0.5);\n}\n",
    );
    text(&mut changed, LINES, "int a = 1;\nint b = __LINE__;\n");
    text(&mut changed, CREDITS, "[{\"section\":\"AB\"}]");

    let same = compare_over(&Files::new(), &before, &minified, &Remap::default(), false);
    let differ = compare_over(&Files::new(), &before, &changed, &Remap::default(), false);

    assert!(same.same(), "{:?}", same.lines);
    assert_eq!(differ.comparison.differing_keys, 3, "{:?}", differ.lines);
}

/// The shader joined lines but kept its tokens: the `__LINE__` its import defines takes another value (6, then 2).
#[test]
fn a_shader_whose_import_reads_its_lines_differs_when_they_move() {
    const SHADER: &str = "assets/minecraft/shaders/core/position_color.fsh";
    let mut before = files();
    text(
        &mut before,
        SHADER,
        "#version 330 core\n#moj_import <line_number.glsl>\n\nout vec4 fragColor;\n\nvoid main() {\n    fragColor = vec4(float(LINE_NUMBER) / 255.0);\n}\n",
    );
    text(
        &mut before,
        "assets/minecraft/shaders/include/line_number.glsl",
        "#define LINE_NUMBER __LINE__\n",
    );
    let mut after = before.clone();
    text(
        &mut after,
        SHADER,
        "#version 330 core\n#moj_import <line_number.glsl>\nout vec4 fragColor;void main(){fragColor=vec4(float(LINE_NUMBER)/255.0);}",
    );

    let result = compare_over(&Files::new(), &before, &after, &Remap::default(), false);

    assert_eq!(result.comparison.differing_keys, 1, "{:?}", result.lines);
    assert!(result.lines[0].contains(SHADER), "{:?}", result.lines);
}

/// The client skips an empty import without numbering it: emptying a comment-only import renumbers the next one's
/// `__FILE__` (2, then 1), while every token keeps its line.
#[test]
fn a_comment_only_import_differs_from_an_empty_one() {
    const SHADER: &str = "assets/minecraft/shaders/core/position_color.fsh";
    const EMPTY: &str = "assets/minecraft/shaders/include/empty.glsl";
    const SOURCE: &str = "assets/minecraft/shaders/include/source.glsl";
    let mut before = files();
    text(
        &mut before,
        SHADER,
        "#version 330 core\n#moj_import <empty.glsl>\n#moj_import <source.glsl>\nout vec4 fragColor;\nvoid main() {\n    fragColor = vec4(float(IMPORT_ID) / 255.0);\n}\n",
    );
    text(
        &mut before,
        EMPTY,
        "/* intentionally empty\n   multiline include\n*/\n",
    );
    text(&mut before, SOURCE, "const int IMPORT_ID = __FILE__;\n");
    let mut after = before.clone();
    text(&mut after, EMPTY, "");

    let result = compare_over(&Files::new(), &before, &after, &Remap::default(), false);

    assert_eq!(result.comparison.differing_keys, 1, "{:?}", result.lines);
    assert!(result.lines[0].contains(EMPTY), "{:?}", result.lines);
}

/// A block comment open across the first LF holds the defines `injectDefines` inserts there: without it they are read
/// before `#version` and the shader no longer compiles.
#[test]
fn removing_the_comment_holding_the_defines_differs() {
    const SHADER: &str = "assets/minecraft/shaders/core/entity.fsh";
    let mut before = files();
    text(
        &mut before,
        SHADER,
        "/* header\n   header continued */\n#version 330 core\n#ifndef ALPHA_CUTOUT\n#define ALPHA_CUTOUT 0.25\n#endif\nout vec4 fragColor;\nvoid main() { fragColor = vec4(ALPHA_CUTOUT); }\n",
    );
    let mut after = before.clone();
    text(
        &mut after,
        SHADER,
        "\n\n#version 330 core\n#ifndef ALPHA_CUTOUT\n#define ALPHA_CUTOUT 0.25\n#endif\nout vec4 fragColor;\nvoid main(){fragColor=vec4(ALPHA_CUTOUT);}\n",
    );

    let result = compare_over(&Files::new(), &before, &after, &Remap::default(), false);

    assert_eq!(result.comparison.differing_keys, 1, "{:?}", result.lines);
}

#[test]
fn a_texel_the_face_samples_differs() {
    let mut after = files();
    // uv 0-8 of a 4-texel texture samples texels 0 and 1 in each direction.
    after.insert(
        TEXTURE_A.into(),
        with_pixel(&after[TEXTURE_A], 1, 1, 0xFFFF_0000),
    );

    let result = compare_to(&after);

    assert_eq!(result.comparison.differing_keys, 1, "{:?}", result.lines);
    let located: Vec<&str> = result
        .lines
        .iter()
        .map(|line| {
            let model = line.find("/model").expect("a model path");
            &line[..model + line[model..].find(':').expect("a detail")]
        })
        .collect();
    assert_eq!(
        located,
        ["view 88.0 (base): item fx:box /model/model/elements/0/faces/north/texture/texels"]
    );
}

#[test]
fn a_texel_no_face_samples_is_the_same_unless_its_sprite_is_named() {
    let mut after = files();
    after.insert(
        TEXTURE_A.into(),
        with_pixel(&after[TEXTURE_A], 3, 3, 0xFFFF_0000),
    );

    let drawn = compare_to(&after);
    let named = compare_over(&Files::new(), &files(), &after, &Remap::default(), true);

    assert!(drawn.same(), "{:?}", drawn.lines);
    // A named texture is read by path as well: its file and its sprite both differ.
    assert_eq!(named.comparison.differing_keys, 2, "{:?}", named.lines);
    assert!(
        named
            .lines
            .iter()
            .any(|line| line
                .starts_with("view 88.0 (base): sprite minecraft:items fx:item/a /texels")),
        "{:?}",
        named.lines
    );
    assert!(
        named
            .lines
            .iter()
            .any(|line| line.starts_with(&format!("view 88.0 (base): file {TEXTURE_A}"))),
        "{:?}",
        named.lines
    );
}

#[test]
fn a_swapped_face_texture_differs() {
    let mut after = files();
    let model = read(&after, ITEM_MODEL).replace(r##""texture": "#a""##, r##""texture": "#b""##);
    text(&mut after, ITEM_MODEL, &model);

    let result = compare_to(&after);

    assert_eq!(result.comparison.differing_keys, 1, "{:?}", result.lines);
    assert!(
        result
            .lines
            .iter()
            .all(|line| line.contains("item fx:box /model/model/elements/0/faces/north/texture/")),
        "{:?}",
        result.lines
    );
}

#[test]
fn a_changed_glyph_advance_differs() {
    let mut after = files();
    let font = read(&after, FONT).replace(r#"" ": 4.0"#, r#"" ": 5.0"#);
    text(&mut after, FONT, &font);

    let result = compare_to(&after);

    assert_eq!(result.comparison.differing_keys, 1, "{:?}", result.lines);
    assert_eq!(
        result.lines,
        ["view 88.0 (base): glyph fx:hud U+0020 /0/glyph/space: 4.0 -> 5.0"]
    );
}

#[test]
fn a_changed_bitmap_glyph_width_changes_its_advance() {
    let mut after = files();
    // The glyph's last column with alpha moves from 7 to 3: its advance shrinks.
    let mut sheet = after["assets/fx/textures/font/glyphs.png"].clone();
    for x in 4..8 {
        for y in 0..8 {
            sheet = with_pixel(&sheet, x, y, 0);
        }
    }
    after.insert("assets/fx/textures/font/glyphs.png".into(), sheet);

    let result = compare_to(&after);

    assert!(
        result
            .lines
            .iter()
            .any(|line| line == "view 88.0 (base): glyph fx:hud U+E000 /0/glyph/advance: 9 -> 5"),
        "{:?}",
        result.lines
    );
}

#[test]
fn a_changed_parent_display_differs() {
    let mut after = files();
    let parent = read(&after, PARENT_MODEL).replace("45.0", "46.0");
    text(&mut after, PARENT_MODEL, &parent);

    let result = compare_to(&after);

    assert_eq!(
        result.lines,
        ["view 88.0 (base): item fx:box /model/model/display/gui/rotation/1: 45.0 -> 46.0"]
    );
}

#[test]
fn a_texture_cropped_to_what_the_face_samples_with_its_uvs_rescaled_is_the_same() {
    let mut after = files();
    after.insert(TEXTURE_A.into(), cropped(&after[TEXTURE_A], 2, 2));
    let model = read(&after, ITEM_MODEL).replace("[0.0, 0.0, 8.0, 8.0]", "[0.0, 0.0, 16.0, 16.0]");
    text(&mut after, ITEM_MODEL, &model);

    let result = compare_to(&after);

    assert!(result.same(), "{:?}", result.lines);
}

#[test]
fn before_atlas_files_a_model_texture_is_stitched_into_the_blocks_atlas() {
    let mut before = Files::new();
    text(
        &mut before,
        "pack.mcmeta",
        r#"{"pack": {"pack_format": 88, "supported_formats": [9, 88], "description": "t"}, "overlays": {"entries": [{"directory": "modern", "formats": [12, 88]}]}}"#,
    );
    text(
        &mut before,
        "assets/fx/items/box.json",
        r#"{"model": {"type": "minecraft:model", "model": "fx:item/box"}}"#,
    );
    text(
        &mut before,
        "assets/fx/models/item/box.json",
        r##"{"textures": {"a": "fx:custom/a"},
 "elements": [{"from": [0, 0, 0], "to": [16, 16, 16], "faces": {"north": {"uv": [0, 0, 16, 16], "texture": "#a"}}}]}"##,
    );
    before.insert(
        "assets/fx/textures/custom/a.png".into(),
        png(4, 4, 0xFF00_0000),
    );
    // Only the formats with atlas files list the texture; format 9 stitches it because the model names it.
    text(
        &mut before,
        "modern/assets/minecraft/atlases/items.json",
        r#"{"sources": [{"type": "single", "resource": "fx:custom/a"}]}"#,
    );
    let mut after = before.clone();
    after.insert(
        "assets/fx/textures/custom/a.png".into(),
        with_pixel(
            &before["assets/fx/textures/custom/a.png"],
            2,
            2,
            0xFFFF_0000,
        ),
    );

    let result = compare_over(&Files::new(), &before, &after, &Remap::default(), false);

    assert!(
        result.lines.iter().any(|line| line.starts_with(
            "view 9.0 (base): item fx:box /model/model/elements/0/faces/north/texture/texels"
        )),
        "{:?}",
        result.lines
    );
}

#[test]
fn a_uv_locked_variant_samples_the_texels_its_rotation_moves_the_face_to() {
    // y 90 with uvlock moves the up face's [0, 0, 4, 4] to [12, 0, 16, 4]: texels 48-64 by 0-16 of 64.
    let mut before = Files::new();
    text(
        &mut before,
        "pack.mcmeta",
        r#"{"pack": {"min_format": 88, "max_format": 88, "description": "t"}}"#,
    );
    text(
        &mut before,
        "assets/minecraft/blockstates/stone.json",
        r#"{"variants": {"": {"model": "fx:block/crop", "y": 90, "uvlock": true}}}"#,
    );
    text(
        &mut before,
        "assets/fx/models/block/crop.json",
        r##"{"textures": {"skin": "fx:block/skin"},
 "elements": [{"from": [0, 0, 0], "to": [16, 16, 16], "faces": {"up": {"uv": [0, 0, 4, 4], "texture": "#skin"}}}]}"##,
    );
    before.insert(
        "assets/fx/textures/block/skin.png".into(),
        png(64, 64, 0xFF00_0000),
    );
    let mut after = before.clone();
    after.insert(
        "assets/fx/textures/block/skin.png".into(),
        cropped(&before["assets/fx/textures/block/skin.png"], 16, 16),
    );
    let model =
        read(&before, "assets/fx/models/block/crop.json").replace("[0, 0, 4, 4]", "[0, 0, 16, 16]");
    text(&mut after, "assets/fx/models/block/crop.json", &model);

    let result = compare_over(&Files::new(), &before, &after, &Remap::default(), false);

    assert!(
        result.lines.iter().any(|line| line
            .starts_with("view 88.0 (base): blockstate minecraft:stone ")
            && line.contains("/faces/up/texture/texels")),
        "{:?}",
        result.lines
    );
}

#[test]
fn a_moved_overlay_bound_is_compared_on_both_sides_of_it() {
    let mut before = Files::new();
    text(
        &mut before,
        "pack.mcmeta",
        r#"{"pack": {"pack_format": 88, "supported_formats": [46, 88], "description": "t"}, "overlays": {"entries": [{"directory": "layer", "formats": [55, 63]}]}}"#,
    );
    text(&mut before, "assets/fx/lang/en_us.json", r#"{"k": "BASE"}"#);
    text(
        &mut before,
        "layer/assets/fx/lang/en_us.json",
        r#"{"k": "OVERLAY"}"#,
    );
    let mut after = before.clone();
    let meta = read(&before, "pack.mcmeta").replace("[55, 63]", "[55, 56]");
    text(&mut after, "pack.mcmeta", &meta);

    let result = compare_over(&Files::new(), &before, &after, &Remap::default(), false);

    assert_eq!(
        result.lines,
        [r#"view 57.0 (layer -> base): file assets/fx/lang/en_us.json /k: "OVERLAY" -> "BASE""#]
    );
}

#[test]
fn a_vanilla_font_sheet_the_pack_overrides_is_compared() {
    let mut client = Files::new();
    text(
        &mut client,
        "assets/minecraft/font/default.json",
        r#"{"providers": [{"type": "reference", "id": "minecraft:include/default"}]}"#,
    );
    text(
        &mut client,
        "assets/minecraft/font/include/default.json",
        r#"{"providers": [{"type": "bitmap", "file": "minecraft:font/ascii.png", "ascent": 7, "chars": ["a"]}]}"#,
    );
    client.insert(ASCII.into(), png(8, 8, 0xFFFF_FF00));
    let mut before = files();
    text(
        &mut before,
        FONT,
        r#"{"providers": [{"type": "reference", "id": "minecraft:default"}]}"#,
    );
    before.insert(ASCII.into(), png(8, 8, 0xFFFF_FF00));
    let mut after = before.clone();
    after.insert(ASCII.into(), png(8, 8, 0xFFFF_0000));

    let result = compare_over(&client, &before, &after, &Remap::default(), false);

    assert!(
        result
            .lines
            .iter()
            .any(|line| line.starts_with(&format!("view 88.0 (base): file {ASCII} "))),
        "{:?}",
        result.lines
    );
}

#[test]
fn a_sheet_the_client_loads_by_path_is_compared_whole_whatever_its_animation() {
    let mut client = Files::new();
    text(
        &mut client,
        "assets/minecraft/font/default.json",
        r#"{"providers": [{"type": "bitmap", "file": "minecraft:font/ascii.png", "ascent": 7, "chars": ["a"]}]}"#,
    );
    client.insert(ASCII.into(), png(8, 16, 0xFF00_0000));
    let mut before = files();
    before.insert(ASCII.into(), png(8, 16, 0xFF00_0000));
    // A font sheet is read whole: an animation section names frames the client never slices it into.
    let meta = format!("{ASCII}.mcmeta");
    text(
        &mut before,
        &meta,
        r#"{"animation": {"width": 8, "height": 8, "frames": [0]}}"#,
    );
    let mut after = before.clone();
    let mut red = before[ASCII].clone();
    for y in 8..16 {
        for x in 0..8 {
            red = with_pixel(&red, x, y, 0xFFFF_0000);
        }
    }
    after.insert(ASCII.into(), red);
    let mut blurred = before.clone();
    text(
        &mut blurred,
        &meta,
        r#"{"animation": {"width": 8, "height": 8, "frames": [0]}, "texture": {"blur": true}}"#,
    );

    let result = compare_over(&client, &before, &after, &Remap::default(), false);
    let meta = compare_over(&client, &before, &blurred, &Remap::default(), false);

    assert!(
        result
            .lines
            .iter()
            .any(|line| line.starts_with(&format!("view 88.0 (base): file {ASCII} /texels"))),
        "{:?}",
        result.lines
    );
    assert!(
        meta.lines
            .iter()
            .any(|line| line.starts_with(&format!("view 88.0 (base): file {ASCII} /meta"))),
        "{:?}",
        meta.lines
    );
}

#[test]
fn a_vanilla_texture_the_client_loads_by_path_is_compared() {
    let cow = "assets/minecraft/textures/entity/cow/cow_temperate.png";
    let mut before = files();
    before.insert(cow.into(), png(4, 4, 0xFF00_0000));
    let mut after = before.clone();
    after.insert(cow.into(), with_pixel(&before[cow], 3, 3, 0xFFFF_0000));
    let client = Files::from([(cow.to_owned(), png(4, 4, 0xFF00_FF00))]);

    let result = compare_over(&client, &before, &after, &Remap::default(), false);

    assert_eq!(result.comparison.differing_keys, 1, "{:?}", result.lines);
    assert!(
        result.lines[0].starts_with(&format!("view 88.0 (base): file {cow} /texels")),
        "{:?}",
        result.lines
    );
}

fn sprite_remap(cx: &RunCx, from: &str, to: &str) -> BTreeMap<ResourceId, ResourceId> {
    let id = |text| ResourceId::parse(&cx.interner, text).expect("an id");
    BTreeMap::from([(id(from), id(to))])
}

fn logical(path: &str) -> LogicalPath {
    LogicalPath::new(path).expect("a valid path")
}

fn physical(path: &str) -> PhysPath {
    PhysPath::new(path).expect("a valid path")
}

/// `compare_over` with a remap built on the comparison's own interner.
fn compare_renamed(after: &Files, remap: impl Fn(&RunCx) -> Remap) -> Result {
    let cx = RunCx::default();
    let remap = remap(&cx);
    let client = client(&Files::new());
    let vanilla = vanilla_index(&client);
    let (before_pack, after_pack) = (pack(&files()), pack(after));
    let layout = |pack: &PackInput| {
        Layout::new(pack, PackMeta::read(pack).expect("a pack.mcmeta"), &vanilla)
    };
    let (before_layout, after_layout) = (layout(&before_pack), layout(&after_pack));
    let index = ReferenceIndex::default();
    let comparison = compare_with(
        &cx,
        &OracleInput {
            pack: &before_pack,
            layout: &before_layout,
            index: &index,
        },
        &OracleInput {
            pack: &after_pack,
            layout: &after_layout,
            index: &index,
        },
        &vanilla,
        &Client(client),
        &remap,
        &|_: &LogicalPath| true,
    );
    let labels = view_labels(&before_layout, &after_layout);
    let lines = comparison
        .differences
        .iter()
        .map(|d| {
            format!("view {}: {} {}", labels[d.view.0 as usize], d.key, d.detail)
                .trim_end()
                .to_owned()
        })
        .collect();
    Result { comparison, lines }
}

fn renamed_texture() -> Files {
    let mut files = files();
    let texture = files.remove(TEXTURE_A).expect("texture a");
    files.insert("assets/fx/textures/item/c.png".into(), texture);
    let model = read(&files, ITEM_MODEL).replace("fx:item/a", "fx:item/c");
    text(&mut files, ITEM_MODEL, &model);
    files
}

#[test]
fn a_renamed_sprite_is_compared_under_its_new_name() {
    let after = renamed_texture();
    let rename = |cx: &RunCx| {
        Remap::step(
            BTreeMap::from([(logical(TEXTURE_A), logical("assets/fx/textures/item/c.png"))]),
            BTreeMap::new(),
            sprite_remap(cx, "fx:item/a", "fx:item/c"),
        )
    };

    let renamed = compare_renamed(&after, rename);
    let unmapped = compare_renamed(&after, |_| Remap::default());

    assert!(renamed.same(), "{:?}", renamed.lines);
    assert!(
        unmapped
            .lines
            .iter()
            .any(|line| line == "view 88.0 (base): sprite minecraft:items fx:item/a only before"),
        "{:?}",
        unmapped.lines
    );
}

// ---- each planted fault is a located difference

#[test]
fn a_wrong_remap_is_a_located_difference() {
    let after = renamed_texture();
    // The sprite is said to have become b, which the output draws differently from what a was.
    let wrong = |cx: &RunCx| {
        Remap::step(
            BTreeMap::from([(logical(TEXTURE_A), logical("assets/fx/textures/item/c.png"))]),
            BTreeMap::new(),
            sprite_remap(cx, "fx:item/a", "fx:item/b"),
        )
    };

    let result = compare_renamed(&after, wrong);

    assert!(
        result
            .lines
            .iter()
            .any(|line| line.starts_with("view 88.0 (base): sprite minecraft:items fx:item/a (now sprite minecraft:items fx:item/b) /texels")),
        "{:?}",
        result.lines
    );
}

#[test]
fn a_dropped_sound_entry_is_a_located_difference() {
    let mut before = files();
    text(
        &mut before,
        "assets/fx/sounds.json",
        r#"{"boom": {"sounds": ["fx:boom", {"name": "fx:boom", "pitch": 2}]}}"#,
    );
    let mut after = before.clone();
    text(
        &mut after,
        "assets/fx/sounds.json",
        r#"{"boom": {"sounds": ["fx:boom"]}}"#,
    );

    let result = compare_over(&Files::new(), &before, &after, &Remap::default(), false);

    assert_eq!(result.comparison.differing_keys, 1, "{:?}", result.lines);
    assert_eq!(result.lines.len(), 1, "{:?}", result.lines);
    assert!(
        result.lines[0].starts_with("view 88.0 (base): sound fx:boom /sounds: [2 entries] "),
        "{:?}",
        result.lines
    );
}

/// A 0.2 s stereo tone: a different frequency per channel, so a channel change is heard.
fn tone() -> audio::Pcm {
    let wave = |hz: f32| -> Vec<f32> {
        (0..8820)
            .map(|i| (i as f32 * hz * std::f32::consts::TAU / 44_100.0).sin() * 0.5)
            .collect()
    };
    audio::Pcm {
        rate: 44_100,
        channels: vec![wave(440.0), wave(660.0)],
    }
}

fn ogg(pcm: &audio::Pcm) -> Vec<u8> {
    audio::encode(pcm, 0.3).expect("encodes")
}

/// `files()` with the boom sound replaced by `ogg`, streamed or loaded whole.
fn with_sound(ogg: &[u8], stream: bool) -> Files {
    let mut files = files();
    text(
        &mut files,
        "assets/fx/sounds.json",
        &format!(r#"{{"boom": {{"sounds": [{{"name": "fx:boom", "stream": {stream}}}]}}}}"#),
    );
    files.insert("assets/fx/sounds/boom.ogg".into(), ogg.to_vec());
    files
}

#[test]
fn a_lossless_ogg_rewrite_sounds_the_same_streamed_or_loaded() {
    let original = ogg(&tone());
    let optimised = audio::optimise(&original).expect("optimises");
    assert_ne!(original, optimised, "the optimiser changes the bytes");
    assert_eq!(
        audio::decode(&original).expect("decodes"),
        audio::decode(&optimised).expect("decodes")
    );

    for stream in [false, true] {
        // Named: the sound file is also compared whole, as a file the server may play by path.
        let result = compare_over(
            &Files::new(),
            &with_sound(&original, stream),
            &with_sound(&optimised, stream),
            &Remap::default(),
            true,
        );

        assert!(result.same(), "stream {stream}: {:?}", result.lines);
        assert!(result.comparison.keys > 0);
    }
}

#[test]
fn a_resampled_ogg_sounds_different() {
    let pcm = tone();
    let slower = audio::resample(&pcm, 22_050).expect("resamples");

    let result = compare_over(
        &Files::new(),
        &with_sound(&ogg(&pcm), false),
        &with_sound(&ogg(&slower), false),
        &Remap::default(),
        false,
    );

    assert_eq!(result.comparison.differing_keys, 1, "{:?}", result.lines);
    assert!(
        result
            .lines
            .iter()
            .any(|line| line.contains("/name/rate: 44100 -> 22050")),
        "{:?}",
        result.lines
    );
}

#[test]
fn an_ogg_with_fewer_channels_sounds_different() {
    let pcm = tone();
    let mono = audio::downmix(&pcm).expect("downmixes");

    let result = compare_over(
        &Files::new(),
        &with_sound(&ogg(&pcm), true),
        &with_sound(&ogg(&mono), true),
        &Remap::default(),
        false,
    );

    assert_eq!(result.comparison.differing_keys, 1, "{:?}", result.lines);
    assert!(
        result
            .lines
            .iter()
            .any(|line| line.contains("/name/channels: 2 -> 1")),
        "{:?}",
        result.lines
    );
}

#[test]
fn a_sound_the_client_cannot_decode_compares_by_its_bytes() {
    let junk = with_sound(b"OggS junk", false);
    let mut other = junk.clone();
    other.insert(
        "assets/fx/sounds/boom.ogg".into(),
        b"OggS other junk".to_vec(),
    );

    assert!(compare_over(&Files::new(), &junk, &junk, &Remap::default(), true).same());
    let result = compare_over(&Files::new(), &junk, &other, &Remap::default(), true);

    assert_eq!(result.comparison.differing_keys, 2, "{:?}", result.lines);
    assert!(
        result.lines.iter().all(|line| line.contains("/bytes:")),
        "{:?}",
        result.lines
    );
}

fn with_lang(json: &str) -> Files {
    let mut files = files();
    text(&mut files, "assets/fx/lang/en_us.json", json);
    files
}

#[test]
fn a_reordered_language_file_is_the_same_and_a_changed_value_differs() {
    let before = with_lang(r#"{"a": "1", "b": "2", "c": "3"}"#);
    let reordered = with_lang(r#"{ "c":"3", "a":"1", "b":"2" }"#);
    let changed = with_lang(r#"{"a": "1", "b": "changed", "c": "3"}"#);

    let same = compare_over(&Files::new(), &before, &reordered, &Remap::default(), false);
    let differs = compare_over(&Files::new(), &before, &changed, &Remap::default(), false);

    assert!(same.same(), "{:?}", same.lines);
    assert_eq!(differs.comparison.differing_keys, 1, "{:?}", differs.lines);
    assert!(
        differs.lines[0].contains("file assets/fx/lang/en_us.json /b:"),
        "{:?}",
        differs.lines
    );
}

#[test]
fn the_last_of_a_duplicate_language_key_wins() {
    let duplicated = with_lang(r#"{"a": "1", "b": "2", "a": "3"}"#);
    let deduplicated = with_lang(r#"{"b": "2", "a": "3"}"#);
    let first_wins = with_lang(r#"{"b": "2", "a": "1"}"#);

    assert!(
        compare_over(
            &Files::new(),
            &duplicated,
            &deduplicated,
            &Remap::default(),
            false
        )
        .same()
    );
    assert!(
        !compare_over(
            &Files::new(),
            &duplicated,
            &first_wins,
            &Remap::default(),
            false
        )
        .same()
    );
}

/// Whether the client's `Language.loadFromJson` makes the same map of `before` and `after`.
fn same_translations(before: &str, after: &str) -> bool {
    let (before, after) = (with_lang(before), with_lang(after));
    compare_over(&Files::new(), &before, &after, &Remap::default(), false).same()
}

#[test]
fn a_number_in_a_language_file_keeps_its_spelling() {
    // The client loads `k=1` and `k=1.0`.
    let (before, after) = (with_lang(r#"{"k":1}"#), with_lang(r#"{"k":1.0}"#));
    let result = compare_over(&Files::new(), &before, &after, &Remap::default(), false);

    assert_eq!(
        result.lines,
        [r#"view 88.0 (base): file assets/fx/lang/en_us.json /k: "1" -> "1.0""#]
    );
    assert!(!same_translations(r#"{"k":1e2}"#, r#"{"k":100}"#));
    assert!(same_translations(r#"{"k":1.0}"#, r#"{ "k" : 1.0 }"#));
}

#[test]
fn a_language_value_is_the_string_the_client_makes_of_it() {
    // `true` and "true" both load as `true`; the client rewrites `%d` and `%f` formats to `%s`.
    assert!(same_translations(r#"{"k":true}"#, r#"{"k":"true"}"#));
    assert!(same_translations(r#"{"k":"%d"}"#, r#"{"k":"%s"}"#));
    assert!(same_translations(r#"{"k":"%1$d"}"#, r#"{"k":"%1$s"}"#));
    assert!(same_translations(r#"{"k":"%.2f"}"#, r#"{"k":"%s"}"#));
    assert!(same_translations(r#"{"k":"100%d"}"#, r#"{"k":"100%s"}"#));
    assert!(!same_translations(r#"{"k":"%1$d"}"#, r#"{"k":"%2$d"}"#));
    assert!(!same_translations(r#"{"k":true}"#, r#"{"k":false}"#));
}

#[test]
fn a_language_file_loads_up_to_the_first_value_the_client_rejects() {
    // The client loads `{a=A}` from the first and `{a=A, b=B}` from the second, then fails on `bad`.
    assert!(!same_translations(
        r#"{"a":"A","bad":{},"b":"B"}"#,
        r#"{"a":"A","b":"B","bad":{}}"#
    ));
    // What follows the rejected value is not loaded, whatever it is.
    assert!(same_translations(
        r#"{"a":"A","bad":{},"b":"B"}"#,
        r#"{"a":"A","bad":null,"b":"C"}"#
    ));
    assert!(same_translations(
        r#"{"a":"A","bad":[],"b":"B"}"#,
        r#"{"a":"A","bad":{}}"#
    ));
    // A duplicate keeps its first place and its last value: `a` is rejected first, nothing loads.
    assert!(same_translations(r#"{"a":"A","b":"B","a":{}}"#, r#"{}"#));
    assert!(same_translations(
        r#"{"b":"B","a":{},"a":"A"}"#,
        r#"{"a":"A","b":"B"}"#
    ));
}

#[test]
fn a_lone_surrogate_name_is_not_merged_with_a_replacement_character() {
    // The client loads `{}` from the first (`\ud800` is rejected first) and `{\ufffd=X, b=B}` from the second.
    let lone = r#"{"\ud800":{},"b":"B","\ufffd":"X"}"#;
    let replacement = r#"{"\ufffd":"X","b":"B"}"#;

    assert!(!same_translations(lone, replacement));
    assert!(same_translations(lone, lone));
}

#[test]
fn a_language_file_is_read_with_the_clients_lenient_grammar() {
    // The client loads `{a=A, b=B}` and `{b=B, a=A}` from these, without error.
    assert!(same_translations(r#"{a:"A",b:"B"}"#, r#"{b:"B",a:"A"}"#));
    assert!(same_translations(r#"{'a':'A'}"#, r#"{"a":"A"}"#));
    assert!(!same_translations(r#"{a:"A"}"#, r#"{a:"B"}"#));
}

#[test]
fn only_the_first_fifty_differing_keys_are_located() {
    let mut before = files();
    for i in 0..60 {
        text(&mut before, &format!("assets/fx/lang/l{i:02}.json"), "{}");
    }
    let mut after = before.clone();
    for i in 0..60 {
        text(
            &mut after,
            &format!("assets/fx/lang/l{i:02}.json"),
            r#"{"k": "v"}"#,
        );
    }

    let result = compare_over(&Files::new(), &before, &after, &Remap::default(), false);

    assert_eq!(result.comparison.differing_keys, 60);
    let bare = result
        .comparison
        .differences
        .iter()
        .filter(|d| d.detail.is_empty())
        .count();
    assert_eq!((result.comparison.differences.len(), bare), (60, 10));
}

// ---- remaps

const M: &str = "assets/fx/models/item/m.json";
const N: &str = "assets/fx/models/item/n.json";

fn rename(from: &str, to: &str) -> Remap {
    Remap::step(
        BTreeMap::from([(logical(from), logical(to))]),
        BTreeMap::new(),
        BTreeMap::new(),
    )
}

fn moved(from: &str, to: &str) -> Remap {
    Remap::step(
        BTreeMap::new(),
        BTreeMap::from([(physical(from), physical(to))]),
        BTreeMap::new(),
    )
}

#[test]
fn a_rename_then_a_move_leads_to_the_final_physical_path() {
    let remap = rename(M, N).then(&moved(&format!("b/{N}"), &format!("c/{N}")));

    assert_eq!(
        remap.physical(&physical(&format!("b/{M}"))),
        physical(&format!("c/{N}"))
    );
    assert_eq!(
        remap.physical(&physical(&format!("a/{M}"))),
        physical(&format!("a/{N}"))
    );
}

#[test]
fn a_move_then_a_rename_leads_to_the_final_physical_path() {
    let remap = moved(&format!("b/{M}"), &format!("c/{M}")).then(&rename(M, N));

    assert_eq!(
        remap.physical(&physical(&format!("b/{M}"))),
        physical(&format!("c/{N}"))
    );
    assert_eq!(
        remap.physical(&physical(&format!("c/{M}"))),
        physical(&format!("c/{N}"))
    );
}

#[test]
fn a_move_reaches_its_target_without_the_rename_of_the_same_pass() {
    let remap = Remap::step(
        BTreeMap::from([(logical(M), logical(N))]),
        BTreeMap::from([(physical(&format!("b/{M}")), physical(&format!("c/{M}")))]),
        BTreeMap::new(),
    );

    assert_eq!(
        remap.physical(&physical(&format!("b/{M}"))),
        physical(&format!("c/{M}"))
    );
    assert_eq!(
        remap.physical(&physical(&format!("a/{M}"))),
        physical(&format!("a/{N}"))
    );
}

#[test]
fn composed_renames_follow_each_other_and_keep_the_first_target() {
    let cx = RunCx::default();
    let first = Remap::step(
        BTreeMap::new(),
        BTreeMap::new(),
        sprite_remap(&cx, "fx:a", "fx:b"),
    );
    let second = Remap::step(
        BTreeMap::new(),
        BTreeMap::new(),
        sprite_remap(&cx, "fx:b", "fx:c"),
    );
    let id = |text| ResourceId::parse(&cx.interner, text).expect("an id");

    let both = first.then(&second);

    assert_eq!(both.sprite(&id("fx:a")), id("fx:c"));
    assert_eq!(both.sprite(&id("fx:b")), id("fx:c"));
    assert_eq!(
        rename(M, N).then(&rename(N, M)).file(&logical(M)),
        logical(M)
    );
}

#[test]
fn a_changed_tint_index_differs() {
    let mut after = files();
    let model = read(&after, ITEM_MODEL).replace(
        r##""south": {"texture": "#b"}"##,
        r##""south": {"texture": "#b", "tintindex": 0}"##,
    );
    text(&mut after, ITEM_MODEL, &model);

    let result = compare_to(&after);

    assert_eq!(
        result.lines,
        ["view 88.0 (base): item fx:box /model/model/elements/0/faces/south/tintindex: -1 -> 0"]
    );
}

#[test]
fn a_glyph_reached_through_a_font_reference_is_compared() {
    let mut before = files();
    text(
        &mut before,
        "assets/fx/font/main.json",
        r#"{"providers": [{"type": "reference", "id": "fx:hud"}]}"#,
    );
    let mut after = before.clone();
    let font = read(&after, FONT).replace(r#"" ": 4.0"#, r#"" ": 5.0"#);
    text(&mut after, FONT, &font);

    let result = compare_over(&Files::new(), &before, &after, &Remap::default(), false);

    assert!(
        result.lines.iter().any(|line| line == "view 88.0 (base): glyph fx:main U+0020 /0/glyph/space: 4.0 -> 5.0"),
        "{:?}",
        result.lines
    );
}

/// An overlay file that appears, every file the old resolution read unchanged, turns the
/// item's GUI light from `side` to `front`.
#[test]
fn an_overlay_override_that_appears_is_a_difference() {
    let mut before = Files::new();
    text(
        &mut before,
        "pack.mcmeta",
        r#"{"pack": {"min_format": 88, "max_format": 88, "description": "t"}, "overlays": {"entries": [{"directory": "over", "formats": [88, 88]}]}}"#,
    );
    text(
        &mut before,
        "assets/fx/items/box.json",
        r#"{"model": {"type": "minecraft:model", "model": "fx:box"}}"#,
    );
    text(
        &mut before,
        "assets/fx/models/box.json",
        r#"{"textures": {"particle": "fx:item/a"}}"#,
    );
    let mut after = before.clone();
    text(
        &mut after,
        "over/assets/fx/models/box.json",
        r#"{"textures": {"particle": "fx:item/a"}, "gui_light": "front"}"#,
    );

    let result = compare_over(&Files::new(), &before, &after, &Remap::default(), false);

    assert_eq!(
        result.lines,
        [r#"view 88.0 (over): item fx:box /model/model/gui_light: "side" -> "front""#]
    );
}

/// Compares real packs: `PACKPRUNE_ORACLE_BEFORE`, `_AFTER`, `_JAR` and optionally `_ASSET_INDEX` name the inputs,
/// `_ASSETS` the jar's `assets/` extracted (else the whole jar is read); prints the counts and every differing key
/// by view, sorted, and each difference's detail with `PACKPRUNE_ORACLE_DETAILS`.
#[test]
#[ignore = "reads real packs named by PACKPRUNE_ORACLE_*"]
fn verify_env() {
    let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"));
    let limits = Limits::default();
    let jar = var("PACKPRUNE_ORACLE_JAR");
    let mut vanilla = VanillaIndex::from_jar(jar.as_str().as_ref(), &limits).expect("a client jar");
    if let Ok(index) = std::env::var("PACKPRUNE_ORACLE_ASSET_INDEX") {
        vanilla = vanilla
            .with_asset_index(index.as_str().as_ref())
            .expect("an asset index");
    }
    let assets = std::env::var("PACKPRUNE_ORACLE_ASSETS").unwrap_or_else(|_| jar.clone());
    let assets = PackInput::open(assets.as_str().as_ref(), &limits).expect("the jar's files");
    let open = |name: &str| PackInput::open(var(name).as_str().as_ref(), &limits).expect("a pack");
    let (before, after) = (
        open("PACKPRUNE_ORACLE_BEFORE"),
        open("PACKPRUNE_ORACLE_AFTER"),
    );
    let layout = |pack: &PackInput| {
        Layout::new(pack, PackMeta::read(pack).expect("a pack.mcmeta"), &vanilla)
    };
    let (before_layout, after_layout) = (layout(&before), layout(&after));
    let index = ReferenceIndex::default();
    let cx = RunCx::default();
    let start = Instant::now();
    let comparison = compare_with(
        &cx,
        &OracleInput {
            pack: &before,
            layout: &before_layout,
            index: &index,
        },
        &OracleInput {
            pack: &after,
            layout: &after_layout,
            index: &index,
        },
        &vanilla,
        &assets,
        &Remap::default(),
        &|_: &LogicalPath| true,
    );
    let elapsed = start.elapsed();
    let labels = view_labels(&before_layout, &after_layout);
    println!(
        "views {} keys {} differing {}",
        comparison.views, comparison.keys, comparison.differing_keys
    );
    let keys: BTreeSet<String> = comparison
        .differences
        .iter()
        .map(|d| format!("{}\t{}", labels[d.view.0 as usize], d.key))
        .collect();
    keys.iter().for_each(|key| println!("{key}"));
    if std::env::var_os("PACKPRUNE_ORACLE_DETAILS").is_some() {
        for d in &comparison.differences {
            eprintln!(
                "DIFFERS view {}: {} {}",
                labels[d.view.0 as usize], d.key, d.detail
            );
        }
    }
    eprintln!("compare ms {}", elapsed.as_millis());
}

/// A finite UV far outside the texture wraps instead of overflowing.
#[test]
fn an_extreme_face_uv_compares_without_overflow() {
    let mut files = Files::new();
    text(
        &mut files,
        "pack.mcmeta",
        r#"{"pack": {"min_format": 88, "max_format": 88, "description": "probe"}}"#,
    );
    text(
        &mut files,
        "assets/fx/items/probe.json",
        r#"{"model": {"type": "model", "model": "fx:probe"}}"#,
    );
    text(
        &mut files,
        "assets/fx/models/probe.json",
        r##"{"textures": {"a": "fx:item/a"}, "elements": [{"from": [0, 0, 0], "to": [16, 16, 16], "faces": {"north": {"texture": "#a", "uv": [-1000000000000, 0, 16, 16]}}}]}"##,
    );
    files.insert(TEXTURE_A.into(), png(4, 4, 0xFF00_0000));

    // Every file named: the item, the texture's file and its sprite, the named model and the pack's description.
    let result = compare_over(&Files::new(), &files, &files, &Remap::default(), true);

    assert!(result.same(), "{:?}", result.lines);
    assert_eq!((result.comparison.views, result.comparison.keys), (1, 5));
}

/// `bytes` with an ancillary `tEXt` chunk before `IEND`: other bytes, the same pixels.
fn with_text_chunk(bytes: &[u8]) -> Vec<u8> {
    let crc = |data: &[u8]| {
        let mut crc = !0u32;
        for &byte in data {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    };
    let body = b"tEXtComment\0re-encoded";
    let mut chunk = ((body.len() - 4) as u32).to_be_bytes().to_vec();
    chunk.extend_from_slice(body);
    chunk.extend_from_slice(&crc(body).to_be_bytes());
    let end = bytes.len() - 12;
    [&bytes[..end], &chunk, &bytes[end..]].concat()
}

#[test]
fn a_named_model_no_item_shows_is_compared() {
    let mut before = files();
    text(
        &mut before,
        "assets/fx/models/item/loose.json",
        r#"{"parent": "fx:item/base", "display": {"gui": {"rotation": [30.0, 45.0, 0.0]}}}"#,
    );
    let mut after = before.clone();
    text(
        &mut after,
        "assets/fx/models/item/loose.json",
        r#"{"parent": "fx:item/base", "display": {"gui": {"rotation": [0.0, 0.0, 0.0]}}}"#,
    );
    let mut removed = before.clone();
    removed.remove("assets/fx/models/item/loose.json");

    let named = compare_over(&Files::new(), &before, &after, &Remap::default(), true);
    let gone = compare_over(&Files::new(), &before, &removed, &Remap::default(), true);
    let unnamed = compare_over(&Files::new(), &before, &after, &Remap::default(), false);

    assert!(
        named
            .lines
            .iter()
            .any(|line| line.contains("model fx:item/loose")),
        "{:?}",
        named.lines
    );
    assert!(
        gone.lines
            .iter()
            .any(|line| line.ends_with("model fx:item/loose only before")),
        "{:?}",
        gone.lines
    );
    assert!(unnamed.same(), "{:?}", unnamed.lines);
}

#[test]
fn the_pack_icon_compares_by_pixels() {
    let mut before = files();
    before.insert("pack.png".into(), png(4, 4, 0xFF10_2030));
    let mut reencoded = before.clone();
    reencoded.insert("pack.png".into(), with_text_chunk(&before["pack.png"]));
    let mut repainted = before.clone();
    repainted.insert(
        "pack.png".into(),
        with_pixel(&before["pack.png"], 1, 1, 0xFF00_0000),
    );
    let mut removed = before.clone();
    removed.remove("pack.png");

    assert_ne!(before["pack.png"], reencoded["pack.png"]);
    let same = compare_over(&Files::new(), &before, &reencoded, &Remap::default(), false);
    assert!(same.same(), "{:?}", same.lines);
    let differs = compare_over(&Files::new(), &before, &repainted, &Remap::default(), false);
    assert!(
        differs
            .lines
            .iter()
            .any(|line| line.contains("pack icon /texels")),
        "{:?}",
        differs.lines
    );
    let gone = compare_over(&Files::new(), &before, &removed, &Remap::default(), false);
    assert!(
        gone.lines
            .iter()
            .any(|line| line.ends_with("pack icon only before")),
        "{:?}",
        gone.lines
    );
}

#[test]
fn the_pack_description_compares_as_a_text_component() {
    let meta = |description: &str| {
        format!(
            r#"{{"pack": {{"min_format": 88, "max_format": 88, "description": {description}}}}}"#
        )
    };
    let with = |description: &str| {
        let mut files = files();
        text(&mut files, "pack.mcmeta", &meta(description));
        files
    };
    let before = with(r#"["Box ", {"text": "pack", "bold": true}]"#);

    for same in [
        r#"{"text": "Box ", "extra": [{"bold": true, "text": "pack"}]}"#,
        r#"[{"text": "Box ", "extra": []}, {"bold": true, "text": "pack"}]"#,
        r#"["Box ",{"text":"pack","bold":true}]"#,
    ] {
        let result = compare_over(
            &Files::new(),
            &before,
            &with(same),
            &Remap::default(),
            false,
        );
        assert!(result.same(), "{same}: {:?}", result.lines);
    }
    for differs in [
        r#""Box pack""#,
        r#"["Box ", {"text": "pack", "bold": false}]"#,
        r#"["Box ", {"text": "Pack", "bold": true}]"#,
    ] {
        let result = compare_over(
            &Files::new(),
            &before,
            &with(differs),
            &Remap::default(),
            false,
        );
        assert!(
            result
                .lines
                .iter()
                .any(|line| line.contains("pack description")),
            "{differs}: {:?}",
            result.lines
        );
    }
}

/// A pack whose blocks atlas makes `fx:trim/base_gold` with `PalettedPermutations`, from a texture of two colours,
/// a two-entry palette key and a gold palette.
fn with_palette_permutations() -> Files {
    let mut files = files();
    text(
        &mut files,
        "assets/minecraft/atlases/blocks.json",
        r#"{"sources": [{"type": "paletted_permutations", "textures": ["fx:trim/base"],
 "palette_key": "fx:trim/key", "permutations": {"gold": "fx:trim/gold"}}]}"#,
    );
    let pixels = |argb: &[u32]| argb.iter().flat_map(|&p| argb_to_rgba(p)).collect();
    let base = [0xFF10_1010, 0xFF20_2020].repeat(8);
    files.insert(
        "assets/fx/textures/trim/base.png".into(),
        encode(4, 4, pixels(&base)),
    );
    files.insert(
        "assets/fx/textures/trim/key.png".into(),
        encode(2, 1, pixels(&[0xFF10_1010, 0xFF20_2020])),
    );
    files.insert(
        "assets/fx/textures/trim/gold.png".into(),
        encode(2, 1, pixels(&[0xFFFF_D700, 0xFFB8_8600])),
    );
    files
}

/// `PalettedPermutations` reads each of its textures through `NativeImage.read`: a palette permutation sprite is its
/// inputs' pixels, which the lossless PNG optimiser keeps.
#[test]
fn a_palette_permutation_sprite_compares_its_inputs_by_pixels() {
    let before = with_palette_permutations();
    let mut optimised = before.clone();
    for (path, bytes) in &mut optimised {
        if path.starts_with("assets/fx/textures/trim/") {
            *bytes = png::optimise(bytes, &png::Options::default()).expect("optimises");
        }
    }
    let mut repainted = before.clone();
    repainted.insert(
        "assets/fx/textures/trim/gold.png".into(),
        with_pixel(
            &before["assets/fx/textures/trim/gold.png"],
            1,
            0,
            0xFFB8_8601,
        ),
    );

    assert_ne!(
        before["assets/fx/textures/trim/base.png"],
        optimised["assets/fx/textures/trim/base.png"]
    );
    let same = compare_over(&Files::new(), &before, &optimised, &Remap::default(), true);
    assert!(same.same(), "{:?}", same.lines);
    let differs = compare_over(&Files::new(), &before, &repainted, &Remap::default(), true);
    assert!(
        differs
            .lines
            .iter()
            .any(|line| line.contains("sprite minecraft:blocks fx:trim/base_gold /inputs/2/texels")),
        "{:?}",
        differs.lines
    );
}

/// `SpriteResourceLoader.loadSprite` drops a sprite whose `.mcmeta` does not parse or whose animation the codec
/// refuses, and loads one with `{}` as one without metadata: the face's texture is not named, so only its sprite
/// tells.
#[test]
fn metadata_the_client_rejects_differs_from_none() {
    let before = files();
    for (metadata, same) in [
        ("{}", true),
        ("{", false),
        (r#"{"animation":{"frametime":0}}"#, false),
    ] {
        let mut after = before.clone();
        text(&mut after, &format!("{TEXTURE_A}.mcmeta"), metadata);

        let result = compare_over(&Files::new(), &before, &after, &Remap::default(), false);

        assert_eq!(result.same(), same, "{metadata}: {:?}", result.lines);
    }
}

/// Metadata whose numbers are not all `int` literals is kept as written: numbers the client's codecs read differently
/// stay apart.
#[test]
fn unmodelled_metadata_keeps_numeric_distinctions() {
    let pairs = [
        (
            4,
            r#"{"animation":{"width":9007199254740992}}"#,
            r#"{"animation":{"width":9007199254740993}}"#,
        ),
        (
            4,
            r#"{"animation":{"frames":[9007199254740992,1],"width":2}}"#,
            r#"{"animation":{"frames":[9007199254740993,1],"width":2}}"#,
        ),
        (
            8,
            r#"{"animation":{"frametime":1.99999999999999999}}"#,
            r#"{"animation":{"frametime":2.0}}"#,
        ),
        (
            8,
            r#"{"animation":{"frames":[0.99999999999999999,1]}}"#,
            r#"{"animation":{"frames":[1.0,1]}}"#,
        ),
        // One double, two floats: `TextureMetadataSection` reads `Float.parseFloat` of the text, and the first lies just
        // past the midpoint between 1.0f and the next float.
        (
            4,
            r#"{"texture":{"alpha_cutoff_bias":1.0000000596046447753906251}}"#,
            r#"{"texture":{"alpha_cutoff_bias":1.000000059604644775390625}}"#,
        ),
    ];
    for (height, left, right) in pairs {
        let mut before = files();
        before.insert(TEXTURE_A.into(), png(4, height, 0xFF00_0000));
        let mut after = before.clone();
        text(&mut before, &format!("{TEXTURE_A}.mcmeta"), left);
        text(&mut after, &format!("{TEXTURE_A}.mcmeta"), right);

        let result = compare_over(&Files::new(), &before, &after, &Remap::default(), false);

        assert!(!result.same(), "{left} -> {right}");
    }
}
