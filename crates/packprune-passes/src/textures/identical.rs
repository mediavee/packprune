use std::cell::RefCell;
use std::collections::BTreeMap;

use packprune_codec::png;
use packprune_graph::{Names, Reference};
use packprune_pack::json::{self, Node};
use packprune_pack::{Digest, FileKind, LogicalPath, PhysPath, ResourceId, TextureSize};
use packprune_pass::{Edits, Loss, Pass, PassCx, PassError, Stage};

use super::{Use, Views, at, file, no_options};

const NAME: &str = "identical-sprites";

/// Textures with identical pixels and identical `.mcmeta` in every layer of the pack keep
/// one file, and the models that named the others name it. Only sprites the pack's models name and nothing else does:
/// a sprite id a usage source names, a font sheet, an equipment texture or a vanilla path stays, because something
/// outside the pack's files knows it by name.
pub(crate) struct IdenticalSprites;

impl Pass for IdenticalSprites {
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
        let mut definitions: BTreeMap<ResourceId, Vec<&Reference>> = BTreeMap::new();
        for reference in cx.index.all().values().flatten() {
            if let Names::SpriteName { sprite, .. } = &reference.names {
                definitions.entry(*sprite).or_default().push(reference);
            }
        }
        let atlases = Atlases::default();
        let removable = |texture: &str| removable(cx, &usage, &definitions, &atlases, texture);
        let mut edits = Edits::default();
        let (mut merged, mut bytes) = (0, 0);
        for group in identical(cx, &usage) {
            let canonical = group
                .iter()
                .find(|texture| !removable(texture))
                .unwrap_or(&group[0]);
            for texture in &group {
                if texture == canonical || !removable(texture) {
                    continue;
                }
                for png in usage.layers(texture).into_values() {
                    bytes += cx.pack.files[&png].blob.len();
                    let meta = PhysPath::new(&format!("{png}.mcmeta")).expect("a valid path");
                    if cx.pack.files.contains_key(&meta) {
                        edits.removed.insert(meta);
                    }
                    edits.removed.insert(png);
                }
                edits
                    .file_remaps
                    .insert(logical(texture), logical(canonical));
                edits.sprite_remaps.insert(
                    usage
                        .sole_id(texture)
                        .expect("grouped textures have one id"),
                    usage
                        .sole_id(canonical)
                        .expect("grouped textures have one id"),
                );
                merged += 1;
            }
        }
        edits
            .changes
            .push(format!("{merged} sprites merged into an identical one"));
        edits.changes.push(format!("{bytes} B of PNG removed"));
        Ok(edits)
    }
}

fn logical(texture: &str) -> LogicalPath {
    LogicalPath::new(texture).expect("a texture of a view is a logical path")
}

/// One layer of a texture as the client draws it: the layer, the decoded pixels, the digest of its `.mcmeta`.
type Signature = Vec<(String, Vec<u8>, Option<Digest>)>;

/// The groups of two or more textures that draw the same everywhere: the same pixels and metadata bytes in the same
/// layers, each the sole input of one sprite id, in the same atlases of the same views. Each group is sorted.
fn identical<'v>(cx: &PassCx<'_>, usage: &Use<'v>) -> Vec<Vec<&'v str>> {
    let mut by_size: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for texture in usage.stitched() {
        let layers = usage.layers(texture);
        let sizes: Option<Vec<TextureSize>> = layers
            .values()
            .map(|png| TextureSize::of_png(&cx.pack.files[png].blob))
            .collect();
        let Some(sizes) = sizes else { continue };
        if FileKind::of(texture) != FileKind::Texture
            || layers.is_empty()
            || usage.sole_id(texture).is_none()
        {
            continue;
        }
        let mut key = format!("{:?}", usage.slots(texture));
        for (layer, size) in layers.keys().zip(sizes) {
            key.push_str(&format!("\n{layer} {}x{}", size.width, size.height));
        }
        by_size.entry(key).or_default().push(texture);
    }
    let mut groups = Vec::new();
    for sized in by_size.into_values().filter(|sized| sized.len() > 1) {
        let mut by_signature: BTreeMap<Signature, Vec<&str>> = BTreeMap::new();
        for texture in sized {
            if let Some(signature) = signature(cx, usage, texture) {
                by_signature.entry(signature).or_default().push(texture);
            }
        }
        groups.extend(by_signature.into_values().filter(|group| group.len() > 1));
    }
    groups
}

/// Per layer, the decoded pixels and the metadata digest; `None` when a PNG does not decode.
fn signature(cx: &PassCx<'_>, usage: &Use<'_>, texture: &str) -> Option<Signature> {
    usage
        .layers(texture)
        .into_iter()
        .map(|(layer, png)| {
            let image = png::decode(&cx.pack.files[&png].blob).ok()?;
            let meta = PhysPath::new(&format!("{png}.mcmeta"))
                .ok()
                .and_then(|meta| cx.pack.files.get(&meta))
                .map(|entry| entry.digest);
            Some((layer.to_owned(), image.rgba, meta))
        })
        .collect()
}

/// True when `texture` may be replaced by an identical one: nothing names it but the pack's models and atlas sources,
/// it is not vanilla's, and every atlas source that defines its sprite id reads this texture.
fn removable(
    cx: &PassCx<'_>,
    usage: &Use<'_>,
    definitions: &BTreeMap<ResourceId, Vec<&Reference>>,
    atlases: &Atlases,
    texture: &str,
) -> bool {
    let Some(id) = usage.sole_id(texture) else {
        return false;
    };
    !cx.kept_names.contains(&logical(texture))
        && !usage.vanilla(texture)
        && !usage.pinned(texture)
        && usage.named_by_model(id)
        && usage
            .layers(texture)
            .values()
            .all(|png| cx.scope.contains(png))
        && definitions
            .get(&id)
            .into_iter()
            .flatten()
            .all(|reference| reads_texture(cx, atlases, reference, texture))
}

/// Atlas files parsed once for the pass, by path; `None` when one does not parse.
type Atlases = RefCell<BTreeMap<PhysPath, Option<Node>>>;

/// The atlas source behind a sprite name is a `single` of `texture`, so renaming the sprite renames nothing else.
fn reads_texture(cx: &PassCx<'_>, atlases: &Atlases, reference: &Reference, texture: &str) -> bool {
    let mut atlases = atlases.borrow_mut();
    let parsed = atlases.entry(reference.file.clone()).or_insert_with(|| {
        cx.pack
            .files
            .get(&reference.file)
            .and_then(|entry| json::parse(&entry.blob).ok())
    });
    let Some(root) = parsed.as_ref() else {
        return false;
    };
    let Names::SpriteName { implicit, .. } = reference.names else {
        return false;
    };
    let pointer = &*reference.pointer.0;
    let source = if implicit {
        at(root, pointer)
    } else {
        at(root, &pointer[..pointer.rfind('/').unwrap_or(0)])
    };
    let interner = &cx.run.interner;
    let parse = |field: &str| ResourceId::parse(interner, &source.path(field).as_string(""));
    parse("type").is_some_and(|kind| {
        interner.resolve(kind.ns) == "minecraft" && interner.resolve(kind.path) == "single"
    }) && parse("resource")
        .is_some_and(|resource| file(interner, resource, "textures", ".png") == texture)
}
