use camino::Utf8Path;
use serde_json::json;

use crate::{Config, PassSelection, parse, schema};

fn config(text: &str) -> Config {
    parse(
        &format!("version = 2\n{text}"),
        Utf8Path::new("/tmp/config/packprune.toml"),
    )
    .unwrap()
}

#[test]
fn resolves_inheritance_with_child_replacements() {
    let config = config(
        r#"
profile = "fast"
[profiles.default]
passes = "lossless"
threads = 4
disable = ["short-json"]
keep_mod_files = ["optifine"]
zip = { effort = "max", recompress_compressed = true, allow = ["lean"] }
[[profiles.default.override]]
glob = "**"
png = { alpha = "keep", effort = 10 }
[profiles.fast]
inherits = "default"
threads = 8
disable = ["short-json", "short-names"]
zip = { mode = "lean" }
[[profiles.fast.override]]
glob = "assets/example/**"
png = { effort = 5 }
"#,
    );
    let profile = config.resolve_profile("fast").unwrap();
    assert_eq!(profile.threads, Some(8));
    assert!(matches!(profile.passes, Some(PassSelection::Preset(ref name)) if name == "lossless"));
    assert_eq!(profile.disable, ["short-json", "short-names"]);
    assert_eq!(profile.keep_mod_files, ["optifine"]);
    let zip = profile.zip.as_ref().unwrap();
    assert_eq!(zip.effort, Some(json!("max")));
    assert_eq!(zip.mode.as_deref(), Some("lean"));
    assert_eq!(zip.recompress_compressed, Some(true));
    assert_eq!(
        profile
            .options_for("png", "assets/example/test.png")
            .unwrap(),
        [("effort".into(), json!(5))].into()
    );
    assert!(
        profile
            .options_for("png", "assets/other/test.png")
            .unwrap()
            .is_empty()
    );
    assert_eq!(config.profiles["fast"].overrides.len(), 1);
}

#[test]
fn inheritance_merges_lossy_options_but_replaces_globs() {
    let config = config(
        r#"
[profiles.default.lossy.texel-downscale]
globs = ["assets/base/**"]
ceiling = 4
max_loss = { ssim_min = 0.98 }
[profiles.fast]
inherits = "default"
[profiles.fast.lossy.texel-downscale]
globs = ["assets/fast/**"]
ceiling = 2
"#,
    );
    let profile = config.resolve_profile("fast").unwrap();
    let lossy = &profile.lossy["texel-downscale"];
    assert_eq!(lossy.globs, ["assets/fast/**"]);
    assert_eq!(lossy.options["ceiling"], 2);
    assert_eq!(lossy.options["max_loss"], json!({"ssim_min": 0.98}));
}

#[test]
fn invalid_semantics_are_located() {
    for (text, key) in [
        ("version = 1", "version"),
        ("version = 2\nprofile = 'missing'", "profile"),
        (
            "version = 2\n[profiles.a]\ninherits = 'missing'",
            "inherits",
        ),
        (
            "version = 2\n[profiles.a]\ninherits = 'b'\n[profiles.b]\ninherits = 'a'",
            "inherits",
        ),
        ("version = 2\n[profiles.a]\nthreads = 0", "threads"),
        ("version = 2\n[profiles.a]\npasses = 'lossy'", "passes"),
        (
            "version = 2\n[profiles.a]\nkeep_mod_files = ['unknown']",
            "keep_mod_files",
        ),
        ("version = 2\n[profiles.a]\nzip = { mode = 'lean' }", "mode"),
        (
            "version = 2\n[profiles.a]\nzip = { effort = true }",
            "effort",
        ),
        (
            "version = 2\n[profiles.a]\nzip = { effort = 256 }",
            "effort",
        ),
        (
            "version = 2\n[profiles.a]\nzip = { allow = ['unknown'] }",
            "allow",
        ),
        (
            "version = 2\n[input.limits]\nmax_entries = 0",
            "max_entries",
        ),
        (
            "version = 2\n[profiles.a.lossy.texel-downscale]\nglobs = []",
            "globs",
        ),
        (
            "version = 2\n[profiles.a.lossy.texel-downscale]\nglobs = ['[']",
            "globs",
        ),
        (
            "version = 2\n[profiles.a.lossy.texel-downscale]\nglobs = ['**']\nmax_loss = { ssim_min = 2.0 }",
            "ssim_min",
        ),
        (
            "version = 2\n[[profiles.a.override]]\nglob = '['\npng = { alpha = 'keep' }",
            "glob",
        ),
    ] {
        let error = parse(text, Utf8Path::new("packprune.toml"))
            .unwrap_err()
            .to_string();
        assert!(error.starts_with("packprune.toml:"), "{error}");
        assert!(error.contains(key), "{error}");
    }
}

#[test]
fn inheritance_resolver_also_refuses_unvalidated_cycles() {
    let config: Config = toml::from_str("version = 2\n[profiles.a]\ninherits = 'a'").unwrap();
    assert!(config.resolve_profile("a").unwrap_err().contains("cycle"));
    assert!(
        config
            .resolve_profile("missing")
            .unwrap_err()
            .contains("unknown profile")
    );
}

