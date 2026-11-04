use std::collections::BTreeSet;

use packprune_pack::json::Node;
use packprune_pass::{Edits, Loss, Pass, PassCx, PassError, Stage};

use super::{models, text};

pub struct DegenerateFaces;

impl Pass for DegenerateFaces {
    fn name(&self) -> &'static str {
        "degenerate-faces"
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
        let mut edits = Edits::default();
        let (mut faces, mut elements, mut files) = (0, 0, 0);
        for model in models(cx) {
            let mut removed = BTreeSet::new();
            for (i, element) in model.json.path("elements").items().iter().enumerate() {
                let Some(from) = point(element.path("from")) else {
                    continue;
                };
                let Some(to) = point(element.path("to")) else {
                    continue;
                };
                let drawn = element.path("faces");
                let (x, y, z) = (from[0] != to[0], from[1] != to[1], from[2] != to[2]);
                let present: Vec<_> = ["down", "up", "north", "south", "west", "east"]
                    .into_iter()
                    .filter(|direction| drawn.path(direction).is_object())
                    .collect();
                let flat: Vec<_> = present
                    .iter()
                    .filter(|direction| !match **direction {
                        "west" | "east" => y && z,
                        "down" | "up" => x && z,
                        _ => x && y,
                    })
                    .collect();
                if flat.is_empty() {
                    continue;
                }
                let pointer = format!("/elements/{i}");
                if flat.len() == present.len() {
                    removed.insert(pointer);
                    elements += 1;
                } else {
                    for direction in &flat {
                        removed.insert(format!("{pointer}/faces/{direction}"));
                    }
                }
                faces += flat.len();
            }
            if !removed.is_empty() {
                let bytes = text::remove(&cx.pack.files[model.physical].blob, &removed);
                edits.replaced.insert(model.physical.clone(), bytes.into());
                files += 1;
            }
        }
        edits.changes = vec![
            format!("{faces} zero-area faces dropped"),
            format!("{elements} elements left without a face removed"),
            format!("{files} models rewritten"),
        ];
        Ok(edits)
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
