//! atlas-budget: the client gives each texture atlas a power-of-two GPU texture, so a few oversized pack sprites can
//! double or quadruple it. This pass simulates the client's stitcher over every sprite, the client's and the pack's,
//! and halves the fewest large pack textures, with the least measured loss, that take an atlas over its budget down
//! a tier. It never lowers an atlas's mip level, and an atlas no downscale takes down a tier keeps every texture.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use packprune_codec::png::{self, Image};
use packprune_codec::{CodecError, metrics};
use packprune_graph::atlases::{Sprite, VanillaAtlases, ViewAtlases};
use packprune_graph::stitch::{self, Item, Stitched};
use packprune_pack::json;
use packprune_pack::{LogicalPath, PhysPath, TextureSize, View};
use packprune_pass::{Edits, Loss, LossRecord, Pass, PassCx, PassError, Stage};
use rayon::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::encode::png_layers;
use crate::textures::{Use, Views};

const NAME: &str = "atlas-budget";
/// The client's `maxTextureSize` the report stitches against.
const MAX_TEXTURE_SIZE: i64 = 16384;
const BLOCKS: &str = "minecraft:blocks";
/// Mip levels of the blocks atlas at the client's default options; the other atlases have none.
const BLOCKS_MIP: u32 = 4;
/// Without `max_atlas_bytes`, the atlases over 2048x2048 that hold pack sprites go one tier down.
const DEFAULT_BUDGET: u64 = 16 * 1024 * 1024;
/// The largest textures of an atlas one search may downscale, and the smallest area worth it.
const CANDIDATES: usize = 64;
const MIN_AREA: i64 = 64 * 64;
/// An image of at most this many colours is drawn art: each block takes its most common colour, not a blend.
const CRISP_COLOURS: usize = 16;

const NOT_RGBA: &str = "not an 8-bit RGB, RGBA or palette PNG";
const NO_FACTOR: &str = "no power-of-two factor up to max_factor divides its size and keeps min_dim and the mip alignment of its atlases";
const LOSSY: &str = "its measured loss exceeds min_ssim or max_delta at every factor";
const UNSTITCHED: &str = "its sprites fit no 16384 px texture";
const NO_DROP: &str = "no downscale within the limits lowers its tier";
const BLOCKED: &str = "every downscale that lowers its tier grows or re-mips another atlas that stitches one of its textures";

pub(super) struct AtlasBudget;

#[derive(Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct Options {
    max_atlas_bytes: Option<u64>,
    min_dim: u32,
    max_factor: u32,
    min_ssim: Option<f64>,
    max_delta: Option<u8>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_atlas_bytes: None,
            min_dim: 16,
            max_factor: 4,
            min_ssim: None,
            max_delta: None,
        }
    }
}

impl Options {
    fn read(options: &toml::Table) -> Result<Self, String> {
        let options: Self = options
            .clone()
            .try_into()
            .map_err(|error: toml::de::Error| error.to_string())?;
        if options.max_atlas_bytes == Some(0)
            || options.min_dim == 0
            || !options.max_factor.is_power_of_two()
            || !(2..=16).contains(&options.max_factor)
            || options
                .min_ssim
                .is_some_and(|v| !v.is_finite() || !(-1.0..=1.0).contains(&v))
        {
            return Err("max_atlas_bytes must be positive, min_dim at least 1, max_factor 2, 4, 8 or 16 and min_ssim in [-1, 1]".into());
        }
        Ok(options)
    }

    fn factors(&self) -> impl Iterator<Item = u32> + '_ {
        (1..=4)
            .map(|shift| 1u32 << shift)
            .filter(|factor| *factor <= self.max_factor)
    }

    fn within(&self, loss: metrics::ImageLoss) -> bool {
        self.min_ssim.is_none_or(|limit| loss.ssim >= limit)
            && self.max_delta.is_none_or(|limit| loss.max_delta <= limit)
    }
}

