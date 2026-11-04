//! A `ResourceId` of the run's interner with its text, which the resolver turns into file paths.

use packprune_pack::{Interner, ResourceId};

use packprune_pack::json::Node;

pub(crate) const DEFAULT_NAMESPACE: &str = "minecraft";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Id<'i> {
    pub(crate) id: ResourceId,
    pub(crate) ns: &'i str,
    pub(crate) path: &'i str,
}

impl<'i> Id<'i> {
    pub(crate) fn parse(interner: &'i Interner, value: &str) -> Option<Self> {
        let id = ResourceId::parse(interner, value)?;
        Some(Self {
            id,
            ns: interner.resolve(id.ns),
            path: interner.resolve(id.path),
        })
    }

    /// `assets/<namespace>/<dir>/<path><suffix>`, no folder segment when `dir` is empty.
    pub(crate) fn file(&self, dir: &str, suffix: &str) -> String {
        let dir = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        format!("assets/{}/{dir}{}{suffix}", self.ns, self.path)
    }

    pub(crate) fn text(&self) -> String {
        format!("{}:{}", self.ns, self.path)
    }
}

/// A `type` field, its path alone when it is in the default namespace.
pub(crate) fn source_type(interner: &Interner, node: &Node) -> String {
    match node.get("type").and_then(Node::as_str) {
        Some(value) => normalized_type(interner, value),
        None => String::new(),
    }
}

pub(crate) fn normalized_type(interner: &Interner, value: &str) -> String {
    match Id::parse(interner, value) {
        Some(id) if id.ns == DEFAULT_NAMESPACE => id.path.to_owned(),
        _ => value.to_owned(),
    }
}
