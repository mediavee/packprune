use super::*;
use std::fs;
use std::io::Write;

use flate2::{Compression, write::DeflateEncoder};
use rayon::ThreadPoolBuilder;
use sha1::{Digest, Sha1};
use tempfile::tempdir;

struct Member<'a> {
    name: &'a [u8],
    bytes: Vec<u8>,
    size: u32,
    method: u16,
    flags: u16,
    crc: u32,
    extra: Vec<u8>,
}

impl<'a> Member<'a> {
    fn stored(name: &'a [u8], bytes: &[u8]) -> Self {
        Self {
            name,
            bytes: bytes.to_vec(),
            size: bytes.len() as u32,
            method: 0,
            flags: 0,
            crc: crc32fast::hash(bytes),
            extra: Vec::new(),
        }
    }

    fn deflated(name: &'a [u8], bytes: &[u8]) -> Self {
        let mut member = Self::stored(name, bytes);
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(bytes).unwrap();
        member.bytes = encoder.finish().unwrap();
        member.method = 8;
        member
    }
}

fn u16s(out: &mut Vec<u8>, fields: &[u16]) {
    for field in fields {
        out.extend(field.to_le_bytes());
    }
}

fn u32s(out: &mut Vec<u8>, fields: &[u32]) {
    for field in fields {
        out.extend(field.to_le_bytes());
    }
}

fn fixture(members: &[Member<'_>]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut offsets = Vec::new();
    for m in members {
        offsets.push(out.len() as u32);
        u32s(&mut out, &[0x04034b50]);
        u16s(&mut out, &[20, m.flags, m.method, 0, 0x0041]);
        u32s(&mut out, &[m.crc, m.bytes.len() as u32, m.size]);
        u16s(&mut out, &[m.name.len() as u16, 0]);
        out.extend(m.name);
        out.extend(&m.bytes);
    }
    let start = out.len() as u32;
    for (m, offset) in members.iter().zip(offsets) {
        u32s(&mut out, &[0x02014b50]);
        u16s(&mut out, &[20, 20, m.flags, m.method, 0, 0x0041]);
        u32s(&mut out, &[m.crc, m.bytes.len() as u32, m.size]);
        u16s(
            &mut out,
            &[m.name.len() as u16, m.extra.len() as u16, 0, 0, 0],
        );
        u32s(&mut out, &[0, offset]);
        out.extend(m.name);
        out.extend(&m.extra);
    }
    let size = out.len() as u32 - start;
    u32s(&mut out, &[0x06054b50]);
    u16s(
        &mut out,
        &[0, 0, members.len() as u16, members.len() as u16],
    );
    u32s(&mut out, &[size, start]);
    u16s(&mut out, &[0]);
    out
}

fn read_fixture(bytes: &[u8], limits: &Limits) -> Result<BTreeMap<String, Vec<u8>>, ZipError> {
    let dir = tempdir().unwrap();
    let path = dir.path().join("input.zip");
    fs::write(&path, bytes).unwrap();
    read(Utf8Path::from_path(&path).unwrap(), limits)
}

fn refused(bytes: &[u8], limits: &Limits, name: &str, reason: &str) {
    match read_fixture(bytes, limits) {
        Err(ZipError::Refused { entry, why }) => {
            assert_eq!(entry, name);
            assert!(why.contains(reason), "{why:?} does not contain {reason:?}");
        }
        other => panic!("expected named refusal, got {other:?}"),
    }
}

#[test]
fn reads_stored_deflated_utf8_and_directories() {
    let bytes = fixture(&[
        Member::stored(b"assets/", b""),
        Member::stored("assets/été.txt".as_bytes(), b"hello"),
        Member::deflated(b"pack.mcmeta", b"{\"pack\":{}}"),
    ]);
    let files = read_fixture(&bytes, &Limits::default()).unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files["assets/été.txt"], b"hello");
    assert_eq!(files["pack.mcmeta"], b"{\"pack\":{}}");
}

