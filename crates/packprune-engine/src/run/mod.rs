//! The `optimise` run: read, analyse with the usage sources, prune; the lossless passes, a re-prune and the strict
//! oracle; the lossy passes, a re-prune and the oracle as measured loss; the file optimisers through
//! the content cache; the zip written beside the output, read back, self-checked and compared by the oracle with what
//! the file optimisers were given, then published. A failed step writes nothing.

mod cache;
mod encode;
mod expect;
mod inputs;
mod lossy;
mod optimisers;
mod options;
mod origins;
mod report;
mod squash;
mod tools;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::time::Instant;

use camino::{Utf8Component, Utf8Path, Utf8PathBuf};
use packprune_cache::Store;
use packprune_config::Overrides;
use packprune_config::{Budgets, InputLimits, Profile};
use packprune_graph::{
    IdGlob, Reached, ReferenceIndex, SourceUsages, UnusedSelector, Usage, UsageKind,
};
use packprune_oracle::memo::Memo;
use packprune_oracle::{
    Comparison, OracleInput, Remap, compare_memoised, compare_with, view_labels,
};
pub use packprune_pack::Quirks;
use packprune_pack::meta::PackMeta;
use packprune_pack::{
    ClientRules, Digest, FileKind, InputError, Layout, Limits, LogicalPath, PackInput, PhysPath,
    RunCx, VanillaIndex, ViewSet,
};
use packprune_pass::{Analysed, Context, Kept, order};
pub use packprune_pass::{Loss, LossRecord, PassReport};
use packprune_report::budgets::Exceeded;
pub use packprune_report::reports;
use packprune_report::reports::{Analysis, Dropped, RunCost};
use packprune_zip::Written;

use cache::Cache;
pub use expect::Expected;
pub use inputs::{SourceRequest, VanillaInputs};
pub use optimisers::OptimiserReport;
pub use options::Registry;
use origins::Origins;
use report::ReportsAt;
pub use report::{Checked, check, report_dir};
pub use squash::{Artefact, DropIn, Squashed, squash};
use squash::{Progress, Step};
pub use tools::{Diff, DiffRequest, SelfTest, diff, self_test};

/// The located differences an error or a verdict lists.
const LISTED: usize = 20;

/// One `optimise` run.
#[derive(Clone, Debug)]
pub struct Request {
    /// Pack directory or zip.
    pub input: Utf8PathBuf,
    /// The zip to write; replaced only once the run passed.
    pub output: Utf8PathBuf,
    pub vanilla: VanillaInputs,
    pub sources: Vec<SourceRequest>,
    /// `<source>:<location glob>`: declared items that are roots only when a source uses them.
    pub prune_unused: Vec<String>,
    /// The resolved profile, the command line's overrides applied.
    pub profile: Profile,
    pub limits: InputLimits,
    pub budgets: Budgets,
    pub cache: Option<CacheAt>,
    /// The report an operator reviewed: the run is refused, before it prunes or writes anything, unless its own
    /// analysis equals it, and it goes on with that same analysis. `check` ignores it.
    pub expect: Option<Expected>,
}

/// Where the content cache lives and the size eviction brings it down to.
#[derive(Clone, Debug)]
pub struct CacheAt {
    pub dir: Utf8PathBuf,
    pub max_bytes: u64,
}

/// `cache.max_bytes` when the config does not set it.
pub const DEFAULT_CACHE_BYTES: u64 = 1 << 30;