impl Pass for AtlasBudget {
    fn name(&self) -> &'static str {
        NAME
    }
    fn stage(&self) -> Stage {
        Stage::Lossy
    }
    fn loss(&self) -> Loss {
        Loss::Lossy
    }
    fn options_schema(&self) -> schemars::Schema {
        schemars::schema_for!(Options)
    }

    fn validate_options(&self, options: &toml::Table) -> Result<(), String> {
        Options::read(options).map(drop)
    }

    fn run(&self, cx: &PassCx<'_>) -> Result<Edits, PassError> {
        let options =
            Options::read(cx.options).map_err(|why| PassError::Refused { pass: NAME, why })?;
        let views = Views::new(cx, NAME)?;
        let usage = Use::new(cx, &views);
        let Some(first) = views.list.first() else {
            return Ok(Edits::default());
        };
        let client = client_stitched(cx, first)?;
        let (slots, textures) = collect(cx, &views);
        let mut planner = Planner::new(&slots, &textures);
        let mut measured = BTreeMap::new();
        let mut kept = BTreeMap::<&'static str, BTreeSet<usize>>::new();
        let mut changes = Vec::new();
        let budget = options.max_atlas_bytes.unwrap_or(DEFAULT_BUDGET);
        for view in deciding(views.list) {
            for objective in (0..slots.len()).filter(|&slot| slots[slot].view == view) {
                let slot = &slots[objective];
                if slot.pack_sprites == 0 {
                    continue;
                }
                let label = format!("{} in view {}", slot.atlas, views.list[view].name);
                let Some(current) = planner.current[objective] else {
                    changes.push(format!("{label}: kept, {UNSTITCHED}"));
                    continue;
                };
                if current.gpu_bytes() <= budget {
                    continue;
                }
                if slot.unknown > 0 {
                    changes.push(format!(
                        "{label}: kept at {}: the size of {} client sprite{} is unknown, so is the stitched size",
                        describe(current),
                        slot.unknown,
                        plural(slot.unknown)
                    ));
                    continue;
                }
                let mut large: Vec<usize> = slot
                    .entries
                    .iter()
                    .filter_map(|entry| entry.texture)
                    .filter(|texture| {
                        !planner.committed.contains_key(texture)
                            && textures[*texture].area() >= MIN_AREA
                    })
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect();
                // Texture indices follow the physical paths, so the index breaks area ties by path.
                large.sort_by(|a, b| textures[*b].area().cmp(&textures[*a].area()).then(a.cmp(b)));
                let mut chosen = Vec::new();
                for texture in large {
                    if chosen.len() == CANDIDATES {
                        break;
                    }
                    match refusal(cx, &views, &usage, &client, &textures[texture]) {
                        Some(why) => {
                            kept.entry(why).or_default().insert(texture);
                        }
                        None => chosen.push(texture),
                    }
                }
                measure(cx, &options, &planner, &chosen, &mut measured)?;
                let mut candidates = Vec::new();
                for &texture in &chosen {
                    let within: Vec<&Measure> = match measured.get(&texture) {
                        Some(Ok(list)) => list.iter().filter(|m| m.within).collect(),
                        Some(Err(why)) => {
                            kept.entry(why).or_default().insert(texture);
                            continue;
                        }
                        None => continue,
                    };
                    if within.is_empty() {
                        kept.entry(LOSSY).or_default().insert(texture);
                    }
                    let area = textures[texture].area();
                    candidates.extend(within.iter().map(|m| Candidate {
                        texture,
                        factor: m.factor,
                        saved: area - area / i64::from(m.factor).pow(2),
                        ssim: m.ssim,
                    }));
                }
                if candidates.is_empty() {
                    changes.push(format!(
                        "{label}: kept at {}: no pack texture of at least 64x64 can be downscaled within the limits",
                        describe(current)
                    ));
                    continue;
                }
                match planner.search(objective, candidates, options.max_atlas_bytes) {
                    Ok((choice, after)) => {
                        let out_of_reach = options
                            .max_atlas_bytes
                            .filter(|budget| after.gpu_bytes() > *budget)
                            .map(|budget| {
                                format!(", the budget of {} is out of reach", mib(budget))
                            })
                            .unwrap_or_default();
                        changes.push(format!(
                            "{label}: {} to {} (-{}), {} texture{} downscaled{out_of_reach}",
                            describe(current),
                            describe(after),
                            mib(current.gpu_bytes() - after.gpu_bytes()),
                            choice.len(),
                            plural(choice.len()),
                        ));
                        planner.commit(choice);
                    }
                    Err(why) => {
                        changes.push(format!("{label}: kept at {}: {why}", describe(current)))
                    }
                }
            }
        }
        let mut edits = Edits {
            changes,
            ..Edits::default()
        };
        let jobs: Vec<(usize, u32, &[u8])> = planner
            .committed
            .iter()
            .filter_map(|(&texture, &factor)| {
                let entry = cx.pack.files.get(&textures[texture].physical)?;
                Some((texture, factor, &*entry.blob))
            })
            .collect();
        let encoded: Vec<_> = jobs
            .into_par_iter()
            .map(|(texture, factor, blob)| {
                let image = png::decode(blob)?;
                let crisp = crisp(&image);
                let small = downscale(&image, factor, crisp);
                Ok::<_, CodecError>((texture, factor, crisp, png::encode(&small)?, blob.len()))
            })
            .collect();
        for result in encoded {
            let (texture, factor, crisp, bytes, before) =
                result.map_err(|e| PassError::Failed {
                    pass: NAME,
                    why: e.to_string(),
                })?;
            let texture_info = &textures[texture];
            let loss = measured
                .get(&texture)
                .and_then(|list| list.as_ref().ok())
                .and_then(|list| list.iter().find(|m| m.factor == factor))
                .ok_or_else(|| PassError::Failed {
                    pass: NAME,
                    why: format!(
                        "{}: no loss measured at factor {factor}",
                        texture_info.physical
                    ),
                })?;
            let (width, height) = (texture_info.width, texture_info.height);
            let step = i64::from(factor);
            edits.changes.push(format!(
                "{}: {width}x{height} to {}x{} (factor {factor}, {}), {before} to {} B, SSIM {:.4} and max delta {} (nearest sampling at the texel centres of {width}x{height})",
                texture_info.physical,
                width / step,
                height / step,
                if crisp { "most common colour of each block" } else { "linear-light box filter" },
                bytes.len(),
                loss.ssim,
                loss.max_delta
            ));
            for (metric, value) in [
                ("ssim", loss.ssim),
                ("max_delta", f64::from(loss.max_delta)),
            ] {
                edits.loss.push(LossRecord {
                    key: texture_info.physical.to_string(),
                    metric: metric.into(),
                    value,
                });
            }
            edits
                .replaced
                .insert(texture_info.physical.clone(), bytes.into());
        }
        for (why, refused) in kept {
            let count = refused
                .iter()
                .filter(|texture| !planner.committed.contains_key(texture))
                .count();
            if count > 0 {
                edits
                    .changes
                    .push(format!("kept {count} texture{}: {why}", plural(count)));
            }
        }
        if !edits.replaced.is_empty() {
            edits.changes.push(format!("loss: each texture before and after, the downscaled one sampled nearest at the texel centres of the original; atlas sizes are the 26.2 stitcher's at {MAX_TEXTURE_SIZE} px, with {BLOCKS_MIP} mip levels on {BLOCKS}, none elsewhere, and no anisotropic filtering"));
        }
        Ok(edits)
    }
}

/// A sprite as the stitcher sees it: its id, its frame, and the pack texture whose downscale shrinks it.
struct Entry {
    namespace: String,
    value: String,
    width: i64,
    height: i64,
    texture: Option<usize>,
}

/// One atlas of one view, with the stitched size the pack gives it.
struct Slot {
    view: usize,
    atlas: String,
    max_mip: u32,
    entries: Vec<Entry>,
    pack_sprites: usize,
    /// Client sprites whose size the vanilla index does not have.
    unknown: usize,
    original: Option<Stitched>,
}

impl Slot {
    fn stitch(&self, factor: impl Fn(usize) -> u32) -> Option<Stitched> {
        let items: Vec<Item<'_>> = self
            .entries
            .iter()
            .map(|entry| {
                let factor = i64::from(entry.texture.map_or(1, &factor));
                Item {
                    width: entry.width / factor,
                    height: entry.height / factor,
                    namespace: &entry.namespace,
                    value: &entry.value,
                }
            })
            .collect();
        stitch::stitch(&items, self.max_mip, MAX_TEXTURE_SIZE)
    }
}

