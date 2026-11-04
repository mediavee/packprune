//! The crop box of one texture axis, in exact integer arithmetic: a texel coordinate is an integer over `2^SCALE`,
//! which every UV a float holds on a frame of at most `2^31` texels is, or the axis is not cropped.

/// Texel coordinates are held as integers over `2^SCALE`; an axis with a UV finer than that is not cropped.
const SCALE: u32 = 32;
const FLOAT_BITS: u32 = 24;

/// Where one axis of a frame is cropped: `[start, start + size)`. UVs are in 1/16 of the frame, so a face's texel
/// coordinate `p` is written `(p - start) * 16 / size` after the crop; the box is chosen so that value is a float,
/// which lands on exactly the texel coordinate it did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CropBox {
    pub start: i32,
    pub size: i32,
}

/// One face's two UVs on this axis, as the floats the client reads.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Span {
    pub from: f32,
    pub to: f32,
}

impl CropBox {
    /// `uv` rescaled to this box of a frame `frame` texels wide; `None` when that is not exactly a float, which
    /// [`Axis::at`] never returns.
    pub(crate) fn rescale(self, uv: f32, frame: i32) -> Option<f32> {
        // (texel - start) * 16 / size, with texel = fixed / 2^SCALE.
        let numerator = (fixed(uv, frame)? - (i128::from(self.start) << SCALE)) * 16;
        let denominator = i128::from(self.size) << SCALE;
        let divisor = gcd(numerator, denominator);
        let (numerator, denominator) = (numerator / divisor, denominator / divisor);
        if !denominator.unsigned_abs().is_power_of_two() || numerator.unsigned_abs() >= 1 << 53 {
            return None;
        }
        let value = (numerator as f64 / denominator as f64) as f32;
        (f64::from(value) * denominator as f64 == numerator as f64).then_some(value)
    }

    /// The faces' UVs on one axis of a frame `frame` texels wide; `None` when a face samples outside the frame or a
    /// UV is finer than the scale.
    pub(crate) fn axis(spans: &[Span], frame: i32) -> Option<Axis> {
        let (mut low, mut high) = (i32::MAX, i32::MIN);
        let mut scaled = Vec::with_capacity(spans.len() * 2);
        for span in spans {
            let a = fixed(span.from, frame)?;
            let b = fixed(span.to, frame)?;
            // The texels nearest sampling reads: from the floor of the smaller coordinate to the ceiling of the larger.
            let from = i32::try_from(a.min(b) >> SCALE).ok()?;
            let to = i32::try_from(-((-a.max(b)) >> SCALE)).ok()?;
            low = low.min(from);
            high = high.max((from + 1).max(to));
            scaled.push(i64::try_from(a).ok()?);
            scaled.push(i64::try_from(b).ok()?);
        }
        if low < 0 || high > frame {
            return None;
        }
        let gcd = scaled
            .iter()
            .fold(0, |gcd, value| gcd64(gcd, value - scaled[0]));
        Some(Axis {
            low,
            high,
            frame,
            scaled,
            gcd,
        })
    }
}

/// `uv * frame / 16 * 2^SCALE`, the texel coordinate on the fixed scale; `None` when it is not an integer there.
fn fixed(uv: f32, frame: i32) -> Option<i128> {
    if !uv.is_finite() {
        return None;
    }
    if uv == 0.0 {
        return Some(0);
    }
    let bits = uv.to_bits();
    let exponent = ((bits >> 23) & 0xff) as i32;
    let fraction = i128::from(bits & 0x7f_ffff);
    let (mantissa, exponent) = if exponent == 0 {
        (fraction, -149)
    } else {
        (fraction | 1 << 23, exponent - 150)
    };
    let sign = if bits >> 31 == 1 { -1 } else { 1 };
    // mantissa * 2^exponent * frame / 16 * 2^SCALE
    let value = mantissa * i128::from(frame);
    let shift = exponent - 4 + SCALE as i32;
    let value = if shift >= 0 {
        value.checked_mul(1i128.checked_shl(shift as u32)?)?
    } else {
        let shift = (-shift) as u32;
        if shift >= 127 || value & ((1 << shift) - 1) != 0 {
            return None;
        }
        value >> shift
    };
    Some(sign * value)
}

fn round_up(value: i32, multiple: i32) -> i32 {
    (value + multiple - 1) / multiple * multiple
}

fn gcd64(a: i64, b: i64) -> i64 {
    if b == 0 { a.abs() } else { gcd64(b, a % b) }
}

fn gcd(a: i128, b: i128) -> i128 {
    if b == 0 { a.abs() } else { gcd(b, a % b) }
}

/// `[low, high)`: the texels the faces sample; `scaled`: each UV's texel coordinate times `2^SCALE`; `gcd`: of
/// their differences.
#[derive(Clone, Debug)]
pub(crate) struct Axis {
    pub low: i32,
    pub high: i32,
    frame: i32,
    scaled: Vec<i64>,
    gcd: i64,
}

