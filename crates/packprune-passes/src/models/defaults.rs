use std::collections::BTreeSet;

use packprune_pack::json::{self, Node, Value};
use packprune_pack::{FileKind, PhysPath, ResourceId};
use packprune_pass::{Edits, Loss, Pass, PassCx, PassError, Stage};
use rayon::prelude::*;

use super::text;

const DIRECTIONS: [&str; 6] = ["down", "up", "north", "south", "west", "east"];
const DISPLAY_CONTEXTS: [&str; 9] = [
    "thirdperson_righthand",
    "thirdperson_lefthand",
    "firstperson_righthand",
    "firstperson_lefthand",
    "head",
    "gui",
    "ground",
    "fixed",
    "on_shelf",
];

/// Removes provably redundant default values and Blockbench editor metadata from models and item definitions:
/// - In model `elements[]`: editor-only `"name"` and `"color"` keys, default `"shade": true`, `"light_emission": 0`.
/// - In model `elements[].faces.*`: default `"rotation": 0`, `"tintindex": -1`, and `"uv"` when bit-identical in `f32`
///   to `FaceBakery.defaultFaceUV(dir, from, to)`.
/// - In model `display.*`: default `"rotation": [0,0,0]`, `"translation": [0,0,0]`, `"scale": [1,1,1]`, and when the
///   model has no `parent`, `*_lefthand` transforms identical to their `*_righthand` counterpart (`ItemTransforms`
///   copies `righthand` automatically when `lefthand` is absent).
/// - In `items/*.json`: empty `"tints": []` on `type: "model"` (`CuboidItemModelWrapper$Unbaked` defaults to empty).
///
/// Runs in `Stage::Models` before `shared-display` and `shared-elements` so stripping defaults also exposes more
/// identical `display` and `elements` blocks to share.
pub struct ModelDefaults;

impl Pass for ModelDefaults {
    fn name(&self) -> &'static str {
        "model-defaults"
    }

    fn stage(&self) -> Stage {
        Stage::Models
    }

    fn loss(&self) -> Loss {
        Loss::Lossless
    }

    fn options_schema(&self) -> schemars::Schema {
        schemars::json_schema!({"type": "object", "additionalProperties": false})
    }

    fn run(&self, cx: &PassCx<'_>) -> Result<Edits, PassError> {
        // Collect every model logical path referenced as a `parent` by any model in the pack.
        // We never drop `*_lefthand == *_righthand` on a parent model (nor a vanilla model), because a child could
        // override `*_righthand` and still rely on inheriting the parent's explicit `*_lefthand`.
        let referenced_as_parent: BTreeSet<String> = cx
            .pack
            .files
            .par_iter()
            .filter_map(|(physical, entry)| {
                if cx.layout.kinds.get(physical) != Some(&FileKind::Model) {
                    return None;
                }
                let root = json::parse(&entry.blob).ok()?;
                let parent = root.path("parent").as_str()?;
                let id = ResourceId::parse(&cx.run.interner, parent)?;
                Some(format!(
                    "assets/{}/models/{}.json",
                    cx.run.interner.resolve(id.ns),
                    cx.run.interner.resolve(id.path)
                ))
            })
            .collect();

        let rewritten: Vec<(PhysPath, usize, Vec<u8>)> = cx
            .pack
            .files
            .par_iter()
            .filter_map(|(physical, entry)| {
                let kind = cx.layout.kinds.get(physical).copied()?;
                let root = match kind {
                    FileKind::Model | FileKind::ItemDefinition => json::parse(&entry.blob).ok()?,
                    _ => return None,
                };
                if !root.is_object() {
                    return None;
                }
                let mut removed = BTreeSet::new();
                match kind {
                    FileKind::Model => {
                        let is_leaf = cx.layout.split(physical).1.as_ref().is_some_and(|logical| {
                            !referenced_as_parent.contains(logical.as_str())
                                && !cx.vanilla.has(logical.as_str())
                        });
                        collect_model_defaults(&root, is_leaf, &mut removed);
                    }
                    FileKind::ItemDefinition => collect_item_defaults(&root, "", &mut removed),
                    _ => {}
                }
                if removed.is_empty() {
                    return None;
                }
                let count = removed.len();
                let bytes = text::remove(&entry.blob, &removed);
                (bytes != entry.blob.as_ref()).then_some((physical.clone(), count, bytes))
            })
            .collect();

        let mut edits = Edits::default();
        let mut fields = 0usize;
        let files = rewritten.len();
        for (physical, count, bytes) in rewritten {
            fields += count;
            edits.replaced.insert(physical, bytes.into());
        }
        edits.changes = vec![
            format!("{fields} default/editor fields dropped"),
            format!("{files} files rewritten"),
        ];
        Ok(edits)
    }
}

