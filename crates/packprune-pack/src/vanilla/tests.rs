use std::collections::BTreeMap;
use std::sync::Arc;

use camino::{Utf8Path, Utf8PathBuf};
use packprune_zip::{EntryClass, WriteOptions};
use tempfile::{TempDir, tempdir};

use super::*;

const ATLAS: &str = r#"{"sources":[{"type":"directory","source":"block","prefix":"block/"}]}"#;

/// A client jar and a launcher layout `assets/indexes/32.json` + `assets/objects/` holding `sounds.json`.
fn client(version: Option<&str>, objects: bool) -> (TempDir, Utf8PathBuf, Utf8PathBuf) {
    let dir = tempdir().unwrap();
    let root = Utf8Path::from_path(dir.path()).unwrap().to_owned();
    let mut entries: BTreeMap<String, (Arc<[u8]>, EntryClass)> = [
        ("net/minecraft/Main.class", "x"),
        ("assets/minecraft/atlases/blocks.json", ATLAS),
        ("assets/minecraft/atlases/notes.txt", ""),
        ("assets/minecraft/textures/block/stone.png", ""),
        ("data/minecraft/recipe/stone.json", "{}"),
    ]
    .into_iter()
    .map(|(name, text)| {
        (
            name.to_owned(),
            (Arc::from(text.as_bytes()), EntryClass::Other),
        )
    })
    .collect();
    if let Some(version) = version {
        entries.insert(
            "version.json".into(),
            (Arc::from(version.as_bytes()), EntryClass::Other),
        );
    }
    let jar = root.join("client.jar");
    packprune_zip::write(&entries, &jar, &WriteOptions::default()).unwrap();
    let hash = format!("ab{}", "0".repeat(38));
    let indexes = root.join("assets/indexes");
    std::fs::create_dir_all(&indexes).unwrap();
    let index = indexes.join("32.json");
    std::fs::write(
        &index,
        format!(
            r#"{{"objects":{{"minecraft/sounds.json":{{"hash":"{hash}","size":1}},
            "minecraft/sounds/ambient/cave1.ogg":{{"hash":"cd","size":1}}}}}}"#
        ),
    )
    .unwrap();
    if objects {
        let folder = root.join("assets/objects/ab");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join(&hash),
            r#"{"ambient.cave":{},"block.stone.break":{}}"#,
        )
        .unwrap();
    }
    (dir, jar, index)
}

const VERSION: &str =
    r#"{"id":"26.2","pack_version":{"resource_major":88,"resource_minor":0,"data_major":107}}"#;

#[test]
fn index_holds_the_jar_assets_and_the_asset_index_sounds() {
    let (_dir, jar, index) = client(Some(VERSION), true);
    let vanilla = VanillaIndex::from_jar(&jar, &Limits::default())
        .unwrap()
        .with_asset_index(&index)
        .unwrap();
    assert_eq!(vanilla.version, "26.2");
    assert_eq!(vanilla.pack_format, "88.0");
    assert_eq!(
        vanilla.names.iter().map(String::as_str).collect::<Vec<_>>(),
        [
            "assets/minecraft/atlases/blocks.json",
            "assets/minecraft/atlases/notes.txt",
            "assets/minecraft/sounds.json",
            "assets/minecraft/sounds/ambient/cave1.ogg",
            "assets/minecraft/textures/block/stone.png",
        ]
    );
    assert_eq!(
        vanilla.atlases,
        BTreeMap::from([(
            "assets/minecraft/atlases/blocks.json".to_owned(),
            ATLAS.as_bytes().to_vec()
        )])
    );
    assert_eq!(
        vanilla.atlas_ids().into_iter().collect::<Vec<_>>(),
        ["minecraft:blocks"]
    );
    assert!(vanilla.sound_files);
    assert_eq!(
        vanilla
            .sound_events
            .as_ref()
            .unwrap()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["ambient.cave", "block.stone.break"]
    );
    assert_eq!(vanilla.jar.as_ref().unwrap().name, "client.jar");
    assert_eq!(
        vanilla.jar.as_ref().unwrap().blake3,
        blake3::hash(&std::fs::read(&jar).unwrap())
            .to_hex()
            .to_string()
    );
    assert_eq!(vanilla.asset_index.as_ref().unwrap().name, "32.json");
    assert!(vanilla.has("assets/minecraft/sounds/ambient/cave1.ogg"));
    assert!(!vanilla.has("data/minecraft/recipe/stone.json"));
}

#[test]
fn cached_bytes_load_the_same_index() {
    let (_dir, jar, index) = client(Some(VERSION), true);
    let built = VanillaIndex::from_jar(&jar, &Limits::default())
        .unwrap()
        .with_asset_index(&index)
        .unwrap();
    let loaded = VanillaIndex::from_bytes(&built.to_bytes()).unwrap();
    assert_eq!(loaded, built);
    assert_eq!(loaded.to_bytes(), built.to_bytes());
}

/// `with_asset_index` starts from an empty event set: a second index whose `sounds.json` object is not on disk
/// leaves no events, even over a cached index that had some.
#[test]
fn replacing_the_asset_index_drops_the_previous_sound_events() {
    let (_dir, jar, with_objects) = client(Some(VERSION), true);
    let (_other, _, without_objects) = client(Some(VERSION), false);
    let built = VanillaIndex::from_jar(&jar, &Limits::default())
        .unwrap()
        .with_asset_index(&with_objects)
        .unwrap();
    assert!(built.sound_events.is_some());
    let loaded = VanillaIndex::from_bytes(&built.to_bytes()).unwrap();
    let replaced = loaded.with_asset_index(&without_objects).unwrap();
    assert_eq!(replaced.sound_events, None);
    assert!(replaced.sound_files);
}

