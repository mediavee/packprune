use camino::Utf8Path;
use serde_json::{Value, json};

use crate::packsquash::{Settings, parse};

fn settings(extra: &str) -> Settings {
    parse(
        &format!("pack_directory = '.'\n{extra}"),
        Utf8Path::new("settings.toml"),
        &[],
    )
    .unwrap()
}

fn file_option(key: &str, value: &str) -> Settings {
    settings(&format!("['**']\n{key} = {value}\n"))
}

macro_rules! mapped_file {
    ($test:ident, $key:literal, $value:literal, $kind:literal, $target:literal, $expected:expr) => {
        #[test]
        fn $test() {
            let mapped = file_option($key, $value);
            assert_eq!(
                mapped
                    .profile
                    .options_for($kind, "assets/test/file")
                    .unwrap()[$target],
                json!($expected)
            );
            assert!(mapped.profile.lossy.is_empty());
        }
    };
}

macro_rules! diagnostic_file {
    ($test:ident, $key:literal, $value:literal, $mark:literal) => {
        #[test]
        fn $test() {
            let mapped = file_option($key, $value);
            assert_eq!(mapped.diagnostics.len(), 1, "{:?}", mapped.diagnostics);
            assert!(mapped.diagnostics[0].starts_with(concat!($mark, " [**] ", $key, ":")));
            assert!(mapped.profile.lossy.is_empty());
        }
    };
}

mapped_file!(
    transcode_ogg,
    "transcode_ogg",
    "false",
    "audio",
    "transcode",
    false
);
mapped_file!(
    two_pass_vorbis,
    "two_pass_vorbis_optimization_and_validation",
    "false",
    "audio",
    "optimize_ogg",
    false
);
mapped_file!(
    empty_audio,
    "empty_audio_optimization",
    "false",
    "audio",
    "empty_audio",
    false
);
mapped_file!(minify_json, "minify_json", "false", "json", "minify", false);
mapped_file!(
    png_effort,
    "image_data_compression_iterations",
    "255",
    "png",
    "effort",
    255
);
mapped_file!(
    no_quantization,
    "color_quantization_target",
    "'none'",
    "png",
    "quantization",
    "none"
);
mapped_file!(
    keep_alpha,
    "skip_alpha_optimizations",
    "true",
    "png",
    "alpha",
    "keep"
);
mapped_file!(
    shader_keep,
    "shader_source_transformation_strategy",
    "'keep_as_is'",
    "shader",
    "minify",
    false
);
mapped_file!(
    shader_minify,
    "shader_source_transformation_strategy",
    "'minify'",
    "shader",
    "minify",
    true
);
mapped_file!(
    legacy_minify,
    "minify_legacy_language",
    "false",
    "legacy_lang",
    "minify",
    false
);
mapped_file!(
    legacy_bom,
    "strip_legacy_language_bom",
    "false",
    "legacy_lang",
    "strip_bom",
    false
);
mapped_file!(
    command_function,
    "minify_command_function",
    "false",
    "mcfunction",
    "minify",
    false
);
mapped_file!(
    nbt_effort,
    "nbt_compression_iterations",
    "0",
    "nbt",
    "effort",
    0
);
mapped_file!(
    force_include,
    "force_include",
    "true",
    "custom",
    "force_include",
    true
);
diagnostic_file!(channels, "channels", "1", "!");
diagnostic_file!(sample_rate, "sampling_frequency", "22050", "!");
diagnostic_file!(pitch, "target_pitch", "2.0", "!");
diagnostic_file!(bitrate_mode, "bitrate_control_mode", "'ABR'", "!");
diagnostic_file!(
    bitrate_metric,
    "target_bitrate_control_metric",
    "128.0",
    "!"
);
diagnostic_file!(ogg_obfuscation, "ogg_obfuscation", "true", "!");
diagnostic_file!(json_comments, "always_allow_json_comments", "false", "-");
diagnostic_file!(json_sort, "sort_json_object_keys", "false", "-");
diagnostic_file!(
    quantization,
    "color_quantization_target",
    "'eight_bit_depth'",
    "!"
);
diagnostic_file!(dithering, "color_quantization_dithering_level", "0.0", "!");
diagnostic_file!(delete_bloat, "delete_bloat_keys", "false", "-");
diagnostic_file!(maximum_dimension, "maximum_width_and_height", "1024", "-");
diagnostic_file!(single_colour, "downsize_if_single_color", "true", "!");
diagnostic_file!(png_obfuscation, "png_obfuscation", "true", "!");
diagnostic_file!(
    shader_prettify,
    "shader_source_transformation_strategy",
    "'prettify'",
    "-"
);
diagnostic_file!(top_level_shader, "is_top_level_shader", "false", "-");

