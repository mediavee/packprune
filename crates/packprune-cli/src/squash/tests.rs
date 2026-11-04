use std::io;

use super::*;

struct Server {
    dir: tempfile::TempDir,
}

impl Server {
    fn new() -> Self {
        let server = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        server.write(
            "pack/pack.mcmeta",
            r#"{"pack": {"min_format": 88, "max_format": 88, "description": "t"}}"#,
        );
        server.write("pack/assets/fx/models/item/orphan.json", "{}");
        server.write("client.jar", "not read before the run");
        server
    }

    fn path(&self, name: &str) -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(self.dir.path().join(name)).unwrap()
    }

    fn write(&self, name: &str, text: &str) -> Utf8PathBuf {
        let path = self.path(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }

    /// The settings as Nexo rewrites them: absolute pack and output paths, then the server's own lines.
    fn settings(&self, rest: &str) -> Utf8PathBuf {
        let text = format!(
            "pack_directory = '{}'\noutput_file_path = '{}'\n{rest}",
            self.path("pack"),
            self.path("out.zip")
        );
        self.write("packsquash.toml", &text)
    }

    fn config(&self, rest: &str) -> Utf8PathBuf {
        self.write(
            "packprune.toml",
            &format!("version = 2\n[vanilla]\nclient_jar = 'client.jar'\n{rest}"),
        )
    }
}

/// Settings refused by key ship nothing and exit 2.
#[test]
fn invalid_settings_exit_2_and_write_nothing() {
    let server = Server::new();
    for rest in [
        "unknown_option = true\n",
        "threads = 'four'\n",
        "zip_compression_iterations = 256\n",
        "['**/*.png']\nnot_a_png_option = 1\n",
        "not = [toml\n",
    ] {
        let settings = server.settings(rest);
        assert_eq!(
            squash(&settings, Ok(server.config("")), &mut io::sink()).unwrap(),
            ExitClass::Refused,
            "{rest}"
        );
        assert!(!server.path("out.zip").exists(), "{rest}");
    }
}

/// A pack directory that is missing, or not a directory, or an unreadable settings file.
#[test]
fn a_missing_pack_directory_or_settings_file_exits_2() {
    let server = Server::new();
    let missing = server.write("missing.toml", "output_file_path = 'out.zip'\n");
    let file = server.write(
        "file.toml",
        &format!("pack_directory = '{}'\n", server.path("client.jar")),
    );
    for settings in [missing, file, server.path("absent.toml")] {
        assert_eq!(
            squash(&settings, Ok(server.config("")), &mut io::sink()).unwrap(),
            ExitClass::Refused,
            "{settings}"
        );
        assert!(!server.path("out.zip").exists());
    }
}

/// Without packprune's config, or with one it refuses, the original ships and the drop-in exits 0.
#[test]
fn no_config_or_a_refused_one_ships_the_original() {
    let server = Server::new();
    let settings = server.settings("");
    let broken = server.write("broken.toml", "version = 2\nunknown = 1\n");
    let no_jar = server.write("no-jar.toml", "version = 2\n");
    for config in [Err("no packprune.toml".to_owned()), Ok(broken), Ok(no_jar)] {
        let _ = std::fs::remove_file(server.path("out.zip"));

        assert_eq!(
            squash(&settings, config, &mut io::sink()).unwrap(),
            ExitClass::Clean
        );

        let zip = std::fs::read(server.path("out.zip")).unwrap();
        assert!(zip.starts_with(b"PK\x03\x04"));
    }
}

/// The config's profile over the settings': what it sets wins, its overrides come first, the settings' glob tables
/// after with the mapped defaults last; what it leaves unset keeps the settings' value.
#[test]
fn the_config_profile_is_layered_over_the_settings() {
    let server = Server::new();
    let settings = server.settings(
        "threads = 2\nzip_compression_iterations = 5\nrecompress_compressed_files = true\n\
         ['assets/**/*.png']\nimage_data_compression_iterations = 3\n",
    );
    let config = server.config(
        "[profiles.default]\nthreads = 3\n\
         [[profiles.default.override]]\nglob = 'assets/fx/**'\npng = { effort = 1 }\n",
    );
    let settings = packsquash::load(&settings, &[]).unwrap();

    let profile = request(&settings, config).unwrap().profile;

    assert_eq!(profile.threads, Some(3));
    let zip = profile.zip.unwrap();
    assert_eq!(zip.effort.and_then(|effort| effort.as_u64()), Some(5));
    assert_eq!(zip.recompress_compressed, Some(true));
    let globs: Vec<&str> = profile
        .overrides
        .iter()
        .map(|rule| rule.glob.as_str())
        .collect();
    assert_eq!(globs, ["assets/fx/**", "assets/**/*.png", "**"]);
}

/// `balanced` becomes `lean` only when the config's profile allows it and no Java 8 client is declared.
#[test]
fn balanced_is_lean_only_when_the_config_allows_it() {
    let server = Server::new();
    let allowed = "[profiles.default]\nzip = { allow = ['lean'] }\n";
    for (settings, rest, mode) in [
        ("", "", "conformant"),
        ("", allowed, "lean"),
        (
            "work_around_minecraft_quirks = ['java8_zip_parsing']\n",
            allowed,
            "conformant",
        ),
    ] {
        let settings = server.settings(&format!(
            "zip_spec_conformance_level = 'balanced'\n{settings}"
        ));
        let config = server.config(rest);
        let settings = read_settings(&settings, &own_config(config.clone())).unwrap();

        let profile = request(&settings, config).unwrap().profile;

        assert_eq!(profile.zip.unwrap().mode.as_deref(), Some(mode), "{rest}");
    }
}

