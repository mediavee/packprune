use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use packprune_codec::DEFAULT_EFFORT;
use packprune_codec::png::{self, Image};
use packprune_pack::{
    ClientRules, FileKind, Limits, LogicalPath, PackInput, PhysPath, Quirks, RunCx, VanillaIndex,
};
use packprune_pass::{
    Context, Enabled, FileCx, Kept, NoCache, Outcome, Pass, Proof, Scope, prove, run,
};

pub(crate) type Files = BTreeMap<String, Vec<u8>>;

#[test]
fn options_validate_at_load() {
    for pass in super::passes() {
        assert!(pass.validate_options(&toml::Table::new()).is_ok());
        assert!(
            pass.validate_options(&"unknown = 1".parse().unwrap())
                .is_err()
        );
        if pass.name() == "texel-downscale" {
            assert!(
                pass.validate_options(
                    &"ceiling = 8\nmin_ssim = 0.9\nmax_delta = 32"
                        .parse()
                        .unwrap()
                )
                .is_ok()
            );
            for invalid in [
                "ceiling = 0",
                "ceiling = nan",
                "min_ssim = 2",
                "max_delta = 256",
                "ceiling = 'eight'",
            ] {
                assert!(
                    pass.validate_options(&invalid.parse().unwrap()).is_err(),
                    "{invalid}"
                );
            }
        }
    }
    for optimiser in super::optimisers() {
        assert!(optimiser.validate_options(&toml::Table::new()).is_ok());
        assert!(
            optimiser
                .validate_options(
                    &"effort = 1\nmin_ssim = 0.9\nmax_delta = 32"
                        .parse()
                        .unwrap()
                )
                .is_ok()
        );
        for option in ["max_colors = 16", "quality = 80", "dithering = 0.5"] {
            assert_eq!(
                optimiser.validate_options(&option.parse().unwrap()).is_ok(),
                optimiser.name() == "png-quantize",
                "{}: {option}",
                optimiser.name()
            );
        }
        for invalid in [
            "unknown = 1",
            "max_colors = 1",
            "max_colors = 257",
            "quality = 101",
            "dithering = nan",
            "dithering = -1",
            "min_ssim = 2",
            "max_delta = 256",
            "effort = 'high'",
        ] {
            assert!(
                optimiser
                    .validate_options(&invalid.parse().unwrap())
                    .is_err(),
                "{}: {invalid}",
                optimiser.name()
            );
        }
    }
}

pub(crate) const BIG: &str = "assets/fx/textures/item/big.png";
const MODEL: &str = "assets/fx/models/item/tool.json";
const ATLAS: &str = "assets/minecraft/atlases/items.json";
const NORTH: &str = r##"{"from":[0,0,0],"to":[16,16,16],"faces":{"north":{"texture":"#a"}}}"##;
const WIDE: &str =
    r##"{"from":[0,0,0],"to":[32,32,1],"faces":{"south":{"uv":[0,0,16,16],"texture":"#a"}}}"##;

pub(crate) struct Fixture {
    pub run: RunCx,
    pub vanilla: VanillaIndex,
    pub rules: ClientRules,
    pub client: PackInput,
}

impl Fixture {
    pub fn new() -> Self {
        let path = format!(
            "{}/../../tests/fixtures/vanilla",
            env!("CARGO_MANIFEST_DIR")
        );
        let client = PackInput::open(path.as_str().as_ref(), &Limits::default()).unwrap();
        let vanilla = VanillaIndex {
            version: "26.2".into(),
            pack_format: "88.0".into(),
            names: client.files.keys().map(ToString::to_string).collect(),
            atlases: client
                .files
                .iter()
                .filter(|(path, _)| path.as_str().contains("/atlases/"))
                .map(|(path, entry)| (path.to_string(), entry.blob.to_vec()))
                .collect(),
            ..Default::default()
        };
        Self {
            run: RunCx::default(),
            rules: ClientRules::for_version(&vanilla.version),
            vanilla,
            client,
        }
    }