#[test]
fn a_selected_read_keeps_only_what_it_selects_yet_checks_every_entry() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("input.zip");
    let path = Utf8Path::from_path(&path).unwrap();
    let assets = |name: &str| name.starts_with("assets/");
    let read = |members: Vec<Member<'_>>, limits: &Limits| {
        fs::write(path, fixture(&members)).unwrap();
        read_selected(path, limits, &assets)
    };
    let kept = || Member::deflated(b"assets/a.txt", b"kept");
    let class = || Member::deflated(b"classes/Liar.class", &[b'a'; 64]);

    let files = read(vec![kept(), class()], &Limits::default()).unwrap();
    assert_eq!(files.keys().collect::<Vec<_>>(), ["assets/a.txt"]);
    assert_eq!(files["assets/a.txt"], b"kept");

    let mut lying = class();
    lying.size = 1;
    let mut corrupt = class();
    corrupt.crc ^= 1;
    let too_big = Limits {
        max_entry_bytes: 5,
        ..Limits::default()
    };
    for (members, limits, reason) in [
        (vec![kept(), lying], Limits::default(), "size"),
        (vec![kept(), corrupt], Limits::default(), "checksum"),
        (vec![kept(), class()], too_big, "entry size"),
    ] {
        match read(members, &limits) {
            Err(ZipError::Refused { entry, why }) => {
                assert_eq!(entry, "classes/Liar.class");
                assert!(why.contains(reason), "{why:?} does not contain {reason:?}");
            }
            other => panic!("expected the unselected entry refused for {reason}, got {other:?}"),
        }
    }
}

#[test]
fn refuses_every_unsafe_path_and_duplicate_by_name() {
    for name in [
        "../escape",
        "a/../escape",
        "a/./b",
        "/absolute",
        "a\\b",
        "a\0b",
        "C:drive",
        "c:/drive",
        "",
        "a//b",
    ] {
        refused(
            &fixture(&[Member::stored(name.as_bytes(), b"")]),
            &Limits::default(),
            name,
            "path",
        );
    }
    refused(
        &fixture(&[Member::stored(b"bad\xff", b"")]),
        &Limits::default(),
        "bad�",
        "UTF-8",
    );
    refused(
        &fixture(&[
            Member::stored(b"same", b"one"),
            Member::stored(b"same", b"two"),
        ]),
        &Limits::default(),
        "same",
        "duplicate",
    );
}

#[test]
fn refuses_entry_count_including_directory_records() {
    let bytes = fixture(&[
        Member::stored(b"dir/", b""),
        Member::stored(b"second", b"x"),
    ]);
    refused(
        &bytes,
        &Limits {
            max_entries: 1,
            ..Limits::default()
        },
        "second",
        "entry count",
    );
}

#[test]
fn refuses_entry_and_total_sizes_before_inflation() {
    let bytes = fixture(&[
        Member::stored(b"first", b"123"),
        Member::stored(b"second", b"456"),
    ]);
    refused(
        &bytes,
        &Limits {
            max_entry_bytes: 2,
            ..Limits::default()
        },
        "first",
        "entry size",
    );
    refused(
        &bytes,
        &Limits {
            max_total_bytes: 5,
            ..Limits::default()
        },
        "second",
        "total size",
    );
}

#[test]
fn ratio_threshold_and_limit_are_inclusive() {
    let bytes = fixture(&[Member::deflated(b"bomb", &vec![b'a'; 4096])]);
    refused(
        &bytes,
        &Limits {
            max_ratio: 2,
            ratio_min_entry_bytes: 4095,
            ..Limits::default()
        },
        "bomb",
        "ratio",
    );
    assert!(
        read_fixture(
            &bytes,
            &Limits {
                max_ratio: 0,
                ratio_min_entry_bytes: 4096,
                ..Limits::default()
            }
        )
        .is_ok()
    );
    let stored = fixture(&[Member::stored(b"equal", b"123")]);
    assert!(
        read_fixture(
            &stored,
            &Limits {
                max_entries: 1,
                max_total_bytes: 3,
                max_entry_bytes: 3,
                max_ratio: 1,
                ratio_min_entry_bytes: 0
            }
        )
        .is_ok()
    );
    let mut empty_compressed = Member::stored(b"zero-compressed", b"");
    empty_compressed.size = 2;
    refused(
        &fixture(&[empty_compressed]),
        &Limits {
            ratio_min_entry_bytes: 1,
            ..Limits::default()
        },
        "zero-compressed",
        "ratio",
    );
}

