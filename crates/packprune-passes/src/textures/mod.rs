//! identical-sprites and animation-frames. [`Views`] and [`Use`] are what these passes and `crop` read of the pack's
//! views and atlases.

mod animation;
mod identical;
mod usage;
mod views;

#[cfg(test)]
pub(crate) mod tests;

use packprune_pass::Pass;

pub(crate) use usage::Use;
pub(crate) use views::{Views, at, file, no_options};

pub fn passes() -> Vec<Box<dyn Pass>> {
    vec![
        Box::new(identical::IdenticalSprites),
        Box::new(animation::AnimationFrames),
    ]
}
