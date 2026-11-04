use std::collections::BTreeMap;
use std::ops::Range;

use packprune_pack::json::{self, Node, Splice, TokenKind, Value};
use packprune_pack::{FileKind, PhysPath, ResourceId};
use packprune_pass::{Edits, Loss, LossRecord, Pass, PassCx, PassError, Stage};
use schemars::JsonSchema;
use serde::Deserialize;

use super::ogg::Ogg;

pub(super) struct StreamSounds;

#[derive(Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct Options {
    seconds: f64,
}

impl Default for Options {
    fn default() -> Self {
        Self { seconds: 5.0 }
    }
}

impl Options {
    fn read(options: &toml::Table) -> Result<Self, String> {
        let options: Self = options
            .clone()
            .try_into()
            .map_err(|error: toml::de::Error| error.to_string())?;
        if !options.seconds.is_finite() || options.seconds < 0.0 {
            return Err("seconds must be finite and non-negative".into());
        }
        Ok(options)
    }
}

impl Pass for StreamSounds {
    fn name(&self) -> &'static str {
        "stream-sounds"
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

    fn validate_options(&self, options: &toml::Table) -> Result<(), String> {
        Options::read(options).map(drop)
    }

    fn run(&self, cx: &PassCx<'_>) -> Result<Edits, PassError> {
        let options = Options::read(cx.options).map_err(|why| PassError::Refused {
            pass: self.name(),
            why,
        })?;
        let mut edits = Edits::default();
        let mut metadata = BTreeMap::new();
        for (physical, file) in &cx.pack.files {
            if cx.layout.kinds.get(physical) != Some(&FileKind::SoundsJson)
                || !cx.scope.contains(physical)
            {
                continue;
            }
            let Ok(arrays) = sound_arrays(&file.blob) else {
                continue;
            };
            let logical = cx
                .layout
                .split(physical)
                .1
                .expect("sounds.json has a logical path");
            let layer = &physical.as_str()[..physical.as_str().len() - logical.as_str().len()];
            let mut splices = Vec::new();
            for (event, range) in arrays {
                let input = &file.blob[range.clone()];
                let tree = json::parse(input).expect("the shared tokenizer read this array");
                for entry in tree.items() {
                    let name = if entry.as_str().is_some() {
                        entry.as_str()
                    } else {
                        entry.path("name").as_str()
                    };
                    let Some(name) = name else {
                        continue;
                    };
                    if entry.path("type").as_str() == Some("event")
                        || entry.path("preload").value == Value::Bool(true)
                        || entry.path("stream").value == Value::Bool(true)
                    {
                        continue;
                    }
                    let Some(id) = ResourceId::parse(&cx.run.interner, name) else {
                        continue;
                    };
                    let path = format!(
                        "assets/{}/sounds/{}.ogg",
                        cx.run.interner.resolve(id.ns),
                        cx.run.interner.resolve(id.path)
                    );
                    let Ok(base) = PhysPath::new(&path) else {
                        continue;
                    };
                    let Ok(overlay) = PhysPath::new(&format!("{layer}{path}")) else {
                        continue;
                    };
                    let target = if cx.pack.files.contains_key(&overlay) {
                        overlay
                    } else {
                        base
                    };
                    let ogg = *metadata.entry(target.clone()).or_insert_with(|| {
                        cx.pack
                            .files
                            .get(&target)
                            .and_then(|file| Ogg::read(&file.blob))
                    });
                    let Some(ogg) = ogg.filter(|ogg| ogg.seconds() > options.seconds) else {
                        continue;
                    };
                    let mut splice = stream(entry, input);
                    splice.range.start += range.start;
                    splice.range.end += range.start;
                    splices.push(splice);
                    edits.changes.push(format!(
                        "stream {}: {name} ({:.1} s)",
                        event,
                        ogg.seconds()
                    ));
                    edits.loss.push(LossRecord {
                        key: format!("{physical}:{}:{name}", event),
                        metric: "stream_seconds".into(),
                        value: ogg.seconds(),
                    });
                }
            }
            if !splices.is_empty() {
                edits
                    .replaced
                    .insert(physical.clone(), json::splice(&file.blob, splices).into());
            }
        }
        Ok(edits)
    }
}

fn sound_arrays(bytes: &[u8]) -> Result<Vec<(String, Range<usize>)>, json::JsonError> {
    json::validate(bytes)?;
    let (mut root, mut event_object) = (false, false);
    let mut event = String::new();
    let mut start = None;
    let mut arrays = Vec::new();
    // Tokens rather than parsed nodes: each duplicate event or `sounds` key gets its own edits.
    json::visit(bytes, |token, path| match (path.depth(), token.kind) {
        (0, TokenKind::StartObject) => root = true,
        (0, TokenKind::EndObject) => root = false,
        (1, TokenKind::Name) if root => event = token.text.to_string(),
        (1, TokenKind::StartObject) if root => event_object = true,
        (1, TokenKind::EndObject) => event_object = false,
        (2, TokenKind::StartArray) if event_object && path.pointer().0.ends_with("/sounds") => {
            start = Some(token.span.start)
        }
        (2, TokenKind::EndArray) => {
            if let Some(start) = start.take() {
                arrays.push((event.clone(), start..token.span.end));
            }
        }
        _ => {}
    })?;
    Ok(arrays)
}

fn stream(entry: &Node, bytes: &[u8]) -> Splice {
    let range = entry.span.start as usize..entry.span.end as usize;
    if entry.as_str().is_some() {
        let mut out = b"{\"name\":".to_vec();
        out.extend_from_slice(&bytes[range.clone()]);
        out.extend_from_slice(b",\"stream\":true}");
        Splice { range, bytes: out }
    } else if entry.path("stream").value == Value::Bool(false) {
        let flag = entry.path("stream");
        Splice {
            range: flag.span.start as usize..flag.span.end as usize,
            bytes: b"true".to_vec(),
        }
    } else {
        let close = range.end - 1;
        Splice {
            range: close..close,
            bytes: format!(
                "{}\"stream\":true",
                if entry.size() == 0 { "" } else { "," }
            )
            .into_bytes(),
        }
    }
}
