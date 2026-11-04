//! Index cases: one import naming two files under two root shaders, an import resolved against the root
//! shader's folder and the parent path of its chain, and `update` after a move equal to `build`.

use std::collections::BTreeMap;

use packprune_pack::{
    ClientRules, Digest, Entry, Layout, PackInput, PackMeta, PhysPath, RunCx, VanillaIndex,
};

use super::support::fixture_vanilla;
use crate::{Change, Computed, Inputs, Names, Policy, Reached, ReferenceIndex, Status};

const META: &str = r#"{"pack": {"min_format": 88, "max_format": 88, "description": "test"}}"#;

pub(super) fn pack(files: &[(&str, &str)]) -> PackInput {
    PackInput {
        name: "test".into(),
        files: files
            .iter()
            .map(|(path, text)| {
                // The index never reads the digest.
                let entry = Entry {
                    blob: text.as_bytes().into(),
                    digest: Digest([0; 32]),
                };
                (PhysPath::new(path).unwrap(), entry)
            })
            .collect(),
        digest: Digest([0; 32]),
    }
}

struct Built {
    layout: Layout,
    index: ReferenceIndex,
}

fn build(cx: &RunCx, pack: &PackInput, vanilla: &VanillaIndex) -> Built {
    let layout = Layout::new(pack, PackMeta::read(pack).unwrap(), vanilla);
    let index = ReferenceIndex::build(cx, pack, &layout, &rules(), &Computed);
    Built { layout, index }
}

fn rules() -> ClientRules {
    ClientRules::for_version("26.2")
}

fn status(
    cx: &RunCx,
    pack: &PackInput,
    vanilla: &VanillaIndex,
    built: &Built,
) -> BTreeMap<String, Status> {
    let reached = Reached::analyse(&Inputs {
        cx,
        pack,
        layout: &built.layout,
        rules: &rules(),
        vanilla,
        index: &built.index,
        sources: &[],
        selectors: &[],
        policy: Policy::Pack,
    })
    .unwrap();
    reached
        .files
        .into_iter()
        .map(|(path, file)| (path.as_str().to_owned(), file.status))
        .collect()
}

/// The logical paths the shader references of `file` name, in index order.
fn imports(built: &Built, file: &str) -> Vec<String> {
    built
        .index
        .references(&PhysPath::new(file).unwrap())
        .iter()
        .filter_map(|reference| match &reference.names {
            Names::Text { logical } => Some(logical.as_str().to_owned()),
            _ => None,
        })
        .collect()
}

/// One quoted import inside one include names a file beside each root shader that compiles it.
#[test]
fn an_import_in_a_shared_include_names_a_file_per_root_shader_folder() {
    let cx = RunCx::default();
    let vanilla = fixture_vanilla();
    let pack = pack(&[
        ("pack.mcmeta", META),
        (
            "assets/fx/shaders/core/a.vsh",
            "#moj_import <fx:common.glsl>\n",
        ),
        (
            "assets/fx/shaders/post/b.fsh",
            "#moj_import <fx:common.glsl>\n",
        ),
        (
            "assets/fx/shaders/include/common.glsl",
            "#moj_import \"helper.glsl\"\n",
        ),
        ("assets/fx/shaders/core/helper.glsl", "// core\n"),
        ("assets/fx/shaders/post/helper.glsl", "// post\n"),
        (
            "assets/fx/shaders/include/helper.glsl",
            "// beside the include: never loaded\n",
        ),
    ]);
    let built = build(&cx, &pack, &vanilla);
    assert_eq!(
        imports(&built, "assets/fx/shaders/include/common.glsl"),
        [
            "assets/fx/shaders/core/helper.glsl",
            "assets/fx/shaders/post/helper.glsl"
        ]
    );
    let status = status(&cx, &pack, &vanilla, &built);
    assert_eq!(
        status["assets/fx/shaders/core/helper.glsl"],
        Status::Reached
    );
    assert_eq!(
        status["assets/fx/shaders/post/helper.glsl"],
        Status::Reached
    );
    assert_eq!(
        status["assets/fx/shaders/include/helper.glsl"],
        Status::Orphan
    );
}