    pub fn context(&self) -> Context<'_> {
        Context {
            run: &self.run,
            vanilla: &self.vanilla,
            rules: &self.rules,
            sources: &[],
            selectors: &[],
            cache: &NoCache,
        }
    }

    pub fn execute(
        &self,
        files: &Files,
        pass: &dyn Pass,
        options: &str,
        scope: Scope,
        names: &[&str],
    ) -> Outcome {
        let cx = self.context();
        let state = cx.analyse(pack(files)).unwrap();
        let kept = Kept {
            names: names
                .iter()
                .map(|path| LogicalPath::new(path).unwrap())
                .collect(),
            pinned: BTreeSet::new(),
        };
        run(
            &cx,
            &kept,
            &state,
            &[Enabled {
                pass,
                scope,
                options: options.parse().unwrap(),
            }],
        )
        .unwrap()
    }

    pub fn prove(&self, files: &Files, pass: &dyn Pass, options: &str) -> Proof {
        prove(
            &self.context(),
            &self.client,
            pack(files),
            vec![Enabled {
                pass,
                scope: Scope::all(),
                options: options.parse().unwrap(),
            }],
        )
        .unwrap()
    }
}

pub(crate) fn pack(files: &Files) -> PackInput {
    let temp = tempfile::tempdir().unwrap();
    for (path, bytes) in files {
        let path = temp.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    PackInput::open(temp.path().to_str().unwrap().as_ref(), &Limits::default()).unwrap()
}

pub(crate) fn files() -> Files {
    BTreeMap::from([(
        "pack.mcmeta".into(),
        br#"{"pack":{"pack_format":88,"description":"test"}}"#.to_vec(),
    )])
}

pub(crate) fn text(files: &mut Files, path: &str, text: &str) {
    files.insert(path.into(), text.as_bytes().to_vec());
}
pub(crate) fn physical(path: &str) -> PhysPath {
    PhysPath::new(path).unwrap()
}
pub(crate) fn bytes<'a>(out: &'a Outcome, path: &str) -> &'a [u8] {
    &out.state.pack.files[&physical(path)].blob
}

fn model(files: &mut Files, elements: &[&str]) {
    text(
        files,
        MODEL,
        &format!(
            r##"{{"textures":{{"a":"fx:item/big"}},"elements":[{}]}}"##,
            elements.join(",")
        ),
    );
}

fn rgba_png(image: &Image) -> Result<Vec<u8>, ::png::EncodingError> {
    let mut out = Vec::new();
    let mut encoder = ::png::Encoder::new(&mut out, image.width, image.height);
    encoder.set_color(::png::ColorType::Rgba);
    encoder.set_depth(::png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&image.rgba)?;
    writer.finish()?;
    Ok(out)
}

fn texture(size: u32, elements: &[&str]) -> Files {
    let mut files = files();
    model(&mut files, elements);
    text(
        &mut files,
        "assets/fx/items/tool.json",
        r#"{"model":{"type":"minecraft:model","model":"fx:item/tool"}}"#,
    );
    files.insert(
        BIG.into(),
        rgba_png(&Image {
            width: size,
            height: size,
            rgba: [0, 0, 0, 255].repeat((size * size) as usize),
        })
        .unwrap(),
    );
    files
}