#[test]
fn refuses_lying_sizes_in_both_directions_and_checks_crc() {
    for deflate in [false, true] {
        for size in [1, 8193] {
            let mut m = if deflate {
                Member::deflated(b"liar", &vec![b'a'; 8192])
            } else {
                Member::stored(b"liar", &vec![b'a'; 8192])
            };
            m.size = size;
            refused(&fixture(&[m]), &Limits::default(), "liar", "size");
        }
    }
    let mut m = Member::deflated(b"corrupt", b"hello world");
    m.crc ^= 1;
    refused(&fixture(&[m]), &Limits::default(), "corrupt", "checksum");
}

#[test]
fn refuses_encryption_and_unknown_methods_by_name() {
    let mut encrypted = Member::stored(b"secret", b"secret");
    encrypted.flags = 1;
    refused(
        &fixture(&[encrypted]),
        &Limits::default(),
        "secret",
        "encrypted",
    );
    for method in [12, 99, 65535] {
        let mut unknown = Member::stored(b"unsupported", b"data");
        unknown.method = method;
        refused(
            &fixture(&[unknown]),
            &Limits::default(),
            "unsupported",
            "method",
        );
    }
}

#[test]
fn malformed_archives_return_refused() {
    for bytes in [b"".as_slice(), b"not a zip", b"PK\x05\x06"] {
        assert!(matches!(
            read_fixture(bytes, &Limits::default()),
            Err(ZipError::Refused { .. })
        ));
    }
}

fn entries() -> BTreeMap<String, (Arc<[u8]>, EntryClass)> {
    BTreeMap::from([
        (
            "a.json".into(),
            (Arc::from(b"{\"value\":123}".repeat(100)), EntryClass::Text),
        ),
        (
            "b.json".into(),
            (Arc::from(b"{\"value\":123}".repeat(100)), EntryClass::Text),
        ),
        ("empty".into(), (Arc::from([]), EntryClass::Other)),
        (
            "été.txt".into(),
            (Arc::from(b"hello".as_slice()), EntryClass::Text),
        ),
    ])
}

#[test]
fn writer_roundtrips_with_fixed_headers_and_sha1() {
    let dir = tempdir().unwrap();
    let out = Utf8Path::from_path(dir.path()).unwrap().join("out.zip");
    let input = entries();
    let written = write(
        &input,
        &out,
        &WriteOptions {
            effort: 1,
            comment: Some("comment".into()),
            ..WriteOptions::default()
        },
    )
    .unwrap();
    let bytes = fs::read(&out).unwrap();
    assert_eq!(written.bytes, bytes.len() as u64);
    assert_eq!(written.sha1, <[u8; 20]>::from(Sha1::digest(&bytes)));
    assert!(!out.with_extension("zip.partial").exists());
    let actual = read(&out, &Limits::default()).unwrap();
    for (path, (blob, _)) in &input {
        assert_eq!(&actual[path], blob.as_ref());
    }
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).unwrap();
    assert_eq!(archive.comment(), b"comment");
    archive.validate_header_consistency().unwrap();
    assert_eq!(
        archive.file_names_raw().collect::<Vec<_>>(),
        input.keys().map(|s| s.as_bytes()).collect::<Vec<_>>()
    );
    for i in 0..archive.len() {
        let f = archive.by_index(i).unwrap();
        let time = f.last_modified().unwrap();
        assert_eq!(
            (
                time.year(),
                time.month(),
                time.day(),
                time.hour(),
                time.minute(),
                time.second()
            ),
            (1980, 2, 1, 0, 0, 0)
        );
        assert_eq!(f.external_attributes(), 0);
        let central = f.central_header_start() as usize;
        assert_eq!(&bytes[central + 6..central + 8], &20_u16.to_le_bytes());
        assert_eq!(
            f.flags().as_u16(),
            if f.name_raw().is_ascii() { 0 } else { 0x800 }
        );
        assert!(f.extra_data().is_none_or(|b| b.is_empty()));
    }
}

#[test]
fn lean_reuses_only_identical_payloads() {
    let dir = tempdir().unwrap();
    let out = Utf8Path::from_path(dir.path()).unwrap().join("lean.zip");
    let input = entries();
    write(
        &input,
        &out,
        &WriteOptions {
            mode: ZipMode::Lean,
            effort: 1,
            ..WriteOptions::default()
        },
    )
    .unwrap();
    let files = read(&out, &Limits::default()).unwrap();
    for (path, (blob, _)) in &input {
        assert_eq!(&files[path], blob.as_ref());
    }
    let archive = zip::ZipArchive::new(std::io::Cursor::new(fs::read(&out).unwrap())).unwrap();
    assert_eq!(
        archive.by_index_data(0).unwrap().header_start(),
        archive.by_index_data(1).unwrap().header_start()
    );
    assert_ne!(
        archive.by_index_data(0).unwrap().header_start(),
        archive.by_index_data(2).unwrap().header_start()
    );
}