/// A pack PNG some atlas of some view stitches whole, and those atlases.
struct Texture {
    physical: PhysPath,
    logical: LogicalPath,
    width: i64,
    height: i64,
    slots: BTreeSet<usize>,
}

impl Texture {
    fn area(&self) -> i64 {
        self.width * self.height
    }
}

/// The frame of a sprite, the pack file of its first input, and whether a downscale of that file shrinks it alone.
struct Frame {
    width: i64,
    height: i64,
    pack: Option<(LogicalPath, PhysPath)>,
    whole: bool,
}

/// Every atlas of every view, its sprites sized as the client reads them, and the pack textures stitched whole.
fn collect(cx: &PassCx<'_>, views: &Views<'_>) -> (Vec<Slot>, Vec<Texture>) {
    let mut slots = Vec::new();
    let mut links: Vec<Vec<Option<PhysPath>>> = Vec::new();
    let mut found = BTreeMap::<PhysPath, (LogicalPath, i64, i64)>::new();
    for (index, view) in views.list.iter().enumerate() {
        let atlases = views.atlases(index);
        for atlas in atlases.atlas_ids() {
            let mut slot = Slot {
                view: index,
                atlas: atlas.to_owned(),
                max_mip: if atlas == BLOCKS { BLOCKS_MIP } else { 0 },
                entries: vec![Entry {
                    namespace: "minecraft".into(),
                    value: "missingno".into(),
                    width: 16,
                    height: 16,
                    texture: None,
                }],
                pack_sprites: 0,
                unknown: 0,
                original: None,
            };
            let mut link = vec![None];
            for (id, sprite) in atlases.sprites(atlas, &cx.run.interner) {
                let Some(frame) = frame(cx, view, sprite) else {
                    // The client skips a pack PNG it cannot read; a client sprite has a size once a jar is given.
                    let in_pack = sprite
                        .inputs
                        .first()
                        .and_then(|input| LogicalPath::new(&input.logical).ok())
                        .is_some_and(|logical| view.files.contains_key(&logical));
                    slot.unknown += usize::from(!in_pack);
                    continue;
                };
                let (namespace, value) = id.split_once(':').unwrap_or(("minecraft", &id));
                slot.entries.push(Entry {
                    namespace: namespace.into(),
                    value: value.into(),
                    width: frame.width,
                    height: frame.height,
                    texture: None,
                });
                slot.pack_sprites += usize::from(frame.pack.is_some());
                match frame.pack {
                    Some((logical, physical)) if frame.whole => {
                        found.entry(physical.clone()).or_insert((
                            logical,
                            frame.width,
                            frame.height,
                        ));
                        link.push(Some(physical));
                    }
                    _ => link.push(None),
                }
            }
            slots.push(slot);
            links.push(link);
        }
    }
    let index: BTreeMap<&PhysPath, usize> = found.keys().zip(0..).collect();
    let mut textures: Vec<Texture> = found
        .iter()
        .map(|(physical, (logical, width, height))| Texture {
            physical: physical.clone(),
            logical: logical.clone(),
            width: *width,
            height: *height,
            slots: BTreeSet::new(),
        })
        .collect();
    for (at, (slot, link)) in slots.iter_mut().zip(&links).enumerate() {
        for (entry, physical) in slot.entries.iter_mut().zip(link) {
            entry.texture = physical.as_ref().map(|physical| index[physical]);
            if let Some(texture) = entry.texture {
                textures[texture].slots.insert(at);
            }
        }
        slot.original = slot.stitch(|_| 1);
    }
    (slots, textures)
}

/// A sprite's frame in `view`: the pack's file when the view shows one, else the client's. Metadata comes from the
/// pack at or above the texture, as `FallbackResourceManager` reads it. `None` when the size is unknown.
fn frame(cx: &PassCx<'_>, view: &View, sprite: &Sprite) -> Option<Frame> {
    let input = sprite.inputs.first()?;
    let logical = LogicalPath::new(&input.logical).ok()?;
    let physical = view.files.get(&logical);
    let size = match physical {
        Some(physical) => TextureSize::of_png(&cx.pack.files.get(physical)?.blob)?,
        None => *cx.vanilla.textures.get(&*input.logical)?,
    };
    let (width, height) = (i64::from(size.width), i64::from(size.height));
    let pack = physical.map(|physical| (logical.clone(), physical.clone()));
    if !sprite.region.is_empty() || sprite.inputs.len() > 1 {
        let (width, height) = if sprite.region.is_empty() {
            (width, height)
        } else {
            region(&sprite.region, width, height)?
        };
        return Some(Frame {
            width,
            height,
            pack,
            whole: false,
        });
    }
    let meta = format!("{}.mcmeta", input.logical);
    let bytes = match LogicalPath::new(&meta)
        .ok()
        .and_then(|meta| view.files.get(&meta))
    {
        Some(path) => Some(&*cx.pack.files.get(path)?.blob),
        None if physical.is_none() => cx.vanilla.texture_meta.get(&meta).map(Vec::as_slice),
        None => None,
    };
    let animated = match bytes {
        Some(bytes) => animation(bytes, width, height)?,
        None => None,
    };
    Some(match animated {
        Some((width, height)) => Frame {
            width,
            height,
            pack,
            whole: false,
        },
        None => Frame {
            width,
            height,
            pack,
            whole: matches!(input.reason, "atlas.single" | "atlas.directory"),
        },
    })
}

/// The frame of an `animation` section (`AnimationMetadataSection`), `Some(None)` without one.
fn animation(bytes: &[u8], width: i64, height: i64) -> Option<Option<(i64, i64)>> {
    let tree = json::parse(bytes).ok()?;
    let Some(animation) = tree.get("animation") else {
        return Some(None);
    };
    let field = |name: &str| animation.get(name).map(|node| i64::from(node.as_int(0)));
    let frame = match (field("width"), field("height")) {
        (Some(frame_width), frame_height) => (frame_width, frame_height.unwrap_or(height)),
        (None, Some(frame_height)) => (width, frame_height),
        (None, None) => (width.min(height), width.min(height)),
    };
    (frame.0 > 0 && frame.1 > 0).then_some(Some(frame))
}