fn single(resource: &str) -> String {
    format!(r#"{{"type":"single","resource":"{resource}"}}"#)
}
fn atlas(files: &mut Files, sources: &[String]) {
    text(
        files,
        ATLAS,
        &format!("{{\"sources\": [{}]}}", sources.join(", ")),
    );
}
fn texel(files: &Files, options: &str) -> Outcome {
    Fixture::new().execute(
        files,
        &super::texel::TexelDownscale,
        options,
        Scope::all(),
        &[],
    )
}
fn side(out: &Outcome) -> u32 {
    png::decode(bytes(out, BIG)).unwrap().width
}

#[test]
fn texel_invalid_directory_sprite_keeps_its_texture() {
    let mut files = files();
    let path = "assets/minecraft/textures/block/not used/andesite_pillar.png";
    let image = Image {
        width: 1,
        height: 1,
        rgba: vec![128, 128, 128, 255],
    };
    files.insert(path.into(), png::encode(&image).unwrap());
    let out = texel(&files, "");
    assert_eq!(bytes(&out, path), files[path]);
}

#[test]
fn texel_largest_factor_and_nearest_loss_are_reported_and_proved() {
    let mut files = texture(256, &[NORTH]);
    let image = Image {
        width: 256,
        height: 256,
        rgba: (0..256 * 256)
            .flat_map(|i| {
                if (i % 256 + i / 256) % 2 == 0 {
                    [255; 4]
                } else {
                    [0, 0, 0, 255]
                }
            })
            .collect(),
    };
    files.insert(BIG.into(), rgba_png(&image).unwrap());
    let fixture = Fixture::new();
    let proof = fixture.prove(&files, &super::texel::TexelDownscale, "");
    assert_eq!(
        png::decode(&proof.output.pack.files[&physical(BIG)].blob)
            .unwrap()
            .width,
        64
    );
    let report = &proof.reports[0];
    assert!(report.changes.iter().any(|line| {
        line.contains("factor 4, tightest face 16 texels per unit, ceiling 4")
            && line.contains("-61440 atlas texels")
            && line.contains("max delta 127")
    }));
    assert!(
        report
            .changes
            .iter()
            .any(|line| line.contains("mip levels"))
    );
    assert_eq!(
        report
            .measured
            .iter()
            .find(|loss| loss.metric == "max_delta")
            .unwrap()
            .value,
        127.0
    );
    assert!(proof.loss.unwrap().differing_keys > 0);
    assert!(proof.proof.is_none());
}

#[test]
fn texel_tightest_face_divisibility_and_ceiling() {
    assert_eq!(side(&texel(&texture(256, &[NORTH, WIDE]), "")), 128);
    assert_eq!(side(&texel(&texture(160, &[NORTH]), "ceiling = 3")), 80);
    let files = texture(64, &[NORTH]);
    assert_eq!(bytes(&texel(&files, ""), BIG), files[BIG]);
    assert_eq!(side(&texel(&texture(256, &[NORTH]), "ceiling = 8")), 128);
}

#[test]
fn texel_rescale_and_euler_rotations_use_the_enlarged_face() {
    let turned = |rescale, euler| {
        format!(
            r##"{{"from":[0,0,0],"to":[16,16,16],"rotation":{{{},"rescale":{rescale}}},"faces":{{"north":{{"texture":"#a"}}}}}}"##,
            if euler {
                r#""x":0,"y":45,"z":0"#
            } else {
                r#""axis":"y","angle":45"#
            }
        )
    };
    for (rescale, euler, expected) in [(false, false, 64), (true, false, 128), (true, true, 128)] {
        assert_eq!(
            side(&texel(&texture(256, &[&turned(rescale, euler)]), "")),
            expected
        );
    }
}

#[test]
fn texel_font_generated_vanilla_parent_unused_slot_alias_and_animation_stay() {
    let modifiers = [
        ("assets/fx/font/default.json", r#"{"providers":[{"type":"bitmap","file":"fx:item/big.png","ascent":7,"chars":["x"]}]}"#.to_owned()),
        ("assets/fx/models/item/layered.json", r#"{"parent":"minecraft:item/generated","textures":{"layer0":"fx:item/big"}}"#.to_owned()),
        ("assets/fx/models/item/cube.json", r#"{"parent":"minecraft:block/cube_all","textures":{"all":"fx:item/big"}}"#.to_owned()),
        (MODEL, format!(r##"{{"textures":{{"a":"fx:item/big","b":"fx:item/big"}},"elements":[{NORTH}]}}"##)),
        (ATLAS, r#"{"sources":[{"type":"single","resource":"fx:item/big","sprite":"fx:alias"}]}"#.to_owned()),
        ("assets/fx/textures/item/big.png.mcmeta", r#"{"animation":{"frametime":2}}"#.to_owned()),
    ];
    for (path, change) in modifiers {
        let mut files = texture(256, &[NORTH]);
        text(&mut files, path, &change);
        assert_eq!(bytes(&texel(&files, ""), BIG), files[BIG], "{path}");
    }
}

#[test]
fn texel_inherited_elements_and_slots_are_resolved_from_the_leaf() {
    let mut files = texture(256, &[NORTH]);
    let parent = String::from_utf8(files[MODEL].clone()).unwrap();
    text(&mut files, "assets/fx/models/item/base.json", &parent);
    text(
        &mut files,
        MODEL,
        r##"{"parent":"fx:item/base","textures":{"a":"#b","b":"fx:item/big"}}"##,
    );
    assert_eq!(side(&texel(&files, "")), 64);
}

#[test]
fn texel_scope_kept_names_and_thresholds_protect_input() {
    let files = texture(256, &[NORTH]);
    let fixture = Fixture::new();
    for (scope, names) in [
        (Scope::matching(|_| false), vec![]),
        (Scope::all(), vec![BIG]),
    ] {
        let out = fixture.execute(&files, &super::texel::TexelDownscale, "", scope, &names);
        assert_eq!(bytes(&out, BIG), files[BIG]);
    }
    let mut noisy = files;
    noisy.insert(
        BIG.into(),
        rgba_png(&Image {
            width: 256,
            height: 256,
            rgba: (0..256 * 256)
                .flat_map(|i| {
                    [
                        (i % 256) as u8,
                        ((i / 256) % 256) as u8,
                        (i * 37 % 256) as u8,
                        255,
                    ]
                })
                .collect(),
        })
        .unwrap(),
    );
    assert_eq!(bytes(&texel(&noisy, "max_delta = 0"), BIG), noisy[BIG]);
    let mut vanilla = noisy.clone();
    vanilla.insert(
        "assets/minecraft/textures/block/stone.png".into(),
        noisy[BIG].clone(),
    );
    text(
        &mut vanilla,
        MODEL,
        &format!(r##"{{"textures":{{"a":"minecraft:block/stone"}},"elements":[{NORTH}]}}"##),
    );
    assert_eq!(
        bytes(
            &texel(&vanilla, ""),
            "assets/minecraft/textures/block/stone.png"
        ),
        noisy[BIG]
    );
}

#[test]
fn texel_invalid_options_are_refused() {
    let fixture = Fixture::new();
    let files = texture(256, &[NORTH]);
    let cx = fixture.context();
    let state = cx.analyse(pack(&files)).unwrap();
    for options in [
        "ceiling = 0",
        "ceiling = nan",
        "min_ssim = 2",
        "unknown = 1",
    ] {
        assert!(
            run(
                &cx,
                &Kept::default(),
                &state,
                &[Enabled {
                    pass: &super::texel::TexelDownscale,
                    scope: Scope::all(),
                    options: options.parse().unwrap()
                }]
            )
            .is_err()
        );
    }
}

#[test]
fn malformed_atlas_resource_paths_do_not_panic_or_hide_valid_textures() {
    let mut files = texture(256, &[NORTH]);
    atlas(&mut files, &[single("fx:../bad")]);
    assert_eq!(side(&texel(&files, "")), 64);
    let out = Fixture::new().execute(
        &files,
        &super::prune::PruneUnnamedSprites,
        "",
        Scope::all(),
        &[],
    );
    assert_eq!(bytes(&out, ATLAS), files[ATLAS]);
}

#[test]
fn prune_unnamed_single_and_metadata_but_keep_the_drawn_sprite() {
    let mut files = texture(16, &[NORTH]);
    let orphan = "assets/fx/textures/custom/orphan.png";
    files.insert(orphan.into(), files[BIG].clone());
    text(
        &mut files,
        &format!("{orphan}.mcmeta"),
        r#"{"texture":{"blur":true}}"#,
    );
    atlas(
        &mut files,
        &[single("fx:item/big"), single("fx:custom/orphan")],
    );
    let fixture = Fixture::new();
    let out = fixture.execute(
        &files,
        &super::prune::PruneUnnamedSprites,
        "",
        Scope::all(),
        &[],
    );
    assert_eq!(
        bytes(&out, ATLAS),
        format!("{{\"sources\":[{}]}}", single("fx:item/big")).as_bytes()
    );
    assert!(!out.state.pack.files.contains_key(&physical(orphan)));
    assert!(
        !out.state
            .pack
            .files
            .contains_key(&physical(&format!("{orphan}.mcmeta")))
    );
    assert_eq!(bytes(&out, BIG), files[BIG]);
    assert!(
        out.reports[0]
            .changes
            .iter()
            .any(|line| line.starts_with("1 sprite removed, 2 files removed"))
    );
    fixture.prove(&files, &super::prune::PruneUnnamedSprites, "");
}

#[test]
fn prune_font_and_another_sprite_keep_the_file() {
    for font in [false, true] {
        let mut files = texture(16, &[NORTH]);
        let glyph = "assets/fx/textures/font/glyph.png";
        files.insert(glyph.into(), files[BIG].clone());
        let resource = if font { "fx:font/glyph" } else { "fx:item/big" };
        if font {
            text(
                &mut files,
                "assets/fx/font/default.json",
                r#"{"providers":[{"type":"bitmap","file":"fx:font/glyph.png","ascent":7,"chars":["x"]}]}"#,
            );
        }
        atlas(
            &mut files,
            &[
                single("fx:item/big"),
                format!(r#"{{"type":"single","resource":"{resource}","sprite":"fx:alias"}}"#),
            ],
        );
        let out = Fixture::new().execute(
            &files,
            &super::prune::PruneUnnamedSprites,
            "",
            Scope::all(),
            &[],
        );
        assert_eq!(
            bytes(&out, ATLAS),
            format!("{{\"sources\":[{}]}}", single("fx:item/big")).as_bytes()
        );
        assert_eq!(
            bytes(&out, if font { glyph } else { BIG }),
            files[if font { glyph } else { BIG }]
        );
    }
}

#[test]
fn prune_kept_name_vanilla_sprite_missing_texture_and_scope_are_protected() {
    let orphan = "assets/fx/textures/custom/orphan.png";
    let mut files = texture(16, &[NORTH]);
    files.insert(orphan.into(), files[BIG].clone());
    atlas(&mut files, &[single("fx:custom/orphan")]);
    let fixture = Fixture::new();
    let out = fixture.execute(
        &files,
        &super::prune::PruneUnnamedSprites,
        "",
        Scope::all(),
        &[orphan],
    );
    assert_eq!(bytes(&out, ATLAS), files[ATLAS]);
    let out = fixture.execute(
        &files,
        &super::prune::PruneUnnamedSprites,
        "",
        Scope::matching(|p| p.as_str().starts_with("assets/fx/")),
        &[],
    );
    assert_eq!(bytes(&out, ATLAS), files[ATLAS]);
    let out = fixture.execute(
        &files,
        &super::prune::PruneUnnamedSprites,
        "",
        Scope::matching(|p| p.as_str() == ATLAS),
        &[],
    );
    assert_eq!(bytes(&out, orphan), files[orphan]);
    files.remove(orphan);
    let out = fixture.execute(
        &files,
        &super::prune::PruneUnnamedSprites,
        "",
        Scope::all(),
        &[],
    );
    assert_eq!(bytes(&out, ATLAS), files[ATLAS]);
    files.insert(orphan.into(), files[BIG].clone());
    let blocks = "assets/minecraft/atlases/blocks.json";
    text(
        &mut files,
        blocks,
        r#"{"sources":[{"type":"single","resource":"fx:custom/orphan","sprite":"minecraft:block/stone"}]}"#,
    );
    let out = fixture.execute(
        &files,
        &super::prune::PruneUnnamedSprites,
        "",
        Scope::all(),
        &[],
    );
    assert_eq!(bytes(&out, blocks), files[blocks]);
}

fn png_context(path: &str, quirks: Quirks) -> FileCx {
    FileCx {
        kind: FileKind::Texture,
        path: physical(path),
        quirks,
    }
}

fn optimise_png(
    name: &str,
    bytes: &[u8],
    path: &str,
    quirks: Quirks,
    options: &str,
) -> (Vec<u8>, bool) {
    let optimiser = super::optimisers()
        .into_iter()
        .find(|pass| pass.name() == name)
        .unwrap();
    match optimiser
        .optimise(bytes, &png_context(path, quirks), &options.parse().unwrap())
        .unwrap()
    {
        Some(out) => (out.bytes, out.loss.is_some()),
        None => (bytes.to_vec(), false),
    }
}

#[test]
fn png_defaults_use_lossless_effort() {
    for optimiser in super::optimisers() {
        let schema = serde_json::to_value(optimiser.options_schema()).unwrap();
        assert_eq!(
            schema["properties"]["effort"]["default"],
            DEFAULT_EFFORT,
            "{}",
            optimiser.name()
        );
    }
}

#[test]
fn png_inputs_the_codec_refuses_stay_as_they_are() {
    let image = Image {
        width: 4,
        height: 4,
        rgba: [9, 8, 7, 0].repeat(16),
    };
    let mut input = rgba_png(&image).unwrap();
    // The IHDR chunk's CRC, after the signature (8), length and type (8) and data (13).
    input[29] ^= 0xff;
    for name in ["png-alpha", "png-quantize", "png-single-color"] {
        let (output, loss) = optimise_png(
            name,
            &input,
            "assets/fx/textures/a.png",
            Quirks::default(),
            "",
        );
        assert_eq!(output, input, "{name}");
        assert!(!loss, "{name}");
    }
}

#[test]
fn png_protection_uses_one_overlay_directory() {
    let image = Image {
        width: 16,
        height: 16,
        rgba: (0..256)
            .flat_map(|i| [i as u8, (i * 31) as u8, 17, 0])
            .collect(),
    };
    let input = rgba_png(&image).unwrap();
    for (path, protected) in [
        ("assets/minecraft/textures/effect/data.png", true),
        ("overlay/assets/minecraft/textures/effect/data.png", true),
        (
            "deep/overlay/assets/minecraft/textures/effect/data.png",
            false,
        ),
        ("assets/fx/textures/effect/data.png", false),
    ] {
        let (output, loss) =
            optimise_png("png-alpha", &input, path, Quirks::default(), "effort = 0");
        assert_eq!(loss, !protected, "{path}");
        assert_eq!(png::decode(&output).unwrap() == image, protected, "{path}");
    }
}

#[test]
fn png_alpha_is_explicit_measured_and_protected_on_eyes_and_shader_targets() {
    let image = Image {
        width: 32,
        height: 32,
        rgba: (0..1024)
            .flat_map(|i| [(i * 7) as u8, (i * 31) as u8, (i * 17) as u8, 0])
            .collect(),
    };
    let input = rgba_png(&image).unwrap();
    let (out, loss) = optimise_png("png-alpha", &input, BIG, Quirks::default(), "");
    assert!(loss);
    assert!(png::decode(&out).unwrap().rgba.iter().all(|&v| v == 0));
    for (path, quirks, options) in [
        (BIG, Quirks::default(), "max_delta = 0"),
        (
            "assets/minecraft/textures/entity/spider_eyes.png",
            Quirks {
                additive_eyes: true,
                ..Default::default()
            },
            "",
        ),
        (
            "assets/minecraft/textures/effect/data.png",
            Quirks::default(),
            "",
        ),
    ] {
        let (out, loss) = optimise_png("png-alpha", &input, path, quirks, options);
        assert!(!loss);
        assert_eq!(png::decode(&out).unwrap(), image);
    }
}

#[test]
fn png_single_color_downsize_and_quantization_compare_to_lossless() {
    let image = Image {
        width: 32,
        height: 32,
        rgba: [5, 8, 15, 255].repeat(1024),
    };
    let input = rgba_png(&image).unwrap();
    let (out, measured) = optimise_png("png-single-color", &input, BIG, Quirks::default(), "");
    assert!(measured);
    assert_eq!(
        png::decode(&out).unwrap(),
        Image {
            width: 16,
            height: 16,
            rgba: [5, 8, 15, 255].repeat(256)
        }
    );
    let image = Image {
        width: 32,
        height: 32,
        rgba: (0..1024)
            .flat_map(|i| {
                let gray = (i % 32 * 8) as u8;
                [gray, gray, gray, 255]
            })
            .collect(),
    };
    let mut input = Vec::new();
    // An ancillary chunk inflates the input without making quantisation beat the lossless encoding.
    let mut encoder = ::png::Encoder::new(&mut input, 32, 32);
    encoder.set_color(::png::ColorType::Rgba);
    encoder
        .add_text_chunk("padding".into(), "x".repeat(4096))
        .unwrap();
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&image.rgba).unwrap();
    writer.finish().unwrap();
    let (out, measured) = optimise_png(
        "png-quantize",
        &input,
        BIG,
        Quirks::default(),
        "max_colors = 16\nquality = 20",
    );
    let lossless = png::Options {
        effort: DEFAULT_EFFORT,
        ..png::Options::default()
    };
    assert_eq!(out, png::optimise(&input, &lossless).unwrap());
    assert!(!measured);
}

#[test]
fn png_quantization_profitable_candidate_is_measured_and_threshold_checked() {
    let mut seed = 37u32;
    let image = Image {
        width: 64,
        height: 64,
        rgba: (0..4096)
            .flat_map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                [seed as u8, (seed >> 8) as u8, (seed >> 16) as u8, 255]
            })
            .collect(),
    };
    let input = rgba_png(&image).unwrap();
    let (out, measured) = optimise_png(
        "png-quantize",
        &input,
        BIG,
        Quirks::default(),
        "max_colors = 4\nquality = 20",
    );
    assert!(measured);
    assert!(
        out.len()
            < png::optimise(&input, &png::Options::default())
                .unwrap()
                .len()
    );
    let (out, measured) = optimise_png(
        "png-quantize",
        &input,
        BIG,
        Quirks::default(),
        "max_colors = 4\nquality = 20\nmax_delta = 0",
    );
    assert_eq!(png::decode(&out).unwrap(), image);
    assert!(!measured);
}