#[test]
fn two_writes_with_one_and_eight_threads_are_byte_identical() {
    let dir = tempdir().unwrap();
    let root = Utf8Path::from_path(dir.path()).unwrap();
    for mode in [ZipMode::Conformant, ZipMode::Lean] {
        for effort in [0, 1, 255] {
            let mut previous = None;
            for threads in [1, 1, 8, 8] {
                let pool = ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .build()
                    .unwrap();
                let out = root.join("deterministic.zip");
                let result = pool
                    .install(|| {
                        write(
                            &entries(),
                            &out,
                            &WriteOptions {
                                mode,
                                effort,
                                ..WriteOptions::default()
                            },
                        )
                    })
                    .unwrap();
                let bytes = fs::read(out).unwrap();
                if let Some((sha1, prior)) = &previous {
                    assert_eq!(&result.sha1, sha1);
                    assert_eq!(&bytes, prior);
                }
                previous = Some((result.sha1, bytes));
            }
        }
    }
}

#[test]
fn writer_refuses_invalid_options_without_replacing_destination() {
    let dir = tempdir().unwrap();
    let out = Utf8Path::from_path(dir.path()).unwrap().join("out.zip");
    fs::write(&out, b"previous").unwrap();
    let options = WriteOptions {
        comment: Some("x".repeat(65536)),
        ..WriteOptions::default()
    };
    assert!(matches!(
        write(&entries(), &out, &options),
        Err(ZipError::Refused { .. })
    ));
    assert_eq!(fs::read(&out).unwrap(), b"previous");
    let invalid = BTreeMap::from([(
        "../bad".into(),
        (Arc::from(b"data".as_slice()), EntryClass::Other),
    )]);
    assert!(matches!(
        write(&invalid, &out, &WriteOptions::default()),
        Err(ZipError::Refused { .. })
    ));
    assert_eq!(fs::read(&out).unwrap(), b"previous");
}

#[test]
fn stored_and_compressed_file_policies_are_honoured() {
    let dir = tempdir().unwrap();
    let out = Utf8Path::from_path(dir.path()).unwrap().join("policy.zip");
    let data: Arc<[u8]> = Arc::from(b"hello hello hello hello\n".repeat(100));
    let entries = BTreeMap::from([
        ("text.json".into(), (Arc::clone(&data), EntryClass::Text)),
        (
            "image.png".into(),
            (Arc::clone(&data), EntryClass::Compressed),
        ),
        ("audio.ogg".into(), (data, EntryClass::Compressed)),
    ]);
    for (effort, recompress, methods) in [
        (0, true, [0, 0, 0]),
        (20, false, [0, 0, 8]),
        (255, false, [0, 0, 8]),
        (20, true, [8, 8, 8]),
    ] {
        write(
            &entries,
            &out,
            &WriteOptions {
                effort,
                recompress_compressed: recompress,
                ..WriteOptions::default()
            },
        )
        .unwrap();
        let archive = zip::ZipArchive::new(std::io::Cursor::new(fs::read(&out).unwrap())).unwrap();
        for (i, expected) in methods.iter().enumerate() {
            let entry = archive.by_index_data(i).unwrap();
            assert_eq!(
                match entry.compression() {
                    zip::CompressionMethod::Stored => 0,
                    zip::CompressionMethod::Deflated => 8,
                    _ => panic!("unexpected method"),
                },
                *expected
            );
        }
    }
}

#[test]
fn zip64_entry_count_roundtrips_without_entry_extra_fields() {
    let dir = tempdir().unwrap();
    let out = Utf8Path::from_path(dir.path()).unwrap().join("many.zip");
    let entries = (0..65536)
        .map(|i| (format!("{i:05}"), (Arc::from([]), EntryClass::Other)))
        .collect();
    write(
        &entries,
        &out,
        &WriteOptions {
            effort: 0,
            ..WriteOptions::default()
        },
    )
    .unwrap();
    assert_eq!(read(&out, &Limits::default()).unwrap().len(), 65536);
}