/// The size of an unstitch region, `"<divisor_x>,<divisor_y> <x>,<y>,<width>,<height>"` as the graph writes it, in
/// the client's `floor(width * image_width / divisor_x)`.
fn region(region: &str, width: i64, height: i64) -> Option<(i64, i64)> {
    let number = |text: &str, default: Option<f64>| {
        let text = text.trim();
        if text.is_empty() {
            default
        } else {
            text.parse::<f64>().ok()
        }
    };
    let (divisors, fields) = region.split_once(' ')?;
    let (divisor_x, divisor_y) = divisors.split_once(',')?;
    let (divisor_x, divisor_y) = (number(divisor_x, Some(1.0))?, number(divisor_y, Some(1.0))?);
    let fields: Vec<&str> = fields.split(',').collect();
    let [_, _, region_width, region_height] = fields.as_slice() else {
        return None;
    };
    let w = (number(region_width, None)? * (width as f64 / divisor_x)).floor() as i64;
    let h = (number(region_height, None)? * (height as f64 / divisor_y)).floor() as i64;
    (w > 0 && h > 0).then_some((w, h))
}

/// The textures the client's own atlases stitch, with the pack's `filter.block`: a pack file at one of these paths
/// is a sprite to the client, not a file it reads by name.
fn client_stitched(cx: &PassCx<'_>, view: &View) -> Result<BTreeSet<String>, PassError> {
    let failed = |error: packprune_pack::PatternError| PassError::Failed {
        pass: NAME,
        why: error.to_string(),
    };
    let vanilla =
        VanillaAtlases::new(cx.run, cx.vanilla, &cx.layout.meta.filters).map_err(failed)?;
    let mut empty = view.clone();
    empty.files.clear();
    let atlases = ViewAtlases::build(cx.run, &vanilla, &empty, |_| None).map_err(failed)?;
    Ok(atlases
        .atlas_ids()
        .flat_map(|atlas| atlases.sprites(atlas, &cx.run.interner))
        .flat_map(|(_, sprite)| &sprite.inputs)
        .filter(|input| matches!(input.reason, "atlas.single" | "atlas.directory"))
        .map(|input| input.logical.to_string())
        .collect())
}

/// The views the budget is decided in: the verified ones, else the newest.
fn deciding(views: &[View]) -> Vec<usize> {
    let verified: Vec<usize> = (0..views.len()).filter(|&at| views[at].verified).collect();
    if !verified.is_empty() {
        return verified;
    }
    (0..views.len())
        .max_by_key(|&at| (views[at].formats.max, at))
        .into_iter()
        .collect()
}

/// Why `texture` must keep its size, if it must.
fn refusal(
    cx: &PassCx<'_>,
    views: &Views<'_>,
    usage: &Use<'_>,
    client: &BTreeSet<String>,
    texture: &Texture,
) -> Option<&'static str> {
    let logical = texture.logical.as_str();
    let layers = png_layers(&texture.physical);
    let client_sprite = client.contains(logical);
    if !cx.scope.contains(&texture.physical) {
        return Some("outside the pass's scope");
    }
    if cx.pinned.contains(&texture.physical) {
        return Some("pinned by a keep entry");
    }
    if dimension_sensitive(logical) {
        return Some("its size is data (a colormap, font sheet or palette)");
    }
    if layers.banner || layers.eyes || layers.effect {
        return Some("a banner, shield, eyes or effect texture, which some client reads by path");
    }
    if usage.loaded_as_file(logical) {
        return Some(
            "a font, equipment layer, special model, post effect or text names the file itself",
        );
    }
    if cx.vanilla.has(logical) && !client_sprite {
        return Some("a vanilla path the client does not stitch, so it may read the file by name");
    }
    let Some(entry) = cx.pack.files.get(&texture.physical) else {
        return Some(NOT_RGBA);
    };
    let blob = &entry.blob;
    // IHDR bit depth (byte 24) and colour type (byte 25): a grey PNG reads lighter through `javax.imageio`.
    if blob.get(24) != Some(&8) && blob.get(25) != Some(&3)
        || !blob.get(25).is_some_and(|kind| [2, 3, 6].contains(kind))
    {
        return Some(NOT_RGBA);
    }
    for (index, view) in views.list.iter().enumerate() {
        if view.files.get(&texture.logical) != Some(&texture.physical) {
            continue;
        }
        for placement in usage.placements(logical).iter().filter(|p| p.view == index) {
            if !placement.plain(logical)
                || !matches!(
                    placement.sprite.inputs[0].reason,
                    "atlas.single" | "atlas.directory"
                )
            {
                return Some("an atlas builds a sprite from part of it or with other files");
            }
        }
        let meta = LogicalPath::new(&format!("{logical}.mcmeta"))
            .ok()
            .and_then(|meta| view.files.get(&meta))
            .and_then(|meta| cx.pack.files.get(meta));
        if let Some(meta) = meta {
            let Ok(tree) = json::parse(&meta.blob) else {
                return Some("its .mcmeta does not parse");
            };
            if tree.has("animation") {
                return Some("animated");
            }
            let scaling = tree.path("gui").path("scaling");
            let kind = scaling.path("type").as_string("stretch");
            if scaling.is_object() && kind.strip_prefix("minecraft:").unwrap_or(&kind) != "stretch"
            {
                return Some("its GUI nine-slice or tile scaling counts texels");
            }
        }
    }
    None
}

/// Textures whose pixel size is data to the client: colormaps (256-wide lookups), font sheets (the cell sets the
/// glyph size) and palettes (their texel count must match the palette key).
fn dimension_sensitive(logical: &str) -> bool {
    let Some((_, rest)) = logical
        .strip_prefix("assets/")
        .and_then(|rest| rest.split_once('/'))
    else {
        return false;
    };
    [
        "textures/colormap/",
        "textures/font/",
        "textures/palettes/",
        "textures/trims/color_palettes/",
    ]
    .iter()
    .any(|folder| rest.starts_with(folder))
}

