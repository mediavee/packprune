mod ogg;

use std::f64::consts::FRAC_PI_2;
use std::io::Cursor;
use std::num::{NonZeroU8, NonZeroU32};
use std::ops::Range;

use ::ogg::PacketReader;

use optivorbis::remuxer::ogg_to_ogg::Settings;
use optivorbis::{
    OggToOgg, Remuxer, VorbisCommentFieldsAction, VorbisOptimizerSettings, VorbisVendorStringAction,
};
use rubato::audioadapter_buffers::direct::SequentialSliceOfVecs;
use rubato::{Fft, FixedSync, Resampler};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::formats::TrackType;
use symphonia::core::formats::probe::Hint;
use symphonia::core::io::MediaSourceStream;
use vorbis_rs::{VorbisBitrateManagementStrategy, VorbisEncoderBuilder};

use crate::{CodecError, MAX_DECODED_BYTES, invalid, vorbis_client};

// A fixed serial keeps the output a function of the input alone; any value is valid for a single-stream file.
const OGG_STREAM_SERIAL: u32 = 1;

/// The extensions that the Symphonia readers enabled in `Cargo.toml` claim for audio-only files, Ogg aside: ADTS AAC,
/// FLAC, MP4 audio, MP3, WAVE. [`decode`] probes the content, whatever the extension.
pub const DECODABLE_EXTENSIONS: [&str; 6] = ["aac", "flac", "m4a", "mp3", "wav", "wave"];

#[derive(Clone, Debug, PartialEq)]
pub struct Pcm {
    pub rate: u32,
    /// Planar, finite samples clipped to [-1, 1], as the client's decoder returns them.
    pub channels: Vec<Vec<f32>>,
}

impl Pcm {
    pub fn frames(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }

    pub fn bytes(&self) -> u64 {
        2 * self.channels.len() as u64 * self.frames() as u64
    }

    pub(crate) fn validate(&self) -> Result<(), CodecError> {
        if self.rate == 0
            || self.channels.is_empty()
            || self.frames() == 0
            || self
                .channels
                .iter()
                .any(|c| c.len() != self.frames() || c.iter().any(|s| !s.is_finite()))
            || self
                .frames()
                .saturating_mul(self.channels.len())
                .saturating_mul(4)
                > MAX_DECODED_BYTES
        {
            return Err(invalid(
                "audio",
                "invalid rate, channels, samples or memory limit exceeded",
            ));
        }
        Ok(())
    }
}