macro_rules! global {
    ($test:ident, $text:literal, $assertion:expr) => {
        #[test]
        fn $test() {
            let mapped = settings($text);
            ($assertion)(mapped);
        }
    };
}

global!(pack_directory, "", |s: Settings| assert_eq!(s.input, "."));
global!(
    output_file_path,
    "output_file_path = 'out.zip'",
    |s: Settings| assert_eq!(s.output, "out.zip")
);
global!(
    recompress,
    "recompress_compressed_files = true",
    |s: Settings| assert_eq!(s.profile.zip.unwrap().recompress_compressed, Some(true))
);
global!(
    zip_effort,
    "zip_compression_iterations = 0",
    |s: Settings| assert_eq!(s.profile.zip.unwrap().effort, Some(json!(0)))
);
global!(
    automatic_quirks,
    "automatic_minecraft_quirks_detection = false",
    |s: Settings| assert!(!s.automatic_quirks)
);
global!(
    forced_quirks,
    "work_around_minecraft_quirks = ['java8_zip_parsing', 'grayscale_images_gamma_miscorrection', 'restrictive_banner_layer_texture_format_check', 'bad_entity_eye_layer_texture_transparency_blending']",
    |s: Settings| assert_eq!(s.quirks.len(), 4)
);
global!(
    obfuscation_quirks,
    "work_around_minecraft_quirks = ['ogg_obfuscation_incompatibility', 'png_obfuscation_incompatibility']",
    |s: Settings| {
        assert_eq!(s.diagnostics.len(), 2);
        assert!(
            s.diagnostics
                .iter()
                .all(|line| line.starts_with("- work_around_minecraft_quirks:"))
        );
    }
);
global!(
    asset_mask,
    "automatic_asset_types_mask_detection = false",
    |s: Settings| assert!(!s.automatic_asset_mask)
);
global!(
    allow_mods,
    "allow_mods = ['OptiFine', 'Minecraft Transit Railway 3']",
    |s: Settings| assert_eq!(s.profile.keep_mod_files, ["optifine", "mtr3"])
);
global!(skip_icon, "skip_pack_icon = true", |s: Settings| assert!(
    s.diagnostics[0].starts_with("! skip_pack_icon:")
));
global!(
    metadata,
    "validate_pack_metadata_file = false",
    |s: Settings| assert!(s.diagnostics[0].starts_with("- validate_pack_metadata_file:"))
);
global!(
    hidden_files,
    "ignore_system_and_hidden_files = false",
    |s: Settings| assert!(!s.ignore_hidden_files)
);
global!(
    zip_pedantic,
    "zip_spec_conformance_level = 'pedantic'",
    |s: Settings| assert_eq!(s.profile.zip.unwrap().mode.as_deref(), Some("conformant"))
);
global!(
    zip_high,
    "zip_spec_conformance_level = 'high'",
    |s: Settings| assert_eq!(s.profile.zip.unwrap().mode.as_deref(), Some("conformant"))
);
global!(
    zip_balanced_refused,
    "zip_spec_conformance_level = 'balanced'",
    |s: Settings| {
        assert_eq!(s.profile.zip.unwrap().mode.as_deref(), Some("conformant"));
        assert!(s.diagnostics[0].starts_with("! zip_spec_conformance_level:"));
    }
);
global!(
    zip_disregard,
    "zip_spec_conformance_level = 'disregard'",
    |s: Settings| {
        assert_eq!(s.profile.zip.unwrap().mode.as_deref(), Some("conformant"));
        assert!(s.diagnostics[0].starts_with("! zip_spec_conformance_level:"));
    }
);
global!(
    size_obfuscation,
    "size_increasing_zip_obfuscation = true",
    |s: Settings| assert!(s.diagnostics[0].starts_with("- size_increasing_zip_obfuscation:"))
);
global!(
    discretion,
    "percentage_of_zip_structures_tuned_for_obfuscation_discretion = 30",
    |s: Settings| assert!(
        s.diagnostics[0]
            .starts_with("- percentage_of_zip_structures_tuned_for_obfuscation_discretion:")
    )
);
global!(
    squash_times,
    "never_store_squash_times = true",
    |s: Settings| assert!(s.diagnostics[0].starts_with("- never_store_squash_times:"))
);
global!(threads, "threads = 4", |s: Settings| assert_eq!(
    s.profile.threads,
    Some(4)
));
global!(
    buffers,
    "spooling_buffers_size = 128",
    |s: Settings| assert!(s.diagnostics[0].starts_with("- spooling_buffers_size:"))
);
global!(comment, "zip_comment = 'hello'", |s: Settings| assert_eq!(
    s.profile.zip.unwrap().comment.as_deref(),
    Some("hello")
));

