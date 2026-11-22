use std::collections::BTreeMap;

use packprune_codec::png::{self, Image};
use packprune_pack::json::{self, Member, Node, Value};
use packprune_pack::{LogicalPath, PhysPath};
use packprune_pass::{Edits, Loss, Pass, PassCx, PassError, Stage};
use rayon::prelude::*;

use super::{Use, Views, no_options};

const NAME: &str = "animation-frames";

/// An animated strip whose frames repeat is rewritten with each distinct frame once, and
/// a `frames` list that gives the original order and times, so the client keeps one CPU copy and one GPU texture per
/// distinct frame. When only one distinct frame exists (and `interpolate` is false), the animation section is removed
/// completely (dropping the `.mcmeta` when empty) so the client uploads no dynamic frame texture and ticks nothing.
/// Consecutive identical frames in non-interpolated animations are merged by summing their `time`.
pub(crate) struct AnimationFrames;

impl Pass for AnimationFrames {
    fn name(&self) -> &'static str {
        NAME
    }

    fn stage(&self) -> Stage {
        Stage::Textures
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
        let mut jobs: Vec<(PhysPath, PhysPath, bool)> = Vec::new();
        for texture in usage.stitched() {
            let kept =
                LogicalPath::new(texture).is_ok_and(|logical| cx.kept_names.contains(&logical));
            if kept
                || usage.loaded_as_file(texture)
                || usage.vanilla(texture)
                || !usage.placements(texture).iter().all(|p| p.plain(texture))
            {
                continue;
            }
            let meta_logical = LogicalPath::new(&format!("{texture}.mcmeta")).ok();
            let meta_kept = meta_logical
                .as_ref()
                .is_some_and(|l| cx.kept_names.contains(l));
            for png in usage.layers(texture).into_values() {
                let meta = PhysPath::new(&format!("{png}.mcmeta")).expect("a valid path");
                if !cx.pack.files.contains_key(&meta) {
                    continue;
                }
                if !paired_alone(&views, texture, &png, &meta) {
                    continue;
                }
                let can_remove_meta = !meta_kept && !cx.pinned.contains(&meta);
                jobs.push((png, meta, can_remove_meta));
            }
        }
        let outcomes: Vec<_> = jobs
            .into_par_iter()
            .filter_map(|(png, meta, can_remove_meta)| {
                let png_bytes = &cx.pack.files[&png].blob;
                let meta_bytes = &cx.pack.files[&meta].blob;
                let strip = dedupe_with_policy(png_bytes, meta_bytes, can_remove_meta)?;
                Some((png, meta, png_bytes.len(), strip))
            })
            .collect();
        let mut edits = Edits::default();
        let (mut textures, mut collapsed, mut removed, mut before, mut after) = (0, 0, 0, 0, 0);
        for (png, meta, orig_len, strip) in outcomes {
            textures += 1;
            removed += strip.removed;
            before += orig_len;
            after += strip.png.len();
            edits.replaced.insert(png, strip.png.into());
            match strip.meta_action {
                MetaAction::Replace(bytes) => {
                    edits.replaced.insert(meta, bytes.into());
                }
                MetaAction::Remove => {
                    collapsed += 1;
                    edits.removed.insert(meta);
                }
            }
        }
        edits
            .changes
            .push(format!("{textures} animated textures rewritten"));
        if collapsed > 0 {
            edits
                .changes
                .push(format!("{collapsed} static animations collapsed"));
        }
        edits
            .changes
            .push(format!("{removed} repeated frames removed"));
        edits
            .changes
            .push(format!("strips {before} -> {after} B as PNG"));
        Ok(edits)
    }
}

/// No view shows the PNG with another metadata file, or the metadata with another PNG: both are rewritten together.
fn paired_alone(views: &Views<'_>, texture: &str, png: &PhysPath, meta: &PhysPath) -> bool {
    let shows = |view: &packprune_pack::View, logical: &str, physical: &PhysPath| {
        LogicalPath::new(logical)
            .ok()
            .and_then(|logical| view.files.get(&logical))
            == Some(physical)
    };
    views
        .list
        .iter()
        .all(|view| shows(view, texture, png) == shows(view, &format!("{texture}.mcmeta"), meta))
}

pub(crate) enum MetaAction {
    Replace(Vec<u8>),
    Remove,
}

pub(crate) struct Strip {
    pub png: Vec<u8>,
    pub meta_action: MetaAction,
    pub removed: usize,
}

/// One frame of the original sequence: its cell and the entry that named it.
struct Step<'n> {
    index: usize,
    entry: Option<&'n Node>,
}

