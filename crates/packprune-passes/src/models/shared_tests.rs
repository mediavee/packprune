use super::super::{SharedDisplay, identical::key};
use super::*;

const DISPLAY: &str = r#""display": {"gui": {"rotation": [30, 225, 0], "translation": [0, 1, 0], "scale": [0.625, 0.625, 0.625]},
 "head": {"rotation": [0, 90, 0], "translation": [0, 4, 0], "scale": [1.2, 1.2, 1.2]}}"#;
const PARENT: &str = "assets/packprune/models/display/e494777bbcddcc0b.json";
const PARENT_ID: &str = "packprune:display/e494777bbcddcc0b";

fn siblings(fx: &Fixture, names: &[&str]) {
    for name in names {
        fx.model(
            name,
            &format!(
                r#"{{"textures": {{"a": "fx:item/a", "{name}": "fx:item/b"}}, {CUBE}, {DISPLAY}}}"#
            ),
        );
    }
}

#[test]
fn shared_display_moves_three_blocks_to_the_sha256_parent_and_keeps_other_keys() {
    let fx = Fixture::new();
    siblings(&fx, &["p", "q", "r"]);
    let before = fx.pack();
    let out = fx.run(&SharedDisplay, &[]);
    assert_eq!(out.state.pack.files.len(), before.files.len() + 1);
    assert!(out.state.pack.files.contains_key(&phys(PARENT)));
    let parent = node(&out, PARENT);
    for name in ["p", "q", "r"] {
        let child = node(&out, &path(name));
        let original = json::parse(&before.files[&phys(&path(name))].blob).unwrap();
        assert!(!child.has("display"));
        assert_eq!(child.path("parent").as_str(), Some(PARENT_ID));
        assert_eq!(key(parent.path("display")), key(original.path("display")));
        for field in ["textures", "elements"] {
            assert_eq!(key(child.path(field)), key(original.path(field)));
        }
    }
    assert_eq!(
        out.reports[0].changes,
        [
            "1 display parents added",
            "3 models now take their display from one"
        ]
    );
    fx.prove(&SharedDisplay);
}

#[test]
fn shared_display_requires_three_equal_blocks() {
    let fx = Fixture::new();
    siblings(&fx, &["p", "q", "r"]);
    for (name, scale) in [("s", "1.3"), ("t", "1.4"), ("u", "1.4")] {
        fx.model(
            name,
            &format!(
                "{{{}}}",
                DISPLAY.replace("1.2, 1.2, 1.2", &format!("{scale}, {scale}, {scale}"))
            ),
        );
    }
    let before = fx.pack();
    let out = fx.run(&SharedDisplay, &[]);
    for name in ["s", "t", "u"] {
        assert_eq!(
            bytes(&out, &path(name)),
            before.files[&phys(&path(name))].blob.as_ref()
        );
    }
    assert_eq!(
        out.reports[0].changes[1],
        "3 models now take their display from one"
    );
    fx.prove(&SharedDisplay);
}

