use std::ops::Range;

use crate::{CodecError, invalid};

pub(crate) fn same_setup(before: &[u8], after: &[u8]) -> Result<bool, CodecError> {
    let a = setup_invariants(before)?;
    let b = setup_invariants(after)?;
    Ok(a.len() == b.len()
        && a.into_iter().zip(b).all(|(a, b)| {
            a.len() == b.len()
                && a.zip(b)
                    .all(|(i, j)| (before[i / 8] >> (i % 8)) & 1 == (after[j / 8] >> (j % 8)) & 1)
        }))
}

/// The width of the mode number opening each audio packet: the setup header after the codebooks
/// (Vorbis I specification, section 4.2.4) read up to the mode count.
// JOrbis 0.0.17 CodeBook.make_decode_tree allocates entries * 2 nodes, then reads node 0.
// Complete prefix trees, empty nonzero books and one-bit singletons fit that allocation.
pub(crate) fn mode_bits(packet: &[u8], channels: u32) -> Result<u32, CodecError> {
    let start = setup_invariants(packet)?
        .last()
        .map_or(0, |range| range.start);
    let mut bits = Bits {
        bytes: packet,
        at: start,
    };
    for _ in 0..=bits.read(6)? {
        if bits.read(16)? != 0 {
            return Err(invalid("Vorbis", "nonzero time domain transform"));
        }
    }
    for _ in 0..=bits.read(6)? {
        match bits.read(16)? {
            0 => {
                bits.skip(8 + 16 + 16 + 6 + 8)?;
                let books = bits.read(4)? + 1;
                bits.skip(u64::from(books) * 8)?;
            }
            1 => {
                let partitions: Vec<_> = (0..bits.read(5)?)
                    .map(|_| bits.read(4))
                    .collect::<Result<_, _>>()?;
                let classes = partitions.iter().max().map_or(0, |&c| c + 1);
                let mut dimensions = Vec::new();
                for _ in 0..classes {
                    dimensions.push(bits.read(3)? + 1);
                    let subclasses = bits.read(2)?;
                    if subclasses > 0 {
                        bits.skip(8)?;
                    }
                    bits.skip((1u64 << subclasses) * 8)?;
                }
                bits.skip(2)?;
                let range = bits.read(4)?;
                for class in partitions {
                    bits.skip(u64::from(dimensions[class as usize] * range))?;
                }
            }
            _ => return Err(invalid("Vorbis", "reserved floor type")),
        }
    }
    for _ in 0..=bits.read(6)? {
        if bits.read(16)? > 2 {
            return Err(invalid("Vorbis", "reserved residue type"));
        }
        bits.skip(24 + 24 + 24)?;
        let classifications = bits.read(6)? + 1;
        bits.skip(8)?;
        let mut books = 0;
        for _ in 0..classifications {
            let low = bits.read(3)?;
            let high = if bits.read(1)? == 1 { bits.read(5)? } else { 0 };
            books += (high << 3 | low).count_ones();
        }
        bits.skip(u64::from(books) * 8)?;
    }
    let channel_bits = u32::BITS - channels.saturating_sub(1).leading_zeros();
    for _ in 0..=bits.read(6)? {
        if bits.read(16)? != 0 {
            return Err(invalid("Vorbis", "reserved mapping type"));
        }
        let submaps = if bits.read(1)? == 1 {
            bits.read(4)? + 1
        } else {
            1
        };
        if bits.read(1)? == 1 {
            let steps = bits.read(8)? + 1;
            bits.skip(u64::from(steps) * 2 * u64::from(channel_bits))?;
        }
        bits.skip(2)?;
        if submaps > 1 {
            bits.skip(u64::from(channels) * 4)?;
        }
        bits.skip(u64::from(submaps) * 24)?;
    }
    let modes = bits.read(6)? + 1;
    Ok(u32::BITS - (modes - 1).leading_zeros())
}

