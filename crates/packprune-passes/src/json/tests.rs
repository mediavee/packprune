//! The short-json, bloat-keys and drop-unread passes, each through the pass framework and its
//! proof harness over the fixtures' client (`tests/fixtures/vanilla/`).

use std::collections::{BTreeMap, BTreeSet};

use packprune_codec::png::{Image, encode};
use packprune_pack::{ClientRules, Limits, PackInput, PhysPath, RunCx, VanillaIndex};
use packprune_pass::{Context, Enabled, Kept, NoCache, Pass, Scope, prove, run};

use super::bloat::{BloatKeys, debloat};
use super::short::{ShortJson, shortest};
use super::unread::DropUnread;

type Files = BTreeMap<String, Vec<u8>>;

const ITEM_MODEL: &str = "assets/fx/models/item/box.json";
const PARENT_MODEL: &str = "assets/fx/models/item/base.json";
const STONE_MODEL: &str = "assets/fx/models/block/stone.json";
const SELECT: &str = "assets/fx/items/select.json";
const META: &str = r#"{"pack": {"min_format": 88, "max_format": 88, "description": "t"}}"#;

fn read_dir(root: &std::path::Path) -> PackInput {
    PackInput::open(root.to_str().unwrap().as_ref(), &Limits::default()).unwrap()
}

/// The fixtures' client, as files and as the index the analysis reads.
struct Fixture {
    run: RunCx,
    client: PackInput,
    vanilla: VanillaIndex,
    rules: ClientRules,
}

impl Fixture {
    fn new() -> Self {
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/vanilla");
        let client = read_dir(&root);
        let names: BTreeSet<String> = client
            .files
            .keys()
            .map(PhysPath::as_str)
            .filter(|path| path.starts_with("assets/"))
            .map(str::to_owned)
            .collect();
        let atlases = names
            .iter()
            .filter(|path| path.starts_with("assets/minecraft/atlases/"))
            .map(|path| {
                let bytes = client.files[&PhysPath::new(path).unwrap()].blob.to_vec();
                (path.clone(), bytes)
            })
            .collect();
        let vanilla = VanillaIndex {
            version: "26.2".into(),
            pack_format: "88.0".into(),
            names,
            atlases,
            ..VanillaIndex::default()
        };
        Self {
            run: RunCx::default(),
            rules: ClientRules::for_version(&vanilla.version),
            client,
            vanilla,
        }
    }

    fn cx(&self) -> Context<'_> {
        Context {
            run: &self.run,
            vanilla: &self.vanilla,
            rules: &self.rules,
            sources: &[],
            selectors: &[],
            cache: &NoCache,
        }
    }
}

fn pack(files: &Files) -> PackInput {
    let dir = tempfile::tempdir().unwrap();
    for (path, bytes) in files {
        let target = dir.path().join(path);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, bytes).unwrap();
    }
    read_dir(dir.path())
}

fn enabled(pass: &dyn Pass) -> Enabled<'_> {
    Enabled {
        pass,
        scope: Scope::all(),
        options: toml::Table::new(),
    }
}

fn text(pack: &PackInput, path: &str) -> String {
    String::from_utf8(pack.files[&PhysPath::new(path).unwrap()].blob.to_vec()).unwrap()
}

fn put(files: &mut Files, path: &str, text: &str) {
    files.insert(path.into(), text.as_bytes().to_vec());
}

/// A PNG whose pixel `(x, y)` is ARGB `base + x + 4 * y`, so every texel differs.
fn png(width: u32, height: u32, base: u32) -> Vec<u8> {
    let mut rgba = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let [a, r, g, b] = (base + x + 4 * y).to_be_bytes();
            rgba.extend_from_slice(&[r, g, b, a]);
        }
    }
    encode(&Image {
        width,
        height,
        rgba,
    })
    .unwrap()
}

