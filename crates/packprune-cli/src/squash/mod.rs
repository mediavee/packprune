//! The PackSquash drop-in: Nexo, ItemsAdder or Oraxen run `<executable> <settings.toml>` and read the
//! zip at `output_file_path`. Lines carry PackSquash's marks, which Nexo logs by level (`!` error, `*` warning, `-`
//! information) and drops unmarked unless in debug.

use std::io::{self, Write};

use camino::{Utf8Path, Utf8PathBuf};
use packprune_config::ConfigError;
use packprune_config::packsquash::{self, Settings};
use packprune_engine::run::{self, Artefact, DropIn, Quirks, Request, RunError, Squashed};

use crate::ExitClass;
use crate::commands::optimise::{self, VanillaArgs};

/// packprune's own config, when not `packprune.toml` beside the executable.
const CONFIG_VARIABLE: &str = "PACKPRUNE_CONFIG";
const CONFIG_NAME: &str = "packprune.toml";

#[derive(Debug, clap::Args)]
pub struct Args {
    /// PackSquash settings file.
    pub settings: Utf8PathBuf,
}

pub fn run(args: &Args) -> ExitClass {
    squash(&args.settings, config_path(), &mut std::io::stdout().lock()).unwrap_or_else(|error| {
        eprintln!("! packprune: stdout: {error}");
        ExitClass::Error
    })
}

/// `PACKPRUNE_CONFIG`, else `packprune.toml` beside the executable with its symlinks resolved: Nexo passes only the
/// settings file, which it rewrites on every build.
fn config_path() -> Result<Utf8PathBuf, String> {
    if let Some(path) = std::env::var_os(CONFIG_VARIABLE) {
        return Utf8PathBuf::from_path_buf(path.into())
            .map_err(|path| format!("{CONFIG_VARIABLE} {}: not UTF-8", path.display()));
    }
    let executable = std::env::current_exe()
        .and_then(|path| path.canonicalize())
        .map_err(|error| format!("the executable's path: {error}"))?;
    let path = Utf8PathBuf::from_path_buf(executable.with_file_name(CONFIG_NAME))
        .map_err(|path| format!("{}: not UTF-8", path.display()))?;
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!(
            "no {CONFIG_NAME} beside the executable ({path}) and {CONFIG_VARIABLE} is not set"
        ))
    }
}

fn squash(
    settings: &Utf8Path,
    config: Result<Utf8PathBuf, String>,
    out: &mut impl Write,
) -> io::Result<ExitClass> {
    let loaded = config.and_then(own_config);
    let settings = match read_settings(settings, &loaded) {
        Ok(settings) => settings,
        Err(error) => {
            writeln!(out, "! {}", one_line(&error.to_string()))?;
            return Ok(ExitClass::Refused);
        }
    };
    for line in settings.diagnostics.iter().chain(&unmapped(&settings)) {
        writeln!(out, "{line}")?;
    }
    let limits = loaded
        .as_ref()
        .ok()
        .and_then(|(_, loaded, _)| loaded.config.as_ref())
        .map(|config| config.input.limits.clone())
        .unwrap_or_default();
    let reports = loaded
        .as_ref()
        .ok()
        .and_then(|(_, loaded, _)| loaded.reports());
    let full =
        loaded.and_then(|(path, _, _)| request(&settings, path).map_err(|error| error.to_string()));
    let drop_in = DropIn {
        input: settings.input.clone(),
        output: settings.output.clone(),
        profile: match &full {
            Ok(request) => request.profile.clone(),
            Err(_) => settings.profile.clone(),
        },
        limits,
        quirks: quirks(&settings.quirks),
        reports,
    };
    match run::squash(&drop_in, full) {
        Ok(squashed) => report(&squashed, &drop_in.output, out),
        Err(error) => {
            writeln!(out, "! {}", one_line(&error.to_string()))?;
            Ok(if error.is_refusal() {
                ExitClass::Refused
            } else {
                ExitClass::Error
            })
        }
    }
}

type OwnConfig = Result<(Utf8PathBuf, optimise::Loaded, Vec<String>), String>;

/// packprune's config read, with the zip modes its profile allows.
fn own_config(path: Utf8PathBuf) -> OwnConfig {
    let loaded = optimise::load(Some(&path)).map_err(|error| error.to_string())?;
    let profile =
        optimise::profile(loaded.config.as_ref(), None).map_err(|error| error.to_string())?;
    let allow = profile.zip.map(|zip| zip.allow).unwrap_or_default();
    Ok((path, loaded, allow))
}

/// The settings' `balanced` level maps to `lean` only when packprune's config allows it.
fn read_settings(path: &Utf8Path, config: &OwnConfig) -> Result<Settings, ConfigError> {
    let allow = match config {
        Ok((_, _, allow)) => allow.as_slice(),
        Err(_) => &[],
    };
    packsquash::load(path, allow)
}

