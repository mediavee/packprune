//! Running passes in order: each pass reads what the previous one left and its edits are applied in sequence; the
//! pack is analysed again after the last pass.

use std::collections::BTreeSet;

use packprune_graph::{
    Change, Inputs, Policy, Reached, ReferenceIndex, Refused, SourceUsages, UnusedSelector,
};
use packprune_oracle::Comparison;
use packprune_pack::meta::MetaError;
use packprune_pack::{
    ClientRules, Layout, LogicalPath, PackInput, PackMeta, PhysPath, RunCx, VanillaIndex,
};

#[cfg(debug_assertions)]
use crate::apply::assert_rebuilt;
use crate::apply::{Current, Refusal, apply, redigest};
use crate::{Loss, LossRecord, Pass, PassCx, PassError, Remap, Scope, SubCache};

/// What stays the same for every pass of a run.
pub struct Context<'a> {
    pub run: &'a RunCx,
    pub vanilla: &'a VanillaIndex,
    pub rules: &'a ClientRules,
    /// What the usage sources named, for every analysis of the run (the prunes and what each pass reads).
    pub sources: &'a [SourceUsages],
    pub selectors: &'a [UnusedSelector],
    pub cache: &'a dyn SubCache,
}

/// A pack with its layout, its reference index and what its analysis reaches.
#[derive(Clone, Debug)]
pub struct Analysed {
    pub pack: PackInput,
    pub layout: Layout,
    pub index: ReferenceIndex,
    pub reached: Reached,
}

/// The names no pass may change, from the analysis of the pack before its first prune. `pinned` also takes what the
/// run keeps by path (`force_include`).
#[derive(Clone, Debug, Default)]
pub struct Kept {
    pub names: BTreeSet<LogicalPath>,
    pub pinned: BTreeSet<PhysPath>,
}

impl Kept {
    pub fn of(state: &Analysed) -> Self {
        Self {
            names: state.reached.kept_names(&state.layout),
            pinned: state.reached.pinned_paths(),
        }
    }
}

/// A pass enabled for a run: on `scope`, with its validated options.
pub struct Enabled<'a> {
    pub pass: &'a dyn Pass,
    pub scope: Scope,
    pub options: toml::Table,
}

/// What one pass did: file counts and bytes before and after, the paths whose bytes it changed (references it
/// made the framework rewrite included), its report lines and measured loss.
#[derive(Clone, Debug, PartialEq)]
pub struct PassReport {
    pub pass: &'static str,
    pub loss: Loss,
    pub files_before: usize,
    pub files_after: usize,
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub changed: Vec<PhysPath>,
    pub changes: Vec<String>,
    pub measured: Vec<LossRecord>,
}

/// The pack the passes left, their remaps composed in run order, one report per pass.
#[derive(Clone, Debug)]
pub struct Outcome {
    pub state: Analysed,
    pub remap: Remap,
    pub reports: Vec<PassReport>,
}

