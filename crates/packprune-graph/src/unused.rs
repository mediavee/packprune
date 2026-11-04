//! The usage level: the item declarations the selectors pick, which of them nothing uses, and
//! the item definitions only those declare. A declaration is named `<source>:<location>`, as a selector would match
//! it. What keeps a picked declaration is a usage that names one of its item definitions and roots: a use, or the
//! usage of any declaration that is not itself unused.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use camino::{Utf8Component, Utf8Path, Utf8PathBuf};
use packprune_pack::PhysPath;

use crate::{
    FileEntry, KeptItem, Refused, SharedFile, SourceUsages, Status, UnusedFolder, UnusedItem,
    UnusedSelection, UnusedSelector, Usage, UsageKind, UsageRef,
};

pub(crate) const LIVE_DATA: &str = "No world, player or database data is read: an item that exists only there looks \
unused, and once dropped it shows the missing model until the next build, its data untouched. Keep it with a \
keep-list entry or a source that names it.";

/// A usage as resolved against the pack, without atlases: item definitions never go through one.
pub(crate) struct Resolved<'a> {
    pub source: &'a str,
    pub usage: &'a Usage,
    pub files: Vec<PhysPath>,
    pub reference: UsageRef,
}

impl Resolved<'_> {
    fn owner(&self) -> Option<String> {
        self.usage
            .declares
            .as_ref()
            .map(|declaration| owner(self.source, declaration))
    }
}

#[derive(Default)]
pub(crate) struct UnusedItems {
    selectors: Vec<UnusedSelector>,
    searched: Vec<String>,
    warnings: Vec<String>,
    /// Every picked declaration with the first selector that picks it, by index.
    picked: BTreeMap<String, usize>,
    /// The item definitions a picked declaration declares that only a use would root.
    definitions: BTreeMap<String, BTreeSet<PhysPath>>,
    /// What keeps each picked declaration; empty for an unused one.
    used_by: BTreeMap<String, Vec<UsageRef>>,
    unused_definitions: BTreeSet<PhysPath>,
}

pub(crate) fn owner(source: &str, declaration: &str) -> String {
    format!("{source}:{declaration}")
}

