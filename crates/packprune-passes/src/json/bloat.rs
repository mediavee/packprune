use std::ops::Range;

use packprune_pack::FileKind;
use packprune_pack::json::{self, Splice, Value};
use packprune_pass::{Edits, Loss, Pass, PassCx, PassError, Stage};

use super::{kind, no_options};

/// Root keys of a model no client reads: `CuboidModel$Deserializer` (26.2) asks the root object only for `parent`,
/// `elements`, `textures`, `ambientocclusion`, `display` and `gui_light`, older clients also for `overrides`. Of
/// what Blockbench's Java model export writes, `credit` and `groups` are its own; `__comment` is the comment key of
/// the vanilla models. Other unread keys may be a mod's (`render_type`) and stay.
const KEYS: [&str; 3] = ["credit", "groups", "__comment"];

/// Removes bloat keys from models, at their root only: deeper, a key is a texture slot or a
/// face direction to the client, so removing it is not provably invisible.
pub(crate) struct BloatKeys;

impl Pass for BloatKeys {
    fn name(&self) -> &'static str {
        "bloat-keys"
    }

    fn stage(&self) -> Stage {
        Stage::Json
    }

    fn loss(&self) -> Loss {
        Loss::Lossless
    }

    fn options_schema(&self) -> schemars::Schema {
        no_options()
    }

    fn run(&self, cx: &PassCx<'_>) -> Result<Edits, PassError> {
        let mut edits = Edits::default();
        let mut keys = 0;
        for (path, entry) in &cx.pack.files {
            if kind(cx, path) != FileKind::Model {
                continue;
            }
            if let Some((debloated, removed)) = debloat(&entry.blob) {
                keys += removed;
                edits.replaced.insert(path.clone(), debloated.into());
            }
        }
        edits.changes.push(format!(
            "{keys} keys removed from {} models",
            edits.replaced.len()
        ));
        Ok(edits)
    }
}

/// `json` without its root's bloat keys, every other byte kept, and how many it removed; `None` when it has none or
/// is not one strict JSON object whose members can be cut out.
pub(crate) fn debloat(json: &[u8]) -> Option<(Vec<u8>, usize)> {
    let root = json::parse(json).ok()?;
    let Value::Object(members) = &root.value else {
        return None;
    };
    let spans: Vec<Range<usize>> = members
        .iter()
        .map(|member| member.name_span.start as usize..member.node.span.end as usize)
        .collect();
    // A repeated name is one member spanning its first name to its last value: between them is more than a colon.
    let whole = members.iter().all(|member| {
        json[member.name_span.end as usize..member.node.span.start as usize]
            .trim_ascii()
            .eq(b":")
    });
    let bloat: Vec<bool> = members
        .iter()
        .map(|member| KEYS.contains(&&*member.name))
        .collect();
    let removed = bloat.iter().filter(|&&bloat| bloat).count();
    if removed == 0 || !whole {
        return None;
    }
    let mut splices = Vec::new();
    // Up to the next member's name for a member followed by a kept one; from the end of the last kept member for the
    // trailing run, so its comma goes with it.
    let last_kept = bloat.iter().rposition(|&bloat| !bloat);
    for (index, span) in spans.iter().enumerate() {
        if !bloat[index] {
            continue;
        }
        let range = match last_kept {
            Some(kept) if index < kept => span.start..spans[index + 1].start,
            Some(kept) if index == kept + 1 => spans[kept].end..spans.last().expect("a member").end,
            Some(_) => continue,
            None if index == 0 => span.start..spans.last().expect("a member").end,
            None => continue,
        };
        splices.push(Splice {
            range,
            bytes: Vec::new(),
        });
    }
    Some((json::splice(json, splices), removed))
}
