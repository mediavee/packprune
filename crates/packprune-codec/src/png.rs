use std::cmp::Reverse;
use std::io::{Cursor, Read};
use std::iter;
use std::num::NonZeroU64;

use flate2::read::ZlibDecoder;
use rayon::prelude::*;

use crate::{CodecError, MAX_DECODED_BYTES, invalid, zopfli_iterations};

/// The Zopfli time budget of an image: what 512×512 takes at the default effort, about 5 s.
const ZOPFLI_PIXELS: usize = 512 * 512;
/// Stop Zopfli early once 4 consecutive iterations fail to shrink the stream further.
const ZOPFLI_STALL_LIMIT: NonZeroU64 = NonZeroU64::new(4).expect("non-zero");
/// The effort from which [`optimise`] re-encodes small images itself rather than keep oxipng's winner.
const SEARCH_EFFORT: u8 = 3;
/// The bounds of that search, which costs six libdeflate level-12 passes per palette order.
const SEARCH_PIXELS: usize = 256 * 256;
const SEARCH_ROWS: u32 = 256;
/// The filtered bytes before a row that the brute-force filter compresses along with it, and its libdeflate level:
/// on vanilla and custom textures, level 5 picks better rows than level 1 at about the same cost.
const BRUTE_CONTEXT_BYTES: usize = 8192;
const BRUTE_LEVEL: u8 = 5;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    /// Unpremultiplied RGBA8, including RGB beneath fully transparent pixels.
    pub rgba: Vec<u8>,
}

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub effort: u8,
    pub keep_color_type: bool,
    pub keep_bit_depth: bool,
    pub keep_grayscale: bool,
    /// The client decodes with `javax.imageio`, not stb_image: ancillary chunks and 16-bit samples stay.
    pub imageio: bool,
    /// Rewrite RGB of fully transparent texels to aid PNG filtering (safe when the client overwrites them, e.g. via
    /// `TextureUtil.solidify`, or when requested by a lossy pass).
    pub optimize_alpha: bool,
}

#[derive(Clone, Debug)]
pub struct Quantization {
    pub max_colors: u16,
    pub quality: u8,
    pub dithering: f32,
}

pub fn decode(bytes: &[u8]) -> Result<Image, CodecError> {
    validate_transparency(bytes)?;
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_limits(png::Limits {
        bytes: MAX_DECODED_BYTES,
    });
    // png performs no colour management. STRIP_16 takes the high byte, as stb_image does.
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| invalid("PNG", e))?;
    if reader.info().animation_control.is_some() {
        return Err(invalid("PNG", "animated PNG is not supported"));
    }
    let width = reader.info().width;
    let height = reader.info().height;
    let rgba_len = image_len(width, height)?;
    let size = reader
        .output_buffer_size()
        .filter(|&n| n <= MAX_DECODED_BYTES)
        .ok_or_else(|| invalid("PNG", "decoded image exceeds the memory limit"))?;
    let mut pixels = vec![0; size];
    let info = reader
        .next_frame(&mut pixels)
        .map_err(|e| invalid("PNG", e))?;
    reader.finish().map_err(|e| invalid("PNG", e))?;
    let pixels = &pixels[..info.buffer_size()];
    let mut rgba = Vec::with_capacity(rgba_len);
    match info.color_type {
        png::ColorType::Rgba => rgba.extend_from_slice(pixels),
        png::ColorType::Rgb => {
            for p in pixels.as_chunks::<3>().0 {
                rgba.extend_from_slice(&[p[0], p[1], p[2], 255]);
            }
        }
        png::ColorType::Grayscale => {
            for &v in pixels {
                rgba.extend_from_slice(&[v, v, v, 255]);
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for p in pixels.as_chunks::<2>().0 {
                rgba.extend_from_slice(&[p[0], p[0], p[0], p[1]]);
            }
        }
        png::ColorType::Indexed => return Err(invalid("PNG", "palette was not expanded")),
    }
    Ok(Image {
        width,
        height,
        rgba,
    })
}

fn validate_transparency(bytes: &[u8]) -> Result<(), CodecError> {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(invalid("PNG", "invalid signature"));
    }
    let mut rest = &bytes[8..];
    let mut color = None;
    let mut palette_len = 0;
    let mut data_seen = false;
    while !rest.is_empty() {
        let header = rest
            .get(..8)
            .ok_or_else(|| invalid("PNG", "truncated chunk"))?;
        let len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let chunk = rest
            .get(..len.saturating_add(12))
            .ok_or_else(|| invalid("PNG", "truncated chunk"))?;
        match &header[4..] {
            b"IHDR" => color = chunk.get(17).copied(),
            b"PLTE" => palette_len = len / 3,
            b"IDAT" => data_seen = true,
            b"tRNS" => {
                // stb rejects these even when png would ignore or repair the chunk.
                let valid = !data_seen
                    && match color {
                        Some(0) => len == 2,
                        Some(2) => len == 6,
                        Some(3) => palette_len != 0 && len <= palette_len,
                        _ => false,
                    };
                if !valid {
                    return Err(invalid(
                        "PNG",
                        "client rejects tRNS placement, color type or length",
                    ));
                }
            }
            b"IEND" => break,
            _ => {}
        }
        rest = &rest[chunk.len()..];
    }
    Ok(())
}

fn bit_depth(bytes: &[u8]) -> Option<u8> {
    bytes
        .get(24)
        .copied()
        .filter(|_| bytes.get(12..16) == Some(b"IHDR"))
}

fn image_len(width: u32, height: u32) -> Result<usize, CodecError> {
    let size = u128::from(width) * u128::from(height) * 4;
    if width == 0 || height == 0 || size > MAX_DECODED_BYTES as u128 {
        return Err(invalid(
            "PNG",
            "invalid dimensions or decoded image exceeds the memory limit",
        ));
    }
    Ok(size as usize)
}