impl Axis {
    /// The smallest exact box at least `min_size` wide whose start and size are multiples of `align`; `None` when only
    /// the whole frame is.
    pub(crate) fn smallest(&self, min_size: i32, align: i32) -> Option<CropBox> {
        let mut size = round_up(min_size.max(self.high - self.low), align);
        while size < self.frame {
            if let Some(found) = self.at(size, align) {
                return Some(found);
            }
            size += align;
        }
        None
    }

    /// The exact box of `size`, if there is one. A coordinate rescaled to it is a binary fraction only when the odd
    /// part of `size` divides the coordinate shifted by `start`, so it must divide every difference and fixes `start`
    /// modulo itself; the float must then hold it in 24 significant bits.
    pub(crate) fn at(&self, size: i32, align: i32) -> Option<CropBox> {
        if size < self.high - self.low || size > self.frame || size % align != 0 {
            return None;
        }
        let odd = size >> size.trailing_zeros();
        if self.gcd % i64::from(odd) != 0 {
            return None;
        }
        let residue = if odd == 1 {
            0
        } else {
            let odd = i128::from(odd);
            let inverse = inverse((1i128 << SCALE) % odd, odd);
            (i128::from(self.scaled[0]) * inverse).rem_euclid(odd) as i32
        };
        let first = round_up((self.high - size).max(0), align);
        let last = self.low.min(self.frame - size);
        let mut start = first;
        while start <= last {
            if (start - residue).rem_euclid(odd) == 0 && self.fits(start, odd) {
                return Some(CropBox { start, size });
            }
            start += align;
        }
        None
    }

    fn fits(&self, start: i32, odd: i32) -> bool {
        self.scaled.iter().all(|&value| {
            let numerator = (i128::from(value) - (i128::from(start) << SCALE)) / i128::from(odd);
            let significant = if numerator == 0 {
                0
            } else {
                numerator.unsigned_abs() >> numerator.trailing_zeros()
            };
            128 - significant.leading_zeros() <= FLOAT_BITS
        })
    }
}

/// The inverse of `value` modulo the odd `modulus`, by the extended Euclidean algorithm.
fn inverse(value: i128, modulus: i128) -> i128 {
    let (mut old_r, mut r) = (value, modulus);
    let (mut old_s, mut s) = (1i128, 0i128);
    while r != 0 {
        let quotient = old_r / r;
        (old_r, r) = (r, old_r - quotient * r);
        (old_s, s) = (s, old_s - quotient * s);
    }
    old_s.rem_euclid(modulus)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_coordinate_is_exact_on_the_fixed_scale_or_refused() {
        assert_eq!(fixed(4.5, 16), Some(9 << 31));
        assert_eq!(fixed(-1.0, 16), Some(-(1 << 32)));
        assert_eq!(fixed(1e-30, 16), None);
        assert_eq!(fixed(f32::NAN, 16), None);
    }

    /// On 48 texels, x samples 0, 1.5 and 40.5, and no
    /// width from 41 to 47 holds all three as floats.
    #[test]
    fn no_box_holds_coordinates_whose_rescale_needs_more_than_a_float() {
        let spans = [
            Span { from: 0.0, to: 0.5 },
            Span {
                from: 0.5,
                to: 13.5,
            },
        ];
        let axis = CropBox::axis(&spans, 48).unwrap();
        assert_eq!((axis.low, axis.high), (0, 41));
        assert_eq!(axis.smallest(1, 1), None);
    }

    /// A size of odd part 3 divides both coordinates, but the second over 3 needs 26 significant bits.
    #[test]
    fn a_box_is_refused_when_a_float_cannot_hold_a_rescaled_coordinate() {
        let axis = |second: i64| Axis {
            low: 0,
            high: 1,
            frame: 6,
            scaled: vec![0, second],
            gcd: second,
        };
        assert_eq!(axis(3 * ((1 << 25) + 1)).at(3, 1), None);
        assert_eq!(
            axis(3 * ((1 << 23) + 1)).at(3, 1),
            Some(CropBox { start: 0, size: 3 })
        );
    }

    #[test]
    fn a_box_rescales_each_coordinate_to_the_texel_it_named() {
        let spans = [Span { from: 4.0, to: 8.0 }];
        let axis = CropBox::axis(&spans, 16).unwrap();
        let found = axis.smallest(1, 1).unwrap();
        assert_eq!(found, CropBox { start: 4, size: 4 });
        assert_eq!(found.rescale(4.0, 16), Some(0.0));
        assert_eq!(found.rescale(8.0, 16), Some(16.0));
        let aligned = axis.smallest(1, 16);
        assert_eq!(aligned, None, "16 is the whole frame");
    }
}
