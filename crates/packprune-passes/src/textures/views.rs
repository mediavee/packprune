use std::collections::BTreeMap;

use packprune_graph::atlases::{AtlasFile, Sprite, VanillaAtlases, VanillaLayer, ViewAtlases};
use packprune_pack::json::{Node, Value};
use packprune_pack::{FileKind, Interner, PhysPath, ResourceId, View};
use packprune_pass::{PassCx, PassError};

/// The pack's views as a pass reads them: the files each shows, the vanilla client below the
/// pack, and each view's atlases.
pub(crate) struct Views<'a> {
    pub list: &'a [View],
    layer: VanillaLayer<'a>,
    atlases: Vec<ViewAtlases>,
}

impl<'a> Views<'a> {
    /// A `filter` pattern that does not compile fails the pass, as it fails the client.
    pub(crate) fn new(cx: &PassCx<'a>, pass: &'static str) -> Result<Self, PassError> {
        let failed = |why: String| PassError::Failed { pass, why };
        let layer = VanillaLayer::new(cx.vanilla, &cx.layout.meta.filters)
            .map_err(|error| failed(error.to_string()))?;
        let vanilla = VanillaAtlases::with_layer(cx.run, &layer)
            .map_err(|error| failed(error.to_string()))?;
        let files: BTreeMap<&PhysPath, AtlasFile> = cx
            .layout
            .kinds
            .iter()
            .filter(|(_, kind)| **kind == FileKind::Atlas)
            .filter_map(|(path, _)| {
                Some((
                    path,
                    AtlasFile::parse(cx.run, &cx.pack.files.get(path)?.blob)?,
                ))
            })
            .collect();
        let atlases = cx
            .layout
            .views
            .iter()
            .map(|view| ViewAtlases::build(cx.run, &vanilla, view, |path| files.get(path)))
            .collect::<Result<_, _>>()
            .map_err(|error| failed(error.to_string()))?;
        Ok(Self {
            list: &cx.layout.views,
            layer,
            atlases,
        })
    }

    /// The vanilla client has `logical` below the pack, the same layer in every view.
    pub(crate) fn vanilla(&self, logical: &str) -> bool {
        self.layer.has(logical)
    }

    pub(crate) fn atlases(&self, view: usize) -> &ViewAtlases {
        &self.atlases[view]
    }

    /// Every sprite of `atlas` in `view` with its id, by id text.
    pub(crate) fn sprites(
        &self,
        view: usize,
        atlas: &str,
        interner: &Interner,
    ) -> Vec<(ResourceId, &Sprite)> {
        self.atlases[view]
            .sprites(atlas, interner)
            .into_iter()
            .map(|(id, sprite)| {
                let (ns, path) = id.split_once(':').expect("an id shown as ns:path");
                let id = ResourceId {
                    ns: interner.intern(ns),
                    path: interner.intern(path),
                };
                (id, sprite)
            })
            .collect()
    }
}

/// The file of `id` under `folder`: `assets/<ns>/<folder>/<path><suffix>`.
pub(crate) fn file(interner: &Interner, id: ResourceId, folder: &str, suffix: &str) -> String {
    format!(
        "assets/{}/{folder}/{}{suffix}",
        interner.resolve(id.ns),
        interner.resolve(id.path)
    )
}

/// The node an RFC 6901 pointer names, or the missing node.
pub(crate) fn at<'n>(node: &'n Node, pointer: &str) -> &'n Node {
    static MISSING: Node = Node {
        value: Value::Missing,
        span: 0..0,
    };
    let mut current = node;
    for segment in pointer.split('/').skip(1) {
        let segment = segment.replace("~1", "/").replace("~0", "~");
        let next = if current.is_array() {
            segment
                .parse::<usize>()
                .ok()
                .and_then(|index| current.item(index))
        } else {
            current.get(&segment)
        };
        match next {
            Some(next) => current = next,
            None => return &MISSING,
        }
    }
    current
}

/// The options of a pass that has none.
pub(crate) fn no_options() -> schemars::Schema {
    schemars::json_schema!({"type": "object", "additionalProperties": false})
}
