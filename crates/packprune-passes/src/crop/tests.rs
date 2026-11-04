//! uv-crop and uv-crop-particles: each lossless run goes through the harness, whose oracle must find the result the
//! same for the player.

use packprune_pack::rules::Glob;
use packprune_pass::{Loss, Scope};

use super::uv_crop::UvCrop;
use crate::textures::tests::support::{Files, Fixture, Ran, decode, files_of, png, put, text};

const MODEL: &str = "assets/fx/models/item/box.json";
const SKIN: &str = "assets/fx/textures/item/skin.png";
const BLOCK_SKIN: &str = "assets/fx/textures/block/skin.png";

const EXACT: UvCrop = UvCrop { particles: false };
const PARTICLES: UvCrop = UvCrop { particles: true };

/// An item drawn by [`MODEL`], whose one cube has `faces` and samples `fx:item/skin` as `#skin`.
fn crop_pack(faces: &str, width: u32, height: u32) -> Files {
    let mut files = Files::new();
    put(
        &mut files,
        "pack.mcmeta",
        r#"{"pack": {"min_format": 88, "max_format": 88, "description": "t"}}"#,
    );
    item(&mut files, "box");
    put(
        &mut files,
        MODEL,
        &format!(
            r#"{{"textures": {{"skin": "fx:item/skin"}},
 "elements": [{{"from": [0, 0, 0], "to": [16, 16, 16], "faces": {{{faces}}}}}]}}"#
        ),
    );
    files.insert(SKIN.into(), coloured(width, height));
    files
}

