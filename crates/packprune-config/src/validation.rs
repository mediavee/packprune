use std::collections::BTreeSet;

use camino::Utf8Path;
use globset::{Glob, GlobBuilder};
use toml_edit::{Document, Item};

use crate::{Config, ConfigError, PassSelection, schema_types};

pub(crate) struct Source<'a> {
    pub(crate) doc: Document<&'a str>,
    text: &'a str,
    path: &'a Utf8Path,
}

impl<'a> Source<'a> {
    pub(crate) fn new(text: &'a str, path: &'a Utf8Path) -> Result<Self, ConfigError> {
        let doc = Document::parse(text).map_err(|error| {
            located_error(
                text,
                path,
                error.span().map_or(0, |span| span.start),
                "TOML",
                error.message(),
            )
        })?;
        Ok(Self { doc, text, path })
    }

    pub(crate) fn parse_error(&self, error: toml::de::Error) -> ConfigError {
        let offset = error.span().map_or(0, |span| span.start);
        let key = key_at(self.doc.as_item(), offset).join(".");
        let why = error
            .message()
            .replace("expected u32", "expected an integer (u32)");
        self.error_at(offset, if key.is_empty() { "config" } else { &key }, &why)
    }

    pub(crate) fn error_at(&self, offset: usize, key: &str, why: &str) -> ConfigError {
        located_error(self.text, self.path, offset, key, why)
    }

    pub(crate) fn item(&self, keys: &[&str]) -> Option<&Item> {
        let mut item = self.doc.as_item();
        for key in keys {
            item = item.as_table_like()?.get(key)?;
        }
        Some(item)
    }

    pub(crate) fn error(&self, keys: &[&str], why: &str) -> ConfigError {
        let offset = location(self.doc.as_item(), keys, 0);
        self.error_at(offset, &keys.join("."), why)
    }
}

fn location(item: &Item, keys: &[&str], offset: usize) -> usize {
    let Some((key, rest)) = keys.split_first() else {
        return offset;
    };
    if let Some((name, value)) = item
        .as_table_like()
        .and_then(|table| table.get_key_value(key))
    {
        let offset = name
            .span()
            .or_else(|| value.span())
            .map_or(offset, |span| span.start);
        return location(value, rest, offset);
    }
    if let Ok(index) = key.parse::<usize>() {
        if let Some(table) = item
            .as_array_of_tables()
            .and_then(|tables| tables.get(index))
        {
            return location(
                &Item::Table(table.clone()),
                rest,
                table.span().map_or(offset, |span| span.start),
            );
        }
        if let Some(value) = item.as_array().and_then(|array| array.get(index)) {
            return location(
                &Item::Value(value.clone()),
                rest,
                value.span().map_or(offset, |span| span.start),
            );
        }
    }
    offset
}

fn located_error(text: &str, path: &Utf8Path, offset: usize, key: &str, why: &str) -> ConfigError {
    let offset = offset.min(text.len());
    let prefix = &text.as_bytes()[..offset];
    let line = prefix.iter().filter(|&&byte| byte == b'\n').count() + 1;
    let start = prefix
        .iter()
        .rposition(|&byte| byte == b'\n')
        .map_or(0, |i| i + 1);
    let col = String::from_utf8_lossy(&prefix[start..]).chars().count() + 1;
    ConfigError::Invalid {
        file: path.to_owned(),
        line: line.try_into().unwrap_or(u32::MAX),
        col: col.try_into().unwrap_or(u32::MAX),
        key: key.to_owned(),
        why: why.to_owned(),
    }
}

pub fn glob(pattern: &str) -> Result<Glob, globset::Error> {
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .backslash_escape(true)
        .build()
}

