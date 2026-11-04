//! PackSquash 0.4.1 settings translated without enabling any lossy work.

use camino::{Utf8Path, Utf8PathBuf};
use serde_json::{Value, json};

use crate::{
    ConfigError, Override, Profile, Zip,
    schema_types::{MOD_MTR3, MOD_OPTIFINE, ZIP_CONFORMANT, ZIP_LEAN},
    validation::{Source, glob},
};

#[derive(Clone, Debug)]
pub struct Settings {
    pub input: Utf8PathBuf,
    pub output: Utf8PathBuf,
    pub profile: Profile,
    pub automatic_quirks: bool,
    pub quirks: Vec<String>,
    pub automatic_asset_mask: bool,
    pub ignore_hidden_files: bool,
    /// PackSquash-style lines, in settings order; libraries never print them.
    pub diagnostics: Vec<String>,
}

pub fn load(path: &Utf8Path, zip_allow: &[String]) -> Result<Settings, ConfigError> {
    let text = std::fs::read_to_string(path)?;
    let settings = parse(&text, path, zip_allow)?;
    if !settings.input.is_dir() {
        return Err(Source::new(&text, path)?.error(
            &["pack_directory"],
            "expected an existing directory (relative to the current working directory)",
        ));
    }
    Ok(settings)
}

pub fn parse(text: &str, path: &Utf8Path, zip_allow: &[String]) -> Result<Settings, ConfigError> {
    let source = Source::new(text, path)?;
    let values: toml::Table = toml::from_str(text).map_err(|error| source.parse_error(error))?;
    let mut settings = Settings {
        input: Utf8PathBuf::new(),
        output: "pack.zip".into(),
        profile: Profile {
            zip: Some(Zip {
                mode: Some(ZIP_CONFORMANT.into()),
                effort: Some(json!(20)),
                recompress_compressed: Some(false),
                allow: zip_allow.to_vec(),
                comment: Some(String::new()),
            }),
            ..Profile::default()
        },
        automatic_quirks: true,
        quirks: Vec::new(),
        automatic_asset_mask: true,
        ignore_hidden_files: true,
        diagnostics: Vec::new(),
    };
    for (name, _) in source.doc.as_table().iter() {
        let value = &values[name];
        if is_global(name) {
            apply_global(name, json_value(value), &source, &mut settings)?;
        } else if let Some(table) = value.as_table() {
            glob(name).map_err(|error| source.error(&[name], &error.to_string()))?;
            if table.is_empty() {
                return Err(source.error(&[name], "expected at least one file option"));
            }
            let mut rule = Override {
                glob: name.to_owned(),
                kinds: Default::default(),
            };
            for (key, _) in source.doc[name].as_table_like().expect("TOML table").iter() {
                let spec = FILE_OPTIONS
                    .iter()
                    .find(|spec| spec.name == key)
                    .ok_or_else(|| {
                        source.error(
                            &[name, key],
                            "unknown file option; expected a PackSquash 0.4.1 file option",
                        )
                    })?;
                let value = json_value(&table[key]);
                validate_value(spec, &value).map_err(|why| source.error(&[name, key], why))?;
                rule.kinds.entry(spec.kind.into()).or_default();
                apply_file(
                    spec,
                    value,
                    &mut rule,
                    Some(name),
                    &mut settings.diagnostics,
                );
            }
            settings.profile.overrides.push(rule);
        } else {
            apply_global(name, json_value(value), &source, &mut settings)?;
        }
    }
    if !values.contains_key("pack_directory") {
        return Err(source.error(
            &["pack_directory"],
            "missing required string pack_directory",
        ));
    }
    let mut defaults = Override {
        glob: "**".into(),
        kinds: Default::default(),
    };
    for spec in FILE_OPTIONS {
        if let Some(default) = spec.default {
            apply_file(
                spec,
                serde_json::from_str(default).expect("constant JSON default"),
                &mut defaults,
                None,
                &mut Vec::new(),
            );
        }
    }
    for rule in &mut settings.profile.overrides {
        for (kind, options) in &mut rule.kinds {
            let mut complete = defaults.kinds.get(kind).cloned().unwrap_or_default();
            complete.append(options);
            *options = complete;
        }
    }
    settings
        .profile
        .overrides
        .sort_by(|a, b| a.glob.cmp(&b.glob));
    settings.profile.overrides.push(defaults);
    Ok(settings)
}

