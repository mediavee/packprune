//! The walk of one view: from the roots, follows what each file's [`Facts`] name, resolving targets in the
//! view, the vanilla layer and the view's atlases, and records what it reaches and what it finds broken. The shader
//! import walk also builds the index's shader references ([`shader_notes`]), since what an import names depends on
//! the root shader it is compiled for.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use packprune_pack::kinds::ResourcePath;
use packprune_pack::meta::BlockFilter;
use packprune_pack::{
    ClientRules, FileKind, Interner, JavaPattern, LogicalPath, PatternError, PhysPath, Pointer,
    VanillaIndex, View,
};

use crate::Names;
use crate::atlases::ViewAtlases;
use crate::facts::{Act, Facts, Import, Note};

/// The vanilla client below the pack, minus what the pack's `filter.block` hides.
pub struct VanillaLayer<'a> {
    pub index: &'a VanillaIndex,
    /// Namespace and path patterns of each `filter.block` entry; `None` matches everything.
    filters: Vec<(Option<JavaPattern>, Option<JavaPattern>)>,
    /// The folders of vanilla's shaders, `(namespace, path/)`, sorted by their `namespace:path` id.
    pub shader_folders: Vec<(Box<str>, Box<str>)>,
}

impl<'a> VanillaLayer<'a> {
    /// A `filter.block` pattern that does not compile is refused.
    pub fn new(index: &'a VanillaIndex, filters: &[BlockFilter]) -> Result<Self, PatternError> {
        let compile =
            |pattern: &Option<String>| pattern.as_deref().map(JavaPattern::new).transpose();
        let filters = filters
            .iter()
            .map(|filter| Ok((compile(&filter.namespace)?, compile(&filter.path)?)))
            .collect::<Result<_, PatternError>>()?;
        let folders: BTreeSet<String> = index
            .names
            .iter()
            .filter(|name| FileKind::of(name) == FileKind::Shader)
            .filter_map(|name| folder_of(name))
            .map(|(ns, path)| format!("{ns}:{path}"))
            .collect();
        Ok(Self {
            index,
            filters,
            shader_folders: folders
                .iter()
                .filter_map(|folder| folder.split_once(':'))
                .map(|(ns, path)| (ns.into(), path.into()))
                .collect(),
        })
    }

    pub fn has(&self, logical: &str) -> bool {
        self.index.has(logical) && !self.hidden(logical)
    }

    fn hidden(&self, logical: &str) -> bool {
        if self.filters.is_empty() {
            return false;
        }
        let Some(path) = ResourcePath::of(logical) else {
            return false;
        };
        let id_path = if path.folder.is_empty() {
            path.rest.to_owned()
        } else {
            format!("{}/{}", path.folder, path.rest)
        };
        let test = |pattern: &Option<JavaPattern>, text: &str| {
            pattern.as_ref().is_none_or(|pattern| pattern.find(text))
        };
        self.filters
            .iter()
            .any(|(ns, path_pattern)| test(ns, path.namespace) && test(path_pattern, &id_path))
    }
}

/// What every view's walk shares.
pub(crate) struct Shared<'a> {
    pub interner: &'a Interner,
    pub rules: &'a ClientRules,
    pub facts: &'a BTreeMap<PhysPath, Arc<Facts>>,
    pub kinds: &'a BTreeMap<PhysPath, FileKind>,
    pub vanilla: &'a VanillaLayer<'a>,
}

/// A view's files by logical path, with the key the view stores.
pub(crate) type Files<'a> = HashMap<&'a str, (&'a str, &'a PhysPath)>;

pub(crate) fn files(view: &View) -> Files<'_> {
    view.files
        .iter()
        .map(|(logical, physical)| (logical.as_str(), (logical.as_str(), physical)))
        .collect()
}

/// A sprite not stitched: its source, the sprite, the atlases looked in, and the file of its name.
pub(crate) type NotStitchedAt<'a> = (&'a PhysPath, Box<str>, Box<str>, Box<str>);

/// A shader reference with the logical path whose visit wrote it and its rank there.
pub(crate) type ShaderNote<'a> = (&'a PhysPath, Note, &'a str, u32);

/// What one view's walk found. Findings carry their source and are tagged with the view when merged.
#[derive(Default)]
pub(crate) struct Found<'a> {
    pub reached: Vec<&'a PhysPath>,
    pub edges: Vec<(&'a PhysPath, &'a PhysPath, Box<str>)>,
    pub missing: Vec<(&'a PhysPath, Box<str>, Box<str>)>,
    pub not_stitched: Vec<NotStitchedAt<'a>>,
    /// Files reached from an unused declaration, with that declaration.
    pub unused: Vec<(&'a PhysPath, &'a str)>,
    /// Files whose JSON the walk asked for: an unreadable one among them is listed invalid.
    pub read: Vec<&'a PhysPath>,
}

