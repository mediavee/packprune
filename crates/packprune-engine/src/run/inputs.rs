//! What a run reads besides the pack: the client jar and asset index, and the usage sources.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use camino::{Utf8Path, Utf8PathBuf};
use packprune_graph::{SourceError, SourceUsages};
use packprune_oracle::VanillaAssets;
use packprune_pack::vanilla::VanillaError;
use packprune_pack::{Blob, Limits, RunCx, VanillaIndex};
use packprune_report::reports::{SourceFacts, SourceInput};

use super::RunError;
use super::cache::Cache;

/// The client the views resolve over.
#[derive(Clone, Debug)]
pub struct VanillaInputs {
    pub client_jar: Utf8PathBuf,
    pub asset_index: Option<Utf8PathBuf>,
}

/// A usage source on its input: a provider id of `packprune_sources::PROVIDERS` and the path it opens.
#[derive(Clone, Debug)]
pub struct SourceRequest {
    pub id: String,
    pub path: Utf8PathBuf,
}

/// The jar's index, cached by the jar's digest; the asset index is read every run, it is one small JSON file.
pub(crate) fn vanilla(
    inputs: &VanillaInputs,
    limits: &Limits,
    cache: &Cache,
) -> Result<VanillaIndex, RunError> {
    let jar = &inputs.client_jar;
    let digest = *blake3::hash(&std::fs::read(jar).map_err(|error| io(jar, error))?).as_bytes();
    // The index is valid only under the read policy that built it: a jar the limits refuse is refused warm too.
    let policy = [
        u64::from(limits.max_entries),
        limits.max_total_bytes,
        limits.max_entry_bytes,
        u64::from(limits.max_ratio),
        limits.ratio_min_entry_bytes,
    ]
    .map(u64::to_le_bytes)
    .concat();
    let cached = cache.get("vanilla-index", &policy, &[digest], |bytes| {
        VanillaIndex::from_bytes(bytes).ok()
    });
    let index = match cached {
        Some(index) => index,
        None => {
            let index = VanillaIndex::from_jar(jar, limits).map_err(vanilla_error)?;
            cache.put("vanilla-index", &policy, &[digest], &index.to_bytes());
            index
        }
    };
    match &inputs.asset_index {
        Some(path) => index.with_asset_index(path).map_err(vanilla_error),
        None => Ok(index),
    }
}

fn vanilla_error(error: VanillaError) -> RunError {
    match error {
        VanillaError::Io(error) => RunError::Io(error),
        error => RunError::Refused(error.to_string()),
    }
}

fn io(path: &Utf8Path, error: std::io::Error) -> RunError {
    RunError::Io(std::io::Error::new(
        error.kind(),
        format!("{path}: {error}"),
    ))
}

/// The jar's `assets/` files, read the first time the oracle needs them: a run whose passes change nothing never
/// reads them, and the oracle asks for nothing else (its classes are most of the jar).
pub(crate) struct JarAssets {
    jar: Utf8PathBuf,
    limits: Limits,
    files: OnceLock<BTreeMap<String, Blob>>,
}

impl JarAssets {
    pub(crate) fn new(jar: &Utf8Path, limits: &Limits) -> Self {
        Self {
            jar: jar.to_owned(),
            limits: limits.clone(),
            files: OnceLock::new(),
        }
    }

    /// The jar read, before the first comparison: a jar that cannot be read fails the run, never the comparison.
    pub(crate) fn load(&self) -> Result<&Self, RunError> {
        if self.files.get().is_none() {
            let files = packprune_zip::read_selected(&self.jar, &self.limits, &|name| {
                name.starts_with("assets/")
            })
            .map_err(|error| RunError::Refused(format!("{}: {error}", self.jar)))?;
            let _ = self.files.set(
                files
                    .into_iter()
                    .map(|(name, bytes)| (name, bytes.into()))
                    .collect(),
            );
        }
        Ok(self)
    }
}

impl VanillaAssets for JarAssets {
    fn bytes(&self, logical: &str) -> Option<Blob> {
        self.files
            .get()
            .expect("JarAssets::load runs before a comparison")
            .get(logical)
            .cloned()
    }
}

/// Every requested source opened and collected, in request order, with what it read for the report; a source that
/// refuses its input refuses the run. What a source memoises across runs lives in `cache`; `own` are packprune's
/// folders no source reads.
pub(crate) fn sources(
    requests: &[SourceRequest],
    cx: &RunCx,
    cache: &Cache,
    own: &[Utf8PathBuf],
) -> Result<(Vec<SourceUsages>, Vec<SourceFacts>), RunError> {
    requests
        .iter()
        .map(|request| {
            let provider = packprune_sources::PROVIDERS
                .iter()
                .find(|provider| provider.id == request.id)
                .ok_or_else(|| {
                    let ids: Vec<&str> = packprune_sources::PROVIDERS
                        .iter()
                        .map(|provider| provider.id)
                        .collect();
                    RunError::Refused(format!(
                        "source {}: unknown, expected one of {}",
                        request.id,
                        ids.join(", ")
                    ))
                })?;
            let path = request.path.canonicalize_utf8().map_err(|error| {
                RunError::Refused(format!("source {} {}: {error}", request.id, request.path))
            })?;
            let mut source =
                (provider.open)(&path, cx).map_err(|error| source_error(&request.id, error))?;
            source.exclude(own);
            let inputs = source
                .inputs()
                .iter()
                .map(|input| {
                    let digest = source.input_digest(input)?;
                    Ok(SourceInput {
                        name: input.file_name().unwrap_or(input.as_str()).to_owned(),
                        blake3: digest.0.iter().map(|byte| format!("{byte:02x}")).collect(),
                    })
                })
                .collect::<Result<Vec<_>, SourceError>>()
                .map_err(|error| source_error(&request.id, error))?;
            let usages = source
                .collect_memoised(cache)
                .map_err(|error| source_error(&request.id, error))?;
            tracing::info!(source = request.id, %path, usages = usages.len(), "source");
            Ok((
                SourceUsages {
                    source: request.id.clone(),
                    path,
                    usages,
                },
                SourceFacts {
                    id: request.id.clone(),
                    inputs,
                },
            ))
        })
        .collect::<Result<Vec<_>, RunError>>()
        .map(|read| read.into_iter().unzip())
}

fn source_error(id: &str, error: SourceError) -> RunError {
    match error {
        SourceError::Refused(..) => RunError::Refused(format!("source {id}: {error}")),
        SourceError::Io(error) => RunError::Io(error),
    }
}
