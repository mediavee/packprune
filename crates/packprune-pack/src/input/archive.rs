use std::collections::BTreeMap;

use camino::Utf8Path;
use rayon::iter::{IntoParallelIterator, ParallelIterator};

use super::{Entry, InputError, refused};
use crate::{Limits, PhysPath};

pub(super) fn read(
    path: &Utf8Path,
    limits: &Limits,
) -> Result<BTreeMap<PhysPath, Entry>, InputError> {
    let inflated: Vec<_> = packprune_zip::read(path, limits)
        .map_err(|error| match error {
            packprune_zip::ZipError::Refused { entry, why } => InputError::Refused { entry, why },
            packprune_zip::ZipError::Io(error) => InputError::Io(error),
        })?
        .into_iter()
        .collect();
    // Hashing dominates, so it runs in parallel; the collected order is the sorted one, so the first error is stable.
    let entries: Vec<_> = inflated
        .into_par_iter()
        .map(|(name, bytes)| {
            let path = PhysPath::new(&name).map_err(|error| refused(&name, error))?;
            Ok((path, Entry::new(bytes)))
        })
        .collect();
    entries.into_iter().collect()
}
