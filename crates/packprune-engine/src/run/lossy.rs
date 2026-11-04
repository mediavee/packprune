//! The lossy passes, one at a time: each pass's scope matches the input paths through the remap of every pass that
//! ran before it, lossy ones included (audio-to-ogg moves a `.wav` to the `.ogg` a later scope is checked on).

use std::sync::Arc;

use packprune_oracle::Remap;
use packprune_pack::PhysPath;
use packprune_pass::{Analysed, Context, Enabled, Kept, Outcome, RunError, order};
use packprune_passes::sounds::Targets;

use super::options::Selection;
use super::origins::Origins;

/// `selection`'s lossy passes on `state`; `start` are the paths the passes started from and `before` the remap of the
/// lossless stage.
pub(super) fn run(
    cx: &Context<'_>,
    kept: &Kept,
    state: &Analysed,
    selection: &Selection<'_>,
    start: &[PhysPath],
    before: &Remap,
) -> Result<Outcome, RunError> {
    let mut owned: Option<Analysed> = None;
    let mut remap = Remap::default();
    let mut reports = Vec::new();
    // The list is rebuilt before each pass with scopes over the provenance so far; its order does not depend on them.
    for at in 0.. {
        let origins = Arc::new(Origins::new(start, &before.then(&remap)));
        let mut passes = selection.lossy_passes(&origins);
        let audio = Targets::take(&mut passes)?;
        if let Some(audio) = &audio {
            passes.push(Enabled {
                pass: audio,
                scope: audio.scope(),
                options: toml::Table::new(),
            });
        }
        order(&mut passes);
        let Some(enabled) = passes.get(at) else {
            break;
        };
        let current = owned.as_ref().unwrap_or(state);
        let step = packprune_pass::run(cx, kept, current, std::slice::from_ref(enabled))?;
        remap = remap.then(&step.remap);
        reports.extend(step.reports.iter().cloned());
        if step.changed() {
            owned = Some(step.state);
        }
    }
    Ok(Outcome {
        state: owned.unwrap_or_else(|| state.clone()),
        remap,
        reports,
    })
}
