//! Content-addressed store `v1/<2 hex>/<62 hex>`.
//!
//! The store holds bytes and knows nothing of packs. A value that crosses runs is serialised with its strings: it
//! never holds a run-local `Sym`, and the caller re-interns it on load through its `RunCx`.

mod location;
#[cfg(test)]
mod tests;

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use camino::Utf8PathBuf;

pub use location::{ENV_VAR, env_override, executable_dir, resolve_dir};

const VERSION_DIR: &str = "v1";
const TEMP_PREFIX: &str = ".tmp-";
/// A temporary file older than this belongs to a writer that crashed.
const STALE_TEMP: Duration = Duration::from_secs(3600);

const MAGIC: [u8; 4] = *b"PPC1";
const HEADER_LEN: usize = MAGIC.len() + size_of::<u64>() + blake3::OUT_LEN;

/// `blake3(tool version ‖ step id ‖ canonical options ‖ input digests)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key(pub [u8; 32]);

impl Key {
    /// Every field is length-prefixed, so moving a byte from one field to the next changes the key.
    /// `options` is the step's canonical serialisation; the order of `inputs` is significant.
    pub fn derive(tool_version: &str, step_id: &str, options: &[u8], inputs: &[[u8; 32]]) -> Self {
        let mut hasher = blake3::Hasher::new_derive_key("packprune cache key v1");
        for field in [tool_version.as_bytes(), step_id.as_bytes(), options] {
            hasher.update(&(field.len() as u64).to_le_bytes());
            hasher.update(field);
        }
        hasher.update(&(inputs.len() as u64).to_le_bytes());
        for digest in inputs {
            hasher.update(digest);
        }
        Self(*hasher.finalize().as_bytes())
    }

    pub fn to_hex(&self) -> String {
        blake3::Hash::from_bytes(self.0).to_hex().to_string()
    }
}

#[derive(Debug)]
pub struct Store {
    pub dir: Utf8PathBuf,
    pub max_bytes: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl Store {
    /// A corrupt or absent entry is a miss.
    pub fn get(&self, key: &Key) -> Option<Vec<u8>> {
        let bytes = fs::read(self.entry_path(key)).ok()?;
        decode(&bytes).map(<[u8]>::to_vec)
    }

    /// Writes a temporary file and renames it, so a reader sees a whole entry or none. An existing entry, valid
    /// or not, is replaced.
    pub fn put(&self, key: &Key, value: &[u8]) -> Result<(), CacheError> {
        let path = self.entry_path(key);
        let shard = path.parent().expect("an entry path has a shard directory");
        fs::create_dir_all(shard)?;
        let mut temp = tempfile::Builder::new()
            .prefix(TEMP_PREFIX)
            .tempfile_in(shard)?;
        temp.write_all(&header(value))?;
        temp.write_all(value)?;
        // No fsync: after a power loss the renamed file may be short or empty, and the checksum makes it a miss.
        temp.persist(&path).map_err(|e| e.error)?;
        Ok(())
    }

    /// Least recently written first, down to `max_bytes`.
    pub fn evict(&self) -> Result<(), CacheError> {
        let now = SystemTime::now();
        let mut entries = Vec::new();
        let mut total = 0u64;
        for shard in read_dir(self.dir.join(VERSION_DIR).as_std_path())? {
            if !shard.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            for file in read_dir(&shard.path())? {
                let Some(name) = file.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                let Some(meta) = file.metadata().ok().filter(|m| m.is_file()) else {
                    continue;
                };
                let written = meta.modified()?;
                if is_entry_name(&name) {
                    total += meta.len();
                    entries.push((written, file.path(), meta.len()));
                } else if name.starts_with(TEMP_PREFIX)
                    && now
                        .duration_since(written)
                        .is_ok_and(|age| age > STALE_TEMP)
                {
                    remove(&file.path())?;
                }
            }
        }
        // The path breaks ties, so two entries with one timestamp always go in the same order.
        entries.sort();
        for (_, path, len) in entries {
            if total <= self.max_bytes {
                break;
            }
            remove(&path)?;
            total -= len;
        }
        Ok(())
    }

    fn entry_path(&self, key: &Key) -> PathBuf {
        let hex = key.to_hex();
        let (shard, rest) = hex.split_at(2);
        self.dir
            .join(VERSION_DIR)
            .join(shard)
            .join(rest)
            .into_std_path_buf()
    }
}

fn header(value: &[u8]) -> [u8; HEADER_LEN] {
    let mut header = [0; HEADER_LEN];
    header[..4].copy_from_slice(&MAGIC);
    header[4..12].copy_from_slice(&(value.len() as u64).to_le_bytes());
    header[12..].copy_from_slice(blake3::hash(value).as_bytes());
    header
}

/// The value of a whole entry; `None` for a short, long, foreign or altered file.
fn decode(bytes: &[u8]) -> Option<&[u8]> {
    let (head, value) = bytes.split_at_checked(HEADER_LEN)?;
    (head == header(value)).then_some(value)
}

fn is_entry_name(name: &str) -> bool {
    name.len() == 62 && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A directory another process removed meanwhile is an empty one.
fn read_dir(path: &Path) -> io::Result<Vec<fs::DirEntry>> {
    match fs::read_dir(path) {
        Ok(entries) => entries.collect(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// A file another process removed first is gone all the same.
fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}
