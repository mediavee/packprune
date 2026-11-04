//! `packprune.toml`: types with every section, validation, JSON schema, PackSquash settings mapping.

mod options;
mod profiles;
mod schema_types;
mod validation;

pub mod packsquash;

pub use profiles::Overrides;
/// The config's glob syntax (`override.glob`, `lossy.<pass>.globs`), for callers that match paths with it.
pub use validation::glob;

use std::collections::BTreeMap;
use std::io;

use camino::{Utf8Path, Utf8PathBuf};
use schemars::JsonSchema;
use serde::Deserialize;

/// A pass's or a file kind's options, read by their owner when the config is loaded.
pub type Options = BTreeMap<String, serde_json::Value>;

#[derive(Clone, Copy, Debug)]
pub enum OptionsOwner<'a> {
    LossyPass(&'a str),
    Kind(&'a str),
}

/// The caller owns pass and kind names and reads each table with the code that will run with it, so a table it
/// accepts here is one the run accepts. A file kind's owner is lossless: a lossy value fails its reading.
pub trait OptionsCheck {
    fn check_owner(&self, owner: OptionsOwner<'_>) -> Result<(), String>;
    /// `Err` names the offending key in its text.
    fn check_options(&self, owner: OptionsOwner<'_>, options: &Options) -> Result<(), String>;
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[schemars(range(min = 2, max = 2))]
    pub version: u32,
    pub profile: Option<String>,
    #[serde(default)]
    pub vanilla: Vanilla,
    #[serde(default)]
    pub sources: Sources,
    #[serde(default)]
    pub input: Input,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
    #[serde(default)]
    pub budgets: Budgets,
    #[serde(default)]
    pub cache: Cache,
    #[serde(default)]
    pub report: Report,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Vanilla {
    pub client_jar: Option<String>,
    pub asset_index: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Sources {
    pub keep: Option<String>,
    pub nexo: Option<String>,
    #[serde(default)]
    pub configs: Vec<String>,
    #[serde(default)]
    pub datapacks: Vec<String>,
    #[serde(default)]
    pub prune_unused: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Input {
    #[serde(default)]
    pub limits: InputLimits,
}

/// Overrides of the reader's limits; an absent key keeps its default.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InputLimits {
    #[schemars(range(min = 1))]
    pub max_entries: Option<u32>,
    #[schemars(range(min = 1))]
    pub max_total_bytes: Option<u64>,
    #[schemars(range(min = 1))]
    pub max_entry_bytes: Option<u64>,
    #[schemars(range(min = 1))]
    pub max_ratio: Option<u32>,
    pub ratio_min_entry_bytes: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub inherits: Option<String>,
    pub passes: Option<PassSelection>,
    #[serde(default)]
    pub disable: Vec<String>,
    #[schemars(range(min = 1))]
    pub threads: Option<u32>,
    pub oldest_format: Option<u32>,
    #[serde(default)]
    #[schemars(schema_with = "schema_types::mods")]
    pub keep_mod_files: Vec<String>,
    pub zip: Option<Zip>,
    #[serde(default)]
    pub lossy: BTreeMap<String, Lossy>,
    #[serde(default, rename = "override")]
    pub overrides: Vec<Override>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum PassSelection {
    /// `"lossless"` or `"none"`.
    Preset(#[schemars(schema_with = "schema_types::pass_preset")] String),
    Names(Vec<String>),
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Zip {
    #[schemars(schema_with = "schema_types::zip_mode")]
    pub mode: Option<String>,
    /// 0 stores, 1 to 254 deflate with libdeflate, 255 or `"max"` also tries Zopfli.
    #[schemars(schema_with = "schema_types::effort")]
    pub effort: Option<serde_json::Value>,
    pub recompress_compressed: Option<bool>,
    #[serde(default)]
    #[schemars(schema_with = "schema_types::allowed_modes")]
    pub allow: Vec<String>,
    pub comment: Option<String>,
}

/// A lossy pass is off unless its table exists; its globs are required.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[schemars(transform = schema_types::lossy)]
pub struct Lossy {
    #[schemars(length(min = 1))]
    pub globs: Vec<String>,
    #[serde(flatten)]
    pub options: Options,
}

/// Per glob, in order: the first that sets an option for the file's kind wins.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct Override {
    pub glob: String,
    #[serde(flatten)]
    pub kinds: BTreeMap<String, Options>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Budgets {
    pub zip_bytes: Option<u64>,
    pub atlas_gpu_mib: Option<u64>,
    pub font_mib: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Cache {
    pub dir: Option<String>,
    pub max_bytes: Option<u64>,
}

/// Where the PackSquash drop-in keeps its report.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Report {
    /// The report of the last run is the folder `latest` in it, its files replaced by name at each run and nothing
    /// else touched. Relative to this file; `reports` when absent.
    pub dir: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// `file:line:col`, the key, what is accepted.
    #[error("{file}:{line}:{col}: {key}: {why}")]
    Invalid {
        file: Utf8PathBuf,
        line: u32,
        col: u32,
        key: String,
        why: String,
    },
    #[error(transparent)]
    Io(#[from] io::Error),
}

pub fn load(path: &Utf8Path) -> Result<Config, ConfigError> {
    parse(&std::fs::read_to_string(path)?, path)
}

/// Validates structure without filesystem access; use `parse_with` for caller-owned option checks.
pub fn parse(text: &str, path: &Utf8Path) -> Result<Config, ConfigError> {
    parse_config(text, path, None)
}

fn parse_config(
    text: &str,
    path: &Utf8Path,
    check: Option<&dyn OptionsCheck>,
) -> Result<Config, ConfigError> {
    let source = validation::Source::new(text, path)?;
    let mut config: Config = toml::from_str(text).map_err(|error| source.parse_error(error))?;
    validation::validate(&config, &source)?;
    if let Some(check) = check {
        options::validate(&config, &source, check)?;
    }
    if let Some(dir) = &mut config.cache.dir {
        let cache_path = Utf8Path::new(dir);
        if cache_path.is_relative() {
            *dir = path
                .parent()
                .unwrap_or(Utf8Path::new("."))
                .join(cache_path)
                .to_string();
        }
    }
    Ok(config)
}

/// Checks caller-owned option tables before their source positions are discarded.
pub fn parse_with(
    text: &str,
    path: &Utf8Path,
    check: &dyn OptionsCheck,
) -> Result<Config, ConfigError> {
    parse_config(text, path, Some(check))
}

/// The config schema, the passes' `options_schema()` merged in by the caller.
pub fn schema() -> schemars::Schema {
    schemars::schema_for!(Config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_input_limit_is_a_key() {
        toml::from_str::<Config>(
            "version = 2\n\
             [input.limits]\n\
             max_entries = 1\n\
             max_total_bytes = 2\n\
             max_entry_bytes = 3\n\
             max_ratio = 4\n\
             ratio_min_entry_bytes = 2097152\n",
        )
        .unwrap();
    }
}

#[cfg(test)]
mod validation_tests {
    use super::*;

    fn loaded(text: &str) -> Result<Config, ConfigError> {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join("packprune.toml")).unwrap();
        std::fs::write(&path, text).unwrap();
        load(&path)
    }

    #[test]
    fn loads_minimal_config() {
        assert_eq!(loaded("version = 2").unwrap().version, 2);
    }

    #[test]
    fn rejects_unknown_key_at_its_position() {
        let error = loaded("version = 2\nunknown = true\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("packprune.toml:2:1:"), "{error}");
        assert!(error.contains("unknown field `unknown`"), "{error}");
    }

    #[test]
    fn rejects_wrong_type_at_its_position() {
        let error = loaded("version = 2\n[profiles.default]\nthreads = 'four'\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("packprune.toml:3:11:"), "{error}");
        assert!(error.contains("integer"), "{error}");
    }

    #[test]
    fn rejects_lossy_without_globs_at_its_table() {
        let error = loaded("version = 2\n[profiles.default.lossy.texel-downscale]\nceiling = 4\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("packprune.toml:2:"), "{error}");
        assert!(error.contains("globs"), "{error}");
    }
}

#[cfg(test)]
mod config_tests;

#[cfg(test)]
mod options_tests;
