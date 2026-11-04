//! The pass API: graph passes, file optimisers, edits, scope, and the proof harness.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use packprune_graph::ReferenceIndex;
use packprune_pack::{
    Blob, ClientRules, FileKind, Layout, LogicalPath, PackInput, PhysPath, Quirks, ResourceId,
    RunCx, VanillaIndex,
};

mod apply;
mod harness;
mod run;

#[cfg(test)]
mod tests;

pub use apply::Refusal;
pub use harness::{Proof, prove};
pub use run::{Analysed, Context, Enabled, Kept, Outcome, PassReport, RunError, order, run};

pub use packprune_graph::{NoCache, SubCache};
pub use packprune_oracle::Remap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Loss {
    Lossless,
    Lossy,
}

/// The stage a pass belongs to; lossless passes run in this order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Stage {
    Models,
    Textures,
    Fonts,
    Overlays,
    Names,
    Json,
    Lossy,
}

/// The paths a pass may change: every path for a lossless pass, its globs for a lossy one. The caller gives the
/// matcher, so the glob syntax stays the config's.
#[derive(Clone, Default)]
pub struct Scope {
    only: Option<Arc<PathTest>>,
}

type PathTest = dyn Fn(&PhysPath) -> bool + Send + Sync;

impl Scope {
    /// Every path: a lossless pass's scope.
    pub fn all() -> Self {
        Self::default()
    }

    /// The paths `test` accepts: a lossy pass's globs.
    pub fn matching(test: impl Fn(&PhysPath) -> bool + Send + Sync + 'static) -> Self {
        Self {
            only: Some(Arc::new(test)),
        }
    }

    pub fn contains(&self, path: &PhysPath) -> bool {
        self.only.as_ref().is_none_or(|test| test(path))
    }
}

impl fmt::Debug for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.only.is_some() {
            "Scope(matching)"
        } else {
            "Scope(all)"
        })
    }
}

/// Per file or key: the metric and its value.
#[derive(Clone, Debug, PartialEq)]
pub struct LossRecord {
    pub key: String,
    pub metric: String,
    pub value: f64,
}

pub struct PassCx<'a> {
    pub run: &'a RunCx,
    pub pack: &'a PackInput,
    pub layout: &'a Layout,
    pub index: &'a ReferenceIndex,
    pub vanilla: &'a VanillaIndex,
    pub rules: &'a ClientRules,
    pub kept_names: &'a BTreeSet<LogicalPath>,
    pub pinned: &'a BTreeSet<PhysPath>,
    pub scope: &'a Scope,
    pub options: &'a toml::Table,
    pub cache: &'a dyn SubCache,
}

#[derive(Debug, thiserror::Error)]
pub enum PassError {
    /// A remap of a kept name, a change outside the scope or to a pinned path.
    #[error("{pass}: refused: {why}")]
    Refused { pass: &'static str, why: String },
    #[error("{pass}: {why}")]
    Failed { pass: &'static str, why: String },
}

pub trait Pass: Send + Sync {
    fn name(&self) -> &'static str;
    fn stage(&self) -> Stage;
    fn loss(&self) -> Loss;
    fn options_schema(&self) -> schemars::Schema;
    /// Checks an options table when the config is loaded, with the code [`Self::run`] reads it with. The default
    /// takes no option.
    fn validate_options(&self, options: &toml::Table) -> Result<(), String> {
        no_options(self.name(), options)
    }
    fn run(&self, cx: &PassCx<'_>) -> Result<Edits, PassError>;
}

/// What a pass changes: file bytes, removals, additions, moves, the remaps of names and sprites, its report lines
/// and its measured loss.
#[derive(Debug, Default)]
pub struct Edits {
    pub replaced: BTreeMap<PhysPath, Blob>,
    pub removed: BTreeSet<PhysPath>,
    pub added: BTreeMap<PhysPath, Blob>,
    pub moves: BTreeMap<PhysPath, PhysPath>,
    pub file_remaps: BTreeMap<LogicalPath, LogicalPath>,
    pub sprite_remaps: BTreeMap<ResourceId, ResourceId>,
    pub changes: Vec<String>,
    pub loss: Vec<LossRecord>,
}

pub struct FileCx {
    pub kind: FileKind,
    pub path: PhysPath,
    pub quirks: Quirks,
}

pub struct Optimised {
    pub bytes: Vec<u8>,
    pub loss: Option<LossRecord>,
}

pub trait FileOptimizer: Send + Sync {
    fn name(&self) -> &'static str;
    fn loss(&self) -> Loss;
    fn applies(&self, kind: FileKind, path: &PhysPath) -> bool;
    fn options_schema(&self) -> schemars::Schema;
    /// Checks an options table when the config is loaded, with the code [`Self::optimise`] reads it with. The
    /// default takes no option.
    fn validate_options(&self, options: &toml::Table) -> Result<(), String> {
        no_options(self.name(), options)
    }
    /// Same input, context and options give the same bytes; `None` means keep the input.
    /// The error is the pass API's: this crate does not depend on `packprune-codec`.
    fn optimise(
        &self,
        input: &[u8],
        cx: &FileCx,
        options: &toml::Table,
    ) -> Result<Option<Optimised>, PassError>;
}

fn no_options(name: &str, options: &toml::Table) -> Result<(), String> {
    match options.keys().next() {
        None => Ok(()),
        Some(key) => Err(format!("unknown option `{key}`: {name} takes none")),
    }
}
