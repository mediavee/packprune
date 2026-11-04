//! Usages as they cross runs in the memo: with their ids as text, interned again on load (`packprune-cache`).

use packprune_graph::{IdGlob, Usage, UsageKind};
use packprune_pack::{ResourceId, RunCx};
use serde_json::{Value, json};

const KINDS: [UsageKind; 9] = [
    UsageKind::ItemDefinition,
    UsageKind::Model,
    UsageKind::Sprite,
    UsageKind::Font,
    UsageKind::SoundEvent,
    UsageKind::Equipment,
    UsageKind::TooltipStyle,
    UsageKind::Path,
    UsageKind::Namespace,
];

pub(crate) fn encode(usages: &[Usage], cx: &RunCx) -> Vec<u8> {
    let interner = &cx.interner;
    let usages: Vec<Value> = usages
        .iter()
        .map(|usage| {
            json!([
                KINDS
                    .iter()
                    .position(|kind| *kind == usage.kind)
                    .expect("every kind is listed"),
                &*usage.id.0,
                usage
                    .atlas
                    .map(|atlas| [interner.resolve(atlas.ns), interner.resolve(atlas.path)]),
                usage.location,
                usage.reason,
                usage.declares,
                usage.quiet,
            ])
        })
        .collect();
    serde_json::to_vec(&usages).expect("JSON values serialise")
}

/// `None` for bytes [`encode`] did not write: the memo treats it as a miss.
pub(crate) fn decode(bytes: &[u8], cx: &RunCx) -> Option<Vec<Usage>> {
    let interner = &cx.interner;
    let text = |value: &Value| value.as_str().map(str::to_owned);
    let optional = |value: &Value| match value {
        Value::Null => Some(None),
        value => text(value).map(Some),
    };
    let usages: Vec<Value> = serde_json::from_slice(bytes).ok()?;
    usages
        .iter()
        .map(|usage| {
            let [kind, id, atlas, location, reason, declares, quiet] = usage.as_array()?.as_slice()
            else {
                return None;
            };
            let atlas = match atlas {
                Value::Null => None,
                atlas => match atlas.as_array()?.as_slice() {
                    [ns, path] => Some(ResourceId {
                        ns: interner.intern(ns.as_str()?),
                        path: interner.intern(path.as_str()?),
                    }),
                    _ => return None,
                },
            };
            Some(Usage {
                kind: *KINDS.get(usize::try_from(kind.as_u64()?).ok()?)?,
                id: IdGlob(id.as_str()?.into()),
                atlas,
                location: text(location)?,
                reason: optional(reason)?,
                declares: optional(declares)?,
                quiet: quiet.as_bool()?,
            })
        })
        .collect()
}