fn is_global(name: &str) -> bool {
    matches!(
        name,
        "pack_directory"
            | "output_file_path"
            | "recompress_compressed_files"
            | "zip_compression_iterations"
            | "automatic_minecraft_quirks_detection"
            | "work_around_minecraft_quirks"
            | "automatic_asset_types_mask_detection"
            | "allow_mods"
            | "skip_pack_icon"
            | "validate_pack_metadata_file"
            | "ignore_system_and_hidden_files"
            | "zip_spec_conformance_level"
            | "size_increasing_zip_obfuscation"
            | "percentage_of_zip_structures_tuned_for_obfuscation_discretion"
            | "never_store_squash_times"
            | "threads"
            | "spooling_buffers_size"
            | "zip_comment"
    )
}

fn json_value(value: &toml::Value) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn apply_global(
    name: &str,
    value: Value,
    source: &Source<'_>,
    settings: &mut Settings,
) -> Result<(), ConfigError> {
    let invalid = |why: &str| source.error(&[name], why);
    let boolean = || value.as_bool().ok_or_else(|| invalid("expected a boolean"));
    let string = || value.as_str().ok_or_else(|| invalid("expected a string"));
    let integer = |max: u64| {
        value
            .as_u64()
            .filter(|&n| n <= max)
            .ok_or_else(|| invalid(&format!("expected an integer in 0..{max}")))
    };
    let names = || {
        value
            .as_array()
            .filter(|names| names.iter().all(Value::is_string))
            .ok_or_else(|| invalid("expected an array of strings"))
    };
    let zip = settings
        .profile
        .zip
        .as_mut()
        .expect("mapping always has ZIP settings");
    match name {
        "pack_directory" => {
            let input = string()?;
            if input.is_empty() {
                return Err(invalid("expected a nonempty directory path"));
            }
            settings.input = input.into();
        }
        "output_file_path" => {
            let output = string()?;
            if output.is_empty() {
                return Err(invalid("expected a nonempty output path"));
            }
            settings.output = output.into();
        }
        "recompress_compressed_files" => zip.recompress_compressed = Some(boolean()?),
        "zip_compression_iterations" => zip.effort = Some(json!(integer(255)?)),
        "automatic_minecraft_quirks_detection" => settings.automatic_quirks = boolean()?,
        "automatic_asset_types_mask_detection" => settings.automatic_asset_mask = boolean()?,
        "ignore_system_and_hidden_files" => settings.ignore_hidden_files = boolean()?,
        "work_around_minecraft_quirks" => {
            for quirk in names()? {
                let quirk = quirk.as_str().expect("validated string");
                match quirk {
                    "grayscale_images_gamma_miscorrection"
                    | "restrictive_banner_layer_texture_format_check"
                    | "bad_entity_eye_layer_texture_transparency_blending"
                    | "java8_zip_parsing" => {
                        if !settings.quirks.iter().any(|name| name == quirk) {
                            settings.quirks.push(quirk.into());
                        }
                    }
                    "ogg_obfuscation_incompatibility" | "png_obfuscation_incompatibility" => {
                        settings
                            .diagnostics
                            .push(format!("- {name}: {quirk}: obfuscation is not implemented"))
                    }
                    _ => return Err(invalid("expected a PackSquash 0.4.1 Minecraft quirk name")),
                }
            }
        }
        "allow_mods" => {
            for name in names()? {
                let mapped = match name.as_str().expect("validated string") {
                    "OptiFine" => MOD_OPTIFINE,
                    "Minecraft Transit Railway 3" => MOD_MTR3,
                    _ => return Err(invalid("expected OptiFine or Minecraft Transit Railway 3")),
                };
                if !settings
                    .profile
                    .keep_mod_files
                    .iter()
                    .any(|name| name == mapped)
                {
                    settings.profile.keep_mod_files.push(mapped.into());
                }
            }
        }
        "zip_spec_conformance_level" => {
            zip.mode = Some(match string()? {
                "pedantic" | "high" => ZIP_CONFORMANT,
                "balanced" if zip.allow.iter().any(|mode| mode == ZIP_LEAN) => ZIP_LEAN,
                "balanced" | "disregard" => {
                    settings.diagnostics.push(format!("! {name}: using conformant; lean needs zip.allow, obfuscation is not implemented"));
                    ZIP_CONFORMANT
                }
                _ => return Err(invalid("expected pedantic, high, balanced, or disregard")),
            }.into());
        }
        "threads" => {
            let threads = integer(u32::MAX.into())?;
            if threads == 0 {
                return Err(invalid("expected an integer greater than zero"));
            }
            settings.profile.threads = Some(threads as u32);
        }
        "zip_comment" => {
            let comment = string()?;
            if comment.len() > 65535
                || !comment.is_ascii()
                || comment
                    .as_bytes()
                    .windows(4)
                    .any(|bytes| matches!(bytes, b"PK\x03\x04" | b"PK\x05\x06" | b"PK\x07\x08"))
            {
                return Err(invalid(
                    "expected at most 65535 ASCII bytes without ZIP record signatures",
                ));
            }
            zip.comment = Some(comment.into());
        }
        "skip_pack_icon" => {
            if boolean()? {
                settings
                    .diagnostics
                    .push(format!("! {name}: pack.png is player-visible and is kept"));
            }
        }
        "validate_pack_metadata_file" => {
            if !boolean()? {
                settings.diagnostics.push(format!(
                    "- {name}: packprune always parses pack.mcmeta, including 26.2 formats"
                ));
            }
        }
        "size_increasing_zip_obfuscation" => {
            if boolean()? {
                settings
                    .diagnostics
                    .push(format!("- {name}: ZIP obfuscation is not implemented"));
            }
        }
        "percentage_of_zip_structures_tuned_for_obfuscation_discretion" => {
            if integer(100)? != 0 {
                settings
                    .diagnostics
                    .push(format!("- {name}: ZIP obfuscation is not implemented"));
            }
        }
        "never_store_squash_times" => {
            if boolean()? {
                settings.diagnostics.push(format!(
                    "- {name}: packprune never stores reuse data in the ZIP"
                ));
            }
        }
        "spooling_buffers_size" => {
            integer(u64::MAX)?;
            settings
                .diagnostics
                .push(format!("- {name}: packprune holds the pack in memory"));
        }
        _ => {
            return Err(invalid(
                "unknown global option; expected a PackSquash 0.4.1 global option or a glob table",
            ));
        }
    }
    Ok(())
}