/// Ogg Vorbis decodes to the samples the client's JOrbis returns; other formats to Symphonia's.
pub fn decode(bytes: &[u8]) -> Result<Pcm, CodecError> {
    let mut jorbis = if bytes.starts_with(b"OggS") {
        Some(validate_ogg(bytes)?)
    } else {
        None
    };
    let mss = MediaSourceStream::new(Box::new(Cursor::new(bytes.to_vec())), Default::default());
    let mut format = symphonia::default::get_probe()
        .probe(&Hint::new(), mss, Default::default(), Default::default())
        .map_err(|e| invalid("audio", e))?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| invalid("audio", "no audio track"))?;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| invalid("audio", "no audio codec parameters"))?;
    let mut decoder = symphonia::default::get_codecs()
        // Symphonia's gapless mode also trims at mid-stream granules, which JOrbis believes instead.
        .make_audio_decoder(
            params,
            &AudioDecoderOptions::default().gapless(jorbis.is_none()),
        )
        .map_err(|e| invalid("audio", e))?;
    let track_id = track.id;
    let mut out = Pcm {
        rate: 0,
        channels: Vec::new(),
    };
    while let Some(packet) = format.next_packet().map_err(|e| invalid("audio", e))? {
        if packet.track_id != track_id {
            return Err(invalid("audio", "multiple tracks are not supported"));
        }
        let decoded = decoder.decode(&packet).map_err(|e| invalid("audio", e))?;
        let channels = decoded.spec().channels().count();
        let rate = decoded.spec().rate();
        if channels == 0 || rate == 0 {
            return Err(invalid("audio", "zero channels or sample rate"));
        }
        if out.channels.is_empty() {
            out.rate = rate;
            out.channels.resize(channels, Vec::new());
        }
        if rate != out.rate || channels != out.channels.len() {
            return Err(invalid("audio", "format changed within the stream"));
        }
        let kept = match &mut jorbis {
            Some(jorbis) => jorbis.returned(decoded.frames())?,
            None => 0..decoded.frames(),
        };
        let total = out
            .frames()
            .saturating_add(kept.len())
            .saturating_mul(channels)
            .saturating_mul(4);
        if total > MAX_DECODED_BYTES {
            return Err(invalid("audio", "decoded PCM exceeds the memory limit"));
        }
        let mut interleaved = vec![0.0f32; decoded.samples_interleaved()];
        decoded.copy_to_slice_interleaved(&mut interleaved);
        let frames = interleaved.chunks_exact(channels).enumerate();
        for (i, frame) in frames.take(kept.end).skip(kept.start) {
            // Only a long first block keeps a suffix, rendered by Symphonia under its long slope.
            // JOrbis windows by the packet's own previous-window flag: flat after a short one.
            let gain = if kept.start > 0 && jorbis.as_ref().is_some_and(|j| !j.first_after_long) {
                long_slope(decoded.frames(), i)
            } else {
                1.0
            };
            for (channel, &sample) in out.channels.iter_mut().zip(frame) {
                if !sample.is_finite() {
                    return Err(invalid("audio", "non-finite sample"));
                }
                channel.push((sample / gain).clamp(-1.0, 1.0));
            }
        }
    }
    if let Some(jorbis) = jorbis {
        if jorbis.next != jorbis.granules.len() {
            return Err(invalid("Vorbis", "audio packets were skipped"));
        }
        // The client's JOrbisAudioStream stops at the last page's granule.
        for channel in &mut out.channels {
            channel.truncate(jorbis.end as usize);
        }
    }
    out.validate()?;
    Ok(out)
}

/// What JOrbis 0.0.17 `DspState.synthesis_blockin` returns per audio packet, from the frame
/// counts Symphonia renders without gapless trimming.
struct Jorbis {
    short_block: usize,
    long_block: usize,
    /// The first audio packet's previous-window flag, read only when it is a long block.
    first_after_long: bool,
    /// Per audio packet, the granule of the page it ends last in.
    granules: Vec<Option<u64>>,
    end: u64,
    next: usize,
    granule: Option<u64>,
}

impl Jorbis {
    /// The frames of the packet's rendered output that JOrbis returns.
    fn returned(&mut self, frames: usize) -> Result<Range<usize>, CodecError> {
        let index = self.next;
        let page = *self
            .granules
            .get(index)
            .ok_or_else(|| invalid("Vorbis", "more audio packets than Ogg packets"))?;
        self.next += 1;
        if index == 0 {
            // Symphonia renders the first block's left half. JOrbis starts as after a short
            // block: nothing from a short one, the part past the short slope from a long one.
            self.granule = page;
            return match frames * 2 {
                block if block == self.short_block => Ok(0..0),
                block if block == self.long_block => Ok(block / 4 + self.short_block / 4..frames),
                _ => Err(invalid("Vorbis", "first block size is neither blocksize")),
            };
        }
        let Some(granule) = self.granule else {
            self.granule = page;
            return Ok(0..frames);
        };
        let counted = granule + frames as u64;
        self.granule = Some(counted);
        let mut returned = frames;
        if let Some(page) = page.filter(|&page| page != counted) {
            // Only the last packet is trimmed; elsewhere JOrbis believes the bitstream.
            if counted > page && index + 1 == self.granules.len() {
                returned = frames.saturating_sub((counted - page) as usize);
            }
            self.granule = Some(page);
        }
        Ok(0..returned)
    }
}

/// Symphonia 0.6's rising window slope for a block whose left half has `half` samples.
fn long_slope(half: usize, i: usize) -> f32 {
    let x = FRAC_PI_2 * (i as f64 + 0.5) / half as f64;
    (FRAC_PI_2 * x.sin().powi(2)).sin() as f32
}

