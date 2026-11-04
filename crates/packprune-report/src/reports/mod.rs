//! The report files: `report.json` and `graph.json` are the analysis of the input, `passes.json` the passes' gains
//! and proofs, `run.json` the output, the dropped bytes per reason, the file optimisers, the oracle on the written
//! zip, client memory and budgets; `report.md` renders them all as text.

mod build;
mod hash;
mod json;
mod markdown;
mod model;
mod run;

use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use tempfile::TempPath;

pub use build::{Analysis, Facts, SourceFacts, analysis};
pub use hash::sha256;
pub use model::*;
pub use run::{
    BudgetLine, CostLine, Dropped, Figures, Measure, OptimiserLine, Output, PassLine, Passes,
    RunCost, RunReport, Verdict,
};

impl Report {
    /// The findings: a broken reference or an unreadable file in a verified view, or anything removable. They set
    /// the exit code of `report` and `check`.
    pub fn has_findings(&self) -> bool {
        let s = &self.summary;
        s.missing > 0
            || s.not_stitched > 0
            || s.invalid > 0
            || s.orphans > 0
            || s.unused > 0
            || s.dead_blockstates > 0
    }

    /// The summary line `report` prints.
    pub fn headline(&self) -> String {
        let s = &self.summary;
        format!(
            "{} files, {} orphans, {} atlas-loaded orphans, {} files of unused items, {} dead blockstates, {} missing \
             references, {} sprites not stitched (other formats, unverified: {} missing, {} not stitched), {} unknown, \
             {} unreadable, {} usages matching nothing",
            s.files,
            s.orphans,
            s.atlas_loaded_orphans,
            s.unused,
            s.dead_blockstates,
            s.missing,
            s.not_stitched,
            s.other_formats_missing,
            s.other_formats_not_stitched,
            s.unknown,
            s.invalid,
            s.unmatched_usages
        )
    }
}

/// Everything a command writes into a report directory.
#[derive(Clone, Debug, PartialEq)]
pub struct Bundle {
    pub analysis: Analysis,
    /// Only when passes ran.
    pub passes: Option<Passes>,
    /// Only when the command measured more than the analysis.
    pub run: Option<RunReport>,
}

#[derive(Debug, thiserror::Error)]
pub enum ReportError {
    #[error("{path}: {why}")]
    Unreadable { path: String, why: String },
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// The files of `bundle` in `dir`, which is created. The same five files at each call: what a bundle has nothing
/// of (the passes of a run without any) is written empty, so an earlier run's file is replaced and nothing is ever
/// removed.
pub fn write(dir: &Path, bundle: &Bundle) -> io::Result<()> {
    stage(dir, bundle)?.commit()
}

/// The files of a bundle written beside their final names: everything that can fail for want of a directory, a
/// permission or a disk has happened, and [`Staged::commit`] only renames. Dropped without a commit, the staged
/// files are removed.
#[derive(Debug)]
pub struct Staged {
    dir: PathBuf,
    files: Vec<(&'static str, TempPath)>,
}

/// Creates `dir` and writes the files of `bundle` into it, rendered in full, each under a name no one has: a
/// link already at a file's name is replaced by the rename, never opened.
pub fn stage(dir: &Path, bundle: &Bundle) -> io::Result<Staged> {
    std::fs::create_dir_all(dir)?;
    let passes = bundle.passes.clone().unwrap_or_default();
    let run = bundle.run.clone().unwrap_or_default();
    let contents = [
        ("report.json", json::to_string(&bundle.analysis.report)),
        (
            "report.md",
            markdown_of(&bundle.analysis.report, Some(&run), Some(&passes)),
        ),
        ("graph.json", json::to_string(&bundle.analysis.graph)),
        ("passes.json", json::to_string(&passes)),
        ("run.json", json::to_string(&run)),
    ];
    let mut files = Vec::new();
    for (name, content) in contents {
        files.push((name, temporary(dir, name, &content)?));
    }
    Ok(Staged {
        dir: dir.to_owned(),
        files,
    })
}

/// `content` in a file created exclusively in `dir`, readable like a file written in place.
fn temporary(dir: &Path, name: &str, content: &str) -> io::Result<TempPath> {
    let mut file = tempfile::Builder::new()
        .prefix(&format!(".{name}."))
        .suffix(".partial")
        .tempfile_in(dir)?;
    file.write_all(content.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o644))?;
    }
    Ok(file.into_temp_path())
}

