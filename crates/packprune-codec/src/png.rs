use std::io::Cursor;
use std::num::NonZeroU64;

use crate::{CodecError, MAX_DECODED_BYTES, invalid, zopfli_iterations};

/// The Zopfli time budget of an image: what 512×512 takes at the default effort, about 5 s.
const ZOPFLI_PIXELS: usize = 512 * 512;

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

pub fn optimise(bytes: &[u8], options: &Options) -> Result<Vec<u8>, CodecError> {
    let before = decode(bytes)?;
    // stb_image reads 16-bit samples as their high byte, so the 8-bit image it decodes is the whole picture.
    let eight_bit;
    let source = if !options.imageio && !options.keep_bit_depth && bit_depth(bytes) == Some(16) {
        eight_bit = encode(&before)?;
        &eight_bit
    } else {
        bytes
    };
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
    oxi.max_decompressed_size = Some(MAX_DECODED_BYTES);
    if let Some(iteration_count) = NonZeroU64::new(zopfli_iterations(
        options.effort,
        before.width as usize * before.height as usize,
        ZOPFLI_PIXELS,
    )) {
        oxi.deflater = oxipng::Deflater::Zopfli(oxipng::ZopfliOptions {
            iteration_count,
            ..Default::default()
        });
    }
    let out = oxipng::optimize_from_memory(source, &oxi).map_err(|e| invalid("PNG", e))?;
    if decode(&out)? != before {
        return Err(invalid("PNG", "optimisation changed client-decoded pixels"));
    }
    Ok(if out.len() < bytes.len() {
        out
    } else {
        bytes.to_vec()
    })
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
}