pub(crate) fn validate(image: &Image) -> Result<(), CodecError> {
    if image_len(image.width, image.height)? != image.rgba.len() {
        return Err(invalid("PNG", "RGBA length does not match dimensions"));
    }
    Ok(())
}

/// RGBA8, the smaller of the plain encoding and oxipng's None and MinSum filters.
pub fn encode(image: &Image) -> Result<Vec<u8>, CodecError> {
    validate(image)?;
    let plain = encode_rgba(image)?;
    let raw = oxipng::RawImage::new(
        image.width,
        image.height,
        oxipng::ColorType::RGBA,
        oxipng::BitDepth::Eight,
        image.rgba.clone(),
    )
    .map_err(|e| invalid("PNG", e))?;
    // Colour type and bit depth reductions belong to `optimise`, which knows the client quirks.
    let mut options = oxipng::Options::from_preset(0);
    options.color_type_reduction = false;
    options.palette_reduction = false;
    options.bit_depth_reduction = false;
    options.grayscale_reduction = false;
    options.filters = [oxipng::FilterStrategy::NONE, oxipng::FilterStrategy::MinSum]
        .into_iter()
        .collect();
    // Level 10 is libdeflate's first near-optimal parser; below it, sparse textures lose to the JDK.
    options.deflater = oxipng::Deflater::Libdeflater { compression: 10 };
    let filtered = raw
        .create_optimized_png(&options)
        .map_err(|e| invalid("PNG", e))?;
    // The plain encoding still wins on some tiny images.
    Ok(if filtered.len() < plain.len() {
        filtered
    } else {
        plain
    })
}

fn encode_rgba(image: &Image) -> Result<Vec<u8>, CodecError> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, image.width, image.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| invalid("PNG", e))?;
        writer
            .write_image_data(&image.rgba)
            .map_err(|e| invalid("PNG", e))?;
        writer.finish().map_err(|e| invalid("PNG", e))?;
    }
    Ok(out)
}

fn build_oxipng_options(options: &Options) -> oxipng::Options {
    let mut oxi = oxipng::Options::from_preset(2);
    oxi.strip = if options.imageio {
        oxipng::StripChunks::Safe
    } else {
        oxipng::StripChunks::All
    };
    oxi.color_type_reduction = !options.keep_color_type;
    oxi.palette_reduction = !options.keep_color_type;
    oxi.bit_depth_reduction = !options.keep_bit_depth;
    oxi.grayscale_reduction = !options.keep_grayscale && !options.keep_color_type;
    oxi.optimize_alpha = options.optimize_alpha;
    oxi.max_decompressed_size = Some(MAX_DECODED_BYTES);
    oxi.fast_evaluation = true;
    oxi.filters = [
        oxipng::FilterStrategy::NONE,
        oxipng::FilterStrategy::SUB,
        oxipng::FilterStrategy::UP,
        oxipng::FilterStrategy::AVERAGE,
        oxipng::FilterStrategy::PAETH,
        oxipng::FilterStrategy::MinSum,
        oxipng::FilterStrategy::Entropy,
        oxipng::FilterStrategy::Bigrams,
        oxipng::FilterStrategy::BigEnt,
    ]
    .into_iter()
    .collect();
    oxi.deflater = oxipng::Deflater::Libdeflater { compression: 12 };
    oxi
}

/// Fast PNG optimisation using rich filter search and libdeflate level 12 (no Zopfli).
/// Returns the optimised PNG bytes (guaranteed <= `bytes.len()`) and the decoded input image.
pub fn optimise_fast(bytes: &[u8], options: &Options) -> Result<(Vec<u8>, Image), CodecError> {
    let before = decode(bytes)?;
    let out = optimise_fast_with_decoded(bytes, &before, options)?;
    Ok((out, before))
}

fn optimise_fast_with_decoded(
    bytes: &[u8],
    before: &Image,
    options: &Options,
) -> Result<Vec<u8>, CodecError> {
    // stb_image reads 16-bit samples as their high byte, so the 8-bit image it decodes is the whole picture.
    let eight_bit;
    let source = if !options.imageio && !options.keep_bit_depth && bit_depth(bytes) == Some(16) {
        eight_bit = encode(before)?;
        &eight_bit
    } else {
        bytes
    };
    let oxi = build_oxipng_options(options);
    let out = oxipng::optimize_from_memory(source, &oxi).map_err(|e| invalid("PNG", e))?;
    if !options.optimize_alpha && decode(&out)? != *before {
        return Err(invalid("PNG", "optimisation changed client-decoded pixels"));
    }
    Ok(if out.len() < bytes.len() {
        out
    } else {
        bytes.to_vec()
    })
}

/// Fast PNG optimisation directly from an in-memory RGBA8 [`Image`] (avoids an intermediate PNG encode+decode).
pub fn optimise_raw_fast(image: &Image, options: &Options) -> Result<Vec<u8>, CodecError> {
    validate(image)?;
    let raw = oxipng::RawImage::new(
        image.width,
        image.height,
        oxipng::ColorType::RGBA,
        oxipng::BitDepth::Eight,
        image.rgba.clone(),
    )
    .map_err(|e| invalid("PNG", e))?;
    let oxi = build_oxipng_options(options);
    raw.create_optimized_png(&oxi)
        .map_err(|e| invalid("PNG", e))
}

