//! `--expect`: a run applies the plan an operator reviewed, or nothing.

use std::collections::BTreeSet;

use camino::Utf8PathBuf;
use packprune_report::reports::{Input, Report};

use super::{RunError, bullets};

/// The report an operator reviewed, and where it was read from.
#[derive(Clone, Debug)]
pub struct Expected {
    pub path: Utf8PathBuf,
    pub report: Report,
}

impl Expected {
    /// Refuses unless the pack, the client jar, the asset index, the sources' inputs and the files to drop of `actual`,
    /// the analysis this run goes on with, are those of the report.
    pub(super) fn check(&self, actual: &Report) -> Result<(), RunError> {
        let differences = differences(&self.report, actual);
        if differences.is_empty() {
            return Ok(());
        }
        Err(RunError::Refused(format!(
            "{} does not match this run:\n{}",
            self.path,
            bullets(&differences)
        )))
    }
}

/// What differs: the inputs' hashes, the sources' inputs, then the files to drop.
fn differences(expected: &Report, actual: &Report) -> Vec<String> {
    let (expected_inputs, inputs) = (&expected.inputs, &actual.inputs);
    let mut differences = Vec::new();
    for (what, expected, actual) in [
        ("pack", &expected_inputs.pack, &inputs.pack),
        (
            "client jar",
            &expected_inputs.client_jar,
            &inputs.client_jar,
        ),
        (
            "asset index",
            &expected_inputs.asset_index,
            &inputs.asset_index,
        ),
    ] {
        if expected.sha256 != actual.sha256 {
            differences.push(format!(
                "{what} differs: the report has {}, this run {}",
                describe(expected),
                describe(actual)
            ));
        }
    }
    let (listed, run) = (source_inputs(expected), source_inputs(actual));
    differences.extend(
        listed
            .difference(&run)
            .map(|input| format!("the report has source {input}, this run does not")),
    );
    differences.extend(
        run.difference(&listed)
            .map(|input| format!("this run has source {input}, the report does not")),
    );
    let listed: BTreeSet<&str> = expected.removable.iter().map(String::as_str).collect();
    let run: BTreeSet<&str> = actual.removable.iter().map(String::as_str).collect();
    differences.extend(
        listed
            .difference(&run)
            .map(|path| format!("the report drops {path}, this run keeps it")),
    );
    differences.extend(
        run.difference(&listed)
            .map(|path| format!("this run drops {path}, the report keeps it")),
    );
    differences
}

/// One line per input of every source: the id, the input's name and its digest.
fn source_inputs(report: &Report) -> BTreeSet<String> {
    report
        .sources
        .iter()
        .flat_map(|source| {
            source
                .inputs
                .iter()
                .map(|input| format!("{} {} (blake3 {})", source.id, input.name, input.blake3))
        })
        .collect()
}

fn describe(input: &Input) -> String {
    if input.sha256.is_empty() {
        "none".into()
    } else {
        format!("{} (sha256 {})", input.name, input.sha256)
    }
}
