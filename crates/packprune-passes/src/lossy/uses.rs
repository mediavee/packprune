use std::collections::{BTreeMap, BTreeSet};

use packprune_graph::Names;
use packprune_pack::json::{self, Node};
use packprune_pack::{FileKind, LogicalPath, PhysPath, ResourceId, View};
use packprune_pass::{PassCx, PassError};

use super::{Atlases, resource_file};

#[derive(Clone, Copy, Debug)]
pub(super) struct Faces {
    pub u: f64,
    pub v: f64,
}

impl Faces {
    fn min(self, other: Self) -> Self {
        Self {
            u: self.u.min(other.u),
            v: self.v.min(other.v),
        }
    }
}

pub(super) struct Uses {
    pub candidates: BTreeMap<PhysPath, Faces>,
    pub skipped: BTreeMap<PhysPath, String>,
}

impl Uses {
    pub fn of(cx: &PassCx<'_>) -> Result<Self, PassError> {
        let atlases = Atlases::build(cx, "texel-downscale")?;
        let trees: BTreeMap<_, _> = cx
            .pack
            .files
            .iter()
            .filter(|(path, _)| {
                matches!(
                    cx.layout.kinds.get(*path),
                    Some(FileKind::Model | FileKind::TextureMeta)
                )
            })
            .filter_map(|(path, entry)| {
                json::parse(&entry.blob)
                    .ok()
                    .map(|tree| (path.clone(), tree))
            })
            .collect();
        let mut blocked = BTreeMap::<LogicalPath, String>::new();
        let mut model_atlases = Vec::new();
        for reference in cx.index.all().values().flatten() {
            match &reference.names {
                Names::File { logical, .. } if !reference.reason.0.starts_with("atlas.") => {
                    blocked
                        .entry(logical.clone())
                        .or_insert_with(|| reference.reason.0.to_string());
                }
                Names::Sprite { atlases, .. }
                    if &*reference.reason.0 == "model.texture" && model_atlases.is_empty() =>
                {
                    model_atlases.extend(
                        atlases
                            .iter()
                            .map(|id| id.display(&cx.run.interner).to_string()),
                    );
                }
                _ => {}
            }
        }
        let mut faces = BTreeMap::<PhysPath, Faces>::new();
        for view in &cx.layout.views {
            let mut named = BTreeMap::<ResourceId, Faces>::new();
            let mut undrawn = BTreeSet::new();
            for physical in view.files.values() {
                if cx.layout.kinds.get(physical) == Some(&FileKind::Model)
                    && let Some(tree) = trees.get(physical)
                {
                    model(cx, view, &trees, tree, &mut named, &mut undrawn);
                }
            }
            let view_atlases = &atlases.views[&view.id];
            for atlas in &model_atlases {
                for sprite in &undrawn {
                    if let Some(found) = view_atlases.sprite(atlas, *sprite) {
                        for input in &found.inputs {
                            let Ok(logical) = LogicalPath::new(&input.logical) else {
                                continue;
                            };
                            if view.files.contains_key(&logical) {
                                blocked.entry(logical).or_insert_with(|| "a model names it in a slot no face draws, or under a vanilla parent".into());
                            }
                        }
                    }
                }
                for (sprite, density) in &named {
                    let Some(found) = view_atlases.sprite(atlas, *sprite) else {
                        continue;
                    };
                    let plain = found.inputs.len() == 1
                        && found.region.is_empty()
                        && matches!(found.inputs[0].reason, "atlas.single" | "atlas.directory");
                    for input in &found.inputs {
                        let Ok(logical) = LogicalPath::new(&input.logical) else {
                            continue;
                        };
                        let Some(physical) = view.files.get(&logical) else {
                            continue;
                        };
                        let metadata =
                            LogicalPath::new(&format!("{logical}.mcmeta")).expect("metadata path");
                        if !plain {
                            blocked.entry(logical).or_insert_with(|| {
                                format!(
                                    "its sprite is built by {}, not from the whole file",
                                    input.reason
                                )
                            });
                        } else if undrawn.contains(sprite) {
                            continue;
                        } else if view
                            .files
                            .get(&metadata)
                            .and_then(|path| trees.get(path))
                            .is_some_and(|meta| meta.has("animation"))
                        {
                            blocked.entry(logical).or_insert_with(|| "animated".into());
                        } else {
                            faces
                                .entry(physical.clone())
                                .and_modify(|old| *old = old.min(*density))
                                .or_insert(*density);
                        }
                    }
                }
            }
            for atlas in view_atlases.atlas_ids() {
                for (id, sprite) in view_atlases.sprites(atlas, &cx.run.interner) {
                    if model_atlases.iter().any(|model| model == atlas)
                        && ResourceId::parse(&cx.run.interner, &id)
                            .is_some_and(|id| named.contains_key(&id))
                    {
                        continue;
                    }
                    for input in &sprite.inputs {
                        let Ok(logical) = LogicalPath::new(&input.logical) else {
                            continue;
                        };
                        if view.files.contains_key(&logical) {
                            blocked.entry(logical).or_insert_with(|| {
                                format!("stitched as {id} in {atlas}, which no model face draws")
                            });
                        }
                    }
                }
            }
        }
        let mut result = Self {
            candidates: BTreeMap::new(),
            skipped: BTreeMap::new(),
        };
        for (physical, density) in faces {
            if !cx.scope.contains(&physical) {
                continue;
            }
            let logical = cx.layout.split(&physical).1.expect("texture logical path");
            let why = if cx.kept_names.contains(&logical) {
                Some("a name that stays (an item definition, font, vanilla path or what a usage source names)".into())
            } else if cx.vanilla.has(logical.as_str()) {
                Some("a vanilla path, which the client loads by name".into())
            } else {
                blocked.get(&logical).cloned()
            };
            if let Some(why) = why {
                result.skipped.insert(physical, why);
            } else {
                result.candidates.insert(physical, density);
            }
        }
        Ok(result)
    }
}

