//! The analysis: the roots of every view, the walk of each from them, and each file's status.
//!
//! Roots accumulate across views (a usage resolved through one view's atlases roots its file from then on, and a
//! shader include rooted in an earlier view is walked for itself in a later one), so they are decided first, in view
//! order; then the views are walked in parallel and their findings merged in view order, which gives the result of
//! walking them one after the other.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use packprune_pack::meta::show_format;
use packprune_pack::{ClientRules, FileKind, PatternError, PhysPath, ViewSet};
use rayon::prelude::*;

use crate::atlases::{Loaded, VanillaAtlases, ViewAtlases};
use crate::usages::{Context, UsageRoots};
use crate::walk::{Files, Found, NotStitchedAt, Shared, VanillaLayer, Walker, files};
use crate::{Edge, FileEntry, Inputs, Missing, NotStitched, Policy, Reached, Refused, Status};

const ROOT_KINDS: [FileKind; 8] = [
    FileKind::ItemDefinition,
    FileKind::Font,
    FileKind::SoundsJson,
    FileKind::Equipment,
    FileKind::Lang,
    FileKind::Shader,
    FileKind::Text,
    FileKind::WaypointStyle,
];

const REGISTRY_KINDS: [FileKind; 12] = [
    FileKind::ItemDefinition,
    FileKind::Blockstate,
    FileKind::Font,
    FileKind::SoundsJson,
    FileKind::Equipment,
    FileKind::Lang,
    FileKind::Shader,
    FileKind::Text,
    FileKind::WaypointStyle,
    FileKind::Atlas,
    FileKind::Particle,
    FileKind::PostEffect,
];

/// Why a file is reached without a reference. `unused_item`: an item definition only unused
/// declarations declare, which its kind alone does not root.
fn root_reason(
    policy: Policy,
    rules: &ClientRules,
    kind: FileKind,
    logical: &str,
    vanilla: &VanillaLayer<'_>,
    verified: bool,
    unused_item: bool,
) -> Option<Cow<'static, str>> {
    if policy == Policy::Vanilla {
        if REGISTRY_KINDS.contains(&kind) {
            return Some("vanilla_registry".into());
        }
        return rules
            .code_loaded
            .iter()
            .find(|glob| glob.matches(logical))
            .map(|glob| format!("code_loaded {}", glob.pattern).into());
    }
    if vanilla.index.has(logical) {
        return Some("vanilla_path".into());
    }
    if ROOT_KINDS.contains(&kind) && !unused_item {
        return Some(kind.id().into());
    }
    // FallbackResourceManager applies metadata from a higher pack to the PNG of a lower one.
    if kind == FileKind::TextureMeta
        && logical
            .strip_suffix(".mcmeta")
            .is_some_and(|texture| vanilla.has(texture))
    {
        return Some("vanilla_texture_meta".into());
    }
    // Without the asset index the vanilla sound names are unknown: a file under minecraft:sounds/ may override one.
    if !vanilla.index.sound_files
        && kind == FileKind::Sound
        && logical.starts_with("assets/minecraft/sounds/")
    {
        return Some("vanilla_sound_path".into());
    }
    // Another version's client may load names or shader syntax the given jar does not have: keep rather than guess.
    if !verified && matches!(kind, FileKind::Shader | FileKind::ShaderInclude) {
        return Some("unverified_format_shader".into());
    }
    if !verified && logical.starts_with("assets/minecraft/") {
        return Some("unverified_format_vanilla".into());
    }
    None
}

/// The roots of one view, in its logical order, and what only unused declarations name, by declaration.
#[derive(Default)]
struct ViewRoots<'a> {
    roots: Vec<&'a str>,
    unused: BTreeMap<String, Vec<&'a str>>,
}

