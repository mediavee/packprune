use std::collections::BTreeSet;

use packprune_pack::json::{self, Node};
use packprune_pack::{FileKind, Interner, PhysPath, ResourceId};
use packprune_pass::{Edits, Loss, Pass, PassCx, PassError, Stage};
use rayon::prelude::*;

use super::{models, text};

/// Removes Blockbench/modern zero-angle element `rotation` objects (both single-axis `angle == 0` and 26.2 Euler
/// `x == 0 && y == 0 && z == 0`, no `rescale`) when exact f32 emulation of `FaceBakery.bakeVertex` confirms
/// `(v/16 - o/16) + o/16 == v/16` on every corner and no blockstate applies a model rotation to the model (so
/// `recalculateWinding` is invariant). Also strips redundant `"rescale": false` and redundant zero axes (`"x": 0`,
/// `"y": 0`, `"z": 0`) inside non-zero Euler rotations (where `CuboidModelElement$Deserializer` defaults missing axes
/// to `0.0f` as long as at least one of `x`, `y`, `z` remains).
pub struct IdentityRotations;

impl Pass for IdentityRotations {
    fn name(&self) -> &'static str {
        "identity-rotations"
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
        let rotated_by_blockstate = blockstate_rotated_models(cx);
        let all_models = models(cx);
        let rewritten: Vec<(PhysPath, usize, usize, Vec<u8>)> = all_models
            .par_iter()
            .filter_map(|model| {
                let logical = model.logical.as_str();
                // Vanilla blockstates are not all read here, so skip vanilla block model paths conservatively
                // for whole-rotation removal (individual default fields are always safe).
                let blockstate_protected = rotated_by_blockstate.contains(logical)
                    || (cx.vanilla.has(logical)
                        && logical.starts_with("assets/minecraft/models/block/"));
                let mut removed = BTreeSet::new();
                let mut whole_dropped = 0usize;
                let mut fields_dropped = 0usize;
                for (i, element) in model.json.path("elements").items().iter().enumerate() {
                    let Some(rotation) = element.get("rotation") else {
                        continue;
                    };
                    if !rotation.is_object() {
                        continue;
                    }
                    if !blockstate_protected
                        && let Some(from) = point(element.path("from"))
                        && let Some(to) = point(element.path("to"))
                        && is_exact_identity(rotation, &from, &to)
                    {
                        removed.insert(format!("/elements/{i}/rotation"));
                        whole_dropped += 1;
                        continue;
                    }
                    // Strip redundant `"rescale": false` (default in every Minecraft version).
                    if rotation.get("rescale").is_some_and(|n| !n.as_bool(true)) {
                        removed.insert(format!("/elements/{i}/rotation/rescale"));
                        fields_dropped += 1;
                    }
                    // Strip zero Euler axes when at least one non-zero Euler axis remains.
                    let single_axis = rotation.has("axis") || rotation.has("angle");
                    if !single_axis {
                        let mut zero_axes = Vec::new();
                        let mut non_zero = false;
                        let mut valid = true;
                        for axis in ["x", "y", "z"] {
                            if let Some(node) = rotation.get(axis) {
                                let Some(v) = node.double_value() else {
                                    valid = false;
                                    break;
                                };
                                if (v as f32) == 0.0 {
                                    zero_axes.push(axis);
                                } else {
                                    non_zero = true;
                                }
                            }
                        }
                        if valid && non_zero {
                            for axis in zero_axes {
                                removed.insert(format!("/elements/{i}/rotation/{axis}"));
                                fields_dropped += 1;
                            }
                        }
                    }
                }
                if removed.is_empty() {
                    return None;
                }
                let bytes = text::remove(&cx.pack.files[model.physical].blob, &removed);
                Some((model.physical.clone(), whole_dropped, fields_dropped, bytes))
            })
            .collect();
        let mut edits = Edits::default();
        let (mut dropped, mut fields) = (0usize, 0usize);
        let files = rewritten.len();
        for (physical, whole, extra, bytes) in rewritten {
            dropped += whole;
            fields += extra;
            edits.replaced.insert(physical, bytes.into());
        }
        edits.changes = vec![
            format!("{dropped} zero-angle rotations dropped"),
            format!("{fields} default rotation fields dropped"),
            format!("{files} models rewritten"),
        ];
        Ok(edits)
    }
}

