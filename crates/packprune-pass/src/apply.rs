//! Applying one pass's edits: its file edits, the `.mcmeta` that follow a renamed texture,
//! then every indexed reference its remaps name, rewritten by splice. An edit the framework does not allow is a
//! [`Refusal`], and the run stops with it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use packprune_graph::atlases::{AtlasFile, VanillaAtlases, VanillaLayer, ViewAtlases};
use packprune_graph::{Change, Names, ReferenceIndex};
use packprune_pack::json::{self, Field};
use packprune_pack::kinds::ResourcePath;
use packprune_pack::{
    Blob, Digest, Entry, FileKind, Interner, Layout, LogicalPath, PackInput, PackMeta, PhysPath,
    Pointer, ResourceId,
};

use crate::run::{Context, Kept, RunError};
use crate::{Edits, PassError, Remap, Scope};

/// Why the framework refuses a pass's edits.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("{verb} {path}, which the pack does not have")]
    Absent { verb: &'static str, path: PhysPath },
    #[error("adds {path}, which exists; replace it instead")]
    Exists { path: PhysPath },
    #[error("{first} and {second} {path}: two edits on one path")]
    TwoEdits {
        path: PhysPath,
        first: &'static str,
        second: &'static str,
    },
    #[error("moves {from} to {to}, but does not remove the first and add the second")]
    MoveNotEdited { from: PhysPath, to: PhysPath },
    #[error(
        "removes {from} and adds {to}, the same file in another overlay, without declaring a move"
    )]
    UndeclaredMove { from: PhysPath, to: PhysPath },
    #[error(
        "remaps {from}, a name that stays (an item definition, font, sounds.json, vanilla path or what a usage source names)"
    )]
    KeptName { from: LogicalPath },
    #[error("remaps {logical}, which {file} names in text")]
    TextName {
        logical: LogicalPath,
        file: PhysPath,
    },
    #[error("remaps sprite {sprite} of {atlas}, a name that stays (its texture {texture} {why})")]
    KeptSprite {
        sprite: Box<str>,
        atlas: Box<str>,
        texture: Box<str>,
        why: &'static str,
    },
    #[error("{verb} {path}, a path a source names")]
    Pinned { verb: &'static str, path: PhysPath },
    #[error("remaps {logical}, the name of {path}, a path a source names")]
    PinnedName {
        logical: LogicalPath,
        path: PhysPath,
    },
    /// A remap whose target is itself remapped in the same step: a cycle, or a chain whose result depends on
    /// the order the remaps are read in.
    #[error("remaps {from} to {to}, which it also remaps: a remap cycle")]
    Cycle { from: Box<str>, to: Box<str> },
    #[error("remaps {from} onto {to}, whose .mcmeta differs from {metadata}")]
    Metadata {
        from: LogicalPath,
        to: LogicalPath,
        metadata: PhysPath,
    },
    #[error("remaps {from} to {to}, which {file}{pointer} cannot name ({folder}/*{suffix})")]
    Unnameable {
        from: LogicalPath,
        to: LogicalPath,
        file: PhysPath,
        pointer: Box<str>,
        folder: Box<str>,
        suffix: Box<str>,
    },
    #[error("remaps sprite {from} to {to}, which {file}{pointer} cannot name")]
    UnnameableSprite {
        from: Box<str>,
        to: Box<str>,
        file: PhysPath,
        pointer: Box<str>,
    },
    #[error("changes {path}, which the pass's scope does not name")]
    OutsideScope { path: PhysPath },
}

/// The pack the passes work on, with its layout and index; a pass's edits are applied to it in place.
pub(crate) struct Current {
    pub pack: PackInput,
    pub layout: Layout,
    pub index: ReferenceIndex,
}

/// What one pass's edits did: the paths whose bytes differ, and the edits as one remap step: the pass's declared
/// moves and renames, and the moves the framework made for them.
pub(crate) struct Applied {
    pub changed: Vec<PhysPath>,
    pub remap: Remap,
}

/// The digest each path had before the edits, recorded at its first change: what was there is what the changed
/// paths and the layout compare against.
#[derive(Default)]
struct Before(BTreeMap<PhysPath, Option<Digest>>);

impl Before {
    fn insert(&mut self, files: &mut BTreeMap<PhysPath, Entry>, path: PhysPath, entry: Entry) {
        self.record(files, &path);
        files.insert(path, entry);
    }

