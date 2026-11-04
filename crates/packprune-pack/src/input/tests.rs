use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use camino::Utf8Path;
use packprune_zip::{EntryClass, WriteOptions};
use tempfile::{TempDir, tempdir};

use super::open::{Opener, Root};
use super::portable;
use super::*;

fn utf8(path: &Path) -> &Utf8Path {
    Utf8Path::from_path(path).unwrap()
}

fn crc32(bytes: &[u8]) -> u32 {
    !bytes.iter().fold(!0_u32, |mut crc, &byte| {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (crc & 1).wrapping_neg());
        }
        crc
    })
}

/// A zip of stored members with the names exactly as given, which `packprune_zip::write` would never emit.
fn crafted_zip(members: &[(&[u8], &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut offsets = Vec::new();
    for (name, bytes) in members {
        offsets.push(out.len() as u32);
        out.extend(0x0403_4b50_u32.to_le_bytes());
        out.extend([20, 0, 0, 0, 0, 0, 0, 0, 0x41, 0]);
        out.extend(crc32(bytes).to_le_bytes());
        out.extend((bytes.len() as u32).to_le_bytes());
        out.extend((bytes.len() as u32).to_le_bytes());
        out.extend((name.len() as u16).to_le_bytes());
        out.extend([0, 0]);
        out.extend(*name);
        out.extend(*bytes);
    }
    let start = out.len() as u32;
    for ((name, bytes), offset) in members.iter().zip(offsets) {
        out.extend(0x0201_4b50_u32.to_le_bytes());
        out.extend([20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0x41, 0]);
        out.extend(crc32(bytes).to_le_bytes());
        out.extend((bytes.len() as u32).to_le_bytes());
        out.extend((bytes.len() as u32).to_le_bytes());
        out.extend((name.len() as u16).to_le_bytes());
        out.extend([0; 12]);
        out.extend(offset.to_le_bytes());
        out.extend(*name);
    }
    let size = out.len() as u32 - start;
    out.extend(0x0605_4b50_u32.to_le_bytes());
    out.extend([0; 4]);
    out.extend((members.len() as u16).to_le_bytes());
    out.extend((members.len() as u16).to_le_bytes());
    out.extend(size.to_le_bytes());
    out.extend(start.to_le_bytes());
    out.extend([0, 0]);
    out
}

fn zip_of(dir: &TempDir, bytes: &[u8]) -> camino::Utf8PathBuf {
    let path = dir.path().join("pack.zip");
    fs::write(&path, bytes).unwrap();
    utf8(&path).to_owned()
}

/// A directory and the zip `packprune_zip::write` makes of the same files, in one temporary directory.
fn pack_on_disk(files: &[(&str, &[u8])]) -> (TempDir, camino::Utf8PathBuf, camino::Utf8PathBuf) {
    let dir = tempdir().unwrap();
    let root = dir.path().join("pack");
    let mut entries = BTreeMap::new();
    for (name, bytes) in files {
        let file = root.join(name);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, bytes).unwrap();
        entries.insert(
            (*name).to_owned(),
            (Arc::<[u8]>::from(*bytes), EntryClass::Other),
        );
    }
    fs::create_dir_all(&root).unwrap();
    let zip = dir.path().join("pack.zip");
    packprune_zip::write(&entries, utf8(&zip), &WriteOptions::default()).unwrap();
    let (root, zip) = (utf8(&root).to_owned(), utf8(&zip).to_owned());
    (dir, root, zip)
}

fn expect_refused(result: Result<PackInput, InputError>, entry: &str, why: &str) {
    match result {
        Err(InputError::Refused {
            entry: got,
            why: reason,
        }) => {
            assert_eq!(got, entry);
            assert!(reason.contains(why), "{reason:?} does not contain {why:?}");
        }
        other => panic!("expected {entry:?} refused for {why:?}, got {other:?}"),
    }
}

fn digest_of(bytes: &[u8]) -> Digest {
    Digest(*blake3::hash(bytes).as_bytes())
}

const FILES: &[(&str, &[u8])] = &[
    ("pack.mcmeta", br#"{"pack":{"pack_format":88}}"#),
    ("assets/fx/textures/été.png", b"\x89PNG not really"),
    ("assets/fx/models/a.json", b"{}"),
    ("overlay_1/assets/fx/models/a.json", b"{\"o\":1}"),
    ("empty.txt", b""),
];

#[test]
fn a_directory_and_its_zip_read_to_the_same_files_with_blake3_digests() {
    let (_dir, root, zip) = pack_on_disk(FILES);

    let from_directory = PackInput::open(&root, &Limits::default()).unwrap();
    let from_zip = PackInput::open(&zip, &Limits::default()).unwrap();

    assert_eq!(from_directory.name, "pack");
    assert_eq!(from_zip.name, "pack.zip");
    assert_eq!(from_directory.files.len(), FILES.len());
    for (name, bytes) in FILES {
        for pack in [&from_directory, &from_zip] {
            let entry = &pack.files[&PhysPath::new(name).unwrap()];
            assert_eq!(&*entry.blob, *bytes, "{name}");
            assert_eq!(entry.digest, digest_of(bytes), "{name}");
        }
    }
    assert_eq!(
        from_directory.files.keys().collect::<Vec<_>>(),
        from_zip.files.keys().collect::<Vec<_>>()
    );
    assert_eq!(from_directory.digest, from_zip.digest);
}

#[test]
fn the_pack_digest_follows_names_and_contents() {
    let (_dir, root, _zip) = pack_on_disk(FILES);
    let before = PackInput::open(&root, &Limits::default()).unwrap().digest;

    fs::write(root.join("assets/fx/models/a.json"), b"{ }").unwrap();
    let edited = PackInput::open(&root, &Limits::default()).unwrap().digest;
    fs::write(root.join("assets/fx/models/a.json"), b"{}").unwrap();
    fs::rename(root.join("empty.txt"), root.join("other.txt")).unwrap();
    let renamed = PackInput::open(&root, &Limits::default()).unwrap().digest;

    assert_ne!(before, edited);
    assert_ne!(before, renamed);
    assert_ne!(edited, renamed);
}

#[test]
fn a_zip_refuses_every_unsafe_path_and_duplicate_by_name() {
    for name in [
        "../escape",
        "a/../escape",
        "a/./b",
        "/absolute",
        "a\\b",
        "a\0b",
        "C:drive",
        "a//b",
    ] {
        let dir = tempdir().unwrap();
        let zip = zip_of(&dir, &crafted_zip(&[(name.as_bytes(), b"x")]));
        expect_refused(PackInput::open(&zip, &Limits::default()), name, "path");
    }
    let dir = tempdir().unwrap();
    let zip = zip_of(&dir, &crafted_zip(&[(b"a.txt", b"1"), (b"a.txt", b"2")]));
    expect_refused(
        PackInput::open(&zip, &Limits::default()),
        "a.txt",
        "duplicate",
    );
}

#[test]
fn a_zip_refuses_each_limit_by_entry_name() {
    let dir = tempdir().unwrap();
    let zip = zip_of(
        &dir,
        &crafted_zip(&[(b"a", b"12345"), (b"b", b"12345"), (b"c", b"12345")]),
    );
    let limits = |count, entry, total| Limits {
        max_entries: count,
        max_entry_bytes: entry,
        max_total_bytes: total,
        ..Limits::default()
    };

    assert_eq!(
        PackInput::open(&zip, &limits(3, 5, 15))
            .unwrap()
            .files
            .len(),
        3
    );
    expect_refused(PackInput::open(&zip, &limits(2, 5, 15)), "c", "entry count");
    expect_refused(PackInput::open(&zip, &limits(3, 4, 15)), "a", "entry size");
    expect_refused(PackInput::open(&zip, &limits(3, 5, 14)), "c", "total size");
}

#[test]
fn a_zip_refuses_a_ratio_above_the_limit() {
    let (_dir, _root, zip) = pack_on_disk(&[("zeros.bin", &vec![0; 4 << 20])]);
    let limits = Limits {
        max_ratio: 10,
        ratio_min_entry_bytes: 1 << 20,
        ..Limits::default()
    };

    expect_refused(PackInput::open(&zip, &limits), "zeros.bin", "ratio");
}

#[test]
fn a_file_that_is_not_a_zip_is_refused_not_panicked() {
    let dir = tempdir().unwrap();
    for bytes in [&b""[..], b"PK\x03\x04 truncated", b"hello"] {
        let zip = zip_of(&dir, bytes);
        assert!(matches!(
            PackInput::open(&zip, &Limits::default()),
            Err(InputError::Refused { .. })
        ));
    }
    let missing = dir.path().join("missing");
    assert!(matches!(
        PackInput::open(utf8(&missing), &Limits::default()),
        Err(InputError::Io(_))
    ));
}

#[cfg(unix)]
#[test]
fn a_directory_refuses_a_symlink_by_name_whatever_it_points_at() {
    for (link, target_is_dir) in [("link.png", false), ("linked", true)] {
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("secret"), b"secret").unwrap();
        let root = dir.path().join("pack");
        fs::create_dir_all(root.join("assets")).unwrap();
        fs::write(root.join("assets/a.json"), b"{}").unwrap();
        let target = if target_is_dir {
            outside.path().to_owned()
        } else {
            outside.path().join("secret")
        };
        std::os::unix::fs::symlink(target, root.join("assets").join(link)).unwrap();

        expect_refused(
            PackInput::open(utf8(&root), &Limits::default()),
            &format!("assets/{link}"),
            "symlink",
        );
    }
}

#[cfg(unix)]
#[test]
fn a_directory_refuses_a_dangling_symlink_and_a_special_file() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("pack");
    fs::create_dir_all(&root).unwrap();
    std::os::unix::fs::symlink(dir.path().join("nowhere"), root.join("dangling")).unwrap();
    expect_refused(
        PackInput::open(utf8(&root), &Limits::default()),
        "dangling",
        "symlink",
    );

    fs::remove_file(root.join("dangling")).unwrap();
    let socket = std::os::unix::net::UnixListener::bind(root.join("sock")).unwrap();
    expect_refused(
        PackInput::open(utf8(&root), &Limits::default()),
        "sock",
        "not a regular file",
    );
    drop(socket);
}

