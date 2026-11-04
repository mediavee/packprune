use std::num::NonZeroU64;

use crate::{CompressOptions, EntryClass};

pub(crate) fn compress(
    bytes: &[u8],
    class: EntryClass,
    options: &CompressOptions,
) -> Option<Vec<u8>> {
    if options.effort == 0 || (!options.recompress_compressed && class == EntryClass::Compressed) {
        return None;
    }
    let mut best = libdeflate(bytes);
    if options.effort == u8::MAX
        && class == EntryClass::Text
        && let Some(zopfli) = zopfli(bytes)
        && zopfli.len() < best.len()
    {
        best = zopfli;
    }
    (best.len() < bytes.len()).then_some(best)
}

pub(crate) fn libdeflate(bytes: &[u8]) -> Vec<u8> {
    let mut compressor = libdeflater::Compressor::new(libdeflater::CompressionLvl::best());
    let mut compressed = vec![0; compressor.deflate_compress_bound(bytes.len())];
    let size = compressor
        .deflate_compress(bytes, &mut compressed)
        .expect("libdeflate output fits its compression bound");
    compressed.truncate(size);
    compressed
}

/// Iterations × size: 15 iterations, zopfli's own default, up to 1 MiB, fewer beyond, none past 16 MiB, where
/// libdeflate's output stands alone. A MiB costs Zopfli about 1.3 s for one iteration, 0.15 s per further one.
const ZOPFLI_BUDGET: u64 = 16 * 1024 * 1024;

pub(crate) fn zopfli(bytes: &[u8]) -> Option<Vec<u8>> {
    let iterations = (ZOPFLI_BUDGET / bytes.len().max(1) as u64).min(15);
    let options = zopfli::Options {
        iteration_count: NonZeroU64::new(iterations)?,
        ..zopfli::Options::default()
    };
    let mut compressed = Vec::new();
    zopfli::compress(options, zopfli::Format::Deflate, bytes, &mut compressed)
        .expect("reading a byte slice and writing a Vec cannot fail");
    Some(compressed)
}