    fn remove(&mut self, files: &mut BTreeMap<PhysPath, Entry>, path: &PhysPath) {
        self.record(files, path);
        files.remove(path);
    }

    fn record(&mut self, files: &BTreeMap<PhysPath, Entry>, path: &PhysPath) {
        if !self.0.contains_key(path) {
            self.0
                .insert(path.clone(), files.get(path).map(|entry| entry.digest));
        }
    }

    /// The paths present on one side only.
    fn presence(&self, files: &BTreeMap<PhysPath, Entry>) -> Vec<PhysPath> {
        self.0
            .iter()
            .filter(|(path, digest)| files.contains_key(*path) != digest.is_some())
            .map(|(path, _)| path.clone())
            .collect()
    }

    /// Every path whose bytes differ, present on one side only included.
    fn changed(&self, files: &BTreeMap<PhysPath, Entry>) -> Vec<PhysPath> {
        self.0
            .iter()
            .filter(|(path, digest)| files.get(*path).map(|entry| entry.digest) != **digest)
            .map(|(path, _)| path.clone())
            .collect()
    }
}

pub(crate) fn apply(
    cx: &Context<'_>,
    kept: &Kept,
    state: &mut Current,
    pass: &'static str,
    scope: &Scope,
    edits: &Edits,
) -> Result<Applied, RunError> {
    let refuse = |refusal| RunError::Refused { pass, refusal };
    check_edits(state, kept, edits).map_err(refuse)?;
    let remaps = !edits.file_remaps.is_empty() || !edits.sprite_remaps.is_empty();
    if remaps {
        check_remaps(cx, state, kept, edits).map_err(refuse)?;
    }

    // Tokens are a function of the bytes and the kind, which `pack.mcmeta` can change for every overlay file.
    let meta_edited = edits
        .replaced
        .keys()
        .any(|path| path.as_str() == "pack.mcmeta");
    let move_targets: BTreeMap<&PhysPath, &PhysPath> =
        edits.moves.iter().map(|(from, to)| (to, from)).collect();
    // A move whose bytes and kind stay is a rename for the index: its tokens follow it.
    let renamed: BTreeSet<&PhysPath> = edits
        .added
        .iter()
        .filter(|(path, blob)| {
            !meta_edited
                && move_targets.get(path).is_some_and(|from| {
                    state.pack.files[*from].digest == digest(blob)
                        && kind_at(cx, &state.layout, from) == kind_at(cx, &state.layout, path)
                })
        })
        .map(|(path, _)| path)
        .collect();
    let mut before = Before::default();
    let files = &mut state.pack.files;
    let mut touched = BTreeSet::new();
    let mut moves = edits.moves.clone();
    for (path, blob) in &edits.replaced {
        before.insert(files, path.clone(), entry(blob));
        touched.insert(path.clone());
    }
    for path in &edits.removed {
        before.remove(files, path);
        if !edits.moves.contains_key(path) {
            touched.insert(path.clone());
        }
    }
    for (path, blob) in &edits.added {
        if !renamed.contains(path) {
            touched.insert(path.clone());
        }
        before.insert(files, path.clone(), entry(blob));
    }
    if !edits.file_remaps.is_empty() {
        move_metadata(
            &state.layout.meta,
            kept,
            edits,
            files,
            &mut before,
            &mut touched,
            &mut moves,
        )
        .map_err(refuse)?;
    }

    let presence = before.presence(files);
    update(
        cx,
        state,
        Change {
            touched,
            moved: edits.moves.clone(),
            layout_changed: false,
        },
        &presence,
    )?;

    if remaps {
        let spliced = rewrite_references(cx, &state.pack, &state.index, edits).map_err(
            |error| match error {
                Rewrite::Refused(refusal) => refuse(refusal),
                Rewrite::Splice(file, why) => RunError::Pass(PassError::Failed {
                    pass,
                    why: format!("rewriting the references of {file}: {why}"),
                }),
            },
        )?;
        if !spliced.is_empty() {
            let touched = spliced.keys().cloned().collect();
            for (path, bytes) in spliced {
                before.insert(&mut state.pack.files, path, entry(&Blob::from(bytes)));
            }
            update(
                cx,
                state,
                Change {
                    touched,
                    ..Change::default()
                },
                &[],
            )?;
        }
    }

    let changed = before.changed(&state.pack.files);
    if let Some(path) = changed.iter().find(|path| !scope.contains(path)) {
        return Err(refuse(Refusal::OutsideScope { path: path.clone() }));
    }
    Ok(Applied {
        changed,
        remap: Remap::step(
            edits.file_remaps.clone(),
            moves,
            edits.sprite_remaps.clone(),
        ),
    })
}