/// A quoted import carries its folder down the chain; an angled one resets it to the root shader's folder.
#[test]
fn an_import_resolves_against_the_root_folder_and_the_carried_parent_path() {
    let cx = RunCx::default();
    let vanilla = fixture_vanilla();
    let pack = pack(&[
        ("pack.mcmeta", META),
        (
            "assets/fx/shaders/post/glow.fsh",
            "#moj_import \"parts/c.glsl\"\n#moj_import <fx:e.glsl>\n",
        ),
        (
            "assets/fx/shaders/post/parts/c.glsl",
            "#moj_import \"d.glsl\"\n",
        ),
        (
            "assets/fx/shaders/post/parts/d.glsl",
            "// parts/ + d.glsl\n",
        ),
        (
            "assets/fx/shaders/post/d.glsl",
            "// not beside the root: never loaded\n",
        ),
        (
            "assets/fx/shaders/include/e.glsl",
            "#moj_import \"f.glsl\"\n",
        ),
        (
            "assets/fx/shaders/post/f.glsl",
            "// the parent resets after an angled import\n",
        ),
    ]);
    let built = build(&cx, &pack, &vanilla);
    assert_eq!(
        imports(&built, "assets/fx/shaders/post/parts/c.glsl"),
        ["assets/fx/shaders/post/parts/d.glsl"]
    );
    assert_eq!(
        imports(&built, "assets/fx/shaders/include/e.glsl"),
        ["assets/fx/shaders/post/f.glsl"]
    );
    let status = status(&cx, &pack, &vanilla, &built);
    assert_eq!(
        status["assets/fx/shaders/post/parts/d.glsl"],
        Status::Reached
    );
    assert_eq!(status["assets/fx/shaders/post/f.glsl"], Status::Reached);
    assert_eq!(status["assets/fx/shaders/post/d.glsl"], Status::Orphan);
}

/// A file moved into an overlay and renamed, its referrer edited, an overlay copy removed (the root one it shadowed
/// shows again), and an include moved where an import looks: `update` gives what `build` gives on the new pack.
#[test]
fn update_after_a_move_equals_build() {
    let cx = RunCx::default();
    let vanilla = fixture_vanilla();
    let overlay_meta = r#"{"pack": {"min_format": 88, "max_format": 88, "description": "test"},
        "overlays": {"entries": [{"formats": [88, 88], "directory": "o"}]}}"#;
    let before = pack(&[
        ("pack.mcmeta", overlay_meta),
        (
            "assets/fx/items/gem.json",
            r#"{"model": {"type": "model", "model": "fx:item/gem"}}"#,
        ),
        (
            "assets/fx/models/item/gem.json",
            r#"{"parent": "fx:item/base", "textures": {"layer0": "fx:item/gem"}}"#,
        ),
        (
            "assets/fx/models/item/base.json",
            r#"{"textures": {"particle": "fx:item/base"}}"#,
        ),
        (
            "o/assets/fx/models/item/base.json",
            r#"{"textures": {"particle": "fx:item/shadow"}}"#,
        ),
        ("assets/fx/textures/item/gem.png", ""),
        ("assets/fx/textures/item/gem.png.mcmeta", "{}"),
        (
            "assets/fx/shaders/core/a.vsh",
            "#moj_import \"lib/x.glsl\"\n",
        ),
        ("assets/fx/shaders/lib/y.glsl", "#moj_import \"z.glsl\"\n"),
    ]);
    let after = pack(&[
        ("pack.mcmeta", overlay_meta),
        (
            "assets/fx/items/gem.json",
            r#"{"model": {"type": "model", "model": "fx:item/g"}}"#,
        ),
        (
            "o/assets/fx/models/item/g.json",
            r#"{"parent": "fx:item/base", "textures": {"layer0": "fx:item/gem"}}"#,
        ),
        (
            "assets/fx/models/item/base.json",
            r#"{"textures": {"particle": "fx:item/base"}}"#,
        ),
        ("o/assets/fx/textures/item/gem.png", ""),
        ("assets/fx/textures/item/gem.png.mcmeta", "{}"),
        (
            "assets/fx/shaders/core/a.vsh",
            "#moj_import \"lib/x.glsl\"\n",
        ),
        (
            "assets/fx/shaders/core/lib/x.glsl",
            "#moj_import \"z.glsl\"\n",
        ),
    ]);
    let path = |text: &str| PhysPath::new(text).unwrap();
    let change = Change {
        touched: [
            path("assets/fx/items/gem.json"),
            path("o/assets/fx/models/item/base.json"),
        ]
        .into(),
        moved: [
            (
                "assets/fx/models/item/gem.json",
                "o/assets/fx/models/item/g.json",
            ),
            (
                "assets/fx/textures/item/gem.png",
                "o/assets/fx/textures/item/gem.png",
            ),
            (
                "assets/fx/shaders/lib/y.glsl",
                "assets/fx/shaders/core/lib/x.glsl",
            ),
        ]
        .into_iter()
        .map(|(old, new)| (path(old), path(new)))
        .collect(),
        layout_changed: false,
    };
    let mut updated = build(&cx, &before, &vanilla);
    let fresh = build(&cx, &after, &vanilla);
    assert_ne!(
        updated.index.all(),
        fresh.index.all(),
        "the move changes the index"
    );
    updated
        .index
        .update(&cx, &after, &fresh.layout, &rules(), &change, &Computed);
    assert_eq!(updated.index.all(), fresh.index.all());
    assert_eq!(
        imports(&fresh, "assets/fx/shaders/core/lib/x.glsl"),
        ["assets/fx/shaders/core/lib/z.glsl"]
    );
    let shown = |built: &Built| {
        let reached = Reached::analyse(&Inputs {
            cx: &cx,
            pack: &after,
            layout: &fresh.layout,
            rules: &rules(),
            vanilla: &vanilla,
            index: &built.index,
            sources: &[],
            selectors: &[],
            policy: Policy::Pack,
        })
        .unwrap();
        (reached.files, reached.edges, reached.missing)
    };
    assert_eq!(shown(&updated), shown(&fresh));
}

