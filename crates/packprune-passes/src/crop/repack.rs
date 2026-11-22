//! Deterministic guillotine packing of disjoint model-face UV islands into the smallest power-of-two (mip-aligned)
//! texture that keeps every rescaled UV coordinate exact in f32 (matching the oracle's 1/4096 texel quantization).

use std::cmp::Reverse;

use packprune_codec::png::Image;

/// Matches `packprune-oracle`: texel coordinates are compared to 1/4096 of a texel.
const TEXEL_STEP: f64 = 4096.0;

fn quantize(texels: f64) -> f64 {
    (texels * TEXEL_STEP).round_ties_even() / TEXEL_STEP
}

/// One axis-aligned texel rectangle `[x0..x1, y0..y1]` sampled by a group of faces.
#[derive(Clone, Debug)]
struct Island {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    faces: Vec<usize>,
}

impl Island {
    fn width(&self) -> i32 {
        self.x1 - self.x0
    }

    fn height(&self) -> i32 {
        self.y1 - self.y0
    }

    fn intersects(&self, other: &Self) -> bool {
        self.x0 < other.x1 && other.x0 < self.x1 && self.y0 < other.y1 && other.y0 < self.y1
    }
}

/// A verified repack plan computed purely from dimensions and face UVs (before decoding the PNG).
pub(super) struct Plan {
    pub width: i32,
    pub height: i32,
    pub uvs: Vec<[f32; 4]>,
    islands: Vec<Island>,
    placements: Vec<(i32, i32)>,
}

impl Plan {
    /// Blits the planned islands from `image` into the new canvas.
    pub(super) fn apply(&self, image: &Image) -> Image {
        let orig_w = image.width as usize;
        let cw = self.width as usize;
        let ch = self.height as usize;
        let mut rgba = vec![0u8; cw * ch * 4];
        for (isle, &(dx, dy)) in self.islands.iter().zip(&self.placements) {
            let row_bytes = (isle.width() as usize) * 4;
            for row in 0..isle.height() {
                let src = (((isle.y0 + row) as usize) * orig_w + (isle.x0 as usize)) * 4;
                let dst = (((dy + row) as usize) * cw + (dx as usize)) * 4;
                rgba[dst..dst + row_bytes].copy_from_slice(&image.rgba[src..src + row_bytes]);
            }
        }
        Image {
            width: self.width as u32,
            height: self.height as u32,
            rgba,
        }
    }
}

