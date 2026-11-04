//! What a run hands to the report files and the budgets: the analysis of the input, the client's memory for the input
//! and the output, and the check of `[budgets]`.

use std::collections::{BTreeMap, BTreeSet};
use std::io;

use camino::{Utf8Path, Utf8PathBuf};
use packprune_config::Budgets as ConfiguredBudgets;
use packprune_graph::{Reached, UnusedSelector};
use packprune_pack::{ClientRules, Layout, PackInput, PhysPath, RunCx, VanillaIndex};
use packprune_pass::{Analysed, Context};
use packprune_report::budgets::{self, Budgets, Exceeded, Measured};
use packprune_report::cost::{Calculator, Cost, CostError};
use packprune_report::reports::{
    self, Analysis, Bundle, Dropped, Facts, Input, Measure, OptimiserLine, Output, PassLine,
    Passes, RunCost, RunReport, SourceFacts, Verdict as VerdictLine,
};

use super::cache::Cache;
use super::{
    Registry, Request, RunError, Summary, VanillaInputs, Verdict, force_included, guard, inputs,
    limits, own_dirs, timed,
};

/// A file's name and SHA-256, as `report.json` lists an input.
fn hashed(path: &Utf8Path) -> Result<Input, RunError> {
    let sha256 = reports::sha256(path.as_std_path()).map_err(|error| {
        RunError::Io(std::io::Error::new(
            error.kind(),
            format!("{path}: {error}"),
        ))
    })?;
    Ok(Input {
        name: path.file_name().unwrap_or(path.as_str()).to_owned(),
        sha256,
    })
}

/// The analysis of the input pack, as `report.json` and `graph.json` list it.
pub(super) fn analysis(
    input: &Utf8Path,
    inputs: &VanillaInputs,
    state: &Analysed,
    vanilla: &VanillaIndex,
    rules: &ClientRules,
    sources: &[SourceFacts],
) -> Result<Analysis, RunError> {
    let asset_index = inputs.asset_index.as_deref().map(hashed).transpose()?;
    Ok(reports::analysis(&Facts {
        pack: hashed(input)?,
        client_jar: hashed(&inputs.client_jar)?,
        asset_index,
        vanilla,
        rules,
        layout: &state.layout,
        reached: &state.reached,
        sources,
    }))
}

/// The client's memory for `pack` in the view of the client jar's format.
pub(super) fn cost(
    pack: &PackInput,
    layout: &Layout,
    vanilla: &VanillaIndex,
) -> Result<Cost, CostError> {
    Calculator {
        pack,
        layout,
        vanilla,
    }
    .client_view()
}

/// The budgets of the config in this crate's own type.
pub(super) fn budgets_of(configured: &ConfiguredBudgets) -> Budgets {
    Budgets {
        zip_bytes: configured.zip_bytes,
        atlas_gpu_mib: configured.atlas_gpu_mib,
        font_mib: configured.font_mib,
    }
}

/// `[budgets]` checked on `zip_bytes` and the memory `cost` found; the exceeded ones, and notes the report carries.
/// A memory budget the cost cannot be computed for refuses the run: silence would read as within budget.
pub(super) fn check_budgets(
    configured: &ConfiguredBudgets,
    zip_bytes: Option<u64>,
    cost: &Result<Cost, CostError>,
) -> Result<(Vec<Exceeded>, Vec<String>), RunError> {
    let limits = budgets_of(configured);
    let mut notes = Vec::new();
    let mut measured = Measured {
        zip_bytes: zip_bytes.unwrap_or_default(),
        ..Measured::default()
    };
    match cost {
        Ok(cost) => {
            measured.atlas_gpu_bytes = cost.atlas_gpu_bytes();
            measured.font_bytes = cost.font_bytes();
            if limits.needs_cost() && !cost.complete() {
                notes.push(format!(
                    "client memory budgets are checked against a floor, the model leaves out: {}",
                    cost.exclusions.join("; ")
                ));
            }
        }
        Err(error) if limits.needs_cost() => {
            return Err(RunError::Refused(format!(
                "atlas_gpu_mib and font_mib budgets need the client's memory, which cannot be computed: {error}"
            )));
        }
        Err(_) => {}
    }
    if limits.zip_bytes.is_some() && zip_bytes.is_none() {
        notes.push("zip_bytes budget not checked: the input is a directory, not a zip".into());
    }
    let limits = Budgets {
        zip_bytes: limits.zip_bytes.filter(|_| zip_bytes.is_some()),
        ..limits
    };
    Ok((budgets::check(&limits, &measured), notes))
}

