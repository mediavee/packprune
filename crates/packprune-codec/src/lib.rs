//! Byte and value transformations of single files: PNG, audio, loss metrics, minifiers, the properties
//! parser, NBT.

pub mod audio;
pub mod glsl;
pub mod metrics;
pub mod minify;
pub mod nbt;
pub mod png;
pub mod properties;

mod vorbis_client;

#[cfg(test)]
mod validation;

use std::fmt::Display;

const MAX_DECODED_BYTES: usize = 512 * 1024 * 1024;

fn invalid(kind: &'static str, why: impl Display) -> CodecError {
    CodecError::Invalid {
        kind,
        why: why.to_string(),
    }
}

/// The default `effort` of the PNG and NBT optimisers: zopfli's own default iteration count.
pub const DEFAULT_EFFORT: u8 = 15;

/// Zopfli iterations for an input of `size` units at `effort`: `effort` up to `limit` units, capped at
/// [`DEFAULT_EFFORT`] since measured outputs stop shrinking by about ten. Above `limit`, 0 (the caller's fast
/// deflater): even one iteration's time grows with the input.
fn zopfli_iterations(effort: u8, size: usize, limit: usize) -> u64 {
    if size > limit {
        0
    } else {
        u64::from(effort.min(DEFAULT_EFFORT))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("invalid {kind}: {why}")]
    Invalid { kind: &'static str, why: String },
}