pub(crate) struct Walker<'a, 'r> {
    shared: &'r Shared<'a>,
    files: &'r Files<'a>,
    verified: bool,
    atlases: &'r ViewAtlases,
    rooted: &'r dyn Fn(&PhysPath) -> bool,
    queue: VecDeque<&'a str>,
    seen: HashSet<&'a str>,
    unused_owner: Option<&'a str>,
    unused_seen: HashSet<&'a str>,
    compiled: HashSet<(&'a PhysPath, usize, String)>,
    folders: Vec<(Box<str>, Box<str>)>,
    pub found: Found<'a>,
}

impl<'a, 'r> Walker<'a, 'r> {
    pub(crate) fn new(
        shared: &'r Shared<'a>,
        files: &'r Files<'a>,
        verified: bool,
        atlases: &'r ViewAtlases,
        rooted: &'r dyn Fn(&PhysPath) -> bool,
    ) -> Self {
        Self {
            shared,
            files,
            verified,
            atlases,
            rooted,
            queue: VecDeque::new(),
            seen: HashSet::new(),
            unused_owner: None,
            unused_seen: HashSet::new(),
            compiled: HashSet::new(),
            folders: Vec::new(),
            found: Found::default(),
        }
    }

    pub(crate) fn enqueue(&mut self, logical: &'a str) {
        let fresh = match self.unused_owner {
            None => self.seen.insert(logical),
            Some(_) => !self.seen.contains(logical) && self.unused_seen.insert(logical),
        };
        if fresh {
            self.queue.push_back(logical);
        }
    }

    pub(crate) fn run(&mut self) {
        while let Some(logical) = self.queue.pop_front() {
            self.visit(logical);
        }
    }

    /// After [`Self::run`]: walks from what only the unused declaration `owner` names, through files no root reached,
    /// so every file comes out tagged with each unused declaration that reaches it.
    pub(crate) fn run_unused(&mut self, owner: &'a str, roots: &[&'a str]) {
        self.unused_owner = Some(owner);
        self.unused_seen.clear();
        for &root in roots {
            self.enqueue(root);
        }
        self.run();
        self.unused_owner = None;
    }

    fn visit(&mut self, logical: &'a str) {
        let (_, physical) = self.files[logical];
        match self.unused_owner {
            None => self.found.reached.push(physical),
            Some(owner) => self.found.unused.push((physical, owner)),
        }
        let facts = &self.shared.facts[physical];
        match self.shared.kinds[physical] {
            FileKind::Texture => {
                let meta = format!("{logical}.mcmeta");
                if self.files.contains_key(&*meta) {
                    self.link(physical, &meta, "texture.mcmeta");
                }
            }
            FileKind::Model
            | FileKind::ItemDefinition
            | FileKind::Blockstate
            | FileKind::Equipment
            | FileKind::Font
            | FileKind::SoundsJson
            | FileKind::PostEffect
            | FileKind::Particle
            | FileKind::WaypointStyle => {
                self.found.read.push(physical);
                for act in &facts.acts {
                    self.act(physical, act);
                }
            }
            FileKind::Atlas => {
                self.found.read.push(physical);
                if let Some(atlas) = &facts.atlas {
                    for id in &atlas.invalid {
                        self.missing(physical, id, "atlas (invalid id)");
                    }
                }
                let atlases = self.atlases;
                for input in atlases.named(physical) {
                    self.reference(physical, &input.logical, input.reason);
                }
            }
            FileKind::Shader => {
                if let Some(folder) = folder_of(logical) {
                    let root = self.folder_index(&folder.0, &folder.1);
                    self.walk_imports(physical, root, String::new(), &mut HashSet::new());
                }
            }
            // An include the client compiles only through a shader; one kept for itself (it overrides vanilla, or a
            // source names it) is walked as each vanilla shader folder would import it.
            FileKind::ShaderInclude if (self.rooted)(physical) => {
                for (ns, path) in &self.shared.vanilla.shader_folders {
                    let root = self.folder_index(ns, path);
                    self.walk_imports(physical, root, String::new(), &mut HashSet::new());
                }
            }
            _ => {}
        }
    }