#[test]
fn balanced_requires_explicit_allow() {
    let mapped = parse(
        "pack_directory = '.'\nzip_spec_conformance_level = 'balanced'",
        Utf8Path::new("settings.toml"),
        &["lean".into()],
    )
    .unwrap();
    assert_eq!(mapped.profile.zip.unwrap().mode.as_deref(), Some("lean"));
    assert!(mapped.diagnostics.is_empty());
}

#[test]
fn properties_require_optifine() {
    let disabled = file_option("minify_properties", "false");
    assert!(
        disabled
            .profile
            .options_for("properties", "test.properties")
            .unwrap()
            .is_empty()
    );
    let enabled = settings("allow_mods = ['OptiFine']\n['**']\nminify_properties = false");
    assert_eq!(
        enabled
            .profile
            .options_for("properties", "test.properties")
            .unwrap()["minify"],
        false
    );
}

#[test]
fn defaults_are_silent_and_lossless() {
    let mapped = settings("");
    assert_eq!(mapped.output, "pack.zip");
    assert!(mapped.diagnostics.is_empty());
    assert!(mapped.profile.lossy.is_empty());
    for (kind, key, expected) in [
        ("audio", "transcode", json!(false)),
        ("png", "quantization", json!("none")),
        ("png", "alpha", json!("keep")),
    ] {
        assert_eq!(
            mapped.profile.options_for(kind, "file").unwrap()[key],
            expected
        );
    }
    for spec in super::FILE_OPTIONS {
        if let Some(value) = spec.default {
            assert!(
                file_option(spec.name, value).diagnostics.is_empty(),
                "{}",
                spec.name
            );
        }
    }
}

#[test]
fn invalid_settings_are_located_and_refused() {
    for (text, key) in [
        ("unknown = true", "unknown"),
        ("threads = 'four'", "threads"),
        ("threads = 0", "threads"),
        (
            "zip_compression_iterations = 256",
            "zip_compression_iterations",
        ),
        ("allow_mods = ['unknown']", "allow_mods"),
        (
            "work_around_minecraft_quirks = ['unknown']",
            "work_around_minecraft_quirks",
        ),
        (
            "zip_spec_conformance_level = 'unknown'",
            "zip_spec_conformance_level",
        ),
        ("['**']\nunknown = true", "unknown"),
        ("['**']\nminify_json = 1", "minify_json"),
        ("['**']\nchannels = 3", "channels"),
        ("['**']\nsampling_frequency = 0", "sampling_frequency"),
        (
            "['**']\nimage_data_compression_iterations = -1",
            "image_data_compression_iterations",
        ),
        (
            "['**']\ncolor_quantization_dithering_level = 1.1",
            "color_quantization_dithering_level",
        ),
        (
            "['**']\nshader_source_transformation_strategy = 'unknown'",
            "shader_source_transformation_strategy",
        ),
    ] {
        let error = parse(
            &format!("pack_directory = '.'\n{text}"),
            Utf8Path::new("settings.toml"),
            &[],
        )
        .unwrap_err()
        .to_string();
        assert!(error.starts_with("settings.toml:"), "{error}");
        assert!(error.contains(key), "{error}");
    }
    assert!(
        parse("threads = 4", Utf8Path::new("settings.toml"), &[])
            .unwrap_err()
            .to_string()
            .contains("pack_directory")
    );
    assert!(parse("pack_directory = [", Utf8Path::new("settings.toml"), &[]).is_err());
}

#[test]
fn directory_is_checked_only_at_the_load_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let path = camino::Utf8PathBuf::from_path_buf(dir.path().join("settings.toml")).unwrap();
    std::fs::write(&path, "pack_directory = '/this/path/does/not/exist'").unwrap();
    let error = crate::packsquash::load(&path, &[]).unwrap_err().to_string();
    assert!(
        error.contains("settings.toml:1:1: pack_directory:"),
        "{error}"
    );
    std::fs::write(
        &path,
        format!("pack_directory = '{}'", dir.path().display()),
    )
    .unwrap();
    assert!(crate::packsquash::load(&path, &[]).is_ok());
}

#[test]
fn first_match_takes_whole_kind_with_literal_separators() {
    let mapped = settings(
        "['assets/ui/**/*.png']\nskip_alpha_optimizations = true\n['{**/*?.png}']\nimage_data_compression_iterations = 10\n['*_*/**/*?.*']\nforce_include = true\n['overlay_*/**']\nforce_include = true",
    );
    let png = mapped
        .profile
        .options_for("png", "assets/ui/ui/test.png")
        .unwrap();
    assert_eq!(png["alpha"], "keep");
    assert_eq!(png["effort"], 5);
    assert_eq!(
        mapped
            .profile
            .options_for("custom", "effects_26_2/assets/minecraft/test.fsh")
            .unwrap()["force_include"],
        true
    );
    assert_eq!(
        mapped
            .profile
            .options_for("custom", "overlay_v1/assets/example/a")
            .unwrap()["force_include"],
        true
    );
    assert_eq!(
        mapped
            .profile
            .options_for("custom", "assets/example/test.bin")
            .unwrap()["force_include"],
        false
    );
    assert_eq!(
        mapped.profile.options_for("png", "file.png").unwrap()["effort"],
        10
    );
    let rules = &mapped.profile.overrides;
    assert_eq!(rules[0].glob, "*_*/**/*?.*");
    assert_eq!(rules[1].glob, "assets/ui/**/*.png");
}

