use packprune_pass::Pass;

mod atlas;
mod short;

pub use atlas::AtlasDirectory;
pub use short::ShortNames;

pub fn passes() -> Vec<Box<dyn Pass>> {
    vec![Box::new(ShortNames), Box::new(AtlasDirectory)]
}

#[cfg(test)]
pub(crate) mod tests;