#[cfg(unix)]
#[test]
fn a_directory_refuses_names_the_path_rules_forbid() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    // APFS refuses to create a name that is not UTF-8.
    let not_utf8: &[(&[u8], &str)] = if cfg!(target_os = "linux") {
        &[(b"bad-\xff-utf8", "UTF-8")]
    } else {
        &[]
    };
    let cases: [(&[u8], &str); 3] = [
        (b"a\\b", "path"),
        (b"C:drive", "path"),
        (b"..\\escape", "path"),
    ];
    for (name, why) in cases.into_iter().chain(not_utf8.iter().copied()) {
        let dir = tempdir().unwrap();
        let root = dir.path().join("pack");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(OsStr::from_bytes(name)), b"x").unwrap();

        expect_refused(
            PackInput::open(utf8(&root), &Limits::default()),
            &String::from_utf8_lossy(name),
            why,
        );
    }
}

#[test]
fn a_directory_refuses_each_limit_by_entry_name() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("pack");
    fs::create_dir_all(root.join("sub")).unwrap();
    for name in ["a", "sub/b", "sub/c"] {
        fs::write(root.join(name), b"12345").unwrap();
    }
    let root = utf8(&root);
    let limits = |count, entry, total| Limits {
        max_entries: count,
        max_entry_bytes: entry,
        max_total_bytes: total,
        ..Limits::default()
    };

    assert_eq!(
        PackInput::open(root, &limits(3, 5, 15))
            .unwrap()
            .files
            .len(),
        3
    );
    expect_refused(
        PackInput::open(root, &limits(2, 5, 15)),
        "sub/c",
        "entry count",
    );
    expect_refused(PackInput::open(root, &limits(3, 4, 15)), "a", "entry size");
    expect_refused(
        PackInput::open(root, &limits(3, 5, 14)),
        "sub/c",
        "total size",
    );
}

