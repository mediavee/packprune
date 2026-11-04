/// Stream metadata from page granules, without decoding PCM.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Ogg {
    pub channels: u8,
    pub rate: u32,
    pub samples: u64,
}

impl Ogg {
    pub fn seconds(self) -> f64 {
        self.samples as f64 / f64::from(self.rate)
    }

    pub fn read(bytes: &[u8]) -> Option<Self> {
        let mut at = 0;
        let mut info = None;
        while at + 27 <= bytes.len() {
            let header = &bytes[at..at + 27];
            if &header[..4] != b"OggS" {
                return None;
            }
            let segments = usize::from(header[26]);
            let lacing = bytes.get(at + 27..at + 27 + segments)?;
            let size: usize = lacing.iter().map(|&n| usize::from(n)).sum();
            let start = at + 27 + segments;
            let body = bytes.get(start..start + size)?;
            if info.is_none() {
                if body.len() < 23 || &body[..7] != b"\x01vorbis" {
                    return None;
                }
                let rate = i32::from_le_bytes(body[12..16].try_into().ok()?);
                if body[11] == 0 || rate <= 0 {
                    return None;
                }
                info = Some(Self {
                    channels: body[11],
                    rate: rate as u32,
                    samples: 0,
                });
            }
            let granule = i64::from_le_bytes(header[6..14].try_into().ok()?);
            if granule >= 0 {
                info.as_mut()?.samples = granule as u64;
            }
            at = start + size;
        }
        info.filter(|_| at == bytes.len())
    }
}
