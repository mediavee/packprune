use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read};
use std::path::Path;

use camino::Utf8Path;
use rayon::iter::{IntoParallelIterator, ParallelIterator};

use super::open::{Opener, Root, context};
use super::{Entry, InputError, refused};
use crate::{Limits, PhysPath};

pub(super) fn read(
    root: &Utf8Path,
    limits: &Limits,
) -> Result<BTreeMap<PhysPath, Entry>, InputError> {
    let root_dir = Root::open(root.as_std_path())?;
    let found = walk(root.as_std_path(), limits)?;
    read_files(&root_dir, found)
}

/// Lists the files of `root` with their sizes without following symlinks, and checks every limit on what it saw.
/// Children are visited in name order so the first refusal is the same on every run.
pub(super) fn walk(root: &Path, limits: &Limits) -> Result<BTreeMap<PhysPath, u64>, InputError> {
    let mut walk = Walk {
        limits,
        found: BTreeMap::new(),
        total: 0,
    };
    walk.directory(root, "")?;
    Ok(walk.found)
}

/// Reads the walked files in parallel, each opened through `root` and refused if it is no longer what the walk saw.
pub(super) fn read_files(
    root: &impl Opener,
    found: BTreeMap<PhysPath, u64>,
) -> Result<BTreeMap<PhysPath, Entry>, InputError> {
    let found: Vec<_> = found.into_iter().collect();
    let entries: Vec<_> = found
        .into_par_iter()
        .map(|(path, size)| {
            let bytes = load(root, &path, size)?;
            Ok((path, Entry::new(bytes)))
        })
        .collect();
    entries.into_iter().collect()
}

struct Walk<'a> {
    limits: &'a Limits,
    found: BTreeMap<PhysPath, u64>,
    total: u64,
}

impl Walk<'_> {
    /// `prefix` is the pack path of `dir` with its trailing `/`, empty for the root.
    fn directory(&mut self, dir: &Path, prefix: &str) -> Result<(), InputError> {
        let mut children = fs::read_dir(dir)?
            .map(|child| {
                let child = child?;
                // Does not follow symlinks.
                Ok((child.file_name(), child.metadata()?))
            })
            .collect::<io::Result<Vec<_>>>()?;
        children.sort_by(|(a, _), (b, _)| a.cmp(b));
        for (name, metadata) in children {
            let Some(name) = name.to_str() else {
                return Err(refused(
                    &format!("{prefix}{}", name.to_string_lossy()),
                    "path is not UTF-8",
                ));
            };
            let entry = format!("{prefix}{name}");
            let path = PhysPath::new(&entry).map_err(|error| refused(&entry, error))?;
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                return Err(refused(&entry, "symlink"));
            } else if file_type.is_dir() {
                self.directory(&dir.join(name), &format!("{entry}/"))?;
            } else if file_type.is_file() {
                self.file(path, metadata.len())?;
            } else {
                return Err(refused(&entry, "not a regular file"));
            }
        }
        Ok(())
    }

    fn file(&mut self, path: PhysPath, size: u64) -> Result<(), InputError> {
        let entry = path.as_str();
        if self.found.len() >= self.limits.max_entries as usize {
            return Err(refused(entry, "entry count limit exceeded"));
        }
        if size > self.limits.max_entry_bytes {
            return Err(refused(entry, "entry size limit exceeded"));
        }
        self.total = self
            .total
            .checked_add(size)
            .filter(|&total| total <= self.limits.max_total_bytes)
            .ok_or_else(|| refused(entry, "total size limit exceeded"))?;
        if self.found.contains_key(&path) {
            return Err(refused(entry, "duplicate path"));
        }
        self.found.insert(path, size);
        Ok(())
    }
}

pub(super) fn load(root: &impl Opener, path: &PhysPath, size: u64) -> Result<Vec<u8>, InputError> {
    read_exactly(root.open_regular(path, size)?, path, size)
}

/// Reads at most `size + 1` bytes, so a file that grew since the walk is refused rather than read whole.
pub(super) fn read_exactly(
    reader: impl Read,
    path: &PhysPath,
    size: u64,
) -> Result<Vec<u8>, InputError> {
    let mut bytes = Vec::with_capacity(usize::try_from(size).unwrap_or_default());
    reader
        .take(size.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| context(path, error))?;
    if bytes.len() as u64 != size {
        return Err(refused(path.as_str(), "size changed while reading"));
    }
    Ok(bytes)
}