impl CacheAt {
    /// `PACKPRUNE_CACHE` when set, else `cache.dir` as the config resolved it, else `.packprune-cache` beside the
    /// config file. Without a config or the variable, a run has no cache.
    pub fn resolve(
        config_dir: Option<&Utf8Path>,
        configured: Option<&Utf8Path>,
        max_bytes: Option<u64>,
    ) -> Result<Option<Self>, RunError> {
        let overridden = packprune_cache::env_override().map_err(|error| match error {
            packprune_cache::CacheError::Io(error) => RunError::Io(error),
        })?;
        let dir = match (overridden, configured, config_dir) {
            (Some(dir), ..) => dir,
            (None, Some(dir), _) => dir.to_owned(),
            (None, None, Some(base)) => base.join(".packprune-cache"),
            (None, None, None) => return Ok(None),
        };
        Ok(Some(Self {
            dir,
            max_bytes: max_bytes.unwrap_or(DEFAULT_CACHE_BYTES),
        }))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// Exit 2: an input, the config, a source or a request refused by name.
    #[error("{0}")]
    Refused(String),
    /// Exit 3: the lossless passes, or the lossless file optimisers and the writer, changed what the player sees;
    /// nothing was written.
    #[error("the oracle found {} keys that differ after {}:\n{}", .verdict.differing_keys, .passes.join(", "), bullets(&.verdict.differences))]
    NotEquivalent {
        passes: Vec<&'static str>,
        verdict: Verdict,
    },
    /// Exit 3: the written pack shows something the input did not; nothing was published.
    #[error("self-check of the output failed, nothing was written:\n{}", bullets(.0))]
    SelfCheck(Vec<String>),
    /// Exit 3: a pass failed, or the framework refused its edits.
    #[error(transparent)]
    Pass(Box<packprune_pass::RunError>),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl RunError {
    /// True for exit 2, false for exit 3.
    pub fn is_refusal(&self) -> bool {
        matches!(self, Self::Refused(_))
    }
}

impl From<packprune_pass::RunError> for RunError {
    fn from(error: packprune_pass::RunError) -> Self {
        match error {
            packprune_pass::RunError::Meta(error) => Self::Refused(error.to_string()),
            packprune_pass::RunError::Analysis(refused) => Self::Refused(refused.0),
            error => Self::Pass(Box::new(error)),
        }
    }
}

impl From<InputError> for RunError {
    fn from(error: InputError) -> Self {
        match error {
            InputError::Refused { entry, why } => Self::Refused(format!("{entry}: {why}")),
            InputError::Io(error) => Self::Io(error),
        }
    }
}

fn bullets(lines: &[String]) -> String {
    let mut shown: Vec<String> = lines
        .iter()
        .take(LISTED)
        .map(|line| format!("  - {line}"))
        .collect();
    if lines.len() > LISTED {
        shown.push(format!("  ... and {} more", lines.len() - LISTED));
    }
    shown.join("\n")
}

/// One oracle comparison: its counts, its located differences, and its wall time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub views: u32,
    pub keys: u32,
    pub differing_keys: u32,
    pub differences: Vec<String>,
    pub millis: u128,
}

/// Files and their bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Count {
    pub files: usize,
    pub bytes: u64,
}

/// What a run did, for the command to print.
#[derive(Clone, Debug)]
pub struct Summary {
    pub output: Utf8PathBuf,
    /// The written pack.
    pub kept: Count,
    pub written: Written,
    /// What the first prune left out.
    pub pruned: Count,
    /// What the re-prunes after the passes left out.
    pub repruned: Count,
    /// True when at least one pass was enabled: the passes' lines are printed then.
    pub passes_ran: bool,
    pub passes: Vec<PassReport>,
    /// The strict oracle after the lossless passes; `None` when they changed nothing.
    pub proof: Option<Verdict>,
    /// The oracle after the lossy passes, their measured loss; `None` when they changed nothing.
    pub loss: Option<Verdict>,
    pub optimisers: Vec<OptimiserReport>,
    /// What lossy file optimisers measured.
    pub optimiser_loss: Vec<LossRecord>,
    /// The strict oracle between the pack the file optimisers were given and what the lossless ones made of it, the
    /// zip as written when no lossy optimiser changed a file; `None` when they changed no byte.
    pub on_zip: Option<Verdict>,
    /// The oracle across the lossy file optimisers, their measured loss; `None` when they changed nothing.
    pub zip_loss: Option<Verdict>,
    pub cache_hits: u64,
    pub cache_misses: u64,
    /// Key digests the oracle reused from its cross-run memo, and those it resolved.
    pub oracle_memo: (u64, u64),
    /// The output's findings the input has too, in verified views.
    pub missing: usize,
    pub not_stitched: usize,
    pub warnings: Vec<String>,
    pub exceeded: Vec<Exceeded>,
    /// The analysis of the input, as `report.json` and `graph.json` list it.
    pub analysis: Box<Analysis>,
    /// What the first prune and the re-prunes left out, per reason.
    pub dropped: Vec<Dropped>,
    /// The paths the re-prunes left out.
    pub reprune: Vec<String>,
    /// The client's memory for the input and the output; `None` when the model cannot size the pack.
    pub cost: Option<RunCost>,
}