/// An item whose model samples two textures through a parent with a display transform, a
/// font, a sound event.
fn test_pack() -> Files {
    let mut files = Files::new();
    put(&mut files, "pack.mcmeta", META);
    put(
        &mut files,
        "assets/fx/items/box.json",
        r#"{"model": {"type": "minecraft:model", "model": "fx:item/box"}}"#,
    );
    put(
        &mut files,
        PARENT_MODEL,
        r#"{"display": {"gui": {"rotation": [30.0, 45.0, 0.0]}}}"#,
    );
    put(
        &mut files,
        ITEM_MODEL,
        r##"{"parent": "fx:item/base", "textures": {"a": "fx:item/a", "b": "fx:item/b"},
 "elements": [{"from": [0.0, 0.0, 0.0], "to": [16.0, 16.0, 16.0], "rotation": {"origin": [8.0, 8.0, 8.0], "axis": "y", "angle": 22.5},
   "faces": {"north": {"uv": [0.0, 0.0, 8.0, 8.0], "texture": "#a"}, "south": {"texture": "#b"}}}]}"##,
    );
    files.insert(
        "assets/fx/textures/item/a.png".into(),
        png(4, 4, 0xFF00_0000),
    );
    files.insert(
        "assets/fx/textures/item/b.png".into(),
        png(4, 4, 0xFF80_8000),
    );
    put(
        &mut files,
        "assets/fx/font/hud.json",
        r#"{"providers": [{"type": "space", "advances": {" ": 4.0}},
 {"type": "bitmap", "file": "fx:font/glyphs.png", "ascent": 7, "chars": [""]}]}"#,
    );
    files.insert(
        "assets/fx/textures/font/glyphs.png".into(),
        png(8, 8, 0xFFFF_FF00),
    );
    put(
        &mut files,
        "assets/fx/sounds.json",
        r#"{"boom": {"sounds": ["fx:boom"]}}"#,
    );
    put(&mut files, "assets/fx/sounds/boom.ogg", "not really vorbis");
    put(
        &mut files,
        "assets/fx/lang/en_us.json",
        r#"{"item.fx.box": "Box 16.0"}"#,
    );
    files
}

#[test]
fn writes_each_number_in_its_shortest_text_for_the_same_value() {
    assert_eq!(shortest("16.0").as_deref(), Some("16"));
    assert_eq!(shortest("0.50").as_deref(), Some("0.5"));
    assert_eq!(shortest("-2.000").as_deref(), Some("-2"));
    assert_eq!(shortest("1.0E-5").as_deref(), Some("1e-5"));
    assert_eq!(shortest("0.1"), None);
    assert_eq!(shortest("1e300"), None);
    assert_eq!(
        shortest("-0.0"),
        None,
        "-0 reads as an integer, which has no sign"
    );
}

#[test]
fn shortest_keeps_what_a_float_reads_differently_and_writes_large_values_with_an_exponent() {
    // Same double, but the float of `0.30000001` is not the float of `0.3`.
    assert_eq!(shortest("0.300000010000000"), Some("0.30000001".into()));
    assert_eq!(shortest("1600.0").as_deref(), Some("1600"));
    assert_eq!(shortest("1.6E18").as_deref(), Some("16e17"));
    assert_eq!(
        shortest("123456789012345678.0").as_deref(),
        Some("123456789012345680")
    );
    // Just under the midpoint of two floats: the same double as `1.0000000596046448`, which reads as the float above.
    assert_eq!(shortest("1.0000000596046447753906249999999"), None);
    assert_eq!(
        shortest("1.0000000596046447753906250000001").as_deref(),
        Some("1.0000000596046448")
    );
}

