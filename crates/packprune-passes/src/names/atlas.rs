use std::collections::{BTreeMap, BTreeSet};

use packprune_pack::json::{self, Node, Value};
use packprune_pack::{FileKind, ResourceId};
use packprune_pass::{Edits, Loss, Pass, PassCx, PassError, Stage};

use crate::overlays::json::{object, set, string};

pub struct AtlasDirectory;

impl Pass for AtlasDirectory {
    fn name(&self) -> &'static str {
        "atlas-directory"
    }
    fn stage(&self) -> Stage {
        Stage::Json
    }
    fn loss(&self) -> Loss {
        Loss::Lossless
    }
    fn options_schema(&self) -> schemars::Schema {
        schemars::schema_for!(crate::overlays::json::NoOptions)
    }

    fn run(&self, cx: &PassCx<'_>) -> Result<Edits, PassError> {
        let vanilla = Folders::new(cx.vanilla.names.iter().map(String::as_str));
        let mut folders = BTreeMap::new();
        let mut edits = Edits::default();
        let mut merged = 0;
        for (physical, entry) in &cx.pack.files {
            let Some(logical) = cx.layout.split(physical).1 else {
                continue;
            };
            if cx.layout.kinds[physical] != FileKind::Atlas {
                continue;
            }
            let reading: Vec<_> = cx
                .layout
                .views
                .iter()
                .filter(|view| view.files.get(&logical) == Some(physical))
                .collect();
            let Ok(mut atlas) = json::parse(&entry.blob) else {
                continue;
            };
            if reading.is_empty() || !atlas.path("sources").is_array() {
                continue;
            }
            let seen: Vec<_> = reading
                .iter()
                .map(|view| {
                    folders
                        .entry(view.id)
                        .or_insert_with(|| {
                            Folders::new(view.files.keys().map(|logical| logical.as_str()))
                        })
                        .clone()
                })
                .collect();
            let runs = runs(cx, atlas.path("sources").items(), &vanilla, &seen);
            if runs.is_empty() {
                continue;
            }
            let mut replaced = BTreeMap::new();
            for (folder, indexes) in &runs {
                for index in indexes {
                    replaced.insert(*index, (folder, indexes));
                }
            }
            let mut sources = Vec::new();
            for (i, source) in atlas.path("sources").items().iter().enumerate() {
                if let Some((folder, indexes)) = replaced.get(&i) {
                    if indexes.first() == Some(&i) {
                        sources.push(object(&[
                            ("type", string("directory")),
                            ("source", string(folder)),
                            ("prefix", string(&format!("{folder}/"))),
                        ]));
                    }
                } else {
                    sources.push(source.clone());
                }
            }
            set(&mut atlas, "sources", Node::new(Value::Array(sources)));
            let shorter = json::write(&atlas);
            if shorter.len() < entry.blob.len() {
                merged += runs.iter().map(|(_, indexes)| indexes.len()).sum::<usize>();
                edits.replaced.insert(physical.clone(), shorter.into());
            }
        }
        edits.changes.push(format!(
            "{merged} single sources merged into directory sources, in {} atlas files",
            edits.replaced.len()
        ));
        Ok(edits)
    }
}

#[derive(Clone, Default)]
struct Folders(BTreeMap<String, BTreeSet<String>>);

impl Folders {
    fn new<'a>(logicals: impl Iterator<Item = &'a str>) -> Self {
        let mut folders = Self::default();
        for logical in logicals {
            if FileKind::of(logical) != FileKind::Texture {
                continue;
            }
            let Some((_, path)) = logical.split_once("/textures/") else {
                continue;
            };
            for (slash, _) in path.match_indices('/') {
                folders
                    .0
                    .entry(path[..slash].to_owned())
                    .or_default()
                    .insert(logical.to_owned());
            }
        }
        folders
    }
    fn inside(&self, folder: &str) -> BTreeSet<String> {
        self.0.get(folder).cloned().unwrap_or_default()
    }
}

struct Single {
    index: usize,
    resource: ResourceId,
    path: String,
    logical: String,
}

fn id(cx: &PassCx<'_>, node: &Node) -> Option<ResourceId> {
    ResourceId::parse(&cx.run.interner, node.as_str()?)
}

fn is_single(source: &Node) -> bool {
    matches!(
        source.path("type").as_str(),
        Some("single" | "minecraft:single")
    )
}

fn runs(
    cx: &PassCx<'_>,
    sources: &[Node],
    vanilla: &Folders,
    views: &[Folders],
) -> Vec<(String, BTreeSet<usize>)> {
    let mut singles = Vec::new();
    let mut candidates = BTreeSet::new();
    for (index, source) in sources.iter().enumerate() {
        let resource = id(cx, source.path("resource"));
        let sprite = if source.has("sprite") {
            id(cx, source.path("sprite"))
        } else {
            resource
        };
        if is_single(source)
            && resource == sprite
            && let Some(resource) = resource
        {
            let path = cx.run.interner.resolve(resource.path).to_string();
            for (slash, _) in path.match_indices('/').filter(|(slash, _)| *slash > 0) {
                let folder = &path[..slash];
                candidates.insert((
                    folder.bytes().filter(|byte| *byte == b'/').count(),
                    folder.to_owned(),
                ));
            }
            let ns = cx.run.interner.resolve(resource.ns);
            singles.push(Single {
                index,
                resource,
                logical: format!("assets/{ns}/textures/{path}.png"),
                path,
            });
        }
    }
    let mut claimed = BTreeSet::<usize>::new();
    let mut runs = Vec::new();
    for (_, folder) in candidates {
        let members: Vec<_> = singles
            .iter()
            .filter(|single| {
                !claimed.contains(&single.index) && single.path.starts_with(&format!("{folder}/"))
            })
            .collect();
        if members.len() < 2 || !vanilla.inside(&folder).is_empty() {
            continue;
        }
        let indexes: BTreeSet<_> = members.iter().map(|single| single.index).collect();
        let sprites: BTreeSet<_> = members.iter().map(|single| single.resource).collect();
        let independent = (members[0].index + 1..members[members.len() - 1].index).all(|i| {
            let source = &sources[i];
            let sprite = id(
                cx,
                if source.has("sprite") {
                    source.path("sprite")
                } else {
                    source.path("resource")
                },
            );
            indexes.contains(&i)
                || is_single(source) && sprite.is_some_and(|sprite| !sprites.contains(&sprite))
        });
        let named: BTreeSet<_> = members
            .iter()
            .map(|single| single.logical.clone())
            .collect();
        if independent && views.iter().all(|view| view.inside(&folder) == named) {
            claimed.extend(indexes.iter().copied());
            runs.push((folder, indexes));
        }
    }
    runs
}