    fn act(&mut self, source: &'a PhysPath, act: &Act) {
        match act {
            Act::Note(_) => {}
            Act::Missing {
                target,
                reason,
                unverified_only,
            } => {
                if !(*unverified_only && self.verified) {
                    self.missing(source, target, reason);
                }
            }
            Act::Ref {
                target,
                reason,
                quiet_verified,
                ..
            } => {
                if *quiet_verified && self.verified {
                    self.link(source, target, reason);
                } else {
                    self.reference(source, target, reason);
                }
            }
            Act::Sprite {
                sprite,
                shown,
                atlases,
                reason,
                missing_sprite,
                ..
            } => {
                if *missing_sprite {
                    return;
                }
                let view_atlases = self.atlases;
                let mut found = false;
                for atlas in atlases.iter() {
                    for input in view_atlases.lookup(atlas, *sprite) {
                        let reason = if input.reason == "atlas.directory" {
                            reason.to_string()
                        } else {
                            format!("{reason}>{}", input.reason)
                        };
                        found |= self.link(source, &input.logical, &reason);
                    }
                }
                if found {
                    return;
                }
                let interner = self.shared.interner;
                let file = format!(
                    "assets/{}/textures/{}.png",
                    interner.resolve(sprite.ns),
                    interner.resolve(sprite.path)
                );
                let shown_file = self
                    .files
                    .get(&*file)
                    .map(|(_, physical)| physical.as_str());
                if shown_file.is_some() || self.shared.vanilla.has(&file) {
                    let atlases = atlases.join(" ");
                    let file = shown_file.unwrap_or(&file).into();
                    self.found
                        .not_stitched
                        .push((source, shown.clone(), atlases.into(), file));
                } else {
                    self.missing(source, shown, reason);
                }
            }
            Act::SoundFile {
                file, minecraft, ..
            } => {
                let assumed = !self.shared.vanilla.index.sound_files && *minecraft;
                if !self.link(source, file, "sound.file") && !assumed {
                    self.missing(source, file, "sound.file");
                }
            }
            Act::SoundEvent {
                namespace,
                path,
                shown,
                ..
            } => {
                let file = format!("assets/{namespace}/sounds.json");
                if let Some(&(_, physical)) = self.files.get(&*file) {
                    self.found.read.push(physical);
                    let defines = self.shared.facts[physical]
                        .events
                        .as_ref()
                        .is_some_and(|events| events.contains(&**path));
                    if defines {
                        self.link(source, &file, "sound.event");
                        return;
                    }
                }
                let events = self.shared.vanilla.index.sound_events.as_ref();
                let minecraft = &**namespace == "minecraft";
                let vanilla_event =
                    minecraft && events.is_some_and(|events| events.contains(&**path));
                let assumed = events.is_none() && minecraft;
                if !vanilla_event && !assumed {
                    self.missing(source, shown, "sound.event");
                }
            }
        }
    }

    fn folder_index(&mut self, ns: &str, path: &str) -> usize {
        match self
            .folders
            .iter()
            .position(|(n, p)| **n == *ns && **p == *path)
        {
            Some(index) => index,
            None => {
                self.folders.push((ns.into(), path.into()));
                self.folders.len() - 1
            }
        }
    }

    /// The imports of a shader, as `ShaderManager.createPreprocessor` and `GlslPreprocessor.processImports` resolve
    /// them.
    fn walk_imports(
        &mut self,
        source: &'a PhysPath,
        root: usize,
        parent: String,
        imported: &mut HashSet<String>,
    ) {
        if !self.compiled.insert((source, root, parent.clone())) {
            return;
        }
        let facts = Arc::clone(&self.shared.facts[source]);
        for import in &facts.imports {
            let (ns, folder) = self.folders[root].clone();
            let (import_path, target) =
                resolve_import(self.shared.rules, &ns, &folder, &parent, import);
            let Some(target) = target else {
                self.missing(source, &import_path, "shader.import (invalid path)");
                continue;
            };
            self.reference(source, &target, "shader.import");
            if let Some(&(_, physical)) = self.files.get(&*target)
                && imported.insert(target)
            {
                let parent = if import.quoted {
                    folder_path(&import_path).to_owned()
                } else {
                    String::new()
                };
                self.walk_imports(physical, root, parent, imported);
            }
        }
    }

    fn reference(&mut self, source: &'a PhysPath, logical: &str, reason: &str) {
        if !self.link(source, logical, reason) {
            self.missing(source, logical, reason);
        }
    }

    /// True when the target exists in this view or in vanilla; only pack files are followed.
    fn link(&mut self, source: &'a PhysPath, logical: &str, reason: &str) -> bool {
        match self.files.get(logical) {
            Some(&(key, physical)) => {
                self.found.edges.push((source, physical, reason.into()));
                self.enqueue(key);
                true
            }
            None => self.shared.vanilla.has(logical),
        }
    }

    fn missing(&mut self, source: &'a PhysPath, target: &str, reason: &str) {
        self.found
            .missing
            .push((source, target.into(), reason.into()));
    }
}