/// Attempts to plan a deterministic repack of all faces' sampled texel boxes from a static `(width, height)` texture
/// into a strictly smaller power-of-two `(new_w, new_h)` texture (aligned to `align`), verifying exact f32 UV
/// roundtrip under the oracle's 1/4096 texel quantization for every face coordinate.
pub(super) fn plan(width: i32, height: i32, faces_uv: &[[f32; 4]], align: i32) -> Option<Plan> {
    if faces_uv.is_empty() || align <= 0 || width <= 0 || height <= 0 {
        return None;
    }

    // Build initial per-face texel boxes aligned to `align`.
    let mut islands: Vec<Island> = Vec::with_capacity(faces_uv.len());
    for (idx, uv) in faces_uv.iter().enumerate() {
        let u_min = uv[0].min(uv[2]);
        let u_max = uv[0].max(uv[2]);
        let v_min = uv[1].min(uv[3]);
        let v_max = uv[1].max(uv[3]);
        // Reject out-of-range UVs and right/bottom-edge degenerate UVs (which clip differently when moved).
        if u_min < 0.0
            || v_min < 0.0
            || u_max > 16.0
            || v_max > 16.0
            || u_min >= 16.0
            || v_min >= 16.0
        {
            return None;
        }
        let tx0 = quantize(f64::from(u_min) * f64::from(width) / 16.0).floor() as i32;
        let ty0 = quantize(f64::from(v_min) * f64::from(height) / 16.0).floor() as i32;
        let tx1 = (tx0 + 1).max(quantize(f64::from(u_max) * f64::from(width) / 16.0).ceil() as i32);
        let ty1 =
            (ty0 + 1).max(quantize(f64::from(v_max) * f64::from(height) / 16.0).ceil() as i32);
        if tx0 < 0 || ty0 < 0 || tx1 > width || ty1 > height {
            return None;
        }
        let x0 = (tx0 / align) * align;
        let y0 = (ty0 / align) * align;
        let x1 = ((tx1 + align - 1) / align * align).min(width);
        let y1 = ((ty1 + align - 1) / align * align).min(height);
        if x1 <= x0 || y1 <= y0 || (x1 - x0) % align != 0 || (y1 - y0) % align != 0 {
            return None;
        }
        islands.push(Island {
            x0,
            y0,
            x1,
            y1,
            faces: vec![idx],
        });
    }

    // Merge overlapping islands into disjoint axis-aligned rectangles.
    let mut merged = true;
    while merged {
        merged = false;
        'outer: for i in 0..islands.len() {
            for j in (i + 1)..islands.len() {
                if islands[i].intersects(&islands[j]) {
                    let b = islands.remove(j);
                    let a = &mut islands[i];
                    a.x0 = a.x0.min(b.x0);
                    a.y0 = a.y0.min(b.y0);
                    a.x1 = a.x1.max(b.x1);
                    a.y1 = a.y1.max(b.y1);
                    a.faces.extend(b.faces);
                    merged = true;
                    break 'outer;
                }
            }
        }
    }

    // Sort deterministically: largest height, largest width, then original coordinates.
    islands.sort_by_key(|isle| {
        (
            Reverse(isle.height()),
            Reverse(isle.width()),
            isle.y0,
            isle.x0,
        )
    });

    let min_area: i64 = islands
        .iter()
        .map(|i| i64::from(i.width()) * i64::from(i.height()))
        .sum();
    let orig_area = i64::from(width) * i64::from(height);
    if min_area >= orig_area {
        return None;
    }

    let max_isle_w = islands.iter().map(Island::width).max().unwrap_or(1);
    let max_isle_h = islands.iter().map(Island::height).max().unwrap_or(1);
    let min_side = align.max(1);

    // Enumerate power-of-two candidate canvases `(cw, ch)` strictly smaller in area than `(width, height)`.
    let mut candidates: Vec<(i32, i32)> = Vec::new();
    let next_pow2 = |v: i32| (v.max(1) as u32).next_power_of_two() as i32;
    let max_dim = next_pow2(width.max(height));
    let mut cw = next_pow2(min_side);
    while cw <= max_dim {
        let mut ch = next_pow2(min_side);
        while ch <= max_dim {
            let area = i64::from(cw) * i64::from(ch);
            if cw >= max_isle_w
                && ch >= max_isle_h
                && area >= min_area
                && area < orig_area
                && cw % align == 0
                && ch % align == 0
            {
                candidates.push((cw, ch));
            }
            if ch >= 16384 {
                break;
            }
            ch *= 2;
        }
        if cw >= 16384 {
            break;
        }
        cw *= 2;
    }
    candidates.sort_by_key(|&(cw, ch)| (i64::from(cw) * i64::from(ch), cw.max(ch), cw));

    for (cw, ch) in candidates {
        let Some(placements) = pack_guillotine(&islands, cw, ch) else {
            continue;
        };
        let Some(new_uvs) = remap_uvs(faces_uv, &islands, &placements, width, height, cw, ch)
        else {
            continue;
        };
        return Some(Plan {
            width: cw,
            height: ch,
            uvs: new_uvs,
            islands,
            placements,
        });
    }
    None
}

