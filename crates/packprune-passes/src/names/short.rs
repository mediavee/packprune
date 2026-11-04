use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use packprune_graph::Names;
use packprune_graph::atlases::{AtlasFile, VanillaAtlases, ViewAtlases};
use packprune_pack::json;
use packprune_pack::{JavaPattern, LogicalPath, PhysPath, ResourceId};
use packprune_pass::{Edits, Loss, Pass, PassCx, PassError, Stage};

pub struct ShortNames;

type Origins = BTreeMap<LogicalPath, BTreeSet<PhysPath>>;
type Sprites = BTreeMap<LogicalPath, ResourceId>;

impl Pass for ShortNames {
    fn name(&self) -> &'static str {
        "short-names"
    }
    fn stage(&self) -> Stage {
        Stage::Names
    }
    fn loss(&self) -> Loss {
        Loss::Lossless
    }
    fn options_schema(&self) -> schemars::Schema {
        schemars::schema_for!(crate::overlays::json::NoOptions)
    }

    fn run(&self, cx: &PassCx<'_>) -> Result<Edits, PassError> {
        let mut physical = BTreeMap::<LogicalPath, Vec<PhysPath>>::new();
        let mut namespaces = BTreeSet::from(["minecraft".to_owned()]);
        let mut uuids = 0;
        for path in cx.pack.files.keys() {
            let Some(logical) = cx.layout.split(path).1 else {
                continue;
            };
            if let Some(namespace) = logical.as_str().split('/').nth(1) {
                namespaces.insert(namespace.to_owned());
            }
            if uuid_file(logical.as_str()).is_some() && path.as_str().ends_with(logical.as_str()) {
                uuids += 1;
                if !cx.kept_names.contains(&logical) {
                    physical.entry(logical).or_default().push(path.clone());
                }
            }
        }
        for reference in cx.index.all().values().flatten() {
            if let Names::Text { logical } = &reference.names {
                physical.remove(logical);
            }
        }
        let (mut sprites, origins, mut unsafe_files) = sprite_trouble(cx, &physical)?;
        for reference in cx.index.all().values().flatten() {
            if let Names::Sprite {
                sprite,
                prefix,
                suffix,
                ..
            } = &reference.names
                && (!prefix.is_empty() || !suffix.is_empty())
            {
                unsafe_files.extend(
                    sprites
                        .iter()
                        .filter(|(_, id)| *id == sprite)
                        .map(|(logical, _)| logical.clone()),
                );
            }
        }
        physical.retain(|logical, _| !unsafe_files.contains(logical));
        sprites.retain(|logical, _| physical.contains_key(logical));
        let filters = filters_after(cx, &sprites, &origins)?;
        let namespace = free_namespace(&namespaces, &filters);
        let (ids, counters) = loop {
            let (ids, counters) = ids(&physical, &origins);
            let differently: Vec<_> = sprites
                .iter()
                .filter_map(|(logical, sprite)| {
                    let renamed = ids.get(logical)?;
                    filters[logical]
                        .iter()
                        .any(|filter| {
                            filter.removes(cx, *sprite) != filter.matches(&namespace, renamed)
                        })
                        .then(|| logical.clone())
                })
                .collect();
            if differently.is_empty() {
                break (ids, counters);
            }
            for logical in differently {
                physical.remove(&logical);
            }
        };
        let mut edits = Edits::default();
        for (logical, paths) in &physical {
            let (kind, _, suffix) = uuid_file(logical.as_str()).expect("candidate");
            let id = &ids[logical];
            let target = LogicalPath::new(&format!("assets/{namespace}/{kind}/{id}{suffix}"))
                .expect("generated path");
            for path in paths {
                let layer = &path.as_str()[..path.as_str().len() - logical.as_str().len()];
                let to = PhysPath::new(&format!("{layer}{target}")).expect("generated path");
                edits
                    .added
                    .insert(to, Arc::clone(&cx.pack.files[path].blob));
                edits.removed.insert(path.clone());
            }
            edits.file_remaps.insert(logical.clone(), target);
            if let Some(sprite) = sprites.get(logical) {
                let renamed = ResourceId::parse(&cx.run.interner, &format!("{namespace}:{id}"))
                    .expect("generated id");
                edits.sprite_remaps.insert(*sprite, renamed);
            }
        }
        let count = |kind| counters.get(kind).copied().unwrap_or(0);
        edits.changes.push(format!(
            "{} models, {} textures and {} sounds renamed into {namespace}:",
            count("models"),
            count("textures"),
            count("sounds")
        ));
        edits
            .changes
            .push(format!("{} UUID-named files kept", uuids - physical.len()));
        Ok(edits)
    }
}