/// `None` when the animation has no frame to drop, or is not one the client reads the way this pass models it.
#[cfg(test)]
pub(crate) fn dedupe(png_bytes: &[u8], meta_bytes: &[u8]) -> Option<Strip> {
    dedupe_with_policy(png_bytes, meta_bytes, true)
}

pub(crate) fn dedupe_with_policy(
    png_bytes: &[u8],
    meta_bytes: &[u8],
    can_remove_meta: bool,
) -> Option<Strip> {
    let mut root = json::parse(meta_bytes).ok()?;
    let animation = root.get("animation").filter(|node| node.is_object())?;
    let image = png::decode(png_bytes).ok()?;
    let (frame_width, frame_height) = frame_size(animation, &image)?;
    if image.width % frame_width != 0 || image.height % frame_height != 0 {
        return None;
    }
    let columns = image.width / frame_width;
    let rows = image.height / frame_height;
    let cells = (columns * rows) as usize;
    let sequence = sequence(animation, cells)?;
    let interpolate = animation.path("interpolate").as_bool(false);
    let default_time = animation
        .get("frametime")
        .and_then(Node::int)
        .filter(|&t| t > 0)
        .unwrap_or(1);
    let cell = |index: usize| {
        let index = index as u32;
        block(
            &image,
            index % columns * frame_width,
            index / columns * frame_height,
            frame_width,
            frame_height,
        )
    };
    let mut distinct: BTreeMap<Vec<u8>, usize> = BTreeMap::new();
    let mut source = Vec::new();
    let mut mapped = Vec::with_capacity(sequence.len());
    // The client uploads the first cell before it plays anything, so it stays the first one.
    distinct.insert(cell(0), 0);
    source.push(0);
    for step in &sequence {
        let pixels = cell(step.index);
        let next = distinct.len();
        let known = *distinct.entry(pixels).or_insert_with(|| {
            source.push(step.index);
            next
        });
        mapped.push(known);
    }

    // Static animation: every played frame has identical pixels and no interpolation blends anything.
    // Collapse to a single-frame static texture and remove the `animation` section (and empty `.mcmeta`).
    if distinct.len() == 1 && !interpolate {
        let pixels = cell(0);
        let png = png::encode(&Image {
            width: frame_width,
            height: frame_height,
            rgba: pixels,
        })
        .ok()?;
        members(&mut root).retain(|member| &*member.name != "animation");
        let meta_action = if members(&mut root).is_empty() && can_remove_meta {
            MetaAction::Remove
        } else {
            MetaAction::Replace(json::write(&root))
        };
        return Some(Strip {
            png,
            meta_action,
            removed: cells.saturating_sub(1),
        });
    }

    // Merge consecutive identical frames when `!interpolate` (never across the loop boundary).
    let mut merged_frames: Vec<(usize, i32)> = Vec::with_capacity(sequence.len());
    for (step, &index) in sequence.iter().zip(&mapped) {
        let time = step
            .entry
            .and_then(|e| e.is_object().then(|| e.get("time")).flatten())
            .and_then(Node::int)
            .filter(|&t| t > 0)
            .unwrap_or(default_time);
        if !interpolate
            && let Some(last) = merged_frames.last_mut()
            && last.0 == index
            && let Some(summed) = last.1.checked_add(time)
        {
            last.1 = summed;
            continue;
        }
        merged_frames.push((index, time));
    }
    let merged_nodes: Option<Vec<Node>> =
        if !interpolate && merged_frames.len() >= 2 && merged_frames.len() < sequence.len() {
            let merged: Vec<Node> = merged_frames
                .into_iter()
                .map(|(index, time)| {
                    if time == default_time {
                        Node::new(Value::Int(index as i32))
                    } else {
                        Node::new(Value::Object(vec![
                            Member::new("index", Node::new(Value::Int(index as i32))),
                            Member::new("time", Node::new(Value::Int(time))),
                        ]))
                    }
                })
                .collect();
            let unmerged: Vec<Node> = sequence
                .iter()
                .zip(&mapped)
                .map(|(step, &index)| {
                    let index = Node::new(Value::Int(index as i32));
                    match step.entry {
                        Some(entry) if entry.is_object() => {
                            let mut copy = entry.clone();
                            set(&mut copy, "index", index);
                            copy
                        }
                        _ => index,
                    }
                })
                .collect();
            let merged_len = json::write(&Node::new(Value::Array(merged.clone()))).len();
            let unmerged_len = json::write(&Node::new(Value::Array(unmerged))).len();
            (merged_len < unmerged_len).then_some(merged)
        } else {
            None
        };

    if distinct.len() >= cells && merged_nodes.is_none() {
        return None;
    }
    let kept = distinct.len() as u32;
    let new_columns = if columns == 1 {
        1
    } else if rows == 1 {
        kept
    } else {
        columns.min(kept)
    };
    let new_rows = kept.div_ceil(new_columns);
    let width = new_columns * frame_width;
    let height = new_rows * frame_height;
    let mut rgba = vec![0; (width * height * 4) as usize];
    let row_bytes = (frame_width * 4) as usize;
    for (i, &from) in source.iter().enumerate() {
        let pixels = cell(from);
        let i = i as u32;
        for row in 0..frame_height {
            let y = i / new_columns * frame_height + row;
            let at = ((y * width + i % new_columns * frame_width) * 4) as usize;
            let start = row as usize * row_bytes;
            rgba[at..at + row_bytes].copy_from_slice(&pixels[start..start + row_bytes]);
        }
    }
    let frames: Vec<Node> = match merged_nodes {
        Some(merged) => merged,
        None => sequence
            .iter()
            .zip(&mapped)
            .map(|(step, &index)| {
                let index = Node::new(Value::Int(index as i32));
                match step.entry {
                    Some(entry) if entry.is_object() => {
                        let mut copy = entry.clone();
                        set(&mut copy, "index", index);
                        copy
                    }
                    _ => index,
                }
            })
            .collect(),
    };
    let png = png::encode(&Image {
        width,
        height,
        rgba,
    })
    .ok()?;
    let animation = members(&mut root)
        .iter_mut()
        .find(|member| &*member.name == "animation")
        .map(|member| &mut member.node)
        .expect("checked above");
    set(animation, "frames", Node::new(Value::Array(frames)));
    Some(Strip {
        png,
        meta_action: MetaAction::Replace(json::write(&root)),
        removed: cells - kept as usize,
    })
}