fn setup_invariants(packet: &[u8]) -> Result<Vec<Range<usize>>, CodecError> {
    if !packet.starts_with(b"\x05vorbis") {
        return Err(invalid("Vorbis", "missing setup header"));
    }
    let mut bits = Bits {
        bytes: packet,
        at: 7 * 8,
    };
    let books = bits.read(8)? + 1;
    let mut invariants = Vec::new();
    invariants.push(0..bits.at);
    for _ in 0..books {
        let header = bits.at;
        if bits.read(24)? != 0x564342 {
            return Err(invalid("Vorbis", "invalid codebook sync"));
        }
        let dimensions = bits.read(16)?;
        let entries = bits.read(24)?;
        if entries == 0 || dimensions == 0 {
            return Err(invalid(
                "Vorbis",
                "client rejects empty codebook or zero dimensions",
            ));
        }
        invariants.push(header..bits.at);
        let mut lengths = [0u32; 33];
        if bits.read(1)? == 0 {
            let sparse = bits.read(1)? != 0;
            for _ in 0..entries {
                if !sparse || bits.read(1)? != 0 {
                    lengths[(bits.read(5)? + 1) as usize] += 1;
                }
            }
        } else {
            let mut length = bits.read(5)? + 1;
            let mut remaining = entries;
            while remaining != 0 {
                if length > 32 {
                    return Err(invalid("Vorbis", "codeword exceeds 32 bits"));
                }
                let count = bits.read(u32::BITS - remaining.leading_zeros())?;
                if count > remaining {
                    return Err(invalid("Vorbis", "invalid ordered codebook run"));
                }
                lengths[length as usize] = count;
                remaining -= count;
                length += 1;
            }
        }
        let used: u32 = lengths.iter().sum();
        let mut slots = 1u64;
        for &count in &lengths[1..] {
            slots = (slots * 2)
                .checked_sub(u64::from(count))
                .ok_or_else(|| invalid("Vorbis", "overpopulated codebook"))?;
        }
        if slots != 0 && used != 0 && !(used == 1 && lengths[1] == 1) {
            return Err(invalid(
                "Vorbis",
                "incomplete codebook is outside the client-safe subset",
            ));
        }
        let lookup_start = bits.at;
        let lookup = bits.read(4)?;
        if lookup > 2 {
            return Err(invalid("Vorbis", "reserved codebook lookup type"));
        }
        if lookup != 0 {
            bits.skip(64)?;
            let width = u64::from(bits.read(4)? + 1);
            bits.skip(1)?;
            let values = if lookup == 2 {
                u64::from(entries) * u64::from(dimensions)
            } else {
                let mut low = 1u32;
                let mut high = entries;
                while low < high {
                    let mid = low + (high - low).div_ceil(2);
                    if mid.checked_pow(dimensions).is_some_and(|n| n <= entries) {
                        low = mid;
                    } else {
                        high = mid - 1;
                    }
                }
                u64::from(low)
            };
            bits.skip(values * width)?;
        }
        invariants.push(lookup_start..bits.at);
    }
    // The setup framing bit is one; only the final byte's zero padding may change alignment.
    let last = *packet
        .last()
        .expect("the packet starts with the setup signature");
    if last == 0 {
        return Err(invalid("Vorbis", "missing setup framing bit"));
    }
    let padding = last.leading_zeros() as usize;
    let end = packet.len() * 8 - padding;
    if bits.at >= end {
        return Err(invalid("Vorbis", "missing setup fields or framing bit"));
    }
    invariants.push(bits.at..end);
    Ok(invariants)
}

struct Bits<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Bits<'_> {
    fn read(&mut self, width: u32) -> Result<u32, CodecError> {
        let start = self.at;
        self.skip(u64::from(width))?;
        let mut value = 0;
        for bit in 0..width as usize {
            value |= u32::from((self.bytes[(start + bit) / 8] >> ((start + bit) % 8)) & 1) << bit;
        }
        Ok(value)
    }

    fn skip(&mut self, width: u64) -> Result<(), CodecError> {
        let end = (self.at as u64).saturating_add(width);
        if end > self.bytes.len() as u64 * 8 {
            return Err(invalid("Vorbis", "truncated codebook"));
        }
        self.at = end as usize;
        Ok(())
    }
}