fn fail(error: impl ToString) -> PassError {
    PassError::Failed {
        pass: "short-names",
        why: error.to_string(),
    }
}

fn uuid_file(logical: &str) -> Option<(&str, &str, &str)> {
    let mut parts = logical.strip_prefix("assets/")?.split('/');
    let namespace = parts.next()?;
    if namespace.is_empty()
        || !namespace
            .bytes()
            .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-'))
    {
        return None;
    }
    let kind = parts.next()?;
    let file = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let suffix = match kind {
        "models" => ".json",
        "textures" => ".png",
        "sounds" => ".ogg",
        _ => return None,
    };
    let uuid = file.strip_suffix(suffix)?;
    if uuid.len() != 36
        || !uuid.bytes().enumerate().all(|(i, byte)| {
            if [8, 13, 18, 23].contains(&i) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')
            }
        })
    {
        return None;
    }
    Some((kind, uuid, suffix))
}

fn sprite_trouble(
    cx: &PassCx<'_>,
    candidates: &BTreeMap<LogicalPath, Vec<PhysPath>>,
) -> Result<(Sprites, Origins, BTreeSet<LogicalPath>), PassError> {
    let by_id: BTreeMap<_, _> = candidates
        .keys()
        .filter_map(|logical| {
            let (kind, uuid, _) = uuid_file(logical.as_str())?;
            let namespace = logical.as_str().split('/').nth(1)?;
            (kind == "textures").then(|| {
                (
                    ResourceId::parse(&cx.run.interner, &format!("{namespace}:{uuid}"))
                        .expect("candidate id"),
                    logical.clone(),
                )
            })
        })
        .collect();
    let vanilla = VanillaAtlases::new(cx.run, cx.vanilla, &cx.layout.meta.filters).map_err(fail)?;
    let atlas_files: BTreeMap<_, _> = cx
        .pack
        .files
        .iter()
        .filter_map(|(path, entry)| {
            (cx.layout.kinds[path] == packprune_pack::FileKind::Atlas)
                .then(|| AtlasFile::parse(cx.run, &entry.blob).map(|atlas| (path.clone(), atlas)))
                .flatten()
        })
        .collect();
    let mut sprites = Sprites::new();
    let mut origins = Origins::new();
    let mut unsafe_files = BTreeSet::new();
    for view in &cx.layout.views {
        let atlases = ViewAtlases::build(cx.run, &vanilla, view, |path| atlas_files.get(path))
            .map_err(fail)?;
        for atlas in atlases.atlas_ids() {
            for (id, sprite) in atlases.sprites(atlas, &cx.run.interner) {
                let Some(id) = ResourceId::parse(&cx.run.interner, &id) else {
                    unsafe_files.extend(
                        sprite
                            .inputs
                            .iter()
                            .filter_map(|input| LogicalPath::new(&input.logical).ok()),
                    );
                    continue;
                };
                let named = by_id.get(&id);
                let inputs = &sprite.inputs;
                if let Some(named) = named
                    && !(inputs.len() == 1 && &*inputs[0].logical == named.as_str())
                {
                    unsafe_files.insert(named.clone());
                }
                for input in inputs {
                    let Ok(texture) = LogicalPath::new(&input.logical) else {
                        continue;
                    };
                    if !candidates.contains_key(&texture) {
                        continue;
                    }
                    let own = named == Some(&texture);
                    let supported = inputs.len() == 1
                        && (input.reason == "atlas.unstitch"
                            || input.reason == "atlas.single" && view.files.contains_key(&texture));
                    if !supported {
                        unsafe_files.insert(texture);
                    } else if own {
                        sprites.insert(texture.clone(), id);
                        if let Some(origin) = &sprite.origin {
                            origins.entry(texture).or_default().insert(origin.clone());
                        }
                    }
                }
            }
        }
    }
    Ok((sprites, origins, unsafe_files))
}

#[derive(Clone)]
struct Filter {
    namespace: Option<JavaPattern>,
    path: Option<JavaPattern>,
}
impl Filter {
    fn matches(&self, namespace: &str, path: &str) -> bool {
        self.namespace
            .as_ref()
            .is_none_or(|pattern| pattern.find(namespace))
            && self.path.as_ref().is_none_or(|pattern| pattern.find(path))
    }
    fn removes(&self, cx: &PassCx<'_>, sprite: ResourceId) -> bool {
        self.matches(
            cx.run.interner.resolve(sprite.ns),
            cx.run.interner.resolve(sprite.path),
        )
    }
}

