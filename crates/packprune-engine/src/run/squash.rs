//! The drop-in's failure matrix: whatever fails after the settings and the input were accepted, a pack goes out,
//! never one built from input packprune could not prove. The optimised run first; when its passes, their proof, the
//! file optimisers or its self-check fail, the same run with no pass (the pruned pack, self-checked); when the
//! analysis fails, or the pruned run too, the validated input as it is. A write error fails the drop-in: the
//! fallback would hit the same disk.

use std::any::Any;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU8, Ordering};

use camino::{Utf8Path, Utf8PathBuf};
use packprune_config::{InputLimits, PassSelection, Profile};
use packprune_pack::{PackInput, Quirks};
use packprune_zip::{WriteOptions, Written, ZipMode};

use super::report::ReportsAt;
use super::{Registry, Reprune, Request, RunError, Seams, Summary, encode, guard_output, limits};

/// Where a run was when it stopped, in run order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub(crate) enum Step {
    Setup,
    Input,
    Analysis,
    Optimisation,
    Write,
    SelfCheck,
    Publish,
}

/// The step a run reached, readable after it failed or panicked on any thread.
#[derive(Default)]
pub(crate) struct Progress(AtomicU8);

impl Progress {
    pub(crate) fn reach(&self, step: Step) {
        self.0.store(step as u8, Ordering::Relaxed);
    }

    fn step(&self) -> Step {
        [
            Step::Setup,
            Step::Input,
            Step::Analysis,
            Step::Optimisation,
            Step::Write,
            Step::SelfCheck,
            Step::Publish,
        ][usize::from(self.0.load(Ordering::Relaxed))]
    }
}

