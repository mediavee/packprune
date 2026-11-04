//! Resolves the usages of every source to the pack files they name, the one place that knows what
//! each usage kind points at, and tells for each usage whether it matched pack files, only vanilla assets, or
//! nothing. The usages of an unused item declaration ([`crate::unused`]) root nothing.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use packprune_pack::kinds::ResourcePath;
use packprune_pack::{ClientRules, FileKind, Interner, Layout, PhysPath, ResourceId};

use crate::atlases::{SpriteInput, ViewAtlases};
use crate::facts::Facts;
use crate::json::id_parts;
use crate::unused::{Resolved, UnusedItems};
use crate::walk::{Files, VanillaLayer};
use crate::{
    Refused, SourceInfo, SourceRoot, SourceUsages, UnusedSelector, Usage, UsageKind, UsageRef,
};

pub(crate) use packprune_pack::rules::Glob;

impl UsageKind {
    /// The kind in snake case.
    pub fn id(self) -> &'static str {
        match self {
            UsageKind::ItemDefinition => "item_definition",
            UsageKind::Model => "model",
            UsageKind::Sprite => "sprite",
            UsageKind::Font => "font",
            UsageKind::SoundEvent => "sound_event",
            UsageKind::Equipment => "equipment",
            UsageKind::TooltipStyle => "tooltip_style",
            UsageKind::Path => "path",
            UsageKind::Namespace => "namespace",
        }
    }
}

/// One usage with the source that named it; `index` orders usages by source, then as the source listed them.
pub(crate) struct Named<'a> {
    pub source: &'a str,
    pub usage: &'a Usage,
    glob: Glob,
    atlas: Option<String>,
}

impl Named<'_> {
    fn reason(&self) -> String {
        match &self.usage.reason {
            Some(reason) => format!("{} {}: {reason}", self.source, self.usage.location),
            None => format!("{} {}", self.source, self.usage.location),
        }
    }

    pub(crate) fn reference(&self) -> UsageRef {
        UsageRef {
            source: self.source.to_owned(),
            location: self.usage.location.clone(),
            reason: self.usage.reason.clone().unwrap_or_default(),
            kind: self.usage.kind.id().to_owned(),
            id: self.usage.id.0.to_string(),
            atlas: self.atlas.clone().unwrap_or_default(),
        }
    }

    /// A sprite with its atlas and a tooltip style name atlas sprites, which only a view's atlases resolve.
    fn through_atlases(&self) -> bool {
        self.usage.kind == UsageKind::TooltipStyle || self.usage.atlas.is_some()
    }

    /// Without a wildcard the glob matches its own text only, so a lookup finds what a scan would.
    fn exact(&self) -> bool {
        !self.usage.id.0.contains(['*', '?'])
    }

    /// The id an exact usage names, if it is written as the client writes ids.
    fn exact_id(&self) -> Option<(&str, &str)> {
        let id = &*self.usage.id.0;
        id_parts(id).filter(|(ns, path)| {
            id.len() == ns.len() + 1 + path.len()
                && id.starts_with(ns)
                && id.as_bytes()[ns.len()] == b':'
        })
    }
}

pub(crate) struct UsageRoots<'a> {
    pub named: Vec<Named<'a>>,
    sources: &'a [SourceUsages],
    rules: &'a ClientRules,
    interner: &'a Interner,
    vanilla: &'a VanillaLayer<'a>,
    /// Physical file to the indices of the usages that name it.
    roots: BTreeMap<PhysPath, BTreeSet<usize>>,
    /// Usages that name a vanilla asset; they count as vanilla only when they name no pack file.
    in_vanilla: Vec<bool>,
    unused_owners: HashMap<usize, String>,
    pub unused: UnusedItems,
    /// `sounds.json` files whose JSON the sound event lookup asked for.
    pub read: Vec<PhysPath>,
}

pub(crate) struct Context<'a> {
    pub layout: &'a Layout,
    pub facts: &'a BTreeMap<PhysPath, Arc<Facts>>,
    pub vanilla: &'a VanillaLayer<'a>,
    pub rules: &'a ClientRules,
    pub interner: &'a Interner,
}