/// `content` as the file `path`, whole or not at all: written beside it and renamed over it. A link at `path` is
/// replaced, its target never opened.
pub fn replace(path: &Path, content: &str) -> io::Result<()> {
    let (Some(dir), Some(name)) = (
        path.parent(),
        path.file_name().and_then(|name| name.to_str()),
    ) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{}: not a file path", path.display()),
        ));
    };
    temporary(dir, name, content)?
        .persist(path)
        .map_err(|error| error.error)
}

impl Staged {
    /// Renames each file over the old one, so a reader never sees half a report.
    pub fn commit(self) -> io::Result<()> {
        for (name, staged) in self.files {
            staged
                .persist(self.dir.join(name))
                .map_err(|error| error.error)?;
        }
        Ok(())
    }
}

/// `report.md`: the analysis, then what `run.json` and `passes.json` hold.
fn markdown_of<C: Figures>(
    report: &Report,
    run: Option<&RunReport<C>>,
    passes: Option<&Passes>,
) -> String {
    let mut markdown = markdown::render(report);
    if let Some(run) = run {
        markdown.push_str(&run_section(run, passes));
    }
    markdown
}

/// `report.md` rendered from the files of `dir` that `write` made, the same text: `report.json` and, when they are
/// there, `run.json` and `passes.json`. Refuses anything that is not a report of this schema.
pub fn render(dir: &Path) -> Result<String, ReportError> {
    let report: Report = read(dir, "report.json")?
        .ok_or_else(|| unreadable(&dir.join("report.json"), "no such file".into()))?;
    let run: Option<RunReport<CostLine>> = read(dir, "run.json")?;
    let passes: Option<Passes> = read(dir, "passes.json")?;
    Ok(markdown_of(&report, run.as_ref(), passes.as_ref()))
}

/// The `report.json` at `path`, refused when it is not a packprune report of this schema.
pub fn read_report(path: &Path) -> Result<Report, ReportError> {
    let bytes = std::fs::read(path).map_err(|error| {
        unreadable(
            path,
            match error.kind() {
                io::ErrorKind::NotFound => "no such file".into(),
                _ => error.to_string(),
            },
        )
    })?;
    parse(path, &bytes, true)
}

fn unreadable(path: &Path, why: String) -> ReportError {
    ReportError::Unreadable {
        path: path.display().to_string(),
        why,
    }
}

/// The file `name` of `dir` parsed, `None` when it is absent. `report.json` must be of this schema.
fn read<T: serde::de::DeserializeOwned>(dir: &Path, name: &str) -> Result<Option<T>, ReportError> {
    let path = dir.join(name);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(ReportError::Io(error)),
    };
    parse(&path, &bytes, name == "report.json").map(Some)
}

fn parse<T: serde::de::DeserializeOwned>(
    path: &Path,
    bytes: &[u8],
    is_report: bool,
) -> Result<T, ReportError> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| unreadable(path, format!("not JSON: {error}")))?;
    if is_report {
        match value.get("schema").and_then(serde_json::Value::as_u64) {
            Some(schema) if schema == u64::from(SCHEMA) => {}
            Some(schema) => {
                return Err(unreadable(
                    path,
                    format!("not a packprune report of schema {SCHEMA} (schema {schema})"),
                ));
            }
            None => {
                return Err(unreadable(
                    path,
                    format!("not a packprune report of schema {SCHEMA} (no schema field)"),
                ));
            }
        }
    }
    serde_json::from_value(value)
        .map_err(|error| unreadable(path, format!("not a packprune report ({error})")))
}