fn item(files: &mut Files, name: &str) {
    put(
        files,
        &format!("assets/fx/items/{name}.json"),
        &format!(r#"{{"model": {{"type": "minecraft:model", "model": "fx:item/{name}"}}}}"#),
    );
}

/// Every pixel differs: `(x, y)` is in the colour.
fn coloured(width: u32, height: u32) -> Vec<u8> {
    png(width, height, |x, y| 0xFF00_0000 | x << 12 | y)
}

fn crop(files: &Files) -> Ran {
    Fixture::new().same(files, &[], &EXACT)
}

fn replace(files: &mut Files, path: &str, from: &str, to: &str) {
    let replaced = text(files, path).replace(from, to);
    assert_ne!(replaced, text(files, path), "{from} in {path}");
    put(files, path, &replaced);
}

#[test]
fn the_report_gives_the_texels_kept() {
    let ran = crop(&crop_pack(
        r##""north": {"uv": [4, 4, 8, 12], "texture": "#skin"}"##,
        16,
        16,
    ));

    assert!(
        ran.changes()
            .iter()
            .any(|line| line == "1 textures cropped, 256 -> 32 texels (-87.5 %)"),
        "{:?}",
        ran.changes()
    );
}

#[test]
fn a_rotated_face_samples_the_same_texels_of_the_cropped_texture() {
    let ran = crop(&crop_pack(
        r##""north": {"uv": [4, 4, 8, 12], "rotation": 90, "texture": "#skin"}"##,
        16,
        16,
    ));

    assert_eq!(ran.size(SKIN), (4, 8));
    assert!(
        ran.text(MODEL)
            .contains(r#""uv": [0, 0, 16, 16], "rotation": 90"#),
        "{}",
        ran.text(MODEL)
    );
}

#[test]
fn a_face_whose_uv_runs_right_to_left_keeps_its_direction() {
    let ran = crop(&crop_pack(
        r##""south": {"uv": [12, 0, 4, 4], "texture": "#skin"}, "up": {"uv": [6, 2, 7, 3], "texture": "#skin"}"##,
        16,
        16,
    ));

    assert_eq!(ran.size(SKIN), (8, 4));
    assert!(
        ran.text(MODEL).contains(r#""uv": [16, 0, 0, 16]"#),
        "{}",
        ran.text(MODEL)
    );
    assert!(
        ran.text(MODEL).contains(r#""uv": [4, 8, 6, 12]"#),
        "{}",
        ran.text(MODEL)
    );
}

#[test]
fn a_texture_a_generated_layer_also_draws_stays_whole() {
    let mut files = crop_pack(
        r##""north": {"uv": [0, 0, 4, 4], "texture": "#skin"}"##,
        16,
        16,
    );
    item(&mut files, "flat");
    // Its own particle, so only the layer draws the skin whole.
    put(
        &mut files,
        "assets/fx/models/item/flat.json",
        r#"{"parent": "builtin/generated", "textures": {"layer0": "fx:item/skin", "particle": "minecraft:block/stone"}}"#,
    );
    let other = "assets/fx/textures/item/other.png";
    files.insert(other.into(), coloured(16, 16));
    put(
        &mut files,
        "assets/fx/models/item/box2.json",
        r##"{"textures": {"skin": "fx:item/other"}, "elements": [{"from": [0, 0, 0], "to": [16, 16, 16],
 "faces": {"north": {"uv": [0, 0, 4, 4], "texture": "#skin"}}}]}"##,
    );
    item(&mut files, "box2");
    item(&mut files, "flat2");
    put(
        &mut files,
        "assets/fx/models/item/flat2.json",
        r#"{"parent": "minecraft:item/generated", "textures": {"layer0": "fx:item/other"}}"#,
    );

    let ran = crop(&files);

    assert_eq!(ran.out[SKIN], files[SKIN]);
    assert_eq!(
        ran.out[other], files[other],
        "a vanilla parent's layers are generated too"
    );
    assert_eq!(ran.out[MODEL], files[MODEL]);
}

#[test]
fn a_particle_texture_stays_whole() {
    let mut files = crop_pack(
        r##""north": {"uv": [0, 0, 4, 4], "texture": "#skin"}"##,
        16,
        16,
    );
    replace(
        &mut files,
        MODEL,
        r#"{"skin": "fx:item/skin"}"#,
        r##"{"skin": "fx:item/skin", "particle": "#skin"}"##,
    );

    let ran = crop(&files);

    assert_eq!(ran.out[SKIN], files[SKIN]);
}

#[test]
fn every_frame_of_an_animated_strip_is_cropped_the_same() {
    let mut files = crop_pack(
        r##""north": {"uv": [8, 8, 16, 16], "texture": "#skin"}"##,
        8,
        24,
    );
    put(
        &mut files,
        &format!("{SKIN}.mcmeta"),
        r#"{"animation": {"frametime": 2}}"#,
    );
    let sized = "assets/fx/textures/item/sized.png";
    files.insert(sized.into(), coloured(8, 24));
    put(
        &mut files,
        &format!("{sized}.mcmeta"),
        r#"{"animation": {"width": 8, "height": 8}}"#,
    );
    item(&mut files, "box2");
    put(
        &mut files,
        "assets/fx/models/item/box2.json",
        r##"{"textures": {"skin": "fx:item/sized"}, "elements": [{"from": [0, 0, 0], "to": [16, 16, 16],
 "faces": {"north": {"uv": [0, 0, 8, 16], "texture": "#skin"}}}]}"##,
    );

    let ran = crop(&files);

    assert_eq!(
        ran.size(SKIN),
        (4, 12),
        "three 4 by 4 frames: the default frame size stays square"
    );
    let meta = format!("{SKIN}.mcmeta");
    assert_eq!(ran.out[&meta], files[&meta]);
    assert_eq!(ran.size(sized), (4, 24));
    assert_eq!(
        ran.text(&format!("{sized}.mcmeta")),
        r#"{"animation": {"width": 4, "height": 8}}"#
    );
}

/// A default frame stays square: the smallest box of each axis alone would make it 4 by 2, which the client would
/// read as frames of 4 by 4.
#[test]
fn a_default_frame_is_cropped_to_a_square() {
    let mut files = crop_pack(
        r##""north": {"uv": [8, 8, 16, 12], "texture": "#skin"}"##,
        8,
        24,
    );
    put(
        &mut files,
        &format!("{SKIN}.mcmeta"),
        r#"{"animation": {"frametime": 2}}"#,
    );

    let ran = crop(&files);

    assert_eq!(ran.size(SKIN), (4, 12));
}

/// [`crop_pack`] with the skin at `fx:block/skin`, on the blocks atlas the fixtures' vanilla stitches mipmapped.
fn on_blocks(faces: &str, skin: Vec<u8>) -> Files {
    let mut files = crop_pack(faces, 1, 1);
    files.remove(SKIN);
    files.insert(BLOCK_SKIN.into(), skin);
    replace(&mut files, MODEL, "fx:item/skin", "fx:block/skin");
    files
}

