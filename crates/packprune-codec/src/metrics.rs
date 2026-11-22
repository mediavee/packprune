use std::f64::consts::{PI, TAU};

use rustfft::FftPlanner;
use rustfft::num_complex::Complex;

use crate::audio::{Pcm, downmix};
use crate::png::{Image, validate};
use crate::{CodecError, MAX_DECODED_BYTES, invalid};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageLoss {
    pub ssim: f64,
    pub max_delta: u8,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SoundLoss {
    pub lsd: f64,
    pub snr: Option<f64>,
    pub pcm_before: u64,
    pub pcm_after: u64,
}

/// SSIM over a `width`×`height` grid: nearest sampling, premultiplied channels, uniform 7x7 windows.
pub fn image_loss(
    before: &Image,
    after: &Image,
    width: u32,
    height: u32,
) -> Result<ImageLoss, CodecError> {
    validate(before)?;
    validate(after)?;
    if width == 0
        || height == 0
        || u128::from(width) * u128::from(height) * 8 > MAX_DECODED_BYTES as u128
    {
        return Err(invalid("SSIM", "invalid sampling grid"));
    }
    let width = width as usize;
    let height = height as usize;
    let w = width.min(7);
    let h = height.min(7);
    let out_w = width - w + 1;
    let inv_n = 1.0 / (w * h) as f64;
    let windows = (out_w * (height - h + 1)) as f64;
    let c1 = (0.01f64 * 255.0).powi(2);
    let c2 = (0.03f64 * 255.0).powi(2);

    // Precompute nearest-neighbor X source indices once for both images.
    let map_axis = |src_len: u32, dst_len: usize| -> Vec<usize> {
        let src_f = f64::from(src_len);
        let dst_f = dst_len as f64;
        let max_idx = src_len as usize - 1;
        (0..dst_len)
            .map(|i| (((i as f64 + 0.5) * src_f / dst_f) as usize).min(max_idx))
            .collect()
    };
    let sx_a = map_axis(before.width, width);
    let sy_a = map_axis(before.height, height);
    let sx_b = map_axis(after.width, width);
    let sy_b = map_axis(after.height, height);

    let mut ssim = 0.0;
    let mut max_delta = 0.0f64;
    // Rolling ring buffer of `h` rows of 5 horizontal 7-wide sums: [sum_a, sum_b, sum_a2, sum_b2, sum_ab].
    // Uses ~1 MiB even for 4096x4096 (vs 256 MiB for full-plane f64 buffers) and O(w+h) per pixel (~50x faster).
    let mut ring = vec![[0.0f64; 5]; h * out_w];
    let mut row_a = vec![0.0f64; width];
    let mut row_b = vec![0.0f64; width];
    let bw = before.width as usize;
    let aw = after.width as usize;

    for channel in 0..4 {
        let mut total = 0.0;
        for y in 0..height {
            let base_a = sy_a[y] * bw;
            let base_b = sy_b[y] * aw;
            if channel == 3 {
                for x in 0..width {
                    let va = f64::from(before.rgba[(base_a + sx_a[x]) * 4 + 3]);
                    let vb = f64::from(after.rgba[(base_b + sx_b[x]) * 4 + 3]);
                    row_a[x] = va;
                    row_b[x] = vb;
                    max_delta = max_delta.max((va - vb).abs());
                }
            } else {
                for x in 0..width {
                    let pa = &before.rgba[(base_a + sx_a[x]) * 4..];
                    let pb = &after.rgba[(base_b + sx_b[x]) * 4..];
                    let va = f64::from(pa[channel]) * f64::from(pa[3]) / 255.0;
                    let vb = f64::from(pb[channel]) * f64::from(pb[3]) / 255.0;
                    row_a[x] = va;
                    row_b[x] = vb;
                    max_delta = max_delta.max((va - vb).abs());
                }
            }
            let slot = (y % h) * out_w;
            for x in 0..out_w {
                let (mut sa, mut sb, mut sa2, mut sb2, mut sab) = (0.0, 0.0, 0.0, 0.0, 0.0);
                for i in 0..w {
                    let va = row_a[x + i];
                    let vb = row_b[x + i];
                    sa += va;
                    sb += vb;
                    sa2 += va * va;
                    sb2 += vb * vb;
                    sab += va * vb;
                }
                ring[slot + x] = [sa, sb, sa2, sb2, sab];
            }
            if y + 1 >= h {
                for x in 0..out_w {
                    let (mut sa, mut sb, mut sa2, mut sb2, mut sab) = (0.0, 0.0, 0.0, 0.0, 0.0);
                    for j in 0..h {
                        let r = ring[j * out_w + x];
                        sa += r[0];
                        sb += r[1];
                        sa2 += r[2];
                        sb2 += r[3];
                        sab += r[4];
                    }
                    let mean_a = sa * inv_n;
                    let mean_b = sb * inv_n;
                    let var_a = (sa2 * inv_n - mean_a * mean_a).max(0.0);
                    let var_b = (sb2 * inv_n - mean_b * mean_b).max(0.0);
                    let cov = if var_a == 0.0 || var_b == 0.0 {
                        0.0
                    } else {
                        sab * inv_n - mean_a * mean_b
                    };
                    total += (2.0 * mean_a * mean_b + c1) * (2.0 * cov + c2)
                        / ((mean_a * mean_a + mean_b * mean_b + c1) * (var_a + var_b + c2));
                }
            }
        }
        ssim += (total / windows) / 4.0;
    }
    Ok(ImageLoss {
        ssim,
        max_delta: max_delta.round() as u8,
    })
}

pub fn sound_loss(before: &Pcm, after: &Pcm) -> Result<SoundLoss, CodecError> {
    before.validate()?;
    after.validate()?;
    let mixed;
    let reference = if before.channels.len() == 2 && after.channels.len() == 1 {
        mixed = downmix(before)?;
        &mixed
    } else {
        before
    };
    let channels = reference.channels.len().min(after.channels.len());
    let mut lsd = 0.0;
    for c in 0..channels {
        lsd += channel_lsd(
            &reference.channels[c],
            reference.rate,
            &after.channels[c],
            after.rate,
        )?;
    }
    Ok(SoundLoss {
        lsd: lsd / channels as f64,
        snr: snr(reference, after),
        pcm_before: before.bytes(),
        pcm_after: after.bytes(),
    })
}

const FRAME_SECONDS: f64 = 2048.0 / 48000.0;
const FLOOR: f64 = 1e-9;

fn channel_lsd(x: &[f32], rx: u32, y: &[f32], ry: u32) -> Result<f64, CodecError> {
    let frame_len =
        |rate: u32| ((f64::from(rate) * FRAME_SECONDS).ceil() as usize).next_power_of_two();
    let nx = frame_len(rx);
    let ny = frame_len(ry);
    if nx > x.len() || ny > y.len() {
        return Err(invalid("LSD", "shorter than one analysis frame of 43 ms"));
    }
    let limit = (f64::from(rx.min(ry)) / 2.0).min(20000.0);
    let mut count = 1;
    while 100.0 * 2.0f64.powf(f64::from(count) / 3.0) <= limit {
        count += 1;
    }
    let edges: Vec<_> = (0..=count)
        .map(|i| (100.0 * 2.0f64.powf(f64::from(i) / 3.0)).min(limit))
        .collect();
    let mut planner = FftPlanner::<f64>::new();
    let fft_x = planner.plan_fft_forward(nx);
    let fft_y = planner.plan_fft_forward(ny);
    let mut total = 0.0;
    let mut frames = 0;
    for f in 0.. {
        let sx = (f as f64 * FRAME_SECONDS / 2.0 * f64::from(rx)).round() as usize;
        let sy = (f as f64 * FRAME_SECONDS / 2.0 * f64::from(ry)).round() as usize;
        if sx + nx > x.len() || sy + ny > y.len() {
            break;
        }
        let powers = |samples: &[f32], rate: u32, fft: &dyn rustfft::Fft<f64>| {
            let n = samples.len();
            let mut spectrum: Vec<_> = samples
                .iter()
                .enumerate()
                .map(|(i, &s)| {
                    Complex::new(
                        f64::from(s) * (0.5 - 0.5 * (TAU * i as f64 / n as f64).cos()),
                        0.0,
                    )
                })
                .collect();
            fft.process(&mut spectrum);
            let mut powers = vec![-1.0f64; edges.len() - 1];
            for (bin, value) in spectrum.iter().enumerate().take(n / 2).skip(1) {
                let hz = bin as f64 * f64::from(rate) / n as f64;
                if hz < edges[0] || hz >= edges[edges.len() - 1] {
                    continue;
                }
                let band = edges.partition_point(|&edge| edge <= hz) - 1;
                powers[band] = powers[band].max(0.0) + value.norm_sqr() / (n as f64 * n as f64);
            }
            powers
        };
        let px = powers(&x[sx..sx + nx], rx, fft_x.as_ref());
        let py = powers(&y[sy..sy + ny], ry, fft_y.as_ref());
        let mut silent = true;
        let mut square = 0.0;
        let mut counted = 0;
        for (&a, &b) in px.iter().zip(&py) {
            if a < 0.0 || b < 0.0 {
                continue;
            }
            silent &= a < FLOOR && b < FLOOR;
            let delta = 10.0 * (a.max(FLOOR) / b.max(FLOOR)).log10();
            square += delta * delta;
            counted += 1;
        }
        if !silent && counted > 0 {
            total += (square / f64::from(counted)).sqrt();
            frames += 1;
        }
    }
    Ok(if frames == 0 {
        0.0
    } else {
        total / f64::from(frames)
    })
}

fn snr(x: &Pcm, y: &Pcm) -> Option<f64> {
    if x.rate < y.rate || !x.rate.is_multiple_of(y.rate) {
        return None;
    }
    let ratio = (x.rate / y.rate) as usize;
    let reference: Vec<_> = x
        .channels
        .iter()
        .take(y.channels.len())
        .map(|c| {
            if ratio == 1 {
                c.clone()
            } else {
                decimate(c, ratio)
            }
        })
        .collect();
    let lag = lag(&reference[0], &y.channels[0]);
    let mut signal = 0.0;
    let mut noise = 0.0;
    for (a, b) in reference.iter().zip(&y.channels) {
        for (i, &sample) in b.iter().enumerate().skip((-lag).max(0) as usize) {
            let j = (i as isize + lag) as usize;
            if j >= a.len() {
                break;
            }
            let s = f64::from(a[j]);
            signal += s * s;
            noise += (s - f64::from(sample)).powi(2);
        }
    }
    (signal != 0.0).then(|| 10.0 * (signal / noise.max(1e-20)).log10())
}

fn lag(reference: &[f32], test: &[f32]) -> isize {
    let len = reference.len().min(test.len());
    let span = (1 << 15).min(len.saturating_sub(1024));
    if span == 0 {
        return 0;
    }
    let from = 512.min((len - span) / 2);
    let mut best = 0;
    let mut best_score = f64::NEG_INFINITY;
    for lag in -512isize..=512 {
        let mut score = 0.0;
        for (i, &sample) in test.iter().enumerate().skip(from).take(span) {
            let j = i as isize + lag;
            if j >= 0 && j < reference.len() as isize {
                score += f64::from(reference[j as usize]) * f64::from(sample);
            }
        }
        if score > best_score {
            best_score = score;
            best = lag;
        }
    }
    best
}

fn decimate(signal: &[f32], ratio: usize) -> Vec<f32> {
    let half = 16 * ratio;
    let cutoff = 0.9 / (2.0 * ratio as f64);
    let taps: Vec<_> = (-(half as isize)..=half as isize)
        .map(|i| {
            let i = i as f64;
            let sinc = if i == 0.0 {
                2.0 * cutoff
            } else {
                (TAU * cutoff * i).sin() / (PI * i)
            };
            sinc * (0.42
                + 0.5 * (PI * i / half as f64).cos()
                + 0.08 * (TAU * i / half as f64).cos())
        })
        .collect();
    let sum: f64 = taps.iter().sum();
    (0..signal.len() / ratio)
        .map(|o| {
            let centre = o * ratio;
            let from = half.saturating_sub(centre);
            let to = taps.len().min(signal.len() + half - centre);
            let acc: f64 = (from..to)
                .map(|j| f64::from(signal[centre + j - half]) * taps[j])
                .sum();
            (acc / sum) as f32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::tone;

    #[test]
    fn raster_uses_premultiplied_channels_and_population_variance() {
        let black = Image {
            width: 1,
            height: 1,
            rgba: vec![0, 0, 0, 255],
        };
        let white = Image {
            width: 1,
            height: 1,
            rgba: vec![255; 4],
        };
        let loss = image_loss(&black, &white, 1, 1).unwrap();
        assert!((loss.ssim - (0.25 + 0.75 / 10001.0)).abs() < 1e-12);
        assert_eq!(loss.max_delta, 255);
        let hidden_a = Image {
            rgba: vec![5, 33, 255, 0],
            ..black
        };
        let hidden_b = Image {
            rgba: vec![0; 4],
            ..black
        };
        assert_eq!(
            image_loss(&hidden_a, &hidden_b, 1, 1).unwrap(),
            ImageLoss {
                ssim: 1.0,
                max_delta: 0
            }
        );
    }

    #[test]
    fn sound_identity_silence_short_frames_and_snr_scale() {
        let x = tone(48000, 1);
        let same = sound_loss(&x, &x).unwrap();
        assert_eq!(same.lsd, 0.0);
        assert!(same.snr.unwrap() > 100.0);
        let mut half = x.clone();
        for s in &mut half.channels[0] {
            *s *= 0.5;
        }
        assert!((sound_loss(&x, &half).unwrap().snr.unwrap() - 6.020599913279624).abs() < 1e-6);
        let silence = Pcm {
            rate: 48000,
            channels: vec![vec![0.0; 4096]],
        };
        assert_eq!(sound_loss(&silence, &silence).unwrap().lsd, 0.0);
        let short = Pcm {
            rate: 48000,
            channels: vec![vec![0.0; 100]],
        };
        assert!(sound_loss(&short, &short).is_err());
    }

    #[test]
    fn stereo_mono_and_noninteger_rate_ratio() {
        let x = tone(48000, 2);
        let mono = downmix(&x).unwrap();
        assert_eq!(sound_loss(&x, &mono).unwrap().lsd, 0.0);
        let other_rate = tone(44100, 1);
        assert!(sound_loss(&x, &other_rate).unwrap().snr.is_none());
    }

    #[test]
    fn raster_windows_and_nearest_grid_match_reference_values() {
        let pixels = |n: u32, alpha, red, green, blue| {
            (0..n)
                .flat_map(|i| {
                    [
                        (i * red % 256) as u8,
                        (i * green % 256) as u8,
                        (i * blue % 256) as u8,
                        (i * alpha % 256) as u8,
                    ]
                })
                .collect()
        };
        let before = Image {
            width: 8,
            height: 8,
            rgba: pixels(64, 37, 11, 29, 7),
        };
        let after = Image {
            width: 4,
            height: 4,
            rgba: pixels(16, 41, 17, 19, 5),
        };
        for (grid, ssim, delta) in [
            (4, -0.1728385136263877, 197),
            (8, -0.0012430986646395975, 237),
        ] {
            let loss = image_loss(&before, &after, grid, grid).unwrap();
            assert!((loss.ssim - ssim).abs() < 1e-12);
            assert_eq!(loss.max_delta, delta);
        }
    }

    #[test]
    fn audio_fixtures_match_reference_metrics() {
        let stereo = include_bytes!("../../../tests/fixtures/sounds/stereo_44k_6s.ogg");
        let mono = include_bytes!("../../../tests/fixtures/sounds/stereo_44k_6s.as_mono.ogg");
        let high = include_bytes!("../../../tests/fixtures/sounds/stereo_96k_1s.ogg");
        let low = include_bytes!("../../../tests/fixtures/sounds/stereo_96k_1s.as_48k.ogg");
        for (before, after, lsd, snr) in [
            (
                stereo.as_slice(),
                mono.as_slice(),
                0.49727619904643927,
                32.813463616437446,
            ),
            (
                high.as_slice(),
                low.as_slice(),
                1.2214561646251108,
                10.216400437705754,
            ),
        ] {
            let loss = sound_loss(
                &crate::audio::decode(before).unwrap(),
                &crate::audio::decode(after).unwrap(),
            )
            .unwrap();
            assert!((loss.lsd - lsd).abs() < 1e-3, "{loss:?}");
            assert!((loss.snr.unwrap() - snr).abs() < 1e-3, "{loss:?}");
        }
    }
}
