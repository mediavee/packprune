use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{Cursor, Read};

use camino::Utf8Path;
use flate2::read::DeflateDecoder;
use rayon::prelude::*;
use zip::{CompressionMethod, ZipArchive};

use crate::{Limits, ZipError, refuse, validate_path};

struct Job {
    name: String,
    header_start: u64,
    compressed_size: u64,
    size: u64,
    crc: u32,
    deflated: bool,
    keep: bool,
}

pub(crate) fn read(
    path: &Utf8Path,
    limits: &Limits,
    select: &dyn Fn(&str) -> bool,
    verify_unselected: bool,
) -> Result<BTreeMap<String, Vec<u8>>, ZipError> {
    let file = File::open(path)?;
    if file.metadata()?.len() == 0 {
        return Err(refuse(path.as_str(), "empty zip archive"));
    }
    // SAFETY: the mapping is read-only and stays alive until every entry has been copied out.
    let mapped = unsafe { memmap2::Mmap::map(&file)? };
    let archive = ZipArchive::new(Cursor::new(&mapped[..]))
        .map_err(|error| refuse(path.as_str(), error.to_string()))?;
    let mut records = check_directory(&mapped, archive.central_directory_start(), limits)?;
    let mut total = 0_u64;
    let mut jobs: Vec<Job> = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        let entry = archive
            .by_index_data(index)
            .map_err(|error| refuse(path.as_str(), error.to_string()))?;
        let name = records
            .remove(&entry.central_header_start())
            .expect("indexed central record was checked");
        if entry.name_raw() != name.as_bytes() {
            return Err(refuse(
                name,
                "Unicode extra field changes the central-directory path",
            ));
        }
        if entry.encrypted() || entry.flags().as_u16() & 0x40 != 0 {
            return Err(refuse(name, "encrypted entry"));
        }
        let deflated = match entry.compression() {
            CompressionMethod::Stored => false,
            CompressionMethod::Deflated => true,
            _ => return Err(refuse(name, "unsupported compression method")),
        };
        if entry.size() > limits.max_entry_bytes {
            return Err(refuse(name, "entry size limit exceeded"));
        }
        total = total
            .checked_add(entry.size())
            .filter(|&size| size <= limits.max_total_bytes)
            .ok_or_else(|| refuse(name, "total size limit exceeded"))?;
        if entry.size() > limits.ratio_min_entry_bytes
            && (entry.compressed_size() == 0
                || u128::from(entry.size())
                    > u128::from(entry.compressed_size()) * u128::from(limits.max_ratio))
        {
            return Err(refuse(name, "compression ratio limit exceeded"));
        }
        let keep = !name.ends_with('/') && select(name);
        if keep || verify_unselected {
            jobs.push(Job {
                name: name.to_owned(),
                header_start: entry.header_start(),
                compressed_size: entry.compressed_size(),
                size: entry.size(),
                crc: entry.crc32(),
                deflated,
                keep,
            });
        }
    }

    if let Some((_, name)) = records.first_key_value() {
        return Err(refuse(name, "central-directory entry is not indexed"));
    }

    drop(archive);
    type EntryResult = Result<Option<(String, Vec<u8>)>, ZipError>;
    let raw: &[u8] = &mapped[..];
    let results: Vec<EntryResult> = if jobs.len() >= 16 {
        jobs.into_par_iter()
            .map(|job| inflate_entry(raw, job))
            .collect()
    } else {
        jobs.into_iter()
            .map(|job| inflate_entry(raw, job))
            .collect()
    };

    let mut files = BTreeMap::new();
    for result in results {
        if let Some((name, bytes)) = result? {
            files.insert(name, bytes);
        }
    }
    Ok(files)
}