#[cfg(unix)]
#[test]
fn the_first_refusal_does_not_depend_on_the_walk_order() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("pack");
    fs::create_dir_all(&root).unwrap();
    for name in ["z", "m", "b", "q"] {
        std::os::unix::fs::symlink("/", root.join(name)).unwrap();
    }

    expect_refused(
        PackInput::open(utf8(&root), &Limits::default()),
        "b",
        "symlink",
    );
}

#[test]
fn an_empty_directory_is_an_empty_pack() {
    let dir = tempdir().unwrap();
    let pack = PackInput::open(utf8(dir.path()), &Limits::default()).unwrap();

    assert!(pack.files.is_empty());
}

#[test]
fn a_file_that_changed_size_since_the_walk_is_refused_not_truncated() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("grown");
    fs::write(&file, b"12345").unwrap();
    let root = Root::open(dir.path()).unwrap();
    let path = PhysPath::new("grown").unwrap();

    assert_eq!(directory::load(&root, &path, 5).unwrap(), b"12345");
    for walked in [4, 6] {
        let error = directory::load(&root, &path, walked).unwrap_err();
        assert!(
            matches!(&error, InputError::Refused { entry, why }
                if entry == "grown" && why.contains("size changed")),
            "{error:?}"
        );
    }
}

#[cfg(unix)]
type Read = Result<BTreeMap<PhysPath, Entry>, InputError>;

