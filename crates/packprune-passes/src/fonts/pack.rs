//! font-pack: packs the small bitmap font providers of a font that share `(cell_width, cell_height, height, ascent)`
//! into grid PNG sheets: single-glyph providers and multi-glyph ones of up to [`MAX_SOURCE_CELLS`] cells. Every glyph
//! cell's exact RGBA texels (including RGB under alpha = 0 and alpha = 1), dimensions, `height`, `ascent`, `advance`,
//! and provider priority order are preserved verbatim — 100% lossless and compatible with custom `rendertype_text*`
//! core shaders.

use std::collections::{BTreeMap, BTreeSet};

use packprune_codec::png::{self, Image};
use packprune_pack::json::{self, Splice, Writer};
use packprune_pack::{FileKind, LogicalPath, PhysPath};
use packprune_pass::{Edits, Loss, Pass, PassCx, PassError, Stage};
use rayon::prelude::*;
use sha2::{Digest, Sha256};

use super::{Bitmap, Font, Readers, always, chars, provider_type};
use crate::textures::{Use, Views, no_options};

const NAME: &str = "font-pack";
const MAX_SHEET_DIM: u32 = 2048;
const MAX_CELL_DIM: u32 = 256;
/// Providers of up to this many cells, `\0` included, are packed; larger sheets are font-trim's.
const MAX_SOURCE_CELLS: usize = 32;
/// Approximate ZIP local + central header overhead per separate PNG file.
const ZIP_ENTRY_OVERHEAD: usize = 110;
/// Cells up to this texel area may pad up to 3 trailing empty `\u0000` slots when the count is prime.
const SMALL_CELL_MAX_TEXELS: u32 = 2048;
/// The folder under `assets/packprune/textures/` of the packed sheets: every byte of their path is paid twice in the
/// ZIP headers and once in the font file.
const SHEET_DIR: &str = "f";
/// Base36 digits of a packed sheet's name, about 52 bits of its digest.
const SHEET_NAME_LEN: u32 = 10;

pub(super) struct FontPack;