fn inflate_entry(raw: &[u8], job: Job) -> Result<Option<(String, Vec<u8>)>, ZipError> {
    let header_start = usize::try_from(job.header_start)
        .map_err(|_| refuse(&job.name, "invalid local header offset"))?;
    let header = raw
        .get(header_start..header_start.saturating_add(30))
        .ok_or_else(|| refuse(&job.name, "truncated local file header"))?;
    if &header[..4] != b"PK\x03\x04" {
        return Err(refuse(&job.name, "invalid local file header signature"));
    }
    let name_len = u16::from_le_bytes([header[26], header[27]]) as usize;
    let extra_len = u16::from_le_bytes([header[28], header[29]]) as usize;
    let data_start = header_start
        .checked_add(30)
        .and_then(|v| v.checked_add(name_len))
        .and_then(|v| v.checked_add(extra_len))
        .ok_or_else(|| refuse(&job.name, "local header offset overflow"))?;
    let compressed_size = usize::try_from(job.compressed_size)
        .map_err(|_| refuse(&job.name, "compressed entry exceeds memory"))?;
    let data_end = data_start
        .checked_add(compressed_size)
        .ok_or_else(|| refuse(&job.name, "entry payload offset overflow"))?;
    let payload = raw
        .get(data_start..data_end)
        .ok_or_else(|| refuse(&job.name, "truncated entry payload"))?;

    if !job.deflated {
        if payload.len() as u64 != job.size {
            return Err(refuse(
                &job.name,
                "inflated size differs from declared size",
            ));
        }
        if crc32fast::hash(payload) != job.crc {
            return Err(refuse(&job.name, "invalid checksum"));
        }
        return Ok(job.keep.then(|| (job.name, payload.to_vec())));
    }

    // Bound the read as well as the declaration, including when the CRC matches a lying size.
    let mut decoder = DeflateDecoder::new(Cursor::new(payload));
    if job.keep {
        let size_usize = usize::try_from(job.size)
            .map_err(|_| refuse(&job.name, "entry size exceeds memory"))?;
        let mut bytes = vec![0u8; size_usize];
        let mut filled = 0usize;
        while filled < size_usize {
            let n = decoder
                .read(&mut bytes[filled..])
                .map_err(|error| refuse(&job.name, error.to_string()))?;
            if n == 0 {
                return Err(refuse(
                    &job.name,
                    "inflated size differs from declared size",
                ));
            }
            filled += n;
        }
        let mut extra = [0u8; 1];
        if decoder
            .read(&mut extra)
            .map_err(|error| refuse(&job.name, error.to_string()))?
            != 0
        {
            return Err(refuse(
                &job.name,
                "inflated size differs from declared size",
            ));
        }
        if crc32fast::hash(&bytes) != job.crc {
            return Err(refuse(&job.name, "invalid checksum"));
        }
        Ok(Some((job.name, bytes)))
    } else {
        let mut bounded = decoder.take(job.size.saturating_add(1));
        let mut hasher = crc32fast::Hasher::new();
        let mut buf = [0u8; 32 * 1024];
        let mut inflated = 0u64;
        loop {
            let n = bounded
                .read(&mut buf)
                .map_err(|error| refuse(&job.name, error.to_string()))?;
            if n == 0 {
                break;
            }
            inflated += n as u64;
            hasher.update(&buf[..n]);
        }
        if inflated != job.size {
            return Err(refuse(
                &job.name,
                "inflated size differs from declared size",
            ));
        }
        if hasher.finalize() != job.crc {
            return Err(refuse(&job.name, "invalid checksum"));
        }
        Ok(None)
    }
}

fn check_directory<'a>(
    bytes: &'a [u8],
    start: u64,
    limits: &Limits,
) -> Result<BTreeMap<u64, &'a str>, ZipError> {
    let mut position =
        usize::try_from(start).map_err(|_| refuse("<archive>", "invalid directory offset"))?;
    let mut names = BTreeSet::new();
    let mut records = BTreeMap::new();
    let mut count = 0_u64;
    // zip indexes by name and overwrites duplicates; inspect the original records before using its index.
    while bytes.get(position..position.saturating_add(4)) == Some(b"PK\x01\x02") {
        let header = bytes
            .get(position..position.saturating_add(46))
            .ok_or_else(|| refuse("<archive>", "truncated central directory"))?;
        let name_len = u16::from_le_bytes([header[28], header[29]]) as usize;
        let extra_len = u16::from_le_bytes([header[30], header[31]]) as usize;
        let comment_len = u16::from_le_bytes([header[32], header[33]]) as usize;
        let end = position
            .checked_add(46 + name_len + extra_len + comment_len)
            .filter(|&end| end <= bytes.len())
            .ok_or_else(|| refuse("<archive>", "truncated central directory"))?;
        let raw = &bytes[position + 46..position + 46 + name_len];
        let name = std::str::from_utf8(raw)
            .map_err(|_| refuse(&String::from_utf8_lossy(raw), "path is not UTF-8"))?;
        validate_path(name)?;
        count += 1;
        if count > u64::from(limits.max_entries) {
            return Err(refuse(name, "entry count limit exceeded"));
        }
        if !names.insert(name) {
            return Err(refuse(name, "duplicate path"));
        }
        records.insert(position as u64, name);
        position = end;
    }
    Ok(records)
}
