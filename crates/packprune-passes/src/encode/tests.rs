//! Each optimiser on the files it applies to, its options and the quirks it honours. The codecs' own equivalence
//! checks are `packprune-codec`'s tests; these check what the wrappers choose.

use packprune_codec::audio::{self, Pcm};
use packprune_codec::png;
use packprune_pack::{FileKind, PhysPath, Quirks};
use packprune_pass::{FileCx, FileOptimizer};

use super::optimisers;

fn optimiser(name: &str) -> Box<dyn FileOptimizer> {
    optimisers()
        .into_iter()
        .find(|optimiser| optimiser.name() == name)
        .unwrap()
}

fn cx(path: &str, quirks: Quirks) -> FileCx {
    let path = PhysPath::new(path).unwrap();
    FileCx {
        kind: FileKind::Unknown,
        path,
        quirks,
    }
}

fn options(toml: &str) -> toml::Table {
    toml.parse().unwrap()
}

/// The optimiser's output for `input` at `path`, `None` when it keeps the input.
fn run(name: &str, input: &[u8], path: &str, quirks: Quirks, toml: &str) -> Option<Vec<u8>> {
    optimiser(name)
        .optimise(input, &cx(path, quirks), &options(toml))
        .unwrap()
        .map(|optimised| {
            assert!(optimised.loss.is_none());
            optimised.bytes
        })
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/../../tests/fixtures/sounds/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

#[test]
fn the_registry_names_the_config_kinds_and_every_optimiser_is_lossless() {
    let names: Vec<&str> = optimisers().iter().map(|o| o.name()).collect();
    assert_eq!(
        names,
        [
            "json",
            "png",
            "audio",
            "shader",
            "properties",
            "legacy_lang",
            "mcfunction",
            "nbt"
        ]
    );
    assert!(
        optimisers()
            .iter()
            .all(|o| o.loss() == packprune_pass::Loss::Lossless)
    );
}

#[test]
fn each_optimiser_applies_to_its_files_only() {
    let cases = [
        ("assets/a/models/b.json", FileKind::Model, "json"),
        ("pack.mcmeta", FileKind::PackMeta, "json"),
        (
            "assets/a/textures/b.png.mcmeta",
            FileKind::TextureMeta,
            "json",
        ),
        ("pack.png", FileKind::PackMeta, "png"),
        ("over/assets/a/textures/b.png", FileKind::Texture, "png"),
        ("assets/a/sounds/b.ogg", FileKind::Sound, "audio"),
        ("assets/a/shaders/core/b.fsh", FileKind::Shader, "shader"),
        (
            "assets/a/shaders/include/b.glsl",
            FileKind::ShaderInclude,
            "shader",
        ),
        (
            "assets/minecraft/optifine/ctm/a.properties",
            FileKind::Unknown,
            "properties",
        ),
        ("assets/a/lang/en_us.lang", FileKind::Unknown, "legacy_lang"),
        (
            "data/a/functions/b.mcfunction",
            FileKind::Unknown,
            "mcfunction",
        ),
        ("data/a/structures/b.nbt", FileKind::Unknown, "nbt"),
        ("assets/a/textures/b.txt", FileKind::Unknown, ""),
    ];
    for (path, kind, expected) in cases {
        let path = PhysPath::new(path).unwrap();
        let applying: Vec<&str> = optimisers()
            .iter()
            .filter(|o| o.applies(kind, &path))
            .map(|o| o.name())
            .collect();
        let expected: Vec<&str> = [expected].into_iter().filter(|n| !n.is_empty()).collect();
        assert_eq!(applying, expected, "{path}");
    }
}

#[test]
fn json_drops_whitespace_and_keeps_what_it_cannot_read_strictly() {
    let path = "assets/a/models/b.json";
    let input = b"{\n  \"parent\": \"item/x\",\n  \"a\": 1.0\n}\n";
    assert_eq!(
        run("json", input, path, Quirks::default(), "").unwrap(),
        br#"{"parent":"item/x","a":1.0}"#
    );
    assert_eq!(
        run("json", input, path, Quirks::default(), "minify = false"),
        None
    );
    let lenient = b"{ /* a font may have comments */ \"providers\": [] }";
    assert_eq!(run("json", lenient, path, Quirks::default(), ""), None);
    assert_eq!(
        run("json", br#"{"a":1}"#, path, Quirks::default(), ""),
        None
    );
}

/// 256 grey levels written as RGBA: oxipng would make it a grey PNG.
fn grey_png() -> Vec<u8> {
    let rgba: Vec<u8> = (0..=255u8).flat_map(|v| [v, v, v, 255]).collect();
    let mut out = Vec::new();
    let mut encoder = ::png::Encoder::new(&mut out, 16, 16);
    encoder.set_color(::png::ColorType::Rgba);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&rgba).unwrap();
    writer.finish().unwrap();
    out
}

/// The IHDR colour type.
fn color_type(png: &[u8]) -> u8 {
    png[25]
}

#[test]
fn png_is_smaller_with_the_same_pixels() {
    let input = grey_png();
    let output = run(
        "png",
        &input,
        "assets/a/textures/b.png",
        Quirks::default(),
        "effort = 0",
    )
    .unwrap();
    assert!(output.len() < input.len());
    assert_eq!(png::decode(&output).unwrap(), png::decode(&input).unwrap());
    assert_eq!(color_type(&output), 0, "grey");
}

#[test]
fn png_keeps_rgb_under_the_grayscale_gamma_quirk() {
    let quirks = Quirks {
        imageio_grey: true,
        ..Quirks::default()
    };
    let output = run(
        "png",
        &grey_png(),
        "assets/a/textures/b.png",
        quirks,
        "effort = 0",
    )
    .unwrap();
    assert!(
        !matches!(color_type(&output), 0 | 4),
        "{}",
        color_type(&output)
    );
}

#[test]
fn png_keeps_the_colour_type_of_banner_and_shield_layers_under_their_quirk() {
    let quirks = Quirks {
        abgr_layers: true,
        ..Quirks::default()
    };
    for path in [
        "assets/minecraft/textures/entity/banner/stripe.png",
        "over/assets/minecraft/textures/entity/shield_base.png",
    ] {
        let output = run("png", &grey_png(), path, quirks, "effort = 0").unwrap();
        assert_eq!(color_type(&output), 6, "{path}");
    }
    let output = run(
        "png",
        &grey_png(),
        "assets/minecraft/textures/entity/creeper.png",
        quirks,
        "effort = 0",
    )
    .unwrap();
    assert_eq!(color_type(&output), 0, "not a banner layer");
}

#[test]
fn png_refuses_an_effort_that_is_not_a_byte() {
    let error = optimiser("png")
        .optimise(
            &grey_png(),
            &cx("a.png", Quirks::default()),
            &options("effort = 300"),
        )
        .err()
        .unwrap();
    let error = error.to_string();
    assert!(
        error.starts_with("png: ") && error.contains("`effort`") && error.contains("u8"),
        "{error}"
    );
}

#[test]
fn ogg_is_remuxed_with_the_same_pcm() {
    let tone = Pcm {
        rate: 22050,
        channels: vec![(0..22050).map(|i| (i as f32 * 0.05).sin() * 0.5).collect()],
    };
    let input = audio::encode(&tone, 0.5).unwrap();
    let output = run(
        "audio",
        &input,
        "assets/a/sounds/b.ogg",
        Quirks::default(),
        "",
    )
    .unwrap();
    assert!(output.len() < input.len());
    assert_eq!(
        audio::decode(&output).unwrap(),
        audio::decode(&input).unwrap()
    );
    assert_eq!(
        run(
            "audio",
            &input,
            "assets/a/sounds/b.ogg",
            Quirks::default(),
            "optimize_ogg = false"
        ),
        None
    );
    // Already remuxed: nothing left to gain.
    let fixture = fixture("mono_22k_1s.ogg");
    assert_eq!(
        run(
            "audio",
            &fixture,
            "assets/a/sounds/b.ogg",
            Quirks::default(),
            ""
        ),
        None
    );
}

#[test]
fn silence_becomes_the_smallest_stream_of_its_length() {
    let silence = Pcm {
        rate: 22050,
        channels: vec![vec![0.0; 22050]],
    };
    let input = audio::encode(&silence, 1.0).unwrap();
    let path = "assets/a/sounds/b.ogg";

    let output = run(
        "audio",
        &input,
        path,
        Quirks::default(),
        "optimize_ogg = false",
    )
    .unwrap();

    assert_eq!(audio::decode(&output).unwrap(), silence);
    let remuxed = run(
        "audio",
        &input,
        path,
        Quirks::default(),
        "empty_audio = false",
    )
    .unwrap();
    assert!(
        output.len() < remuxed.len(),
        "{} vs {}",
        output.len(),
        remuxed.len()
    );
}

#[test]
fn shaders_lose_comments_and_spaces_unless_kept_as_is() {
    let input = b"#version 150\n// a comment\nvoid main() {\n    gl_Position = vec4(0.0);\n}\n";
    let path = "assets/a/shaders/core/b.vsh";
    assert_eq!(
        run("shader", input, path, Quirks::default(), "").unwrap(),
        b"#version 150\n\nvoid main(){\ngl_Position=vec4(0.0);\n}\n"
    );
    assert_eq!(
        run("shader", input, path, Quirks::default(), "minify = false"),
        None
    );
    assert_eq!(
        run("shader", b"float x; /* open", path, Quirks::default(), ""),
        None
    );
}

#[test]
fn properties_keep_entries_only() {
    let input = b"# OptiFine CTM\nmethod = ctm\n\nmatchBlocks: glass\n";
    let path = "assets/minecraft/optifine/ctm/glass.properties";
    assert_eq!(
        run("properties", input, path, Quirks::default(), "").unwrap(),
        b"method=ctm\nmatchBlocks=glass\n"
    );
    assert_eq!(
        run(
            "properties",
            input,
            path,
            Quirks::default(),
            "minify = false"
        ),
        None
    );
}

#[test]
fn legacy_lang_strips_its_bom_even_unminified() {
    let input = b"\xef\xbb\xbf# header\n\nitem.a.name=A\n";
    let path = "assets/a/lang/en_us.lang";
    assert_eq!(
        run("legacy_lang", input, path, Quirks::default(), "").unwrap(),
        b"item.a.name=A\n"
    );
    assert_eq!(
        run(
            "legacy_lang",
            input,
            path,
            Quirks::default(),
            "minify = false"
        )
        .unwrap(),
        b"# header\n\nitem.a.name=A\n"
    );
    assert_eq!(
        run(
            "legacy_lang",
            input,
            path,
            Quirks::default(),
            "minify = false\nstrip_bom = false"
        ),
        None
    );
}

#[test]
fn functions_lose_comments_and_blank_lines() {
    let input = b"# setup\n\nsay hi\n";
    let path = "data/a/function/b.mcfunction";
    assert_eq!(
        run("mcfunction", input, path, Quirks::default(), "").unwrap(),
        b"say hi\n"
    );
    assert_eq!(
        run(
            "mcfunction",
            input,
            path,
            Quirks::default(),
            "minify = false"
        ),
        None
    );
}

/// `payload` as one gzip member of stored deflate blocks: the worst compression there is.
fn stored_gzip(payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff];
    let chunks: Vec<&[u8]> = payload.chunks(65535).collect();
    for (index, chunk) in chunks.iter().enumerate() {
        out.push(u8::from(index + 1 == chunks.len()));
        let len = chunk.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    let mut crc = !0u32;
    for &byte in payload {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    out.extend_from_slice(&(!crc).to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out
}

#[test]
fn nbt_is_recompressed_with_the_same_payload() {
    let payload: Vec<u8> = (0..4096u32).map(|i| (i % 7) as u8).collect();
    let input = stored_gzip(&payload);
    assert_eq!(packprune_codec::nbt::decompress(&input).unwrap(), payload);

    let output = run(
        "nbt",
        &input,
        "data/a/structures/b.nbt",
        Quirks::default(),
        "effort = 1",
    )
    .unwrap();

    assert!(output.len() < input.len());
    assert_eq!(packprune_codec::nbt::decompress(&output).unwrap(), payload);
    assert_eq!(
        run("nbt", b"not gzip", "a.nbt", Quirks::default(), ""),
        None
    );
}