/// Recompresses the PNG's `IDAT` stream with Zopfli when `effort > 0` and within the pixel budget, keeping the
/// smaller of the input PNG and the Zopfli-recompressed PNG. Never emits a PNG larger than `png_bytes`.
pub fn zopfli_idat(png_bytes: &[u8], pixels: usize, effort: u8) -> Result<Vec<u8>, CodecError> {
    let Some(iteration_count) = NonZeroU64::new(zopfli_iterations(effort, pixels, ZOPFLI_PIXELS))
    else {
        return Ok(png_bytes.to_vec());
    };
    let Some((prefix, idat, suffix)) = split_idat_chunks(png_bytes) else {
        return Ok(png_bytes.to_vec());
    };
    let Some(inflated) = inflate(&idat)? else {
        return Ok(png_bytes.to_vec());
    };
    let recompressed = zopfli_zlib(&inflated, iteration_count)?;
    Ok(with_idat(prefix, &recompressed, suffix)
        .filter(|out| out.len() < png_bytes.len())
        .unwrap_or_else(|| png_bytes.to_vec()))
}

/// The zlib stream inflated; `None` beyond the memory limit.
fn inflate(zlib: &[u8]) -> Result<Option<Vec<u8>>, CodecError> {
    let mut inflated = Vec::new();
    ZlibDecoder::new(zlib)
        .take(MAX_DECODED_BYTES as u64 + 1)
        .read_to_end(&mut inflated)
        .map_err(|e| invalid("PNG", e))?;
    Ok((inflated.len() <= MAX_DECODED_BYTES).then_some(inflated))
}

fn zopfli_zlib(data: &[u8], iteration_count: NonZeroU64) -> Result<Vec<u8>, CodecError> {
    let options = zopfli::Options {
        iteration_count,
        iterations_without_improvement: ZOPFLI_STALL_LIMIT,
        ..Default::default()
    };
    let mut out = Vec::new();
    zopfli::compress(options, zopfli::Format::Zlib, data, &mut out)
        .map_err(|e| invalid("PNG", e))?;
    Ok(out)
}

/// `prefix` and `suffix` around a single `IDAT` chunk; `None` when `idat` is too long for one chunk.
fn with_idat(prefix: &[u8], idat: &[u8], suffix: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(prefix.len() + 12 + idat.len() + suffix.len());
    out.extend_from_slice(prefix);
    write_chunk(&mut out, b"IDAT", idat)?;
    out.extend_from_slice(suffix);
    Some(out)
}

fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) -> Option<()> {
    out.extend_from_slice(&u32::try_from(data.len()).ok()?.to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(kind);
    hasher.update(data);
    out.extend_from_slice(&hasher.finalize().to_be_bytes());
    Some(())
}

/// Splits a PNG into `(before_first_idat, concatenated_idat_payload, after_last_idat)`.
fn split_idat_chunks(bytes: &[u8]) -> Option<(&[u8], Vec<u8>, &[u8])> {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return None;
    }
    let mut pos = 8usize;
    let mut first_idat = None;
    let mut last_idat_end = 0usize;
    let mut idat = Vec::new();
    while pos + 12 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[pos..pos + 4].try_into().ok()?) as usize;
        let end = pos.checked_add(12)?.checked_add(len)?;
        if end > bytes.len() {
            return None;
        }
        let kind = &bytes[pos + 4..pos + 8];
        if kind == b"IDAT" {
            if first_idat.is_none() {
                first_idat = Some(pos);
            } else if last_idat_end != pos {
                // Non-contiguous IDAT is invalid PNG.
                return None;
            }
            idat.extend_from_slice(&bytes[pos + 8..pos + 8 + len]);
            last_idat_end = end;
        }
        pos = end;
        if kind == b"IEND" {
            break;
        }
    }
    let start = first_idat?;
    Some((&bytes[..start], idat, &bytes[last_idat_end..pos]))
}

pub fn optimise(bytes: &[u8], options: &Options) -> Result<Vec<u8>, CodecError> {
    let before = decode(bytes)?;
    let fast = optimise_fast_with_decoded(bytes, &before, options)?;
    let pixels = before.width as usize * before.height as usize;
    let out = match search(&fast, pixels, options)? {
        Some(out) => out,
        None => zopfli_idat(&fast, pixels, options.effort)?,
    };
    if out != fast && !options.optimize_alpha && decode(&out)? != before {
        return Err(invalid("PNG", "optimisation changed client-decoded pixels"));
    }
    Ok(if out.len() < bytes.len() {
        out
    } else {
        bytes.to_vec()
    })
}

/// Re-encodes the image data of `png_bytes` with each single filter and a deflate-guided filter per row, and an
/// indexed image also with its palette by frequency, all at libdeflate level 12. Zopfli then compresses the smallest
/// encoding and the input's own, so the result is never larger than [`zopfli_idat`]'s. Keeps every other chunk.
/// `None`, leaving the caller's plain Zopfli pass, when the image is outside the search bounds.
fn search(
    png_bytes: &[u8],
    pixels: usize,
    options: &Options,
) -> Result<Option<Vec<u8>>, CodecError> {
    if options.effort < SEARCH_EFFORT || pixels > SEARCH_PIXELS {
        return Ok(None);
    }
    let Some((prefix, idat, suffix)) = split_idat_chunks(png_bytes) else {
        return Ok(None);
    };
    let Some(image) = Scanlines::read(png_bytes).filter(|image| image.height <= SEARCH_ROWS) else {
        return Ok(None);
    };
    // A candidate is an index into `prefixes` and an IDAT payload; the input's own comes first and wins ties.
    let mut prefixes = vec![prefix.to_vec()];
    let mut candidates = vec![(0, idat)];
    candidates.extend(
        image
            .compress(options.optimize_alpha)?
            .into_iter()
            .map(|idat| (0, idat)),
    );
    if !options.keep_color_type
        && let Some(sorted) = image.palette_by_frequency()
        && let Some(sorted_prefix) = with_palette(prefix, sorted.palette())
    {
        prefixes.push(sorted_prefix);
        candidates.extend(
            sorted
                .compress(options.optimize_alpha)?
                .into_iter()
                .map(|idat| (1, idat)),
        );
    }
    let size = |(prefix, idat): &(usize, Vec<u8>)| prefixes[*prefix].len() + idat.len();
    let own = &candidates[0];
    let mut best = candidates
        .iter()
        .min_by_key(|candidate| size(candidate))
        .unwrap_or(own);
    let mut squeezed = Vec::new();
    if let Some(iterations) =
        NonZeroU64::new(zopfli_iterations(options.effort, pixels, ZOPFLI_PIXELS))
    {
        let finalists: Vec<_> = iter::once(best)
            .chain((own != best).then_some(own))
            .collect();
        squeezed = finalists
            .into_par_iter()
            .map(|(prefix, idat)| {
                Ok(match inflate(idat)? {
                    Some(inflated) => Some((*prefix, zopfli_zlib(&inflated, iterations)?)),
                    None => None,
                })
            })
            .collect::<Result<Vec<_>, CodecError>>()?;
    }
    for candidate in squeezed.iter().flatten() {
        if size(candidate) < size(best) {
            best = candidate;
        }
    }
    Ok(Some(
        with_idat(&prefixes[best.0], &best.1, suffix)
            .filter(|out| out.len() < png_bytes.len())
            .unwrap_or_else(|| png_bytes.to_vec()),
    ))
}