/// The factors a texture may take: powers of two up to `max_factor` that divide both sides, keep both at least
/// `min_dim`, and keep both multiples of `1 << mip_level` of every mipmapped atlas that stitches it, so no atlas's
/// mip level drops.
fn factors(planner: &Planner<'_>, texture: usize, options: &Options) -> Vec<u32> {
    let info = &planner.textures[texture];
    let align = info
        .slots
        .iter()
        .map(|&slot| &planner.slots[slot])
        .filter(|slot| slot.max_mip > 0)
        .map(|slot| {
            1i64 << slot
                .original
                .map_or(slot.max_mip, |stitched| stitched.mip_level)
        })
        .max()
        .unwrap_or(1);
    let min_dim = i64::from(options.min_dim);
    options
        .factors()
        .filter(|&factor| {
            let factor = i64::from(factor);
            let (width, height) = (info.width / factor, info.height / factor);
            info.width % factor == 0
                && info.height % factor == 0
                && width >= min_dim.max(align)
                && height >= min_dim.max(align)
                && width.isolate_lowest_one() >= align
                && height.isolate_lowest_one() >= align
        })
        .collect()
}

#[derive(Clone, Copy)]
struct Measure {
    factor: u32,
    ssim: f64,
    max_delta: u8,
    within: bool,
}

type Measured = BTreeMap<usize, Result<Vec<Measure>, &'static str>>;

struct MeasureJob<'a> {
    texture: usize,
    factors: Vec<u32>,
    digest: [u8; 32],
    blob: &'a [u8],
}

/// The loss of each factor of `textures` not measured yet, in parallel (cached by blob digest + factor).
fn measure(
    cx: &PassCx<'_>,
    options: &Options,
    planner: &Planner<'_>,
    textures: &[usize],
    measured: &mut Measured,
) -> Result<(), PassError> {
    let jobs: Vec<MeasureJob<'_>> = textures
        .iter()
        .filter(|texture| !measured.contains_key(texture))
        .filter_map(|&texture| {
            let entry = cx.pack.files.get(&planner.textures[texture].physical)?;
            Some(MeasureJob {
                texture,
                factors: factors(planner, texture, options),
                digest: entry.digest.0,
                blob: &entry.blob,
            })
        })
        .collect();
    let results: Vec<_> = jobs
        .into_par_iter()
        .map(|job| {
            (
                job.texture,
                measure_one(cx, job.digest, job.blob, &job.factors, options),
            )
        })
        .collect();
    for (texture, result) in results {
        let result = result.map_err(|e| PassError::Failed {
            pass: NAME,
            why: format!("{}: {e}", planner.textures[texture].physical),
        })?;
        measured.insert(texture, result);
    }
    Ok(())
}

/// The original against each downscale, sampled at the original's texel centres: the whole loss a player sees up
/// close. Sampling at the downscaled size instead misses what falls between its texel centres, a 1 px checker say.
fn measure_one(
    cx: &PassCx<'_>,
    digest: [u8; 32],
    blob: &[u8],
    factors: &[u32],
    options: &Options,
) -> Result<Result<Vec<Measure>, &'static str>, CodecError> {
    if factors.is_empty() {
        return Ok(Err(NO_FACTOR));
    }
    let mut cached_out = Vec::with_capacity(factors.len());
    let mut missing = Vec::new();
    for &factor in factors {
        let mut key = [0u8; 36];
        key[..32].copy_from_slice(&digest);
        key[32..36].copy_from_slice(&factor.to_le_bytes());
        if let Some(bytes) = cx.cache.get("atlas-budget-v1", &key)
            && bytes.len() == 9
        {
            let ssim = f64::from_le_bytes(bytes[..8].try_into().expect("8 bytes"));
            let max_delta = bytes[8];
            let loss = metrics::ImageLoss { ssim, max_delta };
            cached_out.push(Some(Measure {
                factor,
                ssim,
                max_delta,
                within: options.within(loss),
            }));
        } else {
            cached_out.push(None);
            missing.push((cached_out.len() - 1, factor, key));
        }
    }
    if !missing.is_empty() {
        let Ok(image) = png::decode(blob) else {
            return Ok(Err(NOT_RGBA));
        };
        let crisp = crisp(&image);
        for (slot, factor, key) in missing {
            let small = downscale(&image, factor, crisp);
            let loss = metrics::image_loss(&image, &small, image.width, image.height)?;
            let mut val = [0u8; 9];
            val[..8].copy_from_slice(&loss.ssim.to_le_bytes());
            val[8] = loss.max_delta;
            cx.cache.put("atlas-budget-v1", &key, &val);
            cached_out[slot] = Some(Measure {
                factor,
                ssim: loss.ssim,
                max_delta: loss.max_delta,
                within: options.within(loss),
            });
        }
    }
    Ok(Ok(cached_out.into_iter().flatten().collect()))
}

/// One way to shrink a texture, and what it saves at what loss.
struct Candidate {
    texture: usize,
    factor: u32,
    saved: i64,
    ssim: f64,
}

impl Candidate {
    fn score(&self) -> f64 {
        self.saved as f64 / (1.0 - self.ssim + 1e-4)
    }
}

/// Texture index to its factor.
type Choice = BTreeMap<usize, u32>;

/// The atlases as the downscales committed so far leave them.
struct Planner<'t> {
    slots: &'t [Slot],
    textures: &'t [Texture],
    current: Vec<Option<Stitched>>,
    committed: Choice,
}

impl<'t> Planner<'t> {
    fn new(slots: &'t [Slot], textures: &'t [Texture]) -> Self {
        Self {
            slots,
            textures,
            current: slots.iter().map(|slot| slot.original).collect(),
            committed: Choice::new(),
        }
    }

    fn stitch(&self, slot: usize, choice: &Choice) -> Option<Stitched> {
        self.slots[slot].stitch(|texture| {
            choice
                .get(&texture)
                .or_else(|| self.committed.get(&texture))
                .copied()
                .unwrap_or(1)
        })
    }

