//! What a run measured beyond the analysis: `passes.json` and `run.json`. Wall times are in the log only: these files
//! are a pure function of the inputs.

use serde::{Deserialize, Serialize};

use crate::budgets::Exceeded;
use crate::cost::Cost;

/// `passes.json`: each pass's gain and changes, the oracle's verdict and the lossy passes' loss.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Passes {
    pub passes: Vec<PassLine>,
    /// What the passes left nothing reaching, dropped with them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reprune: Vec<String>,
    /// The strict oracle after the lossless passes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oracle: Option<Verdict>,
    /// The oracle after the lossy passes: their measured loss.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loss: Option<Verdict>,
}

impl Passes {
    /// Whether the file says nothing: no pass ran and no verdict was reached.
    pub fn is_empty(&self) -> bool {
        self.passes.is_empty() && self.oracle.is_none() && self.loss.is_none()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PassLine {
    pub pass: String,
    pub lossless: bool,
    pub files_before: usize,
    pub files_after: usize,
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub changed_files: usize,
    pub changes: Vec<String>,
    /// Per file or key, what the pass measured on its lossy edits.
    pub measured: Vec<Measure>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Measure {
    pub key: String,
    pub metric: String,
    pub value: f64,
}

/// One oracle comparison.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Verdict {
    pub views: u32,
    pub keys: u32,
    pub differing_keys: u32,
    pub differences: Vec<String>,
}

/// `run.json`: the output, what was dropped, the file optimisers, the oracle on the zip, client memory and budgets.
/// `C` is the memory model of a view: [`Cost`] when a run writes it, [`CostLine`] when `report.md` is rendered again
/// from the file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", bound(deserialize = "C: Deserialize<'de>"))]
pub struct RunReport<C = Cost> {
    /// The written pack; `None` for a command that writes none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Output>,
    /// The files the first prune and the re-prunes left out, per reason.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dropped: Vec<Dropped>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub optimisers: Vec<OptimiserLine>,
    /// Lossy file optimisers: what each measured.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub optimiser_loss: Vec<Measure>,
    /// The strict oracle on the zip as written, for the lossless file optimisers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_zip: Option<Verdict>,
    /// The oracle across the lossy file optimisers: their measured loss.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zip_loss: Option<Verdict>,
    /// The client's memory for the input and the output, in the view of the client jar's format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<RunCost<C>>,
    /// The budgets the output exceeds; empty when it is within all of them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exceeded: Vec<BudgetLine>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Output {
    pub files: usize,
    pub bytes: u64,
    pub zip_bytes: u64,
    pub sha1: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dropped {
    pub reason: String,
    pub files: usize,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OptimiserLine {
    pub optimiser: String,
    pub files: usize,
    pub bytes_before: u64,
    pub bytes_after: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunCost<C = Cost> {
    pub input: Option<C>,
    pub output: Option<C>,
}

/// What `report.md` shows of a view's memory, read back from the `Cost` that `run.json` holds.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct CostLine {
    atlases: Vec<Atlas>,
    fonts: Vec<Font>,
    exclusions: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
struct Atlas {
    gpu: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
struct Font {
    total: u64,
}

/// The figures of a view's memory that `report.md` shows.
pub trait Figures {
    fn atlas_gpu_bytes(&self) -> u64;
    fn font_bytes(&self) -> u64;
    /// What the model leaves out; empty when it is complete.
    fn exclusions(&self) -> &[String];
}

impl Figures for Cost {
    fn atlas_gpu_bytes(&self) -> u64 {
        Cost::atlas_gpu_bytes(self)
    }

    fn font_bytes(&self) -> u64 {
        Cost::font_bytes(self)
    }

    fn exclusions(&self) -> &[String] {
        &self.exclusions
    }
}

impl Figures for CostLine {
    fn atlas_gpu_bytes(&self) -> u64 {
        self.atlases.iter().map(|atlas| atlas.gpu).sum()
    }

    fn font_bytes(&self) -> u64 {
        self.fonts.iter().map(|font| font.total).sum()
    }

    fn exclusions(&self) -> &[String] {
        &self.exclusions
    }
}

impl<C> Default for RunCost<C> {
    fn default() -> Self {
        Self {
            input: None,
            output: None,
        }
    }
}

impl<C> Default for RunReport<C> {
    fn default() -> Self {
        Self {
            output: None,
            dropped: Vec::new(),
            optimisers: Vec::new(),
            optimiser_loss: Vec::new(),
            on_zip: None,
            zip_loss: None,
            cost: None,
            exceeded: Vec::new(),
        }
    }
}

/// A budget the output exceeds: `limit` and `measured` are in the budget's own unit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetLine {
    pub budget: String,
    pub limit: u64,
    pub measured: u64,
}

impl From<&Exceeded> for BudgetLine {
    fn from(exceeded: &Exceeded) -> Self {
        Self {
            budget: exceeded.budget.to_owned(),
            limit: exceeded.limit,
            measured: exceeded.measured,
        }
    }
}