/// The unfiltered rows of a non-interlaced PNG of at most 8 bits per sample, packed as in the file.
struct Scanlines {
    width: u32,
    height: u32,
    color: oxipng::ColorType,
    depth: oxipng::BitDepth,
    data: Vec<u8>,
}

impl Scanlines {
    fn read(bytes: &[u8]) -> Option<Self> {
        let mut decoder = png::Decoder::new(Cursor::new(bytes));
        decoder.set_limits(png::Limits {
            bytes: MAX_DECODED_BYTES,
        });
        decoder.set_transformations(png::Transformations::IDENTITY);
        let mut reader = decoder.read_info().ok()?;
        let info = reader.info();
        if info.interlaced || info.animation_control.is_some() {
            return None;
        }
        let depth = match info.bit_depth {
            png::BitDepth::One => oxipng::BitDepth::One,
            png::BitDepth::Two => oxipng::BitDepth::Two,
            png::BitDepth::Four => oxipng::BitDepth::Four,
            png::BitDepth::Eight => oxipng::BitDepth::Eight,
            png::BitDepth::Sixteen => return None,
        };
        // Only the IDAT of the re-encodings is kept, so a grey or RGB tRNS colour need not be carried.
        let color = match info.color_type {
            png::ColorType::Grayscale => oxipng::ColorType::Grayscale {
                transparent_shade: None,
            },
            png::ColorType::Rgb => oxipng::ColorType::RGB {
                transparent_color: None,
            },
            png::ColorType::Indexed => {
                let alpha = info.trns.as_deref().unwrap_or_default();
                let palette = info.palette.as_deref()?.as_chunks::<3>().0;
                oxipng::ColorType::Indexed {
                    palette: palette
                        .iter()
                        .enumerate()
                        .map(|(i, &[r, g, b])| {
                            oxipng::RGBA8::new(r, g, b, alpha.get(i).copied().unwrap_or(255))
                        })
                        .collect(),
                }
            }
            png::ColorType::GrayscaleAlpha => oxipng::ColorType::GrayscaleAlpha,
            png::ColorType::Rgba => oxipng::ColorType::RGBA,
        };
        let (width, height) = info.size();
        let mut data = vec![0; reader.output_buffer_size()?];
        reader.next_frame(&mut data).ok()?;
        Some(Self {
            width,
            height,
            color,
            depth,
            data,
        })
    }

    /// The image data filtered by each single filter and by the brute-force row filter, then compressed by libdeflate
    /// level 12. oxipng's heuristic strategies are left out: its winner is already a candidate, and ranking them
    /// again at level 12 cost 12 % more time for 0.02 % on vanilla and custom textures.
    fn compress(&self, optimize_alpha: bool) -> Result<Vec<Vec<u8>>, CodecError> {
        let line = self.data.len() / self.height as usize + 1;
        let strategies = [
            oxipng::FilterStrategy::NONE,
            oxipng::FilterStrategy::SUB,
            oxipng::FilterStrategy::UP,
            oxipng::FilterStrategy::AVERAGE,
            oxipng::FilterStrategy::PAETH,
            // Per row, the filter whose output compresses smallest after the preceding rows.
            oxipng::FilterStrategy::Brute {
                num_lines: BRUTE_CONTEXT_BYTES.div_ceil(line) + 1,
                level: BRUTE_LEVEL,
            },
        ];
        let raw = oxipng::RawImage::new(
            self.width,
            self.height,
            self.color.clone(),
            self.depth,
            self.data.clone(),
        )
        .map_err(|e| invalid("PNG", e))?;
        strategies
            .par_iter()
            .map(|strategy| {
                let mut oxi = oxipng::Options::from_preset(2);
                oxi.strip = oxipng::StripChunks::All;
                oxi.color_type_reduction = false;
                oxi.palette_reduction = false;
                oxi.bit_depth_reduction = false;
                oxi.grayscale_reduction = false;
                oxi.optimize_alpha = optimize_alpha;
                oxi.filters = iter::once(strategy.clone()).collect();
                oxi.deflater = oxipng::Deflater::Libdeflater { compression: 12 };
                let png = raw
                    .create_optimized_png(&oxi)
                    .map_err(|e| invalid("PNG", e))?;
                split_idat_chunks(&png)
                    .map(|(_, idat, _)| idat)
                    .ok_or_else(|| invalid("PNG", "oxipng wrote no image data"))
            })
            .collect()
    }