/// The re-prune after the passes; a seam so a test can plant a wrong one for the oracle to catch.
type Reprune =
    fn(&Context<'_>, Analysed) -> Result<(Analysed, BTreeSet<PhysPath>), packprune_pass::RunError>;

/// The run, its report files in `<output>.report` put in place with the zip.
pub fn optimise(request: &Request) -> Result<Summary, RunError> {
    let seams = Seams {
        registry: &Registry::default(),
        prune: |cx, state| cx.prune(state),
        reprune: |cx, state| cx.prune(state),
        quirks: Quirks::default(),
        reports: Some(ReportsAt::required(report_dir(&request.output))),
    };
    execute_staged(request, &seams, &Progress::default())
}

/// A run with no report files.
#[cfg(test)]
fn execute(request: &Request, registry: &Registry, reprune: Reprune) -> Result<Summary, RunError> {
    let seams = Seams {
        registry,
        prune: |cx, state| cx.prune(state),
        reprune,
        quirks: Quirks::default(),
        reports: None,
    };
    execute_staged(request, &seams, &Progress::default())
}

/// What a run is made of beyond its request: the drop-in forces quirks, and tests plant failures through the rest.
struct Seams<'a> {
    registry: &'a Registry,
    prune: Reprune,
    reprune: Reprune,
    /// Applied to every file on top of those its views give.
    quirks: Quirks,
    /// Where the report files go; none writes no report.
    reports: Option<ReportsAt>,
}

/// The run, `progress` left at the step it was in when it failed or panicked.
fn execute_staged(
    request: &Request,
    seams: &Seams<'_>,
    progress: &Progress,
) -> Result<Summary, RunError> {
    let threads = request
        .profile
        .threads
        .map_or(0, |threads| threads as usize);
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .map_err(io::Error::other)?
        .install(|| run(request, seams, progress))
}

