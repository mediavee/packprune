//! The file optimisers fused with zip-entry compression: unique content/option work items run largest-first in
//! parallel through the content cache, deduplicating identical files within a run and screening lossy PNG chains at
//! fast libdeflate-12 before running Zopfli once on the winning image. Each codec checks its own output, so a cached
//! output is a checked one.

use std::cmp::Reverse;
use std::collections::BTreeMap;

use packprune_codec::{DEFAULT_EFFORT, png};
use packprune_config::Overrides;
use packprune_pack::{Digest, Entry, FileKind, Layout, PackInput, PhysPath, Quirks};
use packprune_pass::{FileCx, FileOptimizer, LossRecord, Optimised};
use packprune_zip::{CompressOptions, Encoded, EntryClass};
use rayon::prelude::*;

use super::RunError;
use super::cache::Cache;
use super::encode;
use super::options::{Optimiser, toml_table};
use super::origins::Origins;

/// What one optimiser did over the pack: the files whose bytes it changed and their sizes before and after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OptimiserReport {
    pub optimiser: &'static str,
    pub files: usize,
    pub bytes_before: u64,
    pub bytes_after: u64,
}

pub(crate) struct Stage {
    pub pack: PackInput,
    /// The pack after the lossless optimisers alone, when a lossy one then changed a file: the oracle proves the
    /// first and measures the second apart.
    pub lossless: Option<PackInput>,
    /// Zip-encoded entries ready for `encode::stage_encoded`, keyed by path.
    pub encoded: BTreeMap<String, Encoded>,
    pub reports: Vec<OptimiserReport>,
    pub loss: Vec<LossRecord>,
}

/// One optimiser's change to one file.
#[derive(Clone)]
struct Step {
    optimiser: usize,
    before: u64,
    after: u64,
    loss: Option<LossRecord>,
}

/// Canonical deduplication key for files that undergo identical optimisation and zip compression.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct WorkKey {
    digest: Digest,
    kind: u8,
    quirks: u8,
    class: u8,
    path_flags: u8,
    chain: Vec<(usize, String)>,
}

struct WorkGroup {
    entry: Entry,
    kind: FileKind,
    quirks: Quirks,
    class: EntryClass,
    path_flags: u8,
    chain: Vec<(usize, toml::Table)>,
    paths: Vec<PhysPath>,
}

struct WorkResult {
    paths: Vec<PhysPath>,
    entry: Entry,
    lossless: Entry,
    encoded: Encoded,
    steps: Vec<Step>,
}

