//! Reading the input and writing the output.

use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;
use std::time::Instant;

use camino::Utf8Path;
use packprune_pack::{Blob, InputError, Limits, PackInput};
use packprune_zip::{EntryClass, WriteOptions, Written, ZipError};

#[derive(Debug, thiserror::Error)]
pub enum IoError {
    /// The input or the output layout broke a rule; `entry` names the offender.
    #[error("{entry}: {why}")]
    Refused { entry: String, why: String },
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl From<InputError> for IoError {
    fn from(error: InputError) -> Self {
        match error {
            InputError::Refused { entry, why } => Self::Refused { entry, why },
            InputError::Io(error) => Self::Io(error),
        }
    }
}

impl From<ZipError> for IoError {
    fn from(error: ZipError) -> Self {
        match error {
            ZipError::Refused { entry, why } => Self::Refused { entry, why },
            ZipError::Io(error) => Self::Io(error),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct RunOptions {
    pub limits: Limits,
    pub zip: WriteOptions,
    /// Size of the worker pool; `None` uses rayon's default.
    pub threads: Option<usize>,
}

#[derive(Clone, Copy, Debug)]
pub struct Repacked {
    pub entries: usize,
    pub written: Written,
}

/// The minimal run: reads `input` through `PackInput` and writes the same entries to `output`.
pub fn repack(
    input: &Utf8Path,
    output: &Utf8Path,
    options: &RunOptions,
) -> Result<Repacked, IoError> {
    let run = || -> Result<Repacked, IoError> {
        let started = Instant::now();
        let pack = PackInput::open(input, &options.limits)?;
        tracing::info!(entries = pack.files.len(), elapsed = ?started.elapsed(), "read");
        let started = Instant::now();
        let written = write(&pack, output, &options.zip)?;
        tracing::info!(bytes = written.bytes, elapsed = ?started.elapsed(), "write");
        Ok(Repacked {
            entries: pack.files.len(),
            written,
        })
    };
    match options.threads {
        Some(threads) => rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .map_err(io::Error::other)?
            .install(run),
        None => run(),
    }
}

/// Writes every file of `pack` to `out`: entries compressed in parallel, then assembled in one pass.
pub fn write(pack: &PackInput, out: &Utf8Path, options: &WriteOptions) -> Result<Written, IoError> {
    let files = pack
        .files
        .iter()
        .map(|(path, entry)| (path.as_str().to_owned(), Arc::clone(&entry.blob)))
        .collect();
    write_named(&files, out, options)
}

fn write_named(
    files: &BTreeMap<String, Blob>,
    out: &Utf8Path,
    options: &WriteOptions,
) -> Result<Written, IoError> {
    let entries = files
        .iter()
        .map(|(name, bytes)| (name.clone(), (Arc::clone(bytes), entry_class(name))))
        .collect();
    Ok(packprune_zip::write(&entries, out, options)?)
}

/// How the compressor treats an entry, from its file name alone; the zip crate knows no extensions.
fn entry_class(name: &str) -> EntryClass {
    let file = name.rsplit('/').next().unwrap_or(name);
    let Some((_, extension)) = file.rsplit_once('.') else {
        return EntryClass::Other;
    };
    const TEXT: &[&str] = &[
        "json",
        "mcmeta",
        "properties",
        "fsh",
        "vsh",
        "glsl",
        "txt",
        "lang",
        "mcfunction",
    ];
    const COMPRESSED: &[&str] = &["png", "ogg", "jpg", "jpeg", "nbt", "zip", "jar"];
    let is = |list: &[&str]| {
        list.iter()
            .any(|known| extension.eq_ignore_ascii_case(known))
    };
    if is(TEXT) {
        EntryClass::Text
    } else if is(COMPRESSED) {
        EntryClass::Compressed
    } else {
        EntryClass::Other
    }
}

#[cfg(test)]
mod tests;