fn collect_model_defaults(root: &Node, is_leaf: bool, removed: &mut BTreeSet<String>) {
    if let Some(elements) = root.get("elements")
        && elements.is_array()
    {
        for (i, elem) in elements.items().iter().enumerate() {
            if !elem.is_object() {
                continue;
            }
            // Blockbench editor metadata: `CuboidModelElement$Deserializer` never reads `name` or `color`.
            if elem.has("name") {
                removed.insert(format!("/elements/{i}/name"));
            }
            if elem.has("color") {
                removed.insert(format!("/elements/{i}/color"));
            }
            if matches!(elem.get("shade").map(|n| &n.value), Some(Value::Bool(true))) {
                removed.insert(format!("/elements/{i}/shade"));
            }
            if let Some(le) = elem.get("light_emission")
                && is_exact_int(le, 0)
            {
                removed.insert(format!("/elements/{i}/light_emission"));
            }
            let from = point(elem.path("from"));
            let to = point(elem.path("to"));
            if let Some(faces) = elem.get("faces")
                && faces.is_object()
            {
                for dir in DIRECTIONS {
                    let Some(face) = faces.get(dir) else {
                        continue;
                    };
                    if !face.is_object() {
                        continue;
                    }
                    if let Some(rot) = face.get("rotation")
                        && is_exact_int(rot, 0)
                    {
                        removed.insert(format!("/elements/{i}/faces/{dir}/rotation"));
                    }
                    if let Some(tint) = face.get("tintindex")
                        && is_exact_int(tint, -1)
                    {
                        removed.insert(format!("/elements/{i}/faces/{dir}/tintindex"));
                    }
                    if let (Some(from), Some(to), Some(uv)) = (from, to, face.get("uv"))
                        && is_unit_cube_bounds(from, to)
                        && let Some(actual) = uv4(uv)
                        && same_f32_slice(&actual, &default_uvs(dir, from, to))
                    {
                        removed.insert(format!("/elements/{i}/faces/{dir}/uv"));
                    }
                }
            }
        }
    }

    if let Some(display) = root.get("display")
        && display.is_object()
    {
        let mut dropped_contexts = BTreeSet::new();
        // When a model is a true leaf with no `parent`, `ItemTransforms$Deserializer` copies `righthand` to missing
        // `lefthand`. Parent models keep explicit `lefthand` so children overriding only `righthand` stay unchanged.
        if is_leaf && !root.has("parent") {
            for (left, right) in [
                ("thirdperson_lefthand", "thirdperson_righthand"),
                ("firstperson_lefthand", "firstperson_righthand"),
            ] {
                if let (Some(l), Some(r)) = (display.get(left), display.get(right))
                    && l.is_object()
                    && r.is_object()
                    && resolved_transform(l) == resolved_transform(r)
                {
                    removed.insert(format!("/display/{left}"));
                    dropped_contexts.insert(left);
                }
            }
        }
        for ctx in DISPLAY_CONTEXTS {
            if dropped_contexts.contains(ctx) {
                continue;
            }
            let Some(t) = display.get(ctx) else {
                continue;
            };
            if !t.is_object() {
                continue;
            }
            if let Some(rot) = t.get("rotation")
                && is_vec3_value(rot, 0.0)
            {
                removed.insert(format!("/display/{ctx}/rotation"));
            }
            if let Some(trans) = t.get("translation")
                && is_vec3_value(trans, 0.0)
            {
                removed.insert(format!("/display/{ctx}/translation"));
            }
            if let Some(scale) = t.get("scale")
                && is_vec3_value(scale, 1.0)
            {
                removed.insert(format!("/display/{ctx}/scale"));
            }
        }
    }
}

