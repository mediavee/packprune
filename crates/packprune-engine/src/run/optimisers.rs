//! The file optimisers: one task per file, each optimiser that applies run on what the previous one left,
//! the lossless ones then the lossy ones, each group in registry order, through the content cache. Each codec
//! checks its own output, so a cached output is a checked one.

use std::cmp::Reverse;
use std::collections::BTreeMap;

use packprune_config::Overrides;
use packprune_pack::{Digest, Entry, FileKind, Layout, PackInput, PhysPath, Quirks};
use packprune_pass::{FileCx, FileOptimizer, LossRecord, Optimised};
use rayon::prelude::*;

use super::RunError;
use super::cache::Cache;
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
    pub reports: Vec<OptimiserReport>,
    pub loss: Vec<LossRecord>,
}

/// One optimiser's change to one file.
struct Step {
    optimiser: usize,
    before: u64,
    after: u64,
    loss: Option<LossRecord>,
}

pub(crate) fn run(
    pack: &PackInput,
    layout: &Layout,
    optimisers: &[Optimiser<'_>],
    overrides: &Overrides<'_>,
    origins: &Origins,
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
    // Largest first, handed out in that order: a large PNG started last would run alone while the other workers
    // idle. The results are put back in path order, so the output does not depend on the schedule.
    let mut by_size: Vec<_> = pack.files.iter().collect();
    by_size.sort_by_key(|(path, entry)| (Reverse(entry.blob.len()), *path));
    let mut results: Vec<(PhysPath, Entry, Entry, Vec<Step>)> = by_size
        .into_iter()
        .par_bridge()
        .map(|(path, entry)| {
            let kind = layout.kinds.get(path).copied().unwrap_or(FileKind::Unknown);
            let cx = FileCx {
                kind,
                path: path.clone(),
                quirks: union(quirks.get(path).copied().unwrap_or(every_view), forced),
            };
            let mut current = entry.clone();
            let mut lossless = None;
            let mut steps = Vec::new();
            for &index in &order {
                let enabled = &optimisers[index];
                let optimiser = enabled.optimiser;
                if enabled.lossy.is_some() && lossless.is_none() {
                    lossless = Some(current.clone());
                }
                if !optimiser.applies(kind, path) {
                    continue;
                }
                // Globs and overrides name the input's paths: a renamed or merged file is matched by those.
                let options = match &enabled.lossy {
                    Some((globs, options))
                        if origins.all(path, |origin| globs.is_match(origin)) =>
                    {
                        options.clone()
                    }
                    Some(_) => continue,
                    None => match origins.options(overrides, optimiser.name(), path) {
                        Some(options) => toml_table(&options).map_err(|why| {
                            RunError::Refused(format!(
                                "{} options of {path}: {why}",
                                optimiser.name()
                            ))
                        })?,
                        None => continue,
                    },
                };
                if let Some(output) = optimise(optimiser, &current, &cx, &options, cache)? {
                    steps.push(Step {
                        optimiser: index,
                        before: current.blob.len() as u64,
                        after: output.bytes.len() as u64,
                        loss: output.loss,
                    });
                    current = Entry {
                        digest: Digest(*blake3::hash(&output.bytes).as_bytes()),
                        blob: output.bytes.into(),
                    };
                }
            }
            let lossless = lossless.unwrap_or_else(|| current.clone());
            Ok((path.clone(), current, lossless, steps))
        })
        .collect::<Result<_, RunError>>()?;
    results.sort_unstable_by(|a, b| a.0.cmp(&b.0));

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
    let mut lossy = false;
    for (path, entry, before_lossy, steps) in results {
        lossy |= before_lossy.digest != entry.digest;
        lossless.insert(path.clone(), before_lossy);
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
        reports,
        loss,
    })
}

/// The optimiser's output for `input`, from the cache when this content, path, kind, quirks and options were
/// optimised before. The path is in the key because the optimiser sees it (`FileCx`): the PNG optimiser keeps the
/// colour type of banner layers by path.
fn optimise(
    optimiser: &dyn FileOptimizer,
    input: &Entry,
    cx: &FileCx,
    options: &toml::Table,
    cache: &Cache,
) -> Result<Option<Optimised>, RunError> {
    let step = format!("file-optimiser {}", optimiser.name());
    let key = format!(
        "{options}\0{}\0{:?}\0{:?}",
        cx.path,
        cx.kind,
        cx.quirks,
        options = toml::to_string(options).expect("a table serialises"),
    );
    if let Some(output) = cache.get(&step, key.as_bytes(), &[input.digest.0], decode) {
        return Ok(output);
    }
    let output = optimiser
        .optimise(&input.blob, cx, options)
        .map_err(packprune_pass::RunError::from)?;
    cache.put(
        &step,
        key.as_bytes(),
        &[input.digest.0],
        &encode(output.as_ref()),
    );
    Ok(output)
}

fn encode(output: Option<&Optimised>) -> Vec<u8> {
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
