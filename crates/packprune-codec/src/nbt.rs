use std::io::{Read, Write};
use std::num::NonZeroU64;

use flate2::read::MultiGzDecoder;

use crate::{CodecError, MAX_DECODED_BYTES, invalid, zopfli_iterations};

/// The Zopfli time budget of a payload: what 512 KiB takes at the default effort, about 2 s.
const ZOPFLI_BYTES: usize = 512 * 1024;

pub fn decompress(bytes: &[u8]) -> Result<Vec<u8>, CodecError> {
    let mut out = Vec::new();
    MultiGzDecoder::new(bytes)
        .take(MAX_DECODED_BYTES as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|e| invalid("gzip NBT", e))?;
    if out.len() > MAX_DECODED_BYTES {
        return Err(invalid(
            "gzip NBT",
            "decompressed payload exceeds the memory limit",
        ));
    }
    Ok(out)
}

pub fn recompress(bytes: &[u8], effort: u8) -> Result<Vec<u8>, CodecError> {
    let before = decompress(bytes)?;
    let mut out = Vec::new();
    if let Some(iterations) = NonZeroU64::new(zopfli_iterations(effort, before.len(), ZOPFLI_BYTES))
    {
        let options = zopfli::Options {
            iteration_count: iterations,
            ..Default::default()
        };
        zopfli::compress(options, zopfli::Format::Gzip, before.as_slice(), &mut out)
            .map_err(|e| invalid("gzip NBT", e))?;
    } else {
        let mut encoder = flate2::GzBuilder::new().write(&mut out, flate2::Compression::best());
        encoder
            .write_all(&before)
            .map_err(|e| invalid("gzip NBT", e))?;
        encoder.finish().map_err(|e| invalid("gzip NBT", e))?;
    }
    if decompress(&out)? != before {
        return Err(invalid("gzip NBT", "recompression changed the payload"));
    }
    Ok(if out.len() < bytes.len() {
        out
    } else {
        bytes.to_vec()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn gzip_members_round_trip_and_corruption_is_refused() {
        let payload = b"\x0a\0\0\x08\0\x04name\0\x04test\0";
        let mut gzip = flate2::GzBuilder::new()
            .filename("structure.nbt")
            .mtime(123)
            .write(Vec::new(), flate2::Compression::fast());
        gzip.write_all(payload).unwrap();
        let mut bytes = gzip.finish().unwrap();
        bytes.extend_from_within(..);
        let out = recompress(&bytes, 1).unwrap();
        assert_eq!(decompress(&out).unwrap(), payload.repeat(2));
        assert!(out.len() < bytes.len());
        assert_eq!(out, recompress(&bytes, 1).unwrap());
        let last = bytes.len() - 8;
        bytes[last] ^= 0xff;
        assert!(recompress(&bytes, 1).is_err());
    }
}