impl Pass for FontPack {
    fn name(&self) -> &'static str {
        NAME
    }

    fn stage(&self) -> Stage {
        Stage::Fonts
    }

    fn loss(&self) -> Loss {
        Loss::Lossless
    }

    fn options_schema(&self) -> schemars::Schema {
        no_options()
    }

    fn run(&self, cx: &PassCx<'_>) -> Result<Edits, PassError> {
        let views = Views::new(cx, NAME)?;
        let usage = Use::new(cx, &views);
        let readers = Readers::new(cx, &usage);
        let fonts = if atlas_lists_sheet_dir(cx) {
            Vec::new()
        } else {
            Font::all(cx)
        };

        // Precompute which textures have `.mcmeta` beside them in any layer.
        let mut has_meta: BTreeSet<String> = BTreeSet::new();
        for path in cx.pack.files.keys() {
            if let (_, Some(logical)) = cx.layout.split(path)
                && let Some(stem) = logical.as_str().strip_suffix(".mcmeta")
            {
                has_meta.insert(stem.to_owned());
            }
        }

        // Collect all safe packing groups across every font file.
        let mut plans: Vec<FontPlan<'_>> = Vec::new();
        let mut all_jobs: Vec<PackJob> = Vec::new();
        for font in &fonts {
            if !cx.scope.contains(font.path) || cx.pinned.contains(font.path) {
                continue;
            }
            let font_layer = cx.layout.split(font.path).0;
            let providers = font.providers();
            if providers.len() < 2 {
                continue;
            }

            // Precompute per-provider codepoint sets and barriers (any non-bitmap/non-space provider is a barrier).
            let mut provider_cps: Vec<Option<BTreeSet<char>>> = Vec::with_capacity(providers.len());
            for provider in providers {
                let kind = provider_type(provider);
                let cps = match kind {
                    "bitmap" => chars(provider).map(|rows| {
                        rows.into_iter()
                            .flatten()
                            .filter(|&c| c != '\0')
                            .collect::<BTreeSet<char>>()
                    }),
                    "space" => Some(
                        provider
                            .path("advances")
                            .members()
                            .iter()
                            .filter_map(|m| {
                                let mut it = m.name.chars();
                                let c = it.next()?;
                                it.next().is_none().then_some(c)
                            })
                            .collect::<BTreeSet<char>>(),
                    ),
                    _ => None,
                };
                provider_cps.push(cps);
            }

            // Split into contiguous segments between barrier providers (`reference`, `ttf`, `unihex`, unreadable).
            let mut seg_start = 0usize;
            let mut font_groups: Vec<usize> = Vec::new();
            while seg_start < providers.len() {
                if provider_cps[seg_start].is_none() {
                    seg_start += 1;
                    continue;
                }
                let mut seg_end = seg_start + 1;
                while seg_end < providers.len() && provider_cps[seg_end].is_some() {
                    seg_end += 1;
                }

                // Collect eligible small exclusive bitmap providers in `[seg_start..seg_end)`.
                let mut buckets: BTreeMap<(u32, u32, i32, i32), Vec<Member>> = BTreeMap::new();
                for (index, provider) in providers[seg_start..seg_end]
                    .iter()
                    .enumerate()
                    .map(|(off, p)| (seg_start + off, p))
                {
                    if !always(provider) {
                        continue;
                    }
                    let Some(bitmap) = Bitmap::read(cx, index, provider) else {
                        continue;
                    };
                    let Some(cells) = glyph_cells(&bitmap.rows) else {
                        continue;
                    };
                    if has_meta.contains(bitmap.texture.as_str()) {
                        continue;
                    }
                    if readers
                        .shared(cx, font.path, index, &bitmap.texture)
                        .is_some()
                    {
                        continue;
                    }
                    let Some(sheet) = font.resolve(cx, &bitmap.texture) else {
                        continue;
                    };
                    if cx.layout.split(sheet).0 != font_layer
                        || !cx.scope.contains(sheet)
                        || cx.pinned.contains(sheet)
                    {
                        continue;
                    }
                    let Some((width, height)) = ihdr_size(&cx.pack.files[sheet].blob) else {
                        continue;
                    };
                    // `Bitmap::read` checked the grid is a non-empty rectangle, `glyph_cells` its size.
                    let (cols, rows) = (bitmap.rows[0].len() as u32, bitmap.rows.len() as u32);
                    if !width.is_multiple_of(cols) || !height.is_multiple_of(rows) {
                        continue;
                    }
                    let (cell_w, cell_h) = (width / cols, height / rows);
                    if cell_w > MAX_CELL_DIM || cell_h > MAX_CELL_DIM {
                        continue;
                    }
                    buckets
                        .entry((cell_w, cell_h, bitmap.height, bitmap.ascent))
                        .or_default()
                        .push(Member {
                            index,
                            sheet: sheet.clone(),
                            cols,
                            rows,
                            cells,
                        });
                }

                for ((cell_w, cell_h, height, ascent), members) in buckets {
                    if members.len() < 2 {
                        continue;
                    }
                    // Keep members whose every codepoint no provider between the first member and them defines:
                    // moving them up to the first member's position then changes no glyph. An earlier member is in
                    // that range too, so a grid never repeats a codepoint.
                    let anchor = members[0].index;
                    let safe: Vec<Member> = members
                        .into_iter()
                        .filter(|member| {
                            member.cells.iter().all(|cell| {
                                !(anchor..member.index).any(|mid| {
                                    provider_cps[mid]
                                        .as_ref()
                                        .is_none_or(|cps| cps.contains(&cell.ch))
                                })
                            })
                        })
                        .collect();
                    let max_cols = (MAX_SHEET_DIM / cell_w).max(1);
                    let max_rows = (MAX_SHEET_DIM / cell_h).max(1);
                    let max_cells = (max_cols * max_rows) as usize;
                    let small = cell_w * cell_h <= SMALL_CELL_MAX_TEXELS;
                    // Greedy runs of consecutive members; a run that fits no grid drops trailing members, and a
                    // member no run can take stays where it is.
                    let mut rest = &safe[..];
                    while rest.len() >= 2 {
                        let mut take = 0;
                        let mut cells = 0;
                        while take < rest.len() && cells + rest[take].cells.len() <= max_cells {
                            cells += rest[take].cells.len();
                            take += 1;
                        }
                        let fitted = (2..=take).rev().find_map(|take| {
                            let run = &rest[..take];
                            let cells: usize = run.iter().map(|member| member.cells.len()).sum();
                            let source: usize = run
                                .iter()
                                .map(|member| (member.cols * member.rows) as usize)
                                .sum();
                            // Never more texels than the sheets replaced, but a few `\0` for small cells.
                            let limit = source.max(if small { cells + 3 } else { cells });
                            choose_grid(cells, limit, cell_w, cell_h, max_cols, max_rows)
                                .map(|grid| (take, grid))
                        });
                        let Some((take, (cols, rows))) = fitted else {
                            rest = &rest[1..];
                            continue;
                        };
                        font_groups.push(all_jobs.len());
                        all_jobs.push(PackJob {
                            font_prefix: layer_prefix(font.path, &font.logical).to_owned(),
                            cell_w,
                            cell_h,
                            cols,
                            rows,
                            height,
                            ascent,
                            members: rest[..take].to_vec(),
                        });
                        rest = &rest[take..];
                    }
                }
                seg_start = seg_end;
            }
            if !font_groups.is_empty() {
                plans.push(FontPlan {
                    font,
                    jobs: font_groups,
                });
            }
        }

        if all_jobs.is_empty() {
            let mut edits = Edits::default();
            edits.changes.push("0 font sheets packed into grids".into());
            return Ok(edits);
        }

        // Decode, blit, hash, and PNG-encode all candidate grids in parallel.
        let pack = cx.pack;
        let built: Vec<Option<BuiltGrid>> = all_jobs
            .par_iter()
            .map(|job| build_grid(job, pack))
            .collect();

        let mut edits = Edits::default();
        let mut used_paths: BTreeSet<PhysPath> = cx.pack.files.keys().cloned().collect();
        let (mut sheets_packed, mut grids_written, mut bytes_before, mut bytes_after) =
            (0usize, 0usize, 0usize, 0usize);

        for plan in &plans {
            let providers = plan.font.providers();
            let mut replaced: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
            let mut removed_idx: BTreeSet<usize> = BTreeSet::new();
            for &job_idx in &plan.jobs {
                let job = &all_jobs[job_idx];
                let Some(grid) = &built[job_idx] else {
                    continue;
                };
                let logical_rel = format!("{SHEET_DIR}/{}.png", grid.name);
                let logical_full = format!("assets/packprune/textures/{logical_rel}");
                let Ok(phys_path) = PhysPath::new(&format!("{}{logical_full}", job.font_prefix))
                else {
                    continue;
                };
                if used_paths.contains(&phys_path) || LogicalPath::new(&logical_full).is_err() {
                    continue;
                }
                let provider_json = serialize_provider(
                    &format!("packprune:{logical_rel}"),
                    job.height,
                    job.ascent,
                    job.cols as usize,
                    job.rows as usize,
                    &job.members,
                );
                // Profitability: PNG bytes, per-entry ZIP overhead, and any growth of the font file (one comma
                // per provider).
                let old_json: usize = job
                    .members
                    .iter()
                    .map(|member| {
                        let span = &providers[member.index].span;
                        (span.end - span.start) as usize + 1
                    })
                    .sum();
                let json_overhead = (provider_json.len() + 1).saturating_sub(old_json);
                let orig_cost = grid.orig_png_bytes + job.members.len() * ZIP_ENTRY_OVERHEAD;
                let new_cost = grid.png.len() + json_overhead + ZIP_ENTRY_OVERHEAD;
                if job.members.len() < 2 || new_cost >= orig_cost {
                    continue;
                }
                used_paths.insert(phys_path.clone());
                let anchor = job.members[0].index;
                replaced.insert(anchor, provider_json);
                for member in &job.members[1..] {
                    removed_idx.insert(member.index);
                }
                for member in &job.members {
                    edits.removed.insert(member.sheet.clone());
                }
                edits.added.insert(phys_path, grid.png.clone().into());
                sheets_packed += job.members.len();
                grids_written += 1;
                bytes_before += grid.orig_png_bytes;
                bytes_after += grid.png.len();
            }
            if !replaced.is_empty() {
                let bytes = rewrite_providers(plan.font, &replaced, &removed_idx);
                if bytes != plan.font.bytes {
                    edits.replaced.insert(plan.font.path.clone(), bytes.into());
                }
            }
        }

        edits.changes.push(format!(
            "{sheets_packed} font sheets packed into {grids_written} grids ({bytes_before} -> {bytes_after} B PNG)"
        ));
        Ok(edits)
    }
}

