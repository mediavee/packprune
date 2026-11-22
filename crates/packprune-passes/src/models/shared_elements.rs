use std::collections::{BTreeMap, BTreeSet};

use packprune_pack::json;
use packprune_pack::{LogicalPath, PhysPath};
use packprune_pass::{Edits, Loss, Pass, PassCx, PassError, Stage};
use rayon::prelude::*;
use sha2::{Digest, Sha256};

use super::{identical::key, models, text};

const MINIMUM: usize = 2;
const NAMESPACE: &str = "packprune";
const PARENTS: &str = "models/e/";
const ELEMENTS_FIELD_LEN: usize = "\"elements\":".len();

fn base36_hash(digest: &[u8]) -> String {
    let mut n = u64::from_be_bytes(digest[..8].try_into().expect("8 bytes"));
    let mut out = [b'0'; 10];
    for slot in out.iter_mut().rev() {
        *slot = b"0123456789abcdefghijklmnopqrstuvwxyz"[(n % 36) as usize];
        n /= 36;
    }
    String::from_utf8(out.to_vec()).expect("ASCII")
}

/// Factors repeated `elements` arrays across models into shared generated parent models (`packprune:e/<hash>`),
/// chaining any existing `parent` cleanly (`child -> shared -> old_parent`).
pub struct SharedElements;

impl Pass for SharedElements {
    fn name(&self) -> &'static str {
        "shared-elements"
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
        let all_models = models(cx);
        let keyed: Vec<_> = all_models
            .into_par_iter()
            .filter_map(|model| {
                let elements = model.json.path("elements");
                if !elements.is_array() || elements.size() == 0 {
                    return None;
                }
                // Skip models whose parent is missing/non-string when present, or a builtin model.
                let parent = match model.json.get("parent") {
                    None => None,
                    Some(node) => {
                        let s = node.as_str()?;
                        if s.is_empty()
                            || s.starts_with("builtin/")
                            || s.starts_with("minecraft:builtin/")
                        {
                            return None;
                        }
                        Some(s.to_owned())
                    }
                };
                let elem_key = key(elements);
                Some((model.layer.clone(), parent, elem_key, model))
            })
            .collect();
        let mut groups: BTreeMap<_, Vec<_>> = BTreeMap::new();
        for (layer, parent, elem_key, model) in keyed {
            groups
                .entry((layer, parent, elem_key))
                .or_default()
                .push(model);
        }

        let parents_prefix = format!("assets/{NAMESPACE}/{PARENTS}");
        let mut holders: BTreeMap<LogicalPath, Vec<_>> = BTreeMap::new();
        for physical in cx.pack.files.keys() {
            if let Some(logical) = cx.layout.split(physical).1
                && logical.as_str().starts_with(&parents_prefix)
            {
                holders.entry(logical).or_default().push(physical);
            }
        }

        let mut edits = Edits::default();
        let (mut parents, mut children) = (0, 0);
        for ((_, existing_parent, _), group) in groups {
            if group.len() < MINIMUM {
                continue;
            }
            let first = &group[0];
            let block = String::from_utf8(json::write(first.json.path("elements")))
                .expect("serialized JSON is UTF-8");
            let file = match &existing_parent {
                Some(old_parent) => {
                    let mut pbuf = Vec::new();
                    let mut w = json::Writer::new(&mut pbuf);
                    w.string(old_parent);
                    let quoted = String::from_utf8(pbuf).expect("UTF-8");
                    format!("{{\"parent\":{quoted},\"elements\":{block}}}")
                }
                None => format!("{{\"elements\":{block}}}"),
            };
            let digest = Sha256::digest(file.as_bytes());
            let hash = base36_hash(&digest);
            let parent_id = format!("{NAMESPACE}:e/{hash}");
            let logical = LogicalPath::new(&format!("{parents_prefix}{hash}.json"))
                .expect("valid generated model path");
            let folder = &first.physical.as_str()
                [..first.physical.as_str().len() - first.logical.as_str().len()];
            let path = PhysPath::new(&format!("{folder}{logical}"))
                .expect("valid layer and generated model path");

            let wanted = key(&json::parse(file.as_bytes()).expect("serialized elements JSON"));
            let held = holders.get(&logical).map_or(&[][..], Vec::as_slice);
            if held.iter().any(|physical| {
                json::parse(&cx.pack.files[*physical].blob)
                    .map_or(true, |json| key(&json) != wanted)
            }) {
                continue;
            }

            let takers: Vec<_> = group
                .iter()
                .filter(|model| *model.physical != path)
                .collect();
            if takers.len() < MINIMUM {
                continue;
            }
            let exists = held.contains(&&path);
            let new_parent_field_len = "\"parent\":\"\"".len() + parent_id.len();
            let old_parent_field_len = existing_parent
                .as_ref()
                .map_or(0, |p| "\"parent\":\"\"".len() + p.len());
            // Account for the zip central/local header overhead (~110 bytes) of adding a new file so we only factor
            // when it genuinely shrinks the archive.
            let overhead = if exists { 0 } else { file.len() as i64 + 110 };
            let per_child =
                ELEMENTS_FIELD_LEN as i64 + block.len() as i64 + old_parent_field_len as i64
                    - new_parent_field_len as i64;
            let saved = takers.len() as i64 * per_child - overhead;
            if saved <= 0 {
                continue;
            }

            if !exists {
                edits.added.insert(path, file.into_bytes().into());
                parents += 1;
            }
            let mut drop_fields = BTreeSet::from(["/elements".to_owned()]);
            if existing_parent.is_some() {
                drop_fields.insert("/parent".to_owned());
            }
            let rewritten: Vec<(PhysPath, Vec<u8>)> = takers
                .par_iter()
                .map(|model| {
                    let bytes = text::edit(
                        &cx.pack.files[model.physical].blob,
                        &drop_fields,
                        Some(("parent", &parent_id)),
                    );
                    (model.physical.clone(), bytes)
                })
                .collect();
            for (physical, bytes) in rewritten {
                edits.replaced.insert(physical, bytes.into());
            }
            children += takers.len();
        }
        edits.changes = vec![
            format!("{parents} elements parents added"),
            format!("{children} models now take their elements from one"),
        ];
        Ok(edits)
    }
}