/// What each opener does when the tree changes between the walk and the read: `swap` runs between the two.
/// The openat one is the Unix default; the portable one is what other targets run.
#[cfg(unix)]
fn swapped_after_the_walk(
    files: &[(&str, &[u8])],
    swap: impl Fn(&Path, &Path),
) -> Vec<(&'static str, Read)> {
    fn read<R: Opener + Send + 'static>(opened: R, found: BTreeMap<PhysPath, u64>) -> Read {
        // A FIFO must be refused, not waited on: read on a thread the test can give up on.
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || sender.send(directory::read_files(&opened, found)));
        receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the read blocked")
    }

    ["openat", "portable"]
        .into_iter()
        .map(|label| {
            let dir = tempdir().unwrap();
            let root = dir.path().join("pack");
            let outside = dir.path().join("outside");
            fs::create_dir_all(&outside).unwrap();
            for (name, bytes) in files {
                fs::create_dir_all(root.join(name).parent().unwrap()).unwrap();
                fs::write(root.join(name), bytes).unwrap();
            }
            let found = directory::walk(&root, &Limits::default()).unwrap();
            swap(&root, &outside);
            let result = if label == "openat" {
                read(Root::open(&root).unwrap(), found)
            } else {
                read(portable::PortableRoot::open(&root).unwrap(), found)
            };
            (label, result)
        })
        .collect()
}

#[cfg(unix)]
fn refused_entry(results: Vec<(&str, Read)>, entry: &str, why: &str) {
    for (label, result) in results {
        match result {
            Err(InputError::Refused {
                entry: got,
                why: reason,
            }) => {
                assert_eq!(got, entry, "{label}");
                assert!(
                    reason.contains(why),
                    "{label}: {reason:?} does not contain {why:?}"
                );
            }
            other => panic!("{label}: expected {entry:?} refused, got {other:?}"),
        }
    }
}

#[cfg(unix)]
#[test]
fn a_file_replaced_by_a_same_size_symlink_after_the_walk_is_refused_unread() {
    let result = swapped_after_the_walk(&[("assets/a.json", b"inside")], |root, outside| {
        fs::write(outside.join("same-size"), b"hidden").unwrap();
        fs::remove_file(root.join("assets/a.json")).unwrap();
        std::os::unix::fs::symlink(outside.join("same-size"), root.join("assets/a.json")).unwrap();
    });

    refused_entry(result, "assets/a.json", "symlink");
}

#[cfg(unix)]
#[test]
fn a_parent_directory_replaced_by_a_symlink_after_the_walk_is_refused_unread() {
    let result = swapped_after_the_walk(&[("assets/a.json", b"inside")], |root, outside| {
        fs::write(outside.join("a.json"), b"hidden").unwrap();
        fs::remove_dir_all(root.join("assets")).unwrap();
        std::os::unix::fs::symlink(outside, root.join("assets")).unwrap();
    });

    refused_entry(result, "assets/a.json", "symlink");
}