/// The file edits alone: each path edited once, removed and replaced paths present, added ones absent, every move
/// declared both ways, and no pinned path removed.
fn check_edits(state: &Current, kept: &Kept, edits: &Edits) -> Result<(), Refusal> {
    let files = &state.pack.files;
    let two = |path: &PhysPath, first, second| Refusal::TwoEdits {
        path: path.clone(),
        first,
        second,
    };
    for path in edits.replaced.keys() {
        if edits.removed.contains(path) {
            return Err(two(path, "replaces", "removes"));
        }
        if edits.added.contains_key(path) {
            return Err(two(path, "replaces", "adds"));
        }
    }
    if let Some(path) = edits
        .removed
        .iter()
        .find(|path| edits.added.contains_key(*path))
    {
        return Err(two(path, "removes", "adds"));
    }
    let absent = |verb, path: &PhysPath| Refusal::Absent {
        verb,
        path: path.clone(),
    };
    if let Some(path) = edits
        .replaced
        .keys()
        .find(|path| !files.contains_key(*path))
    {
        return Err(absent("replaces", path));
    }
    if let Some(path) = edits.removed.iter().find(|path| !files.contains_key(*path)) {
        return Err(absent("removes", path));
    }
    if let Some(path) = edits.added.keys().find(|path| files.contains_key(*path)) {
        return Err(Refusal::Exists { path: path.clone() });
    }
    for (from, to) in &edits.moves {
        if !edits.removed.contains(from) || !edits.added.contains_key(to) {
            return Err(Refusal::MoveNotEdited {
                from: from.clone(),
                to: to.clone(),
            });
        }
    }
    // The self-check follows a file to another overlay only through a declared move.
    let mut added: BTreeMap<String, Vec<&PhysPath>> = BTreeMap::new();
    for path in edits.added.keys() {
        added
            .entry(logical_part(path).to_owned())
            .or_default()
            .push(path);
    }
    for from in edits
        .removed
        .iter()
        .filter(|from| !edits.moves.contains_key(*from))
    {
        if let Some(to) = added
            .get(logical_part(from))
            .into_iter()
            .flatten()
            .find(|to| **to != from)
        {
            return Err(Refusal::UndeclaredMove {
                from: from.clone(),
                to: (*to).clone(),
            });
        }
    }
    if let Some(path) = edits
        .removed
        .iter()
        .find(|path| kept.pinned.contains(*path))
    {
        let verb = if edits.moves.contains_key(path) {
            "moves"
        } else {
            "removes"
        };
        return Err(Refusal::Pinned {
            verb,
            path: path.clone(),
        });
    }
    Ok(())
}

