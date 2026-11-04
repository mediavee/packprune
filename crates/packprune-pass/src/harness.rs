//! The proof harness: passes run on a pack as an optimisation runs them, and the oracle compares what the player
//! sees. A lossless pass that changes it fails the harness by name; a lossy pass's differences are its measured loss.

use std::collections::BTreeSet;

use packprune_oracle::{Comparison, OracleInput, VanillaAssets, compare_with};
use packprune_pack::{PackInput, PhysPath};

use crate::run::{Analysed, Context, Enabled, Kept, PassReport, RunError, order, run};
use crate::{Loss, Remap};

/// What [`prove`] found. `proof` is the oracle between the pruned input and the lossless passes' re-pruned output,
/// `loss` between that and the lossy passes' re-pruned output; each is `None` when its passes changed nothing.
#[derive(Debug)]
pub struct Proof {
    pub output: Analysed,
    pub pruned: BTreeSet<PhysPath>,
    pub repruned: BTreeSet<PhysPath>,
    pub remap: Remap,
    pub reports: Vec<PassReport>,
    pub proof: Option<Comparison>,
    pub loss: Option<Comparison>,
}

/// Analyses and prunes `pack`; runs the lossless passes, a re-prune when they changed something, the
/// oracle against the pruned pack, refused on any difference; then the lossy passes on their scopes, a re-prune and
/// the oracle again, whose differences are the loss.
pub fn prove(
    cx: &Context<'_>,
    assets: &dyn VanillaAssets,
    pack: PackInput,
    mut passes: Vec<Enabled<'_>>,
) -> Result<Proof, RunError> {
    let analysed = cx.analyse(pack)?;
    let kept = Kept::of(&analysed);
    let (pruned_state, pruned) = cx.prune(analysed)?;
    order(&mut passes);
    let split = passes
        .iter()
        .position(|enabled| enabled.pass.loss() == Loss::Lossy)
        .unwrap_or(passes.len());
    let (lossless, lossy) = passes.split_at(split);
    let mut repruned = BTreeSet::new();

    let first = run(cx, &kept, &pruned_state, lossless)?;
    let mut reports = first.reports.clone();
    let (proved, proof) = if first.changed() {
        let (proved, dropped) = cx.prune(first.state)?;
        repruned.extend(dropped);
        let comparison = oracle(cx, assets, &kept, &pruned_state, &proved, &first.remap);
        if comparison.differing_keys > 0 {
            return Err(RunError::NotEquivalent {
                passes: lossless.iter().map(|enabled| enabled.pass.name()).collect(),
                comparison,
            });
        }
        (proved, Some(comparison))
    } else {
        (pruned_state, None)
    };

    let second = run(cx, &kept, &proved, lossy)?;
    reports.extend(second.reports.iter().cloned());
    let remap = first.remap.then(&second.remap);
    let (output, loss) = if second.changed() {
        let (output, dropped) = cx.prune(second.state)?;
        repruned.extend(dropped);
        let comparison = oracle(cx, assets, &kept, &proved, &output, &second.remap);
        (output, Some(comparison))
    } else {
        (proved, None)
    };
    Ok(Proof {
        output,
        pruned,
        repruned,
        remap,
        reports,
        proof,
        loss,
    })
}

fn oracle(
    cx: &Context<'_>,
    assets: &dyn VanillaAssets,
    kept: &Kept,
    before: &Analysed,
    after: &Analysed,
    remap: &Remap,
) -> Comparison {
    compare_with(
        cx.run,
        &input(before),
        &input(after),
        cx.vanilla,
        assets,
        remap,
        &|logical| kept.names.contains(logical),
    )
}

fn input(state: &Analysed) -> OracleInput<'_> {
    OracleInput {
        pack: &state.pack,
        layout: &state.layout,
        index: &state.index,
    }
}