#[test]
fn missing_version_and_objects_read_as_unknown() {
    let (_dir, jar, index) = client(None, false);
    let vanilla = VanillaIndex::from_jar(&jar, &Limits::default()).unwrap();
    assert_eq!(
        (vanilla.version.as_str(), vanilla.pack_format.as_str()),
        ("unknown", "unknown")
    );
    assert!(!vanilla.sound_files && vanilla.asset_index.is_none());
    let vanilla = vanilla.with_asset_index(&index).unwrap();
    assert!(vanilla.sound_files);
    assert_eq!(vanilla.sound_events, None);
    let (_dir, jar, _) = client(
        Some(r#"{"id":"x","pack_version":{"resource_major":"9"}}"#),
        false,
    );
    let vanilla = VanillaIndex::from_jar(&jar, &Limits::default()).unwrap();
    assert_eq!(vanilla.pack_format, "9.0");
}

#[test]
fn a_file_without_objects_is_not_an_asset_index() {
    let (dir, jar, _) = client(Some(VERSION), false);
    let other = Utf8Path::from_path(dir.path()).unwrap().join("other.json");
    std::fs::write(&other, r#"{"files":{}}"#).unwrap();
    let error = VanillaIndex::from_jar(&jar, &Limits::default())
        .unwrap()
        .with_asset_index(&other)
        .unwrap_err();
    assert!(
        error
            .to_string()
            .ends_with("not an asset index (no objects)"),
        "{error}"
    );
    let not_zip = Utf8Path::from_path(dir.path()).unwrap().join("other.json");
    assert!(VanillaIndex::from_jar(&not_zip, &Limits::default()).is_err());
}

/// A client jar and its asset index built, then the cached form loaded back. `PACKPRUNE_VANILLA_CORPUS` is
/// `<jar>;<asset index>`.
#[test]
#[ignore = "requires PACKPRUNE_VANILLA_CORPUS (`<client jar>;<asset index>`)"]
fn corpus_index_survives_the_cache() {
    let spec = std::env::var("PACKPRUNE_VANILLA_CORPUS").expect("PACKPRUNE_VANILLA_CORPUS");
    let [jar, index] = spec.split(';').collect::<Vec<_>>()[..] else {
        panic!("<jar>;<asset index>");
    };
    let built = VanillaIndex::from_jar(jar.into(), &Limits::default())
        .unwrap()
        .with_asset_index(index.into())
        .unwrap();
    let loaded = VanillaIndex::from_bytes(&built.to_bytes()).unwrap();
    assert_eq!(loaded, built);
    eprintln!(
        "{} names, {} atlases, {:?} sound events",
        loaded.names.len(),
        loaded.atlases.len(),
        loaded.sound_events.as_ref().map(BTreeSet::len)
    );
}

/// A signature and an `IHDR` of the given size: all the index reads of a PNG.
fn header(width: u32, height: u32) -> Vec<u8> {
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    png.extend(width.to_be_bytes());
    png.extend(height.to_be_bytes());
    png
}

#[test]
fn index_records_each_png_size_and_the_texture_metadata() {
    let dir = tempdir().unwrap();
    let jar = Utf8Path::from_path(dir.path()).unwrap().join("client.jar");
    let entries: BTreeMap<String, (Arc<[u8]>, EntryClass)> = [
        ("assets/minecraft/textures/block/stone.png", header(16, 16)),
        ("assets/minecraft/textures/block/fire.png", header(16, 512)),
        (
            "assets/minecraft/textures/block/fire.png.mcmeta",
            br#"{"animation":{}}"#.to_vec(),
        ),
        (
            "assets/minecraft/textures/block/cut.png",
            header(8, 8)[..20].to_vec(),
        ),
        ("assets/minecraft/textures/block/empty.png", header(0, 8)),
        ("data/minecraft/textures/other.png", header(4, 4)),
    ]
    .into_iter()
    .map(|(name, bytes)| (name.to_owned(), (Arc::from(bytes), EntryClass::Other)))
    .collect();
    packprune_zip::write(&entries, &jar, &WriteOptions::default()).unwrap();
    let vanilla = VanillaIndex::from_jar(&jar, &Limits::default()).unwrap();
    assert_eq!(
        vanilla.textures,
        BTreeMap::from([
            (
                "assets/minecraft/textures/block/fire.png".to_owned(),
                TextureSize {
                    width: 16,
                    height: 512
                }
            ),
            (
                "assets/minecraft/textures/block/stone.png".to_owned(),
                TextureSize {
                    width: 16,
                    height: 16
                }
            ),
        ]),
        "a truncated header, a zero size and a file outside assets/ are not recorded"
    );
    assert_eq!(
        vanilla.texture_meta.keys().collect::<Vec<_>>(),
        ["assets/minecraft/textures/block/fire.png.mcmeta"]
    );
    let cached = VanillaIndex::from_bytes(&vanilla.to_bytes()).unwrap();
    assert_eq!(cached, vanilla, "the sizes survive the cached form");
}