struct FileOption {
    name: &'static str,
    kind: &'static str,
    target: &'static str,
    value_type: &'static str,
    default: Option<&'static str>,
}

const FILE_OPTIONS: &[FileOption] = &[
    FileOption {
        name: "transcode_ogg",
        kind: "audio",
        target: "transcode",
        value_type: "bool",
        default: Some("true"),
    },
    FileOption {
        name: "two_pass_vorbis_optimization_and_validation",
        kind: "audio",
        target: "optimize_ogg",
        value_type: "bool",
        default: Some("true"),
    },
    FileOption {
        name: "channels",
        kind: "audio",
        target: "",
        value_type: "channels",
        default: None,
    },
    FileOption {
        name: "sampling_frequency",
        kind: "audio",
        target: "",
        value_type: "positive",
        default: None,
    },
    FileOption {
        name: "target_pitch",
        kind: "audio",
        target: "",
        value_type: "float",
        default: Some("1.0"),
    },
    FileOption {
        name: "bitrate_control_mode",
        kind: "audio",
        target: "",
        value_type: "bitrate",
        default: Some("\"CQF\""),
    },
    FileOption {
        name: "target_bitrate_control_metric",
        kind: "audio",
        target: "",
        value_type: "float",
        default: None,
    },
    FileOption {
        name: "empty_audio_optimization",
        kind: "audio",
        target: "empty_audio",
        value_type: "bool",
        default: Some("true"),
    },
    FileOption {
        name: "ogg_obfuscation",
        kind: "audio",
        target: "",
        value_type: "bool",
        default: Some("false"),
    },
    FileOption {
        name: "minify_json",
        kind: "json",
        target: "minify",
        value_type: "bool",
        default: Some("true"),
    },
    FileOption {
        name: "delete_bloat_keys",
        kind: "json",
        target: "",
        value_type: "bool",
        default: Some("true"),
    },
    FileOption {
        name: "always_allow_json_comments",
        kind: "json",
        target: "",
        value_type: "bool",
        default: Some("true"),
    },
    FileOption {
        name: "sort_json_object_keys",
        kind: "json",
        target: "",
        value_type: "bool",
        default: Some("true"),
    },
    FileOption {
        name: "image_data_compression_iterations",
        kind: "png",
        target: "effort",
        value_type: "byte",
        default: Some("5"),
    },
    FileOption {
        name: "color_quantization_target",
        kind: "png",
        target: "quantization",
        value_type: "quantization",
        default: Some("\"auto\""),
    },
    FileOption {
        name: "color_quantization_dithering_level",
        kind: "png",
        target: "",
        value_type: "unit",
        default: Some("0.85"),
    },
    FileOption {
        name: "maximum_width_and_height",
        kind: "png",
        target: "",
        value_type: "dimension",
        default: Some("8192"),
    },
    FileOption {
        name: "skip_alpha_optimizations",
        kind: "png",
        target: "alpha",
        value_type: "bool",
        default: Some("false"),
    },
    FileOption {
        name: "downsize_if_single_color",
        kind: "png",
        target: "",
        value_type: "bool",
        default: Some("false"),
    },
    FileOption {
        name: "png_obfuscation",
        kind: "png",
        target: "",
        value_type: "bool",
        default: Some("false"),
    },
    FileOption {
        name: "shader_source_transformation_strategy",
        kind: "shader",
        target: "minify",
        value_type: "shader",
        default: Some("\"minify\""),
    },
    FileOption {
        name: "is_top_level_shader",
        kind: "shader",
        target: "",
        value_type: "bool",
        default: None,
    },
    FileOption {
        name: "minify_legacy_language",
        kind: "legacy_lang",
        target: "minify",
        value_type: "bool",
        default: Some("true"),
    },
    FileOption {
        name: "strip_legacy_language_bom",
        kind: "legacy_lang",
        target: "strip_bom",
        value_type: "bool",
        default: Some("true"),
    },
    FileOption {
        name: "minify_command_function",
        kind: "mcfunction",
        target: "minify",
        value_type: "bool",
        default: Some("true"),
    },
    FileOption {
        name: "minify_properties",
        kind: "properties",
        target: "minify",
        value_type: "bool",
        default: Some("true"),
    },
    FileOption {
        name: "nbt_compression_iterations",
        kind: "nbt",
        target: "effort",
        value_type: "byte",
        default: Some("15"),
    },
    FileOption {
        name: "force_include",
        kind: "custom",
        target: "force_include",
        value_type: "bool",
        default: Some("false"),
    },
];