#[test]
fn shared_display_keeps_existing_parents_and_display_overrides() {
    let fx = Fixture::new();
    siblings(&fx, &["p", "q", "r"]);
    fx.model("over", r#"{"parent": "fx:item/p", "display": {"gui": {"rotation": [0, 0, 0], "scale": [2, 2, 2]}}}"#);
    fx.model("kid", &format!(r#"{{"parent": "fx:item/p", {DISPLAY}}}"#));
    let before = fx.pack();
    let out = fx.run(&SharedDisplay, &[]);
    for name in ["over", "kid"] {
        assert_eq!(
            bytes(&out, &path(name)),
            before.files[&phys(&path(name))].blob.as_ref()
        );
    }
    fx.prove(&SharedDisplay);
}

#[test]
fn shared_display_reuses_its_parent_without_self_inheritance() {
    let fx = Fixture::new();
    siblings(&fx, &["p", "q", "r"]);
    let first = fx.run(&SharedDisplay, &[]);
    for (path, entry) in &first.state.pack.files {
        fx.put(path.as_str(), std::str::from_utf8(&entry.blob).unwrap());
    }
    siblings(&fx, &["s", "t", "u"]);
    let out = fx.run(&SharedDisplay, &[]);
    assert_eq!(bytes(&out, PARENT), bytes(&first, PARENT));
    assert!(!node(&out, PARENT).has("parent"));
    for name in ["p", "s", "t", "u"] {
        let child = node(&out, &path(name));
        assert_eq!(child.path("parent").as_str(), Some(PARENT_ID));
        assert!(!child.has("display"));
    }
    assert_eq!(
        out.reports[0].changes,
        [
            "0 display parents added",
            "3 models now take their display from one"
        ]
    );
    fx.prove(&SharedDisplay);
}

#[test]
fn shared_display_refuses_a_conflicting_parent_in_any_layer() {
    for prefix in ["", "o1/"] {
        let fx = Fixture::new();
        siblings(&fx, &["p", "q", "r"]);
        fx.put("pack.mcmeta", r#"{"pack":{"min_format":88,"max_format":88,"description":"t"},"overlays":{"entries":[{"directory":"o1","min_format":88,"max_format":88}]}}"#);
        let planted = format!("{prefix}{PARENT}");
        fx.put(&planted, r#"{"textures":{"a":"fx:item/a"}}"#);
        let out = fx.run(&SharedDisplay, &[]);
        assert!(out.reports[0].changed.is_empty());
        assert_eq!(bytes(&out, &planted), br#"{"textures":{"a":"fx:item/a"}}"#);
    }
}

#[test]
fn shared_display_skips_blocks_that_do_not_pay_for_the_parent() {
    let fx = Fixture::new();
    for name in ["p", "q", "r"] {
        fx.model(name, r#"{"display":{"gui":{"scale":[2,2,2]}}}"#);
    }
    let out = fx.run(&SharedDisplay, &[]);
    assert!(out.reports[0].changed.is_empty());
    assert_eq!(
        out.reports[0].changes,
        [
            "0 display parents added",
            "0 models now take their display from one"
        ]
    );
    fx.prove(&SharedDisplay);
}

#[test]
fn shared_display_groups_only_models_of_the_same_layer() {
    let fx = Fixture::new();
    siblings(&fx, &["a", "b"]);
    fx.model("c", "{}");
    fx.put("pack.mcmeta", r#"{"pack":{"min_format":88,"max_format":88,"description":"t"},"overlays":{"entries":[{"directory":"o1","min_format":88,"max_format":88}]}}"#);
    for name in ["a", "b", "c"] {
        fx.put(&format!("o1/{}", path(name)), &format!("{{{DISPLAY}}}"));
    }
    let before = fx.pack();
    let out = fx.run(&SharedDisplay, &[]);
    assert!(
        out.state
            .pack
            .files
            .contains_key(&phys(&format!("o1/{PARENT}")))
    );
    assert!(!out.state.pack.files.contains_key(&phys(PARENT)));
    for name in ["a", "b"] {
        assert_eq!(
            bytes(&out, &path(name)),
            before.files[&phys(&path(name))].blob.as_ref()
        );
    }
    assert_eq!(
        out.reports[0].changes[1],
        "3 models now take their display from one"
    );
    fx.prove(&SharedDisplay);
}

#[test]
fn model_text_appends_a_parent_after_removal_even_to_an_empty_root() {
    for (source, removed, expected) in [
        (
            r#"{"a": 1}"#,
            vec![],
            r#"{"a": 1,"parent":"packprune:display/x"}"#,
        ),
        (
            r#"{"display": {"gui": {}}}"#,
            vec!["/display"],
            r#"{"parent":"packprune:display/x"}"#,
        ),
        (r#"{ }"#, vec![], r#"{"parent":"packprune:display/x"}"#),
    ] {
        let removed = removed.into_iter().map(str::to_owned).collect();
        assert_eq!(
            text::edit(
                source.as_bytes(),
                &removed,
                Some(("parent", "packprune:display/x"))
            ),
            expected.as_bytes()
        );
    }
    assert_eq!(
        text::edit(b"{}", &BTreeSet::new(), Some(("a\u{b}", "b\n"))),
        br#"{"a\u000B":"b\n"}"#
    );
}

#[test]
fn shared_display_hashes_its_canonical_number_and_string_serialization() {
    let fx = Fixture::new();
    let display = r#""display":{"gui":{"rotation":[1e-4,1e8,16.0]},"note":"\u0008\u0009\u000a\u000b\u000c\u000d\u001a café 😀","long":[2147483648,9223372036854775808]}"#;
    for name in ["a", "b", "c"] {
        fx.model(name, &format!("{{{display}}}"));
    }
    let out = fx.run(&SharedDisplay, &[]);
    let parent = "assets/packprune/models/display/141f602b7c6b1510.json";
    let expected = concat!(
        "{\"display\":{",
        r#""gui":{"rotation":[1.0E-4,1.0E8,16.0]},"note":"\b\t\n\u000B\f\r\u001A café 😀","long":[2147483648,9223372036854775808]}}"#
    );
    assert_eq!(bytes(&out, parent), expected.as_bytes());
    fx.prove(&SharedDisplay);
}

#[test]
fn shared_display_uses_the_first_models_member_order_for_equal_blocks() {
    let fx = Fixture::new();
    siblings(&fx, &["a", "b", "c"]);
    let reversed = r#""display":{"head":{"scale":[1.2,1.2,1.2],"translation":[0,4,0],"rotation":[0,90,0]},"gui":{"scale":[0.625,0.625,0.625],"translation":[0,1,0],"rotation":[30,225,0]}}"#;
    fx.model("b", &format!("{{{reversed}}}"));
    fx.model("c", &format!("{{{reversed}}}"));
    let out = fx.run(&SharedDisplay, &[]);
    assert!(out.state.pack.files.contains_key(&phys(PARENT)));
    for name in ["a", "b", "c"] {
        assert_eq!(
            node(&out, &path(name)).path("parent").as_str(),
            Some(PARENT_ID)
        );
    }
    fx.prove(&SharedDisplay);
}
