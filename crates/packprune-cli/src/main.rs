//! The `packprune` binary: each command's arguments and body live in its module.

mod commands;
mod logging;
mod squash;

use std::ffi::OsString;
use std::process::ExitCode;

use camino::Utf8Path;
use clap::{CommandFactory, Parser, Subcommand};

/// The process exit codes: 0 clean, 1 findings, 2 refused input, 3 error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitClass {
    Clean = 0,
    Findings = 1,
    Refused = 2,
    Error = 3,
}

/// Maps a library error to its exit class.
pub trait Classify {
    fn exit_class(&self) -> ExitClass;
}

#[derive(Debug, Parser)]
#[command(
    name = "packprune",
    version,
    about = "Minecraft resource pack optimiser"
)]
struct Cli {
    /// More log output (repeat for more).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
    /// Only errors on stderr.
    #[arg(short, long, global = true)]
    quiet: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Optimise a pack into a zip.
    #[command(alias = "optimize")]
    Optimise(commands::optimise::Args),
    /// Lint a pack: missing references, orphans, broken models, budgets.
    Check(commands::check::Args),
    /// Write a pack's report files: report.json, report.md and graph.json.
    Report(commands::report::Args),
    /// Show what changes for the player between two packs.
    Diff(commands::diff::Args),
    /// Check the analysis against the client jar's own assets.
    SelfTest(commands::self_test::Args),
    /// Run as a PackSquash drop-in on a PackSquash settings file.
    Squash(squash::Args),
    /// Print the JSON schema of packprune.toml.
    Schema(commands::schema::Args),
}

// The release binary is static musl, whose allocator is slow under this allocation-heavy, multi-threaded workload.
#[cfg(target_env = "musl")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().collect();
    let class = match drop_in_settings(&args) {
        Some(settings) => {
            logging::init_drop_in();
            squash::run(&squash::Args {
                settings: settings.to_owned(),
            })
        }
        None => {
            let cli = Cli::parse_from(args);
            logging::init(cli.verbose, cli.quiet);
            match cli.command {
                Command::Optimise(args) => commands::optimise::run(&args),
                Command::Check(args) => commands::check::run(&args),
                Command::Report(args) => commands::report::run(&args),
                Command::Diff(args) => commands::diff::run(&args),
                Command::SelfTest(args) => commands::self_test::run(&args),
                Command::Squash(args) => squash::run(&args),
                Command::Schema(args) => commands::schema::run(&args),
            }
        }
    };
    ExitCode::from(class as u8)
}

/// `packprune <settings.toml>`: one argument, an existing file, not a subcommand name, as Nexo calls PackSquash.
fn drop_in_settings(args: &[OsString]) -> Option<&Utf8Path> {
    let [_, only] = args else { return None };
    let path = Utf8Path::new(only.to_str()?);
    let subcommand = Cli::command().find_subcommand(path.as_str()).is_some();
    (!subcommand && path.is_file()).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn every_subcommand_parses() {
        for argv in [
            &["packprune", "optimise", "pack", "-o", "out.zip"][..],
            &["packprune", "optimize", "pack", "-o", "out.zip"],
            &["packprune", "check", "pack"],
            &["packprune", "report", "pack", "--out", "dir"],
            &["packprune", "diff", "a", "b"],
            &["packprune", "self-test"],
            &["packprune", "squash", "settings.toml"],
            &["packprune", "schema"],
        ] {
            Cli::try_parse_from(argv).unwrap_or_else(|e| panic!("{argv:?}: {e}"));
        }
    }
}