    /// `objective` within `goal` at its mip level, and every other atlas that stitches a texture of `choice` at its
    /// mip level and no larger than now.
    fn fits(
        &self,
        objective: usize,
        others: &BTreeSet<usize>,
        choice: &Choice,
        goal: u64,
    ) -> Option<Stitched> {
        let original = self.slots[objective].original?;
        let now = self
            .stitch(objective, choice)
            .filter(|now| now.gpu_bytes() <= goal && now.mip_level == original.mip_level)?;
        others
            .iter()
            .all(
                |&slot| match (self.slots[slot].original, self.current[slot]) {
                    (Some(original), Some(current)) => {
                        self.stitch(slot, choice).is_some_and(|after| {
                            after.mip_level == original.mip_level
                                && after.gpu_bytes() <= current.gpu_bytes()
                        })
                    }
                    _ => true,
                },
            )
            .then_some(now)
    }

    /// The downscales that take `objective` to `budget`, or as near as the candidates reach, one tier down without
    /// one: candidates are applied by bytes saved per loss until the atlas fits, then each, most lossy first, goes
    /// back to the smallest factor that still fits.
    fn search(
        &self,
        objective: usize,
        mut candidates: Vec<Candidate>,
        budget: Option<u64>,
    ) -> Result<(Choice, Stitched), &'static str> {
        let current = self.current[objective].ok_or(UNSTITCHED)?;
        let others: BTreeSet<usize> = candidates
            .iter()
            .flat_map(|candidate| self.textures[candidate.texture].slots.iter().copied())
            .filter(|&slot| slot != objective)
            .collect();
        let mut lowest = Choice::new();
        for candidate in &candidates {
            let factor = lowest.entry(candidate.texture).or_insert(candidate.factor);
            *factor = (*factor).max(candidate.factor);
        }
        let floor = self
            .stitch(objective, &lowest)
            .map_or(u64::MAX, |stitched| stitched.gpu_bytes());
        let goal = match budget {
            Some(budget) => budget.max(floor),
            None => current.gpu_bytes() / 2,
        };
        if floor > goal || floor >= current.gpu_bytes() {
            return Err(NO_DROP);
        }
        candidates.sort_by(|a, b| {
            b.score()
                .total_cmp(&a.score())
                .then(a.texture.cmp(&b.texture))
                .then(a.factor.cmp(&b.factor))
        });
        let mut choice = Choice::new();
        let mut reached = false;
        for candidate in &candidates {
            if choice
                .get(&candidate.texture)
                .is_some_and(|factor| *factor >= candidate.factor)
            {
                continue;
            }
            choice.insert(candidate.texture, candidate.factor);
            if self.fits(objective, &others, &choice, goal).is_some() {
                reached = true;
                break;
            }
        }
        if !reached {
            return Err(BLOCKED);
        }
        let ssim = |texture: usize, factor: u32| {
            candidates
                .iter()
                .find(|c| c.texture == texture && c.factor == factor)
                .map_or(1.0, |c| c.ssim)
        };
        let mut order: Vec<(usize, u32)> = choice.iter().map(|(t, f)| (*t, *f)).collect();
        order.sort_by(|a, b| {
            ssim(a.0, a.1)
                .total_cmp(&ssim(b.0, b.1))
                .then(a.0.cmp(&b.0))
        });
        for (texture, factor) in order {
            let mut lower: Vec<u32> = candidates
                .iter()
                .filter(|c| c.texture == texture && c.factor < factor)
                .map(|c| c.factor)
                .collect();
            lower.sort_unstable();
            lower.insert(0, 1);
            for smaller in lower {
                let mut trial = choice.clone();
                if smaller == 1 {
                    trial.remove(&texture);
                } else {
                    trial.insert(texture, smaller);
                }
                if self.fits(objective, &others, &trial, goal).is_some() {
                    choice = trial;
                    break;
                }
            }
        }
        let after = self
            .fits(objective, &others, &choice, goal)
            .ok_or(BLOCKED)?;
        Ok((choice, after))
    }

    fn commit(&mut self, choice: Choice) {
        let touched: BTreeSet<usize> = choice
            .keys()
            .flat_map(|texture| self.textures[*texture].slots.iter().copied())
            .collect();
        self.committed.extend(choice);
        let none = Choice::new();
        for slot in touched {
            self.current[slot] = self.stitch(slot, &none);
        }
    }
}

/// sRGB to linear light, and the linear value halfway between each pair of consecutive sRGB codes.
struct Srgb {
    linear: [f64; 256],
    bounds: [f64; 255],
}

impl Srgb {
    /// The nearest sRGB code of a linear value.
    fn encode(&self, linear: f64) -> u8 {
        self.bounds.partition_point(|bound| *bound < linear) as u8
    }
}

static SRGB: LazyLock<Srgb> = LazyLock::new(|| Srgb {
    linear: std::array::from_fn(|code| to_linear(code as f64 / 255.0)),
    bounds: std::array::from_fn(|code| to_linear((code as f64 + 0.5) / 255.0)),
});

/// The sRGB transfer function from correctly rounded operations only, so every platform builds the same tables:
/// `x^2.4` is `x^2 * (x^(1/5))^2`, the fifth root found by bisection.
fn to_linear(value: f64) -> f64 {
    if value <= 0.040_45 {
        return value / 12.92;
    }
    let x = (value + 0.055) / 1.055;
    let (mut low, mut high) = (0.0f64, 2.0f64);
    for _ in 0..64 {
        let mid = (low + high) / 2.0;
        if mid * mid * mid * mid * mid <= x {
            low = mid;
        } else {
            high = mid;
        }
    }
    x * x * low * low
}

/// At most [`CRISP_COLOURS`] colours, every fully transparent texel counted as one.
fn crisp(image: &Image) -> bool {
    let mut colours = BTreeSet::new();
    for texel in image.rgba.as_chunks::<4>().0 {
        colours.insert(if texel[3] == 0 { [0; 4] } else { *texel });
        if colours.len() > CRISP_COLOURS {
            return false;
        }
    }
    true
}