#[test]
fn stale_partial_is_replaced_and_removed_on_publication_failure() {
    let dir = tempdir().unwrap();
    let out = Utf8Path::from_path(dir.path()).unwrap().join("out.zip");
    let partial = out.with_extension("zip.partial");
    fs::write(&partial, b"interrupted write").unwrap();
    write(&entries(), &out, &WriteOptions::default()).unwrap();
    assert!(!partial.exists());
    assert_eq!(read(&out, &Limits::default()).unwrap().len(), 4);
    fs::remove_file(&out).unwrap();
    fs::create_dir(&out).unwrap();
    assert!(matches!(
        write(&entries(), &out, &WriteOptions::default()),
        Err(ZipError::Io(_))
    ));
    assert!(out.is_dir());
    assert!(!partial.exists());
}

#[test]
fn refuses_unicode_extra_fields_that_change_java_paths() {
    let mut member = Member::stored(b"safe", b"bytes");
    let mut value = vec![1];
    value.extend(crc32fast::hash(b"safe").to_le_bytes());
    value.extend(b"../unsafe");
    u16s(&mut member.extra, &[0x7075, value.len() as u16]);
    member.extra.extend(value);
    refused(&fixture(&[member]), &Limits::default(), "safe", "path");
}

#[test]
fn refuses_central_records_omitted_from_the_declared_count() {
    let mut bytes = fixture(&[
        Member::stored(b"first", b"1"),
        Member::stored(b"hidden", b"2"),
    ]);
    let end = bytes.len() - 22;
    bytes[end + 8..end + 10].copy_from_slice(&1_u16.to_le_bytes());
    bytes[end + 10..end + 12].copy_from_slice(&1_u16.to_le_bytes());
    refused(&bytes, &Limits::default(), "hidden", "indexed");
}

#[test]
fn default_options_deflate_every_eligible_entry() {
    let dir = tempdir().unwrap();
    let out = Utf8Path::from_path(dir.path()).unwrap().join("default.zip");
    let entries = BTreeMap::from([(
        "image.png".into(),
        (
            Arc::from(b"hello hello hello".repeat(100)),
            EntryClass::Compressed,
        ),
    )]);
    write(&entries, &out, &WriteOptions::default()).unwrap();
    let archive = zip::ZipArchive::new(std::io::Cursor::new(fs::read(&out).unwrap())).unwrap();
    assert_eq!(
        archive.by_index_data(0).unwrap().compression(),
        zip::CompressionMethod::Deflated
    );
}

#[test]
fn reader_does_not_double_declared_entry_allocation() {
    let files = read_fixture(
        &fixture(&[Member::deflated(b"one-mib", &vec![b'a'; 1 << 20])]),
        &Limits::default(),
    )
    .unwrap();
    let bytes = &files["one-mib"];
    assert_eq!(bytes.len(), 1 << 20);
    assert_eq!(bytes.capacity(), bytes.len());
}

#[cfg(unix)]
#[test]
fn stale_partial_symlink_does_not_touch_its_target() {
    let dir = tempdir().unwrap();
    let root = Utf8Path::from_path(dir.path()).unwrap();
    let out = root.join("out.zip");
    let target = root.join("target");
    fs::write(&target, b"keep").unwrap();
    std::os::unix::fs::symlink(&target, out.with_extension("zip.partial")).unwrap();
    write(&entries(), &out, &WriteOptions::default()).unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"keep");
    assert_eq!(read(&out, &Limits::default()).unwrap().len(), 4);
}

#[test]
fn compression_policy_uses_explicit_class_for_any_name() {
    let dir = tempdir().unwrap();
    let out = Utf8Path::from_path(dir.path())
        .unwrap()
        .join("classified.zip");
    let bytes: Arc<[u8]> = Arc::from(b"hello hello hello".repeat(100));
    let entries = BTreeMap::from([
        ("X.PNG".into(), (Arc::clone(&bytes), EntryClass::Compressed)),
        (
            "no-extension".into(),
            (Arc::clone(&bytes), EntryClass::Compressed),
        ),
        ("text.PNG".into(), (bytes, EntryClass::Text)),
    ]);
    write(
        &entries,
        &out,
        &WriteOptions {
            recompress_compressed: false,
            ..WriteOptions::default()
        },
    )
    .unwrap();
    let archive = zip::ZipArchive::new(std::io::Cursor::new(fs::read(&out).unwrap())).unwrap();
    for (index, method) in [
        zip::CompressionMethod::Stored,
        zip::CompressionMethod::Stored,
        zip::CompressionMethod::Deflated,
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(archive.by_index_data(index).unwrap().compression(), method);
    }
}