fn is_exact_identity(rotation: &Node, from: &[f32; 3], to: &[f32; 3]) -> bool {
    if !rotation.is_object() {
        return false;
    }
    if rotation.path("rescale").as_bool(false) {
        return false;
    }
    let single_axis = rotation.has("axis") || rotation.has("angle");
    if single_axis {
        let Some(angle) = rotation.get("angle").and_then(Node::double_value) else {
            return false;
        };
        if (angle as f32) != 0.0 {
            return false;
        }
        let Some(axis) = rotation.get("axis").and_then(Node::as_str) else {
            return false;
        };
        if !matches!(axis.to_ascii_lowercase().as_str(), "x" | "y" | "z") {
            return false;
        }
    } else {
        let mut any_axis = false;
        for axis in ["x", "y", "z"] {
            if let Some(node) = rotation.get(axis) {
                let Some(v) = node.double_value() else {
                    return false;
                };
                if (v as f32) != 0.0 {
                    return false;
                }
                any_axis = true;
            }
        }
        if !any_axis {
            return false;
        }
    }
    let origin = rotation.path("origin");
    if !origin.is_array() || origin.size() != 3 {
        return false;
    }
    for i in 0..3 {
        let Some(o_raw) = origin.item(i).and_then(Node::double_value) else {
            return false;
        };
        let o = (o_raw as f32) / 16.0;
        for v_raw in [from[i], to[i]] {
            let v = v_raw / 16.0;
            if (v - o) + o != v {
                return false;
            }
        }
    }
    true
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

/// Every model logical path referenced by any blockstate with a non-zero `x` or `y` rotation, plus every model in its
/// parent chain (conservatively collected across all pack blockstates).
fn blockstate_rotated_models(cx: &PassCx<'_>) -> BTreeSet<String> {
    let mut rotated = BTreeSet::new();
    let interner = &cx.run.interner;
    for (physical, entry) in &cx.pack.files {
        if cx.layout.kinds.get(physical) != Some(&FileKind::Blockstate) {
            continue;
        }
        if let Ok(root) = json::parse(&entry.blob) {
            collect_rotated(&root, interner, &mut rotated);
        }
    }
    // Propagate up parent chains so a rotated child protects any parent defining its elements.
    let mut parents: BTreeSet<String> = rotated.clone();
    let mut frontier: Vec<String> = rotated.into_iter().collect();
    while let Some(logical) = frontier.pop() {
        for (physical, entry) in &cx.pack.files {
            if cx.layout.kinds.get(physical) != Some(&FileKind::Model) {
                continue;
            }
            if cx.layout.split(physical).1.as_ref().map(|l| l.as_str()) != Some(logical.as_str()) {
                continue;
            }
            if let Ok(root) = json::parse(&entry.blob)
                && let Some(parent) = root.path("parent").as_str()
                && let Some(id) = ResourceId::parse(interner, parent)
            {
                let parent_logical = format!(
                    "assets/{}/models/{}.json",
                    interner.resolve(id.ns),
                    interner.resolve(id.path)
                );
                if parents.insert(parent_logical.clone()) {
                    frontier.push(parent_logical);
                }
            }
        }
    }
    parents
}

fn collect_rotated(node: &Node, interner: &Interner, out: &mut BTreeSet<String>) {
    if node.is_array() {
        for item in node.items() {
            collect_rotated(item, interner, out);
        }
        return;
    }
    if !node.is_object() {
        return;
    }
    if let Some(model) = node.path("model").as_str() {
        let rx = node.path("x").as_int(0) % 360;
        let ry = node.path("y").as_int(0) % 360;
        if (rx != 0 || ry != 0)
            && let Some(id) = ResourceId::parse(interner, model)
        {
            out.insert(format!(
                "assets/{}/models/{}.json",
                interner.resolve(id.ns),
                interner.resolve(id.path)
            ));
        }
    }
    for member in node.members() {
        collect_rotated(&member.node, interner, out);
    }
}
