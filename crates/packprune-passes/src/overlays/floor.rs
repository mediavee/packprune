use packprune_pack::PhysPath;
use packprune_pack::meta::{ANY_MINOR, parse_format};
use packprune_pass::{Edits, Loss, Pass, PassCx, PassError, Stage};
use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::BTreeSet;

use super::entries::{fail, read, replace};

pub struct OverlayFloor;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Options {
    oldest_format: u32,
}

/// The floor of `options`, as the config is checked and as the pass reads it.
fn oldest_format(options: &toml::Table) -> Result<u32, String> {
    let options: Options = options
        .clone()
        .try_into()
        .map_err(|error: toml::de::Error| error.to_string())?;
    if options.oldest_format > i32::MAX as u32 {
        return Err("oldest_format exceeds i32::MAX".into());
    }
    Ok(options.oldest_format)
}

impl Pass for OverlayFloor {
    fn name(&self) -> &'static str {
        "overlay-floor"
    }
    fn stage(&self) -> Stage {
        Stage::Overlays
    }
    fn loss(&self) -> Loss {
        Loss::Lossy
    }
    fn options_schema(&self) -> schemars::Schema {
        schemars::schema_for!(Options)
    }

    fn validate_options(&self, options: &toml::Table) -> Result<(), String> {
        oldest_format(options).map(drop)
    }

    fn run(&self, cx: &PassCx<'_>) -> Result<Edits, PassError> {
        let oldest = oldest_format(cx.options).map_err(|why| fail(self.name(), why))?;
        let mut edits = Edits::default();
        let path = PhysPath::new("pack.mcmeta").expect("valid path");
        let Some(mcmeta) = cx.pack.files.get(&path) else {
            return Ok(edits);
        };
        let floor = (oldest, 0);
        let newest = parse_format(&cx.vanilla.pack_format).map(|(major, _)| (major, ANY_MINOR));
        let entries = read(&mcmeta.blob, self.name())?;
        let folders: BTreeSet<_> = cx
            .pack
            .files
            .keys()
            .filter_map(|path| path.as_str().split_once('/').map(|(folder, _)| folder))
            .collect();
        let mut kept = Vec::new();
        let mut declared = BTreeSet::new();
        let mut used = BTreeSet::new();
        let (mut below, mut above, mut missing, mut raised) = (0, 0, 0, 0);
        for entry in &entries {
            declared.insert(entry.directory.as_str());
            if !folders.contains(entry.directory.as_str()) {
                missing += 1;
            } else if entry.range.max < floor {
                below += 1;
            } else if newest.is_some_and(|newest| entry.range.min > newest) {
                above += 1;
            } else {
                kept.push(if entry.range.min < floor {
                    raised += 1;
                    entry.from(oldest)
                } else {
                    entry.clone()
                });
                used.insert(entry.directory.as_str());
            }
        }
        let mut bytes = 0;
        for (path, entry) in &cx.pack.files {
            let folder = path
                .as_str()
                .split_once('/')
                .map_or("", |(folder, _)| folder);
            if declared.contains(folder) && !used.contains(folder) && !cx.pinned.contains(path) {
                edits.removed.insert(path.clone());
                bytes += entry.blob.len();
            }
        }
        if kept.len() != entries.len() || raised > 0 {
            edits
                .replaced
                .insert(path, replace(&mcmeta.blob, &kept, self.name())?.into());
        }
        edits.changes.push(format!("{} overlay entries dropped ({below} below format {oldest}, {above} above the client's, {missing} without a directory), {raised} starting at {oldest}", below + above + missing));
        edits.changes.push(format!(
            "{} files of dropped overlays removed ({bytes} B)",
            edits.removed.len()
        ));
        Ok(edits)
    }
}
