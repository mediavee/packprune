//! `packprune check`: analyse a pack as `optimise` would and say what is wrong with it, writing nothing. Exit 1 on
//! findings (missing references, sprites no atlas stitches, unreadable files, anything removable) or on a budget the
//! pack exceeds.

use std::panic::{AssertUnwindSafe, catch_unwind};

use camino::Utf8PathBuf;
use packprune_engine::run::{self, Checked, Request, RunError};

use super::optimise::{self, VanillaArgs};
use crate::ExitClass;

/// Findings listed per kind before "and N more".
const LISTED: usize = 20;

#[derive(Debug, clap::Args)]
pub struct Args {
    #[command(flatten)]
    pub input: Input,
}

/// What `check` and `report` analyse: the pack, the client it is read against and the usage sources, as `optimise`
/// takes them.
#[derive(Debug, clap::Args)]
pub struct Input {
    /// Pack directory or zip.
    pub pack: Utf8PathBuf,
    /// packprune.toml.
    #[arg(long)]
    pub config: Option<Utf8PathBuf>,
    #[arg(long)]
    pub profile: Option<String>,
    /// Worker threads; the machine's cores by default.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    pub threads: Option<u32>,
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

impl Input {
    /// The request `optimise` would build for this pack; the output is never used.
    fn request(&self) -> Result<Request, RunError> {
        optimise::request(&optimise::Args {
            pack: self.pack.clone(),
            output: Utf8PathBuf::new(),
            passes: None,
            config: self.config.clone(),
            profile: self.profile.clone(),
            threads: self.threads,
            expect_report: None,
            vanilla: VanillaArgs {
                client_jar: self.vanilla.client_jar.clone(),
                asset_index: self.vanilla.asset_index.clone(),
            },
            sources: self.sources.clone(),
            keep: self.keep.clone(),
            prune_unused: self.prune_unused.clone(),
        })
    }

    pub(crate) fn analyse(&self) -> Result<Checked, ExitClass> {
        guarded(|| run::check(&self.request()?))
    }
}

/// `work` with its error printed as `optimise` prints it and mapped to its exit class; a panic is a bug of the tool
/// (the panic hook printed where).
pub(crate) fn guarded<T>(work: impl FnOnce() -> Result<T, RunError>) -> Result<T, ExitClass> {
    match catch_unwind(AssertUnwindSafe(work)) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => {
            let refused = error.is_refusal();
            eprintln!("{}: {error}", if refused { "refused" } else { "error" });
            Err(if refused {
                ExitClass::Refused
            } else {
                ExitClass::Error
            })
        }
        Err(_) => Err(ExitClass::Error),
    }
}

pub fn run(args: &Args) -> ExitClass {
    let checked = match args.input.analyse() {
        Ok(checked) => checked,
        Err(class) => return class,
    };
    print(&checked);
    exit_class(&checked)
}

/// 1: findings or an exceeded budget.
pub(crate) fn exit_class(checked: &Checked) -> ExitClass {
    if checked.has_findings() || !checked.exceeded.is_empty() {
        ExitClass::Findings
    } else {
        ExitClass::Clean
    }
}

fn print(checked: &Checked) {
    for warning in &checked.warnings {
        eprintln!("warning: {warning}");
    }
    let report = &checked.bundle.analysis.report;
    println!("{}", report.headline());
    print_memory(checked);
    let kinds: [(&str, Vec<String>); 5] = [
        (
            "missing reference",
            report
                .missing
                .iter()
                .map(|m| format!("`{}` ({}) from `{}`", m.target, m.reason, m.source))
                .collect(),
        ),
        (
            "sprite not stitched",
            report
                .not_stitched
                .iter()
                .map(|n| format!("`{}` (file `{}`) from `{}`", n.sprite, n.file, n.source))
                .collect(),
        ),
        (
            "orphan",
            report
                .orphans
                .iter()
                .map(|f| format!("{} ({}, {} B)", f.path, f.kind, f.bytes))
                .collect(),
        ),
        (
            "dead blockstate",
            report
                .dead_blockstates
                .iter()
                .map(|f| f.path.clone())
                .collect(),
        ),
        (
            "unreadable",
            report
                .invalid
                .iter()
                .map(|i| format!("{}: {}", i.path, i.error))
                .collect(),
        ),
    ];
    for (kind, lines) in kinds {
        for line in lines.iter().take(LISTED) {
            println!("{kind}: {line}");
        }
        if lines.len() > LISTED {
            println!("{kind}: ... and {} more", lines.len() - LISTED);
        }
    }
    for exceeded in &checked.exceeded {
        println!(
            "budget {} exceeded: {} > {}",
            exceeded.budget, exceeded.measured, exceeded.limit
        );
    }
}

fn print_memory(checked: &Checked) {
    let cost = checked
        .bundle
        .run
        .as_ref()
        .and_then(|run| run.cost.as_ref())
        .and_then(|cost| cost.input.as_ref());
    if let Some(cost) = cost {
        println!(
            "client memory: atlas GPU {:.1} MiB, fonts {:.1} MiB{}",
            cost.atlas_gpu_bytes() as f64 / 1_048_576.0,
            cost.font_bytes() as f64 / 1_048_576.0,
            if cost.complete() {
                ""
            } else {
                " (a floor: the model leaves parts out)"
            }
        );
    }
}