/// A non-`\0` cell of a provider's grid: its codepoint and where its texels are in the provider's sheet.
#[derive(Clone, Copy)]
struct Cell {
    ch: char,
    col: u32,
    row: u32,
}

/// A provider font-pack may merge: its exclusive sheet, the shape of its grid, and its glyphs in row-major order.
#[derive(Clone)]
struct Member {
    index: usize,
    sheet: PhysPath,
    cols: u32,
    rows: u32,
    cells: Vec<Cell>,
}

struct PackJob {
    font_prefix: String,
    cell_w: u32,
    cell_h: u32,
    cols: u32,
    rows: u32,
    height: i32,
    ascent: i32,
    members: Vec<Member>,
}

struct FontPlan<'a> {
    font: &'a Font<'a>,
    jobs: Vec<usize>,
}

struct BuiltGrid {
    png: Vec<u8>,
    name: String,
    orig_png_bytes: usize,
}

/// The glyph cells of a grid of at most [`MAX_SOURCE_CELLS`] cells; `None` when it has no glyph, or repeats a
/// codepoint, as the client then keeps only one of its cells.
fn glyph_cells(rows: &[Vec<char>]) -> Option<Vec<Cell>> {
    if rows.len().saturating_mul(rows.first()?.len()) > MAX_SOURCE_CELLS {
        return None;
    }
    let mut seen = BTreeSet::new();
    let mut cells = Vec::new();
    for (row, line) in rows.iter().enumerate() {
        for (col, &ch) in line.iter().enumerate() {
            if ch == '\0' {
                continue;
            }
            if !seen.insert(ch) {
                return None;
            }
            cells.push(Cell {
                ch,
                col: col as u32,
                row: row as u32,
            });
        }
    }
    (!cells.is_empty()).then_some(cells)
}

