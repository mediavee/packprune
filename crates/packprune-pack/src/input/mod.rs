use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;

use camino::Utf8Path;

use crate::{Limits, PhysPath};

mod archive;
mod directory;
mod open;
#[cfg(any(not(unix), test))]
mod portable;
#[cfg(test)]
mod tests;

pub type Blob = Arc<[u8]>;

/// blake3 of a file's bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Digest(pub [u8; 32]);

#[derive(Clone, Debug)]
pub struct Entry {
    pub blob: Blob,
    pub digest: Digest,
}

/// A pack read from a directory or a zip: the only reader of the engine, self-check, drop-in and bench.
#[derive(Clone, Debug)]
pub struct PackInput {
    pub name: String,
    pub files: BTreeMap<PhysPath, Entry>,
    /// blake3 over the sorted paths and entry digests: the content only, so a zip and its extraction agree.
    pub digest: Digest,
}

#[derive(Debug, thiserror::Error)]
pub enum InputError {
    #[error("{entry}: {why}")]
    Refused { entry: String, why: String },
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl PackInput {
    /// Reads a directory or a zip whole, refusing any entry that breaks the path rules or `limits`.
    pub fn open(path: &Utf8Path, limits: &Limits) -> Result<Self, InputError> {
        let files = if std::fs::metadata(path)?.is_dir() {
            directory::read(path, limits)?
        } else {
            archive::read(path, limits)?
        };
        let mut pack = blake3::Hasher::new();
        for (path, entry) in &files {
            pack.update(path.as_str().as_bytes())
                .update(&[0])
                .update(&entry.digest.0);
        }
        Ok(Self {
            name: path.file_name().unwrap_or(path.as_str()).to_owned(),
            files,
            digest: Digest(*pack.finalize().as_bytes()),
        })
    }
}

impl Entry {
    fn new(bytes: Vec<u8>) -> Self {
        Self {
            digest: Digest(*blake3::hash(&bytes).as_bytes()),
            blob: bytes.into(),
        }
    }
}

fn refused(entry: &str, why: impl ToString) -> InputError {
    InputError::Refused {
        entry: entry.to_owned(),
        why: why.to_string(),
    }
}