fn run(request: &Request, seams: &Seams<'_>, progress: &Progress) -> Result<Summary, RunError> {
    let (registry, reprune) = (seams.registry, seams.reprune);
    progress.reach(Step::Setup);
    guard_output(&request.input, &request.output)?;
    let mut selection = registry.select(&request.profile)?;
    let overrides = request
        .profile
        .overrides()
        .map_err(|error| RunError::Refused(format!("override glob: {error}")))?;
    let zip = encode::zip_options(&request.profile)?;
    let limits = limits(&request.limits);
    let cache = Cache::new(request.cache.as_ref().map(|at| Store {
        dir: at.dir.clone(),
        max_bytes: at.max_bytes,
    }));
    let run_cx = RunCx::default();
    // The memos are read while the inputs are: nothing asks them before the first analysis.
    let ((memo, tokens), read) = rayon::join(
        || {
            // Without a store the memos would only be recorded into and dropped.
            let memo = cache.enabled().then(|| {
                timed("oracle memo (load)", || {
                    cache
                        .get(ORACLE_MEMO, b"", &[], Memo::from_bytes)
                        .unwrap_or_default()
                })
            });
            (memo, timed("token memo (load)", || cache.load_tokens()))
        },
        || -> Result<_, RunError> {
            progress.reach(Step::Input);
            let pack = timed("read", || PackInput::open(&request.input, &limits))?;
            progress.reach(Step::Analysis);
            let vanilla = timed("vanilla", || {
                inputs::vanilla(&request.vanilla, &limits, &cache)
            })?;
            let sources = timed("sources", || {
                inputs::sources(
                    &request.sources,
                    &run_cx,
                    &cache,
                    &own_dirs(request, seams.reports.as_ref()),
                )
            })?;
            Ok((pack, vanilla, sources))
        },
    );
    let (pack, vanilla, (mut sources, source_facts)) = read?;
    if let Some(tokens) = tokens {
        cache.use_tokens(tokens);
    }
    let rules = ClientRules::for_version(&vanilla.version);
    let forced = force_included(&pack, &overrides, &vanilla, registry, &request.input)?;
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
    let assets = inputs::JarAssets::new(&request.vanilla.client_jar, &limits);

    let analysed = timed("analyse", || cx.analyse(pack))?;
    let (analysis, input_cost) = rayon::join(
        || {
            timed("report", || {
                report::analysis(
                    &request.input,
                    &request.vanilla,
                    &analysed,
                    &vanilla,
                    &rules,
                    &source_facts,
                )
            })
        },
        || {
            timed("cost (input)", || {
                report::cost(&analysed.pack, &analysed.layout, &vanilla)
            })
        },
    );
    let mut analysis = analysis?;
    if let Some(expected) = &request.expect {
        expected.check(&analysis.report)?;
    }
    let kept = Kept::of(&analysed);
    let input = analysed.reached.clone();
    let (pruned, dropped) = timed("prune", || (seams.prune)(&cx, analysed))?;
    let pruned_count = count(&dropped, |path| {
        input.files.get(path).map_or(0, |file| file.bytes)
    });
    let mut dropped_report = report::dropped(&dropped, &input);

    progress.reach(Step::Optimisation);
    let passes_ran = !selection.passes.is_empty() || !selection.lossy.is_empty();
    let mut lossless = std::mem::take(&mut selection.passes);
    // The paths the passes start from: what every profile glob is matched against (`Origins`).
    let start: Vec<PhysPath> = pruned.pack.files.keys().cloned().collect();
    order(&mut lossless);
    let mut repruned = BTreeMap::new();

    // Each state is dropped as soon as no later step reads it: the run's peak is the states alive at once.
    let first = timed("lossless passes", || {
        packprune_pass::run(&cx, &kept, &pruned, &lossless)
    })?;
    let changed = first.changed();
    let mut reports = first.reports;
    let (proved, proof) = if changed {
        let proved = reprune_with(&cx, reprune, first.state, &mut repruned)?;
        let verdict = timed("oracle", || {
            compare(
                &cx,
                memo.as_ref(),
                assets.load()?,
                &kept,
                &oracle_input(&pruned),
                &oracle_input(&proved),
                &first.remap,
            )
        })?;
        if verdict.differing_keys > 0 {
            return Err(RunError::NotEquivalent {
                passes: lossless.iter().map(|enabled| enabled.pass.name()).collect(),
                verdict,
            });
        }
        drop(pruned);
        (proved, Some(verdict))
    } else {
        drop(first.state);
        (pruned, None)
    };

    // Without a lossy pass, `lossy::run` would hand back a copy of `proved`.
    let (output, loss, remap) = if selection.lossy.is_empty() {
        (proved, None, first.remap)
    } else {
        let second = timed("lossy passes", || {
            lossy::run(&cx, &kept, &proved, &selection, &start, &first.remap)
        })?;
        let changed = second.changed();
        reports.extend(second.reports);
        let remap = first.remap.then(&second.remap);
        if changed {
            let output = reprune_with(&cx, reprune, second.state, &mut repruned)?;
            let verdict = timed("oracle (loss)", || {
                compare(
                    &cx,
                    memo.as_ref(),
                    assets.load()?,
                    &kept,
                    &oracle_input(&proved),
                    &oracle_input(&output),
                    &second.remap,
                )
            })?;
            drop(proved);
            (output, Some(verdict), remap)
        } else {
            drop(second.state);
            (proved, None, remap)
        }
    };

    // The optimisers and the writer read files and views only; the last oracle reads `output` and `layout` again.
    let Analysed {
        pack: output,
        layout,
        index,
        reached,
    } = output;
    drop((index, reached));
    let origins = Origins::new(&start, &remap);
    let stage = timed("file optimisers", || {
        optimisers::run(
            &output,
            &layout,
            &selection.optimisers,
            &overrides,
            &origins,
            &cache,
            seams.quirks,
        )
    })?;
    progress.reach(Step::Write);
    let staged = timed("write", || {
        encode::stage(&stage.pack, &layout, &request.output, &zip, &cache)
    })?;
    let optimisers::Stage {
        pack: optimised,
        lossless: only_lossless,
        reports: optimiser_reports,
        loss: optimiser_loss,
    } = stage;
    let kept_files = Count {
        files: optimised.files.len(),
        bytes: optimised
            .files
            .values()
            .map(|entry| entry.blob.len() as u64)
            .sum(),
    };
    // The writer is proved by bytes, stronger than the oracle: the zip read back holds what it was given.
    let given: BTreeMap<PhysPath, Digest> = optimised
        .files
        .iter()
        .map(|(path, entry)| (path.clone(), entry.digest))
        .collect();
    drop(optimised);
    progress.reach(Step::SelfCheck);
    // The optimisers are proved on the zip as written, all four channels of every texel: the lossless ones
    // strictly, apart from the lossy ones, whose differences are their measured loss.
    let laid = |pack: &PackInput| -> Result<Layout, RunError> {
        let meta = PackMeta::read(pack).map_err(|error| RunError::Refused(error.to_string()))?;
        Ok(Layout::new(pack, meta, cx.vanilla))
    };
    let (zip_pack, zip_layout) = timed("read back", || -> Result<_, RunError> {
        let written = PackInput::open(&staged.path, &limits)?;
        let layout = laid(&written)?;
        Ok((written, layout))
    })?;
    // The self-check's analysis, the oracle on the zip and the output's cost read the written pack only: they run
    // at once, and their errors are taken in the order they would come one after the other.
    let changers = lossless_changers(&selection.optimisers, &optimiser_reports);
    let (checked, (proofs, output_cost)) = rayon::join(
        || {
            timed("self-check", || {
                cx.index_and_reached(&zip_pack, &zip_layout)
            })
        },
        || {
            rayon::join(
                || {
                    prove_zip(
                        &cx,
                        memo.as_ref(),
                        &assets,
                        &kept,
                        (&output, &layout),
                        only_lossless.as_ref(),
                        (&zip_pack, &zip_layout),
                        laid,
                        changers,
                    )
                },
                || {
                    timed("cost (output)", || {
                        report::cost(&zip_pack, &zip_layout, &vanilla)
                    })
                },
            )
        },
    );
    let (index, reached) = checked?;
    drop(index);
    // The self-check built the run's last index: the token memo is complete.
    let token_bytes = timed("token memo (save)", || cache.token_bytes());
    let mut differences = crate::selfcheck::differences(&run_cx, &input, &reached, &remap);
    if !same_digests(&given, &zip_pack) {
        differences.push("the zip writer changed bytes it was given".into());
    }
    if !differences.is_empty() {
        return Err(RunError::SelfCheck(differences));
    }
    drop(given);
    // The `force_include` notes are added after the passes ran: they belong to the report this run writes, not to the
    // analysis `--expect-report` checks.
    analysis
        .report
        .warnings
        .extend(ignored_notes(&forced.ignored, &dropped, &repruned, &input));
    let mut warnings = analysis.report.warnings.clone();
    drop(input);
    let verified = |views: &ViewSet| {
        views.iter().any(|view| {
            zip_layout
                .views
                .get(view.0 as usize)
                .is_some_and(|v| v.verified)
        })
    };
    let missing = reached
        .missing
        .iter()
        .filter(|m| verified(&m.views))
        .count();
    let not_stitched = reached
        .not_stitched
        .iter()
        .filter(|n| verified(&n.views))
        .count();
    drop(reached);
    let ZipProofs {
        on_zip,
        zip_loss,
        memo: memo_bytes,
    } = proofs?;
    drop((output, layout, assets, only_lossless, zip_pack, zip_layout));
    let written = staged.written;
    let (exceeded, notes) =
        report::check_budgets(&request.budgets, Some(written.bytes), &output_cost)?;
    warnings.extend(notes);
    let cost = report::run_cost(input_cost, output_cost, &mut warnings);
    let (cache_hits, cache_misses) = cache.counts();
    dropped_report.extend(report::reprune_line(&repruned));
    let mut summary = Summary {
        output: request.output.clone(),
        kept: kept_files,
        written,
        pruned: pruned_count,
        repruned: Count {
            files: repruned.len(),
            bytes: repruned.values().sum(),
        },
        passes_ran,
        passes: reports,
        proof,
        loss,
        optimisers: optimiser_reports,
        optimiser_loss,
        on_zip,
        zip_loss,
        cache_hits,
        cache_misses,
        oracle_memo: memo.as_ref().map_or((0, 0), Memo::counts),
        missing,
        not_stitched,
        warnings,
        exceeded,
        analysis: Box::new(analysis),
        dropped: dropped_report,
        reprune: repruned.keys().map(ToString::to_string).collect(),
        cost,
    };
    // The report files are all written beside their names before the zip replaces the old one: a report that cannot
    // be written never leaves a new zip behind an error.
    let reports = match &seams.reports {
        Some(at) => timed("write report", || at.prepare(&mut summary))?,
        None => None,
    };
    progress.reach(Step::Publish);
    staged.publish(&request.output)?;
    if let Some(bytes) = &memo_bytes {
        cache.put(ORACLE_MEMO, b"", &[], bytes);
    }
    if let Some(bytes) = &token_bytes {
        cache.save_tokens(bytes);
    }
    cache.evict();
    if let Some(at) = &seams.reports {
        at.finish(reports, &mut summary);
    }
    Ok(summary)
}