impl<'a> UsageRoots<'a> {
    pub(crate) fn new(
        cx: &Context<'a>,
        sources: &'a [SourceUsages],
        selectors: &[UnusedSelector],
        droppable: impl Fn(&PhysPath) -> bool,
    ) -> Result<Self, Refused> {
        let mut named = Vec::new();
        for source in sources {
            for usage in &source.usages {
                named.push(Named {
                    source: &source.source,
                    usage,
                    glob: if usage.kind == UsageKind::Path {
                        Glob::path(&usage.id.0)
                    } else {
                        Glob::id(&usage.id.0)
                    },
                    atlas: usage
                        .atlas
                        .map(|atlas| atlas.display(cx.interner).to_string()),
                });
            }
        }
        let mut roots = Self {
            in_vanilla: vec![false; named.len()],
            named,
            sources,
            rules: cx.rules,
            interner: cx.interner,
            vanilla: cx.vanilla,
            roots: BTreeMap::new(),
            unused_owners: HashMap::new(),
            unused: UnusedItems::default(),
            read: Vec::new(),
        };
        // Every layer is indexed, so a copy no view shows (shadowed, undeclared overlay) is kept too.
        let mut pack: HashMap<UsageKind, HashMap<String, Vec<PhysPath>>> = HashMap::new();
        let mut vanilla: HashMap<UsageKind, HashMap<String, Vec<String>>> = HashMap::new();
        let mut resolved = Vec::new();
        for index in 0..roots.named.len() {
            let entry = &roots.named[index];
            if entry.through_atlases() {
                continue;
            }
            let kind = entry.usage.kind;
            let read = &mut roots.read;
            pack.entry(kind).or_insert_with(|| pack_ids(kind, cx, read));
            let files = find(&pack[&kind], entry);
            let in_vanilla = !find(
                vanilla
                    .entry(kind)
                    .or_insert_with(|| vanilla_ids(kind, cx.vanilla)),
                entry,
            )
            .is_empty();
            resolved.push(Resolved {
                source: entry.source,
                usage: entry.usage,
                files: files.clone(),
                reference: entry.reference(),
            });
            for physical in files {
                roots.roots.entry(physical).or_default().insert(index);
            }
            roots.in_vanilla[index] |= in_vanilla;
        }
        let item_definition =
            |physical: &PhysPath| cx.layout.kinds.get(physical) == Some(&FileKind::ItemDefinition);
        roots.unused = UnusedItems::new(selectors, sources, &resolved, droppable, item_definition)?;
        for (index, entry) in roots.named.iter().enumerate() {
            if let Some(owner) = roots.unused.unused_owner(entry.source, entry.usage) {
                roots.unused_owners.insert(index, owner);
            }
        }
        Ok(roots)
    }

    /// Resolves the usages that name atlas sprites through one view's atlases; call before reading its roots.
    pub(crate) fn resolve(&mut self, files: &Files<'_>, atlases: &ViewAtlases) {
        for index in 0..self.named.len() {
            if !self.named[index].through_atlases() {
                continue;
            }
            let logicals: Vec<String> = self
                .atlas_inputs(&self.named[index], atlases)
                .into_iter()
                .map(|input| input.logical.to_string())
                .collect();
            for logical in logicals {
                match files.get(&*logical) {
                    Some(&(_, physical)) => {
                        self.roots
                            .entry(physical.clone())
                            .or_default()
                            .insert(index);
                    }
                    None => {
                        if self.vanilla.has(&logical) {
                            self.in_vanilla[index] = true;
                        }
                    }
                }
            }
        }
    }

    pub(crate) fn reason(&self, physical: &PhysPath) -> Option<String> {
        self.roots
            .get(physical)
            .and_then(|indices| self.first_rooting(indices))
            .map(|index| self.named[index].reason())
    }

    /// A usage that is no unused declaration's roots `physical`: [`Self::reason`] is `Some`.
    pub(crate) fn roots_file(&self, physical: &PhysPath) -> bool {
        self.roots
            .get(physical)
            .is_some_and(|indices| self.first_rooting(indices).is_some())
    }

    /// The unused declarations whose usages name `physical`, when no other usage roots it.
    pub(crate) fn unused_owners(&self, physical: &PhysPath) -> BTreeSet<String> {
        match self.roots.get(physical) {
            Some(indices) if self.first_rooting(indices).is_none() => indices
                .iter()
                .filter_map(|index| self.unused_owners.get(index).cloned())
                .collect(),
            _ => BTreeSet::new(),
        }
    }