fn validate_ogg(bytes: &[u8]) -> Result<Jorbis, CodecError> {
    let mut reader = PacketReader::new(Cursor::new(bytes));
    let mut serial = None;
    let mut ended = false;
    let mut packets = 0;
    let (mut short_block, mut long_block, mut channels, mut mode_bits) = (0, 0, 0, 0);
    let mut first_after_long = false;
    let mut granules = Vec::new();
    while let Some(packet) = reader.read_packet().map_err(|e| invalid("Ogg", e))? {
        if ended || serial.is_some_and(|s| s != packet.stream_serial()) {
            return Err(invalid(
                "Ogg",
                "chained or multiplexed streams are not supported",
            ));
        }
        match packets {
            0 => {
                // The identification header's channel count and blocksizes, at fixed offsets.
                channels = packet.data.get(11).copied().unwrap_or(0).into();
                let sizes = packet.data.get(28).copied().unwrap_or(0);
                (short_block, long_block) = (1 << (sizes & 0x0f), 1 << (sizes >> 4));
            }
            2 => mode_bits = vorbis_client::mode_bits(&packet.data, channels)?,
            1 => {}
            _ => {
                if granules.is_empty() {
                    // Packet type, mode number, then the long-block window flags.
                    let at = 1 + mode_bits as usize;
                    first_after_long = packet
                        .data
                        .get(at / 8)
                        .is_some_and(|b| b >> (at % 8) & 1 == 1);
                }
                granules.push(
                    (packet.last_in_page() && packet.absgp_page() != u64::MAX)
                        .then_some(packet.absgp_page()),
                );
            }
        }
        if packet.absgp_page() != u64::MAX && packet.absgp_page() > i32::MAX as u64 {
            return Err(invalid("Ogg", "granule position exceeds the client limit"));
        }
        serial = Some(packet.stream_serial());
        ended = packet.last_in_stream();
        packets += 1;
    }
    let Some(&Some(end)) = granules.last().filter(|_| ended) else {
        return Err(invalid("Ogg", "incomplete stream or no final granule"));
    };
    Ok(Jorbis {
        short_block,
        long_block,
        first_after_long,
        granules,
        end,
        next: 0,
        granule: None,
    })
}

pub fn encode(pcm: &Pcm, quality: f32) -> Result<Vec<u8>, CodecError> {
    pcm.validate()?;
    if !quality.is_finite() || !(-0.2..=1.0).contains(&quality) {
        return Err(invalid("Vorbis", "quality must be in [-0.2, 1]"));
    }
    let channels = u8::try_from(pcm.channels.len())
        .ok()
        .and_then(NonZeroU8::new)
        .ok_or_else(|| invalid("Vorbis", "channel count exceeds 255"))?;
    let mut out = Vec::new();
    let mut builder = VorbisEncoderBuilder::new_with_serial(
        NonZeroU32::new(pcm.rate).expect("validate refuses a zero rate"),
        channels,
        &mut out,
        OGG_STREAM_SERIAL as i32,
    );
    builder
        .bitrate_management_strategy(VorbisBitrateManagementStrategy::QualityVbr {
            target_quality: quality,
        })
        // The largest pages: less Ogg framing per byte of audio.
        .minimum_page_data_size(Some(u16::MAX));
    let mut encoder = builder.build().map_err(|e| invalid("Vorbis", e))?;
    for start in (0..pcm.frames()).step_by(4096) {
        let end = (start + 4096).min(pcm.frames());
        let block: Vec<_> = pcm.channels.iter().map(|c| &c[start..end]).collect();
        encoder
            .encode_audio_block(&block)
            .map_err(|e| invalid("Vorbis", e))?;
    }
    encoder.finish().map_err(|e| invalid("Vorbis", e))?;
    Ok(out)
}

