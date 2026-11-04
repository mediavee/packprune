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

/// Zopfli iterations for an input of `size` units at `effort`, capped at [`DEFAULT_EFFORT`] since measured outputs
/// stop shrinking by about ten. Iterations × size stays within what `limit` units take at that effort, so a larger
/// input gets fewer iterations, and 0 (the caller's fast deflater) once one would exceed it.
fn zopfli_iterations(effort: u8, size: usize, limit: usize) -> u64 {
    let iterations = u64::from(effort.min(DEFAULT_EFFORT));
    iterations.min(iterations * limit as u64 / size.max(1) as u64)
}

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("invalid {kind}: {why}")]
    Invalid { kind: &'static str, why: String },
}
