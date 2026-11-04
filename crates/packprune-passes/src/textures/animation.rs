use std::collections::BTreeMap;

use packprune_codec::png::{self, Image};
use packprune_pack::json::{self, Member, Node, Value};
use packprune_pack::{LogicalPath, PhysPath};
use packprune_pass::{Edits, Loss, Pass, PassCx, PassError, Stage};

use super::{Use, Views, no_options};

const NAME: &str = "animation-frames";

/// An animated strip whose frames repeat is rewritten with each distinct frame once, and
/// a `frames` list that gives the original order and times, so the client keeps one CPU copy and one GPU texture per
/// distinct frame. The list is the original sequence frame for frame, so an interpolated animation blends the same
/// pictures in the same order. Only textures an atlas alone loads, whose name nothing outside the pack gives:
/// anywhere else the client ignores the animation and draws the strip, and a name that stays is compared whole.
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
        let mut edits = Edits::default();
        let (mut textures, mut removed, mut before, mut after) = (0, 0, 0, 0);
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
            for png in usage.layers(texture).into_values() {
                let meta = PhysPath::new(&format!("{png}.mcmeta")).expect("a valid path");
                let Some(meta_entry) = cx.pack.files.get(&meta) else {
                    continue;
                };
                if !paired_alone(&views, texture, &png, &meta) {
                    continue;
                }
                let png_bytes = &cx.pack.files[&png].blob;
                if let Some(strip) = dedupe(png_bytes, &meta_entry.blob) {
                    textures += 1;
                    removed += strip.removed;
                    before += png_bytes.len();
                    after += strip.png.len();
                    edits.replaced.insert(png, strip.png.into());
                    edits.replaced.insert(meta, strip.meta.into());
                }
            }
        }
        edits
            .changes
            .push(format!("{textures} animated textures rewritten"));
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

pub(crate) struct Strip {
    pub png: Vec<u8>,
    pub meta: Vec<u8>,
    pub removed: usize,
}

/// One frame of the original sequence: its cell and the entry that named it.
struct Step<'n> {
    index: usize,
    entry: Option<&'n Node>,
}

/// `None` when the animation has no frame to drop, or is not one the client reads the way this pass models it.
pub(crate) fn dedupe(png_bytes: &[u8], meta_bytes: &[u8]) -> Option<Strip> {
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
    if distinct.len() >= cells {
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
    let frames: Vec<Node> = sequence
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
        meta: json::write(&root),
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
