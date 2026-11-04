//! `packprune optimise`: the run, its summary on stdout.

use std::panic::{AssertUnwindSafe, catch_unwind};

use camino::{Utf8Path, Utf8PathBuf};
use packprune_config::{Config, PassSelection, Profile};
use packprune_engine::run::reports::read_report;
use packprune_engine::run::{
    self, CacheAt, Expected, Registry, Request, RunError, SourceRequest, Summary, VanillaInputs,
};

use crate::ExitClass;

#[derive(Debug, clap::Args)]
pub struct Args {
    /// Pack directory or zip.
    pub pack: Utf8PathBuf,
    /// Output zip.
    #[arg(short, long)]
    pub output: Utf8PathBuf,
    /// `lossless` (every lossless pass and file optimiser, the default), `none` (prune only) or names,
    /// comma-separated; overrides the profile's `passes`.
    #[arg(long, value_delimiter = ',')]
    pub passes: Option<Vec<String>>,
    /// packprune.toml.
    #[arg(long)]
    pub config: Option<Utf8PathBuf>,
    #[arg(long)]
    pub profile: Option<String>,
    /// Worker threads; the machine's cores by default.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    pub threads: Option<u32>,
    /// `report.json` of an earlier `report` on the same inputs: refuse the run if the pack, client jar, asset index,
    /// sources or the files to drop differ from it.
    #[arg(long)]
    pub expect_report: Option<Utf8PathBuf>,
    #[command(flatten)]
    pub vanilla: VanillaArgs,
    /// A usage source on its input, `<id>=<path>` (keep, configs, datapacks, nexo), repeatable; replaces
    /// `[sources]`.
    #[arg(long = "source", value_name = "ID=PATH")]
    pub sources: Vec<String>,
    /// Keep-list; shorthand for `--source keep=<file>`.
    #[arg(long)]
    pub keep: Option<Utf8PathBuf>,
    /// `<source>:<location glob>`: item declarations that are roots only when a source uses them, repeatable;
    /// replaces `sources.prune_unused`.
    #[arg(long = "prune-unused", value_name = "SOURCE:GLOB")]
    pub prune_unused: Vec<String>,
}

/// The client the views resolve over; `[vanilla]` of the config when absent.
#[derive(Debug, clap::Args)]
pub struct VanillaArgs {
    /// Vanilla client jar of the target version.
    #[arg(long)]
    pub client_jar: Option<Utf8PathBuf>,
    /// Launcher asset index (`indexes/<id>.json`) for the vanilla sound names.
    #[arg(long)]
    pub asset_index: Option<Utf8PathBuf>,
}

pub fn run(args: &Args) -> ExitClass {
    let result = catch_unwind(AssertUnwindSafe(|| {
        request(args).and_then(|request| run::optimise(&request))
    }));
    match result {
        Ok(Ok(summary)) => {
            print(&summary);
            if summary.exceeded.is_empty() {
                ExitClass::Clean
            } else {
                ExitClass::Findings
            }
        }
        Ok(Err(error)) => {
            let refused = error.is_refusal();
            eprintln!("{}: {error}", if refused { "refused" } else { "error" });
            if refused {
                ExitClass::Refused
            } else {
                ExitClass::Error
            }
        }
        // The panic hook printed where: a bug of the tool, never written output.
        Err(_) => ExitClass::Error,
    }
}

/// The config file read and checked against the passes' options, and the directory its paths are relative to.
pub(crate) struct Loaded {
    pub config: Option<Config>,
    pub dir: Option<Utf8PathBuf>,
}

pub(crate) fn load(path: Option<&Utf8Path>) -> Result<Loaded, RunError> {
    let Some(path) = path else {
        return Ok(Loaded {
            config: None,
            dir: None,
        });
    };
    // Absolute before the config resolves its relative paths, so they never depend on how the file was named.
    let path = path
        .canonicalize_utf8()
        .map_err(|error| RunError::Refused(format!("--config {path}: {error}")))?;
    let text = std::fs::read_to_string(&path)
        .map_err(|error| RunError::Refused(format!("--config {path}: {error}")))?;
    let config = packprune_config::parse_with(&text, &path, &Registry::default())
        .map_err(|error| RunError::Refused(error.to_string()))?;
    let dir = path.parent().map(Utf8Path::to_owned);
    Ok(Loaded {
        config: Some(config),
        dir,
    })
}

