use super::read::Key;

pub(super) use packprune_graph::stitch::{Stitched, mip_chain_bytes};

/// A sprite to stitch: its frame size, and its name for the order ties.
pub(super) struct Item<'a> {
    pub width: i64,
    pub height: i64,
    pub name: &'a Key,
}

pub(super) fn stitch(sprites: &[Item<'_>], max_mip: u32, max_size: i64) -> Option<Stitched> {
    stitch_with_aniso(sprites, max_mip, max_size, 0)
}

pub(super) fn stitch_with_aniso(
    sprites: &[Item<'_>],
    max_mip: u32,
    max_size: i64,
    aniso_shift: u32,
) -> Option<Stitched> {
    let items: Vec<packprune_graph::stitch::Item<'_>> = sprites
        .iter()
        .map(|s| packprune_graph::stitch::Item {
            width: s.width,
            height: s.height,
            namespace: &s.name.namespace,
            value: &s.name.value,
        })
        .collect();
    packprune_graph::stitch::stitch_with_aniso(&items, max_mip, max_size, aniso_shift)
}