pub(crate) fn analyse(inputs: &Inputs<'_>) -> Result<Reached, Refused> {
    let Inputs {
        cx,
        pack,
        layout,
        rules,
        vanilla: index,
        index: references,
        sources,
        selectors,
        policy,
    } = *inputs;
    let interner = &*cx.interner;
    let facts = &references.state.facts;
    let vanilla = VanillaLayer::new(index, &layout.meta.filters)
        .map_err(refuse("pack.mcmeta filter.block"))?;
    let shared = Shared {
        interner,
        rules,
        facts,
        kinds: &layout.kinds,
        vanilla: &vanilla,
    };
    let logical_of: BTreeMap<&PhysPath, String> = layout
        .kinds
        .keys()
        .filter_map(|path| {
            layout
                .split(path)
                .1
                .map(|logical| (path, logical.as_str().to_owned()))
        })
        .collect();

    let vanilla_atlases = VanillaAtlases::new(cx, index, &layout.meta.filters)
        .map_err(refuse("vanilla atlas filter"))?;
    let view_files: Vec<Files<'_>> = layout.views.iter().map(files).collect();
    let atlases: Vec<ViewAtlases> = layout
        .views
        .par_iter()
        .map(|view| {
            ViewAtlases::build(cx, &vanilla_atlases, view, |physical| {
                facts.get(physical).and_then(|facts| facts.atlas.as_deref())
            })
        })
        .collect::<Result<_, _>>()
        .map_err(refuse("atlas filter"))?;
    let mut read: BTreeSet<PhysPath> = BTreeSet::new();
    for files in &view_files {
        for (_, logical) in vanilla_atlases.atlas_files() {
            if let Some(&(_, physical)) = files.get(logical) {
                read.insert(physical.clone());
            }
        }
    }

    let reason_at = |physical: &PhysPath, logical: &str, verified: bool, unused_item: bool| {
        root_reason(
            policy,
            rules,
            layout.kinds[physical],
            logical,
            &vanilla,
            verified,
            unused_item,
        )
    };
    let reason = |physical: &PhysPath, verified: bool, unused_item: bool| {
        reason_at(physical, logical_of.get(physical)?, verified, unused_item)
    };
    let context = Context {
        layout,
        facts,
        vanilla: &vanilla,
        rules,
        interner,
    };
    // An item definition is droppable when no view would root it but for its kind.
    let mut usages = UsageRoots::new(&context, sources, selectors, |physical| {
        reason(physical, true, true).is_none() && reason(physical, false, true).is_none()
    })?;
    read.extend(usages.read.drain(..));

    let mut roots: BTreeMap<PhysPath, String> = BTreeMap::new();
    let mut first_root: HashMap<&PhysPath, usize> = HashMap::new();
    for (path, kind) in &layout.kinds {
        if *kind == FileKind::PackMeta {
            roots
                .entry(path.clone())
                .or_insert_with(|| path.as_str().to_owned());
            first_root.entry(path).or_insert(0);
        }
    }
    let mut view_roots: Vec<ViewRoots<'_>> = Vec::with_capacity(layout.views.len());
    for (rank, view) in layout.views.iter().enumerate() {
        usages.resolve(&view_files[rank], &atlases[rank]);
        let mut found = ViewRoots::default();
        for (logical, physical) in &view.files {
            let own = || {
                reason_at(
                    physical,
                    logical.as_str(),
                    view.verified,
                    usages.unused.unused_definition(physical),
                )
            };
            // A file keeps the reason it was first rooted for: later views only ask whether they root it.
            let rooted = if roots.contains_key(physical) {
                usages.roots_file(physical) || own().is_some()
            } else {
                match usages
                    .reason(physical)
                    .or_else(|| own().map(Cow::into_owned))
                {
                    Some(why) => {
                        roots.insert(physical.clone(), why);
                        true
                    }
                    None => false,
                }
            };
            if rooted {
                first_root.entry(physical).or_insert(rank);
                found.roots.push(logical.as_str());
            } else {
                for owner in usages.unused_owners(physical) {
                    found
                        .unused
                        .entry(owner)
                        .or_default()
                        .push(logical.as_str());
                }
            }
        }
        view_roots.push(found);
    }

    let walked: Vec<(Found<'_>, Loaded)> = (0..layout.views.len())
        .into_par_iter()
        .map(|rank| {
            let rooted =
                |physical: &PhysPath| first_root.get(physical).is_some_and(|&first| first <= rank);
            let mut walker = Walker::new(
                &shared,
                &view_files[rank],
                layout.views[rank].verified,
                &atlases[rank],
                &rooted,
            );
            for &logical in &view_roots[rank].roots {
                walker.enqueue(logical);
            }
            walker.run();
            for (owner, logicals) in &view_roots[rank].unused {
                walker.run_unused(owner, logicals);
            }
            (walker.found, atlases[rank].loaded())
        })
        .collect();

    let mut reached: BTreeSet<&PhysPath> = BTreeSet::new();
    // Every view walks mostly the same edges: they are merged by reference and copied once each.
    let mut edges: BTreeSet<(&PhysPath, &PhysPath, &str)> = BTreeSet::new();
    let mut missing: BTreeMap<(&PhysPath, &str, &str), ViewSet> = BTreeMap::new();
    let mut not_stitched: BTreeMap<NotStitchedAt<'_>, ViewSet> = BTreeMap::new();
    let mut unused: BTreeMap<PhysPath, BTreeSet<String>> = BTreeMap::new();
    let mut atlas_loaded: BTreeMap<&PhysPath, BTreeSet<String>> = BTreeMap::new();
    for (rank, (found, loaded)) in walked.iter().enumerate() {
        let id = layout.views[rank].id;
        reached.extend(found.reached.iter().copied());
        for (from, to, why) in &found.edges {
            edges.insert((from, to, why));
        }
        for (source, target, why) in &found.missing {
            missing.entry((source, target, why)).or_default().insert(id);
        }
        for (source, sprite, atlases, file) in &found.not_stitched {
            not_stitched
                .entry((source, sprite.clone(), atlases.clone(), file.clone()))
                .or_default()
                .insert(id);
        }
        for (physical, owner) in &found.unused {
            unused
                .entry((*physical).clone())
                .or_default()
                .insert((*owner).to_owned());
        }
        read.extend(found.read.iter().map(|&physical| physical.clone()));
        for (logical, ids) in loaded {
            if let Some(&(_, physical)) = view_files[rank].get(&**logical) {
                atlas_loaded
                    .entry(physical)
                    .or_default()
                    .extend(ids.iter().map(|id| id.to_string()));
            }
        }
    }
    // A file a usage names that no view shows (a shadowed or undeclared copy) is still kept.
    for (physical, why) in usages.reasons() {
        roots.entry(physical).or_insert(why);
    }

    let mut entries = BTreeMap::new();
    for (path, &kind) in &layout.kinds {
        // A .png.mcmeta is loaded with its texture, so it shares the texture's atlases.
        let texture = match kind {
            FileKind::TextureMeta => path
                .as_str()
                .strip_suffix(".mcmeta")
                .and_then(|texture| PhysPath::new(texture).ok()),
            _ => None,
        };
        let atlases = atlas_loaded
            .get(texture.as_ref().unwrap_or(path))
            .cloned()
            .unwrap_or_default();
        let status = if reached.contains(path) || roots.contains_key(path) {
            Status::Reached
        } else if unused.contains_key(path) {
            Status::Unused
        } else if kind == FileKind::Unknown {
            Status::Unknown
        } else if kind == FileKind::Blockstate
            && !logical_of
                .get(path)
                .is_some_and(|logical| index.has(logical))
        {
            Status::DeadBlockstate
        } else if !atlases.is_empty() {
            Status::AtlasLoadedOrphan
        } else {
            Status::Orphan
        };
        entries.insert(
            path.clone(),
            FileEntry {
                kind,
                bytes: pack
                    .files
                    .get(path)
                    .map_or(0, |entry| entry.blob.len() as u64),
                status,
                root: roots.get(path).cloned(),
                atlases,
            },
        );
    }

    // A visited atlas file that is no atlas the jar defines is still decoded, and refused if its filter is invalid.
    for path in &read {
        if let Some(atlas) = facts.get(path).and_then(|facts| facts.atlas.as_ref()) {
            atlas
                .check()
                .map_err(|error| Refused(format!("{path}: atlas filter: {error}")))?;
        }
    }
    let invalid = read
        .iter()
        .filter_map(|path| {
            let why = facts.get(path)?.invalid.as_ref()?;
            Some((path.clone(), why.to_string()))
        })
        .collect();
    let unused_items = usages.unused.selections(&entries, &unused);
    Ok(Reached {
        files: entries,
        edges: edges
            .into_iter()
            .map(|(from, to, reason)| Edge {
                from: from.clone(),
                to: to.clone(),
                reason: reason.into(),
            })
            .collect(),
        missing: missing
            .into_iter()
            .map(|((source, target, reason), views)| Missing {
                source: source.clone(),
                target: target.into(),
                reason: reason.into(),
                views,
            })
            .collect(),
        not_stitched: not_stitched
            .into_iter()
            .map(|((source, sprite, atlases, file), views)| NotStitched {
                source: source.clone(),
                sprite: sprite.into(),
                atlases: atlases.into(),
                file: file.into(),
                views,
            })
            .collect(),
        invalid,
        sources: usages.summaries(),
        source_roots: usages.provenance(),
        warnings: warnings(inputs, &usages),
        unused_items,
    })
}

fn warnings(inputs: &Inputs<'_>, usages: &UsageRoots<'_>) -> Vec<String> {
    let index = inputs.vanilla;
    let mut notes = Vec::new();
    notes.extend(inputs.rules.warning.clone());
    notes.extend(usages.unused.warnings());
    if !index.sound_files {
        notes.push(
            "no asset index: minecraft: sound files and events the pack does not define are assumed vanilla, and every \
             file under minecraft:sounds/ is kept"
                .into(),
        );
    } else if index.sound_events.is_none() {
        notes.push(
            "the asset index's objects are not beside it: minecraft: sound events the pack does not define are \
             assumed vanilla"
                .into(),
        );
    }
    let views = &inputs.layout.views;
    let others: Vec<String> = views
        .iter()
        .filter(|view| !view.verified)
        .map(|view| show_format(view.formats.min).to_string())
        .collect();
    let jar = &index.pack_format;
    if !others.is_empty() {
        notes.push(format!(
            "views at formats {} cannot be checked against the vanilla of client {} (format {jar}): their minecraft: \
             files and shaders are kept, and their findings are listed under Other formats, unverified",
            others.join(", "),
            index.version
        ));
    }
    if !views.iter().any(|view| view.verified) {
        notes.push(format!(
            "the pack declares no view at the client jar's format {jar}: no finding sets the exit code, only orphans do"
        ));
    }
    notes
}

/// A pattern that does not compile: the run is refused with it.
fn refuse(what: &str) -> impl Fn(PatternError) -> Refused + '_ {
    move |error| Refused(format!("{what}: {error}"))
}