/// The strict oracle between the pack the file optimisers were given and what the lossless ones made of it, then
/// the lossy ones' measured loss, as `run` reports them, and the oracle memo as it stands after them.
#[expect(
    clippy::too_many_arguments,
    reason = "the two sides of each proof are passed apart, borrowed from states the run drops one by one"
)]
fn prove_zip(
    cx: &Context<'_>,
    memo: Option<&Memo>,
    assets: &inputs::JarAssets,
    kept: &Kept,
    (output, layout): (&PackInput, &Layout),
    only_lossless: Option<&PackInput>,
    (zip_pack, zip_layout): (&PackInput, &Layout),
    laid: impl Fn(&PackInput) -> Result<Layout, RunError>,
    changers: Vec<&'static str>,
) -> Result<ZipProofs, RunError> {
    // The oracle reads no index.
    let no_index = ReferenceIndex::default();
    let built;
    let (lossless, lossless_layout) = match only_lossless {
        Some(lossless) => {
            built = laid(lossless)?;
            (lossless, &built)
        }
        None => (zip_pack, zip_layout),
    };
    let on_zip = if same_bytes(output, lossless) {
        None
    } else {
        let verdict = timed("oracle (zip)", || {
            compare(
                cx,
                memo,
                assets.load()?,
                kept,
                &side(output, layout, &no_index),
                &side(lossless, lossless_layout, &no_index),
                &Remap::default(),
            )
        })?;
        if verdict.differing_keys > 0 {
            return Err(RunError::NotEquivalent {
                passes: changers,
                verdict,
            });
        }
        Some(verdict)
    };
    let zip_loss = match only_lossless {
        Some(lossless) => Some(timed("oracle (zip loss)", || {
            compare(
                cx,
                memo,
                assets.load()?,
                kept,
                &side(lossless, lossless_layout, &no_index),
                &side(zip_pack, zip_layout, &no_index),
                &Remap::default(),
            )
        })?),
        None => None,
    };
    // No oracle runs after these: the memo is saved as it is now, once the run is published.
    let memo = memo.map(|memo| timed("oracle memo (save)", || memo.to_bytes()));
    Ok(ZipProofs {
        on_zip,
        zip_loss,
        memo,
    })
}