/// The import path as the chain writes it, and the logical path it names (`None`: it climbs above the root).
fn resolve_import(
    rules: &ClientRules,
    ns: &str,
    folder: &str,
    parent: &str,
    import: &Import,
) -> (String, Option<String>) {
    let import_path = format!("{parent}{}", import.raw);
    let target = if import.quoted {
        normalize(&format!("{folder}{import_path}")).map(|path| format!("assets/{ns}/{path}"))
    } else {
        crate::json::id_parts(&import_path).map(|(ns, path)| rules.shader_includes.file(ns, path))
    };
    (import_path, target)
}

/// The shader references of one view: every shader of the view, in logical order,
/// compiled with its imports. Each note comes with the logical path whose visit wrote it and its rank there, and the
/// walk returns every logical path it looked up, so `update` knows when to walk again.
pub(crate) fn shader_notes<'a>(
    rules: &'a ClientRules,
    kinds: &'a BTreeMap<PhysPath, FileKind>,
    facts: &'a BTreeMap<PhysPath, Arc<Facts>>,
    view: &'a View,
) -> (Vec<ShaderNote<'a>>, BTreeSet<String>) {
    let files = files(view);
    let mut notes = Vec::new();
    let mut looked_up = BTreeSet::new();
    let mut compiled = HashSet::new();
    for (logical, physical) in &view.files {
        if kinds[physical] != FileKind::Shader {
            continue;
        }
        let Some((ns, folder)) = folder_of(logical.as_str()) else {
            continue;
        };
        let mut walk = IndexWalk {
            rules,
            facts,
            files: &files,
            visiting: logical.as_str(),
            ns: &ns,
            folder: &folder,
            compiled: &mut compiled,
            notes: &mut notes,
            looked_up: &mut looked_up,
            rank: 0,
        };
        walk.walk(physical, String::new(), &mut HashSet::new());
    }
    (notes, looked_up)
}

struct IndexWalk<'w, 'a> {
    rules: &'a ClientRules,
    facts: &'a BTreeMap<PhysPath, Arc<Facts>>,
    files: &'w Files<'a>,
    visiting: &'a str,
    ns: &'w str,
    folder: &'w str,
    compiled: &'w mut HashSet<(&'a PhysPath, String, String, String)>,
    notes: &'w mut Vec<ShaderNote<'a>>,
    looked_up: &'w mut BTreeSet<String>,
    rank: u32,
}

impl<'a> IndexWalk<'_, 'a> {
    fn walk(&mut self, source: &'a PhysPath, parent: String, imported: &mut HashSet<String>) {
        if !self.compiled.insert((
            source,
            self.ns.to_owned(),
            self.folder.to_owned(),
            parent.clone(),
        )) {
            return;
        }
        let facts = Arc::clone(&self.facts[source]);
        for import in &facts.imports {
            let (import_path, target) =
                resolve_import(self.rules, self.ns, self.folder, &parent, import);
            let Some(target) = target else {
                continue;
            };
            if let Ok(logical) = LogicalPath::new(&target) {
                let note = Note {
                    pointer: Pointer::new(""),
                    span: import.span.clone(),
                    names: Names::Text { logical },
                    reason: "shader.import".into(),
                };
                self.notes.push((source, note, self.visiting, self.rank));
                self.rank += 1;
            }
            self.looked_up.insert(target.clone());
            if let Some(&(_, physical)) = self.files.get(&*target)
                && imported.insert(target)
            {
                let parent = if import.quoted {
                    folder_path(&import_path).to_owned()
                } else {
                    String::new()
                };
                self.walk(physical, parent, imported);
            }
        }
    }
}

/// The namespace of a logical path and the folder part of its path in it, slash included.
pub(crate) fn folder_of(logical: &str) -> Option<(Box<str>, String)> {
    let path = ResourcePath::of(logical)?;
    let id_path = if path.folder.is_empty() {
        path.rest.to_owned()
    } else {
        format!("{}/{}", path.folder, path.rest)
    };
    Some((path.namespace.into(), folder_path(&id_path).to_owned()))
}

/// `FilenameUtils.getFullPath`: the folder part, with its trailing slash.
fn folder_path(path: &str) -> &str {
    &path[..path.rfind('/').map_or(0, |slash| slash + 1)]
}

/// `FilenameUtils.normalize` as the client uses it: `None` when `..` climbs above the root.
fn normalize(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            ".." => {
                parts.pop()?;
            }
            "." | "" => {}
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_drops_dots_and_refuses_to_climb_above_the_root() {
        assert_eq!(
            normalize("shaders/post/../include/./a.glsl").as_deref(),
            Some("shaders/include/a.glsl")
        );
        assert_eq!(normalize("shaders/../../a.glsl"), None);
        assert_eq!(folder_path("shaders/post/glow.fsh"), "shaders/post/");
        assert_eq!(folder_path("a.glsl"), "");
    }
}