impl Outcome {
    pub fn changed(&self) -> bool {
        self.reports.iter().any(|report| !report.changed.is_empty())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("pass {pass}: refused: {refusal}")]
    Refused {
        pass: &'static str,
        refusal: Refusal,
    },
    #[error(transparent)]
    Pass(#[from] PassError),
    #[error(transparent)]
    Meta(#[from] MetaError),
    #[error(transparent)]
    Analysis(#[from] Refused),
    /// A lossless pass changed what the player sees: the harness fails by the passes' names.
    #[error("{}: the oracle found {} keys that differ", passes.join(", "), comparison.differing_keys)]
    NotEquivalent {
        passes: Vec<&'static str>,
        comparison: Comparison,
    },
}

impl Context<'_> {
    /// `pack` read whole: its layout, its index built, its analysis.
    pub fn analyse(&self, pack: PackInput) -> Result<Analysed, RunError> {
        let layout = Layout::new(&pack, PackMeta::read(&pack)?, self.vanilla);
        let (index, reached) = self.index_and_reached(&pack, &layout)?;
        Ok(Analysed {
            pack,
            layout,
            index,
            reached,
        })
    }

    /// The reference index of `pack` and what its analysis reaches, on `layout`, its own layout.
    pub fn index_and_reached(
        &self,
        pack: &PackInput,
        layout: &Layout,
    ) -> Result<(ReferenceIndex, Reached), RunError> {
        let index = ReferenceIndex::build(self.run, pack, layout, self.rules, self.cache.tokens());
        let reached = self.reached(pack, layout, &index)?;
        Ok((index, reached))
    }

    fn reached(
        &self,
        pack: &PackInput,
        layout: &Layout,
        index: &ReferenceIndex,
    ) -> Result<Reached, RunError> {
        Ok(Reached::analyse(&Inputs {
            cx: self.run,
            pack,
            layout,
            rules: self.rules,
            vanilla: self.vanilla,
            index,
            sources: self.sources,
            selectors: self.selectors,
            policy: Policy::Pack,
        })?)
    }

    /// `state` without the files its analysis finds removable, analysed again, and those files.
    pub fn prune(&self, state: Analysed) -> Result<(Analysed, BTreeSet<PhysPath>), RunError> {
        let dropped = state.reached.removable();
        if dropped.is_empty() {
            return Ok((state, dropped));
        }
        let Analysed {
            mut pack,
            mut layout,
            mut index,
            ..
        } = state;
        for path in &dropped {
            pack.files.remove(path);
        }
        // `pack.mcmeta` is a root, so the views stay those of `layout.meta`.
        layout.update(&pack, &dropped, self.vanilla);
        redigest(&mut pack);
        index.update(
            self.run,
            &pack,
            &layout,
            self.rules,
            &Change {
                touched: dropped.clone(),
                ..Default::default()
            },
            self.cache.tokens(),
        );
        #[cfg(debug_assertions)]
        assert_rebuilt(self, &pack, &layout, &index);
        let reached = self.reached(&pack, &layout, &index)?;
        Ok((
            Analysed {
                pack,
                layout,
                index,
                reached,
            },
            dropped,
        ))
    }
}

/// Sorts `passes`: lossless passes before lossy ones, then by stage, then by name.
pub fn order(passes: &mut [Enabled<'_>]) {
    passes.sort_by(|a, b| {
        (a.pass.loss(), a.pass.stage(), a.pass.name()).cmp(&(
            b.pass.loss(),
            b.pass.stage(),
            b.pass.name(),
        ))
    });
}

/// Runs `passes` in the order given on `state`, each on what the previous one left. A pass's own work may be
/// parallel; applying its edits is sequential and ordered, so the output does not depend on the thread count. No
/// pass reads an analysis, so the pack is analysed once, after the last pass that changed a byte.
pub fn run(
    cx: &Context<'_>,
    kept: &Kept,
    state: &Analysed,
    passes: &[Enabled<'_>],
) -> Result<Outcome, RunError> {
    // The input stays the caller's: the passes work on one copy of it, made when the first pass runs.
    let mut owned: Option<Current> = None;
    let mut changed = false;
    let mut remap = Remap::default();
    let mut reports = Vec::with_capacity(passes.len());
    for enabled in passes {
        let pass = enabled.pass;
        let (pack, layout, index) = match &owned {
            Some(current) => (&current.pack, &current.layout, &current.index),
            None => (&state.pack, &state.layout, &state.index),
        };
        let edits = pass.run(&PassCx {
            run: cx.run,
            pack,
            layout,
            index,
            vanilla: cx.vanilla,
            rules: cx.rules,
            kept_names: &kept.names,
            pinned: &kept.pinned,
            scope: &enabled.scope,
            options: &enabled.options,
            cache: cx.cache,
        })?;
        let (files_before, bytes_before) = (pack.files.len(), bytes(pack));
        let current = owned.get_or_insert_with(|| Current {
            pack: state.pack.clone(),
            layout: state.layout.clone(),
            index: state.index.clone(),
        });
        let applied = apply(cx, kept, current, pass.name(), &enabled.scope, &edits)?;
        changed |= !applied.changed.is_empty();
        remap = remap.then(&applied.remap);
        reports.push(PassReport {
            pass: pass.name(),
            loss: pass.loss(),
            files_before,
            files_after: current.pack.files.len(),
            bytes_before,
            bytes_after: bytes(&current.pack),
            changed: applied.changed,
            changes: edits.changes,
            measured: edits.loss,
        });
    }
    let state = match owned.filter(|_| changed) {
        Some(current) => Analysed {
            reached: cx.reached(&current.pack, &current.layout, &current.index)?,
            pack: current.pack,
            layout: current.layout,
            index: current.index,
        },
        None => state.clone(),
    };
    Ok(Outcome {
        state,
        remap,
        reports,
    })
}

fn bytes(pack: &PackInput) -> u64 {
    pack.files
        .values()
        .map(|entry| entry.blob.len() as u64)
        .sum()
}