fn model(
    cx: &PassCx<'_>,
    view: &View,
    trees: &BTreeMap<PhysPath, Node>,
    leaf: &Node,
    named: &mut BTreeMap<ResourceId, Faces>,
    undrawn: &mut BTreeSet<ResourceId>,
) {
    let mut slots = BTreeMap::<String, String>::new();
    let mut elements = None;
    let mut opaque = false;
    let mut seen = BTreeSet::new();
    let mut node = Some(leaf);
    for depth in 0.. {
        let Some(current) = node else {
            break;
        };
        for slot in current.path("textures").members() {
            let value = if slot.node.is_object() {
                slot.node.path("sprite")
            } else {
                &slot.node
            };
            slots
                .entry(slot.name.to_string())
                .or_insert_with(|| value.as_string("").into_owned());
        }
        if elements.is_none() && current.path("elements").is_array() {
            elements = Some(current.path("elements"));
        }
        let parent = current.path("parent").as_string("");
        if parent.is_empty() {
            break;
        }
        let Some(id) = ResourceId::parse(&cx.run.interner, &parent).filter(|_| depth < 32) else {
            opaque = true;
            break;
        };
        let Some(logical) = resource_file(cx, id, "models", ".json") else {
            opaque = true;
            break;
        };
        node = None;
        if let Some(physical) = view.files.get(&logical)
            && seen.insert(logical)
        {
            node = trees.get(physical);
            opaque = node.is_none();
        } else if id.display(&cx.run.interner).to_string() != "minecraft:block/block" {
            opaque = true;
        }
    }
    if elements.is_none() && !opaque {
        return;
    }
    let mut reached = BTreeSet::new();
    if !opaque && let Some(elements) = elements {
        for element in elements.items() {
            for face in element.path("faces").members() {
                let texture = face.node.path("texture").as_string("");
                if let Some(sprite) = slot(cx, &texture, &slots, &mut reached)
                    && let Some(density) = density(element, &face.name, &face.node)
                {
                    named
                        .entry(sprite)
                        .and_modify(|old| *old = old.min(density))
                        .or_insert(density);
                }
            }
        }
    }
    for (name, value) in slots {
        if !value.starts_with('#')
            && !value.is_empty()
            && (opaque || !reached.contains(&name) && name != "particle")
            && let Some(id) = ResourceId::parse(&cx.run.interner, &value)
        {
            undrawn.insert(id);
        }
    }
}