#[test]
fn glob_stars_respect_path_separators() {
    let profile = config("[[profiles.a.override]]\nglob = 'assets/*.png'\npng = { effort = 5 }")
        .resolve_profile("a")
        .unwrap();
    assert_eq!(
        profile.options_for("png", "assets/a.png").unwrap()["effort"],
        5
    );
    assert!(
        profile
            .options_for("png", "assets/example/a.png")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn relative_cache_directory_is_anchored_to_config() {
    assert_eq!(
        config("[cache]\ndir = '.cache'").cache.dir.as_deref(),
        Some("/tmp/config/.cache")
    );
    assert_eq!(
        config("[cache]\ndir = '/absolute/cache'")
            .cache
            .dir
            .as_deref(),
        Some("/absolute/cache")
    );
}

#[test]
fn budgets_and_all_input_limits_are_preserved() {
    let config = config(
        "[budgets]\nzip_bytes = 12\natlas_gpu_mib = 128\nfont_mib = 64\n[input.limits]\nmax_entries = 1\nmax_total_bytes = 2\nmax_entry_bytes = 3\nmax_ratio = 4\nratio_min_entry_bytes = 0",
    );
    assert_eq!(config.budgets.zip_bytes, Some(12));
    assert_eq!(config.budgets.atlas_gpu_mib, Some(128));
    assert_eq!(config.budgets.font_mib, Some(64));
    assert_eq!(config.input.limits.max_entries, Some(1));
    assert_eq!(config.input.limits.max_total_bytes, Some(2));
    assert_eq!(config.input.limits.max_entry_bytes, Some(3));
    assert_eq!(config.input.limits.max_ratio, Some(4));
    assert_eq!(config.input.limits.ratio_min_entry_bytes, Some(0));
}

#[test]
fn schema_matches_structural_constraints() {
    let schema = serde_json::to_value(schema()).unwrap();
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["properties"]["version"]["minimum"], 2);
    assert_eq!(schema["properties"]["version"]["maximum"], 2);
    let defs = &schema["$defs"];
    assert_eq!(defs["Profile"]["additionalProperties"], false);
    assert_eq!(defs["Lossy"]["properties"]["globs"]["minItems"], 1);
    assert!(
        defs["Lossy"]["required"]
            .as_array()
            .unwrap()
            .contains(&json!("globs"))
    );
    assert_eq!(
        defs["PassSelection"]["anyOf"][0]["enum"],
        json!(["lossless", "none"])
    );
    assert_eq!(
        defs["Zip"]["properties"]["mode"]["anyOf"][0]["enum"],
        json!(["conformant", "lean"])
    );
}

#[test]
fn native_rules_keep_document_order_and_take_the_whole_kind() {
    let profile = config("[[profiles.a.override]]\nglob = 'secret/**'\npng = { effort = 5 }\n[[profiles.a.override]]\nglob = '**'\npng = { effort = 10, alpha = 'keep' }").resolve_profile("a").unwrap();
    assert_eq!(
        profile.options_for("png", "secret/a.png").unwrap(),
        [("effort".into(), json!(5))].into()
    );
}

#[test]
fn max_loss_schema_has_the_validated_metrics_and_bounds() {
    let schema = serde_json::to_value(schema()).unwrap();
    let limits = &schema["$defs"]["Lossy"]["properties"]["max_loss"];
    assert_eq!(limits["type"], "object");
    assert_eq!(limits["additionalProperties"], false);
    assert_eq!(limits["minProperties"], 1);
    assert_eq!(limits["properties"]["ssim_min"]["minimum"], 0.0);
    assert_eq!(limits["properties"]["ssim_min"]["maximum"], 1.0);
    assert_eq!(limits["properties"]["max_channel_delta"]["maximum"], 255.0);
    assert_eq!(limits["properties"]["lsd_max"]["minimum"], 0.0);
    assert_eq!(limits["properties"]["snr_min"]["type"], "number");
}

#[test]
fn nonempty_child_lists_replace_inherited_lists() {
    let config = config(
        "[profiles.parent]\ndisable = ['short-names']\nkeep_mod_files = ['optifine']\nzip = { allow = ['lean', 'lean'] }\n[[profiles.parent.override]]\nglob = '**'\npng = { effort = 10 }\n[profiles.child]\ninherits = 'parent'\ndisable = ['short-json']\nkeep_mod_files = ['mtr3']\nzip = { allow = ['lean'] }\n[[profiles.child.override]]\nglob = 'child/**'\npng = { effort = 5 }",
    );
    let child = config.resolve_profile("child").unwrap();
    assert_eq!(child.disable, ["short-json"]);
    assert_eq!(child.keep_mod_files, ["mtr3"]);
    assert_eq!(child.zip.unwrap().allow, ["lean"]);
    assert_eq!(child.overrides.len(), 1);
    assert_eq!(child.overrides[0].glob, "child/**");
}

#[test]
fn explicit_empty_inherited_lists_are_located_and_refused() {
    let errors: Vec<_> = [
        "disable = []",
        "keep_mod_files = []",
        "zip = { allow = [] }",
        "override = []",
        "passes = []",
    ]
    .into_iter()
    .map(|list| {
        let text = format!(
            "version = 2\n[profiles.parent]\n[profiles.child]\ninherits = 'parent'\n{list}\n"
        );
        parse(&text, Utf8Path::new("packprune.toml"))
            .err()
            .map(|error| error.to_string())
    })
    .collect();
    assert!(errors.iter().all(Option::is_some), "{errors:?}");
    for error in errors.into_iter().flatten() {
        assert!(error.starts_with("packprune.toml:5:"), "{error}");
        assert!(error.contains("empty"), "{error}");
    }
    assert!(parse("version = 2\n[profiles.root]\ndisable = []\nkeep_mod_files = []\nzip = { allow = [] }\noverride = []", Utf8Path::new("packprune.toml")).is_ok());
}
