//! Which graph passes and file optimisers a profile enables, with their scopes and options, and the reading of the
//! config's option tables by their owners when the config is loaded.

use std::collections::BTreeSet;
use std::sync::Arc;

use globset::{GlobSet, GlobSetBuilder};
use packprune_config::{Options, OptionsCheck, OptionsOwner, PassSelection, Profile};
use packprune_pass::{Enabled, FileOptimizer, Loss, Pass, Scope};
use serde::Deserialize;

use super::RunError;
use super::origins::Origins;

/// The override kind whose `force_include` keeps a file by path.
pub(crate) const CUSTOM: &str = "custom";
const OVERLAY_FLOOR: &str = "overlay-floor";

/// Every graph pass and file optimiser this build has (`packprune_passes`).
pub struct Registry {
    pub(crate) passes: Vec<Box<dyn Pass>>,
    pub(crate) optimisers: Vec<Box<dyn FileOptimizer>>,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            passes: packprune_passes::passes(),
            optimisers: packprune_passes::optimisers(),
        }
    }
}

/// A file optimiser enabled for a run. A lossless one reads its options per file from the profile's overrides; a
/// lossy one runs on its globs with its `lossy.<name>` table.
pub(crate) struct Optimiser<'a> {
    pub optimiser: &'a dyn FileOptimizer,
    pub lossy: Option<(GlobSet, toml::Table)>,
}

/// A lossy graph pass with its globs, scoped once the run knows what the lossless passes renamed.
pub(crate) struct LossyPass<'a> {
    pass: &'a dyn Pass,
    globs: GlobSet,
    options: toml::Table,
}

pub(crate) struct Selection<'a> {
    /// The lossless graph passes, on every path.
    pub passes: Vec<Enabled<'a>>,
    pub lossy: Vec<LossyPass<'a>>,
    pub optimisers: Vec<Optimiser<'a>>,
}

impl<'a> Selection<'a> {
    /// The lossy passes, each on the files whose every input path its globs accept (`origins`).
    pub(crate) fn lossy_passes(&self, origins: &Arc<Origins>) -> Vec<Enabled<'a>> {
        self.lossy
            .iter()
            .map(|lossy| {
                let (globs, origins) = (lossy.globs.clone(), Arc::clone(origins));
                Enabled {
                    pass: lossy.pass,
                    scope: Scope::matching(move |path| {
                        origins.all(path, |origin| globs.is_match(origin))
                    }),
                    options: lossy.options.clone(),
                }
            })
            .collect()
    }
}