fn collect_item_defaults(node: &Node, pointer: &str, removed: &mut BTreeSet<String>) {
    match &node.value {
        Value::Object(members) => {
            let is_model_type = node
                .get("type")
                .and_then(Node::as_str)
                .is_some_and(|t| matches!(t, "model" | "minecraft:model"));
            for member in members {
                let escaped = escape_pointer(&member.name);
                let child_ptr = format!("{pointer}/{escaped}");
                if is_model_type
                    && &*member.name == "tints"
                    && member.node.is_array()
                    && member.node.size() == 0
                {
                    removed.insert(child_ptr);
                    continue;
                }
                collect_item_defaults(&member.node, &child_ptr, removed);
            }
        }
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                collect_item_defaults(item, &format!("{pointer}/{i}"), removed);
            }
        }
        _ => {}
    }
}

fn escape_pointer(segment: &str) -> String {
    if !segment.contains('~') && !segment.contains('/') {
        return segment.to_owned();
    }
    segment.replace('~', "~0").replace('/', "~1")
}

fn is_exact_int(node: &Node, expected: i64) -> bool {
    match &node.value {
        Value::Int(v) => i64::from(*v) == expected,
        Value::Long(v) => *v == expected,
        _ => false,
    }
}

fn point(node: &Node) -> Option<[f32; 3]> {
    let items = node.items();
    if items.len() != 3 {
        return None;
    }
    Some([
        items[0].double_value()? as f32,
        items[1].double_value()? as f32,
        items[2].double_value()? as f32,
    ])
}

fn uv4(node: &Node) -> Option<[f32; 4]> {
    let items = node.items();
    if items.len() != 4 {
        return None;
    }
    Some([
        items[0].double_value()? as f32,
        items[1].double_value()? as f32,
        items[2].double_value()? as f32,
        items[3].double_value()? as f32,
    ])
}

fn is_unit_cube_bounds(from: [f32; 3], to: [f32; 3]) -> bool {
    (0..3).all(|i| {
        from[i].is_finite()
            && to[i].is_finite()
            && from[i] >= 0.0
            && from[i] <= to[i]
            && to[i] <= 16.0
    })
}

fn default_uvs(dir: &str, from: [f32; 3], to: [f32; 3]) -> [f32; 4] {
    match dir {
        "down" => [from[0], 16.0 - to[2], to[0], 16.0 - from[2]],
        "up" => [from[0], from[2], to[0], to[2]],
        "north" => [16.0 - to[0], 16.0 - to[1], 16.0 - from[0], 16.0 - from[1]],
        "south" => [from[0], 16.0 - to[1], to[0], 16.0 - from[1]],
        "west" => [from[2], 16.0 - to[1], to[2], 16.0 - from[1]],
        _ => [16.0 - to[2], 16.0 - to[1], 16.0 - from[2], 16.0 - from[1]],
    }
}

fn same_f32_slice(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(&x, &y)| {
            // Canonicalize -0.0 and +0.0 as equal, exact bits otherwise.
            (x == 0.0 && y == 0.0) || x.to_bits() == y.to_bits()
        })
}

fn is_vec3_value(node: &Node, target: f32) -> bool {
    let Some(v) = point(node) else {
        return false;
    };
    same_f32_slice(&v, &[target, target, target])
}

fn resolved_transform(node: &Node) -> ([u32; 3], [u32; 3], [u32; 3]) {
    let vec = |field: &str, absent: f32, scale: f32, clamp: f32| -> [u32; 3] {
        let array = node.path(field);
        let mut out = [0u32; 3];
        for (i, slot) in out.iter_mut().enumerate() {
            let val = if array.is_array() && array.size() == 3 {
                array.item(i).map_or(0.0, Node::float_value) * scale
            } else {
                absent
            };
            let clamped = val.clamp(-clamp, clamp);
            *slot = if clamped == 0.0 { 0 } else { clamped.to_bits() };
        }
        out
    };
    (
        vec("rotation", 0.0, 1.0, f32::INFINITY),
        vec("translation", 0.0, 0.0625, 5.0),
        vec("scale", 1.0, 1.0, 4.0),
    )
}