#[test]
fn on_a_mipmapped_atlas_every_mip_level_of_what_is_kept_stays_the_same() {
    // Opaque, so the mean strategy; its 64 texels make the atlas's mip level 4, so the box is aligned to 16.
    let colour = |x: u32, y: u32| {
        0xFF00_0000
            | ((x * 37 + y * 11) % 256) << 16
            | ((x * 5 + y * 23) % 256) << 8
            | ((x ^ y) * 3 % 256)
    };
    let skin = png(64, 64, colour);
    let ran = crop(&on_blocks(
        r##""north": {"uv": [5, 4.5, 7.5, 10], "texture": "#skin"}"##,
        skin.clone(),
    ));

    let cropped = decode(&ran.out[BLOCK_SKIN]);
    assert_eq!(
        (cropped.width, cropped.height),
        (16, 32),
        "texels 20-30 by 18-40, on the 16 block"
    );
    let original = mipmaps::mean(&decode(&skin), 4);
    let kept = mipmaps::mean(&cropped, 4);
    for level in 0..=4 {
        let (width, height) = (16 >> level, 32 >> level);
        for y in 0..height {
            for x in 0..width {
                assert_eq!(
                    original[level][(16 >> level) + x + ((16 >> level) + y) * (64 >> level)],
                    kept[level][x + y * width],
                    "level {level} texel {x},{y}"
                );
            }
        }
    }
}

#[test]
fn a_transparent_texture_on_a_mipmapped_atlas_stays_whole() {
    let skin = png(64, 64, |x, y| {
        if (x, y) == (63, 63) {
            0
        } else {
            0xFF00_0000 | x << 8 | y
        }
    });
    let files = on_blocks(
        r##""north": {"uv": [0, 0, 4, 4], "texture": "#skin"}"##,
        skin,
    );

    let ran = crop(&files);

    assert_eq!(
        ran.out[BLOCK_SKIN], files[BLOCK_SKIN],
        "its cutout mipmaps read the whole image"
    );
    assert!(
        ran.changes()
            .iter()
            .any(|line| line == "left whole: 1 a cutout mipmap strategy on a mipmapped atlas"),
        "{:?}",
        ran.changes()
    );
}