#[test]
fn nonfinite_lossy_parameters_are_refused() {
    for value in ["nan", "inf", "-inf"] {
        assert!(
            parse(
                &format!("pack_directory = '.'\n['**']\ntarget_pitch = {value}"),
                Utf8Path::new("settings.toml"),
                &[]
            )
            .is_err()
        );
    }
}

#[test]
fn first_match_preserves_explicit_false() {
    let mapped = settings("['assets/**']\nminify_json = false\n['{**}']\nminify_json = true");
    let options = mapped
        .profile
        .options_for("json", "assets/example/model.json")
        .unwrap();
    assert_eq!(options.get("minify"), Some(&Value::Bool(false)));
}

#[test]
fn global_tables_are_wrong_types_not_globs() {
    for text in [
        "pack_directory = { minify_json = true }",
        "pack_directory = '.'\nthreads = { minify_json = true }",
    ] {
        assert!(parse(text, Utf8Path::new("settings.toml"), &[]).is_err());
    }
}

#[test]
fn first_matching_kind_table_uses_its_own_defaults() {
    let mapped = settings(
        "['assets/**/*.json']\ndelete_bloat_keys = false\n['{**/*.json}']\nminify_json = false\n['assets/ui/**/*.png']\nskip_alpha_optimizations = true\n['{**/*.png}']\nimage_data_compression_iterations = 30",
    );
    let json = mapped.profile.options_for("json", "assets/a.json").unwrap();
    assert!(!json.contains_key("delete_bloat_keys"));
    assert_eq!(json["minify"], true);
    let png = mapped
        .profile
        .options_for("png", "assets/ui/a.png")
        .unwrap();
    assert_eq!(png["alpha"], "keep");
    assert_eq!(png["effort"], 5);
}

#[test]
fn drop_in_uses_lexicographic_glob_order() {
    let mapped = settings("['secret/**']\nforce_include = false\n['**']\nforce_include = true");
    assert_eq!(
        mapped
            .profile
            .options_for("custom", "secret/x.bin")
            .unwrap()["force_include"],
        true
    );
    assert_eq!(mapped.profile.overrides[0].glob, "**");
}

#[test]
fn settings_tables_are_sorted_with_specific_png_defaults() {
    let mapped = settings(
        "validate_pack_metadata_file = false\nautomatic_minecraft_quirks_detection = false\nautomatic_asset_types_mask_detection = false\nwork_around_minecraft_quirks = []\nrecompress_compressed_files = true\nzip_compression_iterations = 20\nzip_spec_conformance_level = 'high'\nthreads = 4\nspooling_buffers_size = 128\n['assets/ui/**/*.png']\nimage_data_compression_iterations = 10\nskip_alpha_optimizations = true\n['{**/*?.png}']\nimage_data_compression_iterations = 10\n['**/*?.ogg']\ntranscode_ogg = false\n['*_*/**/*?.*']\nforce_include = true\n['assets/ui/shaders/**']\nshader_source_transformation_strategy = 'keep_as_is'\n['overlay_*/assets/minecraft/shaders/**']\nshader_source_transformation_strategy = 'keep_as_is'\n['overlay_*/**']\nforce_include = true",
    );
    let rules: Vec<_> = mapped
        .profile
        .overrides
        .iter()
        .map(|rule| rule.glob.as_str())
        .collect();
    assert_eq!(
        rules,
        [
            "**/*?.ogg",
            "*_*/**/*?.*",
            "assets/ui/**/*.png",
            "assets/ui/shaders/**",
            "overlay_*/**",
            "overlay_*/assets/minecraft/shaders/**",
            "{**/*?.png}",
            "**"
        ]
    );
    assert_eq!(
        mapped
            .profile
            .options_for("png", "assets/ui/test.png")
            .unwrap()["effort"],
        10
    );
    assert_eq!(
        mapped
            .profile
            .options_for("custom", "effects_26_2/assets/minecraft/test.fsh")
            .unwrap()["force_include"],
        true
    );
    assert_eq!(
        mapped
            .profile
            .options_for("custom", "overlay_v1/test")
            .unwrap()["force_include"],
        true
    );
    assert_eq!(
        mapped
            .profile
            .options_for("shader", "overlay_v1/assets/minecraft/shaders/test.fsh")
            .unwrap()["minify"],
        false
    );
}