#[expect(
    clippy::too_many_arguments,
    reason = "fuses file optimisation and zip encoding into one parallel pass"
)]
pub(crate) fn run(
    pack: &PackInput,
    layout: &Layout,
    optimisers: &[Optimiser<'_>],
    overrides: &Overrides<'_>,
    origins: &Origins,
    zip_compress: &CompressOptions,
    cache: &Cache,
    forced: Quirks,
) -> Result<Stage, RunError> {
    let quirks = quirks(layout);
    let every_view = quirks
        .values()
        .fold(Quirks::default(), |all, q| union(all, *q));
    let order: Vec<usize> = (0..optimisers.len())
        .filter(|&index| optimisers[index].lossy.is_none())
        .chain((0..optimisers.len()).filter(|&index| optimisers[index].lossy.is_some()))
        .collect();

    // Group files with identical bytes, effective quirks/flags, and active optimiser options so duplicate files in a
    // pack are optimised and zip-compressed only once.
    let mut grouped: BTreeMap<WorkKey, WorkGroup> = BTreeMap::new();
    for (path, entry) in &pack.files {
        let kind = layout.kinds.get(path).copied().unwrap_or(FileKind::Unknown);
        let file_quirks = union(quirks.get(path).copied().unwrap_or(every_view), forced);
        let class = kind.entry_class(path);
        let mut chain = Vec::new();
        let mut chain_key = Vec::new();
        for &index in &order {
            let enabled = &optimisers[index];
            let optimiser = enabled.optimiser;
            if !optimiser.applies(kind, path) {
                continue;
            }
            // Globs and overrides name the input's paths: a renamed or merged file is matched by those.
            let options = match &enabled.lossy {
                Some((globs, options)) if origins.all(path, |origin| globs.is_match(origin)) => {
                    options.clone()
                }
                Some(_) => continue,
                None => match origins.options(overrides, optimiser.name(), path) {
                    Some(options) => toml_table(&options).map_err(|why| {
                        RunError::Refused(format!("{} options of {path}: {why}", optimiser.name()))
                    })?,
                    None => continue,
                },
            };
            let serialised = toml::to_string(&options).expect("a table serialises");
            chain_key.push((index, serialised));
            chain.push((index, options));
        }
        let path_flags = png_path_flags(path);
        let key = WorkKey {
            digest: entry.digest,
            kind: kind_id(kind),
            quirks: quirks_bits(file_quirks),
            class: class_id(class),
            path_flags,
            chain: chain_key,
        };
        grouped
            .entry(key)
            .and_modify(|group| group.paths.push(path.clone()))
            .or_insert_with(|| WorkGroup {
                entry: entry.clone(),
                kind,
                quirks: file_quirks,
                class,
                path_flags,
                chain,
                paths: vec![path.clone()],
            });
    }

    // Largest first (LPT scheduling): a large PNG or JSON started last would run alone while the other workers idle.
    // The results are put back in path order, so the output does not depend on the schedule.
    let mut work: Vec<WorkGroup> = grouped.into_values().collect();
    work.sort_by(|a, b| {
        Reverse(a.entry.blob.len())
            .cmp(&Reverse(b.entry.blob.len()))
            .then_with(|| a.paths[0].cmp(&b.paths[0]))
    });

    let results: Vec<WorkResult> = work
        .into_iter()
        .par_bridge()
        .map(|group| {
            let rep = &group.paths[0];
            let cx = FileCx {
                kind: group.kind,
                path: rep.clone(),
                quirks: group.quirks,
            };
            let (current, lossless, steps) = run_chain(
                &group.entry,
                &cx,
                group.path_flags,
                &group.chain,
                optimisers,
                cache,
            )?;
            let encoded = encode::encode_entry(
                &current.blob,
                current.digest.0,
                group.class,
                zip_compress,
                cache,
            );
            Ok(WorkResult {
                paths: group.paths,
                entry: current,
                lossless,
                encoded,
                steps,
            })
        })
        .collect::<Result<_, RunError>>()?;

    let mut reports: Vec<OptimiserReport> = optimisers
        .iter()
        .map(|enabled| OptimiserReport {
            optimiser: enabled.optimiser.name(),
            files: 0,
            bytes_before: 0,
            bytes_after: 0,
        })
        .collect();
    let mut loss = Vec::new();
    let mut files = BTreeMap::new();
    let mut lossless = BTreeMap::new();
    let mut encoded = BTreeMap::new();
    let mut lossy = false;
    let mut by_path: BTreeMap<PhysPath, (Entry, Entry, Encoded, Vec<Step>)> = BTreeMap::new();
    for result in results {
        let multi = result.paths.len() > 1;
        for path in result.paths {
            let steps = if multi {
                result
                    .steps
                    .iter()
                    .map(|step| Step {
                        optimiser: step.optimiser,
                        before: step.before,
                        after: step.after,
                        loss: step.loss.as_ref().map(|record| LossRecord {
                            key: path.to_string(),
                            metric: record.metric.clone(),
                            value: record.value,
                        }),
                    })
                    .collect()
            } else {
                result.steps.clone()
            };
            by_path.insert(
                path,
                (
                    result.entry.clone(),
                    result.lossless.clone(),
                    result.encoded.clone(),
                    steps,
                ),
            );
        }
    }
    for (path, (entry, before_lossy, zip_entry, steps)) in by_path {
        lossy |= before_lossy.digest != entry.digest;
        lossless.insert(path.clone(), before_lossy);
        encoded.insert(path.as_str().to_owned(), zip_entry);
        for step in steps {
            let report = &mut reports[step.optimiser];
            report.files += 1;
            report.bytes_before += step.before;
            report.bytes_after += step.after;
            loss.extend(step.loss);
        }
        files.insert(path, entry);
    }
    let packed = |files: BTreeMap<PhysPath, Entry>| PackInput {
        name: pack.name.clone(),
        digest: digest(&files),
        files,
    };
    Ok(Stage {
        lossless: lossy.then(|| packed(lossless)),
        pack: packed(files),
        encoded,
        reports,
        loss,
    })
}

/// Runs the active optimiser chain for one unique file, using single-Zopfli screening when both lossless `png` and
/// lossy PNG optimisers apply to the same image.
fn run_chain(
    input: &Entry,
    cx: &FileCx,
    path_flags: u8,
    chain: &[(usize, toml::Table)],
    optimisers: &[Optimiser<'_>],
    cache: &Cache,
) -> Result<(Entry, Entry, Vec<Step>), RunError> {
    let lossless_png_pos = chain
        .iter()
        .position(|(idx, opts)| optimisers[*idx].optimiser.name() == "png" && png_effort(opts) > 0);
    let has_lossy_png = chain.iter().any(|(idx, _)| {
        matches!(
            optimisers[*idx].optimiser.name(),
            "png-alpha" | "png-quantize" | "png-single-color"
        )
    });
    if let Some(ll_pos) = lossless_png_pos
        && has_lossy_png
    {
        return run_png_single_zopfli(input, cx, path_flags, chain, ll_pos, optimisers, cache);
    }
    run_chain_standard(input, cx, path_flags, chain, optimisers, cache)
}

fn run_chain_standard(
    input: &Entry,
    cx: &FileCx,
    path_flags: u8,
    chain: &[(usize, toml::Table)],
    optimisers: &[Optimiser<'_>],
    cache: &Cache,
) -> Result<(Entry, Entry, Vec<Step>), RunError> {
    let mut current = input.clone();
    let mut lossless = None;
    let mut steps = Vec::new();
    for (index, options) in chain {
        let enabled = &optimisers[*index];
        if enabled.lossy.is_some() && lossless.is_none() {
            lossless = Some(current.clone());
        }
        if let Some(output) = optimise(enabled.optimiser, &current, cx, path_flags, options, cache)?
        {
            steps.push(Step {
                optimiser: *index,
                before: current.blob.len() as u64,
                after: output.bytes.len() as u64,
                loss: output.loss,
            });
            current = entry_of(output.bytes);
        }
    }
    let lossless = lossless.unwrap_or_else(|| current.clone());
    Ok((current, lossless, steps))
}

/// Screens lossless `png` and all active lossy PNG optimisers at fast libdeflate-12 (`effort = 0`), then runs Zopfli
/// once on the winning image (and only compares Zopfli of lossless vs lossy when within a 2 % tie band).
fn run_png_single_zopfli(
    input: &Entry,
    cx: &FileCx,
    path_flags: u8,
    chain: &[(usize, toml::Table)],
    ll_pos: usize,
    optimisers: &[Optimiser<'_>],
    cache: &Cache,
) -> Result<(Entry, Entry, Vec<Step>), RunError> {
    let mut current = input.clone();
    let mut steps = Vec::new();
    // Run any non-PNG lossless optimisers before `png` normally.
    for (index, options) in &chain[..ll_pos] {
        let optimiser = optimisers[*index].optimiser;
        if let Some(output) = optimise(optimiser, &current, cx, path_flags, options, cache)? {
            steps.push(Step {
                optimiser: *index,
                before: current.blob.len() as u64,
                after: output.bytes.len() as u64,
                loss: output.loss,
            });
            current = entry_of(output.bytes);
        }
    }
    let before_png = current.clone();
    let (ll_index, ll_opts) = &chain[ll_pos];
    let ll_opt = optimisers[*ll_index].optimiser;
    let fast_ll_opts = with_effort(ll_opts, 0);
    let mut fast_ll_step = None;
    if let Some(output) = optimise(ll_opt, &before_png, cx, path_flags, &fast_ll_opts, cache)? {
        fast_ll_step = Some(Step {
            optimiser: *ll_index,
            before: before_png.blob.len() as u64,
            after: output.bytes.len() as u64,
            loss: output.loss,
        });
        current = entry_of(output.bytes);
    }
    let fast_lossless = current.clone();

    // Screen all remaining optimisers after `png` (lossy PNGs run with effort = 0).
    let mut lossy_steps = Vec::new();
    let mut last_lossy_effort: Option<u8> = None;
    for (index, options) in &chain[ll_pos + 1..] {
        let enabled = &optimisers[*index];
        let optimiser = enabled.optimiser;
        let is_lossy_png = matches!(
            optimiser.name(),
            "png-alpha" | "png-quantize" | "png-single-color"
        );
        let eff_opts = if is_lossy_png {
            with_effort(options, 0)
        } else {
            options.clone()
        };
        if let Some(output) = optimise(optimiser, &current, cx, path_flags, &eff_opts, cache)? {
            if is_lossy_png {
                last_lossy_effort = Some(png_effort(options));
            }
            lossy_steps.push(Step {
                optimiser: *index,
                before: current.blob.len() as u64,
                after: output.bytes.len() as u64,
                loss: output.loss,
            });
            current = entry_of(output.bytes);
        }
    }

    if lossy_steps.is_empty() {
        // No lossy PNG beat fast lossless: run full-effort Zopfli on the lossless PNG.
        let mut final_entry = before_png.clone();
        if let Some(output) = optimise(ll_opt, &before_png, cx, path_flags, ll_opts, cache)? {
            steps.push(Step {
                optimiser: *ll_index,
                before: before_png.blob.len() as u64,
                after: output.bytes.len() as u64,
                loss: output.loss,
            });
            final_entry = entry_of(output.bytes);
        }
        return Ok((final_entry.clone(), final_entry, steps));
    }

    // Run Zopfli once on the winning lossy PNG (cached).
    let effort = last_lossy_effort.unwrap_or(DEFAULT_EFFORT);
    let final_lossy = if effort > 0 {
        zopfli_png_cached(&current, effort, cache).unwrap_or_else(|| current.clone())
    } else {
        current
    };

    // If the lossy Zopfli result beats fast libdeflate-12 lossless by > 2%, Zopfli on lossless cannot catch up.
    if final_lossy.blob.len() * 100 < fast_lossless.blob.len() * 98 {
        if let Some(step) = fast_ll_step {
            steps.push(step);
        }
        if let Some(last) = lossy_steps.last_mut() {
            last.after = final_lossy.blob.len() as u64;
        }
        steps.extend(lossy_steps);
        return Ok((final_lossy, fast_lossless, steps));
    }

    // Close race (within 2%): run full-effort lossless Zopfli and pick the true byte winner.
    let mut zopfli_lossless = before_png.clone();
    let mut zopfli_ll_step = None;
    if let Some(output) = optimise(ll_opt, &before_png, cx, path_flags, ll_opts, cache)? {
        zopfli_ll_step = Some(Step {
            optimiser: *ll_index,
            before: before_png.blob.len() as u64,
            after: output.bytes.len() as u64,
            loss: output.loss,
        });
        zopfli_lossless = entry_of(output.bytes);
    }
    if final_lossy.blob.len() < zopfli_lossless.blob.len() {
        if let Some(step) = zopfli_ll_step {
            steps.push(step);
        }
        if let Some(first) = lossy_steps.first_mut() {
            first.before = zopfli_lossless.blob.len() as u64;
        }
        if let Some(last) = lossy_steps.last_mut() {
            last.after = final_lossy.blob.len() as u64;
        }
        steps.extend(lossy_steps);
        Ok((final_lossy, zopfli_lossless, steps))
    } else {
        if let Some(step) = zopfli_ll_step {
            steps.push(step);
        }
        Ok((zopfli_lossless.clone(), zopfli_lossless, steps))
    }
}

fn zopfli_png_cached(entry: &Entry, effort: u8, cache: &Cache) -> Option<Entry> {
    let key = [effort];
    if let Some(cached) = cache.get("png-zopfli-idat", &key, &[entry.digest.0], |value| {
        Some(value.to_vec())
    }) {
        return (!cached.is_empty()).then(|| entry_of(cached));
    }
    let image = png::decode(&entry.blob).ok()?;
    let pixels = image.width as usize * image.height as usize;
    let out = png::zopfli_idat(&entry.blob, pixels, effort).ok()?;
    if out.len() < entry.blob.len() && png::decode(&out).is_ok_and(|after| after == image) {
        cache.put("png-zopfli-idat", &key, &[entry.digest.0], &out);
        Some(entry_of(out))
    } else {
        cache.put("png-zopfli-idat", &key, &[entry.digest.0], &[]);
        None
    }
}

fn png_effort(options: &toml::Table) -> u8 {
    options
        .get("effort")
        .and_then(toml::Value::as_integer)
        .and_then(|v| u8::try_from(v).ok())
        .unwrap_or(DEFAULT_EFFORT)
}

fn with_effort(options: &toml::Table, effort: u8) -> toml::Table {
    let mut copy = options.clone();
    copy.insert("effort".into(), toml::Value::Integer(i64::from(effort)));
    copy
}

fn entry_of(bytes: Vec<u8>) -> Entry {
    Entry {
        digest: Digest(*blake3::hash(&bytes).as_bytes()),
        blob: bytes.into(),
    }
}

/// The optimiser's output for `input`, from the cache when this content, path-sensitivity flags, kind, quirks and
/// options were optimised before.
fn optimise(
    optimiser: &dyn FileOptimizer,
    input: &Entry,
    cx: &FileCx,
    path_flags: u8,
    options: &toml::Table,
    cache: &Cache,
) -> Result<Option<Optimised>, RunError> {
    let step = format!("file-optimiser {}", optimiser.name());
    let key = format!(
        "{options}\0{path_flags}\0{:?}\0{:?}",
        cx.kind,
        cx.quirks,
        options = toml::to_string(options).expect("a table serialises"),
    );
    if let Some(mut output) = cache.get(&step, key.as_bytes(), &[input.digest.0], decode) {
        if let Some(opt) = &mut output
            && let Some(record) = &mut opt.loss
        {
            record.key = cx.path.to_string();
        }
        return Ok(output);
    }
    let output = optimiser
        .optimise(&input.blob, cx, options)
        .map_err(packprune_pass::RunError::from)?;
    cache.put(
        &step,
        key.as_bytes(),
        &[input.digest.0],
        &encode_cached(output.as_ref()),
    );
    Ok(output)
}

/// Captures every path-dependent property inspected by PNG optimisers so identical files at different paths share
/// both in-run work and on-disk cache entries.
fn png_path_flags(path: &PhysPath) -> u8 {
    let text = path.as_str();
    let logical = if text.starts_with("assets/") {
        Some(text)
    } else {
        text.split_once('/')
            .map(|(_, rest)| rest)
            .filter(|rest| rest.starts_with("assets/"))
    };
    let mut flags = 0u8;
    if text == "pack.png" {
        flags |= 1 << 0;
    }
    if let Some(logical) = logical {
        if let Some(vanilla) = logical.strip_prefix("assets/minecraft/textures/") {
            if let Some(entity) = vanilla.strip_prefix("entity/") {
                if ["banner_base.png", "shield_base.png"].contains(&entity)
                    || entity.starts_with("banner/")
                    || entity.starts_with("shield/")
                {
                    flags |= 1 << 1;
                }
                if [
                    "enderdragon/dragon_eyes.png",
                    "enderman/enderman_eyes.png",
                    "phantom_eyes.png",
                    "spider_eyes.png",
                ]
                .contains(&entity)
                {
                    flags |= 1 << 2;
                }
            }
            if vanilla.starts_with("effect/") {
                flags |= 1 << 3;
            }
        }
        if let Some((_, rest)) = logical
            .strip_prefix("assets/")
            .and_then(|s| s.split_once('/'))
            && (rest.starts_with("textures/colormap/")
                || rest.starts_with("textures/font/")
                || rest.starts_with("textures/palettes/")
                || rest.starts_with("textures/trims/color_palettes/"))
        {
            flags |= 1 << 4;
        }
    }
    flags
}

fn kind_id(kind: FileKind) -> u8 {
    match kind {
        FileKind::PackMeta => 0,
        FileKind::Model => 1,
        FileKind::Blockstate => 2,
        FileKind::ItemDefinition => 3,
        FileKind::Atlas => 4,
        FileKind::Texture => 5,
        FileKind::TextureMeta => 6,
        FileKind::Sound => 7,
        FileKind::SoundsJson => 8,
        FileKind::Font => 9,
        FileKind::FontFile => 10,
        FileKind::Lang => 11,
        FileKind::Shader => 12,
        FileKind::ShaderInclude => 13,
        FileKind::PostEffect => 14,
        FileKind::Equipment => 15,
        FileKind::Particle => 16,
        FileKind::Text => 17,
        FileKind::WaypointStyle => 18,
        FileKind::Other => 19,
        FileKind::Unknown => 20,
    }
}

fn class_id(class: EntryClass) -> u8 {
    match class {
        EntryClass::Text => 0,
        EntryClass::Compressed => 1,
        EntryClass::Other => 2,
    }
}

fn quirks_bits(quirks: Quirks) -> u8 {
    u8::from(quirks.imageio_grey)
        | (u8::from(quirks.abgr_layers) << 1)
        | (u8::from(quirks.additive_eyes) << 2)
        | (u8::from(quirks.imageio_textures) << 3)
}

fn encode_cached(output: Option<&Optimised>) -> Vec<u8> {
    let Some(output) = output else {
        return vec![0];
    };
    let mut value = vec![1];
    put(&mut value, &output.bytes);
    if let Some(loss) = &output.loss {
        value.push(1);
        put(&mut value, loss.key.as_bytes());
        put(&mut value, loss.metric.as_bytes());
        value.extend(loss.value.to_le_bytes());
    } else {
        value.push(0);
    }
    value
}

fn put(value: &mut Vec<u8>, bytes: &[u8]) {
    value.extend((bytes.len() as u64).to_le_bytes());
    value.extend(bytes);
}

/// `None` for a value this version cannot read: a miss.
fn decode(value: &[u8]) -> Option<Option<Optimised>> {
    let mut reader = Reader(value);
    match reader.take(1)? {
        [0] => return reader.0.is_empty().then_some(None),
        [1] => {}
        _ => return None,
    }
    let bytes = reader.field()?.to_vec();
    let loss = match reader.take(1)? {
        [0] => None,
        [1] => Some(LossRecord {
            key: String::from_utf8(reader.field()?.to_vec()).ok()?,
            metric: String::from_utf8(reader.field()?.to_vec()).ok()?,
            value: f64::from_le_bytes(reader.take(8)?.try_into().ok()?),
        }),
        _ => return None,
    };
    reader
        .0
        .is_empty()
        .then_some(Some(Optimised { bytes, loss }))
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let (head, tail) = self.0.split_at_checked(len)?;
        self.0 = tail;
        Some(head)
    }

    fn field(&mut self) -> Option<&'a [u8]> {
        let len = u64::from_le_bytes(self.take(8)?.try_into().ok()?);
        self.take(usize::try_from(len).ok()?)
    }
}

/// Each file's quirks, from the oldest pack format of each view that shows it; a file no view shows is not in the map.
fn quirks(layout: &Layout) -> BTreeMap<PhysPath, Quirks> {
    let mut quirks = BTreeMap::new();
    for view in &layout.views {
        let at = quirks_at(view.formats.min);
        for path in view.files.values() {
            let entry = quirks.entry(path.clone()).or_insert_with(Quirks::default);
            *entry = union(*entry, at);
        }
    }
    quirks
}

/// The behaviours a client of `format` may have. A format spans several versions, so a change shipped mid-format
/// counts from the next one: 24w38a and 24w39a share format 39, the eyes fix is certain from 40 (24w40a).
fn quirks_at(format: (u32, u32)) -> Quirks {
    // 1.12.2 decodes textures with javax.imageio; every client of format 4 (17w48a on) with stb_image.
    let stb_textures = format >= (4, 0);
    Quirks {
        imageio_grey: !stb_textures,
        abgr_layers: !stb_textures,
        additive_eyes: format < (40, 0),
        imageio_textures: !stb_textures,
    }
}

fn union(a: Quirks, b: Quirks) -> Quirks {
    Quirks {
        imageio_grey: a.imageio_grey || b.imageio_grey,
        abgr_layers: a.abgr_layers || b.abgr_layers,
        additive_eyes: a.additive_eyes || b.additive_eyes,
        imageio_textures: a.imageio_textures || b.imageio_textures,
    }
}

/// `PackInput::digest`: blake3 over the sorted paths and entry digests.
pub(crate) fn digest(files: &BTreeMap<PhysPath, Entry>) -> Digest {
    let mut hasher = blake3::Hasher::new();
    for (path, entry) in files {
        hasher
            .update(path.as_str().as_bytes())
            .update(&[0])
            .update(&entry.digest.0);
    }
    Digest(*hasher.finalize().as_bytes())
}
