use std::collections::{BTreeMap, BTreeSet};

use packprune_graph::Names;
use packprune_pack::json::{self, Splice};
use packprune_pack::{LogicalPath, PhysPath, ResourceId, ViewSet};
use packprune_pass::{Edits, Loss, Pass, PassCx, PassError, Stage};

use super::{Atlases, resource_file};

pub(super) struct PruneUnnamedSprites;

struct Single {
    file: PhysPath,
    index: usize,
    atlas: ResourceId,
    sprite: ResourceId,
    texture: LogicalPath,
    views: ViewSet,
}

impl Pass for PruneUnnamedSprites {
    fn name(&self) -> &'static str {
        "prune-unnamed-sprites"
    }
    fn stage(&self) -> Stage {
        Stage::Lossy
    }
    fn loss(&self) -> Loss {
        Loss::Lossy
    }
    fn options_schema(&self) -> schemars::Schema {
        schemars::schema_for!(Options)
    }

    fn run(&self, cx: &PassCx<'_>) -> Result<Edits, PassError> {
        let _: Options = cx
            .options
            .clone()
            .try_into()
            .map_err(|e| PassError::Refused {
                pass: self.name(),
                why: format!("{e}"),
            })?;
        let atlases = Atlases::build(cx, self.name())?;
        let mut named = BTreeSet::new();
        let mut file_users = BTreeMap::new();
        let mut singles: BTreeMap<String, Single> = BTreeMap::new();
        for reference in cx.index.all().values().flatten() {
            match &reference.names {
                Names::Sprite {
                    sprite, atlases, ..
                } => {
                    for view in reference.views.iter() {
                        for atlas in atlases {
                            named.insert((view, *atlas, *sprite));
                        }
                    }
                }
                Names::File { logical, .. } if !reference.reason.0.starts_with("atlas.") => {
                    file_users
                        .entry(logical.clone())
                        .or_insert_with(|| reference.reason.0.to_string());
                }
                Names::SpriteName { atlas, sprite, .. } if cx.scope.contains(&reference.file) => {
                    let Some(at) = reference.pointer.0.strip_prefix("/sources/") else {
                        continue;
                    };
                    let at = at.strip_suffix("/sprite").unwrap_or(at);
                    let Ok(index) = at.parse::<usize>() else {
                        continue;
                    };
                    let Ok(tree) = json::parse(&cx.pack.files[&reference.file].blob) else {
                        continue;
                    };
                    let Some(source) = tree.path("sources").item(index) else {
                        continue;
                    };
                    let kind = source.path("type").as_string("");
                    if kind.strip_prefix("minecraft:").unwrap_or(&kind) != "single" {
                        continue;
                    }
                    let Some(resource) =
                        ResourceId::parse(&cx.run.interner, &source.path("resource").as_string(""))
                    else {
                        continue;
                    };
                    let Some(texture) = resource_file(cx, resource, "textures", ".png") else {
                        continue;
                    };
                    let key = format!("{} {index}", reference.file);
                    singles
                        .entry(key)
                        .and_modify(|single| single.views.union_with(&reference.views))
                        .or_insert_with(|| Single {
                            file: reference.file.clone(),
                            index,
                            atlas: *atlas,
                            sprite: *sprite,
                            texture,
                            views: reference.views.clone(),
                        });
                }
                _ => {}
            }
        }
        let mut kept = BTreeMap::<&str, usize>::new();
        let mut dropped = Vec::new();
        for single in singles.into_values() {
            let atlas = single.atlas.display(&cx.run.interner).to_string();
            let why = if single
                .views
                .iter()
                .any(|view| named.contains(&(view, single.atlas, single.sprite)))
            {
                Some("a reference of the pack names it")
            } else if cx.kept_names.contains(&single.texture)
                || cx.vanilla.has(single.texture.as_str())
            {
                Some("its texture is a name that stays")
            } else if atlases.vanilla.sprite(&atlas, single.sprite).is_some() {
                Some("vanilla's sprite")
            } else if !cx
                .layout
                .views
                .iter()
                .any(|view| view.files.contains_key(&single.texture))
            {
                Some("its texture is not in the pack")
            } else {
                None
            };
            if let Some(why) = why {
                *kept.entry(why).or_default() += 1;
            } else {
                dropped.push(single);
            }
        }
        let dropped_keys: BTreeSet<_> = dropped
            .iter()
            .flat_map(|single| {
                single.views.iter().map(move |view| {
                    (
                        view,
                        single.atlas.display(&cx.run.interner).to_string(),
                        single.sprite.display(&cx.run.interner).to_string(),
                    )
                })
            })
            .collect();
        let textures: BTreeSet<_> = dropped
            .iter()
            .map(|single| single.texture.clone())
            .collect();
        let mut held = BTreeSet::new();
        for (view, atlases) in &atlases.views {
            for atlas in atlases.atlas_ids() {
                for (id, sprite) in atlases.sprites(atlas, &cx.run.interner) {
                    for input in &sprite.inputs {
                        let Ok(logical) = LogicalPath::new(&input.logical) else {
                            continue;
                        };
                        if textures.contains(&logical)
                            && !(input.reason == "atlas.single"
                                && dropped_keys.contains(&(*view, atlas.to_owned(), id.clone())))
                        {
                            held.insert(logical);
                        }
                    }
                }
            }
        }
        let mut by_file: BTreeMap<PhysPath, Vec<&Single>> = BTreeMap::new();
        for single in &dropped {
            by_file.entry(single.file.clone()).or_default().push(single);
        }
        let mut edits = Edits::default();
        for (file, singles) in by_file {
            let input = &cx.pack.files[&file].blob;
            let tree = json::parse(input).expect("the index read this atlas");
            let sources = tree.path("sources");
            let mut array = Vec::from(b"[");
            for (index, source) in sources.items().iter().enumerate() {
                if singles.iter().any(|single| single.index == index) {
                    continue;
                }
                if array.len() > 1 {
                    array.push(b',');
                }
                array.extend_from_slice(
                    &input[source.span.start as usize..source.span.end as usize],
                );
            }
            array.push(b']');
            let rewritten = json::splice(
                input,
                vec![Splice {
                    range: sources.span.start as usize..sources.span.end as usize,
                    bytes: array,
                }],
            );
            let compact =
                packprune_codec::minify::json(&rewritten).map_err(|e| PassError::Failed {
                    pass: self.name(),
                    why: format!("{file}: {e}"),
                })?;
            edits.replaced.insert(file.clone(), compact.into());
            for single in singles {
                edits.changes.push(format!(
                    "{file}: removed {} of {}",
                    single.sprite.display(&cx.run.interner),
                    single.atlas.display(&cx.run.interner)
                ));
            }
        }
        let (mut bytes, mut files, mut held_files) = (0, 0, 0);
        for texture in textures {
            let why = file_users
                .get(&texture)
                .map(String::as_str)
                .or_else(|| held.contains(&texture).then_some("another sprite"))
                .or_else(|| {
                    (cx.kept_names.contains(&texture) || cx.vanilla.has(texture.as_str()))
                        .then_some("a name that stays")
                });
            if let Some(why) = why {
                held_files += 1;
                edits.changes.push(format!("kept {texture}: used as {why}"));
                continue;
            }
            let physicals: BTreeSet<_> = cx
                .layout
                .views
                .iter()
                .filter_map(|view| view.files.get(&texture))
                .collect();
            for physical in physicals {
                for path in [
                    physical.clone(),
                    PhysPath::new(&format!("{physical}.mcmeta")).expect("metadata path"),
                ] {
                    if let Some(entry) = cx.pack.files.get(&path)
                        && cx.scope.contains(&path)
                        && edits.removed.insert(path)
                    {
                        bytes += entry.blob.len();
                        files += 1;
                    }
                }
            }
        }
        for (why, count) in kept {
            edits
                .changes
                .push(format!("kept {count} sprite{}: {why}", plural(count)));
        }
        edits.changes.push(format!("{} sprite{} removed, {files} file{} removed ({bytes} B), {held_files} file{} kept as used otherwise", dropped.len(), plural(dropped.len()), plural(files), plural(held_files)));
        Ok(edits)
    }
}

#[derive(Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct Options {}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}