/// What [`prove_zip`] found.
struct ZipProofs {
    on_zip: Option<Verdict>,
    zip_loss: Option<Verdict>,
    /// The oracle memo's bytes.
    memo: Option<Vec<u8>>,
}

/// The folders packprune writes, which no source reads: the cache and the reports.
fn own_dirs(request: &Request, reports: Option<&ReportsAt>) -> Vec<Utf8PathBuf> {
    request
        .cache
        .iter()
        .map(|at| at.dir.clone())
        .chain(reports.map(|at| at.root().to_owned()))
        .collect()
}

/// What `custom = { force_include = true }` asks of a run.
struct ForceIncluded {
    /// A usage source naming each file of no known kind by path: like a keep-list entry, each is a root of every
    /// analysis of the run, with what it references, and a pass may not move it.
    source: Option<SourceUsages>,
    /// The files of a known kind the option covers, with the glob that does: left to the passes and the prune like
    /// any other file.
    ignored: BTreeMap<PhysPath, String>,
}

/// `force_include` keeps by path only files of no known kind: a file is of a known kind when the layout classifies it
/// or a file optimiser applies to it.
fn force_included(
    pack: &PackInput,
    overrides: &Overrides<'_>,
    vanilla: &VanillaIndex,
    registry: &Registry,
    input: &Utf8Path,
) -> Result<ForceIncluded, RunError> {
    let mut matched = Vec::new();
    for path in pack.files.keys() {
        let Some((glob, options)) = overrides.rule_for(options::CUSTOM, path.as_str()) else {
            continue;
        };
        let custom = options::custom(&options)
            .map_err(|why| RunError::Refused(format!("custom options of {path}: {why}")))?;
        if custom.force_include {
            matched.push((path, glob));
        }
    }
    let mut forced = ForceIncluded {
        source: None,
        ignored: BTreeMap::new(),
    };
    if matched.is_empty() {
        return Ok(forced);
    }
    let meta = PackMeta::read(pack).map_err(|error| RunError::Refused(error.to_string()))?;
    let layout = Layout::new(pack, meta, vanilla);
    let mut usages = Vec::new();
    for (path, glob) in matched {
        let kind = layout.kinds.get(path).copied().unwrap_or(FileKind::Unknown);
        if kind == FileKind::Unknown
            && !registry
                .optimisers
                .iter()
                .any(|optimiser| optimiser.applies(kind, path))
        {
            usages.push(Usage {
                kind: UsageKind::Path,
                id: IdGlob(path.as_str().into()),
                atlas: None,
                location: path.to_string(),
                reason: Some("force_include".into()),
                declares: None,
                quiet: false,
            });
        } else {
            forced.ignored.insert(path.clone(), glob.to_owned());
        }
    }
    if !usages.is_empty() {
        forced.source = Some(SourceUsages {
            source: "force_include".into(),
            path: input.canonicalize_utf8()?,
            usages,
        });
    }
    Ok(forced)
}