/// The memory of the input and the output for the report. A cost the model cannot compute is a note, never a
/// failure, when no budget needs it.
pub(super) fn run_cost(
    input: Result<Cost, CostError>,
    output: Result<Cost, CostError>,
    notes: &mut Vec<String>,
) -> Option<RunCost> {
    let mut keep = |which: &str, cost: Result<Cost, CostError>| match cost {
        Ok(cost) => Some(cost),
        Err(error) => {
            notes.push(format!(
                "client memory of the {which} not computed: {error}"
            ));
            None
        }
    };
    let (input, output) = (keep("input", input), keep("output", output));
    (input.is_some() || output.is_some()).then_some(RunCost { input, output })
}

/// What the first prune left out per reason, from the status the analysis gave each file.
pub(super) fn dropped(first: &BTreeSet<PhysPath>, reached: &Reached) -> Vec<Dropped> {
    let mut by_reason: BTreeMap<&str, (usize, u64)> = BTreeMap::new();
    for path in first {
        let file = &reached.files[path];
        let line = by_reason.entry(file.status.id()).or_default();
        line.0 += 1;
        line.1 += file.bytes;
    }
    by_reason
        .into_iter()
        .map(|(reason, (files, bytes))| Dropped {
            reason: reason.to_owned(),
            files,
            bytes,
        })
        .collect()
}

/// What the passes left nothing reaching, dropped with them.
pub(super) fn reprune_line(repruned: &BTreeMap<PhysPath, u64>) -> Option<Dropped> {
    (!repruned.is_empty()).then(|| Dropped {
        reason: "reprune".into(),
        files: repruned.len(),
        bytes: repruned.values().sum(),
    })
}

fn verdict(verdict: &Verdict) -> VerdictLine {
    VerdictLine {
        views: verdict.views,
        keys: verdict.keys,
        differing_keys: verdict.differing_keys,
        differences: verdict.differences.clone(),
    }
}

impl Summary {
    /// The files `optimise` writes next to its zip: the report of the input, `passes.json` when passes ran, and
    /// `run.json`.
    pub fn bundle(&self) -> Bundle {
        let measures = |records: &[packprune_pass::LossRecord]| -> Vec<Measure> {
            records
                .iter()
                .map(|record| Measure {
                    key: record.key.clone(),
                    metric: record.metric.clone(),
                    value: record.value,
                })
                .collect()
        };
        let passes = self.passes_ran.then(|| Passes {
            passes: self
                .passes
                .iter()
                .map(|pass| PassLine {
                    pass: pass.pass.to_owned(),
                    lossless: pass.loss == packprune_pass::Loss::Lossless,
                    files_before: pass.files_before,
                    files_after: pass.files_after,
                    bytes_before: pass.bytes_before,
                    bytes_after: pass.bytes_after,
                    changed_files: pass.changed.len(),
                    changes: pass.changes.clone(),
                    measured: measures(&pass.measured),
                })
                .collect(),
            reprune: self.reprune.clone(),
            oracle: self.proof.as_ref().map(verdict),
            loss: self.loss.as_ref().map(verdict),
        });
        let run = RunReport {
            output: Some(Output {
                files: self.kept.files,
                bytes: self.kept.bytes,
                zip_bytes: self.written.bytes,
                sha1: self
                    .written
                    .sha1
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect(),
            }),
            dropped: self.dropped.clone(),
            optimisers: self
                .optimisers
                .iter()
                .filter(|optimiser| optimiser.files > 0)
                .map(|optimiser| OptimiserLine {
                    optimiser: optimiser.optimiser.to_owned(),
                    files: optimiser.files,
                    bytes_before: optimiser.bytes_before,
                    bytes_after: optimiser.bytes_after,
                })
                .collect(),
            optimiser_loss: measures(&self.optimiser_loss),
            on_zip: self.on_zip.as_ref().map(verdict),
            zip_loss: self.zip_loss.as_ref().map(verdict),
            cost: self.cost.clone(),
            exceeded: self.exceeded.iter().map(Into::into).collect(),
        };
        Bundle {
            analysis: (*self.analysis).clone(),
            passes,
            run: Some(run),
        }
    }
}