fn validate_value(spec: &FileOption, value: &Value) -> Result<(), &'static str> {
    let valid = match spec.value_type {
        "bool" => value.is_boolean(),
        "float" => value.as_f64().is_some_and(f64::is_finite),
        "byte" => value.as_u64().is_some_and(|n| n <= 255),
        "positive" => value
            .as_u64()
            .is_some_and(|n| (1..=u32::MAX.into()).contains(&n)),
        "dimension" => value.as_u64().is_some_and(|n| (1..=65535).contains(&n)),
        "channels" => value.as_u64().is_some_and(|n| (1..=2).contains(&n)),
        "unit" => value.as_f64().is_some_and(|n| (0.0..=1.0).contains(&n)),
        "bitrate" => value
            .as_str()
            .is_some_and(|name| ["CQF", "VBR", "ABR", "CABR"].contains(&name)),
        "quantization" => value.as_str().is_some_and(|name| {
            [
                "none",
                "auto",
                "one_bit_depth",
                "two_bit_depth",
                "four_bit_depth",
                "eight_bit_depth",
            ]
            .contains(&name)
        }),
        "shader" => value
            .as_str()
            .is_some_and(|name| ["minify", "prettify", "keep_as_is"].contains(&name)),
        _ => unreachable!(),
    };
    if valid {
        Ok(())
    } else {
        Err(match spec.value_type {
            "bool" => "expected a boolean",
            "float" => "expected a finite number",
            "byte" => "expected an integer in 0..255",
            "positive" => "expected an integer in 1..4294967295",
            "dimension" => "expected an integer in 1..65535",
            "channels" => "expected 1 or 2 channels",
            "unit" => "expected a number in 0..1",
            "bitrate" => "expected CQF, VBR, ABR, or CABR",
            "quantization" => {
                "expected none, auto, one_bit_depth, two_bit_depth, four_bit_depth, or eight_bit_depth"
            }
            "shader" => "expected minify, prettify, or keep_as_is",
            _ => unreachable!(),
        })
    }
}