impl Loaded {
    /// A path of the config, relative to its file.
    fn path(&self, value: &str) -> Utf8PathBuf {
        match &self.dir {
            Some(dir) => dir.join(value),
            None => value.into(),
        }
    }

    /// `[report] dir`, `reports` beside the config when it is not set: where the drop-in keeps its reports. None
    /// without a config file.
    pub(crate) fn reports(&self) -> Option<Utf8PathBuf> {
        let config = self.config.as_ref()?;
        Some(self.path(config.report.dir.as_deref().unwrap_or("reports")))
    }

    pub(crate) fn vanilla(&self, args: &VanillaArgs) -> Result<VanillaInputs, RunError> {
        let configured = self.config.as_ref().map(|config| &config.vanilla);
        let client_jar = args
            .client_jar
            .clone()
            .or_else(|| configured?.client_jar.as_deref().map(|jar| self.path(jar)))
            .ok_or_else(|| {
                RunError::Refused(
                    "a client jar is required: --client-jar or [vanilla] client_jar".into(),
                )
            })?;
        let asset_index = args.asset_index.clone().or_else(|| {
            configured?
                .asset_index
                .as_deref()
                .map(|index| self.path(index))
        });
        for (flag, path) in [
            ("client jar", Some(&client_jar)),
            ("asset index", asset_index.as_ref()),
        ] {
            if let Some(path) = path
                && !path.is_file()
            {
                return Err(RunError::Refused(format!("{flag} {path}: no such file")));
            }
        }
        Ok(VanillaInputs {
            client_jar,
            asset_index,
        })
    }
}

pub(crate) fn request(args: &Args) -> Result<Request, RunError> {
    let loaded = load(args.config.as_deref())?;
    let config = loaded.config.as_ref();
    let mut profile = profile(config, args.profile.as_deref())?;
    if let Some(passes) = &args.passes {
        profile.passes = Some(match passes.as_slice() {
            [preset] if preset == "lossless" || preset == "none" => {
                PassSelection::Preset(preset.clone())
            }
            names => PassSelection::Names(names.to_vec()),
        });
    }
    if args.threads.is_some() {
        profile.threads = args.threads;
    }
    let configured = config.map(|config| &config.sources);
    let mut sources = Vec::new();
    if args.sources.is_empty() && args.keep.is_none() {
        if let Some(configured) = configured {
            let mut add = |id: &str, path: &str| {
                sources.push(SourceRequest {
                    id: id.into(),
                    path: loaded.path(path),
                });
            };
            configured.keep.iter().for_each(|path| add("keep", path));
            configured.nexo.iter().for_each(|path| add("nexo", path));
            configured
                .configs
                .iter()
                .for_each(|path| add("configs", path));
            configured
                .datapacks
                .iter()
                .for_each(|path| add("datapacks", path));
        }
    } else {
        if let Some(keep) = &args.keep {
            sources.push(SourceRequest {
                id: "keep".into(),
                path: keep.clone(),
            });
        }
        for source in &args.sources {
            let Some((id, path)) = source.split_once('=').filter(|(id, _)| !id.is_empty()) else {
                return Err(RunError::Refused(format!(
                    "--source {source}: expected <id>=<path>"
                )));
            };
            sources.push(SourceRequest {
                id: id.into(),
                path: path.into(),
            });
        }
    }
    let prune_unused = if args.prune_unused.is_empty() {
        configured
            .map(|sources| sources.prune_unused.clone())
            .unwrap_or_default()
    } else {
        args.prune_unused.clone()
    };
    let cache = config.map(|config| &config.cache);
    Ok(Request {
        input: args.pack.clone(),
        output: args.output.clone(),
        vanilla: loaded.vanilla(&args.vanilla)?,
        sources,
        prune_unused,
        profile,
        limits: config
            .map(|config| config.input.limits.clone())
            .unwrap_or_default(),
        budgets: config
            .map(|config| config.budgets.clone())
            .unwrap_or_default(),
        cache: CacheAt::resolve(
            loaded.dir.as_deref(),
            cache
                .and_then(|cache| cache.dir.as_deref())
                .map(Utf8Path::new),
            cache.and_then(|cache| cache.max_bytes),
        )?,
        expect: args.expect_report.as_deref().map(expected).transpose()?,
    })
}

/// The report `--expect-report` names, refused when it cannot be read as one.
fn expected(path: &Utf8Path) -> Result<Expected, RunError> {
    let report = read_report(path.as_std_path())
        .map_err(|error| RunError::Refused(format!("--expect-report {error}")))?;
    Ok(Expected {
        path: path.to_owned(),
        report,
    })
}