/// A `pack.mcmeta` edit that drops an overlay changes which files views show: every file is attached again.
#[test]
fn update_after_a_layout_change_equals_build() {
    let cx = RunCx::default();
    let vanilla = fixture_vanilla();
    let files = |meta| {
        pack(&[
            ("pack.mcmeta", meta),
            ("assets/fx/models/a.json", r#"{"parent": "fx:b"}"#),
            ("o/assets/fx/models/a.json", r#"{"parent": "fx:c"}"#),
        ])
    };
    let before = files(
        r#"{"pack": {"min_format": 88, "max_format": 88, "description": "t"},
            "overlays": {"entries": [{"formats": [88, 88], "directory": "o"}]}}"#,
    );
    let after = files(META);
    let mut updated = build(&cx, &before, &vanilla);
    let fresh = build(&cx, &after, &vanilla);
    let change = Change {
        touched: [PhysPath::new("pack.mcmeta").unwrap()].into(),
        moved: BTreeMap::new(),
        layout_changed: true,
    };
    updated
        .index
        .update(&cx, &after, &fresh.layout, &rules(), &change, &Computed);
    assert_eq!(updated.index.all(), fresh.index.all());
}

/// A file an overlay listed later shadows, an include a root shader stops importing, and a texture removed beside
/// its `.mcmeta`: none of the three files whose references change is touched, and `update` still gives `build`.
#[test]
fn update_renotes_the_files_an_edit_changes_without_touching_them() {
    let cx = RunCx::default();
    let vanilla = fixture_vanilla();
    let meta = r#"{"pack": {"min_format": 88, "max_format": 88, "description": "test"},
        "overlays": {"entries": [{"formats": [88, 88], "directory": "o1"},
                                 {"formats": [88, 88], "directory": "o2"}]}}"#;
    let common: &[(&str, &str)] = &[
        ("pack.mcmeta", meta),
        ("o1/assets/fx/models/a.json", r#"{"parent": "fx:b"}"#),
        (
            "assets/fx/shaders/include/common.glsl",
            "#moj_import \"helper.glsl\"\n",
        ),
        ("assets/fx/shaders/core/helper.glsl", "// helper\n"),
        ("assets/fx/textures/t.png.mcmeta", "{}"),
    ];
    let with = |more: &[(&'static str, &'static str)]| {
        pack(&common.iter().chain(more).copied().collect::<Vec<_>>())
    };
    let before = with(&[
        (
            "assets/fx/shaders/core/a.vsh",
            "#moj_import <fx:common.glsl>\n",
        ),
        ("assets/fx/textures/t.png", ""),
    ]);
    let after = with(&[
        ("o2/assets/fx/models/a.json", r#"{"parent": "fx:c"}"#),
        ("assets/fx/shaders/core/a.vsh", "// no import\n"),
    ]);
    let change = Change {
        touched: [
            "o2/assets/fx/models/a.json",
            "assets/fx/shaders/core/a.vsh",
            "assets/fx/textures/t.png",
        ]
        .map(|text| PhysPath::new(text).unwrap())
        .into(),
        moved: BTreeMap::new(),
        layout_changed: false,
    };
    let mut updated = build(&cx, &before, &vanilla);
    let fresh = build(&cx, &after, &vanilla);
    for file in [
        "o1/assets/fx/models/a.json",
        "assets/fx/shaders/include/common.glsl",
        "assets/fx/textures/t.png.mcmeta",
    ] {
        let file = PhysPath::new(file).unwrap();
        assert!(!updated.index.references(&file).is_empty(), "{file}");
        assert!(fresh.index.references(&file).is_empty(), "{file}");
    }
    updated
        .index
        .update(&cx, &after, &fresh.layout, &rules(), &change, &Computed);
    assert_eq!(updated.index.all(), fresh.index.all());
}

/// A `.mcmeta` is noted when the walk visits the texture beside it, in the views that show both: one without its
/// texture, or beside a file of another kind, is noted by no view.
#[test]
fn a_texture_meta_is_noted_in_the_views_that_show_its_texture() {
    let cx = RunCx::default();
    let vanilla = fixture_vanilla();
    let built = build(
        &cx,
        &pack(&[
            ("pack.mcmeta", META),
            ("assets/fx/textures/t.png", ""),
            ("assets/fx/textures/t.png.mcmeta", "{}"),
            ("assets/fx/textures/lonely.png.mcmeta", "{}"),
            ("assets/fx/models/m.json", "{}"),
            ("assets/fx/models/m.json.mcmeta", "{}"),
        ]),
        &vanilla,
    );
    let beside = |file: &str| -> Vec<String> {
        built
            .index
            .references(&PhysPath::new(file).unwrap())
            .iter()
            .map(|reference| format!("{:?} {:?}", reference.names, reference.views))
            .collect()
    };
    assert_eq!(beside("assets/fx/textures/t.png.mcmeta").len(), 1);
    assert!(beside("assets/fx/textures/lonely.png.mcmeta").is_empty());
    assert!(beside("assets/fx/models/m.json.mcmeta").is_empty());
}

/// An include kept for itself (here a source names it) is compiled as each vanilla shader folder would import it.
#[test]
fn a_kept_include_is_walked_from_each_vanilla_shader_folder() {
    let cx = RunCx::default();
    let vanilla = fixture_vanilla();
    let pack = pack(&[
        ("pack.mcmeta", META),
        (
            "assets/fx/shaders/include/kept.glsl",
            "#moj_import \"helper.glsl\"\n",
        ),
        (
            "assets/minecraft/shaders/core/helper.glsl",
            "// beside vanilla's core shaders\n",
        ),
        (
            "assets/fx/shaders/include/helper.glsl",
            "// beside the include: never loaded\n",
        ),
    ]);
    let built = build(&cx, &pack, &vanilla);
    let keep = crate::SourceUsages {
        source: "keep".into(),
        path: "keep.yml".into(),
        usages: vec![crate::Usage {
            kind: crate::UsageKind::Path,
            id: crate::IdGlob("assets/fx/shaders/include/kept.glsl".into()),
            atlas: None,
            location: "keep.yml:3".into(),
            reason: None,
            declares: None,
            quiet: false,
        }],
    };
    let reached = Reached::analyse(&Inputs {
        cx: &cx,
        pack: &pack,
        layout: &built.layout,
        rules: &rules(),
        vanilla: &vanilla,
        index: &built.index,
        sources: &[keep],
        selectors: &[],
        policy: Policy::Pack,
    })
    .unwrap();
    let status = |path: &str| reached.files[&PhysPath::new(path).unwrap()].status;
    assert_eq!(
        status("assets/minecraft/shaders/core/helper.glsl"),
        Status::Reached
    );
    assert_eq!(
        status("assets/fx/shaders/include/helper.glsl"),
        Status::Orphan
    );
}
