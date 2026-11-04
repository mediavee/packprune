use packprune_pass::Pass;

mod entries;
mod floor;
pub(crate) mod json;
mod merge;

pub use floor::OverlayFloor;
pub use merge::OverlayMerge;

pub fn passes() -> Vec<Box<dyn Pass>> {
    vec![Box::new(OverlayMerge), Box::new(OverlayFloor)]
}

#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
