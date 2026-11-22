use packprune_pack::json::{self, Node};
use packprune_pack::{FileKind, Layer, LogicalPath, PhysPath};
use packprune_pass::{Pass, PassCx};
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};

mod defaults;
mod degenerate;
mod identical;
mod identity_rotations;
mod shared;
mod shared_elements;
mod text;

#[cfg(test)]
mod corpus;
#[cfg(test)]
mod tests;

pub use defaults::ModelDefaults;
pub use degenerate::DegenerateFaces;
pub use identical::IdenticalModels;
pub use identity_rotations::IdentityRotations;
pub use shared::SharedDisplay;
pub use shared_elements::SharedElements;

pub fn passes() -> Vec<Box<dyn Pass>> {
    vec![
        Box::new(DegenerateFaces),
        Box::new(IdenticalModels),
        Box::new(IdentityRotations),
        Box::new(ModelDefaults),
        Box::new(SharedDisplay),
        Box::new(SharedElements),
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