/// The files `force_include` covers that the prune removed, each with its glob and why it went: the option keeps no
/// file of a known kind, so what keeps one is a usage source or the keep-list.
fn ignored_notes(
    ignored: &BTreeMap<PhysPath, String>,
    dropped: &BTreeSet<PhysPath>,
    repruned: &BTreeMap<PhysPath, u64>,
    reached: &Reached,
) -> Vec<String> {
    ignored
        .iter()
        .filter_map(|(path, glob)| {
            let why = if dropped.contains(path) {
                reached.files[path].status.id()
            } else if repruned.contains_key(path) {
                "reprune"
            } else {
                return None;
            };
            Some(format!(
                "force_include {glob:?} covers {path}, a file of a known kind: it is not kept by path, and the prune \
                 removed it ({why}); keep it with a usage source or the keep-list"
            ))
        })
        .collect()
}

/// `state` re-pruned, what it dropped added to `repruned` with its size.
fn reprune_with(
    cx: &Context<'_>,
    reprune: Reprune,
    state: Analysed,
    repruned: &mut BTreeMap<PhysPath, u64>,
) -> Result<Analysed, RunError> {
    let sizes: BTreeMap<PhysPath, u64> = state
        .pack
        .files
        .iter()
        .map(|(path, entry)| (path.clone(), entry.blob.len() as u64))
        .collect();
    let (state, dropped) = timed("re-prune", || reprune(cx, state))?;
    repruned.extend(dropped.into_iter().map(|path| {
        let size = sizes[&path];
        (path, size)
    }));
    Ok(state)
}

/// The oracle's cross-run memo: one entry, rewritten by every run with what it used.
const ORACLE_MEMO: &str = "oracle memo";

/// The oracle between two states of the run. What a source, the keep-list or `force_include` names compares by name:
/// they are all roots of the analysis.
fn compare(
    cx: &Context<'_>,
    memo: Option<&Memo>,
    assets: &inputs::JarAssets,
    kept: &Kept,
    before: &OracleInput<'_>,
    after: &OracleInput<'_>,
    remap: &Remap,
) -> Result<Verdict, RunError> {
    let started = Instant::now();
    let named = |logical: &LogicalPath| kept.names.contains(logical);
    let comparison = match memo {
        Some(memo) => compare_memoised(
            cx.run, before, after, cx.vanilla, assets, remap, &named, memo,
        ),
        None => compare_with(cx.run, before, after, cx.vanilla, assets, remap, &named),
    };
    let labels = view_labels(before.layout, after.layout);
    Ok(verdict(&comparison, &labels, started))
}

pub(crate) fn verdict(comparison: &Comparison, labels: &[String], started: Instant) -> Verdict {
    Verdict {
        views: comparison.views,
        keys: comparison.keys,
        differing_keys: comparison.differing_keys,
        differences: comparison
            .differences
            .iter()
            .map(|difference| {
                let view = labels
                    .get(difference.view.0 as usize)
                    .map_or("?", String::as_str);
                let detail = if difference.detail.is_empty() {
                    String::new()
                } else {
                    format!(" {}", difference.detail)
                };
                format!("view {view}: {}{detail}", difference.key)
            })
            .collect(),
        millis: started.elapsed().as_millis(),
    }
}

