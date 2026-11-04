//! Three invalidation cases, each a lookup that found nothing in the memoised resolution and finds a file now,
//! every file that resolution read unchanged: a memo keyed on the files read reuses the old digest and finds the
//! packs the same; the query memo finds what the memo-less oracle finds. Then a warm comparison, through the saved
//! bytes, equals the cold one with every digest reused.

use std::collections::BTreeMap;
use std::time::Instant;

use packprune_graph::ReferenceIndex;
use packprune_pack::{
    Blob, Entry, Layout, Limits, LogicalPath, PackInput, PackMeta, PhysPath, RunCx, VanillaIndex,
};

use super::*;
use crate::{Comparison, OracleInput, Remap, VanillaAssets, compare_memoised, compare_with};

type Files = BTreeMap<String, Vec<u8>>;

fn client() -> Files {
    let mut files = Files::new();
    for (atlas, source, prefix) in [("blocks", "block", "block/"), ("items", "item", "item/")] {
        files.insert(
            format!("assets/minecraft/atlases/{atlas}.json"),
            format!(r#"{{"sources": [{{"type": "minecraft:directory", "source": "{source}", "prefix": "{prefix}"}}]}}"#)
                .into_bytes(),
        );
    }
    files
}

struct Client(Files);

impl VanillaAssets for Client {
    fn bytes(&self, logical: &str) -> Option<Blob> {
        self.0.get(logical).map(|bytes| bytes.as_slice().into())
    }
}

fn vanilla(client: &Files) -> VanillaIndex {
    VanillaIndex {
        version: "26.2".into(),
        pack_format: "88.0".into(),
        names: client.keys().cloned().collect(),
        atlases: client
            .iter()
            .filter(|(path, _)| path.starts_with("assets/minecraft/atlases/"))
            .map(|(path, bytes)| (path.clone(), bytes.clone()))
            .collect(),
        ..VanillaIndex::default()
    }
}

fn pack(files: &Files) -> PackInput {
    PackInput {
        name: "test".into(),
        files: files
            .iter()
            .map(|(path, bytes)| {
                let entry = Entry {
                    digest: packprune_pack::Digest(*blake3::hash(bytes).as_bytes()),
                    blob: bytes.as_slice().into(),
                };
                (PhysPath::new(path).expect("a valid path"), entry)
            })
            .collect(),
        digest: packprune_pack::Digest([0; 32]),
    }
}

/// A `width` by `height` PNG whose every texel differs.
fn png(width: u32, height: u32) -> Vec<u8> {
    let rgba = (0..width * height)
        .flat_map(|i| [i as u8, 0x40, 0x80, 0xFF])
        .collect();
    packprune_codec::png::encode(&packprune_codec::png::Image {
        width,
        height,
        rgba,
    })
    .expect("encodes")
}

fn text(files: &mut Files, path: &str, text: &str) {
    files.insert(path.into(), text.as_bytes().to_vec());
}

/// The comparison of `before` and `after`, with `memo` when given.
fn compare(before: &Files, after: &Files, memo: Option<&Memo>) -> Comparison {
    let cx = RunCx::default();
    let client = client();
    let vanilla = vanilla(&client);
    let (before, after) = (pack(before), pack(after));
    let layout = |pack: &PackInput| {
        Layout::new(pack, PackMeta::read(pack).expect("a pack.mcmeta"), &vanilla)
    };
    let (before_layout, after_layout) = (layout(&before), layout(&after));
    let index = ReferenceIndex::default();
    let input = |pack, layout| OracleInput {
        pack,
        layout,
        index: &index,
    };
    let named = |_: &LogicalPath| false;
    let (before, after) = (input(&before, &before_layout), input(&after, &after_layout));
    let assets = Client(client);
    match memo {
        Some(memo) => compare_memoised(
            &cx,
            &before,
            &after,
            &vanilla,
            &assets,
            &Remap::default(),
            &named,
            memo,
        ),
        None => compare_with(
            &cx,
            &before,
            &after,
            &vanilla,
            &assets,
            &Remap::default(),
            &named,
        ),
    }
}

/// The differences `memo` finds between `before` and `after` once it has memoised `before` against itself.
fn warm(memo: &Memo, before: &Files, after: &Files) -> Comparison {
    let cold = compare(before, before, Some(memo));
    assert_eq!(cold.differing_keys, 0);
    compare(before, after, Some(memo))
}

/// What the query memo finds is what the oracle finds without a memo, and a difference; a files-read memo finds
/// nothing.
fn invalidated(before: &Files, after: &Files) {
    let expected = compare(before, after, None);
    assert!(expected.differing_keys > 0, "the case changes a key");

    let memo = Memo::new();
    assert_eq!(warm(&memo, before, after), expected);
    assert!(memo.stale.lock().expect("unpoisoned").is_empty());

    let files_read = Memo::keyed_on_files_read();
    let comparison = warm(&files_read, before, after);
    assert_eq!(
        comparison.differing_keys, 0,
        "a files-read key reuses the old digest"
    );
    assert!(!files_read.stale.lock().expect("unpoisoned").is_empty());
}

/// An item whose model draws texture `a` as its particle.
fn item(files: &mut Files, model: &str) {
    text(
        files,
        "assets/fx/items/box.json",
        r#"{"model": {"type": "minecraft:model", "model": "fx:box"}}"#,
    );
    text(files, "assets/fx/models/box.json", model);
    files.insert("assets/fx/textures/item/a.png".into(), png(4, 8));
}

#[test]
fn an_overlay_override_that_appears_invalidates() {
    let mut before = Files::new();
    text(
        &mut before,
        "pack.mcmeta",
        r#"{"pack": {"min_format": 88, "max_format": 88, "description": "t"}, "overlays": {"entries": [{"directory": "over", "formats": [88, 88]}]}}"#,
    );
    item(&mut before, r#"{"textures": {"particle": "fx:item/a"}}"#);
    let mut after = before.clone();
    text(
        &mut after,
        "over/assets/fx/models/box.json",
        r#"{"textures": {"particle": "fx:item/a"}, "gui_light": "front"}"#,
    );

    invalidated(&before, &after);
}

#[test]
fn a_parent_that_appears_invalidates() {
    let mut before = Files::new();
    text(
        &mut before,
        "pack.mcmeta",
        r#"{"pack": {"min_format": 88, "max_format": 88, "description": "t"}}"#,
    );
    item(
        &mut before,
        r#"{"parent": "fx:base", "textures": {"particle": "fx:item/a"}}"#,
    );
    let mut after = before.clone();
    text(
        &mut after,
        "assets/fx/models/base.json",
        r#"{"gui_light": "front"}"#,
    );

    invalidated(&before, &after);
}

#[test]
fn an_mcmeta_that_appears_invalidates() {
    let mut before = Files::new();
    text(
        &mut before,
        "pack.mcmeta",
        r#"{"pack": {"min_format": 88, "max_format": 88, "description": "t"}}"#,
    );
    item(&mut before, r#"{"textures": {"particle": "fx:item/a"}}"#);
    let mut after = before.clone();
    text(
        &mut after,
        "assets/fx/textures/item/a.png.mcmeta",
        r#"{"animation": {}}"#,
    );

    invalidated(&before, &after);
}

/// A pack with an overlay, a font, a sound event and a language file, against a copy with one texture edited.
fn corpus() -> (Files, Files) {
    let mut before = Files::new();
    text(
        &mut before,
        "pack.mcmeta",
        r#"{"pack": {"min_format": 46, "max_format": 88, "description": "t"}, "overlays": {"entries": [{"directory": "over", "formats": [55, 88]}]}}"#,
    );
    item(
        &mut before,
        r##"{"textures": {"a": "fx:item/a"}, "elements": [{"from": [0, 0, 0], "to": [16, 16, 16], "faces": {"north": {"uv": [0, 0, 8, 8], "texture": "#a"}}}]}"##,
    );
    text(
        &mut before,
        "over/assets/fx/models/box.json",
        r#"{"textures": {"particle": "fx:item/a"}}"#,
    );
    text(
        &mut before,
        "assets/fx/font/hud.json",
        r#"{"providers": [{"type": "space", "advances": {" ": 4.0}}, {"type": "bitmap", "file": "fx:font/glyphs.png", "ascent": 7, "chars": ["AB"]}]}"#,
    );
    before.insert("assets/fx/textures/font/glyphs.png".into(), png(16, 8));
    text(
        &mut before,
        "assets/fx/sounds.json",
        r#"{"boom": {"sounds": ["fx:boom"]}}"#,
    );
    before.insert("assets/fx/sounds/boom.ogg".into(), b"not vorbis".to_vec());
    text(&mut before, "assets/fx/lang/en_us.json", r#"{"a": "b"}"#);
    let mut after = before.clone();
    after.insert("assets/fx/textures/font/glyphs.png".into(), png(16, 16));
    (before, after)
}

#[test]
fn a_warm_comparison_through_the_saved_memo_equals_the_cold_one() {
    let (before, after) = corpus();
    let cold = compare(&before, &after, None);
    assert!(cold.differing_keys > 0);

    let memo = Memo::new();
    assert_eq!(compare(&before, &after, Some(&memo)), cold);
    let (hits, misses) = memo.counts();
    assert!(misses > 0);
    let bytes = memo.to_bytes();

    let warm = Memo::from_bytes(&bytes).expect("reads its own bytes");
    assert_eq!(compare(&before, &after, Some(&warm)), cold);
    assert_eq!(warm.counts(), (hits + misses, 0), "every digest reused");
    assert!(warm.stale.lock().expect("unpoisoned").is_empty());
    assert_eq!(warm.to_bytes(), bytes);

    // One file edited since: only the keys that read it resolve again.
    let mut edited = after;
    edited.insert("assets/fx/textures/item/a.png".into(), png(4, 4));
    let expected = compare(&before, &edited, None);
    let again = Memo::from_bytes(&bytes).expect("reads its own bytes");
    assert_eq!(compare(&before, &edited, Some(&again)), expected);
    let (hits, misses) = again.counts();
    assert!(hits > 0 && misses > 0, "{hits} hits, {misses} misses");
    assert!(again.stale.lock().expect("unpoisoned").is_empty());
}

#[test]
fn bytes_of_another_format_or_cut_short_are_no_memo() {
    let (before, after) = corpus();
    let memo = Memo::new();
    compare(&before, &after, Some(&memo));
    let bytes = memo.to_bytes();
    assert!(Memo::from_bytes(&bytes[..bytes.len() - 1]).is_none());
    assert!(Memo::from_bytes(b"PPOMEMO0").is_none());
    let mut longer = bytes;
    longer.push(0);
    assert!(Memo::from_bytes(&longer).is_none());
}

#[test]
fn every_file_kind_reads_back() {
    for kind in KINDS {
        let mut bytes = Vec::new();
        let answer = Answer::File {
            physical: "a".into(),
            content: None,
            kind: Some(kind),
        };
        put_answer(&mut bytes, &answer);
        let mut reader = Reader {
            bytes: &bytes,
            at: 0,
        };
        assert_eq!(reader.answer(), Some(answer));
    }
    assert_eq!(KINDS.len(), FileKind::Unknown as usize + 1);
}

/// Real packs named by `PACKPRUNE_ORACLE_BEFORE`, `_AFTER` and `_JAR` (optionally `_ASSET_INDEX`), every name named:
/// the comparison without a memo, cold with one, then warm through its saved bytes, each memo hit resolved again;
/// prints the times and the memo's counts.
#[test]
#[ignore = "reads real packs named by PACKPRUNE_ORACLE_*"]
fn memo_verify_env() {
    let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"));
    let limits = Limits::default();
    let jar = var("PACKPRUNE_ORACLE_JAR");
    let mut vanilla = VanillaIndex::from_jar(jar.as_str().as_ref(), &limits).expect("a client jar");
    if let Ok(index) = std::env::var("PACKPRUNE_ORACLE_ASSET_INDEX") {
        vanilla = vanilla
            .with_asset_index(index.as_str().as_ref())
            .expect("an asset index");
    }
    let assets = PackInput::open(jar.as_str().as_ref(), &limits).expect("the jar's files");
    let open = |name: &str| PackInput::open(var(name).as_str().as_ref(), &limits).expect("a pack");
    let (before, after) = (
        open("PACKPRUNE_ORACLE_BEFORE"),
        open("PACKPRUNE_ORACLE_AFTER"),
    );
    let layout = |pack: &PackInput| {
        Layout::new(pack, PackMeta::read(pack).expect("a pack.mcmeta"), &vanilla)
    };
    let (before_layout, after_layout) = (layout(&before), layout(&after));
    let index = ReferenceIndex::default();
    let cx = RunCx::default();
    let input = |pack, layout| OracleInput {
        pack,
        layout,
        index: &index,
    };
    let (before, after) = (input(&before, &before_layout), input(&after, &after_layout));
    let named = |_: &LogicalPath| true;
    let run = |memo: Option<&Memo>| {
        let start = Instant::now();
        let comparison = match memo {
            Some(memo) => compare_memoised(
                &cx,
                &before,
                &after,
                &vanilla,
                &assets,
                &Remap::default(),
                &named,
                memo,
            ),
            None => compare_with(
                &cx,
                &before,
                &after,
                &vanilla,
                &assets,
                &Remap::default(),
                &named,
            ),
        };
        (comparison, start.elapsed())
    };
    let (expected, plain) = run(None);
    let memo = Memo::new();
    let (cold, cold_time) = run(Some(&memo));
    let bytes = memo.to_bytes();
    let warm_memo = Memo::from_bytes(&bytes).expect("reads its own bytes");
    let (warm, warm_time) = run(Some(&warm_memo));
    eprintln!(
        "views {} keys {} differing {}; no memo {plain:?}, cold {cold_time:?} {:?}, warm {warm_time:?} {:?} (hits each resolved again), memo {} B",
        expected.views,
        expected.keys,
        expected.differing_keys,
        memo.counts(),
        warm_memo.counts(),
        bytes.len()
    );
    assert_eq!(cold, expected);
    assert_eq!(warm, expected);
    assert_eq!(warm_memo.counts().1, 0, "every digest reused");
    assert!(warm_memo.stale.lock().expect("unpoisoned").is_empty());
}
