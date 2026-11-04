#![no_main]

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use libfuzzer_sys::fuzz_target;
use packprune_zip::{CompressOptions, EntryClass, LayoutOptions, Limits, ZipMode};

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

fn temp_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "packprune-zip-fuzz-{}-{}.zip",
        std::process::id(),
        NEXT_FILE.fetch_add(1, Ordering::Relaxed)
    ))
}

fn read_bytes(data: &[u8]) {
    let path = temp_path();
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    file.write_all(data).unwrap();
    drop(file);
    let limits = Limits {
        max_entries: 128,
        max_total_bytes: 256 << 10,
        max_entry_bytes: 64 << 10,
        max_ratio: 100,
        ratio_min_entry_bytes: 1024,
    };
    let _ = packprune_zip::read(path.to_str().unwrap().as_ref(), &limits);
    fs::remove_file(path).unwrap();
}

fuzz_target!(|data: &[u8]| {
    read_bytes(data);

    // `data` split on `|` gives the contents; every other piece is stored twice so `lean` shares data. Each mode's
    // output must read back exactly, and a copy with `data`-chosen bytes flipped must be read or refused, never panic.
    let mut files = BTreeMap::new();
    for (i, piece) in data.split(|&byte| byte == b'|').take(16).enumerate() {
        files.insert(format!("d{}/f{i}", i % 3), piece.to_vec());
        if i % 2 == 0 {
            files.insert(format!("copy/f{i}"), piece.to_vec());
        }
    }
    let options = CompressOptions {
        effort: data.first().map_or(0, |&byte| byte & 1),
        recompress_compressed: true,
    };
    let encoded = files
        .iter()
        .map(|(name, bytes)| {
            (
                name.clone(),
                packprune_zip::encode(bytes, EntryClass::Other, &options),
            )
        })
        .collect();
    for mode in [ZipMode::Conformant, ZipMode::Lean] {
        let path = temp_path();
        let out = path.to_str().unwrap().as_ref();
        packprune_zip::assemble(
            &encoded,
            out,
            &LayoutOptions {
                mode,
                comment: None,
            },
        )
        .unwrap();
        assert_eq!(packprune_zip::read(out, &Limits::default()).unwrap(), files);
        let mut bytes = fs::read(&path).unwrap();
        fs::remove_file(&path).unwrap();
        let len = bytes.len();
        for flip in data.chunks_exact(3).take(8) {
            bytes[usize::from(u16::from_le_bytes([flip[0], flip[1]])) % len] ^= flip[2];
        }
        read_bytes(&bytes);
    }
});