/// A name the client or the server can know stays, so a remap from one is refused; a remap that is
/// itself remapped is refused too.
fn check_remaps(
    cx: &Context<'_>,
    state: &Current,
    kept: &Kept,
    edits: &Edits,
) -> Result<(), Refusal> {
    let interner = &cx.run.interner;
    if let Some((from, to)) = edits
        .file_remaps
        .iter()
        .find(|(_, to)| edits.file_remaps.contains_key(*to))
    {
        return Err(Refusal::Cycle {
            from: from.as_str().into(),
            to: to.as_str().into(),
        });
    }
    if let Some((from, to)) = edits
        .sprite_remaps
        .iter()
        .find(|(_, to)| edits.sprite_remaps.contains_key(*to))
    {
        return Err(Refusal::Cycle {
            from: from.display(interner).to_string().into(),
            to: to.display(interner).to_string().into(),
        });
    }
    if let Some(from) = edits
        .file_remaps
        .keys()
        .find(|from| kept.names.contains(*from))
    {
        return Err(Refusal::KeptName { from: from.clone() });
    }
    for path in &kept.pinned {
        if let Some(logical) = state.layout.split(path).1
            && edits.file_remaps.contains_key(&logical)
        {
            return Err(Refusal::PinnedName {
                logical,
                path: path.clone(),
            });
        }
    }
    for (file, references) in state.index.all() {
        for reference in references {
            if let Names::Text { logical } = &reference.names
                && edits.file_remaps.contains_key(logical)
            {
                return Err(Refusal::TextName {
                    logical: logical.clone(),
                    file: file.clone(),
                });
            }
        }
    }
    if edits.sprite_remaps.is_empty() {
        return Ok(());
    }
    let layer = VanillaLayer::new(cx.vanilla, &state.layout.meta.filters).ok();
    let vanilla_atlases = layer
        .as_ref()
        .and_then(|layer| VanillaAtlases::with_layer(cx.run, layer).ok());
    let Some(vanilla_atlases) = vanilla_atlases else {
        return Ok(());
    };
    let atlas_files: BTreeMap<&PhysPath, AtlasFile> = state
        .layout
        .kinds
        .iter()
        .filter(|(_, kind)| **kind == FileKind::Atlas)
        .filter_map(|(path, _)| {
            let entry = state.pack.files.get(path)?;
            Some((path, AtlasFile::parse(cx.run, &entry.blob)?))
        })
        .collect();
    let views: Vec<ViewAtlases> = state
        .layout
        .views
        .iter()
        .map(|view| {
            ViewAtlases::build(cx.run, &vanilla_atlases, view, |path| atlas_files.get(path))
                .unwrap_or_default()
        })
        .collect();
    let vanilla_has = |logical: &str| match &layer {
        Some(layer) => layer.has(logical),
        None => cx.vanilla.has(logical),
    };
    for from in edits.sprite_remaps.keys() {
        for (view, atlases) in state.layout.views.iter().zip(&views) {
            for atlas in atlases.atlas_ids() {
                let Some(sprite) = atlases.sprite(atlas, *from) else {
                    continue;
                };
                for input in &sprite.inputs {
                    let in_view = LogicalPath::new(&input.logical)
                        .is_ok_and(|logical| view.files.contains_key(&logical));
                    let vanilla_only = !in_view && vanilla_has(&input.logical);
                    let named = LogicalPath::new(&input.logical)
                        .is_ok_and(|logical| kept.names.contains(&logical));
                    if named || vanilla_only {
                        return Err(Refusal::KeptSprite {
                            sprite: from.display(interner).to_string().into(),
                            atlas: atlas.into(),
                            texture: (*input.logical).into(),
                            why: if vanilla_only {
                                "is vanilla's"
                            } else {
                                "is named from outside the pack"
                            },
                        });
                    }
                }
            }
        }
    }
    Ok(())
}

/// A texture's `.mcmeta` is bound to it by path: it follows a file remap of the texture, in every layer, and the move
/// is added to `moves`.
fn move_metadata(
    meta: &PackMeta,
    kept: &Kept,
    edits: &Edits,
    files: &mut BTreeMap<PhysPath, Entry>,
    before: &mut Before,
    touched: &mut BTreeSet<PhysPath>,
    moves: &mut BTreeMap<PhysPath, PhysPath>,
) -> Result<(), Refusal> {
    let mut layers = vec![String::new()];
    for overlay in &meta.overlays {
        let layer = format!("{}/", overlay.directory);
        if !layers.contains(&layer) {
            layers.push(layer);
        }
    }
    for (from, to) in &edits.file_remaps {
        if !from.as_str().ends_with(".png") {
            continue;
        }
        for layer in &layers {
            let (Ok(source), Ok(target)) = (
                PhysPath::new(&format!("{layer}{from}.mcmeta")),
                PhysPath::new(&format!("{layer}{to}.mcmeta")),
            ) else {
                continue;
            };
            let Some(meta) = files.get(&source).cloned() else {
                continue;
            };
            if files
                .get(&target)
                .is_some_and(|existing| existing.digest != meta.digest)
            {
                return Err(Refusal::Metadata {
                    from: from.clone(),
                    to: to.clone(),
                    metadata: source,
                });
            }
            if kept.pinned.contains(&source) {
                return Err(Refusal::Pinned {
                    verb: "moves",
                    path: source,
                });
            }
            before.remove(files, &source);
            before.insert(files, target.clone(), meta);
            moves.insert(source.clone(), target.clone());
            touched.insert(source);
            touched.insert(target);
        }
    }
    Ok(())
}

enum Rewrite {
    Refused(Refusal),
    Splice(PhysPath, String),
}

