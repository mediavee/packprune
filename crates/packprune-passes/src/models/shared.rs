use std::collections::{BTreeMap, BTreeSet};

use packprune_pack::json;
use packprune_pack::{LogicalPath, PhysPath};
use packprune_pass::{Edits, Loss, Pass, PassCx, PassError, Stage};
use sha2::{Digest, Sha256};

use super::{identical::key, models, text};

const MINIMUM: usize = 3;
const NAMESPACE: &str = "packprune";
const PARENTS: &str = "models/display/";
const DISPLAY_FIELD_LEN: usize = "\"display\":".len();

pub struct SharedDisplay;

impl Pass for SharedDisplay {
    fn name(&self) -> &'static str {
        "shared-display"
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
        let mut groups: BTreeMap<_, Vec<_>> = BTreeMap::new();
        for model in models(cx) {
            let display = model.json.path("display");
            if !model.json.has("parent") && display.is_object() && display.size() > 0 {
                groups
                    .entry((model.layer.clone(), key(display)))
                    .or_default()
                    .push(model);
            }
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
        for group in groups.values() {
            let first = &group[0];
            let block = String::from_utf8(json::write(first.json.path("display")))
                .expect("serialized JSON is UTF-8");
            let digest = Sha256::digest(block.as_bytes());
            let hash: String = digest[..8]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            let parent = format!("{NAMESPACE}:display/{hash}");
            let logical = LogicalPath::new(&format!("{parents_prefix}{hash}.json"))
                .expect("valid generated model path");
            let folder = &first.physical.as_str()
                [..first.physical.as_str().len() - first.logical.as_str().len()];
            let path = PhysPath::new(&format!("{folder}{logical}"))
                .expect("valid layer and generated model path");
            let mut field = Vec::new();
            let mut writer = json::Writer::new(&mut field);
            writer.name("parent");
            writer.string(&parent);
            let field = String::from_utf8(field).expect("serialized JSON is UTF-8");
            let file = format!("{{\"display\":{block}}}");
            let wanted = key(&json::parse(file.as_bytes()).expect("serialized display JSON"));
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
            let exists = held.contains(&&path);
            let saved = takers.len() as i64
                * (DISPLAY_FIELD_LEN as i64 + block.encode_utf16().count() as i64
                    - field.encode_utf16().count() as i64)
                - if exists {
                    0
                } else {
                    file.encode_utf16().count() as i64
                };
            if group.len() < MINIMUM || saved <= 0 {
                continue;
            }
            if !exists {
                edits.added.insert(path, file.into_bytes().into());
                parents += 1;
            }
            for model in &takers {
                let bytes = text::edit(
                    &cx.pack.files[model.physical].blob,
                    &BTreeSet::from(["/display".into()]),
                    Some(("parent", &parent)),
                );
                edits.replaced.insert(model.physical.clone(), bytes.into());
            }
            children += takers.len();
        }
        edits.changes = vec![
            format!("{parents} display parents added"),
            format!("{children} models now take their display from one"),
        ];
        Ok(edits)
    }
}
