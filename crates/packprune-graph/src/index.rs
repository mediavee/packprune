//! The reference index: tokens per file, keyed on content, and their attachment, recomputed per run. Each reference
//! keeps where the walk first notes it (view, visited file, rank in that visit), which fixes the order of references
//! merged from several views and shader walks, so `update` can redo a part without changing it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use packprune_pack::{
    ClientRules, FileKind, Layout, LogicalPath, PackInput, PhysPath, Pointer, RunCx, ViewId,
    ViewSet,
};
use rayon::prelude::*;

use crate::facts::{Act, Attach, Facts, Note};
use crate::walk::shader_notes;
use crate::{Change, Names, Reason, Reference, TokenSource, Tokens};

/// A note with the views it applies in and where the walk first writes it.
#[derive(Clone, Debug)]
struct Noted {
    note: Note,
    views: ViewSet,
    first: (u32, Box<str>, u32),
}

#[derive(Clone, Debug, Default)]
struct ViewShaders {
    notes: Vec<(PhysPath, Noted)>,
    /// Every logical path the view's shader walk looked up.
    looked_up: BTreeSet<String>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct State {
    tokens: BTreeMap<PhysPath, Tokens>,
    pub(crate) facts: BTreeMap<PhysPath, Arc<Facts>>,
    /// The notes each file's own visits write, and the `.mcmeta` notes its texture's visits write on it. Shared, so
    /// the state a pass leaves holds only the notes it changed.
    local: BTreeMap<PhysPath, Arc<[Noted]>>,
    shaders: Vec<ViewShaders>,
}

impl State {
    pub(crate) fn build(
        cx: &RunCx,
        pack: &PackInput,
        layout: &Layout,
        rules: &ClientRules,
        source: &dyn TokenSource,
    ) -> Self {
        let tokens: BTreeMap<PhysPath, Tokens> = pack
            .files
            .par_iter()
            .map(|(path, entry)| (path.clone(), source.tokens(entry, kind(layout, path))))
            .collect();
        let mut state = Self {
            tokens,
            ..Self::default()
        };
        let all: BTreeSet<PhysPath> = state.tokens.keys().cloned().collect();
        state.attach(cx, layout, rules, &all);
        state.local = local_notes(layout, &state.facts, &all);
        state.shaders = (0..layout.views.len())
            .into_par_iter()
            .map(|view| view_shaders(layout, rules, &state.facts, view))
            .collect();
        state
    }

    pub(crate) fn update(
        &mut self,
        cx: &RunCx,
        pack: &PackInput,
        layout: &Layout,
        rules: &ClientRules,
        change: &Change,
        source: &dyn TokenSource,
    ) -> Option<BTreeSet<PhysPath>> {
        for (old, new) in &change.moved {
            if let Some(tokens) = self.tokens.remove(old) {
                self.tokens.insert(new.clone(), tokens);
            }
            self.facts.remove(old);
            self.local.remove(old);
        }
        let retokenized: Vec<(PhysPath, Option<Tokens>)> = change
            .touched
            .par_iter()
            .map(|path| {
                let tokens = pack
                    .files
                    .get(path)
                    .map(|entry| source.tokens(entry, kind(layout, path)));
                (path.clone(), tokens)
            })
            .collect();
        for (path, tokens) in retokenized {
            match tokens {
                Some(tokens) => {
                    self.tokens.insert(path, tokens);
                }
                None => {
                    self.tokens.remove(&path);
                    self.facts.remove(&path);
                    self.local.remove(&path);
                }
            }
        }
        let layout_changed = change.layout_changed || self.shaders.len() != layout.views.len();
        let changed: BTreeSet<PhysPath> = if layout_changed {
            self.tokens.keys().cloned().collect()
        } else {
            change
                .touched
                .iter()
                .chain(change.moved.values())
                .filter(|path| self.tokens.contains_key(*path))
                .cloned()
                .collect()
        };
        self.attach(cx, layout, rules, &changed);
        if layout_changed {
            let all = self.tokens.keys().cloned().collect();
            self.local = local_notes(layout, &self.facts, &all);
            self.shaders = (0..layout.views.len())
                .into_par_iter()
                .map(|view| view_shaders(layout, rules, &self.facts, view))
                .collect();
            return None;
        }
        // A file's notes also depend on which views show it, which a file of the same logical path in another layer
        // decides, and a `.mcmeta`'s on its texture: those are attached again too.
        let mut logicals: BTreeSet<String> = BTreeSet::new();
        for path in change
            .touched
            .iter()
            .chain(change.moved.keys())
            .chain(change.moved.values())
        {
            if let Some(logical) = layout.split(path).1 {
                let logical = logical.as_str();
                logicals.insert(logical.to_owned());
                match logical.strip_suffix(".mcmeta") {
                    Some(texture) => logicals.insert(texture.to_owned()),
                    None => logicals.insert(format!("{logical}.mcmeta")),
                };
            }
        }
        let mut affected = changed;
        let mut text = String::new();
        for logical in &logicals {
            holders(layout, &self.tokens, logical, &mut text, &mut affected);
        }
        for path in &affected {
            self.local.remove(path);
        }
        self.local
            .extend(local_notes(layout, &self.facts, &affected));
        let shader_changed = logicals
            .iter()
            .any(|logical| FileKind::of(logical) == FileKind::Shader);
        let stale: Vec<usize> = (0..layout.views.len())
            .filter(|&view| {
                shader_changed
                    || self.shaders[view]
                        .looked_up
                        .iter()
                        .any(|logical| logicals.contains(logical))
            })
            .collect();
        let walked: Vec<(usize, ViewShaders)> = stale
            .into_par_iter()
            .map(|view| (view, view_shaders(layout, rules, &self.facts, view)))
            .collect();
        // The files whose references can differ: those noted again, and those a walk noted before or now.
        let mut dirty = affected;
        dirty.extend(change.touched.iter().cloned());
        dirty.extend(change.moved.keys().cloned());
        for (view, shaders) in walked {
            let before = std::mem::replace(&mut self.shaders[view], shaders);
            dirty.extend(before.notes.into_iter().map(|(path, _)| path));
            dirty.extend(
                self.shaders[view]
                    .notes
                    .iter()
                    .map(|(path, _)| path.clone()),
            );
        }
        Some(dirty)
    }