/// Each `factor` x `factor` block to one texel: premultiplied linear-light mean, or for `crisp` art the block's most
/// common colour (ties to the one nearest the mean, then the lowest).
fn downscale(image: &Image, factor: u32, crisp: bool) -> Image {
    let srgb = &*SRGB;
    let (width, height) = (image.width / factor, image.height / factor);
    let (step, stride) = (factor as usize, image.width as usize);
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    let mut block: Vec<[u8; 4]> = Vec::with_capacity(step * step);
    for y in 0..height as usize {
        for x in 0..width as usize {
            block.clear();
            for row in y * step..(y + 1) * step {
                let start = (row * stride + x * step) * 4;
                let texels = image.rgba[start..start + step * 4].as_chunks::<4>().0;
                block.extend(
                    texels
                        .iter()
                        .map(|texel| if texel[3] == 0 { [0; 4] } else { *texel }),
                );
            }
            let mean = blend(&block, srgb);
            rgba.extend_from_slice(&if crisp { majority(&block, mean) } else { mean });
        }
    }
    Image {
        width,
        height,
        rgba,
    }
}

fn blend(block: &[[u8; 4]], srgb: &Srgb) -> [u8; 4] {
    let alpha: u64 = block.iter().map(|texel| u64::from(texel[3])).sum();
    if alpha == 0 {
        return [0; 4];
    }
    let count = block.len() as u64;
    let mut out = [0u8; 4];
    for (channel, slot) in out.iter_mut().take(3).enumerate() {
        let sum: f64 = block
            .iter()
            .map(|texel| srgb.linear[usize::from(texel[channel])] * f64::from(texel[3]))
            .sum();
        *slot = srgb.encode(sum / alpha as f64);
    }
    out[3] = ((alpha + count / 2) / count) as u8;
    out
}

fn majority(block: &[[u8; 4]], mean: [u8; 4]) -> [u8; 4] {
    let mut counts: Vec<([u8; 4], usize)> = Vec::new();
    for texel in block {
        match counts.iter_mut().find(|(colour, _)| colour == texel) {
            Some((_, count)) => *count += 1,
            None => counts.push((*texel, 1)),
        }
    }
    let distance = |colour: &[u8; 4]| -> i32 {
        colour
            .iter()
            .zip(&mean)
            .map(|(a, b)| (i32::from(*a) - i32::from(*b)).pow(2))
            .sum()
    };
    counts
        .iter()
        .max_by(|a, b| {
            a.1.cmp(&b.1)
                .then_with(|| distance(&b.0).cmp(&distance(&a.0)))
                .then_with(|| b.0.cmp(&a.0))
        })
        .map_or(mean, |(colour, _)| *colour)
}

fn describe(stitched: Stitched) -> String {
    format!(
        "{}x{} at mip {}, {}",
        stitched.width,
        stitched.height,
        stitched.mip_level,
        mib(stitched.gpu_bytes())
    )
}