/// Where a run keeps its report files, and what failing to write them means.
#[derive(Debug)]
pub(super) struct ReportsAt {
    dir: Utf8PathBuf,
    /// The pack the drop-in's report must stay out of; none for `optimise`.
    drop_in: Option<Utf8PathBuf>,
}

/// The one folder of the drop-in's reports: each run replaces the files of the last, there is no history.
const LATEST: &str = "latest";

impl ReportsAt {
    /// `optimise`: the report is part of the result, so a run that cannot write it fails before it replaces the zip.
    pub(super) fn required(dir: Utf8PathBuf) -> Self {
        Self { dir, drop_in: None }
    }

    /// The folder this run's reports go under: the drop-in's report root, or `optimise`'s report folder.
    pub(super) fn root(&self) -> &Utf8Path {
        match &self.drop_in {
            Some(_) => self.dir.parent().unwrap_or(&self.dir),
            None => &self.dir,
        }
    }

    /// The drop-in: `root/latest`. The pack is what matters, so a report that cannot be written is a warning.
    pub(super) fn drop_in(root: &Utf8Path, input: &Utf8Path) -> Self {
        Self {
            dir: root.join(LATEST),
            drop_in: Some(input.to_owned()),
        }
    }

    /// The folder refused for the drop-in: a link, or one that is, holds or lies inside the pack.
    fn refused(&self, input: &Utf8Path) -> Result<(), RunError> {
        if std::fs::symlink_metadata(&self.dir).is_ok_and(|metadata| metadata.is_symlink()) {
            return Err(RunError::Refused(format!(
                "report directory {} is a link",
                self.dir
            )));
        }
        guard(input, &self.dir, "report directory")
    }