impl UnusedItems {
    pub(crate) fn new(
        selectors: &[UnusedSelector],
        sources: &[SourceUsages],
        usages: &[Resolved<'_>],
        droppable: impl Fn(&PhysPath) -> bool,
        item_definition: impl Fn(&PhysPath) -> bool,
    ) -> Result<Self, Refused> {
        let searched: BTreeSet<String> =
            sources.iter().map(|source| source.source.clone()).collect();
        let mut items = Self {
            selectors: selectors.to_vec(),
            searched: searched.into_iter().collect(),
            ..Self::default()
        };
        if selectors.is_empty() {
            return Ok(items);
        }
        for selector in selectors {
            if !items.searched.contains(&selector.source) {
                return Err(Refused(format!(
                    "unused-item selector {selector}: no source {} is given",
                    selector.source
                )));
            }
        }
        refuse_without_uses(sources, usages, &item_definition)?;
        items.warn_uses_inside_declaring_inputs(sources);
        for resolved in usages {
            let Some(declaration) = &resolved.usage.declares else {
                continue;
            };
            let Some(selector) = selectors
                .iter()
                .position(|selector| selector.matches(resolved.source, declaration))
            else {
                continue;
            };
            let owner = owner(resolved.source, declaration);
            items.picked.entry(owner.clone()).or_insert(selector);
            if resolved.usage.kind == UsageKind::ItemDefinition {
                for file in resolved.files.iter().filter(|file| droppable(file)) {
                    items
                        .definitions
                        .entry(owner.clone())
                        .or_default()
                        .insert(file.clone());
                }
            }
        }
        let picking: BTreeSet<usize> = items.picked.values().copied().collect();
        for (index, selector) in selectors.iter().enumerate() {
            if !picking.contains(&index) {
                items.warnings.push(format!(
                    "unused-item selector {selector} picks no declaration"
                ));
            }
        }
        items.kept_by_what_roots(usages);
        for (owner, files) in &items.definitions {
            if items.used_by[owner].is_empty() {
                items.unused_definitions.extend(files.iter().cloned());
            }
        }
        Ok(items)
    }

    /// Starts with every candidate unused, then keeps those a rooting usage names, until none changes: keeping one
    /// makes its own usages root, which can keep another.
    fn kept_by_what_roots(&mut self, usages: &[Resolved<'_>]) {
        for owner in self.definitions.keys() {
            self.used_by.insert(owner.clone(), Vec::new());
        }
        let declared: BTreeSet<&PhysPath> = self.definitions.values().flatten().collect();
        let mut naming: HashMap<&PhysPath, Vec<&Resolved<'_>>> = HashMap::new();
        for resolved in usages {
            let mut files: Vec<&PhysPath> = resolved
                .files
                .iter()
                .filter(|file| declared.contains(file))
                .collect();
            files.dedup();
            let mut seen = BTreeSet::new();
            for file in files {
                if seen.insert(file) {
                    naming.entry(file).or_default().push(resolved);
                }
            }
        }
        let mut changed = true;
        while changed {
            changed = false;
            let unused: BTreeSet<String> = self
                .used_by
                .iter()
                .filter(|(_, refs)| refs.is_empty())
                .map(|(owner, _)| owner.clone())
                .collect();
            for owner in &unused {
                let mut refs: Vec<UsageRef> = Vec::new();
                for file in &self.definitions[owner] {
                    for resolved in naming.get(file).into_iter().flatten() {
                        if resolved
                            .owner()
                            .is_none_or(|other| !unused.contains(&other))
                            && !refs.contains(&resolved.reference)
                        {
                            refs.push(resolved.reference.clone());
                        }
                    }
                }
                if !refs.is_empty() {
                    self.used_by
                        .get_mut(owner)
                        .expect("a candidate")
                        .extend(refs);
                    changed = true;
                }
            }
        }
    }

    /// A use read inside a generator's own folder is that generator's declaration seen again: it keeps everything.
    fn warn_uses_inside_declaring_inputs(&mut self, sources: &[SourceUsages]) {
        let declaring: Vec<usize> = (0..sources.len())
            .filter(|&index| {
                sources[index]
                    .usages
                    .iter()
                    .any(|usage| usage.declares.is_some())
            })
            .collect();
        for (index, source) in sources.iter().enumerate() {
            let root: &Utf8Path = if source.path.is_dir() {
                &source.path
            } else {
                source.path.parent().unwrap_or(&source.path)
            };
            for &generator_index in &declaring {
                let generator = &sources[generator_index];
                if generator_index == index || !generator.path.starts_with(root) {
                    continue;
                }
                let inside = source
                    .usages
                    .iter()
                    .filter(|usage| {
                        usage.declares.is_none()
                            && normalize(&root.join(UnusedSelector::file(&usage.location)))
                                .starts_with(&generator.path)
                    })
                    .count();
                if inside > 0 {
                    let folder = generator
                        .path
                        .strip_prefix(root)
                        .map_or_else(|_| generator.path.to_string(), |rest| rest.to_string());
                    self.warnings.push(format!(
                        "{}: {inside} uses come from inside the input of {} ({folder}), which declares its items: \
                         each keeps the item it names; exclude {folder}/** from {}",
                        source.source, generator.source, source.source
                    ));
                }
            }
        }
    }

    /// The declaration of `usage` when it is picked and unused: its usages root nothing.
    pub(crate) fn unused_owner(&self, source: &str, usage: &Usage) -> Option<String> {
        let owner = owner(source, usage.declares.as_ref()?);
        self.used_by
            .get(&owner)
            .is_some_and(Vec::is_empty)
            .then_some(owner)
    }

    /// An item definition only unused declarations declare.
    pub(crate) fn unused_definition(&self, physical: &PhysPath) -> bool {
        self.unused_definitions.contains(physical)
    }

    pub(crate) fn warnings(&self) -> Vec<String> {
        let mut all = self.warnings.clone();
        if !self.selectors.is_empty() {
            all.push(LIVE_DATA.to_owned());
        }
        all
    }

    /// One section per selector. `owners`: for each file reached only from unused declarations, those declarations.
    pub(crate) fn selections(
        &self,
        files: &BTreeMap<PhysPath, FileEntry>,
        owners: &BTreeMap<PhysPath, BTreeSet<String>>,
    ) -> Vec<UnusedSelection> {
        let mut exclusive: BTreeMap<&str, Vec<&PhysPath>> = BTreeMap::new();
        for (path, declarations) in owners {
            if declarations.len() == 1 && files[path].status == Status::Unused {
                let owner = declarations.first().expect("one declaration");
                exclusive.entry(owner).or_default().push(path);
            }
        }
        let mut selections = Vec::new();
        for (index, selector) in self.selectors.iter().enumerate() {
            let mut unused = Vec::new();
            let mut kept = Vec::new();
            let mut groups: BTreeMap<String, (usize, usize, u64)> = BTreeMap::new();
            for (owner, declared) in &self.definitions {
                if self.picked[owner] != index {
                    continue;
                }
                let refs = &self.used_by[owner];
                if refs.is_empty() {
                    let own: Vec<PhysPath> = exclusive
                        .get(owner.as_str())
                        .map(|paths| paths.iter().map(|path| (*path).clone()).collect())
                        .unwrap_or_default();
                    let bytes = own.iter().map(|path| files[path].bytes).sum();
                    let group = groups.entry(folder(owner)).or_default();
                    group.0 += 1;
                    group.1 += own.len();
                    group.2 += bytes;
                    unused.push(UnusedItem {
                        declaration: owner.clone(),
                        definitions: declared.iter().cloned().collect(),
                        files: own,
                        bytes,
                    });
                } else {
                    kept.push(KeptItem {
                        declaration: owner.clone(),
                        definitions: declared.iter().cloned().collect(),
                        used_by: refs.clone(),
                    });
                }
            }
            let shared = owners
                .iter()
                .filter(|(path, declarations)| {
                    declarations.len() > 1
                        && files[*path].status == Status::Unused
                        && declarations
                            .iter()
                            .any(|owner| self.picked.get(owner) == Some(&index))
                })
                .map(|(path, declarations)| SharedFile {
                    path: path.clone(),
                    bytes: files[path].bytes,
                    declarations: declarations.iter().cloned().collect(),
                })
                .collect();
            selections.push(UnusedSelection {
                selector: selector.to_string(),
                searched: self.searched.clone(),
                folders: groups
                    .into_iter()
                    .map(|(folder, (items, files, bytes))| UnusedFolder {
                        folder,
                        items,
                        files,
                        bytes,
                    })
                    .collect(),
                unused,
                kept,
                shared,
            });
        }
        selections
    }
}

fn refuse_without_uses(
    sources: &[SourceUsages],
    usages: &[Resolved<'_>],
    item_definition: &impl Fn(&PhysPath) -> bool,
) -> Result<(), Refused> {
    let uses = usages.iter().any(|resolved| {
        resolved.usage.declares.is_none() && resolved.files.iter().any(item_definition)
    });
    if uses {
        return Ok(());
    }
    let mut names: Vec<&str> = Vec::new();
    for source in sources {
        if !names.contains(&source.source.as_str()) {
            names.push(&source.source);
        }
    }
    Err(Refused(format!(
        "unused-item selectors need a source that uses items: no usage of {} that is not a declaration names an item \
         definition of the pack, so nothing could keep an item",
        names.join(", ")
    )))
}

/// The folder of a declaration's file, with its source: `nexo:items/example/`.
fn folder(owner: &str) -> String {
    let colon = owner.find(':').map_or(0, |colon| colon + 1);
    let file = UnusedSelector::file(&owner[colon..]);
    let slash = file.rfind('/').map_or(0, |slash| slash + 1);
    format!("{}{}", &owner[..colon], &file[..slash])
}

/// `Path.normalize`: `.` dropped, `..` folded into its parent where there is one.
fn normalize(path: &Utf8Path) -> Utf8PathBuf {
    let mut out = Utf8PathBuf::new();
    for component in path.components() {
        match component {
            Utf8Component::CurDir => {}
            Utf8Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Utf8Component::Normal(_))) {
                    out.pop();
                } else {
                    out.push("..");
                }
            }
            other => out.push(other.as_str()),
        }
    }
    out
}
