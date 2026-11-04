use std::io::Cursor;

use ::ogg::{Packet, PacketReader, PacketWriteEndInfo, PacketWriter};

use crate::{CodecError, invalid, vorbis_client};

/// Whether `after` has as many packets as `before` and headers JOrbis reads alike: the same identification header,
/// framed comments and a setup with the same client-safe codebooks and configurations.
pub(super) fn compatible_headers(before: &[u8], after: &[u8]) -> Result<bool, CodecError> {
    let mut before = PacketReader::new(Cursor::new(before));
    let mut after = PacketReader::new(Cursor::new(after));
    let mut index = 0;
    loop {
        let a = before.read_packet().map_err(|e| invalid("Ogg", e))?;
        let b = after.read_packet().map_err(|e| invalid("Ogg", e))?;
        let (a, b) = match (a, b) {
            (None, None) => return Ok(true),
            (Some(a), Some(b)) => (a, b),
            _ => return Ok(false),
        };
        let compatible = match index {
            0 => a.data == b.data,
            1 => valid_comment(&a.data) && valid_comment(&b.data),
            2 => vorbis_client::same_setup(&a.data, &b.data).unwrap_or(false),
            _ => true,
        };
        if !compatible {
            return Ok(false);
        }
        index += 1;
    }
}

/// `after`'s packets in `before`'s page groups and granules.
pub(super) fn compatible_rewrite(
    before: &[u8],
    after: &[u8],
    serial: u32,
) -> Result<Option<Vec<u8>>, CodecError> {
    if !compatible_headers(before, after)? {
        return Ok(None);
    }
    let mut before = PacketReader::new(Cursor::new(before));
    let mut after = PacketReader::new(Cursor::new(after));
    let mut writer = PacketWriter::new(Vec::new());
    let mut timing = Vec::new();
    loop {
        let a = before.read_packet().map_err(|e| invalid("Ogg", e))?;
        let b = after.read_packet().map_err(|e| invalid("Ogg", e))?;
        let (a, b) = match (a, b) {
            (None, None) => break,
            (Some(a), Some(b)) => (a, b),
            _ => return Ok(None),
        };
        timing.push(packet_timing(&a));
        let end = if a.last_in_stream() {
            PacketWriteEndInfo::EndStream
        } else if a.last_in_page() {
            PacketWriteEndInfo::EndPage
        } else {
            PacketWriteEndInfo::NormalPacket
        };
        // JOrbis trims PCM using packet granules and EOS, so preserve the original page groups.
        writer
            .write_packet(b.data, serial, end, a.absgp_page())
            .map_err(|e| invalid("Ogg", e))?;
    }
    let out = writer.into_inner();
    let mut reader = PacketReader::new(Cursor::new(&out));
    for expected in timing {
        let Some(packet) = reader.read_packet().map_err(|e| invalid("Ogg", e))? else {
            return Ok(None);
        };
        if packet_timing(&packet) != expected {
            return Ok(None);
        }
    }
    if reader
        .read_packet()
        .map_err(|e| invalid("Ogg", e))?
        .is_some()
    {
        return Ok(None);
    }
    Ok(Some(out))
}

fn packet_timing(packet: &Packet) -> (u64, bool, bool) {
    // Jogg exposes a page's granule only on its last completed packet.
    (
        if packet.last_in_page() {
            packet.absgp_page()
        } else {
            u64::MAX
        },
        packet.first_in_stream(),
        packet.last_in_stream(),
    )
}

fn valid_comment(packet: &[u8]) -> bool {
    if !packet.starts_with(b"\x03vorbis") {
        return false;
    }
    let mut rest = &packet[7..];
    let length = |rest: &mut &[u8]| -> Option<usize> {
        let value = u32::from_le_bytes(rest.get(..4)?.try_into().ok()?);
        *rest = &rest[4..];
        (value < i32::MAX as u32).then_some(value as usize)
    };
    let Some(vendor) = length(&mut rest) else {
        return false;
    };
    let Some(mut rest) = rest.get(vendor..) else {
        return false;
    };
    let Some(comments) = length(&mut rest) else {
        return false;
    };
    for _ in 0..comments {
        let Some(len) = length(&mut rest) else {
            return false;
        };
        let Some(next) = rest.get(len..) else {
            return false;
        };
        rest = next;
    }
    rest.first().is_some_and(|b| b & 1 == 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_preserves_client_packet_timing() {
        let input = include_bytes!("../fixtures/vorbis-client.ogg");
        let mut reader = PacketReader::new(Cursor::new(input));
        let mut writer = PacketWriter::new(Vec::new());
        let mut expected = Vec::new();
        while let Some(packet) = reader.read_packet().unwrap() {
            expected.push(packet_timing(&packet));
            let end = if packet.last_in_stream() {
                PacketWriteEndInfo::EndStream
            } else if packet.last_in_page() {
                PacketWriteEndInfo::EndPage
            } else {
                PacketWriteEndInfo::NormalPacket
            };
            let granule = if packet.last_in_stream() {
                packet.absgp_page() - 1
            } else {
                packet.absgp_page()
            };
            writer.write_packet(packet.data, 1, end, granule).unwrap();
        }
        let candidate = writer.into_inner();
        assert_ne!(candidate, input);
        let out = compatible_rewrite(input, &candidate, 1).unwrap().unwrap();
        let mut reader = PacketReader::new(Cursor::new(out));
        for timing in expected {
            assert_eq!(
                packet_timing(&reader.read_packet().unwrap().unwrap()),
                timing
            );
        }
        assert!(reader.read_packet().unwrap().is_none());
    }
}
