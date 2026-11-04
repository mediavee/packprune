use packprune_pass::{Edits, Enabled, Loss, Pass, PassCx, PassError, Scope, Stage};

use super::transcode::{Audio, Options, transcode};

pub(super) struct Selection {
    pub audio: Audio,
    pub scope: Scope,
    pub options: Options,
}

/// The Ogg selections of a run, merged into one target and one encode per file.
pub struct Targets {
    selections: Vec<Selection>,
}

impl Targets {
    pub fn take(passes: &mut Vec<Enabled<'_>>) -> Result<Option<Self>, PassError> {
        let selections: Vec<_> = passes
            .iter()
            .filter_map(|enabled| {
                Audio::of(enabled.pass.name()).map(|audio| {
                    audio
                        .options(&enabled.options)
                        .map(|options| Selection {
                            audio,
                            scope: enabled.scope.clone(),
                            options,
                        })
                        .map_err(|why| PassError::Refused {
                            pass: audio.name(),
                            why,
                        })
                })
            })
            .collect::<Result<_, _>>()?;
        if selections.len() < 2 {
            return Ok(None);
        }
        passes.retain(|enabled| Audio::of(enabled.pass.name()).is_none());
        Ok(Some(Self { selections }))
    }

    pub fn scope(&self) -> Scope {
        let scopes: Vec<_> = self.selections.iter().map(|s| s.scope.clone()).collect();
        Scope::matching(move |path| scopes.iter().any(|scope| scope.contains(path)))
    }
}

impl Pass for Targets {
    fn name(&self) -> &'static str {
        "squash-sounds"
    }
    fn stage(&self) -> Stage {
        Stage::Lossy
    }
    fn loss(&self) -> Loss {
        Loss::Lossy
    }
    fn options_schema(&self) -> schemars::Schema {
        schemars::json_schema!({"type": "object", "additionalProperties": false})
    }
    fn run(&self, cx: &PassCx<'_>) -> Result<Edits, PassError> {
        transcode(cx, &self.selections, self.name())
    }
}