pub fn optimise(bytes: &[u8]) -> Result<Vec<u8>, CodecError> {
    let before = decode(bytes)?;
    // JOrbis reads coupled channel indices with floor(log2(channels)); keep multichannel inputs.
    if before.channels.len() > 2 {
        return Ok(bytes.to_vec());
    }
    let mut optimizer = VorbisOptimizerSettings::default();
    optimizer.comment_fields_action = VorbisCommentFieldsAction::Delete;
    optimizer.vendor_string_action = VorbisVendorStringAction::Empty;
    let remuxer = OggToOgg::new(
        Settings {
            randomize_stream_serials: false,
            first_stream_serial_offset: OGG_STREAM_SERIAL,
            ignore_start_sample_offset: true,
            ..Default::default()
        },
        optimizer,
    );
    let out = remuxer
        .remux(Cursor::new(bytes), Vec::new())
        .map_err(|e| invalid("Ogg", e))?;
    let out = if same_client_packets(bytes, &out)? {
        out
    } else if let Some(candidate) = ogg::compatible_rewrite(bytes, &out, OGG_STREAM_SERIAL)? {
        candidate
    } else {
        return Ok(bytes.to_vec());
    };
    if !decode(&out).is_ok_and(|after| after == before) {
        return Ok(bytes.to_vec());
    }
    Ok(if out.len() < bytes.len() {
        out
    } else {
        bytes.to_vec()
    })
}

fn same_client_packets(before: &[u8], after: &[u8]) -> Result<bool, CodecError> {
    let mut before = PacketReader::new(Cursor::new(before));
    let mut after = PacketReader::new(Cursor::new(after));
    loop {
        let a = before.read_packet().map_err(|e| invalid("Ogg", e))?;
        let b = after.read_packet().map_err(|e| invalid("Ogg", e))?;
        match (a, b) {
            (None, None) => return Ok(true),
            (Some(a), Some(b)) => {
                if a.data != b.data
                    || a.absgp_page() != b.absgp_page()
                    || a.first_in_page() != b.first_in_page()
                    || a.last_in_page() != b.last_in_page()
                    || a.first_in_stream() != b.first_in_stream()
                    || a.last_in_stream() != b.last_in_stream()
                {
                    return Ok(false);
                }
            }
            _ => return Ok(false),
        }
    }
}

pub fn optimise_empty(bytes: &[u8]) -> Result<Vec<u8>, CodecError> {
    let pcm = decode(bytes)?;
    if pcm.channels.iter().flatten().any(|&s| s != 0.0) {
        return Ok(bytes.to_vec());
    }
    // Preserve the sample count and rate: silence has a duration too.
    let candidate = optimise(&encode(&pcm, -0.2)?)?;
    if candidate.len() < bytes.len() && decode(&candidate)? == pcm {
        Ok(candidate)
    } else {
        Ok(bytes.to_vec())
    }
}

pub fn downmix(pcm: &Pcm) -> Result<Pcm, CodecError> {
    pcm.validate()?;
    let mut mono = vec![0.0; pcm.frames()];
    for channel in &pcm.channels {
        for (sum, &sample) in mono.iter_mut().zip(channel) {
            *sum += sample / pcm.channels.len() as f32;
        }
    }
    Ok(Pcm {
        rate: pcm.rate,
        channels: vec![mono],
    })
}

pub fn resample(pcm: &Pcm, rate: u32) -> Result<Pcm, CodecError> {
    pcm.validate()?;
    if rate == 0 {
        return Err(invalid("audio", "output rate must be positive"));
    }
    if rate == pcm.rate {
        return Ok(pcm.clone());
    }
    let mut divisor = pcm.rate;
    let mut remainder = rate;
    while remainder != 0 {
        (divisor, remainder) = (remainder, divisor % remainder);
    }
    let fft_unit = u64::from(pcm.rate / divisor) + u64::from(rate / divisor);
    if fft_unit * 64 > MAX_DECODED_BYTES as u64 {
        return Err(invalid(
            "audio",
            "resampling ratio exceeds the FFT memory limit",
        ));
    }
    let frames = (pcm.frames() as u128 * u128::from(rate)).div_ceil(u128::from(pcm.rate));
    if frames * pcm.channels.len() as u128 * 4 > MAX_DECODED_BYTES as u128 {
        return Err(invalid("audio", "resampled PCM exceeds the memory limit"));
    }
    let input = SequentialSliceOfVecs::new(&pcm.channels, pcm.channels.len(), pcm.frames())
        .map_err(|e| invalid("audio", e))?;
    let mut resampler = Fft::<f32>::new(
        pcm.rate as usize,
        rate as usize,
        1024,
        pcm.channels.len(),
        FixedSync::Both,
    )
    .map_err(|e| invalid("audio", e))?;
    let samples = resampler
        .process_all(&input, pcm.frames(), None)
        .map_err(|e| invalid("audio", e))?
        .take_data();
    let mut channels = vec![Vec::new(); pcm.channels.len()];
    for frame in samples.chunks_exact(channels.len()) {
        for (channel, &sample) in channels.iter_mut().zip(frame) {
            channel.push(sample.clamp(-1.0, 1.0));
        }
    }
    let out = Pcm { rate, channels };
    out.validate()?;
    Ok(out)
}

