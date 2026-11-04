use std::fmt::Write;

use super::*;
use crate::meta::show_format;
use crate::{Digest, Entry};

fn pack(files: &[(&str, &str)]) -> PackInput {
    PackInput {
        name: "fixture".into(),
        files: files
            .iter()
            .map(|(path, text)| {
                (
                    PhysPath::new(path).unwrap(),
                    Entry {
                        blob: text.as_bytes().into(),
                        digest: Digest([0; 32]),
                    },
                )
            })
            .collect(),
        digest: Digest([0; 32]),
    }
}

fn vanilla(names: &[&str]) -> VanillaIndex {
    VanillaIndex {
        pack_format: "88.0".into(),
        names: names.iter().map(|name| name.to_string()).collect(),
        ..VanillaIndex::default()
    }
}

fn layout(files: &[(&str, &str)], vanilla: &VanillaIndex) -> Layout {
    let pack = pack(files);
    Layout::new(&pack, PackMeta::read(&pack).unwrap(), vanilla)
}

const META: &str = r#"{"pack":{"supported_formats":[80,100]},"overlays":{"entries":[
    {"formats":[84,99],"directory":"a"},{"formats":[88,90],"directory":"b"}]}}"#;

#[test]
fn files_are_classified_by_listed_layer_and_kind() {
    let layout = layout(
        &[
            ("pack.mcmeta", META),
            ("pack.png", ""),
            ("assets/x/models/m.json", ""),
            ("a/assets/x/textures/t.png", ""),
            ("a/assets/x/textures/t.png.mcmeta", ""),
            ("a/pack.mcmeta", ""),
            ("c/assets/x/models/m.json", ""),
            ("assets/minecraft/gpu_warnlist.json", ""),
            ("assets/x/optifine/ctm.properties", ""),
            ("readme.txt", ""),
        ],
        &vanilla(&["assets/minecraft/gpu_warnlist.json"]),
    );
    let kinds: Vec<(&str, FileKind)> = layout
        .kinds
        .iter()
        .map(|(path, kind)| (path.as_str(), *kind))
        .collect();
    assert_eq!(
        kinds,
        [
            ("a/assets/x/textures/t.png", FileKind::Texture),
            ("a/assets/x/textures/t.png.mcmeta", FileKind::TextureMeta),
            ("a/pack.mcmeta", FileKind::Unknown),
            ("assets/minecraft/gpu_warnlist.json", FileKind::Other),
            ("assets/x/models/m.json", FileKind::Model),
            ("assets/x/optifine/ctm.properties", FileKind::Unknown),
            // Not a listed overlay: a root file outside `assets/`.
            ("c/assets/x/models/m.json", FileKind::Unknown),
            ("pack.mcmeta", FileKind::PackMeta),
            ("pack.png", FileKind::PackMeta),
            ("readme.txt", FileKind::Unknown),
        ]
    );
    let phys = |path| PhysPath::new(path).unwrap();
    assert_eq!(
        layout.split(&phys("a/assets/x/textures/t.png")),
        (
            Layer::Overlay("a".into()),
            Some(LogicalPath::new("assets/x/textures/t.png").unwrap())
        )
    );
    assert_eq!(
        layout.split(&phys("c/assets/x/models/m.json")),
        (Layer::Root, None)
    );
}

#[test]
fn later_overlays_win_and_views_are_named_by_their_overlays() {
    let layout = layout(
        &[
            ("pack.mcmeta", META),
            ("assets/x/models/m.json", ""),
            ("assets/x/models/n.json", ""),
            ("a/assets/x/models/m.json", ""),
            ("a/assets/x/models/n.json", ""),
            ("b/assets/x/models/m.json", ""),
        ],
        &vanilla(&[]),
    );
    let names: Vec<(String, String, bool)> = layout
        .views
        .iter()
        .map(|view| {
            (
                view.name.clone(),
                show_format(view.formats.min).to_string(),
                view.verified,
            )
        })
        .collect();
    // The views of META at the 26.2 jar's format.
    let row = |name: &str, format: &str, verified| (name.to_owned(), format.to_owned(), verified);
    assert_eq!(
        names,
        [
            row("base", "80.0", false),
            row("a", "84.0", false),
            row("a+b", "88.0", true),
            row("a+b@90.*", "90.*", false),
        ]
    );
    let files = |index: usize| -> Vec<(String, String)> {
        layout.views[index]
            .files
            .iter()
            .map(|(logical, physical)| (logical.to_string(), physical.to_string()))
            .collect()
    };
    let pair = |logical: &str, physical: &str| (logical.to_owned(), physical.to_owned());
    assert_eq!(
        files(0),
        [
            pair("assets/x/models/m.json", "assets/x/models/m.json"),
            pair("assets/x/models/n.json", "assets/x/models/n.json"),
        ]
    );
    assert_eq!(
        files(2),
        [
            pair("assets/x/models/m.json", "b/assets/x/models/m.json"),
            pair("assets/x/models/n.json", "a/assets/x/models/n.json"),
        ]
    );
    assert_eq!(
        layout.views[2].layers,
        [
            Layer::Root,
            Layer::Overlay("a".into()),
            Layer::Overlay("b".into())
        ]
    );
    let at = layout.view_at((95, 0));
    assert_eq!(at.id, ViewId(4));
    // A view at a format is named by its overlays alone.
    assert_eq!(at.name, "a");
    assert_eq!(layout.view_at((70, 0)).name, "base");
    assert_eq!(at.files, layout.views[1].files);
}