    fn palette(&self) -> &[oxipng::RGBA8] {
        match &self.color {
            oxipng::ColorType::Indexed { palette } => palette,
            _ => &[],
        }
    }

    /// The indexed image with its unused entries dropped and the others by decreasing use, translucent ones first so
    /// tRNS stays short; `None` when that is already the order. oxipng's own sorts ignore frequency.
    fn palette_by_frequency(&self) -> Option<Self> {
        let palette = self.palette();
        let bits = self.depth as usize;
        if palette.is_empty() {
            return None;
        }
        let stride = self.data.len() / self.height as usize;
        let mask = u8::MAX >> (8 - bits);
        let shift = |x: usize| 8 - bits - (x * bits) % 8;
        let mut counts = vec![0_usize; palette.len()];
        for row in self.data.chunks_exact(stride) {
            for x in 0..self.width as usize {
                let index = (row[x * bits / 8] >> shift(x)) & mask;
                *counts.get_mut(usize::from(index))? += 1;
            }
        }
        let mut order: Vec<_> = (0..palette.len()).filter(|&i| counts[i] > 0).collect();
        order.sort_by_key(|&i| (palette[i].a == u8::MAX, Reverse(counts[i]), i));
        if order.iter().copied().eq(0..palette.len()) {
            return None;
        }
        let mut remap = vec![0_u8; palette.len()];
        for (new, &old) in order.iter().enumerate() {
            remap[old] = u8::try_from(new).ok()?;
        }
        let mut data = self.data.clone();
        for row in data.chunks_exact_mut(stride) {
            for x in 0..self.width as usize {
                let byte = &mut row[x * bits / 8];
                let index = (*byte >> shift(x)) & mask;
                *byte = (*byte & !(mask << shift(x))) | (remap[usize::from(index)] << shift(x));
            }
        }
        Some(Self {
            color: oxipng::ColorType::Indexed {
                palette: order.iter().map(|&i| palette[i]).collect(),
            },
            data,
            ..*self
        })
    }
}

/// `prefix` (the chunks before IDAT) with its PLTE and tRNS rewritten for `palette`; `None` when another chunk
/// refers to palette indices.
fn with_palette(prefix: &[u8], palette: &[oxipng::RGBA8]) -> Option<Vec<u8>> {
    let mut out = prefix.get(..8)?.to_vec();
    let mut rest = &prefix[8..];
    while !rest.is_empty() {
        let len = u32::from_be_bytes(rest.get(..4)?.try_into().ok()?) as usize;
        let chunk = rest.get(..len.checked_add(12)?)?;
        match &chunk[4..8] {
            b"PLTE" => {
                let rgb: Vec<_> = palette.iter().flat_map(|c| [c.r, c.g, c.b]).collect();
                write_chunk(&mut out, b"PLTE", &rgb)?;
                if let Some(last) = palette.iter().rposition(|c| c.a != u8::MAX) {
                    let alpha: Vec<_> = palette[..=last].iter().map(|c| c.a).collect();
                    write_chunk(&mut out, b"tRNS", &alpha)?;
                }
            }
            b"tRNS" => {}
            b"bKGD" | b"hIST" => return None,
            _ => out.extend_from_slice(chunk),
        }
        rest = &rest[chunk.len()..];
    }
    Some(out)
}

/// The image with the colour of fully transparent pixels zeroed; `None` when none has a colour.
pub fn clear_transparent(image: &Image) -> Option<Image> {
    let hidden = |p: &[u8; 4]| p[3] == 0 && p[..3] != [0; 3];
    if !image.rgba.as_chunks::<4>().0.iter().any(hidden) {
        return None;
    }
    let mut cleared = image.clone();
    for pixel in cleared.rgba.as_chunks_mut::<4>().0 {
        if pixel[3] == 0 {
            pixel[..3].fill(0);
        }
    }
    Some(cleared)
}