fn slot(
    cx: &PassCx<'_>,
    texture: &str,
    slots: &BTreeMap<String, String>,
    reached: &mut BTreeSet<String>,
) -> Option<ResourceId> {
    let mut value = texture;
    for _ in 0..16 {
        let Some(name) = value.strip_prefix('#') else {
            return ResourceId::parse(&cx.run.interner, value);
        };
        reached.insert(name.to_owned());
        value = slots.get(name)?;
    }
    if value.starts_with('#') {
        None
    } else {
        ResourceId::parse(&cx.run.interner, value)
    }
}

fn point(node: &Node) -> Option<[f64; 3]> {
    if node.items().len() != 3 {
        return None;
    }
    Some(std::array::from_fn(|i| node.items()[i].as_double(0.0)))
}

fn density(element: &Node, direction: &str, face: &Node) -> Option<Faces> {
    let from = point(element.path("from"))?;
    let to = point(element.path("to"))?;
    let size: [f64; 3] = std::array::from_fn(|i| (to[i] - from[i]).abs());
    let (u, v) = match direction {
        "north" | "south" => (0, 1),
        "east" | "west" => (2, 1),
        "up" | "down" => (0, 2),
        _ => return None,
    };
    if size[u] <= 0.0 || size[v] <= 0.0 {
        return None;
    }
    let stretch = rescale(element.path("rotation"));
    let (mut span_u, mut span_v) = (size[u], size[v]);
    let uv = face.path("uv").items();
    if uv.len() == 4 {
        span_u = (uv[2].as_double(0.0) - uv[0].as_double(0.0)).abs();
        span_v = (uv[3].as_double(0.0) - uv[1].as_double(0.0)).abs();
    }
    let (length_u, length_v) = (size[u] * stretch[u], size[v] * stretch[v]);
    let turned = face.path("rotation").as_int(0) % 180 != 0;
    Some(Faces {
        u: span_u / 16.0 / if turned { length_v } else { length_u },
        v: span_v / 16.0 / if turned { length_u } else { length_v },
    })
}

fn rescale(rotation: &Node) -> [f64; 3] {
    if !rotation.path("rescale").as_bool(false) {
        return [1.0; 3];
    }
    let matrix = if rotation.has("axis") {
        let Some(axis) = "xyz".find(rotation.path("axis").as_str().unwrap_or("?")) else {
            return [1.0; 3];
        };
        if axis > 2 {
            return [1.0; 3];
        }
        turn(axis, rotation.path("angle").as_double(0.0))
    } else {
        multiply(
            turn(2, rotation.path("z").as_double(0.0)),
            multiply(
                turn(1, rotation.path("y").as_double(0.0)),
                turn(0, rotation.path("x").as_double(0.0)),
            ),
        )
    };
    std::array::from_fn(|axis| {
        1.0 / matrix[0][axis]
            .abs()
            .max(matrix[1][axis].abs())
            .max(matrix[2][axis].abs())
    })
}

fn turn(axis: usize, degrees: f64) -> [[f64; 3]; 3] {
    let (sin, cos) = degrees.to_radians().sin_cos();
    match axis {
        0 => [[1.0, 0.0, 0.0], [0.0, cos, -sin], [0.0, sin, cos]],
        1 => [[cos, 0.0, sin], [0.0, 1.0, 0.0], [-sin, 0.0, cos]],
        _ => [[cos, -sin, 0.0], [sin, cos, 0.0], [0.0, 0.0, 1.0]],
    }
}

fn multiply(a: [[f64; 3]; 3], b: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[k][j]).sum()))
}