#[cfg(unix)]
#[test]
fn a_file_replaced_by_a_fifo_after_the_walk_is_refused_without_blocking() {
    // Empty, as a FIFO's length is: only its type can refuse it.
    let result = swapped_after_the_walk(&[("a.json", b"")], |root, _| {
        fs::remove_file(root.join("a.json")).unwrap();
        let fifo = std::ffi::CString::new(root.join("a.json").to_str().unwrap()).unwrap();
        // SAFETY: `fifo` is a NUL-terminated path that outlives the call.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    });

    refused_entry(result, "a.json", "not a regular file");
}

#[cfg(unix)]
#[test]
fn a_directory_applies_the_path_rules_to_empty_directories_too() {
    // A drive prefix only counts at the start of the path.
    for entry in ["assets/a\\b", "assets/..\\escape", "C:drive"] {
        let dir = tempdir().unwrap();
        let root = dir.path().join("pack");
        fs::create_dir_all(root.join(entry)).unwrap();
        fs::write(root.join("pack.mcmeta"), b"{}").unwrap();

        expect_refused(
            PackInput::open(utf8(&root), &Limits::default()),
            entry,
            "path",
        );
    }
}

#[test]
fn a_reader_that_grows_or_shrinks_past_the_walked_size_is_refused() {
    use std::io::Read;

    let path = PhysPath::new("grown").unwrap();

    assert_eq!(
        directory::read_exactly(&b"12345"[..], &path, 5).unwrap(),
        b"12345"
    );
    // The endless reader is stopped at size + 1 instead of being read to the end.
    let endless = std::io::repeat(7).take(u64::MAX);
    for reader in [
        Box::new(&b"123456"[..]) as Box<dyn Read>,
        Box::new(&b"1234"[..]),
        Box::new(endless),
    ] {
        match directory::read_exactly(reader, &path, 5) {
            Err(InputError::Refused { entry, why }) => {
                assert_eq!(entry, "grown");
                assert!(why.contains("size changed"), "{why}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
}

#[cfg(unix)]
fn portable_read_with_swaps(
    files: &[(&str, &[u8])],
    before_open: impl Fn(&Path, &Path) + Send + Sync + 'static,
    after_open: impl Fn(&Path, &Path) + Send + Sync + 'static,
) -> Read {
    let dir = tempdir().unwrap();
    let root = dir.path().join("pack");
    let outside = dir.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    for (name, bytes) in files {
        fs::create_dir_all(root.join(name).parent().unwrap()).unwrap();
        fs::write(root.join(name), bytes).unwrap();
    }
    let found = directory::walk(&root, &Limits::default()).unwrap();
    let mut opened = portable::PortableRoot::open(&root).unwrap();
    let (swap_root, swap_outside) = (root.clone(), outside.clone());
    opened.before_open = Some(Box::new(move || before_open(&swap_root, &swap_outside)));
    opened.after_open = Some(Box::new(move || after_open(&root, &outside)));
    directory::read_files(&opened, found)
}

#[cfg(unix)]
#[test]
fn the_portable_open_sees_a_parent_swapped_for_a_link_during_the_open() {
    let result = portable_read_with_swaps(
        &[("assets/a.json", b"inside")],
        |_, _| {},
        |root, outside| {
            fs::write(outside.join("a.json"), b"hidden").unwrap();
            fs::remove_dir_all(root.join("assets")).unwrap();
            std::os::unix::fs::symlink(outside, root.join("assets")).unwrap();
        },
    );

    refused_entry(vec![("portable", result)], "assets/a.json", "symlink");
}

#[cfg(unix)]
#[test]
fn the_portable_open_sees_the_file_swapped_for_another_one_during_the_open() {
    let result = portable_read_with_swaps(
        &[("a.json", b"inside")],
        |_, _| {},
        |root, _| {
            let other = root.join("other");
            fs::write(&other, b"hidden").unwrap();
            let later = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
            fs::File::options()
                .write(true)
                .open(&other)
                .unwrap()
                .set_modified(later)
                .unwrap();
            fs::rename(other, root.join("a.json")).unwrap();
        },
    );

    refused_entry(
        vec![("portable", result)],
        "a.json",
        "replaced while opening",
    );
}

#[cfg(unix)]
#[test]
fn the_portable_open_sees_a_file_swapped_before_the_open_and_put_back_after_it() {
    let result = portable_read_with_swaps(
        &[("a.json", b"inside")],
        |root, _| {
            let other = root.join("other");
            fs::write(&other, b"hidden").unwrap();
            let later = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
            fs::File::options()
                .write(true)
                .open(&other)
                .unwrap()
                .set_modified(later)
                .unwrap();
            fs::rename(root.join("a.json"), root.join("original")).unwrap();
            fs::rename(other, root.join("a.json")).unwrap();
        },
        |root, _| {
            fs::remove_file(root.join("a.json")).unwrap();
            fs::rename(root.join("original"), root.join("a.json")).unwrap();
        },
    );

    refused_entry(
        vec![("portable", result)],
        "a.json",
        "replaced while opening",
    );
}