/// The image quantised to a palette, as an indexed PNG for [`optimise`]; `None` when no pixel changes.
pub fn quantize(image: &Image, quantization: &Quantization) -> Result<Option<Vec<u8>>, CodecError> {
    validate(image)?;
    let pixels: Vec<_> = image
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| imagequant::RGBA::new(p[0], p[1], p[2], p[3]))
        .collect();
    let mut attr = imagequant::new();
    attr.set_max_colors(u32::from(quantization.max_colors))
        .map_err(|e| invalid("PNG quantization", e))?;
    attr.set_quality(0, quantization.quality)
        .map_err(|e| invalid("PNG quantization", e))?;
    let mut input = attr
        .new_image(pixels, image.width as usize, image.height as usize, 0.0)
        .map_err(|e| invalid("PNG quantization", e))?;
    let mut result = attr
        .quantize(&mut input)
        .map_err(|e| invalid("PNG quantization", e))?;
    result
        .set_dithering_level(quantization.dithering)
        .map_err(|e| invalid("PNG quantization", e))?;
    let (palette, indices) = result
        .remapped(&mut input)
        .map_err(|e| invalid("PNG quantization", e))?;
    let unchanged = indices
        .iter()
        .zip(image.rgba.as_chunks::<4>().0)
        .all(|(&i, p)| {
            let c = palette[usize::from(i)];
            [c.r, c.g, c.b, c.a] == *p
        });
    if unchanged {
        return Ok(None);
    }
    let mut candidate = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut candidate, image.width, image.height);
        encoder.set_color(png::ColorType::Indexed);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_palette(
            palette
                .iter()
                .flat_map(|p| [p.r, p.g, p.b])
                .collect::<Vec<_>>(),
        );
        encoder.set_trns(palette.iter().map(|p| p.a).collect::<Vec<_>>());
        let mut writer = encoder.write_header().map_err(|e| invalid("PNG", e))?;
        writer
            .write_image_data(&indices)
            .map_err(|e| invalid("PNG", e))?;
        writer.finish().map_err(|e| invalid("PNG", e))?;
    }
    Ok(Some(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(depth: png::BitDepth, color: png::ColorType, pixels: &[u8], gamma: bool) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, 1, 1);
            encoder.set_depth(depth);
            encoder.set_color(color);
            if gamma {
                encoder.set_source_gamma(png::ScaledFloat::new(1.0));
            }
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(pixels).unwrap();
        }
        out
    }

    #[test]
    fn stb_rejects_invalid_transparency_chunks() {
        // LWJGL stb 3.4.1 rejects these fixtures before decoding any pixels.
        for bytes in [
            include_bytes!("fixtures/rgba-trns.png").as_slice(),
            include_bytes!("fixtures/gray-alpha-trns.png"),
            include_bytes!("fixtures/rgb-short-trns.png"),
            include_bytes!("fixtures/gray-long-trns.png"),
            include_bytes!("fixtures/rgb-late-trns.png"),
            include_bytes!("fixtures/palette-early-trns.png"),
            include_bytes!("fixtures/palette-long-trns.png"),
        ] {
            assert!(decode(bytes).is_err());
            assert!(optimise(bytes, &Options::default()).is_err());
        }
        let valid = include_bytes!("fixtures/rgb-valid-trns.png");
        assert_eq!(decode(valid).unwrap().rgba, [0, 0, 0, 0]);
    }

    #[test]
    fn stb_gamma_and_high_byte_semantics_survive_optimisation() {
        for (depth, pixels, expected) in [
            (
                png::BitDepth::Eight,
                vec![32, 128, 240, 0],
                vec![32, 128, 240, 0],
            ),
            (
                png::BitDepth::Sixteen,
                vec![0x12, 0xff, 0x80, 0xfe, 0xab, 0xcd, 0xff, 0],
                vec![0x12, 0x80, 0xab, 0xff],
            ),
        ] {
            let bytes = fixture(depth, png::ColorType::Rgba, &pixels, true);
            assert_eq!(decode(&bytes).unwrap().rgba, expected);
            let out = optimise(&bytes, &Options::default()).unwrap();
            assert_eq!(decode(&out).unwrap().rgba, expected);
        }
    }

    #[test]
    fn palette_low_depth_and_grayscale_alpha_expand_to_rgba() {
        let grey = fixture(
            png::BitDepth::Two,
            png::ColorType::Grayscale,
            &[0b1000_0000],
            false,
        );
        assert_eq!(decode(&grey).unwrap().rgba, [170, 170, 170, 255]);
        let ga = fixture(
            png::BitDepth::Eight,
            png::ColorType::GrayscaleAlpha,
            &[17, 9],
            false,
        );
        assert_eq!(decode(&ga).unwrap().rgba, [17, 17, 17, 9]);
    }

    #[test]
    fn transparent_rgb_and_thread_count_are_preserved() {
        let bytes = fixture(
            png::BitDepth::Eight,
            png::ColorType::Rgba,
            &[42, 7, 15, 0],
            false,
        );
        let outputs: Vec<_> = [1, 8]
            .into_iter()
            .map(|threads| {
                rayon::ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .build()
                    .unwrap()
                    .install(|| optimise(&bytes, &Options::default()).unwrap())
            })
            .collect();
        assert_eq!(outputs[0], outputs[1]);
        assert_eq!(decode(&outputs[0]).unwrap().rgba, [42, 7, 15, 0]);
    }

    #[test]
    fn quantization_changes_pixels_or_yields_nothing() {
        let rgba = (0..1024)
            .flat_map(|i| {
                let grey = (i % 32 * 8) as u8;
                [grey, grey, grey, 255]
            })
            .collect::<Vec<_>>();
        let image = Image {
            width: 32,
            height: 32,
            rgba,
        };
        let exact = Quantization {
            max_colors: 256,
            quality: 100,
            dithering: 1.0,
        };
        assert_eq!(quantize(&image, &exact).unwrap(), None);
        let coarse = Quantization {
            max_colors: 4,
            ..exact
        };
        let candidate = quantize(&image, &coarse).unwrap().unwrap();
        let out = decode(&optimise(&candidate, &Options::default()).unwrap()).unwrap();
        assert_ne!(out, image);
        let colours: std::collections::BTreeSet<_> = out.rgba.as_chunks::<4>().0.iter().collect();
        assert!(colours.len() <= 4, "{}", colours.len());
    }

    #[test]
    fn encoding_is_no_larger_than_the_jdk() {
        // A 64×64 sparse texture as JDK 25's ImageIO writes it (TYPE_INT_ARGB).
        let jdk = include_bytes!("fixtures/jdk-sparse.png");
        let image = decode(jdk).unwrap();
        let out = encode(&image).unwrap();
        assert!(out.len() <= jdk.len(), "{} > {}", out.len(), jdk.len());
        assert_eq!(decode(&out).unwrap(), image);
    }

    #[test]
    fn encoding_is_never_larger_than_plain_rgba() {
        // oxipng spends a few more bytes than plain RGBA on a single opaque pixel.
        let image = Image {
            width: 1,
            height: 1,
            rgba: vec![25, 37, 52, 255],
        };
        let plain = encode_rgba(&image).unwrap();
        assert_eq!(encode(&image).unwrap(), plain);
    }

    #[test]
    fn encoding_leaves_colour_type_reductions_to_the_quirk_aware_optimiser() {
        // ImageIO clients (1.12.2 and older) brighten a grey PNG; only the optimiser knows which
        // clients a file meets, so the encoder must not make the PNG grey itself.
        let image = Image {
            width: 16,
            height: 16,
            rgba: (0..=255u8).flat_map(|v| [v, v, v, 255]).collect(),
        };
        let bytes = encode(&image).unwrap();
        assert_eq!((bytes[24], bytes[25]), (8, 6), "RGBA8");
        let kept = optimise(
            &bytes,
            &Options {
                keep_grayscale: true,
                ..Options::default()
            },
        )
        .unwrap();
        assert!(!matches!(kept[25], 0 | 4), "colour type {}", kept[25]);
    }

    #[test]
    fn clearing_transparent_colour_touches_only_hidden_pixels() {
        let image = Image {
            width: 2,
            height: 2,
            rgba: vec![7, 8, 9, 0, 1, 2, 3, 255, 0, 0, 0, 0, 4, 5, 6, 128],
        };
        let cleared = clear_transparent(&image).unwrap();
        assert_eq!(
            cleared.rgba,
            [0, 0, 0, 0, 1, 2, 3, 255, 0, 0, 0, 0, 4, 5, 6, 128]
        );
        assert_eq!(clear_transparent(&cleared), None);
    }

    fn chunks(bytes: &[u8]) -> Vec<String> {
        let mut names = Vec::new();
        let mut rest = &bytes[8..];
        while rest.len() >= 12 {
            let len = u32::from_be_bytes(rest[..4].try_into().unwrap()) as usize;
            names.push(String::from_utf8_lossy(&rest[4..8]).into_owned());
            rest = &rest[12 + len..];
        }
        names
    }

    #[test]
    fn stb_clients_lose_ancillary_chunks_and_sixteen_bit_samples_imageio_keeps_them() {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 4, 4);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Sixteen);
            encoder.set_source_gamma(png::ScaledFloat::new(0.45455));
            encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
            encoder.set_pixel_dims(Some(png::PixelDimensions {
                xppu: 2835,
                yppu: 2835,
                unit: png::Unit::Meter,
            }));
            let pixels: Vec<u8> = (0..16u16)
                .flat_map(|i| {
                    let v = i * 4099;
                    [v, v / 2, 65535 - v, 65535]
                        .into_iter()
                        .flat_map(u16::to_be_bytes)
                })
                .collect();
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&pixels).unwrap();
        }
        let before = decode(&bytes).unwrap();

        let stb = optimise(&bytes, &Options::default()).unwrap();
        assert_eq!(decode(&stb).unwrap(), before);
        assert_eq!(stb[24], 8, "bit depth");
        assert!(
            chunks(&stb)
                .iter()
                .all(|c| ["IHDR", "PLTE", "tRNS", "IDAT", "IEND"].contains(&c.as_str())),
            "{:?}",
            chunks(&stb)
        );

        let imageio = optimise(
            &bytes,
            &Options {
                imageio: true,
                ..Options::default()
            },
        )
        .unwrap();
        assert_eq!(imageio[24], 16, "bit depth");
        assert!(
            chunks(&imageio).iter().any(|c| c == "sRGB"),
            "{:?}",
            chunks(&imageio)
        );
    }

    /// A deterministic pseudo-random byte stream.
    fn noise(seed: u32) -> impl FnMut() -> u8 {
        let mut state = seed;
        move || {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            (state >> 16) as u8
        }
    }

    /// Grey bands that alternate between noisy horizontal and noisy vertical gradients, so the best filter changes
    /// every 8 rows between Sub and Up.
    fn striped_gradient() -> Vec<u8> {
        let (width, height) = (64, 64);
        let mut next = noise(7);
        let mut grey = vec![0_u8; width * height];
        for y in 0..height {
            for x in 0..width {
                let step = next() % 5;
                grey[y * width + x] = if y / 8 % 2 == 0 {
                    let left = if x == 0 {
                        next()
                    } else {
                        grey[y * width + x - 1]
                    };
                    left.wrapping_add(step).wrapping_sub(2)
                } else {
                    let up = if y % 8 == 0 {
                        next()
                    } else {
                        grey[(y - 1) * width + x]
                    };
                    up.wrapping_add(step).wrapping_sub(2)
                };
            }
        }
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, width as u32, height as u32);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&grey).unwrap();
        }
        out
    }

    /// 32×32 at 4 bits: ten opaque entries, then two fully transparent ones with distinct RGB, the last of which
    /// is the background.
    fn backwards_palette() -> (Vec<u8>, Vec<[u8; 4]>) {
        let mut palette: Vec<_> = (0..10_u8)
            .map(|i| [i * 25, 200 - i * 20, 40 + i * 9, 255])
            .collect();
        palette.extend([[10, 10, 200, 0], [10, 200, 10, 0]]);
        let (width, height) = (32_usize, 32_usize);
        let indices: Vec<u8> = (0..width * height)
            .map(|i| {
                let (x, y) = (i % width, i / width);
                let (dx, dy) = (x.abs_diff(16), y.abs_diff(16));
                if dx * dx + dy * dy < 100 {
                    ((x + y) / 3 % 10) as u8
                } else if x < 4 && y < 4 {
                    10
                } else {
                    11
                }
            })
            .collect();
        let packed: Vec<u8> = indices
            .chunks_exact(width)
            .flat_map(|row| {
                row.chunks(2)
                    .map(|pair| pair[0] << 4 | pair.get(1).copied().unwrap_or(0))
            })
            .collect();
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, width as u32, height as u32);
            encoder.set_color(png::ColorType::Indexed);
            encoder.set_depth(png::BitDepth::Four);
            encoder.set_palette(
                palette
                    .iter()
                    .flat_map(|c| [c[0], c[1], c[2]])
                    .collect::<Vec<_>>(),
            );
            encoder.set_trns(palette.iter().map(|c| c[3]).collect::<Vec<_>>());
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&packed).unwrap();
        }
        (out, palette)
    }

    fn chunk<'a>(bytes: &'a [u8], kind: &str) -> Option<&'a [u8]> {
        let mut rest = &bytes[8..];
        while rest.len() >= 12 {
            let len = u32::from_be_bytes(rest[..4].try_into().unwrap()) as usize;
            if &rest[4..8] == kind.as_bytes() {
                return Some(&rest[8..8 + len]);
            }
            rest = &rest[12 + len..];
        }
        None
    }

    const SEARCHED: Options = Options {
        effort: crate::DEFAULT_EFFORT,
        keep_color_type: false,
        keep_bit_depth: false,
        keep_grayscale: false,
        imageio: false,
        optimize_alpha: false,
    };

    /// What `optimise` gave before it searched: oxipng's winner recompressed by Zopfli.
    fn oxipng_then_zopfli(bytes: &[u8], options: &Options) -> Vec<u8> {
        let (fast, image) = optimise_fast(bytes, options).unwrap();
        let pixels = image.width as usize * image.height as usize;
        zopfli_idat(&fast, pixels, options.effort).unwrap()
    }

    #[test]
    fn brute_force_row_filter_beats_every_single_filter_on_mixed_bands() {
        let bytes = striped_gradient();
        let sizes: Vec<_> = Scanlines::read(&bytes)
            .unwrap()
            .compress(false)
            .unwrap()
            .iter()
            .map(Vec::len)
            .collect();
        let (single, brute) = (&sizes[..5], sizes[5]);
        assert!(single.iter().all(|&size| brute < size), "{sizes:?}");

        let out = optimise(&bytes, &SEARCHED).unwrap();
        assert_eq!(decode(&out).unwrap(), decode(&bytes).unwrap());
        let before = oxipng_then_zopfli(&bytes, &SEARCHED);
        assert!(
            out.len() < before.len(),
            "{} >= {}",
            out.len(),
            before.len()
        );
    }

    #[test]
    fn palette_by_frequency_shortens_trns_and_keeps_transparent_colours() {
        let (bytes, palette) = backwards_palette();
        let before = decode(&bytes).unwrap();
        assert!(before.rgba.as_chunks::<4>().0.contains(&[10, 10, 200, 0]));
        let out = search(&bytes, 32 * 32, &SEARCHED).unwrap().unwrap();
        assert!(out.len() < bytes.len(), "{} >= {}", out.len(), bytes.len());
        assert_eq!((out[24], out[25]), (4, 3), "bit depth and colour type");
        let plte = chunk(&out, "PLTE").unwrap();
        assert_eq!(plte[..6], [10, 200, 10, 10, 10, 200], "background first");
        assert_eq!(plte.len(), palette.len() * 3);
        assert_eq!(chunk(&out, "tRNS").unwrap(), [0, 0]);
        assert_eq!(decode(&out).unwrap(), before);

        let optimised = optimise(&bytes, &SEARCHED).unwrap();
        assert_eq!(decode(&optimised).unwrap(), before);
        assert!(optimised.len() <= oxipng_then_zopfli(&bytes, &SEARCHED).len());
    }

    #[test]
    fn search_is_deterministic_across_thread_counts() {
        for bytes in [striped_gradient(), backwards_palette().0] {
            let image = decode(&bytes).unwrap();
            let pixels = image.width as usize * image.height as usize;
            assert!(search(&bytes, pixels, &SEARCHED).unwrap().is_some());
            let outputs: Vec<_> = [1, 8]
                .into_iter()
                .map(|threads| {
                    rayon::ThreadPoolBuilder::new()
                        .num_threads(threads)
                        .build()
                        .unwrap()
                        .install(|| optimise(&bytes, &SEARCHED).unwrap())
                })
                .collect();
            assert_eq!(outputs[0], outputs[1]);
        }
    }

    #[test]
    fn search_keeps_client_quirks() {
        let (indexed, _) = backwards_palette();
        let kept = optimise(
            &indexed,
            &Options {
                keep_color_type: true,
                ..SEARCHED
            },
        )
        .unwrap();
        assert_eq!(kept[25], 3, "colour type");
        assert_eq!(chunk(&kept, "PLTE"), chunk(&indexed, "PLTE"));
        assert_eq!(decode(&kept).unwrap(), decode(&indexed).unwrap());

        let grey = encode(&Image {
            width: 16,
            height: 16,
            rgba: (0..=255u8).flat_map(|v| [v, v, v, 255]).collect(),
        })
        .unwrap();
        let kept = optimise(
            &grey,
            &Options {
                keep_grayscale: true,
                ..SEARCHED
            },
        )
        .unwrap();
        assert!(!matches!(kept[25], 0 | 4), "colour type {}", kept[25]);

        let mut tagged = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut tagged, 32, 32);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
            encoder.set_pixel_dims(Some(png::PixelDimensions {
                xppu: 2835,
                yppu: 2835,
                unit: png::Unit::Meter,
            }));
            let mut writer = encoder.write_header().unwrap();
            writer
                .write_image_data(&decode(&indexed).unwrap().rgba)
                .unwrap();
        }
        let imageio = Options {
            imageio: true,
            ..SEARCHED
        };
        let kept = optimise(&tagged, &imageio).unwrap();
        let fast = optimise_fast(&tagged, &imageio).unwrap().0;
        assert!(search(&fast, 32 * 32, &imageio).unwrap().is_some());
        assert_eq!(decode(&kept).unwrap(), decode(&tagged).unwrap());
        for kind in ["sRGB", "pHYs"] {
            assert!(
                chunk(&kept, kind).is_some(),
                "{kind} in {:?}",
                chunks(&kept)
            );
        }
    }
}
