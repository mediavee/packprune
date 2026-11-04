//! `packprune self-test`: the graph over the client's own assets; exit 1 when a vanilla model or
//! texture is not reached, a code-loaded pattern matches nothing, or a pack copy of a vanilla blockstate is no root.

use std::panic::{AssertUnwindSafe, catch_unwind};

use packprune_engine::run::{self, RunError};

use super::optimise::{VanillaArgs, load};
use crate::ExitClass;

#[derive(Debug, clap::Args)]
pub struct Args {
    /// packprune.toml, for `[vanilla]`.
    #[arg(long)]
    pub config: Option<camino::Utf8PathBuf>,
    #[command(flatten)]
    pub vanilla: VanillaArgs,
}

pub fn run(args: &Args) -> ExitClass {
    let result = catch_unwind(AssertUnwindSafe(|| -> Result<_, RunError> {
        let loaded = load(args.config.as_deref())?;
        run::self_test(&loaded.vanilla(&args.vanilla)?.client_jar)
    }));
    match result {
        Ok(Ok(test)) => {
            println!(
                "{} models, {} textures; {} files rooted as code-loaded; {} unexplained, {} stale code-loaded patterns; \
                 {} vanilla blockstates, {} a pack copy of would not root",
                test.models,
                test.textures,
                test.code_loaded,
                test.unexplained.len(),
                test.stale.len(),
                test.blockstates,
                test.unrooted.len()
            );
            test.unexplained
                .iter()
                .for_each(|path| println!("UNREACHED {path}"));
            test.stale
                .iter()
                .for_each(|glob| println!("STALE     {glob}"));
            test.unrooted
                .iter()
                .for_each(|name| println!("UNROOTED  {name}"));
            if test.passed() {
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
