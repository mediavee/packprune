use std::collections::BTreeSet;
use std::fs;

use packprune_pack::json::{self, Node};
use packprune_pack::{ClientRules, Limits, LogicalPath, PackInput, PhysPath, RunCx, VanillaIndex};
use packprune_pass::{Context, Enabled, Kept, NoCache, Outcome, Pass, Scope, prove, run};

use super::{DegenerateFaces, IdenticalModels, text};

fn path(name: &str) -> String {
    format!("assets/fx/models/item/{name}.json")
}

fn phys(path: &str) -> PhysPath {
    PhysPath::new(path).unwrap()
}

struct Fixture {
    dir: tempfile::TempDir,
    client: PackInput,
    vanilla: VanillaIndex,
    rules: ClientRules,
    run: RunCx,
}

impl Fixture {
    fn new() -> Self {
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/vanilla");
        let client = PackInput::open(root.to_str().unwrap().into(), &Limits::default()).unwrap();
        let vanilla = VanillaIndex {
            version: "26.2".into(),
            pack_format: "88.0".into(),
            names: client
                .files
                .keys()
                .filter(|p| p.as_str().starts_with("assets/"))
                .map(|p| p.as_str().to_owned())
                .collect(),
            atlases: client
                .files
                .iter()
                .filter(|(p, _)| p.as_str().starts_with("assets/minecraft/atlases/"))
                .map(|(p, e)| (p.as_str().to_owned(), e.blob.to_vec()))
                .collect(),
            ..VanillaIndex::default()
        };
        let fx = Self {
            dir: tempfile::tempdir().unwrap(),
            client,
            rules: ClientRules::for_version(&vanilla.version),
            vanilla,
            run: RunCx::default(),
        };
        fx.put(
            "pack.mcmeta",
            r#"{"pack":{"min_format":88,"max_format":88,"description":"t"}}"#,
        );
        fx
    }

    fn put(&self, path: &str, text: &str) {
        let file = self.dir.path().join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
    }

    fn model(&self, name: &str, json: &str) {
        self.put(&path(name), json);
        self.put(
            &format!("assets/fx/items/{name}_item.json"),
            &format!(r#"{{"model": {{"type": "minecraft:model", "model": "fx:item/{name}"}}}}"#),
        );
    }

    fn pack(&self) -> PackInput {
        PackInput::open(self.dir.path().to_str().unwrap().into(), &Limits::default()).unwrap()
    }

    fn cx(&self) -> Context<'_> {
        Context {
            run: &self.run,
            vanilla: &self.vanilla,
            rules: &self.rules,
            sources: &[],
            selectors: &[],
            cache: &NoCache,
        }
    }

    fn run(&self, pass: &dyn Pass, names: &[&str]) -> Outcome {
        let cx = self.cx();
        let state = cx.analyse(self.pack()).unwrap();
        let mut kept = Kept::of(&state);
        kept.names
            .extend(names.iter().map(|p| LogicalPath::new(p).unwrap()));
        run(&cx, &kept, &state, &[enabled(pass)]).unwrap()
    }

    fn prove(&self, pass: &dyn Pass) {
        let proof = prove(&self.cx(), &self.client, self.pack(), vec![enabled(pass)]).unwrap();
        if let Some(comparison) = proof.proof {
            assert_eq!(comparison.differing_keys, 0, "{:?}", comparison.differences);
        }
    }
}

fn enabled(pass: &dyn Pass) -> Enabled<'_> {
    Enabled {
        pass,
        scope: Scope::all(),
        options: toml::Table::new(),
    }
}

fn bytes<'a>(result: &'a Outcome, path: &str) -> &'a [u8] {
    &result.state.pack.files[&phys(path)].blob
}

fn node(result: &Outcome, path: &str) -> Node {
    json::parse(bytes(result, path)).unwrap()
}

const CUBE: &str = r##""elements": [{"from": [0, 0, 0], "to": [16, 16, 16], "faces": {"north": {"texture": "#a"}, "south": {"texture": "#a"}}}]"##;

#[test]
fn drops_zero_area_faces_and_elements_left_without_a_face() {
    let fx = Fixture::new();
    fx.model("flat", r##"{"textures": {"a": "fx:item/a"}, "elements": [
        {"from": [0, 0, 8], "to": [16, 16, 8], "faces": {"north": {"texture": "#a"}, "south": {"texture": "#a"}, "up": {"texture": "#a"}, "east": {"texture": "#a"}}},
        {"from": [4, 0, 0], "to": [4, 0, 16], "faces": {"up": {"texture": "#a"}, "west": {"texture": "#a"}}},
        {"from": [1, 1, 1], "to": [2, 2, 2], "faces": {"down": {"texture": "#a"}}},
        {"from": [8, 0, 0], "to": [8, 16, 16], "faces": {"north": {"texture": "#a"}, "west": {"texture": "#a"}}},
        {"from": [0, 8, 0], "to": [16, 8, 16], "faces": {"up": {"texture": "#a"}, "south": {"texture": "#a"}, "west": {"texture": "#a"}}}]}"##);
    let out = fx.run(&DegenerateFaces, &[]);
    let model = node(&out, &path("flat"));
    let elements = model.path("elements").items();
    assert_eq!(elements.len(), 4);
    for (element, expected) in elements.iter().zip([
        vec!["north", "south"],
        vec!["down"],
        vec!["west"],
        vec!["up"],
    ]) {
        assert_eq!(
            element
                .path("faces")
                .members()
                .iter()
                .map(|m| m.name.as_ref())
                .collect::<Vec<_>>(),
            expected
        );
    }
    assert_eq!(
        out.reports[0].changes,
        [
            "7 zero-area faces dropped",
            "1 elements left without a face removed",
            "1 models rewritten"
        ]
    );
    fx.prove(&DegenerateFaces);
}

