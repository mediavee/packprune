use std::sync::Arc;

use camino::Utf8PathBuf;

use super::*;

fn blobs(files: &[(&str, &[u8])]) -> BTreeMap<String, Blob> {
    files
        .iter()
        .map(|(name, bytes)| ((*name).to_owned(), Arc::from(*bytes)))
        .collect()
}

fn out_in(dir: &tempfile::TempDir, name: &str) -> Utf8PathBuf {
    Utf8PathBuf::from_path_buf(dir.path().join(name)).expect("utf-8 temp dir")
}

#[test]
fn entry_class_follows_the_extension_of_the_file_name() {
    for (name, class) in [
        ("pack.mcmeta", EntryClass::Text),
        ("assets/a/models/b.json", EntryClass::Text),
        ("assets/a/shaders/core/x.fsh", EntryClass::Text),
        ("assets/a/lang/en_us.lang", EntryClass::Text),
        ("assets/a/textures/b.png", EntryClass::Compressed),
        ("assets/a/textures/B.PNG", EntryClass::Compressed),
        ("assets/a/sounds/b.ogg", EntryClass::Compressed),
        ("LICENSE", EntryClass::Other),
        // Only the last segment has an extension: a dot in a folder name says nothing.
        ("assets/a.json/texture", EntryClass::Other),
        ("assets/a/textures/b.png.mcmeta", EntryClass::Text),
    ] {
        assert_eq!(entry_class(name), class, "{name}");
    }
}

#[test]
fn compressed_class_is_stored_when_recompression_is_off() {
    let dir = tempfile::tempdir().unwrap();
    let body = vec![b'a'; 4096];
    let options = WriteOptions {
        recompress_compressed: false,
        ..WriteOptions::default()
    };
    let size = |name: &str| {
        write_named(&blobs(&[(name, &body)]), &out_in(&dir, "out.zip"), &options)
            .unwrap()
            .bytes
    };
    // Same bytes, same name length: only the class differs.
    assert!(size("a.png") > 4096, "a png entry stays stored");
    assert!(size("a.txt") < 4096, "a text entry is deflated");
}

#[test]
fn written_entries_read_back_equal() {
    let dir = tempfile::tempdir().unwrap();
    let out = out_in(&dir, "out.zip");
    let files = blobs(&[
        ("pack.mcmeta", br#"{"pack":{"pack_format":69}}"#),
        (
            "assets/a/textures/b.png",
            &[0x89, b'P', b'N', b'G', 0, 1, 2],
        ),
        ("assets/é/empty", b""),
    ]);
    write_named(&files, &out, &WriteOptions::default()).unwrap();
    let read = packprune_zip::read(&out, &Limits::default()).unwrap();
    let expected: BTreeMap<_, _> = files.iter().map(|(n, b)| (n.clone(), b.to_vec())).collect();
    assert_eq!(read, expected);
}

#[test]
fn one_and_eight_threads_write_the_same_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let files: BTreeMap<String, Blob> = (0..64)
        .map(|i| {
            let body = format!("{{\"n\":{i},\"pad\":\"{}\"}}", "x".repeat(i * 7));
            (
                format!("assets/a/models/m{i}.json"),
                Arc::from(body.as_bytes()),
            )
        })
        .collect();
    let sha1 = |threads: usize| {
        let out = out_in(&dir, &format!("t{threads}.zip"));
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap()
            .install(|| write_named(&files, &out, &WriteOptions::default()))
            .unwrap()
            .sha1
    };
    assert_eq!(sha1(1), sha1(8));
}

#[test]
fn an_unsafe_name_is_refused_with_the_entry_and_nothing_is_published() {
    let dir = tempfile::tempdir().unwrap();
    let out = out_in(&dir, "out.zip");
    let error = write_named(
        &blobs(&[("assets/../evil", b"x")]),
        &out,
        &WriteOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(&error, IoError::Refused { entry, .. } if entry == "assets/../evil"),
        "{error:?}"
    );
    assert!(!out.exists());
}

#[test]
fn library_errors_keep_their_class() {
    let refused = IoError::from(InputError::Refused {
        entry: "a".into(),
        why: "b".into(),
    });
    assert!(matches!(refused, IoError::Refused { .. }));
    let io = IoError::from(ZipError::Io(io::Error::other("disk")));
    assert!(matches!(io, IoError::Io(_)));
}

#[test]
fn repacked_fixture_has_the_input_entries_and_digests() {
    let dir = tempfile::tempdir().unwrap();
    let input = Utf8PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/packs/overlay-formats"
    ));
    let out = out_in(&dir, "out.zip");
    let repacked = repack(&input, &out, &RunOptions::default()).unwrap();
    let before = PackInput::open(&input, &Limits::default()).unwrap();
    let after = PackInput::open(&out, &Limits::default()).unwrap();
    assert!(repacked.entries > 1);
    assert_eq!(repacked.entries, before.files.len());
    let digests = |pack: &PackInput| -> Vec<_> {
        pack.files
            .iter()
            .map(|(path, entry)| (path.as_str().to_owned(), entry.digest))
            .collect()
    };
    assert_eq!(digests(&after), digests(&before));
}