/// Exact f32 UV remapping check: guarantees every remapped corner matches the oracle's exact 1/4096 quantized
/// texel offset and stays within `[0, 16]`.
fn remap_uvs(
    faces_uv: &[[f32; 4]],
    islands: &[Island],
    placements: &[(i32, i32)],
    orig_w: i32,
    orig_h: i32,
    new_w: i32,
    new_h: i32,
) -> Option<Vec<[f32; 4]>> {
    let mut out = vec![[0f32; 4]; faces_uv.len()];
    let remap_coord = |uv: f32, orig_dim: i32, src0: i32, dst0: i32, new_dim: i32| -> Option<f32> {
        let orig_texel = f64::from(uv) * f64::from(orig_dim) / 16.0;
        let rel = orig_texel - f64::from(src0);
        let new_texel = f64::from(dst0) + rel;
        let new_uv = (new_texel * 16.0 / f64::from(new_dim)) as f32;
        if !new_uv.is_finite() || !(0.0..=16.0).contains(&new_uv) {
            return None;
        }
        let check_texel = f64::from(new_uv) * f64::from(new_dim) / 16.0;
        let check_rel = check_texel - f64::from(dst0);
        // Verify exact oracle quantization equality for both floor/ceil bounds and sub-texel corners.
        if quantize(check_rel) != quantize(rel) {
            return None;
        }
        if quantize(check_texel).floor() as i32 - dst0 != quantize(orig_texel).floor() as i32 - src0
        {
            return None;
        }
        if quantize(check_texel).ceil() as i32 - dst0 != quantize(orig_texel).ceil() as i32 - src0 {
            return None;
        }
        Some(new_uv)
    };
    for (isle, &(dx, dy)) in islands.iter().zip(placements) {
        for &face_idx in &isle.faces {
            let uv = faces_uv[face_idx];
            let u0 = remap_coord(uv[0], orig_w, isle.x0, dx, new_w)?;
            let v0 = remap_coord(uv[1], orig_h, isle.y0, dy, new_h)?;
            let u1 = remap_coord(uv[2], orig_w, isle.x0, dx, new_w)?;
            let v1 = remap_coord(uv[3], orig_h, isle.y0, dy, new_h)?;
            out[face_idx] = [u0, v0, u1, v1];
        }
    }
    Some(out)
}

#[derive(Clone, Copy)]
struct Rect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

/// Deterministic guillotine bin packer (Best Short Side Fit).
fn pack_guillotine(islands: &[Island], canvas_w: i32, canvas_h: i32) -> Option<Vec<(i32, i32)>> {
    let mut free = vec![Rect {
        x: 0,
        y: 0,
        w: canvas_w,
        h: canvas_h,
    }];
    let mut placements = Vec::with_capacity(islands.len());
    for isle in islands {
        let (iw, ih) = (isle.width(), isle.height());
        let mut best: Option<(usize, i32, i32, i32, i32)> = None;
        for (i, r) in free.iter().enumerate() {
            if iw <= r.w && ih <= r.h {
                let short = (r.w - iw).min(r.h - ih);
                let long = (r.w - iw).max(r.h - ih);
                let key = (short, long, r.y, r.x);
                if best.is_none_or(|b| key < (b.1, b.2, b.3, b.4)) {
                    best = Some((i, short, long, r.y, r.x));
                }
            }
        }
        let (idx, ..) = best?;
        let chosen = free.swap_remove(idx);
        placements.push((chosen.x, chosen.y));
        let rw = chosen.w - iw;
        let rh = chosen.h - ih;
        if rw > 0 && rh > 0 {
            if rw <= rh {
                free.push(Rect {
                    x: chosen.x + iw,
                    y: chosen.y,
                    w: rw,
                    h: ih,
                });
                free.push(Rect {
                    x: chosen.x,
                    y: chosen.y + ih,
                    w: chosen.w,
                    h: rh,
                });
            } else {
                free.push(Rect {
                    x: chosen.x,
                    y: chosen.y + ih,
                    w: iw,
                    h: rh,
                });
                free.push(Rect {
                    x: chosen.x + iw,
                    y: chosen.y,
                    w: rw,
                    h: chosen.h,
                });
            }
        } else if rw > 0 {
            free.push(Rect {
                x: chosen.x + iw,
                y: chosen.y,
                w: rw,
                h: chosen.h,
            });
        } else if rh > 0 {
            free.push(Rect {
                x: chosen.x,
                y: chosen.y + ih,
                w: chosen.w,
                h: rh,
            });
        }
    }
    Some(placements)
}