    fn attach(
        &mut self,
        cx: &RunCx,
        layout: &Layout,
        rules: &ClientRules,
        paths: &BTreeSet<PhysPath>,
    ) {
        let attach = Attach::new(rules, &cx.interner);
        let facts: Vec<(PhysPath, Arc<Facts>)> = paths
            .par_iter()
            .filter_map(|path| {
                let tokens = self.tokens.get(path)?;
                let logical = layout.split(path).1;
                let facts = attach.facts(
                    tokens,
                    kind(layout, path),
                    logical.as_ref().map(LogicalPath::as_str),
                );
                Some((path.clone(), Arc::new(facts)))
            })
            .collect();
        self.facts.extend(facts);
    }

    /// The index per file, by pointer, then in the order the walk notes them. With `only`, the references of those
    /// files alone.
    pub(crate) fn references(
        &self,
        only: Option<&BTreeSet<PhysPath>>,
    ) -> BTreeMap<PhysPath, Vec<Reference>> {
        let mut by_file: BTreeMap<&PhysPath, Vec<&Noted>> = BTreeMap::new();
        match only {
            None => {
                for (path, notes) in &self.local {
                    by_file.entry(path).or_default().extend(notes.iter());
                }
            }
            Some(only) => {
                for path in only {
                    if let Some(notes) = self.local.get(path) {
                        by_file.entry(path).or_default().extend(notes.iter());
                    }
                }
            }
        }
        for shaders in &self.shaders {
            for (path, noted) in shaders
                .notes
                .iter()
                .filter(|(path, _)| only.is_none_or(|only| only.contains(path)))
            {
                by_file.entry(path).or_default().push(noted);
            }
        }
        by_file
            .into_iter()
            .filter_map(|(path, notes)| {
                // The same note written in several views (or by several shader walks) is one reference.
                let mut merged: BTreeMap<(&Pointer, u32, u32), Vec<Noted>> = BTreeMap::new();
                for noted in notes {
                    let same = merged
                        .entry((
                            &noted.note.pointer,
                            noted.note.span.start,
                            noted.note.span.end,
                        ))
                        .or_default();
                    match same
                        .iter_mut()
                        .find(|other| other.note.names == noted.note.names)
                    {
                        Some(other) => {
                            other.views.union_with(&noted.views);
                            if noted.first < other.first {
                                other.first = noted.first.clone();
                                other.note.reason = noted.note.reason.clone();
                            }
                        }
                        None => same.push(noted.clone()),
                    }
                }
                let mut references: Vec<Noted> = merged.into_values().flatten().collect();
                references.sort_by(|a, b| {
                    a.note
                        .pointer
                        .cmp(&b.note.pointer)
                        .then_with(|| a.first.cmp(&b.first))
                });
                (!references.is_empty()).then(|| {
                    let references = references
                        .into_iter()
                        .map(|noted| Reference {
                            file: path.clone(),
                            pointer: noted.note.pointer,
                            span: noted.note.span,
                            names: noted.note.names,
                            reason: Reason(noted.note.reason),
                            views: noted.views,
                        })
                        .collect();
                    (path.clone(), references)
                })
            })
            .collect()
    }
}

/// Every path of `files` that splits to `logical`, the root's and each overlay's, added to `found`; `text` is the
/// buffer each candidate path is spelt in.
fn holders<V>(
    layout: &Layout,
    files: &BTreeMap<PhysPath, V>,
    logical: &str,
    text: &mut String,
    found: &mut BTreeSet<PhysPath>,
) {
    let overlays = layout
        .meta
        .overlays
        .iter()
        .map(|overlay| &*overlay.directory);
    for directory in std::iter::once(None).chain(overlays.map(Some)) {
        text.clear();
        if let Some(directory) = directory {
            text.push_str(directory);
            text.push('/');
        }
        text.push_str(logical);
        if let Some((path, _)) = files.get_key_value(text.as_str())
            && layout
                .split(path)
                .1
                .is_some_and(|split| split.as_str() == logical)
        {
            found.insert(path.clone());
        }
    }
}

fn kind(layout: &Layout, path: &PhysPath) -> FileKind {
    layout.kinds.get(path).copied().unwrap_or(FileKind::Unknown)
}

/// The notes of `paths` that do not come from a shader walk: each file's own, in every view that shows it, and the
/// `.mcmeta` note a texture's visit writes on its metadata. Each path is looked up in the views by its own logical
/// path, so the cost follows `paths`, not the pack.
fn local_notes(
    layout: &Layout,
    facts: &BTreeMap<PhysPath, Arc<Facts>>,
    paths: &BTreeSet<PhysPath>,
) -> BTreeMap<PhysPath, Arc<[Noted]>> {
    paths
        .par_iter()
        .filter_map(|physical| {
            let logical = layout.split(physical).1?;
            let views: ViewSet = layout
                .views
                .iter()
                .filter(|view| view.files.get(&logical) == Some(physical))
                .map(|view| view.id)
                .collect();
            let mut notes = Vec::new();
            if !views.is_empty() {
                let first_view = views.iter().next().unwrap_or(ViewId(0)).0;
                notes.extend(
                    facts[physical]
                        .acts
                        .iter()
                        .filter_map(|act| match act {
                            Act::Note(note)
                            | Act::Sprite { note, .. }
                            | Act::SoundEvent { note, .. } => Some(note),
                            Act::Ref { note, .. } | Act::SoundFile { note, .. } => note.as_ref(),
                            Act::Missing { .. } => None,
                        })
                        .enumerate()
                        .map(|(rank, note)| Noted {
                            note: note.clone(),
                            views: views.clone(),
                            first: (first_view, logical.as_str().into(), rank as u32),
                        }),
                );
            }
            // A `.mcmeta` beside the texture a view shows under the same name.
            let texture = logical
                .as_str()
                .strip_suffix(".mcmeta")
                .and_then(|texture| LogicalPath::new(texture).ok());
            if let Some(texture) = texture {
                let beside: ViewSet = layout
                    .views
                    .iter()
                    .filter(|view| {
                        views.contains(view.id)
                            && view.files.get(&texture).is_some_and(|shown| {
                                layout.kinds.get(shown) == Some(&FileKind::Texture)
                            })
                    })
                    .map(|view| view.id)
                    .collect();
                let first = beside.iter().next();
                if let Some(first) = first {
                    notes.push(Noted {
                        note: Note {
                            pointer: Pointer::new(""),
                            span: 0..0,
                            names: Names::Beside {
                                logical: texture.clone(),
                            },
                            reason: "texture.mcmeta".into(),
                        },
                        views: beside,
                        first: (first.0, texture.as_str().into(), 0),
                    });
                }
            }
            (!notes.is_empty()).then(|| (physical.clone(), Arc::from(notes)))
        })
        .collect()
}

fn view_shaders(
    layout: &Layout,
    rules: &ClientRules,
    facts: &BTreeMap<PhysPath, Arc<Facts>>,
    view: usize,
) -> ViewShaders {
    let view = &layout.views[view];
    let (notes, looked_up) = shader_notes(rules, &layout.kinds, facts, view);
    ViewShaders {
        notes: notes
            .into_iter()
            .map(|(physical, note, visiting, rank)| {
                let noted = Noted {
                    note,
                    views: [view.id].into_iter().collect(),
                    first: (view.id.0, visiting.into(), rank),
                };
                (physical.clone(), noted)
            })
            .collect(),
        looked_up,
    }
}