#[cfg(test)]
pub(crate) fn tone(rate: u32, channels: usize) -> Pcm {
    let wave: Vec<_> = (0..rate / 5)
        .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.5)
        .collect();
    Pcm {
        rate,
        channels: vec![wave; channels],
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn client_rejects_zero_entry_codebooks() {
        assert!(decode(include_bytes!("fixtures/vorbis-rewritten-client.ogg")).is_ok());
        assert!(
            !same_client_packets(
                include_bytes!("fixtures/vorbis-client.ogg"),
                include_bytes!("fixtures/vorbis-rewritten-client.ogg")
            )
            .unwrap()
        );
        // JOrbis 0.0.17 decodes the originals; both rewritten files fail in make_decode_tree.
        for (original, broken) in [
            (
                include_bytes!("fixtures/vorbis-client.ogg").as_slice(),
                include_bytes!("fixtures/vorbis-zero-entry.ogg").as_slice(),
            ),
            (
                include_bytes!("fixtures/vorbis-silence.ogg").as_slice(),
                include_bytes!("fixtures/vorbis-silence-zero-entry.ogg").as_slice(),
            ),
        ] {
            assert!(decode(original).is_ok());
            assert!(decode(broken).is_err());
            assert_eq!(
                decode(&optimise(original).unwrap()).unwrap(),
                decode(original).unwrap()
            );
        }
    }

    #[test]
    fn encoded_audio_shares_pages_up_to_the_ogg_limit() {
        // A page holds up to 255 segments of 255 bytes; this second of noise (about 12 KB) fits one.
        let mut state = 1u32;
        let noise = (0..48000)
            .map(|_| {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                (state >> 8) as f32 / (1 << 24) as f32 - 0.5
            })
            .collect();
        let pcm = Pcm {
            rate: 48000,
            channels: vec![noise],
        };
        let bytes = encode(&pcm, 0.4).unwrap();
        let mut packets = PacketReader::new(Cursor::new(&bytes));
        let mut pages = BTreeSet::new();
        while let Some(packet) = packets.read_packet().unwrap() {
            pages.insert(packet.absgp_page());
        }
        // The headers' pages carry granule 0; the single audio page carries the last sample.
        assert_eq!(pages.into_iter().collect::<Vec<_>>(), [0, 48000]);
    }

    #[test]
    fn mid_stream_granules_trim_like_jorbis() {
        // One 56 000-sample stream, its middle page's granule lowered or raised by 128.
        // JOrbis 0.0.17 believes each page's granule and trims only the last packet.
        let low = decode(include_bytes!("fixtures/vorbis-granule-low.ogg")).unwrap();
        let high = decode(include_bytes!("fixtures/vorbis-granule-high.ogg")).unwrap();
        assert_eq!(low.frames(), 56000);
        assert_eq!(high.frames(), 55936);
        assert_eq!(high.channels[0], low.channels[0][..55936]);
    }

    #[test]
    fn long_first_block_returns_what_jorbis_returns() {
        // The low-granule stream without its first two or three audio packets: both start on a
        // long block, whose own previous-window flag is short then long. Frame counts and the
        // first 448 samples are JOrbis 0.0.17's.
        for (pcm, jorbis, frames) in [
            (
                include_bytes!("fixtures/vorbis-long-first-after-short.ogg").as_slice(),
                include_bytes!("fixtures/vorbis-long-first-after-short.jorbis-f32le").as_slice(),
                55808,
            ),
            (
                include_bytes!("fixtures/vorbis-long-first-after-long.ogg"),
                include_bytes!("fixtures/vorbis-long-first-after-long.jorbis-f32le"),
                54784,
            ),
        ] {
            let pcm = decode(pcm).unwrap();
            assert_eq!(
                (pcm.rate, pcm.channels.len(), pcm.frames()),
                (48000, 1, frames)
            );
            let jorbis = jorbis
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b));
            for (i, (expected, &actual)) in jorbis.zip(&pcm.channels[0]).enumerate() {
                assert!(
                    (expected - actual).abs() <= 1e-5,
                    "{frames}, sample {i}: {expected} != {actual}"
                );
            }
        }
    }

    #[test]
    fn lossless_optimisation_reduces_client_safe_fixture() {
        let input = include_bytes!("fixtures/vorbis-client.ogg");
        let output = optimise(input).unwrap();
        assert!(output.len() < input.len());
        assert_eq!(decode(&output).unwrap(), decode(input).unwrap());
    }

    #[test]
    fn client_packet_check_includes_comment_framing() {
        // JOrbis rejects the zero framing bit; Symphonia's metadata reader ignores it.
        let valid = include_bytes!("fixtures/vorbis-client.ogg");
        let invalid = include_bytes!("fixtures/vorbis-comment-framing.ogg");
        assert!(!same_client_packets(valid, invalid).unwrap());
    }

    #[test]
    fn unproven_multichannel_stream_keeps_input() {
        let input = encode(&tone(48000, 6), 0.4).unwrap();
        assert_eq!(optimise(&input).unwrap(), input);
    }

    #[test]
    fn client_rewrite_rejects_unproven_headers() {
        let input = include_bytes!("fixtures/vorbis-client.ogg");
        let malformed = include_bytes!("fixtures/vorbis-comment-framing.ogg");
        assert_eq!(optimise(malformed).unwrap(), malformed);
        assert!(
            ogg::compatible_rewrite(input, malformed, OGG_STREAM_SERIAL)
                .unwrap()
                .is_none()
        );

        let mut packets = PacketReader::new(Cursor::new(input));
        packets.read_packet().unwrap();
        packets.read_packet().unwrap();
        let setup = packets.read_packet().unwrap().unwrap().data;
        let mut changed = setup.clone();
        let tail = changed.len() - 2;
        changed[tail] ^= 1; // Floor, residue, mapping and mode bits must remain identical.
        assert!(!vorbis_client::same_setup(&setup, &changed).unwrap_or(false));
    }

    #[test]
    fn vorbis_round_trip_lossless_optimisation_and_serials() {
        let pcm = tone(48000, 2);
        let bytes = encode(&pcm, 0.4).unwrap();
        assert_eq!(bytes, encode(&pcm, 0.4).unwrap());
        let decoded = decode(&bytes).unwrap();
        assert_eq!(
            (decoded.rate, decoded.channels.len(), decoded.frames()),
            (48000, 2, 9600)
        );
        let out = optimise(&bytes).unwrap();
        assert_eq!(out, optimise(&bytes).unwrap());
        assert_eq!(decode(&out).unwrap(), decoded);
        assert_eq!(u32::from_le_bytes(out[14..18].try_into().unwrap()), 1);
        assert!(out.len() <= bytes.len());
        assert!(decode(&bytes[..bytes.len() - 5]).is_err());
    }

    #[test]
    fn mixing_and_resampling_keep_duration_and_trim_delay() {
        let input = tone(48000, 2);
        let mono = downmix(&input).unwrap();
        assert_eq!(mono.channels[0], input.channels[0]);
        let half = resample(&mono, 24000).unwrap();
        assert_eq!(half.frames(), 4800);
        let expected = tone(24000, 1);
        let mse = half.channels[0]
            .iter()
            .zip(&expected.channels[0])
            .map(|(&a, &b)| f64::from(a - b).powi(2))
            .sum::<f64>()
            / half.frames() as f64;
        assert!(mse < 1e-4, "mse={mse}");
    }

    #[test]
    fn silent_stream_preserves_duration() {
        let pcm = Pcm {
            rate: 48000,
            channels: vec![vec![0.0; 9600]],
        };
        let bytes = encode(&pcm, 1.0).unwrap();
        let out = optimise_empty(&bytes).unwrap();
        assert_eq!(decode(&out).unwrap(), pcm);
        assert!(out.len() <= bytes.len());
    }
}
