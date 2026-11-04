//! The output zip: entries compressed in parallel through the content cache (keyed on content, class and effort,
//! so a renamed file reuses its entry), assembled beside the output, published only once checked.

use std::collections::BTreeMap;
use std::sync::Arc;

use camino::{Utf8Path, Utf8PathBuf};
use packprune_config::Profile;
use packprune_pack::{FileKind, Layout, PackInput};
use packprune_zip::{
    CompressOptions, DEFAULT_EFFORT, Encoded, EntryClass, LayoutOptions, Written, ZipMode,
};
use rayon::prelude::*;

use super::RunError;
use super::cache::Cache;

#[derive(Clone, Debug)]
pub(crate) struct ZipOptions {
    pub compress: CompressOptions,
    pub layout: LayoutOptions,
}

/// The profile's `zip` table. Without a mode, `lean`: the default for packs served to players.
pub(crate) fn zip_options(profile: &Profile) -> Result<ZipOptions, RunError> {
    let zip = profile.zip.clone().unwrap_or_default();
    let mode = match zip.mode.as_deref() {
        None | Some("lean") => ZipMode::Lean,
        Some("conformant") => ZipMode::Conformant,
        Some(other) => {
            return Err(RunError::Refused(format!(
                "zip.mode = {other:?}: expected \"conformant\" or \"lean\""
            )));
        }
    };
    let effort = match &zip.effort {
        None => DEFAULT_EFFORT,
        Some(serde_json::Value::String(max)) if max == "max" => u8::MAX,
        Some(value) => value
            .as_u64()
            .and_then(|n| u8::try_from(n).ok())
            .ok_or_else(|| {
                RunError::Refused(format!("zip.effort = {value}: expected \"max\" or 0..255"))
            })?,
    };
    Ok(ZipOptions {
        compress: CompressOptions {
            effort,
            recompress_compressed: zip.recompress_compressed.unwrap_or(true),
        },
        layout: LayoutOptions {
            mode,
            comment: zip.comment.filter(|comment| !comment.is_empty()),
        },
    })
}

/// A zip written in a directory of its own beside the output: dropped with it unless published.
pub(crate) struct Staged {
    _dir: tempfile::TempDir,
    pub path: Utf8PathBuf,
    pub written: Written,
}

impl Staged {
    pub(crate) fn publish(self, output: &Utf8Path) -> std::io::Result<()> {
        std::fs::rename(&self.path, output)
    }
}

pub(crate) fn stage(
    pack: &PackInput,
    layout: &Layout,
    output: &Utf8Path,
    options: &ZipOptions,
    cache: &Cache,
) -> Result<Staged, RunError> {
    let parent = match output.parent() {
        Some(parent) if !parent.as_str().is_empty() => parent,
        _ => Utf8Path::new("."),
    };
    let dir = tempfile::Builder::new()
        .prefix(".packprune-")
        .tempdir_in(parent)?;
    let path = Utf8Path::from_path(dir.path())
        .ok_or_else(|| RunError::Refused(format!("{parent}: not a UTF-8 path")))?
        .join("output.zip");
    let entries: BTreeMap<String, Encoded> = pack
        .files
        .par_iter()
        .map(|(path, entry)| {
            let kind = layout.kinds.get(path).copied().unwrap_or(FileKind::Unknown);
            let class = kind.entry_class(path);
            let encoded = encode(&entry.blob, entry.digest.0, class, &options.compress, cache);
            (path.as_str().to_owned(), encoded)
        })
        .collect();
    let written =
        packprune_zip::assemble(&entries, &path, &options.layout).map_err(|error| match error {
            packprune_zip::ZipError::Refused { entry, why } => {
                RunError::Refused(format!("{entry}: {why}"))
            }
            packprune_zip::ZipError::Io(error) => RunError::Io(error),
        })?;
    Ok(Staged {
        _dir: dir,
        path,
        written,
    })
}

fn encode(
    bytes: &[u8],
    digest: [u8; 32],
    class: EntryClass,
    options: &CompressOptions,
    cache: &Cache,
) -> Encoded {
    let class_id = match class {
        EntryClass::Text => 0,
        EntryClass::Compressed => 1,
        EntryClass::Other => 2,
    };
    let key = [
        options.effort,
        u8::from(options.recompress_compressed),
        class_id,
    ];
    if let Some(encoded) = cache.get("zip-entry", &key, &[digest], |value| {
        decode(value, bytes.len())
    }) {
        return encoded;
    }
    let encoded = packprune_zip::encode(bytes, class, options);
    let mut value = Vec::with_capacity(14 + encoded.payload.len());
    value.extend(encoded.method.to_le_bytes());
    value.extend(encoded.crc.to_le_bytes());
    value.extend(encoded.size.to_le_bytes());
    value.extend(encoded.payload.iter());
    cache.put("zip-entry", &key, &[digest], &value);
    encoded
}

/// `None` for a value that does not describe an entry of `size` bytes: a miss.
fn decode(value: &[u8], size: usize) -> Option<Encoded> {
    let (method, rest) = value.split_at_checked(2)?;
    let (crc, rest) = rest.split_at_checked(4)?;
    let (stored, payload) = rest.split_at_checked(8)?;
    let encoded = Encoded {
        method: u16::from_le_bytes(method.try_into().ok()?),
        crc: u32::from_le_bytes(crc.try_into().ok()?),
        size: u64::from_le_bytes(stored.try_into().ok()?),
        payload: Arc::from(payload),
    };
    (matches!(encoded.method, 0 | 8) && encoded.size == size as u64).then_some(encoded)
}