fn apply_file(
    spec: &FileOption,
    value: Value,
    rule: &mut Override,
    scope: Option<&str>,
    diagnostics: &mut Vec<String>,
) {
    let changed = spec.default.is_none_or(|default| {
        serde_json::from_str::<Value>(default).expect("constant JSON") != value
    });
    let mut diagnostic = |mark, why| {
        if changed {
            diagnostics.push(format!(
                "{mark} {}{}: {why}",
                scope.map_or_else(String::new, |glob| format!("[{glob}] ")),
                spec.name
            ));
        }
    };
    let mapped = match spec.name {
        "transcode_ogg" => {
            if value == true {
                diagnostic('!', "lossy; configure explicit globs in packprune.toml");
            }
            Some(json!(false))
        }
        "color_quantization_target" => {
            if value != "none" {
                diagnostic('!', "lossy; configure explicit globs in packprune.toml");
            }
            Some(json!("none"))
        }
        "skip_alpha_optimizations" => {
            if value == false {
                diagnostic('!', "alpha rewrite is lossy; alpha is kept");
            }
            Some(json!("keep"))
        }
        "channels"
        | "sampling_frequency"
        | "target_pitch"
        | "bitrate_control_mode"
        | "target_bitrate_control_metric"
        | "color_quantization_dithering_level"
        | "downsize_if_single_color" => {
            diagnostic('!', "lossy; configure explicit globs in packprune.toml");
            None
        }
        "ogg_obfuscation" | "png_obfuscation" => {
            diagnostic('!', "obfuscation is not implemented");
            None
        }
        "always_allow_json_comments" => {
            diagnostic(
                '-',
                "strict JSON; files with comments are listed invalid and kept byte for byte",
            );
            None
        }
        "sort_json_object_keys" => {
            diagnostic('-', "JSON key order is kept");
            None
        }
        "maximum_width_and_height" => {
            diagnostic(
                '-',
                "oversized images are kept: a texture the client draws is never dropped",
            );
            None
        }
        "delete_bloat_keys" => {
            diagnostic(
                '-',
                "the bloat-keys pass runs on every model; the keys it drops are read by no client",
            );
            None
        }
        "is_top_level_shader" => {
            diagnostic('-', "the minifier never expands or drops directives");
            None
        }
        "shader_source_transformation_strategy" => {
            if value == "prettify" {
                diagnostic('-', "bytes kept; prettification would add bytes");
            }
            Some(json!(value == "minify"))
        }
        _ => Some(value),
    };
    if let Some(value) = mapped {
        rule.kinds
            .entry(spec.kind.into())
            .or_default()
            .insert(spec.target.into(), value);
    }
}

#[cfg(test)]
#[path = "mapping_tests.rs"]
mod tests;
