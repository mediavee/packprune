//! uv-crop and uv-crop-particles: textures cropped to the texels their model faces sample.

mod crop_box;
mod uv_crop;

#[cfg(test)]
mod tests;

use packprune_pass::Pass;

pub fn passes() -> Vec<Box<dyn Pass>> {
    vec![
        Box::new(uv_crop::UvCrop { particles: false }),
        Box::new(uv_crop::UvCrop { particles: true }),
    ]
}
