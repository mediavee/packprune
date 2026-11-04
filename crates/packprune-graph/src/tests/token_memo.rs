use camino::Utf8Path;
use packprune_pack::{Entry, FileKind, PhysPath};

use super::support::open;
use crate::{Computed, TokenMemo, TokenSource};

/// The entries a pack directory holding `files` reads as, with their content digests.
fn entries(files: &[(&str, &str)]) -> Vec<Entry> {
    let dir = tempfile::tempdir().unwrap();
    let root = Utf8Path::from_path(dir.path()).unwrap();
    for (path, text) in files {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let pack = open(root);
    files
        .iter()
        .map(|(path, _)| pack.files[&PhysPath::new(path).unwrap()].clone())
        .collect()
}

/// A memo as the next run reads it.
fn saved(memo: &TokenMemo) -> TokenMemo {
    TokenMemo::from_bytes(&memo.to_bytes()).expect("the bytes this build wrote")
}

const MODEL: &str = "assets/fx/models/box.json";

#[test]
fn memoised_tokens_follow_an_edited_file() {
    let before = entries(&[(MODEL, r#"{"parent": "fx:block/a"}"#)]).remove(0);
    let after = entries(&[(MODEL, r#"{"parent": "fx:block/b"}"#)]).remove(0);
    let memo = TokenMemo::default();
    assert_eq!(
        memo.tokens(&before, FileKind::Model),
        Computed.tokens(&before, FileKind::Model)
    );
    // The same path edited, in this run and in the next.
    for memo in [&memo, &saved(&memo)] {
        assert_eq!(
            memo.tokens(&after, FileKind::Model),
            Computed.tokens(&after, FileKind::Model)
        );
    }
}

#[test]
fn memoised_tokens_are_those_of_the_kind_asked() {
    let entry = entries(&[(MODEL, r#"{"parent": "fx:block/a", "model": "fx:item/b"}"#)]).remove(0);
    let memo = TokenMemo::default();
    let as_model = memo.tokens(&entry, FileKind::Model);
    let as_item = memo.tokens(&entry, FileKind::ItemDefinition);
    assert_ne!(as_model, as_item, "the two kinds read different tokens");
    for memo in [&memo, &saved(&memo)] {
        assert_eq!(memo.tokens(&entry, FileKind::Model), as_model);
        assert_eq!(memo.tokens(&entry, FileKind::ItemDefinition), as_item);
    }
}

#[test]
fn saved_tokens_read_back_as_computed() {
    let files = [
        (
            MODEL,
            r#"{"parent": "fx:block/a", "textures": {"all": "fx:block/stone"}}"#,
        ),
        ("assets/fx/models/broken.json", r#"{"parent": "#),
        (
            "assets/fx/shaders/core/x.fsh",
            "#version 150\n#moj_import <fx:light.glsl>\n#moj_import \"local.glsl\"\nvoid main() {}\n",
        ),
        (
            "assets/fx/sounds.json",
            r#"{"hit": {"sounds": ["fx:hit/one"]}}"#,
        ),
    ];
    let kinds = [
        FileKind::Model,
        FileKind::Model,
        FileKind::Shader,
        FileKind::SoundsJson,
    ];
    let entries = entries(&files);
    let memo = TokenMemo::default();
    for (entry, kind) in entries.iter().zip(kinds) {
        memo.tokens(entry, kind);
    }
    let next = saved(&memo);
    for (entry, kind) in entries.iter().zip(kinds) {
        assert_eq!(next.tokens(entry, kind), Computed.tokens(entry, kind));
    }
    assert!(Computed.tokens(&entries[1], FileKind::Model).is_err());
}