/// An atlas file's sources as `filters_after` reads them: where each resource is first defined, and each filter
/// source with its position, its pattern error kept until a sprite defined at or before it reads it.
struct Sources {
    defined: BTreeMap<ResourceId, usize>,
    filters: Vec<(usize, Result<Filter, String>)>,
}

impl Sources {
    fn read(cx: &PassCx<'_>, blob: &[u8]) -> Option<Self> {
        let atlas = json::parse(blob).ok()?;
        let mut defined = BTreeMap::new();
        let mut filters = Vec::new();
        for (at, source) in atlas.path("sources").items().iter().enumerate() {
            if let Some(id) =
                ResourceId::parse(&cx.run.interner, &source.path("resource").as_string(""))
            {
                defined.entry(id).or_insert(at);
            }
            if matches!(
                source.path("type").as_str(),
                Some("filter" | "minecraft:filter")
            ) {
                let pattern = |field| {
                    source
                        .path("pattern")
                        .path(field)
                        .as_str()
                        .map(JavaPattern::new)
                        .transpose()
                        .map_err(|error| error.to_string())
                };
                let filter = pattern("namespace")
                    .and_then(|namespace| Ok((namespace, pattern("path")?)))
                    .map(|(namespace, path)| Filter { namespace, path });
                filters.push((at, filter));
            }
        }
        Some(Self { defined, filters })
    }
}

/// The filter sources of each texture's atlas files from the source defining its sprite on (from the first source
/// when none does), each file parsed once.
fn filters_after(
    cx: &PassCx<'_>,
    sprites: &Sprites,
    origins: &Origins,
) -> Result<BTreeMap<LogicalPath, Vec<Filter>>, PassError> {
    let mut files: BTreeMap<&PhysPath, Option<Sources>> = BTreeMap::new();
    let mut filters = BTreeMap::new();
    for (texture, sprite) in sprites {
        let mut after = Vec::new();
        for file in origins.get(texture).into_iter().flatten() {
            let sources = files
                .entry(file)
                .or_insert_with(|| Sources::read(cx, &cx.pack.files[file].blob));
            let Some(sources) = sources else {
                continue;
            };
            let defined = sources.defined.get(sprite).copied().unwrap_or(0);
            for (at, filter) in &sources.filters {
                if *at >= defined {
                    after.push(filter.clone().map_err(fail)?);
                }
            }
        }
        filters.insert(texture.clone(), after);
    }
    Ok(filters)
}

fn free_namespace(used: &BTreeSet<String>, filters: &BTreeMap<LogicalPath, Vec<Filter>>) -> String {
    let mut namespace = "z".to_owned();
    while used.contains(&namespace) {
        namespace.push('z');
    }
    let mut candidate = namespace.clone();
    for _ in 0..8 {
        if !used.contains(&candidate)
            && filters.values().flatten().all(|filter| {
                filter
                    .namespace
                    .as_ref()
                    .is_none_or(|pattern| !pattern.find(&candidate))
            })
        {
            return candidate;
        }
        candidate.push('z');
    }
    namespace
}

fn base36(mut n: usize) -> String {
    let mut digits = Vec::new();
    loop {
        digits.push(b"0123456789abcdefghijklmnopqrstuvwxyz"[n % 36] as char);
        n /= 36;
        if n == 0 {
            break;
        }
    }
    digits.into_iter().rev().collect()
}

fn ids(
    physical: &BTreeMap<LogicalPath, Vec<PhysPath>>,
    origins: &Origins,
) -> (BTreeMap<LogicalPath, String>, BTreeMap<String, usize>) {
    let mut signatures = BTreeMap::<String, Vec<LogicalPath>>::new();
    for (texture, files) in origins {
        if physical.contains_key(texture) {
            let signature = files
                .iter()
                .map(PhysPath::as_str)
                .collect::<Vec<_>>()
                .join("|");
            signatures
                .entry(signature)
                .or_default()
                .push(texture.clone());
        }
    }
    let mut folders = BTreeMap::new();
    for (n, group) in signatures
        .values()
        .filter(|group| group.len() >= 2)
        .enumerate()
    {
        for texture in group {
            folders.insert(texture.clone(), format!("{}/", base36(n)));
        }
    }
    let mut ids = BTreeMap::new();
    let mut counters = BTreeMap::<String, usize>::new();
    for logical in physical.keys() {
        let (kind, _, _) = uuid_file(logical.as_str()).expect("candidate");
        let n = counters.entry(kind.to_owned()).or_default();
        ids.insert(
            logical.clone(),
            format!(
                "{}{}",
                folders.get(logical).map_or("", String::as_str),
                base36(*n)
            ),
        );
        *n += 1;
    }
    (ids, counters)
}
