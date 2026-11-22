//! uv-crop, uv-repack, and their `-particles` variants: textures cropped or UV-island-repacked to the texels their
//! model faces sample.

mod crop_box;
mod repack;
mod uv_crop;

#[cfg(test)]
mod tests;

use packprune_pass::Pass;

pub fn passes() -> Vec<Box<dyn Pass>> {
    vec![
        Box::new(uv_crop::UvCrop { particles: false }),
        Box::new(uv_crop::UvCrop { particles: true }),
        Box::new(uv_crop::UvRepack { particles: false }),
        Box::new(uv_crop::UvRepack { particles: true }),
    ]
}
