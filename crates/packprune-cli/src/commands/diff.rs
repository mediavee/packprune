//! `packprune diff`: what changes for the player between two packs, view by view; exit 1 with the
//! located differences.

use std::panic::{AssertUnwindSafe, catch_unwind};

use packprune_engine::run::{self, DiffRequest, RunError};

use super::optimise::{VanillaArgs, load};
use crate::ExitClass;

/// The differences printed before counting the rest.
const LISTED: usize = 200;

#[derive(Debug, clap::Args)]
pub struct Args {
    /// The reference pack, directory or zip.
    pub before: camino::Utf8PathBuf,
    /// The pack to check against it, directory or zip.
    pub after: camino::Utf8PathBuf,
    #[command(flatten)]
    pub vanilla: VanillaArgs,
    /// packprune.toml, for `[vanilla]` and `[input.limits]`.
    #[arg(long)]
    pub config: Option<camino::Utf8PathBuf>,
}

pub fn run(args: &Args) -> ExitClass {
    let result = catch_unwind(AssertUnwindSafe(|| -> Result<_, RunError> {
        let loaded = load(args.config.as_deref())?;
        run::diff(&DiffRequest {
            before: args.before.clone(),
            after: args.after.clone(),
            vanilla: loaded.vanilla(&args.vanilla)?,
            limits: loaded
                .config
                .map(|config| config.input.limits)
                .unwrap_or_default(),
        })
    }));
    match result {
        Ok(Ok(diff)) => {
            let verdict = diff.verdict;
            for difference in verdict.differences.iter().take(LISTED) {
                println!("DIFFERS {difference}");
            }
            if verdict.differences.len() > LISTED {
                println!("... and {} more", verdict.differences.len() - LISTED);
            }
            println!(
                "{} views, {} keys compared, {} differ, in {} ms",
                verdict.views, verdict.keys, verdict.differing_keys, verdict.millis
            );
            if verdict.differing_keys == 0 {
                ExitClass::Clean
            } else {
                ExitClass::Findings
            }
        }
        Ok(Err(error)) if error.is_refusal() => {
            eprintln!("refused: {error}");
            ExitClass::Refused
        }
        Ok(Err(error)) => {
            eprintln!("error: {error}");
            ExitClass::Error
        }
        Err(_) => ExitClass::Error,
    }
}