#[test]
fn full_and_malformed_elements_keep_every_byte() {
    let fx = Fixture::new();
    let full = format!(r#"{{"textures": {{"a": "fx:item/a"}}, {CUBE}}}"#);
    let odd = r##"{"elements": [{"from": [0, 0], "to": [0, 0, 0], "faces": {"north": {"texture": "#a"}}}]}"##;
    fx.model("full", &full);
    fx.model("odd", odd);
    let out = fx.run(&DegenerateFaces, &[]);
    assert_eq!(bytes(&out, &path("full")), full.as_bytes());
    assert_eq!(bytes(&out, &path("odd")), odd.as_bytes());
    assert!(out.reports[0].changed.is_empty());
    fx.prove(&DegenerateFaces);
}

fn twins(fx: &Fixture) {
    fx.model(
        "a",
        &format!(r#"{{"textures": {{"a": "fx:item/a"}}, {CUBE}}}"#),
    );
    fx.model("b", r##"{ "elements": [{"faces": {"south": {"texture": "#a"}, "north": {"texture": "#a"}}, "to": [16, 16, 16], "from": [0, 0, 0]}],
  "textures": {"a": "fx:item/a"} }"##);
    fx.model(
        "c",
        &format!(r#"{{"textures": {{"a": "fx:item/b"}}, {CUBE}}}"#),
    );
    fx.model("d", r#"{"parent": "fx:item/b"}"#);
}

#[test]
fn identical_models_merge_and_the_framework_splices_every_reference() {
    let fx = Fixture::new();
    twins(&fx);
    let out = fx.run(&IdenticalModels, &[]);
    assert!(out.state.pack.files.contains_key(&phys(&path("a"))));
    assert!(!out.state.pack.files.contains_key(&phys(&path("b"))));
    assert!(out.state.pack.files.contains_key(&phys(&path("c"))));
    assert_eq!(
        bytes(&out, "assets/fx/items/b_item.json"),
        br#"{"model": {"type": "minecraft:model", "model": "fx:item/a"}}"#
    );
    assert_eq!(bytes(&out, &path("d")), br#"{"parent": "fx:item/a"}"#);
    let removed = fx.pack().files[&phys(&path("b"))].blob.len();
    assert_eq!(
        out.reports[0].changes,
        [
            format!("1 identical models merged into another, {removed} B of files removed"),
            "0 identical models left, their names stay".into()
        ]
    );
    fx.prove(&IdenticalModels);
}

#[test]
fn kept_names_are_targets_and_two_kept_twins_both_stay() {
    let fx = Fixture::new();
    twins(&fx);
    let one = fx.run(&IdenticalModels, &[&path("b")]);
    assert!(one.state.pack.files.contains_key(&phys(&path("b"))));
    assert!(!one.state.pack.files.contains_key(&phys(&path("a"))));
    assert_eq!(bytes(&one, &path("d")), br#"{"parent": "fx:item/b"}"#);
    let both = fx.run(&IdenticalModels, &[&path("a"), &path("b")]);
    assert!(both.state.pack.files.contains_key(&phys(&path("a"))));
    assert!(both.state.pack.files.contains_key(&phys(&path("b"))));
    assert_eq!(
        both.reports[0].changes[1],
        "1 identical models left, their names stay"
    );
}

#[test]
fn models_in_different_layers_and_shadowed_paths_do_not_merge() {
    let fx = Fixture::new();
    twins(&fx);
    fx.put("pack.mcmeta", r#"{"pack":{"min_format":88,"max_format":88,"description":"t"},"overlays":{"entries":[{"directory":"o1","min_format":88,"max_format":88}]}}"#);
    fx.put(
        &format!("o1/{}", path("e")),
        &format!(r#"{{"textures": {{"a": "fx:item/a"}}, {CUBE}}}"#),
    );
    fx.put(
        &format!("o1/{}", path("b")),
        r#"{"textures":{"a":"fx:item/b"}}"#,
    );
    let out = fx.run(&IdenticalModels, &[]);
    assert!(out.state.pack.files.contains_key(&phys(&path("b"))));
    assert!(
        out.state
            .pack
            .files
            .contains_key(&phys(&format!("o1/{}", path("e"))))
    );
    assert!(out.reports[0].changed.is_empty());
    fx.prove(&IdenticalModels);
}

#[test]
fn parsed_number_types_are_distinct_and_the_shortest_name_wins() {
    let fx = Fixture::new();
    fx.model(
        "a_long",
        r#"{"ambientocclusion":true,"display":{"gui":{"scale":[16,16,16]}}}"#,
    );
    fx.model(
        "z",
        r#"{ "display": {"gui": {"scale": [16,16,16]}}, "ambientocclusion": true }"#,
    );
    fx.model(
        "float",
        r#"{"ambientocclusion":true,"display":{"gui":{"scale":[16.0,16,16]}}}"#,
    );
    let out = fx.run(&IdenticalModels, &[]);
    assert!(!out.state.pack.files.contains_key(&phys(&path("a_long"))));
    assert!(out.state.pack.files.contains_key(&phys(&path("z"))));
    assert!(out.state.pack.files.contains_key(&phys(&path("float"))));
    fx.prove(&IdenticalModels);
}

#[test]
fn vanilla_names_stay_and_invalid_or_non_object_models_are_untouched() {
    let mut fx = Fixture::new();
    fx.model("a", "{}");
    fx.model("z", "{}");
    fx.model("invalid", "{bad}");
    fx.model("array", "[]");
    fx.vanilla.names.insert(path("z"));
    let cx = fx.cx();
    let state = cx.analyse(fx.pack()).unwrap();
    let out = run(&cx, &Kept::default(), &state, &[enabled(&IdenticalModels)]).unwrap();
    assert!(!out.state.pack.files.contains_key(&phys(&path("a"))));
    assert!(out.state.pack.files.contains_key(&phys(&path("z"))));
    assert_eq!(bytes(&out, &path("invalid")), b"{bad}");
    assert_eq!(bytes(&out, &path("array")), b"[]");
}

#[test]
fn model_text_removes_members_arrays_and_nested_values_without_reserializing() {
    let cases = [
        (
            r#"{ "a" : [1 , 2.0], "b":{"c": 3} }"#,
            vec![],
            r#"{ "a" : [1 , 2.0], "b":{"c": 3} }"#,
        ),
        (
            r#"{"a": 1, "b": {"x": [1, 2]}, "c": "s,"}"#,
            vec!["/a"],
            r#"{"b": {"x": [1, 2]},"c": "s,"}"#,
        ),
        (
            r#"{"a": 1, "b": {"x": [1, 2]}, "c": "s,"}"#,
            vec!["/b"],
            r#"{"a": 1,"c": "s,"}"#,
        ),
        (
            r#"{"a": 1, "b": {"x": [1, 2]}, "c": "s,"}"#,
            vec!["/c"],
            r#"{"a": 1,"b": {"x": [1, 2]}}"#,
        ),
        (r#"{"a": 1, "b": 2}"#, vec!["/a", "/b"], "{}"),
        (
            r#"{"e": [{"k": 1}, {"k": 2}, {"k": 3}]}"#,
            vec!["/e/0", "/e/2"],
            r#"{"e": [{"k": 2}]}"#,
        ),
        (r#"{"e": [1, 2]}"#, vec!["/e/0", "/e/1"], r#"{"e": []}"#),
        (
            r#"{"e": [{"f": {"n": 1, "s": 2}}, {"f": {"n": 3}}], "z":  [ 1 , 2 ]}"#,
            vec!["/e/0/f/n"],
            r#"{"e": [{"f": {"s": 2}},{"f": {"n": 3}}],"z":  [ 1 , 2 ]}"#,
        ),
        (
            r#"{"a/b": 1, "a": {"b": 2}}"#,
            vec!["/a~1b"],
            r#"{"a": {"b": 2}}"#,
        ),
        (
            r#" {"a":1, "a":2, "b": 3} "#,
            vec!["/b"],
            r#" {"a":1,"a":2} "#,
        ),
    ];
    for (source, removed, expected) in cases {
        let removed = removed
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            text::remove(source.as_bytes(), &removed),
            expected.as_bytes(),
            "{source}"
        );
    }
}

#[test]
fn registry_exposes_the_lossless_model_passes_with_empty_options() {
    use packprune_pass::{Loss, Stage};
    let passes = super::passes();
    assert_eq!(
        passes.iter().map(|p| p.name()).collect::<Vec<_>>(),
        ["degenerate-faces", "identical-models", "shared-display"]
    );
    for pass in passes {
        assert_eq!(pass.stage(), Stage::Models);
        assert_eq!(pass.loss(), Loss::Lossless);
        assert_eq!(
            pass.options_schema(),
            schemars::json_schema!({"type": "object", "additionalProperties": false})
        );
    }
}

#[path = "shared_tests.rs"]
mod shared;
