use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};

use camino::Utf8Path;
use sha1::{Digest, Sha1};

use crate::{Encoded, LayoutOptions, Written, ZipError, ZipMode, refuse, validate_path};

struct Prepared<'a> {
    name: &'a str,
    encoded: &'a Encoded,
}

impl Prepared<'_> {
    fn flags(&self) -> u16 {
        if self.name.is_ascii() { 0 } else { 0x800 }
    }
}

pub(crate) fn assemble(
    entries: &BTreeMap<String, Encoded>,
    out: &Utf8Path,
    layout: &LayoutOptions,
) -> Result<Written, ZipError> {
    let comment = layout.comment.as_deref().unwrap_or("").as_bytes();
    if comment.len() > u16::MAX as usize {
        return Err(refuse(out.as_str(), "zip comment exceeds 65535 bytes"));
    }
    for (name, encoded) in entries {
        validate_path(name)?;
        if name.len() > u16::MAX as usize {
            return Err(refuse(name, "path exceeds 65535 bytes"));
        }
        zip32(encoded.size, name)?;
        zip32(encoded.payload.len() as u64, name)?;
        match encoded.method {
            0 if encoded.payload.len() as u64 == encoded.size => {}
            8 if !encoded.payload.is_empty() => {}
            0 | 8 => return Err(refuse(name, "payload length is inconsistent with method")),
            _ => return Err(refuse(name, "unsupported compression method")),
        }
    }
    let prepared: Vec<_> = entries
        .iter()
        .map(|(name, encoded)| Prepared { name, encoded })
        .collect();

    let partial = out.with_file_name(format!("{}.partial", out.file_name().unwrap_or("output")));
    match fs::remove_file(&partial) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(path_error(&partial, error)),
    }
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)
        .map_err(|error| path_error(&partial, error))?;
    let result = (|| {
        let mut output = Output {
            file: BufWriter::new(file),
            sha1: Sha1::new(),
            position: 0,
        };
        write_records(&mut output, &prepared, layout.mode, comment)?;
        output.file.flush()?;
        output.file.get_ref().sync_all()?;
        let written = Written {
            sha1: output.sha1.finalize().into(),
            bytes: output.position,
        };
        drop(output.file);
        fs::rename(&partial, out).map_err(|error| path_error(out, error))?;
        Ok(written)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&partial);
    }
    result.map_err(|error| match error {
        ZipError::Io(error) => path_error(&partial, error),
        other => other,
    })
}

fn path_error(path: &Utf8Path, error: io::Error) -> ZipError {
    ZipError::Io(io::Error::new(error.kind(), format!("{path}: {error}")))
}

fn write_records(
    output: &mut Output,
    entries: &[Prepared<'_>],
    mode: ZipMode,
    comment: &[u8],
) -> Result<(), ZipError> {
    let mut shared = BTreeMap::new();
    let mut offsets = Vec::with_capacity(entries.len());
    for entry in entries {
        let payload = entry.encoded.payload.as_ref();
        let key = (
            entry.encoded.method,
            entry.encoded.crc,
            entry.encoded.size,
            payload,
        );
        if mode.shares_data()
            && let Some(&offset) = shared.get(&key)
        {
            offsets.push(offset);
            continue;
        }
        let offset = zip32(output.position, entry.name)?;
        offsets.push(offset);
        let (name, flags) = mode.local_name(entry.name, entry.flags());
        output.u32s(&[0x04034b50])?;
        output.u16s(&[20, flags, entry.encoded.method, 0, 0x0041])?;
        output.u32s(&[
            entry.encoded.crc,
            zip32(payload.len() as u64, entry.name)?,
            entry.encoded.size as u32,
        ])?;
        output.u16s(&[name.len() as u16, 0])?;
        output.bytes(name)?;
        output.bytes(payload)?;
        if mode.shares_data() {
            shared.insert(key, offset);
        }
    }
    let central_start = zip32(output.position, "<archive>")?;
    for (entry, offset) in entries.iter().zip(offsets) {
        output.u32s(&[0x02014b50])?;
        output.u16s(&[20, 20, entry.flags(), entry.encoded.method, 0, 0x0041])?;
        output.u32s(&[
            entry.encoded.crc,
            zip32(entry.encoded.payload.as_ref().len() as u64, entry.name)?,
            entry.encoded.size as u32,
        ])?;
        output.u16s(&[entry.name.len() as u16, 0, 0, 0, 0])?;
        output.u32s(&[0, offset])?;
        output.bytes(entry.name.as_bytes())?;
    }
    let central_size = zip32(output.position - u64::from(central_start), "<archive>")?;
    let count = entries.len() as u64;
    if count >= u64::from(u16::MAX) {
        let zip64_offset = output.position;
        output.u32s(&[0x06064b50])?;
        output.bytes(&44_u64.to_le_bytes())?;
        output.u16s(&[45, 45])?;
        output.u32s(&[0, 0])?;
        for value in [
            count,
            count,
            u64::from(central_size),
            u64::from(central_start),
        ] {
            output.bytes(&value.to_le_bytes())?;
        }
        output.u32s(&[0x07064b50, 0])?;
        output.bytes(&zip64_offset.to_le_bytes())?;
        output.u32s(&[1])?;
    }
    output.u32s(&[0x06054b50])?;
    output.u16s(&[
        0,
        0,
        count.min(u64::from(u16::MAX)) as u16,
        count.min(u64::from(u16::MAX)) as u16,
    ])?;
    output.u32s(&[central_size, central_start])?;
    output.u16s(&[comment.len() as u16])?;
    output.bytes(comment)
}

fn zip32(value: u64, name: &str) -> Result<u32, ZipError> {
    if value >= u64::from(u32::MAX) {
        return Err(refuse(
            name,
            "ZIP64 entry sizes and offsets are not supported",
        ));
    }
    Ok(value as u32)
}

struct Output {
    file: BufWriter<File>,
    sha1: Sha1,
    position: u64,
}

impl Output {
    fn bytes(&mut self, bytes: &[u8]) -> Result<(), ZipError> {
        self.file.write_all(bytes)?;
        self.sha1.update(bytes);
        self.position += bytes.len() as u64;
        Ok(())
    }

    fn u16s(&mut self, values: &[u16]) -> Result<(), ZipError> {
        for value in values {
            self.bytes(&value.to_le_bytes())?;
        }
        Ok(())
    }

    fn u32s(&mut self, values: &[u32]) -> Result<(), ZipError> {
        for value in values {
            self.bytes(&value.to_le_bytes())?;
        }
        Ok(())
    }
}