    /// Every file a usage roots, with the reason of the first usage that does.
    pub(crate) fn reasons(&self) -> Vec<(PhysPath, String)> {
        self.roots
            .iter()
            .filter_map(|(physical, indices)| {
                self.first_rooting(indices)
                    .map(|index| (physical.clone(), self.named[index].reason()))
            })
            .collect()
    }

    pub(crate) fn provenance(&self) -> Vec<SourceRoot> {
        self.roots
            .iter()
            .map(|(physical, indices)| SourceRoot {
                path: physical.clone(),
                usages: indices
                    .iter()
                    .filter(|index| !self.unused_owners.contains_key(index))
                    .map(|&index| self.named[index].reference())
                    .collect(),
            })
            .filter(|root| !root.usages.is_empty())
            .collect()
    }

    fn first_rooting(&self, indices: &BTreeSet<usize>) -> Option<usize> {
        indices
            .iter()
            .copied()
            .find(|index| !self.unused_owners.contains_key(index))
    }

    pub(crate) fn summaries(&self) -> Vec<SourceInfo> {
        let mut matched = vec![false; self.named.len()];
        for &index in self.roots.values().flatten() {
            matched[index] = true;
        }
        let mut summaries = Vec::new();
        let mut first = 0;
        for source in self.sources {
            let listed = first..first + source.usages.len();
            first = listed.end;
            let own: Vec<usize> = listed
                .filter(|&index| matched[index] || !self.named[index].usage.quiet)
                .collect();
            let unmatched: Vec<usize> = own
                .iter()
                .copied()
                .filter(|&index| !matched[index])
                .collect();
            let unknown: Vec<usize> = unmatched
                .iter()
                .copied()
                .filter(|&index| !self.in_vanilla[index])
                .collect();
            summaries.push(SourceInfo {
                id: source.source.clone(),
                usages: own.len(),
                matched: own.len() - unmatched.len(),
                vanilla: unmatched.len() - unknown.len(),
                unmatched: unknown
                    .iter()
                    .map(|&index| self.named[index].reference())
                    .collect(),
            });
        }
        summaries
    }

    fn atlas_inputs<'v>(
        &self,
        named: &Named<'_>,
        atlases: &'v ViewAtlases,
    ) -> Vec<&'v SpriteInput> {
        let interner = self.interner;
        let shown =
            |id: ResourceId| format!("{}:{}", interner.resolve(id.ns), interner.resolve(id.path));
        if named.usage.kind == UsageKind::TooltipStyle {
            let mut inputs = Vec::new();
            for rule in &self.rules.tooltip_sprites {
                for atlas in &rule.atlases {
                    if named.exact() {
                        if let Some((ns, path)) = named.exact_id() {
                            let sprite = ResourceId {
                                ns: interner.intern(ns),
                                path: interner.intern(&rule.of(path)),
                            };
                            inputs.extend(atlases.lookup(atlas, sprite));
                        }
                    } else {
                        inputs.extend(atlases.matching(atlas, |sprite| {
                            rule.unwrap(interner.resolve(sprite.path))
                                .is_some_and(|style| {
                                    named.glob.matches(&format!(
                                        "{}:{style}",
                                        interner.resolve(sprite.ns)
                                    ))
                                })
                        }));
                    }
                }
            }
            return inputs;
        }
        let atlas = named.atlas.as_deref().unwrap_or_default();
        if named.exact() {
            return named.exact_id().map_or_else(Vec::new, |(ns, path)| {
                let sprite = ResourceId {
                    ns: interner.intern(ns),
                    path: interner.intern(path),
                };
                atlases.lookup(atlas, sprite).iter().collect()
            });
        }
        atlases.matching(atlas, |sprite| named.glob.matches(&shown(sprite)))
    }
}

fn find<T: Clone>(ids: &HashMap<String, Vec<T>>, named: &Named<'_>) -> Vec<T> {
    if named.exact() {
        return ids.get(&*named.usage.id.0).cloned().unwrap_or_default();
    }
    let mut found: Vec<(&String, &Vec<T>)> = ids
        .iter()
        .filter(|(id, _)| named.glob.matches(id))
        .collect();
    // The order only reaches sets, but sorting keeps it independent of hashing.
    found.sort_by(|a, b| a.0.cmp(b.0));
    found
        .into_iter()
        .flat_map(|(_, targets)| targets.iter().cloned())
        .collect()
}