/// What the drop-in knows from the PackSquash settings alone, enough to ship the original.
#[derive(Clone, Debug)]
pub struct DropIn {
    /// `pack_directory`.
    pub input: Utf8PathBuf,
    /// `output_file_path`.
    pub output: Utf8PathBuf,
    /// Its `zip` table compresses the original; the original is always written `conformant`.
    pub profile: Profile,
    pub limits: InputLimits,
    /// `work_around_minecraft_quirks`, applied to every file on top of those its views give.
    pub quirks: Quirks,
    /// Where the runs keep their report, in its `latest` folder; none writes no report.
    pub reports: Option<Utf8PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Artefact {
    Optimised,
    /// Prune only, after the optimised run failed.
    Pruned,
    /// The validated input as it is.
    Original,
}

#[derive(Debug)]
pub struct Squashed {
    pub artefact: Artefact,
    /// The run that wrote the artefact; `None` for the original.
    pub summary: Option<Summary>,
    pub written: Written,
    /// Why each fallback was taken, in order.
    pub reasons: Vec<String>,
}

/// Writes `drop_in.output`. `full` is the optimised run, or why packprune's own config gives none. An error is a
/// refusal of the settings or the input (nothing written) or a write error (`.partial` staging removed).
pub fn squash(drop_in: &DropIn, full: Result<Request, String>) -> Result<Squashed, RunError> {
    squash_with(
        drop_in,
        full,
        &Registry::default(),
        |cx, state| cx.prune(state),
        |cx, state| cx.prune(state),
    )
}

pub(crate) fn squash_with(
    drop_in: &DropIn,
    full: Result<Request, String>,
    registry: &Registry,
    prune: Reprune,
    reprune: Reprune,
) -> Result<Squashed, RunError> {
    guard_output(&drop_in.input, &drop_in.output)?;
    writable(&drop_in.output)?;
    drop(PackInput::open(&drop_in.input, &limits(&drop_in.limits))?);

    let mut reasons = Vec::new();
    let request = match full {
        Ok(request) => request,
        Err(why) => {
            reasons.push(why);
            return original(drop_in, reasons);
        }
    };
    let seams = Seams {
        registry,
        prune,
        reprune,
        quirks: drop_in.quirks,
        reports: drop_in
            .reports
            .as_deref()
            .map(|root| ReportsAt::drop_in(root, &drop_in.input)),
    };
    let step = match attempt(&request, &seams) {
        Ok(summary) => return Ok(shipped(Artefact::Optimised, summary, reasons)),
        Err((step, error)) => {
            reasons.push(error.to_string());
            match step {
                Step::Input => return Err(error),
                Step::Write | Step::Publish => return Err(write_error(error)),
                step => step,
            }
        }
    };
    if step < Step::Optimisation {
        return original(drop_in, reasons);
    }

    let mut pruned = request;
    pruned.profile.passes = Some(PassSelection::Preset("none".into()));
    pruned.profile.lossy.clear();
    pruned.profile.oldest_format = None;
    let seams = Seams {
        quirks: Quirks::default(),
        ..seams
    };
    match attempt(&pruned, &seams) {
        Ok(summary) => Ok(shipped(Artefact::Pruned, summary, reasons)),
        Err((Step::Write | Step::Publish, error)) => Err(write_error(error)),
        Err((_, error)) => {
            reasons.push(error.to_string());
            original(drop_in, reasons)
        }
    }
}

/// One run with a panic caught as its failure, and the step it stopped at.
fn attempt(request: &Request, seams: &Seams<'_>) -> Result<Summary, (Step, RunError)> {
    let progress = Progress::default();
    match catch_unwind(AssertUnwindSafe(|| {
        super::execute_staged(request, seams, &progress)
    })) {
        Ok(Ok(summary)) => Ok(summary),
        Ok(Err(error)) => Err((progress.step(), error)),
        Err(payload) => Err((
            progress.step(),
            RunError::Io(io::Error::other(format!("panic: {}", message(&*payload)))),
        )),
    }
}

fn shipped(artefact: Artefact, summary: Summary, reasons: Vec<String>) -> Squashed {
    Squashed {
        artefact,
        written: summary.written,
        summary: Some(summary),
        reasons,
    }
}

/// The input read again through the validating reader and written as it is, staged beside the output.
fn original(drop_in: &DropIn, reasons: Vec<String>) -> Result<Squashed, RunError> {
    let pack = PackInput::open(&drop_in.input, &limits(&drop_in.limits))?;
    let zip = encode::zip_options(&drop_in.profile).ok();
    let options = WriteOptions {
        mode: ZipMode::Conformant,
        effort: zip
            .as_ref()
            .map_or(WriteOptions::default().effort, |zip| zip.compress.effort),
        recompress_compressed: zip
            .as_ref()
            .is_none_or(|zip| zip.compress.recompress_compressed),
        comment: zip.and_then(|zip| zip.layout.comment),
    };
    let staging = staging_dir(&drop_in.output)?;
    let path = Utf8Path::from_path(staging.path())
        .ok_or_else(|| RunError::Refused(format!("{}: not a UTF-8 path", drop_in.output)))?
        .join("output.zip");
    let written = crate::io::write(&pack, &path, &options).map_err(|error| match error {
        crate::io::IoError::Io(error) => RunError::Io(error),
        refused => RunError::Io(io::Error::other(refused.to_string())),
    })?;
    std::fs::rename(&path, &drop_in.output)?;
    Ok(Squashed {
        artefact: Artefact::Original,
        summary: None,
        written,
        reasons,
    })
}

fn staging_dir(output: &Utf8Path) -> io::Result<tempfile::TempDir> {
    let parent = match output.parent() {
        Some(parent) if !parent.as_str().is_empty() => parent,
        _ => Utf8Path::new("."),
    };
    tempfile::Builder::new()
        .prefix(".packprune-")
        .tempdir_in(parent)
}

/// `output_file_path` refused before any work when its directory cannot take a file.
fn writable(output: &Utf8Path) -> Result<(), RunError> {
    staging_dir(output)
        .map(drop)
        .map_err(|error| RunError::Refused(format!("output_file_path {output}: {error}")))
}

/// A failure while writing is exit 3 whatever its kind: the output's disk, not the input, refused it.
fn write_error(error: RunError) -> RunError {
    match error {
        RunError::Io(error) => RunError::Io(error),
        error => RunError::Io(io::Error::other(error.to_string())),
    }
}

fn message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "no message".into())
}

#[cfg(test)]
mod tests;