/// The lossless optimisers that changed a file, which a refusal names.
fn lossless_changers(
    optimisers: &[options::Optimiser<'_>],
    reports: &[OptimiserReport],
) -> Vec<&'static str> {
    optimisers
        .iter()
        .zip(reports)
        .filter(|(enabled, report)| enabled.lossy.is_none() && report.files > 0)
        .map(|(_, report)| report.optimiser)
        .collect()
}

/// `pack` holds exactly these paths with these contents.
fn same_digests(digests: &BTreeMap<PhysPath, Digest>, pack: &PackInput) -> bool {
    digests.len() == pack.files.len()
        && digests
            .iter()
            .zip(&pack.files)
            .all(|((a, x), (b, entry))| a == b && *x == entry.digest)
}

/// The same paths with the same bytes: the oracle would find nothing.
fn same_bytes(a: &PackInput, b: &PackInput) -> bool {
    a.files.len() == b.files.len()
        && a.files
            .iter()
            .zip(&b.files)
            .all(|((a, x), (b, y))| a == b && x.digest == y.digest)
}

fn side<'a>(pack: &'a PackInput, layout: &'a Layout, index: &'a ReferenceIndex) -> OracleInput<'a> {
    OracleInput {
        pack,
        layout,
        index,
    }
}

fn oracle_input(state: &Analysed) -> OracleInput<'_> {
    OracleInput {
        pack: &state.pack,
        layout: &state.layout,
        index: &state.index,
    }
}

fn count(paths: &BTreeSet<PhysPath>, size: impl Fn(&PhysPath) -> u64) -> Count {
    Count {
        files: paths.len(),
        bytes: paths.iter().map(size).sum(),
    }
}

pub(crate) fn limits(config: &InputLimits) -> Limits {
    let default = Limits::default();
    Limits {
        max_entries: config.max_entries.unwrap_or(default.max_entries),
        max_total_bytes: config.max_total_bytes.unwrap_or(default.max_total_bytes),
        max_entry_bytes: config.max_entry_bytes.unwrap_or(default.max_entry_bytes),
        max_ratio: config.max_ratio.unwrap_or(default.max_ratio),
        ratio_min_entry_bytes: config
            .ratio_min_entry_bytes
            .unwrap_or(default.ratio_min_entry_bytes),
    }
}

/// The output is never the input, inside it, or around it, where a later run would read it as pack files.
fn guard_output(input: &Utf8Path, output: &Utf8Path) -> Result<(), RunError> {
    guard(input, output, "output")
}

/// `path` (a `what`) is none of: the input, inside it, around it.
fn guard(input: &Utf8Path, path: &Utf8Path, what: &str) -> Result<(), RunError> {
    let input = input
        .canonicalize_utf8()
        .map_err(|error| RunError::Refused(format!("{input}: {error}")))?;
    let real = resolve(path)?;
    let why = if real == input {
        "is the input"
    } else if real.starts_with(&input) {
        "is inside the input"
    } else if input.starts_with(&real) {
        "contains the input"
    } else {
        return Ok(());
    };
    Err(RunError::Refused(format!("{what} {path} {why} {input}")))
}

/// `path` made absolute with every symlink on the way resolved, the last component included, as far as it exists;
/// what is not there yet is kept as written. A directory a link leads into the input is the input.
pub fn resolve(path: &Utf8Path) -> io::Result<Utf8PathBuf> {
    let absolute = Utf8PathBuf::try_from(std::path::absolute(path)?)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let mut real = Utf8PathBuf::new();
    for component in absolute.components() {
        match component {
            Utf8Component::ParentDir => {
                real.pop();
            }
            Utf8Component::CurDir => {}
            Utf8Component::Normal(name) => {
                real.push(name);
                match real.canonicalize_utf8() {
                    Ok(resolved) => real = resolved,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
            prefix_or_root => real.push(prefix_or_root),
        }
    }
    Ok(real)
}

fn timed<T>(step: &str, work: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let result = work();
    // glibc keeps what a step freed in each worker thread's arena; without a trim the next step's peak adds to it.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: `malloc_trim` only returns free pages to the system.
    unsafe {
        libc::malloc_trim(0);
    }
    tracing::info!(step, elapsed = ?started.elapsed(), "done");
    result
}