/// An overlay directory named `assets`: the layer is the first segment when `pack.mcmeta` lists it, whatever its
/// name, so the root's `assets/x/...` files become that overlay's non-`assets/` files and no view shows them.
#[test]
fn an_overlay_named_assets_takes_the_root_files() {
    let layout = layout(
        &[
            (
                "pack.mcmeta",
                r#"{"pack":{"supported_formats":[80,100]},"overlays":{"entries":[
                    {"formats":[88,90],"directory":"assets"},{"formats":[84,99],"directory":"o"}]}}"#,
            ),
            ("pack.png", ""),
            ("assets/x/models/m.json", "{}"),
            ("assets/assets/x/models/m.json", "{}"),
            ("assets/assets/x/textures/t.png", ""),
            ("assets/pack.mcmeta", "{}"),
            ("o/assets/x/models/m.json", "{}"),
            ("o/assets/x/models/n.json", "{}"),
            ("assets/minecraft/gpu_warnlist.json", "{}"),
            ("assets/assets/minecraft/gpu_warnlist.json", "{}"),
        ],
        &vanilla(&["assets/minecraft/gpu_warnlist.json"]),
    );
    let expected = "\
view\tbase\t80.0\t\tfalse\t0
view\to\t84.0\to\tfalse\t2
file\to\tassets/x/models/m.json\to/assets/x/models/m.json
file\to\tassets/x/models/n.json\to/assets/x/models/n.json
view\tassets+o\t88.0\tassets,o\ttrue\t4
file\tassets+o\tassets/minecraft/gpu_warnlist.json\tassets/assets/minecraft/gpu_warnlist.json
file\tassets+o\tassets/x/models/m.json\to/assets/x/models/m.json
file\tassets+o\tassets/x/models/n.json\to/assets/x/models/n.json
file\tassets+o\tassets/x/textures/t.png\tassets/assets/x/textures/t.png
view\tassets+o@90.*\t90.*\tassets,o\tfalse\t4
file\tassets+o@90.*\tassets/minecraft/gpu_warnlist.json\tassets/assets/minecraft/gpu_warnlist.json
file\tassets+o@90.*\tassets/x/models/m.json\to/assets/x/models/m.json
file\tassets+o@90.*\tassets/x/models/n.json\to/assets/x/models/n.json
file\tassets+o@90.*\tassets/x/textures/t.png\tassets/assets/x/textures/t.png
kind\tassets/assets/minecraft/gpu_warnlist.json\tother\tassets\tassets/minecraft/gpu_warnlist.json
kind\tassets/assets/x/models/m.json\tmodel\tassets\tassets/x/models/m.json
kind\tassets/assets/x/textures/t.png\ttexture\tassets\tassets/x/textures/t.png
kind\tassets/minecraft/gpu_warnlist.json\tunknown\tassets\t-
kind\tassets/pack.mcmeta\tunknown\tassets\t-
kind\tassets/x/models/m.json\tunknown\tassets\t-
kind\to/assets/x/models/m.json\tmodel\to\tassets/x/models/m.json
kind\to/assets/x/models/n.json\tmodel\to\tassets/x/models/n.json
kind\tpack.mcmeta\tpack_meta\t\t-
kind\tpack.png\tpack_meta\t\t-
";
    assert_eq!(dump(&layout), expected);
}