#[test]
fn quirk_names_are_packsquash_s() {
    let forced = quirks(&[
        "java8_zip_parsing".into(),
        "bad_entity_eye_layer_texture_transparency_blending".into(),
    ]);
    assert!(forced.additive_eyes);
    assert!(!forced.imageio_grey && !forced.abgr_layers);
}

/// Every option the PackSquash mapping emits, at its default and at another value of each file option, is one its
/// optimiser reads: the drop-in's profile never reaches the config loader's check.
#[test]
fn every_mapped_file_option_is_read_by_its_optimiser() {
    use packprune_config::{OptionsCheck, OptionsOwner};
    use packprune_engine::run::Registry;

    let settings = "pack_directory = 'pack'\n\
        ['**/*.ogg']\ntranscode_ogg = false\ntwo_pass_vorbis_optimization_and_validation = false\nchannels = 1\n\
        sampling_frequency = 22050\ntarget_pitch = 2.0\nbitrate_control_mode = 'ABR'\n\
        target_bitrate_control_metric = 128.0\nempty_audio_optimization = false\nogg_obfuscation = true\n\
        ['**/*.json']\nminify_json = false\ndelete_bloat_keys = false\nalways_allow_json_comments = false\n\
        sort_json_object_keys = true\n\
        ['**/*.png']\nimage_data_compression_iterations = 3\ncolor_quantization_target = 'none'\n\
        color_quantization_dithering_level = 0.5\nmaximum_width_and_height = 1024\nskip_alpha_optimizations = true\n\
        downsize_if_single_color = true\npng_obfuscation = true\n\
        ['**/*.fsh']\nshader_source_transformation_strategy = 'keep_as_is'\nis_top_level_shader = false\n\
        ['**/*.lang']\nminify_legacy_language = false\nstrip_legacy_language_bom = false\n\
        ['**/*.mcfunction']\nminify_command_function = false\n\
        ['**/*.properties']\nminify_properties = false\n\
        ['**/*.nbt']\nnbt_compression_iterations = 3\n\
        ['**/*.bin']\nforce_include = true\n";
    let mapped = packsquash::parse(settings, Utf8Path::new("packsquash.toml"), &[]).unwrap();
    let registry = Registry::default();

    for rule in &mapped.profile.overrides {
        for (kind, options) in &rule.kinds {
            let owner = OptionsOwner::Kind(kind);
            registry
                .check_owner(owner)
                .and_then(|()| registry.check_options(owner, options))
                .unwrap_or_else(|why| panic!("[{}] {kind}: {why}", rule.glob));
        }
    }
}

/// A lossy file optimiser's measured loss reaches the log Nexo shows, one `*` line per file, as `optimise`
/// prints it.
#[test]
fn a_lossy_optimiser_s_measured_loss_is_logged() {
    let server = Server::new();
    let fixtures = Utf8Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    packprune_engine::io::repack(
        &fixtures.join("vanilla"),
        &server.path("client.jar"),
        &Default::default(),
    )
    .unwrap();
    let settings = server.write(
        "packsquash.toml",
        &format!(
            "pack_directory = '{}'\noutput_file_path = '{}'\n",
            fixtures.join("drop-in/lossy-png"),
            server.path("out.zip")
        ),
    );
    let config = server.config(
        "[profiles.default]\npasses = 'none'\n\
         [profiles.default.lossy.png-quantize]\nglobs = ['pack.png']\neffort = 0\nmax_colors = 2\nquality = 0\n\
         dithering = 0.0\n",
    );
    let mut log = Vec::new();

    let exit = squash(&settings, Ok(config), &mut log).unwrap();

    let log = String::from_utf8(log).unwrap();
    assert_eq!(exit, ExitClass::Clean, "{log}");
    assert!(log.contains("optimised pack"), "{log}");
    let loss = log
        .lines()
        .find(|line| line.starts_with("* packprune: loss pack.png: ssim "))
        .unwrap_or_else(|| panic!("no measured loss in:\n{log}"));
    let ssim: f64 = loss.rsplit(' ').next().unwrap().parse().unwrap();
    assert!(ssim < 1.0, "{loss}");
}

/// A known-kind file the settings' `force_include` covers and the prune removes is told to the console, one `*` line
/// per file, with how to keep it.
#[test]
fn a_force_included_file_the_prune_removes_is_logged() {
    let server = Server::new();
    let fixtures = Utf8Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    packprune_engine::io::repack(
        &fixtures.join("vanilla"),
        &server.path("client.jar"),
        &Default::default(),
    )
    .unwrap();
    let settings = server.settings("['assets/fx/models/item/orphan.json']\nforce_include = true\n");
    let config = server.config("[profiles.default]\npasses = 'none'\n");
    let mut log = Vec::new();

    let exit = squash(&settings, Ok(config), &mut log).unwrap();

    let log = String::from_utf8(log).unwrap();
    assert_eq!(exit, ExitClass::Clean, "{log}");
    let line = log
        .lines()
        .find(|line| line.starts_with("* packprune: force_include "))
        .unwrap_or_else(|| panic!("no force_include note in:\n{log}"));
    for expected in [
        "assets/fx/models/item/orphan.json",
        "usage source",
        "keep-list",
    ] {
        assert!(line.contains(expected), "{expected}: {line}");
    }
}
