use std::cell::RefCell;
use std::num::NonZeroU64;

use crate::{CompressOptions, EntryClass};

/// Stop Zopfli early once 4 consecutive iterations fail to shrink the stream further.
const ZOPFLI_STALL_LIMIT: NonZeroU64 = NonZeroU64::new(4).expect("non-zero");

thread_local! {
    static LIBDEFLATE: RefCell<libdeflater::Compressor> =
        RefCell::new(libdeflater::Compressor::new(libdeflater::CompressionLvl::best()));
}

pub(crate) fn compress(
    bytes: &[u8],
    class: EntryClass,
    options: &CompressOptions,
) -> Option<Vec<u8>> {
    if options.effort == 0 || (!options.recompress_compressed && class == EntryClass::Compressed) {
        return None;
    }
    let mut best = libdeflate(bytes);
    // Zopfli improves on a fraction of what libdeflate removes: on already compressed data such as PNG, where
    // libdeflate saves under 1 %, it costs seconds for a few bytes.
    if options.effort == u8::MAX
        && best.len() * 100 < bytes.len() * 99
        && let Some(zopfli) = zopfli(bytes)
        && zopfli.len() < best.len()
    {
        best = zopfli;
    }
    (best.len() < bytes.len()).then_some(best)
}

pub(crate) fn libdeflate(bytes: &[u8]) -> Vec<u8> {
    LIBDEFLATE.with(|cell| {
        let mut compressor = cell.borrow_mut();
        let mut compressed = vec![0; compressor.deflate_compress_bound(bytes.len())];
        let size = compressor
            .deflate_compress(bytes, &mut compressed)
            .expect("libdeflate output fits its compression bound");
        compressed.truncate(size);
        compressed
    })
}

/// Iterations × size: 15 iterations, zopfli's own default, up to 1 MiB, fewer beyond, none past 16 MiB, where
/// libdeflate's output stands alone. A MiB costs Zopfli about 1.3 s for one iteration, 0.15 s per further one.
const ZOPFLI_BUDGET: u64 = 16 * 1024 * 1024;

pub(crate) fn zopfli(bytes: &[u8]) -> Option<Vec<u8>> {
    let iterations = (ZOPFLI_BUDGET / bytes.len().max(1) as u64).min(15);
    let options = zopfli::Options {
        iteration_count: NonZeroU64::new(iterations)?,
        iterations_without_improvement: ZOPFLI_STALL_LIMIT,
        ..zopfli::Options::default()
    };
    let mut compressed = Vec::new();
    zopfli::compress(options, zopfli::Format::Deflate, bytes, &mut compressed)
        .expect("reading a byte slice and writing a Vec cannot fail");
    Some(compressed)
}