/// The run `packprune optimise --config <config>` makes of the pack, its profile over the settings' one: what the
/// config sets wins, its overrides come before the settings' globs.
fn request(settings: &Settings, config: Utf8PathBuf) -> Result<Request, RunError> {
    let mut request = optimise::request(&optimise::Args {
        pack: settings.input.clone(),
        output: settings.output.clone(),
        passes: None,
        config: Some(config),
        profile: None,
        threads: None,
        expect_report: None,
        vanilla: VanillaArgs {
            client_jar: None,
            asset_index: None,
        },
        sources: Vec::new(),
        keep: None,
        prune_unused: Vec::new(),
    })?;
    let own = request.profile;
    let mut profile = settings.profile.clone();
    profile.layer(&own);
    profile.overrides = own
        .overrides
        .into_iter()
        .chain(settings.profile.overrides.clone())
        .collect();
    // Java 8 clients refuse the lean layout, whatever the config allows.
    if settings
        .quirks
        .iter()
        .any(|quirk| quirk == "java8_zip_parsing")
        && let Some(zip) = &mut profile.zip
    {
        zip.mode = Some("conformant".into());
    }
    request.profile = profile;
    Ok(request)
}

/// The settings' quirk names, PackSquash 0.4.1's.
fn quirks(names: &[String]) -> Quirks {
    let named = |name: &str| names.iter().any(|quirk| quirk == name);
    let imageio_grey = named("grayscale_images_gamma_miscorrection");
    let abgr_layers = named("restrictive_banner_layer_texture_format_check");
    Quirks {
        imageio_grey,
        abgr_layers,
        additive_eyes: named("bad_entity_eye_layer_texture_transparency_blending"),
        // Both name a client that decodes textures with javax.imageio.
        imageio_textures: imageio_grey || abgr_layers,
    }
}

/// The `-` lines of the global switches the engine does not take: each only keeps more of the pack than PackSquash
/// would, never less.
fn unmapped(settings: &Settings) -> Vec<String> {
    let mut lines = Vec::new();
    if !settings.automatic_quirks {
        lines.push(
            "- automatic_minecraft_quirks_detection: packprune still derives quirks from the formats of the views that \
             show each file"
                .into(),
        );
    }
    if !settings.automatic_asset_mask {
        lines.push(
            "- automatic_asset_types_mask_detection: drop-unread drops only files no client of a declared view reads"
                .into(),
        );
    }
    if !settings.ignore_hidden_files {
        lines.push(
            "- ignore_system_and_hidden_files: drop-unread drops only the hidden files no client can name".into(),
        );
    }
    lines
}

fn report(squashed: &Squashed, output: &Utf8Path, out: &mut impl Write) -> io::Result<ExitClass> {
    let shipped = match squashed.artefact {
        Artefact::Optimised => "the optimised pack",
        Artefact::Pruned => "the pruned pack, without the passes and file optimisers",
        Artefact::Original => "the original pack, as it is",
    };
    for reason in &squashed.reasons {
        writeln!(out, "! packprune: {}", one_line(reason))?;
    }
    let sha1: String = squashed
        .written
        .sha1
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    writeln!(
        out,
        "- packprune {}: wrote {shipped} to {output} ({} B, sha1 {sha1})",
        env!("CARGO_PKG_VERSION"),
        squashed.written.bytes
    )?;
    let Some(summary) = &squashed.summary else {
        return Ok(ExitClass::Clean);
    };
    for warning in &summary.warnings {
        writeln!(out, "* packprune: {}", one_line(warning))?;
    }
    writeln!(
        out,
        "- packprune: kept {} files, {} B; dropped {} files, {} B",
        summary.kept.files,
        summary.kept.bytes,
        summary.pruned.files + summary.repruned.files,
        summary.pruned.bytes + summary.repruned.bytes
    )?;
    let changed: usize = summary.passes.iter().map(|pass| pass.changed.len()).sum();
    if let Some(proof) = &summary.proof {
        writeln!(
            out,
            "- packprune: {} passes changed {changed} files; the oracle found {} views and {} keys the same for the \
             player",
            summary.passes.len(),
            proof.views,
            proof.keys
        )?;
    }
    if let Some(loss) = &summary.loss {
        writeln!(
            out,
            "* packprune: lossy passes: {} of {} keys differ, as configured",
            loss.differing_keys, loss.keys
        )?;
    }
    for loss in &summary.optimiser_loss {
        writeln!(
            out,
            "* packprune: loss {}: {} {}",
            one_line(&loss.key),
            loss.metric,
            loss.value
        )?;
    }
    if summary.cache_hits + summary.cache_misses > 0 {
        writeln!(
            out,
            "- packprune: cache {} hits, {} misses",
            summary.cache_hits, summary.cache_misses
        )?;
    }
    for exceeded in &summary.exceeded {
        writeln!(
            out,
            "! packprune: budget {} exceeded: {} > {}",
            exceeded.budget, exceeded.measured, exceeded.limit
        )?;
    }
    Ok(if summary.exceeded.is_empty() {
        ExitClass::Clean
    } else {
        ExitClass::Findings
    })
}

/// Nexo logs a line at a time and only the first carries the mark.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests;
