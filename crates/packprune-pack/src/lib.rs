//! The pack model: ids, paths, input, `pack.mcmeta`, overlays, views, the vanilla index, client rules, JSON.

pub mod ids;
pub mod input;
pub mod json;
pub mod kinds;
pub mod meta;
pub mod paths;
pub mod pattern;
pub mod rules;
pub mod vanilla;
pub mod views;

pub use ids::{Interner, ResourceId, RunCx, Sym};
pub use input::{Blob, Digest, Entry, InputError, PackInput};
pub use json::Pointer;
pub use kinds::FileKind;
pub use meta::{FormatRange, PackMeta};
pub use packprune_zip::Limits;
pub use paths::{Layer, LogicalPath, PhysPath};
pub use pattern::{JavaPattern, PatternError};
pub use rules::ClientRules;
pub use vanilla::{TextureSize, VanillaIndex};
pub use views::{Layout, Quirks, View, ViewId, ViewSet};