const MIB: f64 = 1_048_576.0;

fn run_section<C: Figures>(run: &RunReport<C>, passes: Option<&Passes>) -> String {
    let mut md = String::new();
    if let Some(cost) = &run.cost {
        md.push_str("\n## Client memory\n\n");
        md.push_str("In the view of the client jar's format; a total is a floor while its model is incomplete.\n\n");
        md.push_str("| | Atlas GPU (MiB) | Fonts (MiB) | Model |\n|---|---:|---:|---|\n");
        for (label, cost) in [("Input", &cost.input), ("Output", &cost.output)] {
            if let Some(cost) = cost {
                let _ = writeln!(
                    md,
                    "| {label} | {:.1} | {:.1} | {} |",
                    cost.atlas_gpu_bytes() as f64 / MIB,
                    cost.font_bytes() as f64 / MIB,
                    completeness(cost)
                );
            }
        }
    }
    if let Some(output) = &run.output {
        let _ = write!(
            md,
            "\n## Run\n\nWritten pack: {} files, {} B, zip {} B, sha1 {}.\n",
            output.files, output.bytes, output.zip_bytes, output.sha1
        );
    }
    if !run.dropped.is_empty() {
        md.push_str("\n### Dropped\n\n| Reason | Files | Bytes |\n|---|---:|---:|\n");
        for dropped in &run.dropped {
            let _ = writeln!(
                md,
                "| {} | {} | {} |",
                dropped.reason, dropped.files, dropped.bytes
            );
        }
    }
    if let Some(passes) = passes.filter(|passes| !passes.is_empty()) {
        md.push_str("\n### Passes\n\n");
        for pass in &passes.passes {
            let _ = writeln!(
                md,
                "- `{}` ({}): {} files changed, {:+} B raw, {} -> {} files; {}",
                pass.pass,
                if pass.lossless { "lossless" } else { "lossy" },
                pass.changed_files,
                pass.bytes_after as i64 - pass.bytes_before as i64,
                pass.files_before,
                pass.files_after,
                pass.changes.join("; ")
            );
        }
        verdict_line(&mut md, "oracle after the lossless passes", &passes.oracle);
        verdict_line(&mut md, "loss of the lossy passes", &passes.loss);
    }
    if !run.optimisers.is_empty() {
        md.push_str("\n### File optimisers\n\n");
        for optimiser in &run.optimisers {
            let _ = writeln!(
                md,
                "- `{}`: {} files, {} -> {} B",
                optimiser.optimiser, optimiser.files, optimiser.bytes_before, optimiser.bytes_after
            );
        }
        for measure in &run.optimiser_loss {
            let _ = writeln!(
                md,
                "- loss `{}`: {} {}",
                measure.key, measure.metric, measure.value
            );
        }
        verdict_line(&mut md, "oracle on the zip as written", &run.on_zip);
        verdict_line(&mut md, "loss of the lossy file optimisers", &run.zip_loss);
    }
    if !run.exceeded.is_empty() {
        md.push_str("\n### Budgets exceeded\n\n");
        for budget in &run.exceeded {
            let _ = writeln!(
                md,
                "- `{}`: {} > {}",
                budget.budget, budget.measured, budget.limit
            );
        }
    }
    md
}

fn completeness(cost: &impl Figures) -> String {
    if cost.exclusions().is_empty() {
        "complete".into()
    } else {
        format!("floor: {}", cost.exclusions().join("; "))
    }
}

fn verdict_line(md: &mut String, what: &str, verdict: &Option<Verdict>) {
    if let Some(verdict) = verdict {
        let _ = writeln!(
            md,
            "- {what}: {} views, {} keys, {} differ",
            verdict.views, verdict.keys, verdict.differing_keys
        );
        for difference in &verdict.differences {
            let _ = writeln!(md, "  - {difference}");
        }
    }
}

#[cfg(test)]
mod tests;
