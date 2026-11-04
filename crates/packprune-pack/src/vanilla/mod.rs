use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::Path;

use camino::Utf8Path;
use serde::{Deserialize, Serialize};

use crate::Limits;
use crate::json;
use crate::kinds::ResourcePath;

#[cfg(test)]
mod tests;

const ATLASES: &str = "assets/minecraft/atlases/";

/// The asset names of a vanilla client jar, its version, and its atlas definitions (a pack's atlas file only adds
/// sources to vanilla's, so vanilla's directory sources decide what loads); the launcher's asset index adds the
/// sound file names and, when its `objects/` folder is beside it, the vanilla sound events.
///
/// It holds text only, no run-local `Sym`, so [`Self::to_bytes`] is the cached form whatever run made it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VanillaIndex {
    pub jar: Option<Input>,
    /// `version.json`'s `id`, `unknown` without one.
    pub version: String,
    /// `<resource_major>.<resource_minor>` of `version.json`, `unknown` without them.
    pub pack_format: String,
    /// Every `assets/` entry of the jar and every object of the asset index, as logical paths.
    pub names: BTreeSet<String>,
    /// The bytes of each `assets/minecraft/atlases/*.json` of the jar, by path.
    pub atlases: BTreeMap<String, Vec<u8>>,
    /// The `IHDR` size of each `assets/**.png` of the jar, by path; a file with no readable header is left out.
    pub textures: BTreeMap<String, TextureSize>,
    /// The bytes of each `assets/**.png.mcmeta` of the jar, by path.
    pub texture_meta: BTreeMap<String, Vec<u8>>,
    pub asset_index: Option<Input>,
    /// True once an asset index added the sound file names.
    pub sound_files: bool,
    /// The vanilla `sounds.json`'s event names, when the asset index's objects were there to read it.
    pub sound_events: Option<BTreeSet<String>>,
}

/// Width and height in texels of a PNG, read from its `IHDR`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextureSize {
    pub width: u32,
    pub height: u32,
}

impl TextureSize {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

    /// The size a PNG declares, or `None` when it does not start with a signature and an `IHDR` of non-zero size.
    pub fn of_png(png: &[u8]) -> Option<Self> {
        let field = |at: usize| {
            png.get(at..at + 4)
                .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        };
        let (width, height) = (field(16)?, field(20)?);
        (png.starts_with(Self::SIGNATURE)
            && png.get(12..16) == Some(b"IHDR")
            && width > 0
            && height > 0)
            .then_some(Self { width, height })
    }
}

/// A file the index was built from: its name and the hex blake3 of its bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Input {
    pub name: String,
    pub blake3: String,
}

#[derive(Debug, thiserror::Error)]
pub enum VanillaError {
    #[error("{path}: {why}")]
    Invalid { path: String, why: String },
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl VanillaIndex {
    /// Reads the client jar's `version.json`, its `assets/` names and its atlas definitions.
    pub fn from_jar(jar: &Utf8Path, limits: &Limits) -> Result<Self, VanillaError> {
        let invalid = |why: String| VanillaError::Invalid {
            path: jar.to_string(),
            why,
        };
        // Classes are most of a client jar: only what the index reads is inflated.
        let entries = packprune_zip::read_selected(jar, limits, &|name| {
            name == "version.json" || name.starts_with("assets/")
        })
        .map_err(|error| match error {
            packprune_zip::ZipError::Io(error) => VanillaError::Io(error),
            error => invalid(format!("not a zip ({error})")),
        })?;
        let mut index = Self {
            jar: Some(input(jar)?),
            version: "unknown".into(),
            pack_format: "unknown".into(),
            ..Self::default()
        };
        if let Some(bytes) = entries.get("version.json") {
            let version =
                json::parse(bytes).map_err(|error| invalid(format!("version.json: {error}")))?;
            index.version = version.path("id").as_string("unknown").into_owned();
            let pack = version.path("pack_version");
            if pack.has("resource_major") {
                let int = |field| {
                    pack.path(field)
                        .try_as_int()
                        .ok_or_else(|| invalid(format!("version.json: {field} is not an int")))
                };
                index.pack_format =
                    format!("{}.{}", int("resource_major")?, int("resource_minor")?);
            }
        }
        for (name, bytes) in entries {
            if !name.starts_with("assets/") {
                continue;
            }
            if name.starts_with(ATLASES) && name.ends_with(".json") {
                json::validate(&bytes).map_err(|error| invalid(format!("{name}: {error}")))?;
                index.atlases.insert(name.clone(), bytes);
            } else if name.ends_with(".png.mcmeta") {
                index.texture_meta.insert(name.clone(), bytes);
            } else if name.ends_with(".png")
                && let Some(size) = TextureSize::of_png(&bytes)
            {
                index.textures.insert(name.clone(), size);
            }
            index.names.insert(name);
        }
        Ok(index)
    }

    /// Adds the names of a launcher asset index (`indexes/<id>.json`); its `objects/` folder is read if present.
    pub fn with_asset_index(mut self, path: &Utf8Path) -> Result<Self, VanillaError> {
        let invalid = |why: String| VanillaError::Invalid {
            path: path.to_string(),
            why,
        };
        let root = json::parse(&std::fs::read(path)?)
            .map_err(|error| invalid(format!("not an asset index ({error})")))?;
        let objects = root.path("objects");
        if !objects.is_object() {
            return Err(invalid("not an asset index (no objects)".into()));
        }
        self.names.extend(
            objects
                .members()
                .iter()
                .map(|object| format!("assets/{}", object.name)),
        );
        let hash = objects
            .path("minecraft/sounds.json")
            .path("hash")
            .as_string("")
            .into_owned();
        // A new index's events or none: never the ones of the index it replaces.
        self.sound_events = None;
        let absolute = std::path::absolute(path)?;
        let assets = absolute.parent().and_then(Path::parent);
        if let (Some(assets), Some(prefix)) = (assets, hash.get(..2))
            && hash.len() > 2
        {
            let sounds = assets.join("objects").join(prefix).join(&hash);
            if sounds.is_file() {
                let events = json::parse(&std::fs::read(&sounds)?).map_err(|error| {
                    VanillaError::Invalid {
                        path: sounds.display().to_string(),
                        why: error.to_string(),
                    }
                })?;
                self.sound_events = Some(
                    events
                        .members()
                        .iter()
                        .map(|event| event.name.to_string())
                        .collect(),
                );
            }
        }
        self.asset_index = Some(input(path)?);
        self.sound_files = true;
        Ok(self)
    }

    /// The cached form: the whole index as JSON, strings only.
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("an index serialises")
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }

    pub fn has(&self, logical: &str) -> bool {
        self.names.contains(logical)
    }

    /// Atlas ids the client loads: one per `assets/minecraft/atlases/*.json` of the jar.
    pub fn atlas_ids(&self) -> BTreeSet<String> {
        self.atlases
            .keys()
            .filter_map(|path| ResourcePath::of(path))
            .map(|path| {
                let id = path.rest.strip_suffix(".json").unwrap_or(path.rest);
                format!("{}:{id}", path.namespace)
            })
            .collect()
    }
}

fn input(path: &Utf8Path) -> Result<Input, VanillaError> {
    Ok(Input {
        name: path.file_name().unwrap_or(path.as_str()).to_owned(),
        blake3: blake3::hash(&std::fs::read(path)?).to_hex().to_string(),
    })
}