#[test]
fn unknown_jar_format_verifies_every_view() {
    let unknown = VanillaIndex::default();
    let layout = layout(&[("pack.mcmeta", META)], &unknown);
    assert!(layout.views.iter().all(|view| view.verified));
    assert_eq!(layout.views.len(), 3);
}

#[test]
fn view_set_has_no_cap() {
    let mut set: ViewSet = [3, 64, 130, 1000].into_iter().map(ViewId).collect();
    assert!(set.contains(ViewId(1000)) && set.contains(ViewId(64)));
    assert!(!set.contains(ViewId(65)) && !set.contains(ViewId(5000)));
    assert_eq!(set.len(), 4);
    assert_eq!(
        set.iter().collect::<Vec<_>>(),
        [ViewId(3), ViewId(64), ViewId(130), ViewId(1000)]
    );
    set.remove(ViewId(1000));
    let small: ViewSet = [3, 64, 130].into_iter().map(ViewId).collect();
    assert_eq!(set, small, "removal trims, so equal sets are equal");
    let mut both = small.clone();
    both.intersect_with(&[130, 2000].into_iter().map(ViewId).collect());
    assert_eq!(both, [130].into_iter().map(ViewId).collect());
    let mut union = ViewSet::new();
    union.union_with(&small);
    assert_eq!(union, small);
    let mut emptied = [200].into_iter().map(ViewId).collect::<ViewSet>();
    emptied.remove(ViewId(200));
    assert!(emptied.is_empty());
    assert_eq!(emptied, ViewSet::new());
}

/// A pack's layout as lines: its views with their files, then every file's kind, layer and logical path.
fn dump(layout: &Layout) -> String {
    let mut out = String::new();
    for view in &layout.views {
        let name = &view.name;
        let overlays: Vec<&str> = view.layers.iter().filter_map(overlay_name).collect();
        writeln!(
            out,
            "view\t{name}\t{}\t{}\t{}\t{}",
            show_format(view.formats.min),
            overlays.join(","),
            view.verified,
            view.files.len()
        )
        .unwrap();
        for (logical, physical) in &view.files {
            writeln!(out, "file\t{name}\t{logical}\t{physical}").unwrap();
        }
    }
    for (path, kind) in &layout.kinds {
        let (layer, logical) = layout.split(path);
        let layer = overlay_name(&layer).unwrap_or("");
        let logical = logical.as_ref().map_or("-", LogicalPath::as_str);
        // `pack.mcmeta` and `pack.png` have no logical path.
        writeln!(out, "kind\t{path}\t{}\t{layer}\t{logical}", kind.id()).unwrap();
    }
    out
}

fn overlay_name(layer: &Layer) -> Option<&str> {
    match layer {
        Layer::Root => None,
        Layer::Overlay(directory) => Some(directory),
    }
}

#[test]
fn a_layout_updated_as_files_come_and_go_equals_a_new_one() {
    // `a` listed again after `b`: where both apply, `a` ranks at its last place.
    const META: &str = r#"{"pack":{"supported_formats":[80,100]},"overlays":{"entries":[
        {"formats":[84,99],"directory":"a"},{"formats":[88,90],"directory":"b"},
        {"formats":[89,90],"directory":"a"}]}}"#;
    let vanilla = vanilla(&[]);
    let mut pack = pack(&[
        ("pack.mcmeta", META),
        ("assets/x/models/m.json", ""),
        ("a/assets/x/models/m.json", ""),
        ("b/assets/x/models/m.json", ""),
        ("b/assets/x/textures/t.png", ""),
    ]);
    let meta = PackMeta::read(&pack).unwrap();
    let mut layout = Layout::new(&pack, meta.clone(), &vanilla);
    for (path, added) in [
        ("b/assets/x/models/m.json", false),
        ("a/assets/x/models/m.json", false),
        ("b/assets/x/models/m.json", true),
        ("a/assets/x/models/m.json", true),
        ("a/assets/x/models/n.json", true),
        ("c/assets/x/models/m.json", true),
        ("assets/x/models/m.json", false),
        ("b/assets/x/textures/t.png", false),
    ] {
        let path = PhysPath::new(path).unwrap();
        if added {
            pack.files.insert(
                path.clone(),
                Entry {
                    blob: b"".as_slice().into(),
                    digest: Digest([0; 32]),
                },
            );
        } else {
            pack.files.remove(&path);
        }
        layout.update(&pack, [&path], &vanilla);
        assert_eq!(
            layout,
            Layout::new(&pack, meta.clone(), &vanilla),
            "after {} {path}",
            if added { "adding" } else { "removing" }
        );
    }
}