    /// The report files written beside their names, before the zip is published: every failure that can come of a
    /// directory, a permission or a disk is here, so none is left for after the zip replaced the old one.
    pub(super) fn prepare(
        &self,
        summary: &mut Summary,
    ) -> Result<Option<reports::Staged>, RunError> {
        let staged = match &self.drop_in {
            Some(input) => self.refused(input),
            None => Ok(()),
        }
        .and_then(|()| {
            reports::stage(self.dir.as_std_path(), &summary.bundle()).map_err(|error| {
                RunError::Io(io::Error::new(
                    error.kind(),
                    format!("report {}: {error}", self.dir),
                ))
            })
        });
        match staged {
            Ok(staged) => Ok(Some(staged)),
            Err(error) if self.drop_in.is_some() => {
                summary
                    .warnings
                    .push(format!("report not written: {error}"));
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    /// After the zip is published: the staged files into place. The zip is out and stays, so a failure is a warning.
    pub(super) fn finish(&self, staged: Option<reports::Staged>, summary: &mut Summary) {
        if let Some(Err(error)) = staged.map(reports::Staged::commit) {
            summary
                .warnings
                .push(format!("report not written: {}: {error}", self.dir));
        }
    }
}

/// `<output>.report`, where `optimise` writes the report of its run.
pub fn report_dir(output: &Utf8Path) -> Utf8PathBuf {
    let mut dir = output.to_owned().into_string();
    dir.push_str(".report");
    dir.into()
}

/// What `check` and `report` read: the analysis of the input and its client memory, with the budgets it exceeds.
/// Nothing is written.
#[derive(Debug)]
pub struct Checked {
    pub bundle: Bundle,
    pub exceeded: Vec<Exceeded>,
    /// The analysis's notes, and those on the budgets and the memory model.
    pub warnings: Vec<String>,
}

impl Checked {
    /// Findings make `check` exit 1: broken references or unreadable files in a verified view, and anything
    /// removable.
    pub fn has_findings(&self) -> bool {
        self.bundle.analysis.report.has_findings()
    }
}

/// The input analysed as `optimise` analyses it, its memory measured, `[budgets]` checked on it. The
/// zip budget is the input's size when it is a zip file.
pub fn check(request: &Request) -> Result<Checked, RunError> {
    let threads = request
        .profile
        .threads
        .map_or(0, |threads| threads as usize);
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .map_err(std::io::Error::other)?
        .install(|| analyse(request))
}

fn analyse(request: &Request) -> Result<Checked, RunError> {
    let overrides = request
        .profile
        .overrides()
        .map_err(|error| RunError::Refused(format!("override glob: {error}")))?;
    let limits = limits(&request.limits);
    // `check` writes nothing, the cache included: it reads the entries a run left and adds none.
    let cache = Cache::new(request.cache.as_ref().map(|at| packprune_cache::Store {
        dir: at.dir.clone(),
        max_bytes: at.max_bytes,
    }))
    .read_only();
    let pack = timed("read", || PackInput::open(&request.input, &limits))?;
    let vanilla = timed("vanilla", || {
        inputs::vanilla(&request.vanilla, &limits, &cache)
    })?;
    let rules = ClientRules::for_version(&vanilla.version);
    let run_cx = RunCx::default();
    let (mut sources, source_facts) = timed("sources", || {
        inputs::sources(&request.sources, &run_cx, &cache, &own_dirs(request, None))
    })?;
    let forced = force_included(
        &pack,
        &overrides,
        &vanilla,
        &Registry::default(),
        &request.input,
    )?;
    sources.extend(forced.source);
    let selectors = request
        .prune_unused
        .iter()
        .map(|text| UnusedSelector::parse(text).map_err(|refused| RunError::Refused(refused.0)))
        .collect::<Result<Vec<_>, _>>()?;
    let cx = Context {
        run: &run_cx,
        vanilla: &vanilla,
        rules: &rules,
        sources: &sources,
        selectors: &selectors,
        cache: &cache,
    };
    let analysed = timed("analyse", || cx.analyse(pack))?;
    let analysis = timed("report", || {
        analysis(
            &request.input,
            &request.vanilla,
            &analysed,
            &vanilla,
            &rules,
            &source_facts,
        )
    })?;
    let cost = timed("cost", || cost(&analysed.pack, &analysed.layout, &vanilla));
    let zip_bytes = request
        .input
        .metadata()
        .ok()
        .filter(|metadata| metadata.is_file())
        .map(|metadata| metadata.len());
    let (exceeded, mut warnings) = check_budgets(&request.budgets, zip_bytes, &cost)?;
    warnings.splice(0..0, analysis.report.warnings.iter().cloned());
    let cost = match cost {
        Ok(cost) => Some(RunCost {
            input: Some(cost),
            output: None,
        }),
        Err(error) => {
            warnings.push(format!("client memory not computed: {error}"));
            None
        }
    };
    let run = RunReport {
        cost,
        exceeded: exceeded.iter().map(Into::into).collect(),
        ..RunReport::default()
    };
    Ok(Checked {
        bundle: Bundle {
            analysis,
            passes: None,
            run: Some(run),
        },
        exceeded,
        warnings,
    })
}

#[cfg(test)]
mod tests;