fn build_grid(job: &PackJob, pack: &packprune_pack::PackInput) -> Option<BuiltGrid> {
    let (width, height) = (job.cols * job.cell_w, job.rows * job.cell_h);
    let (cell_w, cell_h) = (job.cell_w as usize, job.cell_h as usize);
    let grid_w = width as usize;
    let mut rgba = vec![0u8; grid_w * height as usize * 4];
    let mut orig_png_bytes = 0usize;
    let row_bytes = cell_w * 4;
    let mut slot = 0usize;
    for member in &job.members {
        let blob = &pack.files.get(&member.sheet)?.blob;
        orig_png_bytes += blob.len();
        let img = png::decode(blob).ok()?;
        if img.width != member.cols * job.cell_w || img.height != member.rows * job.cell_h {
            return None;
        }
        for cell in &member.cells {
            let (src_x, src_y) = (cell.col as usize * cell_w, cell.row as usize * cell_h);
            let dst_x = (slot % job.cols as usize) * cell_w;
            let dst_y = (slot / job.cols as usize) * cell_h;
            for y in 0..cell_h {
                let src_off = ((src_y + y) * img.width as usize + src_x) * 4;
                let dst_off = ((dst_y + y) * grid_w + dst_x) * 4;
                rgba[dst_off..dst_off + row_bytes]
                    .copy_from_slice(&img.rgba[src_off..src_off + row_bytes]);
            }
            slot += 1;
        }
    }
    let mut hasher = Sha256::new();
    hasher.update(width.to_le_bytes());
    hasher.update(height.to_le_bytes());
    hasher.update(&rgba);
    let digest = hasher.finalize();
    let mut head = [0u8; 8];
    head.copy_from_slice(&digest[..8]);
    let encoded = png::encode(&Image {
        width,
        height,
        rgba,
    })
    .ok()?;
    Some(BuiltGrid {
        png: encoded,
        name: sheet_name(u64::from_be_bytes(head)),
        orig_png_bytes,
    })
}