fn mib(bytes: u64) -> String {
    let text = format!("{:.2}", bytes as f64 / (1024.0 * 1024.0));
    format!("{} MiB", text.trim_end_matches('0').trim_end_matches('.'))
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use packprune_codec::png::{self, Image};
    use packprune_graph::stitch::Stitched;
    use packprune_pack::{LogicalPath, PhysPath, TextureSize};
    use packprune_pass::{Outcome, Pass, Scope};

    use super::{AtlasBudget, Candidate, Choice, Entry, Planner, Slot, Texture};
    use crate::lossy::tests::{Files, Fixture, bytes, files};

    const BIG_ITEM: &str = "assets/fx/textures/item/big.png";
    const BIG_BLOCK: &str = "assets/fx/textures/block/big.png";

    /// The test client with the size of each of its textures, as a jar gives them: its placeholder PNGs are 16x16.
    fn fixture() -> Fixture {
        let mut fixture = Fixture::new();
        for (path, entry) in &fixture.client.files {
            if path.as_str().ends_with(".png") {
                let size = TextureSize::of_png(&entry.blob).unwrap_or(TextureSize {
                    width: 16,
                    height: 16,
                });
                fixture.vanilla.textures.insert(path.to_string(), size);
            }
        }
        fixture
    }

    fn image(side: u32, texel: impl Fn(u32, u32) -> [u8; 4]) -> Image {
        Image {
            width: side,
            height: side,
            rgba: (0..side * side)
                .flat_map(|at| texel(at % side, at / side))
                .collect(),
        }
    }

    fn gradient(side: u32) -> Image {
        image(side, |x, y| {
            [(x * 255 / side) as u8, (y * 255 / side) as u8, 128, 255]
        })
    }

    fn put(files: &mut Files, path: &str, image: &Image) {
        files.insert(path.into(), png::encode(image).unwrap());
    }

    fn run(files: &Files, options: &str, names: &[&str]) -> Outcome {
        fixture().execute(files, &AtlasBudget, options, Scope::all(), names)
    }

    fn size(out: &Outcome, path: &str) -> (u32, u32) {
        let image = png::decode(bytes(out, path)).unwrap();
        (image.width, image.height)
    }

    fn changes(out: &Outcome) -> &[String] {
        &out.reports[0].changes
    }

    #[test]
    fn options_validate_at_load() {
        let pass = AtlasBudget;
        assert!(pass.validate_options(&toml::Table::new()).is_ok());
        assert!(
            pass.validate_options(
                &"max_atlas_bytes = 67108864\nmin_dim = 8\nmax_factor = 16\nmin_ssim = 0.9\nmax_delta = 32"
                    .parse()
                    .unwrap()
            )
            .is_ok()
        );
        for invalid in [
            "unknown = 1",
            "max_atlas_bytes = 0",
            "max_atlas_bytes = -1",
            "min_dim = 0",
            "max_factor = 1",
            "max_factor = 3",
            "max_factor = 32",
            "min_ssim = 2",
            "min_ssim = nan",
            "max_delta = 256",
        ] {
            assert!(
                pass.validate_options(&invalid.parse().unwrap()).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn an_oversized_sprite_takes_its_atlas_down_to_the_budget_at_the_smallest_factor() {
        let mut files = files();
        put(&mut files, BIG_ITEM, &gradient(512));
        for index in 0..4 {
            put(
                &mut files,
                &format!("assets/fx/textures/item/small_{index}.png"),
                &image(16, |_, _| [200, 10, 10, 255]),
            );
        }
        let out = run(&files, "max_atlas_bytes = 1048576", &[]);
        assert_eq!(size(&out, BIG_ITEM), (256, 256));
        assert_eq!(size(&out, "assets/fx/textures/item/small_0.png"), (16, 16));
        assert!(
            changes(&out).iter().any(|line| line.starts_with("minecraft:items in view")
                && line.contains(
                    "1024x1024 at mip 0, 4 MiB to 512x512 at mip 0, 1 MiB (-3 MiB), 1 texture downscaled"
                )),
            "{:#?}",
            changes(&out)
        );
        assert!(changes(&out).iter().any(|line| line.starts_with(
            "assets/fx/textures/item/big.png: 512x512 to 256x256 (factor 2, linear-light box filter)"
        )));
        let measured = &out.reports[0].measured;
        assert!(
            measured.iter().any(|record| record.key == BIG_ITEM
                && record.metric == "ssim"
                && record.value > 0.9)
        );
        let again = run(&files, "max_atlas_bytes = 1048576", &[]);
        assert_eq!(bytes(&again, BIG_ITEM), bytes(&out, BIG_ITEM));
    }

    #[test]
    fn the_blocks_atlas_keeps_its_mip_level() {
        let mut files = files();
        // 320 = 5 * 64: halved twice it is 80, a multiple of 16; a third time, 40 would take the atlas to mip 3.
        put(&mut files, BIG_BLOCK, &gradient(320));
        let out = run(&files, "max_atlas_bytes = 1\nmax_factor = 16", &[]);
        assert_eq!(size(&out, BIG_BLOCK), (80, 80));
        assert!(
            changes(&out)
                .iter()
                .any(|line| line.starts_with("minecraft:blocks in view")
                    && line.contains("512x512 at mip 4")
                    && line.contains("to 256x128 at mip 4")
                    && line.contains("out of reach")),
            "{:#?}",
            changes(&out)
        );
    }

    #[test]
    fn a_loss_over_min_ssim_keeps_the_texture() {
        let mut files = files();
        let checker = image(256, |x, y| {
            if (x + y) % 2 == 0 {
                [255; 4]
            } else {
                [0, 0, 0, 255]
            }
        });
        put(&mut files, BIG_ITEM, &checker);
        let out = run(&files, "max_atlas_bytes = 65536\nmin_ssim = 0.9", &[]);
        assert_eq!(bytes(&out, BIG_ITEM), files[BIG_ITEM].as_slice());
        assert!(
            changes(&out)
                .iter()
                .any(|line| line.contains("kept 1 texture: its measured loss exceeds")),
            "{:#?}",
            changes(&out)
        );
        assert!(out.reports[0].measured.is_empty());
    }

    #[test]
    fn no_edit_when_no_downscale_lowers_the_tier() {
        let mut files = files();
        let wall = "assets/fx/textures/item/wall.png";
        put(&mut files, BIG_ITEM, &gradient(256));
        // 1022 + 2 px of padding fills 1024x1024: the atlas is 2048 wide whatever size the other sprite takes.
        put(&mut files, wall, &image(1022, |_, _| [90, 90, 90, 255]));
        let out = fixture().execute(
            &files,
            &AtlasBudget,
            "max_atlas_bytes = 1",
            Scope::matching(move |p| p.as_str() != wall),
            &[],
        );
        assert_eq!(bytes(&out, BIG_ITEM), files[BIG_ITEM].as_slice());
        assert!(
            changes(&out).iter().any(|line| line.contains(
                "2048x1024 at mip 0, 8 MiB: no downscale within the limits lowers its tier"
            )),
            "{:#?}",
            changes(&out)
        );
        assert!(
            changes(&out)
                .iter()
                .any(|line| line.contains("kept 1 texture: outside the pass's scope"))
        );
    }

    #[test]
    fn an_atlas_within_the_default_budget_is_left_alone() {
        let mut files = files();
        put(&mut files, BIG_ITEM, &gradient(512));
        let out = run(&files, "", &[]);
        assert_eq!(bytes(&out, BIG_ITEM), files[BIG_ITEM].as_slice());
        assert!(changes(&out).is_empty(), "{:#?}", changes(&out));
    }

    /// A 4096x4096 scope texture takes the blocks atlas to 8192x8192: without a budget, one tier down is the goal,
    /// and the search settles on halving it even when quartering it ranks first.
    #[test]
    fn without_a_budget_an_oversized_block_sprite_goes_one_tier_down() {
        let slot = Slot {
            view: 0,
            atlas: "minecraft:blocks".into(),
            max_mip: 4,
            entries: [
                ("missingno", 16, None),
                ("stone", 16, None),
                ("scope", 4096, Some(0)),
            ]
            .into_iter()
            .map(|(value, side, texture)| Entry {
                namespace: "minecraft".into(),
                value: value.into(),
                width: side,
                height: side,
                texture,
            })
            .collect(),
            pack_sprites: 1,
            unknown: 0,
            original: None,
        };
        let slots = [Slot {
            original: slot.stitch(|_| 1),
            ..slot
        }];
        assert_eq!(
            slots[0].original,
            Some(Stitched {
                width: 8192,
                height: 8192,
                mip_level: 4
            })
        );
        let textures = [Texture {
            physical: PhysPath::new("assets/guns/textures/item/scope.png").unwrap(),
            logical: LogicalPath::new("assets/guns/textures/item/scope.png").unwrap(),
            width: 4096,
            height: 4096,
            slots: [0].into(),
        }];
        let area = 4096 * 4096;
        for quarter_ssim in [0.85, 0.99] {
            let candidates = vec![
                Candidate {
                    texture: 0,
                    factor: 2,
                    saved: area - area / 4,
                    ssim: 0.95,
                },
                Candidate {
                    texture: 0,
                    factor: 4,
                    saved: area - area / 16,
                    ssim: quarter_ssim,
                },
            ];
            let planner = Planner::new(&slots, &textures);
            let (choice, after) = planner.search(0, candidates, None).unwrap();
            assert_eq!(choice, Choice::from([(0, 2)]), "{quarter_ssim}");
            assert_eq!(
                (after.width, after.height, after.mip_level),
                (4096, 4096, 4)
            );
            assert_eq!(
                after.gpu_bytes() * 4,
                slots[0].original.unwrap().gpu_bytes()
            );
        }
    }
}