/// `AnimationMetadataSection.calculateFrameSize`: a square of the shorter side when the metadata gives none.
fn frame_size(animation: &Node, image: &Image) -> Option<(u32, u32)> {
    let width = positive(animation.get("width"));
    let height = positive(animation.get("height"));
    if width == 0 || height == 0 {
        return None;
    }
    if width > 0 || height > 0 {
        let pick = |given: i32, whole: u32| if given > 0 { given as u32 } else { whole };
        return Some((pick(width, image.width), pick(height, image.height)));
    }
    let side = image.width.min(image.height);
    Some((side, side))
}

/// -1 when absent, 0 when present but not a positive integer.
fn positive(node: Option<&Node>) -> i32 {
    match node {
        None => -1,
        Some(node) => node.int().filter(|value| *value > 0).unwrap_or(0),
    }
}

/// The frames in the order the client plays them; `None` when the list is one the client would not read as written.
fn sequence(animation: &Node, cells: usize) -> Option<Vec<Step<'_>>> {
    if let Some(time) = animation.get("frametime")
        && positive(Some(time)) <= 0
    {
        return None;
    }
    let Some(frames) = animation.get("frames") else {
        return Some(
            (0..cells)
                .map(|index| Step { index, entry: None })
                .collect(),
        );
    };
    // The client plays fewer than two frames as the strip's first cell, which is not what the list names.
    if !frames.is_array() || frames.size() < 2 {
        return None;
    }
    frames
        .items()
        .iter()
        .map(|entry| {
            let index = if entry.is_object() {
                entry.get("index")?
            } else {
                entry
            };
            let index = usize::try_from(index.int()?)
                .ok()
                .filter(|index| *index < cells)?;
            if entry.is_object()
                && let Some(time) = entry.get("time")
                && positive(Some(time)) <= 0
            {
                return None;
            }
            Some(Step {
                index,
                entry: Some(entry),
            })
        })
        .collect()
}

/// The `width` by `height` block of `image` at `x, y`, as RGBA rows.
fn block(image: &Image, x: u32, y: u32, width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((width * height * 4) as usize);
    for row in y..y + height {
        let at = ((row * image.width + x) * 4) as usize;
        out.extend_from_slice(&image.rgba[at..at + (width * 4) as usize]);
    }
    out
}

/// Sets member `name` of `object`: replaced where it is, or appended.
fn set(object: &mut Node, name: &str, node: Node) {
    let members = members(object);
    match members.iter_mut().find(|member| &*member.name == name) {
        Some(member) => member.node = node,
        None => members.push(Member::new(name, node)),
    }
}

fn members(object: &mut Node) -> &mut Vec<Member> {
    match &mut object.value {
        Value::Object(members) => members,
        _ => unreachable!("an object node"),
    }
}
