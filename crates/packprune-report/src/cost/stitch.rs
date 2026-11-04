use super::read::Key;

/// A sprite to stitch: its frame size, and its name for the order ties.
pub(super) struct Item<'a> {
    pub width: i64,
    pub height: i64,
    pub name: &'a Key,
}

/// The texture the client allocates for an atlas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Stitched {
    pub width: i64,
    pub height: i64,
    pub mip_level: u32,
}

impl Stitched {
    pub(super) fn gpu_bytes(&self) -> u64 {
        mip_chain_bytes(self.width, self.height, self.mip_level)
    }
}

/// RGBA bytes of `width` x `height` over `mip_level + 1` levels.
pub(super) fn mip_chain_bytes(width: i64, height: i64, mip_level: u32) -> u64 {
    (0..=mip_level)
        .map(|level| 4 * (width >> level).max(1) as u64 * (height >> level).max(1) as u64)
        .sum()
}

/// The 26.2 client's `SpriteLoader` mip-level rule and `Stitcher` with the default options (no anisotropic
/// filtering, so a sprite is padded by `1 << mip_level` on each side): the atlas size, or `None` where a sprite
/// fits no texture of `max_size`.
pub(super) fn stitch(sprites: &[Item<'_>], max_mip: u32, max_size: i64) -> Option<Stitched> {
    let mut min_texel = i64::MAX;
    let mut lowest_one_bit = 1_i64 << max_mip;
    for sprite in sprites {
        min_texel = min_texel.min(sprite.width.min(sprite.height));
        lowest_one_bit =
            lowest_one_bit.min(lowest_bit(sprite.width).min(lowest_bit(sprite.height)));
    }
    let mip_level = max_mip.min(min_texel.min(lowest_one_bit).ilog2());
    let padding = 1_i64 << mip_level;
    let mut holders: Vec<_> = sprites
        .iter()
        .map(|sprite| {
            (
                sprite,
                fit(sprite.width + padding * 2, mip_level),
                fit(sprite.height + padding * 2, mip_level),
            )
        })
        .collect();
    // Identifier order: path first, then namespace. The sort is stable, as the client's.
    holders.sort_by(|(a, a_width, a_height), (b, b_width, b_height)| {
        b_height
            .cmp(a_height)
            .then_with(|| b_width.cmp(a_width))
            .then_with(|| a.name.value.cmp(&b.name.value))
            .then_with(|| a.name.namespace.cmp(&b.name.namespace))
    });
    let mut stitcher = Stitcher {
        max_size,
        storage: Vec::new(),
        x: 0,
        y: 0,
    };
    for &(_, width, height) in &holders {
        if !stitcher.add(width, height) {
            return None;
        }
    }
    Some(Stitched {
        width: stitcher.x,
        height: stitcher.y,
        mip_level,
    })
}

fn lowest_bit(value: i64) -> i64 {
    value.isolate_lowest_one()
}

fn fit(input: i64, mip_level: u32) -> i64 {
    let rest = input & ((1 << mip_level) - 1);
    ((input >> mip_level) + i64::from(rest != 0)) << mip_level
}

/// `Mth.smallestEncompassingPowerOfTwo`, with 0 for 0.
fn power_of_two(value: i64) -> i64 {
    if value <= 0 {
        0
    } else {
        (value as u64).next_power_of_two() as i64
    }
}

struct Stitcher {
    max_size: i64,
    storage: Vec<Region>,
    x: i64,
    y: i64,
}

impl Stitcher {
    fn add(&mut self, width: i64, height: i64) -> bool {
        self.storage
            .iter_mut()
            .any(|region| region.add(width, height))
            || self.expand(width, height)
    }

    fn expand(&mut self, width: i64, height: i64) -> bool {
        let x_current = power_of_two(self.x);
        let y_current = power_of_two(self.y);
        let x_new = power_of_two(self.x + width);
        let y_new = power_of_two(self.y + height);
        let x_can_grow = x_new <= self.max_size;
        let y_can_grow = y_new <= self.max_size;
        if !x_can_grow && !y_can_grow {
            return false;
        }
        let x_will_grow = x_can_grow && x_current != x_new;
        let y_will_grow = y_can_grow && y_current != y_new;
        let grow_on_x = if x_will_grow ^ y_will_grow {
            x_will_grow
        } else {
            x_can_grow && x_current <= y_current
        };
        let mut slot = if grow_on_x {
            if self.y == 0 {
                self.y = y_new;
            }
            let slot = Region::new(x_new - self.x, self.y);
            self.x = x_new;
            slot
        } else {
            let slot = Region::new(self.x, y_new - self.y);
            self.y = y_new;
            slot
        };
        slot.add(width, height);
        self.storage.push(slot);
        true
    }
}

/// The client's `Stitcher.Region`: only the sizes decide the atlas, so the origins are not kept.
struct Region {
    width: i64,
    height: i64,
    sub_slots: Option<Vec<Region>>,
    filled: bool,
}

impl Region {
    fn new(width: i64, height: i64) -> Self {
        Self {
            width,
            height,
            sub_slots: None,
            filled: false,
        }
    }

    fn add(&mut self, width: i64, height: i64) -> bool {
        if self.filled || width > self.width || height > self.height {
            return false;
        }
        if width == self.width && height == self.height {
            self.filled = true;
            return true;
        }
        let (own_width, own_height) = (self.width, self.height);
        let sub_slots = self.sub_slots.get_or_insert_with(|| {
            let mut slots = vec![Region::new(width, height)];
            let spare_width = own_width - width;
            let spare_height = own_height - height;
            if spare_height > 0 && spare_width > 0 {
                let right = own_height.max(spare_width);
                let bottom = own_width.max(spare_height);
                if right >= bottom {
                    slots.push(Region::new(width, spare_height));
                    slots.push(Region::new(spare_width, own_height));
                } else {
                    slots.push(Region::new(spare_width, height));
                    slots.push(Region::new(own_width, spare_height));
                }
            } else if spare_width == 0 {
                slots.push(Region::new(width, spare_height));
            } else if spare_height == 0 {
                slots.push(Region::new(spare_width, height));
            }
            slots
        });
        sub_slots.iter_mut().any(|slot| slot.add(width, height))
    }
}