#[test]
fn cached_encodings_match_parallel_write_for_every_class() {
    let dir = tempdir().unwrap();
    let root = Utf8Path::from_path(dir.path()).unwrap();
    let bytes: Arc<[u8]> = Arc::from(b"{\"value\":123}".repeat(100));
    let input: BTreeMap<String, (Arc<[u8]>, EntryClass)> = BTreeMap::from([
        (
            "compressed".into(),
            (Arc::clone(&bytes), EntryClass::Compressed),
        ),
        ("other".into(), (Arc::clone(&bytes), EntryClass::Other)),
        ("text".into(), (bytes, EntryClass::Text)),
    ]);
    for mode in [ZipMode::Conformant, ZipMode::Lean] {
        for effort in [0, 20, 255] {
            let compression = CompressOptions {
                effort,
                recompress_compressed: false,
            };
            let encoded: BTreeMap<_, _> = input
                .iter()
                .map(|(name, (bytes, class))| (name.clone(), encode(bytes, *class, &compression)))
                .collect();
            let cached = encoded.clone();
            let layout = LayoutOptions {
                mode,
                comment: Some("cache".into()),
            };
            let cached_out = root.join("cached.zip");
            let expected = assemble(&cached, &cached_out, &layout).unwrap();
            let expected_bytes = fs::read(&cached_out).unwrap();
            for threads in [1, 8] {
                let pool = ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .build()
                    .unwrap();
                let out = root.join("fresh.zip");
                let actual = pool
                    .install(|| {
                        write(
                            &input,
                            &out,
                            &WriteOptions {
                                mode,
                                effort,
                                recompress_compressed: false,
                                comment: layout.comment.clone(),
                            },
                        )
                    })
                    .unwrap();
                assert_eq!(actual, expected);
                assert_eq!(fs::read(&out).unwrap(), expected_bytes);
            }
            let actual = read(&cached_out, &Limits::default()).unwrap();
            for (name, (bytes, _)) in &input {
                assert_eq!(&actual[name], bytes.as_ref());
            }
        }
    }
}

#[test]
fn assembler_refuses_invalid_cached_metadata_before_publication() {
    let dir = tempdir().unwrap();
    let out = Utf8Path::from_path(dir.path()).unwrap().join("out.zip");
    fs::write(&out, b"previous").unwrap();
    let stored = encode(
        b"hello",
        EntryClass::Other,
        &CompressOptions {
            effort: 0,
            ..CompressOptions::default()
        },
    );
    for (bad, reason) in [
        (
            Encoded {
                method: 99,
                ..stored.clone()
            },
            "method",
        ),
        (
            Encoded {
                size: 6,
                ..stored.clone()
            },
            "length",
        ),
        (
            Encoded {
                method: 8,
                payload: Arc::from([]),
                ..stored.clone()
            },
            "length",
        ),
        (
            Encoded {
                size: u64::from(u32::MAX),
                ..stored
            },
            "ZIP64",
        ),
    ] {
        match assemble(
            &BTreeMap::from([("cached".into(), bad)]),
            &out,
            &LayoutOptions::default(),
        ) {
            Err(ZipError::Refused { entry, why }) => {
                assert_eq!(entry, "cached");
                assert!(why.contains(reason), "{why}");
            }
            other => panic!("expected named refusal, got {other:?}"),
        }
        assert_eq!(fs::read(&out).unwrap(), b"previous");
        assert!(!out.with_extension("zip.partial").exists());
    }
}

#[test]
fn partial_io_errors_name_the_path() {
    let dir = tempdir().unwrap();
    let out = Utf8Path::from_path(dir.path()).unwrap().join("out.zip");
    let partial = out.with_extension("zip.partial");
    fs::create_dir(&partial).unwrap();
    let error = write(&entries(), &out, &WriteOptions::default()).unwrap_err();
    assert!(matches!(error, ZipError::Io(_)));
    assert!(error.to_string().contains(partial.as_str()), "{error}");
    assert!(partial.is_dir());
}