/// The new bytes of every file a reference the remaps name is written in.
fn rewrite_references(
    cx: &Context<'_>,
    pack: &PackInput,
    index: &ReferenceIndex,
    edits: &Edits,
) -> Result<BTreeMap<PhysPath, Vec<u8>>, Rewrite> {
    let interner = &*cx.run.interner;
    let mut strings: BTreeMap<PhysPath, BTreeMap<Pointer, String>> = BTreeMap::new();
    let mut fields: BTreeMap<PhysPath, BTreeMap<Pointer, Vec<Field>>> = BTreeMap::new();
    let mut set = |file: &PhysPath, pointer: &Pointer, value: String| {
        let at = strings
            .entry(file.clone())
            .or_default()
            .entry(pointer.clone())
            .or_insert_with(|| value.clone());
        if *at == value {
            Ok(())
        } else {
            Err(Rewrite::Splice(
                file.clone(),
                format!("{} would be both {at} and {value}", pointer.0),
            ))
        }
    };
    for (file, references) in index.all() {
        for reference in references {
            let pointer = &reference.pointer;
            match &reference.names {
                Names::File {
                    logical,
                    folder,
                    suffix,
                } => {
                    let Some(to) = edits.file_remaps.get(logical) else {
                        continue;
                    };
                    let value = file_value(to, folder, suffix).ok_or_else(|| {
                        Rewrite::Refused(Refusal::Unnameable {
                            from: logical.clone(),
                            to: to.clone(),
                            file: file.clone(),
                            pointer: pointer.0.clone(),
                            folder: folder.clone(),
                            suffix: suffix.clone(),
                        })
                    })?;
                    set(file, pointer, value)?;
                }
                Names::Sprite {
                    sprite,
                    prefix,
                    suffix,
                    ..
                } => {
                    let Some(to) = edits.sprite_remaps.get(sprite) else {
                        continue;
                    };
                    let path = interner.resolve(to.path);
                    let inner = path
                        .strip_prefix(&**prefix)
                        .and_then(|rest| rest.strip_suffix(&**suffix))
                        .filter(|_| path.len() >= prefix.len() + suffix.len());
                    let Some(inner) = inner else {
                        return Err(Rewrite::Refused(Refusal::UnnameableSprite {
                            from: sprite.display(interner).to_string().into(),
                            to: to.display(interner).to_string().into(),
                            file: file.clone(),
                            pointer: pointer.0.clone(),
                        }));
                    };
                    set(
                        file,
                        pointer,
                        format!("{}:{inner}", interner.resolve(to.ns)),
                    )?;
                }
                Names::SpriteName {
                    sprite,
                    implicit: false,
                    ..
                } => {
                    if let Some(to) = edits.sprite_remaps.get(sprite) {
                        set(file, pointer, to.display(interner).to_string())?;
                    }
                }
                Names::SpriteName {
                    sprite,
                    implicit: true,
                    ..
                } => {
                    // A single source without a sprite field names its sprite by its resource: a remapped resource
                    // keeps the sprite id the rest of the pack knows unless the sprite is remapped too.
                    let texture = texture_of(interner, *sprite);
                    let resource = texture
                        .as_ref()
                        .map(|texture| edits.file_remaps.get(texture).unwrap_or(texture));
                    let named = edits.sprite_remaps.get(sprite).unwrap_or(sprite);
                    if texture_of(interner, *named).as_ref() != resource {
                        fields
                            .entry(file.clone())
                            .or_default()
                            .entry(pointer.clone())
                            .or_default()
                            .push(Field {
                                name: "sprite".into(),
                                value: named.display(interner).to_string(),
                            });
                    }
                }
                Names::SoundEvent { .. } | Names::Beside { .. } | Names::Text { .. } => {}
            }
        }
    }
    let files: BTreeSet<&PhysPath> = strings.keys().chain(fields.keys()).collect();
    let none_strings = BTreeMap::new();
    let none_fields = BTreeMap::new();
    files
        .into_iter()
        .map(|file| {
            let bytes = &pack.files[file].blob;
            json::edit(
                bytes,
                strings.get(file).unwrap_or(&none_strings),
                fields.get(file).unwrap_or(&none_fields),
            )
            .map(|edited| (file.clone(), edited))
            .map_err(|error| Rewrite::Splice(file.clone(), error.to_string()))
        })
        .collect()
}

