use std::collections::BTreeMap;

use packprune_pack::json::{self, Node, Value};
use packprune_pass::{Edits, Loss, Pass, PassCx, PassError, Stage};
use rayon::iter::{IntoParallelIterator, ParallelIterator};

use super::{Model, models};

pub struct IdenticalModels;

impl Pass for IdenticalModels {
    fn name(&self) -> &'static str {
        "identical-models"
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
        let mut holders = BTreeMap::new();
        for path in cx.pack.files.keys() {
            if let Some(logical) = cx.layout.split(path).1 {
                *holders.entry(logical).or_insert(0) += 1;
            }
        }
        let keyed: Vec<(String, Model<'_>)> = models(cx)
            .into_par_iter()
            .filter(|model| holders[&model.logical] == 1)
            .map(|model| (key(&model.json), model))
            .collect();
        let mut groups: BTreeMap<_, Vec<_>> = BTreeMap::new();
        for (key, model) in keyed {
            groups
                .entry((model.layer.clone(), key))
                .or_default()
                .push(model);
        }
        let mut edits = Edits::default();
        let (mut merged, mut kept, mut bytes) = (0, 0, 0);
        for group in groups.values() {
            if group.len() < 2 {
                continue;
            }
            let target = group
                .iter()
                .min_by_key(|model| {
                    let logical = model.logical.as_str();
                    (!stays(cx, model), logical.encode_utf16().count(), logical)
                })
                .expect("a group has at least two models");
            for model in group {
                if model.physical == target.physical {
                    continue;
                }
                if stays(cx, model) {
                    kept += 1;
                    continue;
                }
                edits.removed.insert(model.physical.clone());
                edits
                    .file_remaps
                    .insert(model.logical.clone(), target.logical.clone());
                bytes += cx.pack.files[model.physical].blob.len();
                merged += 1;
            }
        }
        edits.changes = vec![
            format!("{merged} identical models merged into another, {bytes} B of files removed"),
            format!("{kept} identical models left, their names stay"),
        ];
        Ok(edits)
    }
}

fn stays(cx: &PassCx<'_>, model: &Model<'_>) -> bool {
    cx.kept_names.contains(&model.logical) || cx.vanilla.has(model.logical.as_str())
}

/// A model's identity: its parsed value, object member order ignored, an integer distinct from a float of the same
/// value.
pub(super) fn key(node: &Node) -> String {
    match &node.value {
        Value::Object(members) => {
            let mut members: Vec<_> = members.iter().collect();
            members.sort_by(|a, b| a.name.cmp(&b.name));
            format!(
                "{{{}}}",
                members
                    .into_iter()
                    .map(|m| format!("{}:{}", json::quote(&m.name), key(&m.node)))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
        Value::Array(items) => format!("[{}]", items.iter().map(key).collect::<Vec<_>>().join(",")),
        Value::String(text) => json::quote(text),
        Value::Int(value) => format!("i{value}"),
        Value::Long(value) => format!("l{value}"),
        Value::Big(text) => format!("b{text}"),
        Value::Double(value) => format!(
            "d{:x}",
            if value.is_nan() {
                f64::NAN.to_bits()
            } else {
                value.to_bits()
            }
        ),
        Value::Bool(value) => value.to_string(),
        Value::Null => "null".into(),
        Value::Missing => "missing".into(),
    }
}
