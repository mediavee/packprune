use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, Cursor, Read};

use camino::Utf8Path;
use zip::{CompressionMethod, ZipArchive};

use crate::{Limits, ZipError, refuse, validate_path};

pub(crate) fn read(
    path: &Utf8Path,
    limits: &Limits,
    select: &dyn Fn(&str) -> bool,
) -> Result<BTreeMap<String, Vec<u8>>, ZipError> {
    let file = File::open(path)?;
    if file.metadata()?.len() == 0 {
        return Err(refuse(path.as_str(), "empty zip archive"));
    }
    // SAFETY: the mapping is read-only and stays alive until every entry has been copied out.
    let mapped = unsafe { memmap2::Mmap::map(&file)? };
    let mut archive = ZipArchive::new(Cursor::new(&mapped[..]))
        .map_err(|error| refuse(path.as_str(), error.to_string()))?;
    let mut records = check_directory(&mapped, archive.central_directory_start(), limits)?;
    let mut total = 0_u64;
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
        if !matches!(
            entry.compression(),
            CompressionMethod::Stored | CompressionMethod::Deflated
        ) {
            return Err(refuse(name, "unsupported compression method"));
        }
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
    }

    if let Some((_, name)) = records.first_key_value() {
        return Err(refuse(name, "central-directory entry is not indexed"));
    }

    let mut files = BTreeMap::new();
    for index in 0..archive.len() {
        let metadata = archive
            .by_index_data(index)
            .map_err(|error| refuse(path.as_str(), error.to_string()))?;
        let name = std::str::from_utf8(metadata.name_raw())
            .expect("validated UTF-8 path")
            .to_owned();
        let size = metadata.size();
        let keep = !name.ends_with('/') && select(&name);
        let mut entry = archive
            .by_index(index)
            .map_err(|error| refuse(&name, error.to_string()))?;
        // Bound the read as well as the declaration, including when the CRC matches a lying size. An entry not kept is
        // inflated all the same, through a small buffer, so its size and CRC are checked as a kept one's.
        let mut bounded = (&mut entry).take(size.saturating_add(1));
        let mut bytes = Vec::new();
        let inflated = if keep {
            bytes.reserve_exact(size as usize);
            bounded.read_to_end(&mut bytes).map(|read| read as u64)
        } else {
            io::copy(&mut bounded, &mut io::sink())
        }
        .map_err(|error| refuse(&name, error.to_string()))?;
        if inflated != size {
            return Err(refuse(&name, "inflated size differs from declared size"));
        }
        if keep {
            files.insert(name, bytes);
        }
    }
    Ok(files)
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