fn pack_ids(
    kind: UsageKind,
    cx: &Context<'_>,
    read: &mut Vec<PhysPath>,
) -> HashMap<String, Vec<PhysPath>> {
    let mut ids: HashMap<String, Vec<PhysPath>> = HashMap::new();
    for (physical, &file_kind) in &cx.layout.kinds {
        let logical = cx.layout.split(physical).1;
        if kind == UsageKind::Path {
            ids.entry(physical.as_str().to_owned())
                .or_default()
                .push(physical.clone());
        } else if kind == UsageKind::SoundEvent && file_kind == FileKind::SoundsJson {
            let namespace = logical
                .as_ref()
                .and_then(|logical| {
                    ResourcePath::of(logical.as_str()).map(|path| path.namespace.to_owned())
                })
                .unwrap_or_default();
            read.push(physical.clone());
            if let Some(events) = cx
                .facts
                .get(physical)
                .and_then(|facts| facts.events.as_ref())
            {
                for event in events {
                    ids.entry(format!("{namespace}:{event}"))
                        .or_default()
                        .push(physical.clone());
                }
            }
        } else if let Some(logical) = logical
            && let Some(id) = typed_id(kind, logical.as_str(), file_kind)
        {
            ids.entry(id).or_default().push(physical.clone());
        }
    }
    ids
}

/// Vanilla's names are logical paths; its sound events come from the asset index, when its objects are there.
fn vanilla_ids(kind: UsageKind, vanilla: &VanillaLayer<'_>) -> HashMap<String, Vec<String>> {
    let mut ids: HashMap<String, Vec<String>> = HashMap::new();
    if kind == UsageKind::SoundEvent {
        for event in vanilla.index.sound_events.iter().flatten() {
            ids.entry(format!("minecraft:{event}"))
                .or_default()
                .push(event.clone());
        }
        return ids;
    }
    for name in &vanilla.index.names {
        if !vanilla.has(name) {
            continue;
        }
        let id = if kind == UsageKind::Path {
            Some(name.clone())
        } else {
            typed_id(kind, name, FileKind::of(name))
        };
        if let Some(id) = id {
            ids.entry(id).or_default().push(name.clone());
        }
    }
    ids
}

/// The id a file under `assets/` answers to as `kind`: its namespace, or its id in its folder.
fn typed_id(kind: UsageKind, logical: &str, file_kind: FileKind) -> Option<String> {
    let path = ResourcePath::of(logical)?;
    let id_in = |expected: FileKind, suffix: &str| {
        (file_kind == expected).then(|| {
            let rest = &path.rest[..path.rest.len() - suffix.len()];
            format!("{}:{rest}", path.namespace)
        })
    };
    match kind {
        UsageKind::Namespace => Some(path.namespace.to_owned()),
        UsageKind::ItemDefinition => id_in(FileKind::ItemDefinition, ".json"),
        UsageKind::Model => id_in(FileKind::Model, ".json"),
        UsageKind::Sprite => id_in(FileKind::Texture, ".png"),
        UsageKind::Font => id_in(FileKind::Font, ".json"),
        UsageKind::Equipment => id_in(FileKind::Equipment, ".json"),
        UsageKind::Path | UsageKind::SoundEvent | UsageKind::TooltipStyle => None,
    }
}

#[cfg(test)]
mod tests {
    use super::Glob;

    #[test]
    fn path_globs_stay_in_a_folder_and_id_globs_do_not() {
        assert!(Glob::path("assets/fx/*.png").matches("assets/fx/a.png"));
        assert!(!Glob::path("assets/fx/*.png").matches("assets/fx/b/a.png"));
        assert!(Glob::path("assets/**.png").matches("assets/fx/b/a.png"));
        assert!(Glob::id("fx:*").matches("fx:furniture/chair"));
        assert!(Glob::id("fx:icon/c?in").matches("fx:icon/coin"));
        assert!(!Glob::id("fx:icon?coin").matches("fx:icon/coin"));
    }
}