/// `value` in [`SHEET_NAME_LEN`] lowercase base36 digits, its high digits dropped: a valid resource path.
fn sheet_name(value: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut value = value % 36u64.pow(SHEET_NAME_LEN);
    let mut name = [b'0'; SHEET_NAME_LEN as usize];
    for digit in name.iter_mut().rev() {
        *digit = DIGITS[(value % 36) as usize];
        value /= 36;
    }
    name.iter().map(|&digit| char::from(digit)).collect()
}

/// Chooses `(cols, rows)` for `n` cells of `(cell_w, cell_h)` fitting within `(max_cols, max_rows)` with at most
/// `limit` slots. Prefers an exact factorization (`cols * rows == n`), then the fewest trailing `\u0000` padding
/// slots; among equal slot counts, the squarest grid.
fn choose_grid(
    n: usize,
    limit: usize,
    cell_w: u32,
    cell_h: u32,
    max_cols: u32,
    max_rows: u32,
) -> Option<(u32, u32)> {
    let best_exact = |count: u32| -> Option<(u32, u32)> {
        let mut best: Option<(u32, u32, u32)> = None;
        for cols in 1..=max_cols.min(count) {
            if !count.is_multiple_of(cols) {
                continue;
            }
            let rows = count / cols;
            if rows > max_rows {
                continue;
            }
            let pw = cols * cell_w;
            let ph = rows * cell_h;
            let diff = pw.abs_diff(ph);
            if best.is_none_or(|b| (diff, pw.max(ph), cols) < (b.0, b.1, b.2)) {
                best = Some((diff, pw.max(ph), cols));
            }
        }
        best.map(|(_, _, cols)| (cols, count / cols))
    };

    // `max_cols` columns by `ceil(n / max_cols)` rows always fits when `n <= max_cols * max_rows`: no count past
    // `n + max_cols` needs a look.
    let last = limit.min(n + max_cols as usize);
    (n..=last).find_map(|count| best_exact(u32::try_from(count).ok()?))
}

fn serialize_provider(
    file_id: &str,
    height: i32,
    ascent: i32,
    cols: usize,
    rows: usize,
    members: &[Member],
) -> Vec<u8> {
    let mut glyphs = members
        .iter()
        .flat_map(|member| member.cells.iter().map(|cell| cell.ch));
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out);
    w.start_object();
    w.name("type");
    w.string("bitmap");
    w.name("file");
    w.string(file_id);
    if height != 8 {
        w.name("height");
        w.int(i64::from(height));
    }
    w.name("ascent");
    w.int(i64::from(ascent));
    w.name("chars");
    w.start_array();
    for _ in 0..rows {
        let line: String = (0..cols).map(|_| glyphs.next().unwrap_or('\0')).collect();
        w.string(&line);
    }
    w.end_array();
    w.end_object();
    out
}

