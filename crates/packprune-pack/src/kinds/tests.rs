use super::*;

const CLASSIFIED: &[(&str, &str)] = &[
    ("assets/x/sounds.json", "sounds_json"),
    ("assets/x/other.json", "unknown"),
    ("assets/x/items/a.json", "item_definition"),
    ("assets/x/items/a.png", "unknown"),
    ("assets/x/models/block/a.json", "model"),
    ("assets/x/textures/a.png", "texture"),
    ("assets/x/textures/a.png.mcmeta", "texture_meta"),
    ("assets/x/textures/a.mcmeta", "unknown"),
    ("assets/x/textures/a.jpg", "unknown"),
    ("assets/x/blockstates/a.json", "blockstate"),
    ("assets/x/atlases/blocks.json", "atlas"),
    ("assets/x/equipment/a.json", "equipment"),
    ("assets/x/font/default.json", "font"),
    ("assets/x/font/a.ttf", "font_file"),
    ("assets/x/font/sub/a.png", "font_file"),
    ("assets/x/sounds/a.ogg", "sound"),
    ("assets/x/sounds/a.wav", "unknown"),
    ("assets/x/lang/en_us.json", "lang"),
    ("assets/x/lang/en_us.lang", "unknown"),
    ("assets/x/shaders/core/a.vsh", "shader"),
    ("assets/x/shaders/core/a.fsh", "shader"),
    ("assets/x/shaders/include/a.glsl", "shader_include"),
    ("assets/x/shaders/core/a.json", "unknown"),
    ("assets/x/post_effect/a.json", "post_effect"),
    ("assets/x/particles/a.json", "particle"),
    ("assets/x/waypoint_style/a.json", "waypoint_style"),
    ("assets/x/texts/end.txt", "text"),
    ("assets/x/optifine/ctm/a.properties", "unknown"),
    ("assets//models/a.json", "model"),
    ("assets/x", "unknown"),
    ("assets/x/", "unknown"),
    ("pack.mcmeta", "unknown"),
    ("assets/x/models/a.JSON", "unknown"),
];

#[test]
fn logical_paths_classify_by_folder_and_extension() {
    for (path, id) in CLASSIFIED {
        assert_eq!(FileKind::of(path).id(), *id, "{path}");
    }
}

/// The class M1's engine io gives by file name (`entry_class`, branch `feat/m1-minimal-run`) is what the kind
/// gives: switching to the kind changes no zip entry.
#[test]
fn entry_class_agrees_with_the_extension_rule_on_every_kind() {
    for (path, _) in CLASSIFIED
        .iter()
        .chain(&[("pack.png", ""), ("pack.mcmeta", ""), ("x", "")])
    {
        let kind = if matches!(*path, "pack.png" | "pack.mcmeta") {
            FileKind::PackMeta
        } else {
            FileKind::of(path)
        };
        let Ok(phys) = PhysPath::new(path) else {
            continue;
        };
        assert_eq!(kind.entry_class(&phys), class_of_extension(path), "{path}");
    }
    let phys = |path| PhysPath::new(path).unwrap();
    assert_eq!(
        FileKind::Texture.entry_class(&phys("assets/x/textures/a.png")),
        EntryClass::Compressed
    );
    assert_eq!(
        FileKind::Model.entry_class(&phys("assets/x/models/a.json")),
        EntryClass::Text
    );
    assert_eq!(
        FileKind::PackMeta.entry_class(&phys("pack.png")),
        EntryClass::Compressed
    );
    assert_eq!(
        FileKind::FontFile.entry_class(&phys("assets/x/font/a.ttf")),
        EntryClass::Other
    );
}

#[test]
fn resource_path_splits_namespace_folder_and_path() {
    assert_eq!(
        ResourcePath::of("assets/ns/textures/block/a.png"),
        Some(ResourcePath {
            namespace: "ns",
            folder: "textures",
            rest: "block/a.png"
        })
    );
    assert_eq!(
        ResourcePath::of("assets/ns/sounds.json"),
        Some(ResourcePath {
            namespace: "ns",
            folder: "",
            rest: "sounds.json"
        })
    );
    assert_eq!(ResourcePath::of("assets/ns"), None);
    assert_eq!(ResourcePath::of("data/ns/a.json"), None);
}