impl Registry {
    fn names(&self, loss: Loss) -> Vec<&'static str> {
        let passes = self.passes.iter().filter(|pass| pass.loss() == loss);
        let optimisers = self.optimisers.iter().filter(|o| o.loss() == loss);
        passes
            .map(|pass| pass.name())
            .chain(optimisers.map(|optimiser| optimiser.name()))
            .collect()
    }

    fn loss_of(&self, name: &str) -> Option<Loss> {
        let pass = self.passes.iter().find(|pass| pass.name() == name);
        let optimiser = self.optimisers.iter().find(|o| o.name() == name);
        pass.map(|pass| pass.loss())
            .or_else(|| optimiser.map(|optimiser| optimiser.loss()))
    }

    /// `options` read by the pass or optimiser `name` of that loss, as it reads them when it runs.
    fn validate(&self, name: &str, loss: Loss, options: &Options) -> Result<(), String> {
        let table = toml_table(options)?;
        let pass = self
            .passes
            .iter()
            .find(|pass| pass.name() == name && pass.loss() == loss);
        let optimiser = self
            .optimisers
            .iter()
            .find(|o| o.name() == name && o.loss() == loss);
        match (pass, optimiser) {
            (Some(pass), _) => pass.validate_options(&table),
            (None, Some(optimiser)) => optimiser.validate_options(&table),
            (None, None) => Err(format!("no {name} pass or optimiser")),
        }
    }

    /// `passes` (default `lossless`: every lossless pass and optimiser; `none`; or names) less `disable`, and each
    /// `lossy.<name>` table on its globs.
    pub(crate) fn select(&self, profile: &Profile) -> Result<Selection<'_>, RunError> {
        let mut names: BTreeSet<&str> = match &profile.passes {
            None => self.names(Loss::Lossless).into_iter().collect(),
            Some(PassSelection::Preset(preset)) => match preset.as_str() {
                "lossless" => self.names(Loss::Lossless).into_iter().collect(),
                "none" => BTreeSet::new(),
                other => {
                    return Err(refused(format!(
                        "passes = {other:?}: expected \"lossless\", \"none\" or a list of names"
                    )));
                }
            },
            Some(PassSelection::Names(list)) => list
                .iter()
                .map(|name| match self.loss_of(name) {
                    Some(Loss::Lossless) => Ok(name.as_str()),
                    Some(Loss::Lossy) => Err(refused(format!(
                        "passes: {name} is lossy: enable it with a lossy.{name} table and its globs"
                    ))),
                    None => Err(self.unknown("passes", name)),
                })
                .collect::<Result<_, _>>()?,
        };
        for name in &profile.disable {
            if self.loss_of(name).is_none() {
                return Err(self.unknown("disable", name));
            }
            names.remove(name.as_str());
        }
        let mut lossy = Vec::new();
        for (name, table) in &profile.lossy {
            if self.loss_of(name) != Some(Loss::Lossy) {
                let known = self.names(Loss::Lossy);
                return Err(refused(format!(
                    "lossy.{name}: expected a lossy pass or optimiser ({})",
                    if known.is_empty() {
                        "this build has none".into()
                    } else {
                        known.join(", ")
                    }
                )));
            }
            if profile.disable.contains(name) {
                continue;
            }
            let mut globs = GlobSetBuilder::new();
            for pattern in &table.globs {
                globs.add(
                    packprune_config::glob(pattern)
                        .map_err(|error| refused(format!("lossy.{name}.globs: {error}")))?,
                );
            }
            let globs = globs
                .build()
                .map_err(|error| refused(format!("lossy.{name}.globs: {error}")))?;
            let options = toml_table(&table.options)
                .map_err(|why| refused(format!("lossy.{name}: {why}")))?;
            lossy.push((name.as_str(), globs, options));
        }
        // `oldest_format`: overlay-floor on every path, at the profile's floor.
        if let Some(oldest) = profile.oldest_format
            && !profile.disable.iter().any(|name| name == OVERLAY_FLOOR)
        {
            if profile.lossy.contains_key(OVERLAY_FLOOR) {
                return Err(refused(format!(
                    "oldest_format and lossy.{OVERLAY_FLOOR} both set the floor: keep one"
                )));
            }
            if self.loss_of(OVERLAY_FLOOR) != Some(Loss::Lossy) {
                return Err(refused(format!(
                    "oldest_format: this build has no {OVERLAY_FLOOR} pass"
                )));
            }
            let every = GlobSetBuilder::new()
                .add(packprune_config::glob("**").expect("a valid glob"))
                .build()
                .expect("one valid glob");
            let options = toml::Table::from_iter([(
                "oldest_format".to_owned(),
                toml::Value::Integer(oldest.into()),
            )]);
            lossy.push((OVERLAY_FLOOR, every, options));
        }

        let mut passes = Vec::new();
        let mut lossy_passes = Vec::new();
        for pass in &self.passes {
            if pass.loss() == Loss::Lossless && names.contains(pass.name()) {
                passes.push(Enabled {
                    pass: pass.as_ref(),
                    scope: Scope::all(),
                    options: toml::Table::new(),
                });
            }
            if let Some((_, globs, options)) = lossy.iter().find(|(name, ..)| *name == pass.name())
            {
                lossy_passes.push(LossyPass {
                    pass: pass.as_ref(),
                    globs: globs.clone(),
                    options: options.clone(),
                });
            }
        }
        let mut optimisers = Vec::new();
        for optimiser in &self.optimisers {
            if optimiser.loss() == Loss::Lossless
                && names.contains(optimiser.name())
                && profile.kind_enabled(optimiser.name())
            {
                optimisers.push(Optimiser {
                    optimiser: optimiser.as_ref(),
                    lossy: None,
                });
            }
            if let Some((_, globs, options)) =
                lossy.iter().find(|(name, ..)| *name == optimiser.name())
            {
                optimisers.push(Optimiser {
                    optimiser: optimiser.as_ref(),
                    lossy: Some((globs.clone(), options.clone())),
                });
            }
        }
        Ok(Selection {
            passes,
            lossy: lossy_passes,
            optimisers,
        })
    }

    fn unknown(&self, key: &str, name: &str) -> RunError {
        let mut known = self.names(Loss::Lossless);
        known.extend(self.names(Loss::Lossy));
        refused(format!(
            "{key}: unknown pass {name}, expected one of {}",
            known.join(", ")
        ))
    }
}

/// `custom = { force_include = true }`: a file the run keeps by path, as a root of every analysis.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Custom {
    pub force_include: bool,
}

/// The `custom` options of an override, as the config is checked and as the run reads them.
pub(crate) fn custom(options: &Options) -> Result<Custom, String> {
    let value = serde_json::to_value(options).map_err(|error| error.to_string())?;
    serde_json::from_value(value).map_err(|error| error.to_string())
}

pub(crate) fn toml_table(options: &Options) -> Result<toml::Table, String> {
    match toml::Value::try_from(options) {
        Ok(toml::Value::Table(table)) => Ok(table),
        Ok(_) => unreachable!("a map serialises to a table"),
        Err(error) => Err(error.to_string()),
    }
}

fn refused(why: String) -> RunError {
    RunError::Refused(why)
}

impl OptionsCheck for Registry {
    fn check_owner(&self, owner: OptionsOwner<'_>) -> Result<(), String> {
        let kinds = || {
            let mut kinds: Vec<&str> = self
                .optimisers
                .iter()
                .filter(|o| o.loss() == Loss::Lossless)
                .map(|o| o.name())
                .collect();
            kinds.push(CUSTOM);
            kinds
        };
        match owner {
            OptionsOwner::LossyPass(name) if self.loss_of(name) == Some(Loss::Lossy) => Ok(()),
            OptionsOwner::LossyPass(_) => {
                let known = self.names(Loss::Lossy);
                Err(if known.is_empty() {
                    "expected a lossy pass or optimiser; this build has none".into()
                } else {
                    format!("expected a lossy pass or optimiser: {}", known.join(", "))
                })
            }
            OptionsOwner::Kind(kind) if kinds().contains(&kind) => Ok(()),
            OptionsOwner::Kind(_) => Err(format!("expected a file kind: {}", kinds().join(", "))),
        }
    }

    fn check_options(&self, owner: OptionsOwner<'_>, options: &Options) -> Result<(), String> {
        match owner {
            OptionsOwner::Kind(CUSTOM) => custom(options).map(drop),
            OptionsOwner::Kind(kind) => self.validate(kind, Loss::Lossless, options),
            OptionsOwner::LossyPass(name) => self.validate(name, Loss::Lossy, options),
        }
    }
}
