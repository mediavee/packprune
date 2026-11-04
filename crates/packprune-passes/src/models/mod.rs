use packprune_pack::json::{self, Node};
use packprune_pack::{FileKind, Layer, LogicalPath, PhysPath};
use packprune_pass::{Pass, PassCx};
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};

mod degenerate;
mod identical;
mod shared;
mod text;

#[cfg(test)]
mod corpus;
#[cfg(test)]
mod tests;

pub use degenerate::DegenerateFaces;
pub use identical::IdenticalModels;
pub use shared::SharedDisplay;

pub fn passes() -> Vec<Box<dyn Pass>> {
    vec![
        Box::new(DegenerateFaces),
        Box::new(IdenticalModels),
        Box::new(SharedDisplay),
    ]
}

struct Model<'a> {
    physical: &'a PhysPath,
    logical: LogicalPath,
    layer: Layer,
    json: Node,
}

/// Every model file that parses as an object, in path order, parsed in parallel.
fn models<'a>(cx: &'a PassCx<'_>) -> Vec<Model<'a>> {
    cx.pack
        .files
        .par_iter()
        .filter_map(|(physical, entry)| {
            if cx.layout.kinds.get(physical) != Some(&FileKind::Model) {
                return None;
            }
            let (layer, logical) = cx.layout.split(physical);
            let json = json::parse(&entry.blob).ok()?;
            json.is_object().then_some(Model {
                physical,
                logical: logical?,
                layer,
                json,
            })
        })
        .collect()
}