/// `--profile`, else the config's `profile`, else `default`; a profile the config does not declare is refused when
/// named, and empty when it is the implicit `default`.
pub(crate) fn profile(config: Option<&Config>, named: Option<&str>) -> Result<Profile, RunError> {
    let Some(config) = config else {
        return match named {
            Some(_) => Err(RunError::Refused("--profile needs --config".into())),
            None => Ok(Profile::default()),
        };
    };
    let name = named.or(config.profile.as_deref());
    match name {
        Some(name) => config.resolve_profile(name),
        None if config.profiles.contains_key("default") => config.resolve_profile("default"),
        None => Ok(Profile::default()),
    }
    .map_err(|why| RunError::Refused(format!("--profile: {why}")))
}

fn print(summary: &Summary) {
    for warning in &summary.warnings {
        eprintln!("warning: {warning}");
    }
    let sha1: String = summary
        .written
        .sha1
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    println!(
        "kept {} files, {} B: {} (sha1 {sha1}, {} B)",
        summary.kept.files, summary.kept.bytes, summary.output, summary.written.bytes
    );
    println!(
        "dropped {} files, {} B",
        summary.pruned.files + summary.repruned.files,
        summary.pruned.bytes + summary.repruned.bytes
    );
    if summary.passes_ran {
        for pass in &summary.passes {
            println!(
                "pass {} ({}): {} files changed, {:+} B raw, {} -> {} files; {}",
                pass.pass,
                match pass.loss {
                    run::Loss::Lossless => "lossless",
                    run::Loss::Lossy => "lossy",
                },
                pass.changed.len(),
                pass.bytes_after as i64 - pass.bytes_before as i64,
                pass.files_before,
                pass.files_after,
                pass.changes.join("; ")
            );
        }
        if summary.repruned.files > 0 {
            println!(
                "re-prune: {} files no view reaches after the passes dropped, {} B",
                summary.repruned.files, summary.repruned.bytes
            );
        }
        match &summary.proof {
            Some(proof) => println!(
                "oracle: {} views, {} keys, the same for the player ({} ms)",
                proof.views, proof.keys, proof.millis
            ),
            None => println!("oracle: not run, the lossless passes changed nothing"),
        }
        if let Some(loss) = &summary.loss {
            println!(
                "loss of the lossy passes: {} keys differ of {} ({} ms)",
                loss.differing_keys, loss.keys, loss.millis
            );
            for difference in loss.differences.iter().take(20) {
                println!("  {difference}");
            }
        }
    }
    for optimiser in summary.optimisers.iter().filter(|o| o.files > 0) {
        println!(
            "optimiser {}: {} files, {} -> {} B",
            optimiser.optimiser, optimiser.files, optimiser.bytes_before, optimiser.bytes_after
        );
    }
    for loss in &summary.optimiser_loss {
        println!("  loss {}: {} {}", loss.key, loss.metric, loss.value);
    }
    if summary.cache_hits + summary.cache_misses > 0 {
        println!(
            "cache: {} hits, {} misses",
            summary.cache_hits, summary.cache_misses
        );
    }
    println!(
        "self-check: nothing left to drop, nothing newly broken; the output still has {} missing references and {} \
         sprites not stitched, which the input has too",
        summary.missing, summary.not_stitched
    );
    for exceeded in &summary.exceeded {
        println!(
            "budget {} exceeded: {} > {}",
            exceeded.budget, exceeded.measured, exceeded.limit
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(pack: &Utf8PathBuf, output: &Utf8PathBuf) -> Args {
        Args {
            pack: pack.clone(),
            output: output.clone(),
            passes: None,
            config: None,
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
        }
    }

    fn temp(dir: &tempfile::TempDir, name: &str) -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(dir.path().join(name)).expect("utf-8 temp dir")
    }

    /// A file that stands for the jar: the input is read and refused before the jar is.
    fn client_jar(dir: &tempfile::TempDir) -> Utf8PathBuf {
        let jar = temp(dir, "client.jar");
        std::fs::write(&jar, b"not read").unwrap();
        jar
    }

    /// A one-entry stored zip whose name is `name`, built by hand: the zip writer refuses such names.
    fn zip_named(name: &str) -> Vec<u8> {
        let data = b"x";
        let mut zip = Vec::new();
        let local = |zip: &mut Vec<u8>| {
            zip.extend(0x0403_4b50_u32.to_le_bytes());
            zip.extend([20, 0, 0, 0, 0, 0, 0, 0, 0x21, 0]);
            zip.extend([0; 4]); // crc, not read before the path is refused
            zip.extend((data.len() as u32).to_le_bytes().repeat(2));
            zip.extend((name.len() as u16).to_le_bytes());
            zip.extend([0, 0]);
            zip.extend(name.as_bytes());
            zip.extend(data);
        };
        local(&mut zip);
        let directory = zip.len();
        zip.extend(0x0201_4b50_u32.to_le_bytes());
        zip.extend([20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0x21, 0]);
        zip.extend([0; 4]);
        zip.extend((data.len() as u32).to_le_bytes().repeat(2));
        zip.extend((name.len() as u16).to_le_bytes());
        zip.extend([0; 12]);
        zip.extend((0_u32).to_le_bytes());
        zip.extend(name.as_bytes());
        let size = zip.len() - directory;
        zip.extend(0x0605_4b50_u32.to_le_bytes());
        zip.extend([0; 4]);
        zip.extend([1, 0, 1, 0]);
        zip.extend((size as u32).to_le_bytes());
        zip.extend((directory as u32).to_le_bytes());
        zip.extend([0, 0]);
        zip
    }

    #[test]
    fn a_refused_input_exits_2_and_names_the_entry() {
        let dir = tempfile::tempdir().unwrap();
        let (pack, output) = (temp(&dir, "evil.zip"), temp(&dir, "out.zip"));
        std::fs::write(&pack, zip_named("assets/../evil")).unwrap();
        let mut args = args(&pack, &output);
        args.vanilla.client_jar = Some(client_jar(&dir));
        let error = request(&args)
            .and_then(|request| run::optimise(&request))
            .unwrap_err();
        assert!(error.to_string().contains("assets/../evil"), "{error}");
        assert_eq!(run(&args), ExitClass::Refused);
        assert!(!output.exists());
    }

    #[test]
    fn a_missing_client_jar_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let args = args(&temp(&dir, "absent.zip"), &temp(&dir, "out.zip"));
        assert_eq!(run(&args), ExitClass::Refused);
    }

    #[test]
    fn a_missing_input_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut args = args(&temp(&dir, "absent.zip"), &temp(&dir, "out.zip"));
        args.vanilla.client_jar = Some(client_jar(&dir));
        assert_eq!(run(&args), ExitClass::Refused);
    }

    /// `cache.dir` is resolved once, beside the config file, however the file is named.
    #[test]
    fn a_relative_config_puts_its_cache_beside_it() {
        let dir = tempfile::tempdir_in(".").unwrap();
        let nested = Utf8PathBuf::from_path_buf(dir.path().join("nested")).unwrap();
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(
            nested.join("packprune.toml"),
            "version = 2\n[cache]\ndir = \"cache\"\n",
        )
        .unwrap();
        let relative = nested
            .strip_prefix(std::env::current_dir().unwrap().to_str().unwrap())
            .map_or_else(|_| nested.clone(), Utf8Path::to_owned)
            .join("packprune.toml");
        assert!(relative.is_relative(), "{relative}");
        let mut args = args(&temp(&dir, "in"), &temp(&dir, "out.zip"));
        args.vanilla.client_jar = Some(client_jar(&dir));
        args.config = Some(relative);

        let request = request(&args).unwrap();

        let expected = nested.canonicalize_utf8().unwrap().join("cache");
        assert_eq!(request.cache.unwrap().dir, expected);
    }

    /// A config whose lossy table its pass cannot read exits 2 before anything is read, naming the key.
    #[test]
    fn an_unreadable_lossy_table_exits_2() {
        let dir = tempfile::tempdir().unwrap();
        let config = temp(&dir, "packprune.toml");
        std::fs::write(
            &config,
            "version = 2\n[profiles.default.lossy.overlay-floor]\nglobs = [\"**\"]\n",
        )
        .unwrap();
        let mut args = args(&temp(&dir, "in"), &temp(&dir, "out.zip"));
        args.vanilla.client_jar = Some(client_jar(&dir));
        args.config = Some(config);

        let Err(error) = request(&args) else {
            panic!("refused at load");
        };

        assert!(error.is_refusal());
        assert!(error.to_string().contains("oldest_format"), "{error}");
        assert_eq!(run(&args), ExitClass::Refused);
    }
}