/// The `ns:path` that names `logical` from a reference to `<folder>/*<suffix>`.
fn file_value(logical: &LogicalPath, folder: &str, suffix: &str) -> Option<String> {
    let text = logical.as_str();
    let path = ResourcePath::of(text)?;
    if !text.ends_with(suffix) {
        return None;
    }
    let prefix = if folder.is_empty() {
        format!("assets/{}/", path.namespace)
    } else {
        format!("assets/{}/{folder}/", path.namespace)
    };
    (text.starts_with(&prefix) && text.len() > prefix.len() + suffix.len()).then(|| {
        format!(
            "{}:{}",
            path.namespace,
            &text[prefix.len()..text.len() - suffix.len()]
        )
    })
}

/// The texture file of `sprite`: `assets/<ns>/textures/<path>.png`.
fn texture_of(interner: &Interner, sprite: ResourceId) -> Option<LogicalPath> {
    LogicalPath::new(&format!(
        "assets/{}/textures/{}.png",
        interner.resolve(sprite.ns),
        interner.resolve(sprite.path)
    ))
    .ok()
}

/// The kind `path` has under `layout`, as `Layout::new` classifies a file.
fn kind_at(cx: &Context<'_>, layout: &Layout, path: &PhysPath) -> FileKind {
    if matches!(path.as_str(), "pack.mcmeta" | "pack.png") {
        return FileKind::PackMeta;
    }
    let Some(logical) = layout.split(path).1 else {
        return FileKind::Unknown;
    };
    match FileKind::of(logical.as_str()) {
        FileKind::Unknown if cx.vanilla.has(logical.as_str()) => FileKind::Other,
        kind => kind,
    }
}

/// The path after its overlay folder.
fn logical_part(path: &PhysPath) -> &str {
    let text = path.as_str();
    if text.starts_with("assets/") {
        return text;
    }
    text.find("/assets/").map_or(text, |at| &text[at + 1..])
}

fn entry(blob: &Blob) -> Entry {
    Entry {
        blob: Arc::clone(blob),
        digest: digest(blob),
    }
}

fn digest(blob: &Blob) -> Digest {
    Digest(*blake3::hash(blob).as_bytes())
}

/// The layout and index of the pack after `change`: the layout is rebuilt when `pack.mcmeta` changed, else updated
/// for the paths present on one side only (`presence`); the index is updated.
fn update(
    cx: &Context<'_>,
    state: &mut Current,
    mut change: Change,
    presence: &[PhysPath],
) -> Result<(), RunError> {
    let meta_path = PhysPath::new("pack.mcmeta").expect("a valid path");
    change.layout_changed = change.touched.contains(&meta_path);
    if change.layout_changed {
        state.layout = Layout::new(&state.pack, PackMeta::read(&state.pack)?, cx.vanilla);
    } else if !presence.is_empty() {
        state.layout.update(&state.pack, presence, cx.vanilla);
    }
    redigest(&mut state.pack);
    state.index.update(
        cx.run,
        &state.pack,
        &state.layout,
        cx.rules,
        &change,
        cx.cache.tokens(),
    );
    #[cfg(debug_assertions)]
    assert_rebuilt(cx, &state.pack, &state.layout, &state.index);
    Ok(())
}

/// Debug builds check every incremental update against a full rebuild: every test and fixture run that prunes or
/// applies a pass's edits proves it.
#[cfg(debug_assertions)]
pub(crate) fn assert_rebuilt(
    cx: &Context<'_>,
    pack: &PackInput,
    updated: &Layout,
    index: &ReferenceIndex,
) {
    let meta = PackMeta::read(pack).expect("pack.mcmeta read as the update read it");
    let layout = Layout::new(pack, meta, cx.vanilla);
    assert!(
        layout == *updated,
        "the updated layout differs from a rebuild"
    );
    let built = ReferenceIndex::build(cx.run, pack, &layout, cx.rules, &packprune_graph::Computed);
    assert!(
        built.all() == index.all(),
        "the updated index differs from a rebuild"
    );
}

/// The pack digest over its entries, as [`PackInput::open`] computes it.
pub(crate) fn redigest(pack: &mut PackInput) {
    let mut digest = blake3::Hasher::new();
    for (path, entry) in &pack.files {
        digest
            .update(path.as_str().as_bytes())
            .update(&[0])
            .update(&entry.digest.0);
    }
    pack.digest = Digest(*digest.finalize().as_bytes());
}