fn rewrite_providers(
    font: &Font<'_>,
    replaced: &BTreeMap<usize, Vec<u8>>,
    removed: &BTreeSet<usize>,
) -> Vec<u8> {
    let array = font.root.path("providers");
    let providers = array.items();
    let mut out = b"[".to_vec();
    for (index, provider) in providers.iter().enumerate() {
        if removed.contains(&index) {
            continue;
        }
        if out.len() > 1 {
            out.push(b',');
        }
        if let Some(bytes) = replaced.get(&index) {
            out.extend_from_slice(bytes);
        } else {
            let (start, end) = (provider.span.start as usize, provider.span.end as usize);
            out.extend_from_slice(&font.bytes[start..end]);
        }
    }
    out.push(b']');
    json::splice(
        font.bytes,
        vec![Splice {
            range: array.span.start as usize..array.span.end as usize,
            bytes: out,
        }],
    )
}

/// Whether a pack atlas has a `directory` source listing `textures/` or `textures/f/`: it would stitch every sheet
/// this pass writes into that atlas.
fn atlas_lists_sheet_dir(cx: &PassCx<'_>) -> bool {
    cx.layout
        .kinds
        .iter()
        .filter(|(_, kind)| **kind == FileKind::Atlas)
        .filter_map(|(path, _)| json::parse_lenient(&cx.pack.files.get(path)?.blob).ok())
        .any(|root| {
            root.path("sources").items().iter().any(|source| {
                let kind = source.path("type").as_str().unwrap_or("");
                kind.strip_prefix("minecraft:").unwrap_or(kind) == "directory"
                    && source.path("source").as_str().is_some_and(|folder| {
                        matches!(folder.trim_end_matches('/'), "" | SHEET_DIR)
                    })
            })
        })
}

fn layer_prefix<'a>(phys: &'a PhysPath, logical: &LogicalPath) -> &'a str {
    let p = phys.as_str();
    let l = logical.as_str();
    p.strip_suffix(l).unwrap_or("")
}

fn ihdr_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}

#[cfg(test)]
mod pack_tests {
    use packprune_pack::json::{self, Node};

    use super::{FontPack, sheet_name};
    use crate::textures::tests::support::{FONT, Files, Fixture, argb, decode, png, put};

    const SHADER: &str = "assets/minecraft/shaders/core/rendertype_text.fsh";
    const PACKED: &str = "assets/packprune/textures/f/";
    const CELL: u32 = 8;
    /// What a `\0` cell of a source sheet holds: texels no glyph draws.
    const JUNK: u32 = 0xFF12_3456;

    /// The texel at `(x, y)` of the cell drawing `ch`: a width of its own, and coloured texels under alpha 0 and
    /// alpha 1 a text shader could read.
    fn texel(ch: char, x: u32, y: u32) -> u32 {
        let code = u32::from(ch);
        match (x, y) {
            (7, 7) => 0x00FC_FE00 | (code & 0xFF),
            (7, 6) => 0x01AB_CD00 | (code & 0xFF),
            _ if x <= code % 6 && y < 7 => 0xFF00_0040 | ((code & 0xFF) << 8),
            _ => 0,
        }
    }

    fn grid_png(rows: &[&str]) -> Vec<u8> {
        let grid: Vec<Vec<char>> = rows.iter().map(|row| row.chars().collect()).collect();
        let cols = grid[0].len() as u32;
        png(cols * CELL, grid.len() as u32 * CELL, |x, y| {
            match grid[(y / CELL) as usize][(x / CELL) as usize] {
                '\0' => JUNK,
                ch => texel(ch, x % CELL, y % CELL),
            }
        })
    }

