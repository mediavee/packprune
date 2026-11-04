//! The JSON stage: shortest numbers, bloat keys out of models, and the files no client reads.

mod bloat;
mod short;
mod unread;

#[cfg(test)]
mod corpus;
#[cfg(test)]
mod tests;

use packprune_pack::{FileKind, PhysPath};
use packprune_pass::{Pass, PassCx};

pub fn passes() -> Vec<Box<dyn Pass>> {
    vec![
        Box::new(short::ShortJson),
        Box::new(bloat::BloatKeys),
        Box::new(unread::DropUnread),
    ]
}

fn kind(cx: &PassCx<'_>, path: &PhysPath) -> FileKind {
    cx.layout
        .kinds
        .get(path)
        .copied()
        .unwrap_or(FileKind::Unknown)
}

fn no_options() -> schemars::Schema {
    schemars::json_schema!({"type": "object", "additionalProperties": false})
}
