//! identical-sprites and animation-frames: each run goes through the harness, whose oracle must find the result the
//! same for the player.

pub(crate) mod support;

mod corpus;

use support::{FONT, Files, Fixture, ITEM_MODEL, argb, decode, gradient, png, put, test_pack};

use super::animation::AnimationFrames;
use super::identical::IdenticalSprites;

fn texture(name: &str) -> String {
    format!("assets/fx/textures/item/{name}.png")
}

/// [`test_pack`] with a box whose model has one element drawing `uv` of each texture.
fn with_box(uv: &str, textures: &[&str]) -> Files {
    let mut files = test_pack();
    let directions = ["down", "up", "north", "south", "west", "east"];
    let slots: Vec<String> = textures
        .iter()
        .enumerate()
        .map(|(i, name)| format!(r#""s{i}":"fx:item/{name}""#))
        .collect();
    let faces: Vec<String> = (0..textures.len())
        .map(|i| format!(r##""{}":{{"uv":{uv},"texture":"#s{i}"}}"##, directions[i]))
        .collect();
    put(
        &mut files,
        ITEM_MODEL,
        &format!(
            r#"{{"textures":{{{}}},"elements":[{{"from":[0,0,0],"to":[16,16,16],"faces":{{{}}}}}]}}"#,
            slots.join(","),
            faces.join(",")
        ),
    );
    files
}

/// A vertical strip of `size` by `size` frames, each one colour.
fn strip(size: u32, colours: &[u32]) -> Vec<u8> {
    png(size, size * colours.len() as u32, |_, y| {
        colours[(y / size) as usize]
    })
}

/// [`test_pack`] with `strip` as `fx:item/a`, its metadata `meta`, drawn by the box.
fn animated(meta: &str, strip: Vec<u8>) -> Files {
    let mut files = with_box("[0,0,16,16]", &["a"]);
    files.insert(texture("a"), strip);
    put(&mut files, &format!("{}.mcmeta", texture("a")), meta);
    files
}

// identical-sprites

#[test]
fn identical_textures_keep_one_file_and_the_model_names_it() {
    let fx = Fixture::new();
    let mut files = with_box("[0,0,16,16]", &["a", "a2"]);
    files.insert(texture("a"), gradient(4, 4, 0xFF00_0000));
    files.insert(texture("a2"), gradient(4, 4, 0xFF00_0000));

    let ran = fx.same(&files, &[], &IdenticalSprites);

    assert!(ran.out.contains_key(&texture("a")));
    assert!(!ran.out.contains_key(&texture("a2")));
    assert_eq!(
        ran.out[ITEM_MODEL].len(),
        files[ITEM_MODEL].len() - 1,
        "fx:item/a2 became fx:item/a"
    );
    assert!(ran.text(ITEM_MODEL).contains(r#""s1":"fx:item/a""#));
    assert_eq!(
        ran.changes(),
        [
            "1 sprites merged into an identical one",
            &format!("{} B of PNG removed", files[&texture("a2")].len())
        ]
    );
}

#[test]
fn identical_textures_merge_when_every_atlas_file_defining_the_sprite_reads_it() {
    // Each atlas file is read at its own sources: blocks lists `a2` then `a`, items `a` then `a2`.
    let fx = Fixture::new();
    let mut files = with_box("[0,0,16,16]", &["a", "a2"]);
    files.insert(texture("a"), gradient(4, 4, 0xFF00_0000));
    files.insert(texture("a2"), gradient(4, 4, 0xFF00_0000));
    put(
        &mut files,
        "assets/minecraft/atlases/blocks.json",
        r#"{"sources": [{"type": "single", "resource": "fx:item/a2"}, {"type": "single", "resource": "fx:item/a"}]}"#,
    );
    put(
        &mut files,
        "assets/minecraft/atlases/items.json",
        r#"{"sources": [{"type": "single", "resource": "fx:item/a"}, {"type": "single", "resource": "fx:item/a2"}]}"#,
    );

    let ran = fx.same(&files, &[], &IdenticalSprites);

    assert!(!ran.out.contains_key(&texture("a2")));
    assert!(ran.text(ITEM_MODEL).contains(r#""s1":"fx:item/a""#));
}

#[test]
fn textures_that_differ_by_a_pixel_or_by_metadata_stay() {
    let fx = Fixture::new();
    let mut files = with_box("[0,0,16,16]", &["a", "b", "c"]);
    files.insert(texture("a"), gradient(4, 4, 0xFF00_0000));
    files.insert(
        texture("b"),
        png(4, 4, |x, y| {
            if (x, y) == (3, 3) {
                0xFFFF_0000
            } else {
                0xFF00_0000 + x + 4 * y
            }
        }),
    );
    files.insert(texture("c"), files[&texture("a")].clone());
    put(
        &mut files,
        &format!("{}.mcmeta", texture("c")),
        r#"{"texture":{"blur":true}}"#,
    );

    let ran = fx.same(&files, &[], &IdenticalSprites);

    assert_eq!(ran.changed(), 0);
    assert!(ran.out.keys().eq(files.keys()));
}

#[test]
fn identical_textures_with_the_same_metadata_merge_and_the_metadata_goes_with_them() {
    let fx = Fixture::new();
    let mut files = with_box("[0,0,16,16]", &["a", "a2"]);
    files.insert(texture("a"), gradient(4, 4, 0xFF00_0000));
    files.insert(texture("a2"), files[&texture("a")].clone());
    for name in ["a", "a2"] {
        put(
            &mut files,
            &format!("{}.mcmeta", texture(name)),
            r#"{"texture":{"blur":true}}"#,
        );
    }

    let ran = fx.same(&files, &[], &IdenticalSprites);

    assert!(!ran.out.contains_key(&texture("a2")));
    assert!(!ran.out.contains_key(&format!("{}.mcmeta", texture("a2"))));
    assert!(ran.out.contains_key(&format!("{}.mcmeta", texture("a"))));
}

#[test]
fn a_name_a_usage_source_gives_stays_and_the_other_merges_into_it() {
    let fx = Fixture::new();
    let mut files = with_box("[0,0,16,16]", &["a", "a2"]);
    files.insert(texture("a"), gradient(4, 4, 0xFF00_0000));
    files.insert(texture("a2"), files[&texture("a")].clone());

    let ran = fx.same(&files, &["fx:item/a2"], &IdenticalSprites);

    assert!(
        ran.out.contains_key(&texture("a2")),
        "named from outside the pack: it keeps its name"
    );
    assert!(!ran.out.contains_key(&texture("a")));

    let both = fx.same(&files, &["fx:item/a", "fx:item/a2"], &IdenticalSprites);

    assert!(both.out.keys().eq(files.keys()));
}

#[test]
fn a_texture_a_font_loads_is_never_merged_away() {
    let fx = Fixture::new();
    let mut files = with_box("[0,0,16,16]", &["a", "sheet"]);
    files.insert(texture("a"), gradient(8, 8, 0xFFFF_FF00));
    files.insert(texture("sheet"), files[&texture("a")].clone());
    put(
        &mut files,
        FONT,
        r#"{"providers":[{"type":"bitmap","file":"fx:item/sheet.png","ascent":7,"chars":[""]}]}"#,
    );

    let ran = fx.same(&files, &[], &IdenticalSprites);

    assert!(ran.out.contains_key(&texture("sheet")));
    assert_eq!(ran.out[FONT], files[FONT]);
}

#[test]
fn a_texture_an_equipment_layer_or_a_particle_names_is_never_merged_away() {
    let fx = Fixture::new();
    let mut files = with_box("[0,0,16,16]", &["a", "worn"]);
    files.insert(texture("a"), gradient(4, 4, 0xFF00_0000));
    files.insert(texture("worn"), files[&texture("a")].clone());
    let particle = "assets/fx/particles/spark.json";
    put(&mut files, particle, r#"{"textures":["fx:item/worn"]}"#);

    // Named by path, so the prune before the pass keeps it: no item of the pack reaches a particle.
    let ran = fx.same(&files, &[particle], &IdenticalSprites);

    assert!(ran.out.contains_key(&texture("worn")));
    assert_eq!(ran.out[particle], files[particle]);
}

// animation-frames

#[test]
fn repeated_frames_are_stored_once_and_the_list_keeps_the_order_and_the_times() {
    let fx = Fixture::new();
    let files = animated(
        r#"{"animation":{"frametime":2,"frames":[0,1,{"index":2,"time":7},3]}}"#,
        strip(4, &[0xFFFF_0000, 0xFF00_FF00, 0xFFFF_0000, 0xFF00_FF00]),
    );

    let ran = fx.same(&files, &[], &AnimationFrames);

    let out = decode(&ran.out[&texture("a")]);
    assert_eq!((out.width, out.height), (4, 8), "two distinct frames");
    assert_eq!(argb(&out, 0, 4), 0xFF00_FF00);
    assert_eq!(
        ran.text(&format!("{}.mcmeta", texture("a"))),
        r#"{"animation":{"frametime":2,"frames":[0,1,{"index":0,"time":7},1]}}"#
    );
    assert!(
        ran.changes()
            .iter()
            .any(|line| line == "2 repeated frames removed"),
        "{:?}",
        ran.changes()
    );
}

#[test]
fn an_interpolated_animation_plays_the_same_frames_in_the_same_order() {
    let fx = Fixture::new();
    let files = animated(
        r#"{"animation":{"interpolate":true}}"#,
        strip(4, &[0xFFFF_0000, 0xFFFF_0000, 0xFF00_FF00]),
    );

    let ran = fx.same(&files, &[], &AnimationFrames);

    assert_eq!(ran.size(&texture("a")), (4, 8));
    assert_eq!(
        ran.text(&format!("{}.mcmeta", texture("a"))),
        r#"{"animation":{"interpolate":true,"frames":[0,0,1]}}"#
    );
}

#[test]
fn a_strip_without_repeats_is_left_alone() {
    let fx = Fixture::new();
    let files = animated(
        r#"{"animation":{}}"#,
        strip(4, &[0xFFFF_0000, 0xFF00_FF00, 0xFF00_00FF]),
    );

    assert_eq!(fx.same(&files, &[], &AnimationFrames).changed(), 0);
}

#[test]
fn a_strip_a_font_loads_is_left_alone_because_the_font_ignores_the_animation() {
    let fx = Fixture::new();
    let mut files = animated(r#"{"animation":{}}"#, strip(4, &[0xFFFF_0000, 0xFFFF_0000]));
    put(
        &mut files,
        FONT,
        r#"{"providers":[{"type":"bitmap","file":"fx:item/a.png","ascent":7,"chars":["",""]}]}"#,
    );

    assert_eq!(fx.same(&files, &[], &AnimationFrames).changed(), 0);
}

/// The client plays an animation of fewer than two frames as the strip's first cell, whatever cell the list names; the
/// strip repeats a cell, so only that rule leaves it alone.
#[test]
fn a_list_of_one_frame_shows_the_first_cell_and_is_left_alone() {
    let fx = Fixture::new();
    let files = animated(
        r#"{"animation":{"frames":[1]}}"#,
        strip(4, &[0xFFFF_0000, 0xFF00_FF00, 0xFF00_FF00]),
    );

    assert_eq!(fx.same(&files, &[], &AnimationFrames).changed(), 0);
}

#[test]
fn a_strip_whose_list_starts_at_another_cell_keeps_the_first_cell_first() {
    let fx = Fixture::new();
    let files = animated(
        r#"{"animation":{"frames":[1,2,3]}}"#,
        strip(4, &[0xFFFF_0000, 0xFF00_FF00, 0xFF00_00FF, 0xFF00_FF00]),
    );

    let ran = fx.same(&files, &[], &AnimationFrames);

    let out = decode(&ran.out[&texture("a")]);
    assert_eq!(out.height, 12, "the first cell and the two distinct frames");
    assert_eq!(argb(&out, 0, 0), 0xFFFF_0000);
    assert_eq!(
        ran.text(&format!("{}.mcmeta", texture("a"))),
        r#"{"animation":{"frames":[1,2,1]}}"#
    );
}

#[test]
fn a_strip_a_name_that_stays_gives_is_left_alone_because_that_name_is_compared_whole() {
    let fx = Fixture::new();
    let files = animated(r#"{"animation":{}}"#, strip(4, &[0xFFFF_0000, 0xFFFF_0000]));

    assert_eq!(
        fx.same(&files, &["fx:item/a"], &AnimationFrames).changed(),
        0
    );
}

#[test]
fn the_two_passes_together_are_the_same_for_the_player() {
    let fx = Fixture::new();
    let mut files = with_box("[0,0,4,4]", &["a", "a2", "moving"]);
    files.insert(texture("a"), gradient(16, 16, 0xFF10_2030));
    files.insert(texture("a2"), files[&texture("a")].clone());
    files.insert(
        texture("moving"),
        strip(16, &[0xFFFF_0000, 0xFFFF_0000, 0xFF00_FF00]),
    );
    put(
        &mut files,
        &format!("{}.mcmeta", texture("moving")),
        r#"{"animation":{}}"#,
    );

    let proof = fx
        .prove(
            &files,
            &[],
            &[&IdenticalSprites, &AnimationFrames],
            packprune_pass::Scope::all(),
        )
        .unwrap();

    let order: Vec<&str> = proof.reports.iter().map(|report| report.pass).collect();
    assert_eq!(order, ["animation-frames", "identical-sprites"]);
    assert!(
        !proof
            .output
            .pack
            .files
            .keys()
            .any(|path| path.as_str() == texture("a2"))
    );
}

/// Rewritten metadata is compact: members in place, doubles as `Double.toString` writes them, control characters
/// escaped.
#[test]
fn rewritten_metadata_is_written_compact_with_members_in_place() {
    let meta = r#"{"animation": {"frametime": 2, "frames": [0, 1, 0]}, "x": "a/b\u001fé\n", "y": 12345678901, "z": 1e2}"#;
    let strip = strip(1, &[0xFF00_0000, 0xFF00_0000]);

    let out = super::animation::dedupe(&strip, meta.as_bytes()).unwrap();

    assert_eq!(
        String::from_utf8(out.meta).unwrap(),
        "{\"animation\":{\"frametime\":2,\"frames\":[0,0,0]},\"x\":\"a/b\\u001F\u{e9}\\n\",\"y\":12345678901,\"z\":100.0}"
    );
}

/// A member the animation does not read may hold a number that overflows a double: it is written back as the string
/// `"Infinity"`, so the metadata stays JSON.
#[test]
fn an_overflowing_number_in_the_metadata_is_written_back_as_a_string() {
    let fx = Fixture::new();
    let files = animated(
        r#"{"animation":{"custom":1e999}}"#,
        strip(4, &[0xFFFF_0000, 0xFFFF_0000, 0xFF00_FF00]),
    );

    let ran = fx.same(&files, &[], &AnimationFrames);

    assert_eq!(
        ran.text(&format!("{}.mcmeta", texture("a"))),
        r#"{"animation":{"custom":"Infinity","frames":[0,0,1]}}"#
    );
}