#[test]
fn encoder_tries_zopfli_on_every_class_at_maximum_effort() {
    let bytes = b"{\"value\":123}".repeat(100);
    let fast_options = CompressOptions::default();
    let max_options = CompressOptions {
        effort: 255,
        ..fast_options
    };
    let fast = encode(&bytes, EntryClass::Text, &fast_options);
    let max = encode(&bytes, EntryClass::Text, &max_options);
    assert!(
        max.payload.len() < fast.payload.len(),
        "Zopfli fixture must beat libdeflate"
    );
    assert_eq!(max.method, 8);
    assert_eq!(max.size, bytes.len() as u64);
    assert_eq!(max.crc, crc32fast::hash(&bytes));
    for class in [EntryClass::Other, EntryClass::Compressed] {
        assert_eq!(encode(&bytes, class, &max_options), max);
    }
}

#[test]
fn conformant_bytes_are_unchanged_by_the_modes() {
    let dir = tempdir().unwrap();
    let root = Utf8Path::from_path(dir.path()).unwrap();
    // Golden SHA-1s: the conformant layout stays byte for byte.
    for (effort, bytes, sha1) in [
        (0, 2984, "aa735a5017842a0022cd184893a6abd6aaec9935"),
        (20, 436, "c2bd161fbe4eefd57001dfa61866dc5c2d28556b"),
    ] {
        let out = root.join("conformant.zip");
        let written = write(
            &entries(),
            &out,
            &WriteOptions {
                effort,
                comment: Some("c".into()),
                ..WriteOptions::default()
            },
        )
        .unwrap();
        let hex: String = written.sha1.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!((written.bytes, hex.as_str()), (bytes, sha1));
    }
}

#[test]
fn lean_shares_data_and_empties_local_names_with_an_exact_directory() {
    let dir = tempdir().unwrap();
    let root = Utf8Path::from_path(dir.path()).unwrap();
    let input = entries();
    let mut sizes = Vec::new();
    for mode in [ZipMode::Conformant, ZipMode::Lean] {
        let out = root.join(format!("{mode:?}.zip"));
        let options = WriteOptions {
            mode,
            effort: 0,
            ..WriteOptions::default()
        };
        sizes.push(write(&input, &out, &options).unwrap().bytes);
        let actual = read(&out, &Limits::default()).unwrap();
        assert_eq!(actual.len(), input.len());
        for (path, (blob, _)) in &input {
            assert_eq!(&actual[path], blob.as_ref());
        }
        if mode != ZipMode::Lean {
            continue;
        }
        let bytes = fs::read(&out).unwrap();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).unwrap();
        assert_eq!(
            archive.file_names_raw().collect::<Vec<_>>(),
            input.keys().map(|s| s.as_bytes()).collect::<Vec<_>>()
        );
        let starts: Vec<_> = (0..archive.len())
            .map(|i| archive.by_index_data(i).unwrap().header_start())
            .collect();
        // a.json and b.json are identical: one local record; empty and été.txt have their own.
        assert_eq!(starts[0], starts[1]);
        assert_ne!(starts[1], starts[2]);
        assert_ne!(starts[2], starts[3]);
        for (i, (name, (blob, _))) in input.iter().enumerate() {
            let entry = archive.by_index(i).unwrap();
            assert_eq!(entry.crc32(), crc32fast::hash(blob));
            assert_eq!(entry.size(), blob.len() as u64);
            assert_eq!(
                entry.flags().as_u16(),
                if name.is_ascii() { 0 } else { 0x800 }
            );
            let local = starts[i] as usize;
            assert_eq!(&bytes[local..local + 4], b"PK\x03\x04");
            assert_eq!(&bytes[local + 6..local + 8], &0_u16.to_le_bytes());
            assert_eq!(
                &bytes[local + 14..local + 18],
                &crc32fast::hash(blob).to_le_bytes()
            );
            assert_eq!(&bytes[local + 26..local + 30], &[0; 4]);
        }
    }
    // Lean drops b.json's local record (30 B header, 6 B name, 1300 B stored data) and the three names left
    // (a.json 6 B, empty 5 B, été.txt 9 B).
    assert_eq!(sizes[0] - sizes[1], 30 + 6 + 1300 + 6 + 5 + 9);
}