pub(crate) fn validate(config: &Config, source: &Source<'_>) -> Result<(), ConfigError> {
    if config.version != 2 {
        return Err(source.error(&["version"], "expected config version 2"));
    }
    if let Some(name) = &config.profile
        && !config.profiles.contains_key(name)
    {
        return Err(source.error(&["profile"], "expected the name of a declared profile"));
    }
    let limits = &config.input.limits;
    for (name, value) in [
        ("max_entries", limits.max_entries.map(u64::from)),
        ("max_total_bytes", limits.max_total_bytes),
        ("max_entry_bytes", limits.max_entry_bytes),
        ("max_ratio", limits.max_ratio.map(u64::from)),
    ] {
        if value == Some(0) {
            return Err(source.error(
                &["input", "limits", name],
                "expected an integer greater than zero",
            ));
        }
    }
    for (name, profile) in &config.profiles {
        let base = ["profiles", name.as_str()];
        if profile.inherits.is_some() {
            for tail in [
                &["disable"][..],
                &["keep_mod_files"],
                &["zip", "allow"],
                &["override"],
                &["passes"],
            ] {
                let mut keys = base.to_vec();
                keys.extend(tail);
                if source
                    .item(&keys)
                    .and_then(Item::as_array)
                    .is_some_and(|array| array.is_empty())
                {
                    return Err(source.error(&keys, "explicit empty inherited lists cannot clear their parent; omit the key or use a nonempty list"));
                }
            }
        }
        let mut seen = BTreeSet::new();
        let mut current = name.as_str();
        while let Some(parent) = config.profiles[current].inherits.as_deref() {
            if !seen.insert(current) {
                return Err(
                    source.error(&[base[0], base[1], "inherits"], "profile inheritance cycle")
                );
            }
            if !config.profiles.contains_key(parent) {
                return Err(source.error(
                    &[base[0], current, "inherits"],
                    "expected the name of a declared profile",
                ));
            }
            current = parent;
        }
        if profile.threads == Some(0) {
            return Err(source.error(
                &[base[0], base[1], "threads"],
                "expected an integer greater than zero",
            ));
        }
        if let Some(PassSelection::Preset(preset)) = &profile.passes
            && !schema_types::PASS_PRESETS.contains(&preset.as_str())
        {
            return Err(source.error(
                &[base[0], base[1], "passes"],
                &format!(
                    "expected one of {:?}, or an array of pass names",
                    schema_types::PASS_PRESETS
                ),
            ));
        }
        for name in &profile.keep_mod_files {
            if !schema_types::MODS.contains(&name.as_str()) {
                return Err(source.error(
                    &[base[0], base[1], "keep_mod_files"],
                    &format!("expected one of {:?}", schema_types::MODS),
                ));
            }
        }
        if let Some(zip) = &profile.zip {
            if zip
                .mode
                .as_deref()
                .is_some_and(|mode| !schema_types::ZIP_MODES.contains(&mode))
            {
                return Err(source.error(
                    &[base[0], base[1], "zip", "mode"],
                    &format!("expected one of {:?}", schema_types::ZIP_MODES),
                ));
            }
            if zip
                .allow
                .iter()
                .any(|mode| !schema_types::ALLOWED_MODES.contains(&mode.as_str()))
            {
                return Err(source.error(
                    &[base[0], base[1], "zip", "allow"],
                    &format!("expected one of {:?}", schema_types::ALLOWED_MODES),
                ));
            }
            if zip.effort.as_ref().is_some_and(|effort| {
                effort != schema_types::MAX_EFFORT
                    && effort
                        .as_u64()
                        .is_none_or(|n| n > schema_types::MAX_EFFORT_LEVEL)
            }) {
                return Err(source.error(
                    &[base[0], base[1], "zip", "effort"],
                    &format!(
                        "expected {} or an integer in 0..{}",
                        schema_types::MAX_EFFORT,
                        schema_types::MAX_EFFORT_LEVEL
                    ),
                ));
            }
        }
        let resolved = config
            .resolve_profile(name)
            .map_err(|why| source.error(&base, &why))?;
        if let Some(zip) = &resolved.zip
            && let Some(mode) = &zip.mode
            && mode != schema_types::ZIP_CONFORMANT
            && !zip.allow.contains(mode)
        {
            return Err(source.error(
                &[base[0], base[1], "zip", "mode"],
                "non-conformant mode requires an explicit zip.allow entry",
            ));
        }
        for (pass, lossy) in &profile.lossy {
            let keys = [base[0], base[1], "lossy", pass, "globs"];
            if lossy.globs.is_empty() {
                return Err(source.error(&keys, "expected at least one explicit glob"));
            }
            for pattern in &lossy.globs {
                glob(pattern).map_err(|error| source.error(&keys, &error.to_string()))?;
            }
            if let Some(thresholds) = lossy.options.get("max_loss") {
                let keys = [base[0], base[1], "lossy", pass, "max_loss"];
                schema_types::check_thresholds(thresholds).map_err(|(metric, why)| {
                    let mut keys = keys.to_vec();
                    if let Some(metric) = metric {
                        keys.push(metric);
                    }
                    source.error(&keys, &why)
                })?;
            }
        }
        for (index, rule) in profile.overrides.iter().enumerate() {
            let offset = source.doc["profiles"][name]["override"]
                .as_array_of_tables()
                .and_then(|tables| tables.get(index))
                .and_then(|table| table.get("glob"))
                .and_then(Item::span)
                .map_or(0, |span| span.start);
            glob(&rule.glob)
                .map_err(|error| source.error_at(offset, "override.glob", &error.to_string()))?;
        }
    }
    for pattern in &config.sources.prune_unused {
        glob(pattern)
            .map_err(|error| source.error(&["sources", "prune_unused"], &error.to_string()))?;
    }
    Ok(())
}

fn key_at(item: &Item, offset: usize) -> Vec<String> {
    if let Some(table) = item.as_table_like() {
        for (name, value) in table.iter() {
            if value.span().is_some_and(|span| span.contains(&offset))
                || table
                    .key(name)
                    .and_then(|key| key.span())
                    .is_some_and(|span| span.contains(&offset))
            {
                let mut path = vec![name.to_owned()];
                path.extend(key_at(value, offset));
                return path;
            }
            let nested = key_at(value, offset);
            if !nested.is_empty() {
                let mut path = vec![name.to_owned()];
                path.extend(nested);
                return path;
            }
        }
    }
    if let Some(tables) = item.as_array_of_tables() {
        for table in tables.iter() {
            let nested = key_at(&Item::Table(table.clone()), offset);
            if !nested.is_empty() {
                return nested;
            }
        }
    }
    Vec::new()
}