#[test]
fn a_texture_a_uv_locked_variant_turns_stays_whole() {
    // y 90 with uvlock makes the up face sample texels 48-64 by 0-16, not the 0-16 its UVs name.
    let skin = png(64, 64, |x, y| 0xFF00_0000 | x << 8 | y);
    let mut files = on_blocks(r##""up": {"uv": [0, 0, 4, 4], "texture": "#skin"}"##, skin);
    put(
        &mut files,
        "assets/minecraft/blockstates/stone.json",
        r#"{"variants": {"": {"model": "fx:item/box", "y": 90, "uvlock": true}}}"#,
    );

    let ran = crop(&files);

    assert_eq!(ran.out[BLOCK_SKIN], files[BLOCK_SKIN]);
    assert!(
        ran.changes()
            .iter()
            .any(|line| line == "left whole: 1 a face a uvlock rotation moves"),
        "{:?}",
        ran.changes()
    );
}

#[test]
fn a_texture_of_a_model_at_a_vanilla_block_path_stays_whole() {
    // Vanilla's block states, which may turn it with uvlock, are not read.
    let skin = png(64, 64, |x, y| 0xFF00_0000 | x << 8 | y);
    let mut files = on_blocks(r##""up": {"uv": [0, 0, 4, 4], "texture": "#skin"}"##, skin);
    let vanilla_path = "assets/minecraft/models/block/cube_all.json";
    let model = files.remove(MODEL).unwrap();
    files.insert(vanilla_path.into(), model);
    put(
        &mut files,
        "assets/fx/items/box.json",
        r#"{"model": {"type": "minecraft:model", "model": "minecraft:block/cube_all"}}"#,
    );

    let ran = crop(&files);

    assert_eq!(ran.out[BLOCK_SKIN], files[BLOCK_SKIN]);
}

#[test]
fn before_atlas_files_a_texture_a_model_names_is_cropped_on_the_mip_block_of_the_blocks_atlas() {
    let mut files = crop_pack(
        r##""north": {"uv": [5, 4.5, 7.5, 10], "texture": "#skin"}"##,
        1,
        1,
    );
    put(
        &mut files,
        "pack.mcmeta",
        r#"{"pack": {"pack_format": 88, "supported_formats": [9, 88], "description": "t"}, "overlays": {"entries": [{"directory": "modern", "formats": [12, 88]}]}}"#,
    );
    let custom = "assets/fx/textures/custom/skin.png";
    files.remove(SKIN);
    files.insert(custom.into(), coloured(64, 64));
    replace(&mut files, MODEL, "fx:item/skin", "fx:custom/skin");
    // Atlas files list it from format 12, on the items atlas; format 9 stitches it into blocks because the model
    // names it.
    put(
        &mut files,
        "modern/assets/minecraft/atlases/items.json",
        r#"{"sources": [{"type": "single", "resource": "fx:custom/skin"}]}"#,
    );

    let ran = crop(&files);

    assert_eq!(ran.size(custom), (16, 32));
}

#[test]
fn a_parent_an_overlay_replaces_adds_the_faces_of_its_view() {
    // The model's own file is the same in both views; the parent each view resolves is not, and both draw the skin.
    let mut files = crop_pack("", 16, 16);
    put(
        &mut files,
        "pack.mcmeta",
        r#"{"pack": {"min_format": 87, "max_format": 88, "description": "t"}, "overlays": {"entries": [{"directory": "alt", "formats": [88, 88]}]}}"#,
    );
    put(
        &mut files,
        MODEL,
        r#"{"parent": "fx:item/base", "textures": {"skin": "fx:item/skin"}}"#,
    );
    let base = |uv: &str| {
        format!(
            r##"{{"elements": [{{"from": [0, 0, 0], "to": [16, 16, 16], "faces": {{"north": {{"uv": {uv}, "texture": "#skin"}}}}}}]}}"##
        )
    };
    put(
        &mut files,
        "assets/fx/models/item/base.json",
        &base("[4, 4, 8, 12]"),
    );
    put(
        &mut files,
        "alt/assets/fx/models/item/base.json",
        &base("[8, 4, 12, 12]"),
    );

    let ran = crop(&files);

    assert_eq!(ran.size(SKIN), (8, 8));
}

#[test]
fn a_texture_whose_mcmeta_an_overlay_replaces_stays_whole() {
    let mut files = crop_pack(
        r##""north": {"uv": [4, 4, 8, 12], "texture": "#skin"}"##,
        16,
        16,
    );
    put(
        &mut files,
        "pack.mcmeta",
        r#"{"pack": {"min_format": 87, "max_format": 88, "description": "t"}, "overlays": {"entries": [{"directory": "alt", "formats": [88, 88]}]}}"#,
    );
    put(&mut files, &format!("{SKIN}.mcmeta"), r#"{"texture": {}}"#);
    put(
        &mut files,
        &format!("alt/{SKIN}.mcmeta"),
        r#"{"texture": {"blur": false}}"#,
    );

    let ran = crop(&files);

    assert_eq!(ran.out[SKIN], files[SKIN]);
}

#[test]
fn the_particle_variant_is_lossy_off_by_default_and_reports_what_it_cropped() {
    let fx = Fixture::new();
    let mut files = crop_pack(
        r##""north": {"uv": [0, 0, 4, 4], "texture": "#skin"}"##,
        16,
        16,
    );
    replace(
        &mut files,
        MODEL,
        r#"{"skin": "fx:item/skin"}"#,
        r##"{"skin": "fx:item/skin", "particle": "#skin"}"##,
    );
    let lossless: Vec<&str> = crate::passes()
        .iter()
        .filter(|pass| pass.loss() == Loss::Lossless)
        .map(|pass| pass.name())
        .collect();
    assert!(lossless.contains(&"uv-crop") && !lossless.contains(&"uv-crop-particles"));
    let lossy = |glob: &str| {
        let glob = Glob::path(glob);
        fx.prove(
            &files,
            &[],
            &[&PARTICLES],
            Scope::matching(move |path| glob.matches(path.as_str())),
        )
        .unwrap()
    };

    let outside = lossy("assets/fx/textures/**");
    let ran = lossy("assets/**");

    assert_eq!(
        &*outside.output.pack.files[&packprune_pack::PhysPath::new(SKIN).unwrap()].blob,
        files[SKIN].as_slice(),
        "the model is outside its globs"
    );
    let out = files_of(&ran.output.pack);
    let skin = decode(&out[SKIN]);
    assert_eq!((skin.width, skin.height), (4, 4));
    assert!(
        ran.reports[0].changes.iter().any(|line| line
            == "1 particle sprites cropped, 256 -> 16 texels: their particles show only the texels the faces sample"),
        "{:?}",
        ran.reports[0].changes
    );
    let loss = ran.loss.as_ref().unwrap();
    assert_eq!(loss.differing_keys, 1, "{:?}", loss.differences);
    assert!(
        loss.differences
            .iter()
            .all(|difference| difference.detail.starts_with("/model/model/particle/")),
        "{:?}",
        loss.differences
    );
}

#[test]
fn a_crop_no_float_keeps_exact_is_skipped_and_reported() {
    // On 48 texels, x samples 0, 1.5 and 40.5: no width from 41 to 47 holds all three as floats.
    let files = crop_pack(
        r##""north": {"uv": [0, 0, 0.5, 16], "texture": "#skin"}, "south": {"uv": [0.5, 0, 13.5, 16], "texture": "#skin"}"##,
        48,
        16,
    );

    let ran = crop(&files);

    assert_eq!(ran.out[SKIN], files[SKIN]);
    assert!(
        ran.changes()
            .iter()
            .any(|line| *line
                == format!("1 textures left whole, no crop keeps their UVs exact: {SKIN}")),
        "{:?}",
        ran.changes()
    );
}

/// The 26.2 client's mip chain for the mean strategy:
/// `MipmapGenerator.generateMipLevels` halves each level, each texel `ARGB.meanLinear` of the 2x2 block under it,
/// through the sRGB tables of `ARGB`.
mod mipmaps {
    use packprune_codec::png::Image;

    fn tables() -> ([i32; 256], [u8; 1024]) {
        let mut to_linear = [0i32; 256];
        for (i, slot) in to_linear.iter_mut().enumerate() {
            let channel = i as f32 / 255.0;
            let linear = if channel >= 0.04045 {
                ((f64::from(channel) + 0.055) / 1.055).powf(2.4) as f32
            } else {
                channel / 12.92
            };
            *slot = (linear * 1023.0).round() as i32;
        }
        let mut to_srgb = [0u8; 1024];
        for (i, slot) in to_srgb.iter_mut().enumerate() {
            let channel = i as f32 / 1023.0;
            let srgb = if channel >= 0.003_130_8 {
                (1.055 * f64::from(channel).powf(0.416_666_666_666_666_7) - 0.055) as f32
            } else {
                12.92 * channel
            };
            *slot = (srgb * 255.0).round() as u8;
        }
        (to_linear, to_srgb)
    }

    /// Levels 0 to `levels` of `image`, each row by row, as ARGB ints.
    pub(super) fn mean(image: &Image, levels: usize) -> Vec<Vec<u32>> {
        let (to_linear, to_srgb) = tables();
        let (mut width, mut height) = (image.width as usize, image.height as usize);
        let mut chain = vec![
            image
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| u32::from_be_bytes([p[3], p[0], p[1], p[2]]))
                .collect::<Vec<_>>(),
        ];
        for _ in 0..levels {
            let last = chain.last().unwrap();
            let last_width = width;
            width >>= 1;
            height >>= 1;
            let mut data = vec![0u32; width * height];
            for x in 0..width {
                for y in 0..height {
                    let c = [
                        last[x * 2 + y * 2 * last_width],
                        last[x * 2 + 1 + y * 2 * last_width],
                        last[x * 2 + (y * 2 + 1) * last_width],
                        last[x * 2 + 1 + (y * 2 + 1) * last_width],
                    ];
                    let alpha = c.iter().map(|c| c >> 24).sum::<u32>() / 4;
                    let channel = |shift: u32| {
                        let linear: i32 = c
                            .iter()
                            .map(|c| to_linear[(c >> shift & 0xFF) as usize])
                            .sum::<i32>()
                            / 4;
                        u32::from(to_srgb[linear as usize])
                    };
                    data[x + y * width] =
                        alpha << 24 | channel(16) << 16 | channel(8) << 8 | channel(0);
                }
            }
            chain.push(data);
        }
        chain
    }
}
