//! Zip reader with limits, deterministic writer, compression backends.

pub mod modes;

mod compression;
mod reader;
mod writer;

use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;

use camino::Utf8Path;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};

/// Input limits (`[input.limits]`), each refused with exit 2 and the entry named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_entries: u32,
    pub max_total_bytes: u64,
    pub max_entry_bytes: u64,
    pub max_ratio: u32,
    pub ratio_min_entry_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_entries: 262_144,
            max_total_bytes: 2 << 30,
            max_entry_bytes: 512 << 20,
            max_ratio: 1_000,
            ratio_min_entry_bytes: 1 << 20,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ZipMode {
    #[default]
    Conformant,
    Lean,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryClass {
    Text,
    Compressed,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompressOptions {
    /// 0 stores, 1..=254 uses libdeflate-12, 255 also tries size-scaled Zopfli on text.
    pub effort: u8,
    pub recompress_compressed: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LayoutOptions {
    pub mode: ZipMode,
    pub comment: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Encoded {
    /// ZIP method 0 (stored) or 8 (deflate).
    pub method: u16,
    pub crc: u32,
    pub size: u64,
    pub payload: Arc<[u8]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteOptions {
    pub mode: ZipMode,
    /// 0 stores, 1..=254 uses libdeflate-12, 255 also tries size-scaled Zopfli on text.
    pub effort: u8,
    pub recompress_compressed: bool,
    pub comment: Option<String>,
}

/// What the writer reports of the zip it wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Written {
    pub sha1: [u8; 20],
    pub bytes: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum ZipError {
    #[error("{entry}: {why}")]
    Refused { entry: String, why: String },
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Reads every entry through the central directory, within `limits`.
pub fn read(path: &Utf8Path, limits: &Limits) -> Result<BTreeMap<String, Vec<u8>>, ZipError> {
    reader::read(path, limits, &|_| true)
}

/// [`read`], inflating only the entries `select` takes: every entry is still checked against `limits`.
pub fn read_selected(
    path: &Utf8Path,
    limits: &Limits,
    select: &dyn Fn(&str) -> bool,
) -> Result<BTreeMap<String, Vec<u8>>, ZipError> {
    reader::read(path, limits, select)
}

pub fn encode(bytes: &[u8], class: EntryClass, options: &CompressOptions) -> Encoded {
    let compressed = compression::compress(bytes, class, options);
    Encoded {
        method: if compressed.is_some() { 8 } else { 0 },
        crc: crc32fast::hash(bytes),
        size: bytes.len() as u64,
        payload: compressed.map_or_else(|| Arc::from(bytes), Arc::from),
    }
}

/// Publishes encoded entries through `<out>.partial`, replacing an interrupted partial write.
pub fn assemble(
    entries: &BTreeMap<String, Encoded>,
    out: &Utf8Path,
    layout: &LayoutOptions,
) -> Result<Written, ZipError> {
    writer::assemble(entries, out, layout)
}

pub fn write(
    entries: &BTreeMap<String, (Arc<[u8]>, EntryClass)>,
    out: &Utf8Path,
    options: &WriteOptions,
) -> Result<Written, ZipError> {
    let compression = CompressOptions {
        effort: options.effort,
        recompress_compressed: options.recompress_compressed,
    };
    let encoded = entries
        .par_iter()
        .map(|(name, (bytes, class))| (name.clone(), encode(bytes, *class, &compression)))
        .collect();
    assemble(
        &encoded,
        out,
        &LayoutOptions {
            mode: options.mode,
            comment: options.comment.clone(),
        },
    )
}

#[cfg(test)]
mod tests;

/// Efforts 1 to 254 all deflate with libdeflate's strongest level, 12: the default names it.
pub const DEFAULT_EFFORT: u8 = 12;

impl Default for CompressOptions {
    fn default() -> Self {
        Self {
            effort: DEFAULT_EFFORT,
            recompress_compressed: true,
        }
    }
}

impl Default for WriteOptions {
    fn default() -> Self {
        Self {
            mode: ZipMode::Conformant,
            effort: DEFAULT_EFFORT,
            recompress_compressed: true,
            comment: None,
        }
    }
}

fn refuse(entry: &str, why: impl Into<String>) -> ZipError {
    ZipError::Refused {
        entry: entry.to_owned(),
        why: why.into(),
    }
}

/// The pack path rules of every input: UTF-8 `/`-separated, no leading `/`, `\`, NUL, drive prefix, or empty,
/// `.` or `..` segment. One trailing `/` (a directory marker) is allowed.
pub fn validate_path(name: &str) -> Result<(), ZipError> {
    let path = name.strip_suffix('/').unwrap_or(name);
    let drive = name.as_bytes().get(1) == Some(&b':') && name.as_bytes()[0].is_ascii_alphabetic();
    if path.is_empty()
        || name.starts_with('/')
        || name.contains(['\\', '\0'])
        || drive
        || path.split('/').any(|part| matches!(part, "" | "." | ".."))
    {
        return Err(refuse(name, "unsafe path"));
    }
    Ok(())
}
