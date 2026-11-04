//! Every pass, one module per family, and their registry.

pub mod crop;
pub mod encode;
pub mod json;
pub mod lossy;
pub mod models;
pub mod names;
pub mod overlays;
pub mod sounds;
pub mod textures;

use packprune_pass::{FileOptimizer, Pass};

/// Every graph pass; the engine sorts them by loss, stage and name.
pub fn passes() -> Vec<Box<dyn Pass>> {
    [
        models::passes(),
        textures::passes(),
        crop::passes(),
        overlays::passes(),
        names::passes(),
        json::passes(),
        encode::passes(),
        lossy::passes(),
        sounds::passes(),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Every file optimiser.
pub fn optimisers() -> Vec<Box<dyn FileOptimizer>> {
    [encode::optimisers(), lossy::optimisers()]
        .into_iter()
        .flatten()
        .collect()
}
