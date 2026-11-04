//! The paths the server knows each file by. A profile's globs (lossy scopes, per-file overrides) are written against
//! the paths of the input; by the lossy stage and the file optimisers, short-names, identical-sprites or
//! overlay-merge may have renamed or merged a file, so every check of a user glob goes through here.

use std::collections::{BTreeMap, BTreeSet};

use packprune_config::{Options, Overrides};
use packprune_oracle::Remap;
use packprune_pack::PhysPath;

/// Each current path with the input paths the run's remap sends to it.
#[derive(Debug, Default)]
pub(crate) struct Origins {
    by_current: BTreeMap<PhysPath, BTreeSet<PhysPath>>,
}

impl Origins {
    /// `start`: the paths the passes started from; `remap`: what the passes since then renamed, moved or merged.
    pub(crate) fn new<'a>(start: impl IntoIterator<Item = &'a PhysPath>, remap: &Remap) -> Self {
        let mut by_current: BTreeMap<PhysPath, BTreeSet<PhysPath>> = BTreeMap::new();
        for path in start {
            by_current
                .entry(remap.physical(path))
                .or_default()
                .insert(path.clone());
        }
        Self { by_current }
    }

    /// The input paths of `current`; a file a pass created with no remap to it is known by its own path.
    fn of<'a>(&'a self, current: &'a PhysPath) -> Box<dyn Iterator<Item = &'a PhysPath> + 'a> {
        match self.by_current.get(current) {
            Some(origins) => Box::new(origins.iter()),
            None => Box::new(std::iter::once(current)),
        }
    }

    /// True when `test` accepts every path the file was known by: a file merged from an excluded one stays out.
    pub(crate) fn all(&self, current: &PhysPath, test: impl Fn(&str) -> bool) -> bool {
        self.of(current).all(|path| test(path.as_str()))
    }

    /// The override options of `kind` for `current`, read at its input paths. `None` when they disagree: the file
    /// was several files whose settings differ, and no single table is the server's.
    pub(crate) fn options(
        &self,
        overrides: &Overrides<'_>,
        kind: &str,
        current: &PhysPath,
    ) -> Option<Options> {
        let mut tables = self
            .of(current)
            .map(|path| overrides.options_for(kind, path.as_str()));
        let first = tables.next().expect("a file has at least one path");
        tables.all(|table| table == first).then_some(first)
    }
}