    /// A pack whose font lists `providers`, each `(sheet name, JSON fields, chars)` with a sheet of 8x8 cells, and
    /// a text shader when `shader` holds.
    fn files(providers: &[(&str, &str, &[&str])], shader: bool) -> Files {
        let mut files = Files::new();
        put(
            &mut files,
            "pack.mcmeta",
            r#"{"pack": {"min_format": 88, "max_format": 88, "description": "t"}}"#,
        );
        if shader {
            put(&mut files, SHADER, "void main() {}");
        }
        let mut json = Vec::new();
        for (name, fields, rows) in providers {
            files.insert(
                format!("assets/fx/textures/font/{name}.png"),
                grid_png(rows),
            );
            let chars: Vec<String> = rows
                .iter()
                .map(|row| format!("\"{}\"", row.replace('\0', "\\u0000")))
                .collect();
            json.push(format!(
                r#"{{"type": "bitmap", "file": "fx:font/{name}.png", {fields}, "chars": [{}]}}"#,
                chars.join(", ")
            ));
        }
        put(
            &mut files,
            FONT,
            &format!(r#"{{"providers": [{}]}}"#, json.join(", ")),
        );
        files
    }

    fn providers(files: &Files) -> Vec<Node> {
        json::parse(&files[FONT])
            .unwrap()
            .path("providers")
            .items()
            .to_vec()
    }

    fn rows(provider: &Node) -> Vec<String> {
        provider
            .path("chars")
            .items()
            .iter()
            .map(|row| row.as_str().unwrap().to_owned())
            .collect()
    }

    fn packed(files: &Files) -> Vec<&str> {
        files
            .keys()
            .filter(|path| path.starts_with(PACKED))
            .map(String::as_str)
            .collect()
    }

    const FIELDS: &str = r#""height": 12, "ascent": 10"#;

    #[test]
    fn multi_glyph_providers_with_nul_cells_pack_into_one_grid() {
        let sources: [(&str, &str, &[&str]); 5] = [
            ("a", FIELDS, &["AB"]),
            ("b", FIELDS, &["C\0", "DE"]),
            ("c", FIELDS, &["\0F"]),
            ("d", FIELDS, &["G", "H"]),
            ("e", FIELDS, &["I"]),
        ];
        for shader in [false, true] {
            let fx = Fixture::new();
            let input = files(&sources, shader);

            // The oracle must find every glyph the same, cell by cell under the text shader.
            let ran = fx.same(&input, &[], &FontPack);

            for (name, _, _) in &sources {
                let sheet = format!("assets/fx/textures/font/{name}.png");
                assert!(!ran.out.contains_key(&sheet), "{sheet} is packed");
            }
            let packed = packed(&ran.out);
            assert_eq!(packed.len(), 1, "{packed:?}");
            let name = packed[0]
                .strip_prefix(PACKED)
                .and_then(|name| name.strip_suffix(".png"))
                .unwrap();
            assert_eq!(name.len(), 10, "{name}");
            assert!(
                name.bytes()
                    .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
            );
            let providers = providers(&ran.out);
            assert_eq!(providers.len(), 1);
            let provider = &providers[0];
            assert_eq!(
                provider.path("file").as_str(),
                Some(format!("packprune:f/{name}.png").as_str())
            );
            assert_eq!(provider.path("height").int(), Some(12));
            assert_eq!(provider.path("ascent").int(), Some(10));
            let grid = rows(provider);
            assert_eq!(grid.concat().replace('\0', ""), "ABCDEFGHI");

            // Every glyph's texels, RGB under alpha 0 and 1 included, are the source's; the `\0` cells went.
            let image = decode(&ran.out[packed[0]]);
            assert!(image.width * image.height <= 11 * CELL * CELL);
            for (row, line) in grid.iter().enumerate() {
                for (col, ch) in line.chars().enumerate() {
                    if ch == '\0' {
                        continue;
                    }
                    for (x, y) in (0..CELL).flat_map(|x| (0..CELL).map(move |y| (x, y))) {
                        let at = (col as u32 * CELL + x, row as u32 * CELL + y);
                        assert_eq!(argb(&image, at.0, at.1), texel(ch, x, y), "{ch} at {at:?}");
                    }
                }
            }
            assert!(
                ran.changes()[0].starts_with("5 font sheets packed into 1 grids"),
                "{:?}",
                ran.changes()
            );
        }
    }

    #[test]
    fn a_provider_repeating_a_codepoint_stays() {
        let fx = Fixture::new();
        let input = files(
            &[
                ("a", FIELDS, &["AA"]),
                ("b", FIELDS, &["B"]),
                ("c", FIELDS, &["CD"]),
            ],
            true,
        );

        let ran = fx.same(&input, &[], &FontPack);

        assert_eq!(
            ran.out["assets/fx/textures/font/a.png"],
            input["assets/fx/textures/font/a.png"]
        );
        let providers = providers(&ran.out);
        assert_eq!(providers.len(), 2);
        assert_eq!(providers[0].path("file").as_str(), Some("fx:font/a.png"));
        assert_eq!(rows(&providers[0]), ["AA"]);
        assert_eq!(rows(&providers[1]).concat().replace('\0', ""), "BCD");
        assert_eq!(packed(&ran.out).len(), 1);
    }

    #[test]
    fn a_sheet_its_grid_does_not_divide_or_of_too_many_cells_stays() {
        let fx = Fixture::new();
        let wide: String = (0..33)
            .map(|i| char::from_u32(0xE100 + i).unwrap())
            .collect();
        let mut input = files(
            &[
                ("a", FIELDS, &["AB"]),
                ("b", FIELDS, &["C", "D"]),
                ("w", FIELDS, &[wide.as_str()]),
                ("e", FIELDS, &["E"]),
                ("f", FIELDS, &["F"]),
            ],
            false,
        );
        // 17 columns of texels for 2 cells, 17 rows for 2 cells.
        input.insert(
            "assets/fx/textures/font/a.png".into(),
            png(17, 8, |x, y| texel('A', x % CELL, y)),
        );
        input.insert(
            "assets/fx/textures/font/b.png".into(),
            png(8, 17, |x, y| texel('C', x, y % CELL)),
        );

        let ran = fx.same(&input, &[], &FontPack);

        for name in ["a", "b", "w"] {
            let sheet = format!("assets/fx/textures/font/{name}.png");
            assert_eq!(ran.out[&sheet], input[&sheet], "{sheet} stays");
        }
        let providers = providers(&ran.out);
        assert_eq!(providers.len(), 4);
        assert_eq!(rows(&providers[3]).concat().replace('\0', ""), "EF");
    }

    #[test]
    fn a_provider_an_earlier_one_shadows_a_codepoint_of_stays() {
        let fx = Fixture::new();
        // `x` draws D first, in a bucket of its own: `c` may not move above it, `e` may.
        let input = files(
            &[
                ("a", FIELDS, &["AB"]),
                ("x", r#""height": 12, "ascent": 9"#, &["D"]),
                ("c", FIELDS, &["CD"]),
                ("e", FIELDS, &["E"]),
            ],
            false,
        );

        let ran = fx.same(&input, &[], &FontPack);

        let providers = providers(&ran.out);
        assert_eq!(providers.len(), 3);
        assert_eq!(rows(&providers[0]).concat().replace('\0', ""), "ABE");
        assert_eq!(providers[1].path("file").as_str(), Some("fx:font/x.png"));
        assert_eq!(providers[2].path("file").as_str(), Some("fx:font/c.png"));
        assert!(ran.out.contains_key("assets/fx/textures/font/c.png"));
    }

    #[test]
    fn repeated_runs_write_the_same_bytes() {
        let input = files(
            &[
                ("a", FIELDS, &["AB"]),
                ("b", FIELDS, &["C\0", "DE"]),
                ("e", FIELDS, &["F"]),
            ],
            true,
        );

        let runs: Vec<Files> = (0..3)
            .map(|_| Fixture::new().same(&input, &[], &FontPack).out)
            .collect();

        assert_ne!(runs[0], input);
        assert!(runs.iter().all(|run| *run == runs[0]));
    }

    #[test]
    fn sheet_names_are_ten_base36_digits() {
        let modulus = 36u64.pow(10);

        assert_eq!(sheet_name(0), "0000000000");
        assert_eq!(sheet_name(35), "000000000z");
        assert_eq!(sheet_name(modulus - 1), "zzzzzzzzzz");
        assert_eq!(sheet_name(modulus), "0000000000");
        assert_eq!(sheet_name(u64::MAX).len(), 10);
    }
}
