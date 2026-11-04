//! `packprune report`: the `report.json`, `report.md` and `graph.json` of a pack, and its client memory. Given a
//! report directory instead of a pack, it renders that report's `report.md` again.

use std::io;

use camino::{Utf8Path, Utf8PathBuf};
use packprune_engine::run::{self, reports};

use super::check::{self, Input};
use crate::ExitClass;

#[derive(Debug, clap::Args)]
pub struct Args {
    #[command(flatten)]
    pub input: Input,
    /// Report directory, outside the pack.
    #[arg(long)]
    pub out: Utf8PathBuf,
}

pub fn run(args: &Args) -> ExitClass {
    let pack = &args.input.pack;
    if is_report_dir(pack) {
        return render(pack, &args.out);
    }
    if let Err(why) = guard_out(pack, &args.out) {
        eprintln!("refused: --out {}: {why}", args.out);
        return ExitClass::Refused;
    }
    let checked = match args.input.analyse() {
        Ok(checked) => checked,
        Err(class) => return class,
    };
    if let Err(error) = reports::write(args.out.as_std_path(), &checked.bundle) {
        eprintln!("error: report {}: {error}", args.out);
        return ExitClass::Error;
    }
    for warning in &checked.warnings {
        eprintln!("warning: {warning}");
    }
    for exceeded in &checked.exceeded {
        println!(
            "budget {} exceeded: {} > {}",
            exceeded.budget, exceeded.measured, exceeded.limit
        );
    }
    println!(
        "{}; report in {}",
        checked.bundle.analysis.report.headline(),
        args.out
    );
    check::exit_class(&checked)
}

/// A directory of a report, not of a pack: a pack that kept a `report.json` at its root has a `pack.mcmeta` too.
fn is_report_dir(path: &Utf8Path) -> bool {
    path.join("report.json").is_file() && !path.join("pack.mcmeta").exists()
}

/// `report.md` of `dir`'s `report.json`, written to `out`.
fn render(dir: &Utf8Path, out: &Utf8Path) -> ExitClass {
    let result = reports::render(dir.as_std_path()).map_err(|error| error.to_string());
    let markdown = match result {
        Ok(markdown) => markdown,
        Err(why) => {
            eprintln!("refused: {why}");
            return ExitClass::Refused;
        }
    };
    let written = std::fs::create_dir_all(out)
        .and_then(|()| reports::replace(out.join("report.md").as_std_path(), &markdown));
    match written {
        Ok(()) => {
            println!("rendered {dir}/report.json to {out}/report.md");
            ExitClass::Clean
        }
        Err(error) => {
            eprintln!("error: {out}: {error}");
            ExitClass::Error
        }
    }
}

/// The report directory is never the pack, inside it or around it, where a later run would read
/// it as pack files; and it is a directory or absent.
fn guard_out(pack: &Utf8Path, out: &Utf8Path) -> Result<(), String> {
    let pack = pack
        .canonicalize_utf8()
        .map_err(|error| format!("{pack}: {error}"))?;
    // The destination itself is followed too: a link to the pack is the pack, and the report is written into it.
    let real = run::resolve(out).map_err(|error| format!("{out}: {error}"))?;
    if real == pack {
        return Err(format!("is the pack {pack}"));
    }
    if real.starts_with(&pack) {
        return Err(format!("is inside the pack {pack}"));
    }
    if pack.starts_with(&real) {
        return Err(format!("contains the pack {pack}"));
    }
    match std::fs::metadata(out) {
        Ok(metadata) if !metadata.is_dir() => Err("is a file, expected a directory".into()),
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error.to_string()),
        _ => Ok(()),
    }
}