#[test]
fn shortens_numbers_and_drops_the_default_namespace_from_ids_only_where_the_client_reads_an_id() {
    let fixture = Fixture::new();
    let cx = fixture.cx();
    let mut files = test_pack();
    put(
        &mut files,
        STONE_MODEL,
        r#"{"parent": "minecraft:block/cube_all", "textures": {"all": "minecraft:block/stone"}}"#,
    );
    put(
        &mut files,
        SELECT,
        r#"{"model": {"type": "minecraft:select", "property": "minecraft:display_context",
 "cases": [{"when": "minecraft:gui", "model": {"type": "minecraft:model", "model": "minecraft:item/paper"}}],
 "fallback": {"type": "minecraft:model", "model": "fx:block/stone"}}}"#,
    );
    put(
        &mut files,
        "pack.mcmeta",
        r#"{"pack": {"min_format": 88, "max_format": 88, "description": "t"}, "x": 1.0}"#,
    );
    let input = pack(&files);
    let state = cx.analyse(input.clone()).unwrap();

    let outcome = run(&cx, &Kept::of(&state), &state, &[enabled(&ShortJson)]).unwrap();

    let out = &outcome.state.pack;
    assert_eq!(
        text(out, STONE_MODEL),
        r#"{"parent": "block/cube_all", "textures": {"all": "block/stone"}}"#
    );
    let model = text(out, ITEM_MODEL);
    assert!(
        model.contains(r#""from": [0, 0, 0], "to": [16, 16, 16], "rotation": {"origin": [8, 8, 8], "axis": "y", "angle": 22.5}"#),
        "{model}"
    );
    let select = text(out, SELECT);
    assert!(select.contains(r#""model": "item/paper""#), "{select}");
    assert!(
        select.contains(r#""when": "minecraft:gui""#)
            && select.contains(r#""type": "minecraft:select""#),
        "a value no reference names keeps its namespace: {select}"
    );
    assert_eq!(
        text(out, "pack.mcmeta"),
        text(&input, "pack.mcmeta"),
        "pack.mcmeta is not rewritten"
    );
    assert_eq!(
        text(out, "assets/fx/lang/en_us.json"),
        text(&input, "assets/fx/lang/en_us.json")
    );
    assert_eq!(
        outcome.reports[0].changes,
        [
            "17 numbers shortened",
            "3 minecraft: prefixes dropped",
            "5 files rewritten"
        ]
    );

    let proof = prove(&cx, &fixture.client, input, vec![enabled(&ShortJson)]).unwrap();
    let oracle = proof.proof.expect("short-json changed something");
    assert_eq!(oracle.differing_keys, 0, "{:?}", oracle.differences);
}

#[test]
fn lists_files_that_are_not_strict_json_and_leaves_them() {
    let fixture = Fixture::new();
    let cx = fixture.cx();
    let mut files = test_pack();
    put(
        &mut files,
        PARENT_MODEL,
        r#"{"display": {}, /* x */ "a": 1.0}"#,
    );
    let state = cx.analyse(pack(&files)).unwrap();

    let outcome = run(&cx, &Kept::of(&state), &state, &[enabled(&ShortJson)]).unwrap();

    assert_eq!(
        text(&outcome.state.pack, PARENT_MODEL),
        r#"{"display": {}, /* x */ "a": 1.0}"#
    );
    assert_eq!(
        outcome.reports[0].changes.last().unwrap(),
        &format!("1 files left as they are, not strict JSON: {PARENT_MODEL}")
    );
}

#[test]
fn debloat_cuts_root_bloat_members_and_keeps_every_other_byte() {
    let cut =
        |json: &str| debloat(json.as_bytes()).map(|(out, n)| (String::from_utf8(out).unwrap(), n));
    assert_eq!(
        cut(r#"{"credit": "Made with Blockbench", "parent": "item/x", "groups": [1, {"a": 2}]}"#),
        Some((r#"{"parent": "item/x"}"#.into(), 2))
    );
    assert_eq!(
        cut("{\n  \"parent\": \"a\",\n  \"__comment\": \"x\",\n  \"textures\": {}\n}"),
        Some(("{\n  \"parent\": \"a\",\n  \"textures\": {}\n}".into(), 1))
    );
    assert_eq!(
        cut(r#"{"credit": "c", "_comment": "x"}"#),
        Some((r#"{"_comment": "x"}"#.into(), 1)),
        "a key neither Blockbench nor the vanilla models write stays"
    );
    assert_eq!(
        cut(r#"{"textures": {"__comment": "x"}, "elements": [{"__comment": 1}]}"#),
        None,
        "below the root a key is a slot or a face to the client"
    );
    assert_eq!(
        cut(r#"{"credit": "a", "credit": "b", "parent": "x"}"#),
        None,
        "a repeated name"
    );
    assert_eq!(
        cut(r#"{"credit": "a", /* c */ "parent": "x"}"#),
        None,
        "not strict JSON"
    );
    assert_eq!(cut(r#"["credit"]"#), None);
}

#[test]
fn bloat_keys_leave_what_the_player_sees() {
    let fixture = Fixture::new();
    let cx = fixture.cx();
    let mut files = test_pack();
    put(
        &mut files,
        PARENT_MODEL,
        r#"{"credit": "Made with Blockbench", "display": {"gui": {"rotation": [30.0, 45.0, 0.0]}}, "groups": [{"name": "g", "children": [0]}]}"#,
    );
    let input = pack(&files);

    let proof = prove(&cx, &fixture.client, input, vec![enabled(&BloatKeys)]).unwrap();

    assert_eq!(
        text(&proof.output.pack, PARENT_MODEL),
        r#"{"display": {"gui": {"rotation": [30.0, 45.0, 0.0]}}}"#
    );
    assert_eq!(proof.reports[0].changes, ["2 keys removed from 1 models"]);
    let oracle = proof.proof.expect("bloat-keys changed something");
    assert_eq!(oracle.differing_keys, 0, "{:?}", oracle.differences);
}

fn unread_pack() -> Files {
    let mut files = test_pack();
    put(
        &mut files,
        "pack.mcmeta",
        r#"{"pack": {"min_format": 88, "max_format": 88, "description": "t"},
 "overlays": {"entries": [{"formats": [88, 88], "directory": "over"}]}}"#,
    );
    put(&mut files, "README.md", "# a pack");
    put(&mut files, "over/pack.png", "not shown");
    put(
        &mut files,
        "over/assets/fx/lang/en_us.json",
        r#"{"a": "b"}"#,
    );
    put(&mut files, "old/assets/fx/lang/en_us.json", r#"{"a": "c"}"#);
    put(&mut files, "assets/fx/textures/.DS_Store", "finder");
    put(
        &mut files,
        "assets/fx/textures/desktop.ini",
        "a nameable file",
    );
    put(&mut files, "assets/fx/textures/Thumbs.db", "explorer");
    put(&mut files, "assets/fx/sounds/boom.mp3", "not ogg");
    files
}

/// The paths of `input` that `output` no longer has.
fn removed<'a>(input: &'a PackInput, output: &PackInput) -> Vec<&'a str> {
    input
        .files
        .keys()
        .filter(|path| !output.files.contains_key(*path))
        .map(PhysPath::as_str)
        .collect()
}

#[test]
fn drops_only_files_no_declared_view_reads() {
    let fixture = Fixture::new();
    let cx = fixture.cx();
    let input = pack(&unread_pack());

    let proof = prove(
        &cx,
        &fixture.client,
        input.clone(),
        vec![enabled(&DropUnread)],
    )
    .unwrap();

    let removed: Vec<&str> = removed(&input, &proof.output.pack)
        .into_iter()
        .filter(|path| !proof.pruned.contains(&PhysPath::new(path).unwrap()))
        .collect();
    assert_eq!(
        removed,
        [
            "README.md",
            "assets/fx/textures/.DS_Store",
            "assets/fx/textures/Thumbs.db",
            "old/assets/fx/lang/en_us.json",
            "over/pack.png",
        ]
    );
    assert_eq!(
        proof.reports[0].changes,
        [
            "5 files no client reads dropped",
            "3 files, 27 bytes: outside assets/ of the root and the declared overlays",
            "2 files, 14 bytes: system or hidden, a path no client of the views can name",
            "1 non-Ogg audio files kept, the client reads only .ogg: assets/fx/sounds/boom.mp3",
        ]
    );
    let oracle = proof.proof.expect("drop-unread changed something");
    assert_eq!(oracle.differing_keys, 0, "{:?}", oracle.differences);
}

#[test]
fn keeps_an_unread_file_a_source_pins() {
    let fixture = Fixture::new();
    let cx = fixture.cx();
    let state = cx.analyse(pack(&unread_pack())).unwrap();
    let mut kept = Kept::of(&state);
    kept.pinned.insert(PhysPath::new("README.md").unwrap());

    let outcome = run(&cx, &kept, &state, &[enabled(&DropUnread)]).unwrap();

    assert!(
        outcome
            .state
            .pack
            .files
            .contains_key(&PhysPath::new("README.md").unwrap())
    );
    let changes = &outcome.reports[0].changes;
    assert_eq!(changes[0], "4 files no client reads dropped");
    assert!(
        changes.contains(&"1 files no client reads kept, a source names them: README.md".into()),
        "{changes:?}"
    );
}

#[test]
fn keeps_hidden_files_a_pre_1_13_view_may_read() {
    let fixture = Fixture::new();
    let cx = fixture.cx();
    let hidden = "assets/fx/textures/.Hidden.png";
    let dropped = |meta: &str| {
        let mut files = unread_pack();
        put(&mut files, "pack.mcmeta", meta);
        files.insert(hidden.into(), png(2, 2, 0xFF00_0000));
        let state = cx.analyse(pack(&files)).unwrap();
        let outcome = run(&cx, &Kept::of(&state), &state, &[enabled(&DropUnread)]).unwrap();
        !outcome
            .state
            .pack
            .files
            .contains_key(&PhysPath::new(hidden).unwrap())
    };

    assert!(!dropped(
        r#"{"pack": {"pack_format": 2, "supported_formats": [2, 88], "description": "t"}}"#
    ));
    assert!(dropped(
        r#"{"pack": {"min_format": 88, "max_format": 88, "description": "t"}}"#
    ));
}

/// Files under `assets/` 26.2 does not classify: some a mod, an older client or a font reads, some no reader reads.
fn kinds_pack() -> Files {
    let mut files = test_pack();
    for (path, text) in [
        // Kept: read by OptiFine, MTR3, a client before 26.2, a font, or simply not provably unread.
        (
            "assets/minecraft/optifine/ctm/glass/glass.properties",
            "method=ctm",
        ),
        ("assets/minecraft/optifine/notes.properties", "a=b"),
        ("assets/minecraft/optifine/cem/sub/creeper.jem", "{}"),
        ("assets/minecraft/unused/x.properties", "a=b"),
        ("assets/minecraft/shaders/program/blit.json", "{}"),
        ("assets/minecraft/lang/en_us.lang", "a=b"),
        ("assets/fx/textures/item/a.psd", "layers"),
        ("assets/fx/textures/item/notes.txt", "todo"),
        (
            "assets/fx/font/uni.json",
            r#"{"providers": [{"type": "unihex", "hex_file": "fx:misc/glyphs.zip", "size_overrides": []}]}"#,
        ),
        ("assets/fx/misc/glyphs.zip", "a hex archive"),
        ("assets/minecraft/optifine/cem/creeper.jemc", "{}"),
        // Dropped: a kind no reader reads.
        (
            "assets/minecraft/optifine/ctm/glass/template.bbmodel",
            "a Blockbench project",
        ),
        ("assets/fx/textures/item/a.png.bak", "a backup"),
        ("assets/fx/textures/item/b.png.old", "a leftover"),
        ("assets/fx/models/item/box.json.orig", "a merge leftover"),
        ("assets/fx/models/item/box.json~", "an editor backup"),
        ("assets/fx/models/item/box.jsonc", "{}"),
        ("assets/minecraft/optifine/ctm/glass/old.zip", "an old copy"),
    ] {
        put(&mut files, path, text);
    }
    files
}

#[test]
fn drops_only_kinds_no_reader_reads() {
    let fixture = Fixture::new();
    let cx = fixture.cx();
    let input = pack(&kinds_pack());
    let state = cx.analyse(input.clone()).unwrap();

    let outcome = run(&cx, &Kept::of(&state), &state, &[enabled(&DropUnread)]).unwrap();

    let size = |path: &str| input.files[&PhysPath::new(path).unwrap()].blob.len();
    assert_eq!(
        removed(&input, &outcome.state.pack),
        [
            "assets/fx/models/item/box.json.orig",
            "assets/fx/models/item/box.jsonc",
            "assets/fx/models/item/box.json~",
            "assets/fx/textures/item/a.png.bak",
            "assets/fx/textures/item/b.png.old",
            "assets/minecraft/optifine/ctm/glass/old.zip",
            "assets/minecraft/optifine/ctm/glass/template.bbmodel",
        ]
    );
    let backups: usize = [
        "assets/fx/textures/item/a.png.bak",
        "assets/fx/textures/item/b.png.old",
        "assets/fx/models/item/box.json.orig",
        "assets/fx/models/item/box.json~",
    ]
    .iter()
    .map(|path| size(path))
    .sum();
    assert_eq!(
        outcome.reports[0].changes,
        [
            "7 files no client reads dropped".to_owned(),
            format!(
                "1 files, {} bytes: an editor project no client or mod reads",
                size("assets/minecraft/optifine/ctm/glass/template.bbmodel")
            ),
            format!("4 files, {backups} bytes: a backup or leftover no reader asks for"),
            format!(
                "1 files, {} bytes: a commented JSON source no reader opens",
                size("assets/fx/models/item/box.jsonc")
            ),
            format!(
                "1 files, {} bytes: an archive no font's hex_file names",
                size("assets/minecraft/optifine/ctm/glass/old.zip")
            ),
        ]
    );

    let proof = prove(&cx, &fixture.client, input, vec![enabled(&DropUnread)]).unwrap();
    let oracle = proof.proof.expect("drop-unread changed something");
    assert_eq!(oracle.differing_keys, 0, "{:?}", oracle.differences);
}

/// The paths of `files` that `DropUnread` alone, without a prune, leaves out.
fn dropped_alone(files: &Files) -> Vec<String> {
    let fixture = Fixture::new();
    let cx = fixture.cx();
    let input = pack(files);
    let state = cx.analyse(input.clone()).unwrap();
    let outcome = run(&cx, &Kept::of(&state), &state, &[enabled(&DropUnread)]).unwrap();
    removed(&input, &outcome.state.pack)
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// OptiFine reads `optifine/cit.properties` (`CustomItems.update`), the properties of a CEM model
/// (`CustomEntityModels.makeProperties`) and the parts a `.jem` names, in any subfolder
/// (`CustomEntityModelParser.processExternalModel`).
#[test]
fn keeps_what_optifine_reads() {
    let mut files = test_pack();
    for (path, text) in [
        ("assets/minecraft/optifine/cit.properties", "useGlint=false"),
        (
            "assets/minecraft/optifine/cem/creeper.jem",
            r#"{"models": [{"part": "head", "model": "./parts/head.jpm"}]}"#,
        ),
        (
            "assets/minecraft/optifine/cem/creeper.properties",
            "models.1=2\nname.1=James",
        ),
        ("assets/minecraft/optifine/cem/parts/head.jpm", "{}"),
    ] {
        put(&mut files, path, text);
    }
    assert_eq!(dropped_alone(&files), Vec::<String>::new());
}

/// A font is read leniently (`FontManager.GSON`): one with a comment still loads the archive its `hex_file` names.
#[test]
fn keeps_an_archive_a_lenient_font_names() {
    let mut files = test_pack();
    put(
        &mut files,
        "assets/fx/font/review.json",
        "// valid FontManager JSON\n{\"providers\": [\
         {\"type\": \"unihex\", \"hex_file\": \"fx:glyphs/unifont.zip\", \"size_overrides\": []},\
         {\"type\": \"bitmap\", \"file\": \"fx:font/glyphs.png.bak\", \"ascent\": 7, \"chars\": [\"a\"]},\
         {\"type\": \"legacy_unicode\", \"sizes\": \"fx:font/sizes.bin\", \"template\": \"fx:font/page_%s.png.old\"}]}",
    );
    put(&mut files, "assets/fx/glyphs/unifont.zip", "a hex archive");
    put(
        &mut files,
        "assets/fx/textures/font/glyphs.png.bak",
        "a bitmap",
    );
    put(
        &mut files,
        "assets/fx/textures/font/page_00.png.old",
        "a page",
    );
    // What the font does not name still goes: the font was read.
    put(&mut files, "assets/fx/glyphs/orphan.zip", "an old archive");
    assert_eq!(dropped_alone(&files), ["assets/fx/glyphs/orphan.zip"]);
}

/// A font no parser reads may name any file: then no file goes for its kind.
#[test]
fn keeps_every_kind_while_a_font_is_unreadable() {
    let mut files = test_pack();
    put(
        &mut files,
        "assets/fx/font/broken.json",
        "{\"providers\": [",
    );
    put(&mut files, "assets/fx/misc/glyphs.zip", "a hex archive");
    put(&mut files, "assets/fx/textures/item/a.png.bak", "a backup");
    assert_eq!(dropped_alone(&files), Vec::<String>::new());
}

/// MTR3 reads a `.bbmodel` from any path of its namespace (`CustomResources`), whatever the first folder.
#[test]
fn keeps_mtr3_models_in_every_folder() {
    let mut files = test_pack();
    for path in [
        "assets/mtr/textures/train.bbmodel",
        "assets/mtr/models/train.bbmodel",
        "assets/mtr/trains/normal.bbmodel",
        "assets/mtr/sounds/train.bbmodel",
    ] {
        put(&mut files, path, "{}");
    }
    assert_eq!(dropped_alone(&files), Vec::<String>::new());
}

/// A view at a format the jar cannot verify roots every `assets/minecraft/` file whatever its kind (as CTM Overhaul's
/// `[15, 75]` over a 26.2 jar): a Blockbench project there is still read by no client.
#[test]
fn drops_unread_kinds_in_a_view_the_jar_cannot_verify() {
    let fixture = Fixture::new();
    let cx = fixture.cx();
    let mut files = kinds_pack();
    put(
        &mut files,
        "pack.mcmeta",
        r#"{"pack": {"pack_format": 64, "min_format": 15, "max_format": 75, "description": "t"}}"#,
    );
    let input = pack(&files);
    let state = cx.analyse(input.clone()).unwrap();

    let outcome = run(&cx, &Kept::of(&state), &state, &[enabled(&DropUnread)]).unwrap();

    let removed = removed(&input, &outcome.state.pack);
    assert!(
        removed.contains(&"assets/minecraft/optifine/ctm/glass/template.bbmodel"),
        "{removed:?}"
    );
    assert!(!removed.contains(&"assets/minecraft/optifine/ctm/glass/glass.properties"));
}

/// OptiFine's `TextureAnimations` opens the `from=` of `optifine/anim/*.properties` as written, whatever its suffix:
/// a source a properties file names stays, though its kind is one no reader asks for by itself.
#[test]
fn keeps_files_a_properties_file_names() {
    let mut files = test_pack();
    let frames = png(16, 32, 0xFF00_0000);
    let sources = [
        ("bak", "frames.png.bak"),
        ("old", "frames.png.old"),
        ("orig", "frames.png.orig"),
        ("tilde", "frames.png~"),
        ("zip", "frames.zip"),
    ];
    for (name, source) in sources {
        put(
            &mut files,
            &format!("assets/minecraft/optifine/anim/review_{name}.properties"),
            &format!(
                "from=./{source}\nto=textures/entity/pig/pig_temperate.png\nx=0\ny=0\nw=16\nh=16\n"
            ),
        );
        files.insert(
            format!("assets/minecraft/optifine/anim/{source}"),
            frames.clone(),
        );
    }
    // Named only by a file that goes too, a backup no reader asks for: both go.
    files.insert(
        "assets/minecraft/optifine/anim/spare.png.bak".into(),
        frames,
    );
    put(
        &mut files,
        "assets/minecraft/optifine/anim/spare.properties.bak",
        "from=./spare.png.bak\nto=textures/entity/pig/pig_temperate.png\n",
    );

    let fixture = Fixture::new();
    let cx = fixture.cx();
    let input = pack(&files);
    let state = cx.analyse(input.clone()).unwrap();
    let outcome = run(&cx, &Kept::of(&state), &state, &[enabled(&DropUnread)]).unwrap();

    assert_eq!(
        removed(&input, &outcome.state.pack),
        [
            "assets/minecraft/optifine/anim/spare.png.bak",
            "assets/minecraft/optifine/anim/spare.properties.bak",
        ]
    );
    assert!(
        outcome.reports[0].changes.contains(
            &"5 files of a kind no reader asks for kept, a text file of the pack names them: \
              assets/minecraft/optifine/anim/frames.png.bak, assets/minecraft/optifine/anim/frames.png.old, \
              assets/minecraft/optifine/anim/frames.png.orig, assets/minecraft/optifine/anim/frames.png~, \
              assets/minecraft/optifine/anim/frames.zip"
                .to_owned()
        ),
        "{:?}",
        outcome.reports[0].changes
    );
}

/// `TextureAnimations` loads its properties with `java.util.Properties`: Latin-1, `\uXXXX` and backslash escapes. A
/// name the file spells only through an escape, or in a file that is not UTF-8, is still the path OptiFine opens.
#[test]
fn keeps_files_a_properties_file_names_through_java_escapes_or_latin_1() {
    let frames = png(16, 32, 0xFF00_0000);
    let target = "to=textures/entity/pig/pig_temperate.png\nx=0\ny=0\nw=16\nh=16\n";
    let cases: [(&str, Vec<u8>); 2] = [
        (
            "escaped",
            format!("from=./frames.png.\\u0062ak\n{target}").into_bytes(),
        ),
        (
            "latin-1",
            [
                b"# caf\xe9\nfrom=./frames.png.bak\n".as_slice(),
                target.as_bytes(),
            ]
            .concat(),
        ),
    ];
    for (case, properties) in cases {
        let mut files = test_pack();
        files.insert(
            "assets/minecraft/optifine/anim/review.properties".into(),
            properties,
        );
        files.insert(
            "assets/minecraft/optifine/anim/frames.png.bak".into(),
            frames.clone(),
        );
        assert_eq!(dropped_alone(&files), Vec::<String>::new(), "{case}");
    }
}

/// A 1.12 client lowercases the path a properties file names, and a kept backup a text file names may name another.
#[test]
fn keeps_files_named_in_another_case_or_through_a_kept_file() {
    let mut files = test_pack();
    put(
        &mut files,
        "assets/minecraft/optifine/anim/upper.properties",
        "from=./FRAMES.PNG.BAK\nto=textures/entity/pig/pig_temperate.png\n",
    );
    files.insert(
        "assets/minecraft/optifine/anim/frames.png.bak".into(),
        png(16, 32, 0xFF00_0000),
    );
    put(&mut files, "assets/fx/root.txt", "config.properties.bak");
    put(
        &mut files,
        "assets/fx/config.properties.bak",
        "from=./second.png.bak",
    );
    put(&mut files, "assets/fx/second.png.bak", "a source");
    assert_eq!(dropped_alone(&files), Vec::<String>::new());
}

/// Changes `DropUnread` alone reports on `files`, and what it leaves out.
fn drop_unread_alone(files: &Files) -> (Vec<String>, Vec<String>) {
    let fixture = Fixture::new();
    let cx = fixture.cx();
    let input = pack(files);
    let state = cx.analyse(input.clone()).unwrap();
    let outcome = run(&cx, &Kept::of(&state), &state, &[enabled(&DropUnread)]).unwrap();
    let removed = removed(&input, &outcome.state.pack)
        .into_iter()
        .map(str::to_owned)
        .collect();
    (removed, outcome.reports[0].changes.clone())
}

/// Java's `Properties` accepts a lone surrogate escape the codec refuses, and a JSON reader decodes string escapes:
/// a file the pass cannot read as its reader does stops every drop by kind in the pack, and says which.
#[test]
fn drops_nothing_by_kind_while_a_namer_is_unreadable() {
    let frames = png(16, 32, 0xFF00_0000);
    let cases: [(&str, &str, &str); 3] = [
        (
            "assets/minecraft/optifine/anim/review.properties",
            "ignored=\\uD800\nfrom=./frames.png.\\u0062ak\nto=textures/entity/pig/pig_temperate.png\nx=0\ny=0\nw=16\nh=16\n",
            "assets/minecraft/optifine/anim/review.properties",
        ),
        (
            "assets/fx/notes.json",
            "{\"unclosed\": ",
            "assets/fx/notes.json",
        ),
        (
            "assets/fx/font/broken.json",
            "{\"providers\": [",
            "assets/fx/font/broken.json",
        ),
    ];
    for (path, text, unreadable) in cases {
        let mut files = test_pack();
        put(&mut files, path, text);
        files.insert(
            "assets/minecraft/optifine/anim/frames.png.bak".into(),
            frames.clone(),
        );
        put(&mut files, "assets/fx/project.bbmodel", "{}");
        put(&mut files, "README.md", "outside assets/");

        let (removed, changes) = drop_unread_alone(&files);

        assert_eq!(removed, ["README.md"], "{path}");
        assert!(
            changes.contains(&format!("drop by kind skipped: {unreadable} unreadable")),
            "{changes:?}"
        );
    }
}
