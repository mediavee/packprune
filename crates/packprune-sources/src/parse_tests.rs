use std::fs;
use std::time::{Duration, Instant};

use camino::Utf8Path;
use packprune_graph::SourceError;
use packprune_pack::RunCx;
use serde_json::Value;

use crate::tests::rows;
use crate::{PROVIDERS, configs, nexo};

fn cases(names: &[&str]) {
    let cases: Vec<Value> = serde_json::from_str(include_str!("parse-cases.json")).unwrap();
    for case in cases
        .iter()
        .filter(|c| names.contains(&c["name"].as_str().unwrap()))
    {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap();
        for (path, text) in case["files"].as_object().unwrap() {
            let file = root.join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, text.as_str().unwrap()).unwrap();
        }
        let cx = RunCx::default();
        let provider = PROVIDERS
            .iter()
            .find(|p| p.id == case["source"].as_str().unwrap())
            .unwrap();
        let usages = (provider.open)(root, &cx).unwrap().collect().unwrap();
        assert_eq!(
            serde_json::to_value(rows(&usages, &cx)).unwrap(),
            case["expected"],
            "{}",
            case["name"]
        );
    }
}

#[test]
fn profile_globs_need_a_folder_before_a_double_star_slash() {
    cases(&["glob"]);
}

#[test]
fn nexo_refuses_only_mappings_it_reads() {
    cases(&["nexo-merge", "nexo-nullkey"]);
    let dir = tempfile::tempdir().unwrap();
    let root = Utf8Path::from_path(dir.path()).unwrap();
    fs::create_dir(root.join("items")).unwrap();
    fs::write(root.join("settings.yml"), "Misc:\n  <<: {a: 1}\n").unwrap();
    fs::write(
        root.join("items/a.yml"),
        "a:\n  Pack: {model: fx:a}\n  lore: {? [a, b]: x}\n",
    )
    .unwrap();
    let cx = RunCx::default();
    assert!(nexo::open(root, &cx).unwrap().collect().is_ok());
    fs::write(
        root.join("items/a.yml"),
        "a:\n  Pack:\n    <<: {model: fx:a}\n",
    )
    .unwrap();
    let error = nexo::open(root, &cx).unwrap().collect().unwrap_err();
    assert!(matches!(error, SourceError::Refused(_, _)));
    assert!(error.to_string().contains("items/a.yml:3"));
    assert!(error.to_string().contains("YAML merge keys"));
}

#[test]
fn yaml_uses_json_schema_and_accepts_tags() {
    cases(&["tags", "tags2", "nexo-schema"]);
}

#[test]
fn flow_indentation_and_block_lines_match_snakeyaml() {
    cases(&["lines", "lines2", "indent1", "indent2", "indent3"]);
}

#[test]
fn json_duplicates_take_last_values_in_original_key_order() {
    cases(&["edge", "dp"]);
}

#[test]
fn yaml_limits_match_snake_yaml_load_settings() {
    let cx = RunCx::default();
    let mut aliases = "base: &a {font: fx:a}\n".to_owned();
    for i in 0..50 {
        aliases.push_str(&format!("a{i}: *a\n"));
    }
    assert_eq!(
        configs::yaml_usages(aliases.as_bytes(), &cx).unwrap().len(),
        51
    );
    let two_documents = format!("{aliases}---\n{aliases}");
    assert_eq!(
        configs::yaml_usages(two_documents.as_bytes(), &cx)
            .unwrap()
            .len(),
        102
    );
    aliases.push_str("too_many: *a\n");
    let error = configs::yaml_usages(aliases.as_bytes(), &cx).unwrap_err();
    assert!(error.to_string().contains("max=50"));
    assert!(error.to_string().contains("input.yml"));

    let mut scalar_aliases = "font: &a fx:a\n".to_owned();
    for i in 0..80 {
        scalar_aliases.push_str(&format!("a{i}: *a\n"));
    }
    assert!(configs::yaml_usages(scalar_aliases.as_bytes(), &cx).is_ok());
    let mut nested = String::new();
    for i in 0..200 {
        nested.push_str(&format!("{}a:\n", "  ".repeat(i)));
    }
    nested.push_str(&format!("{}font: fx:deep\n", "  ".repeat(200)));
    assert_eq!(
        configs::yaml_usages(nested.as_bytes(), &cx).unwrap().len(),
        1
    );

    let flow = format!("a: {}{}", "[".repeat(2000), "]".repeat(2000));
    assert!(configs::yaml_usages(flow.as_bytes(), &cx).is_ok());
    assert!(nexo::yaml_usages(flow.as_bytes(), &cx).is_ok());

    let unicode = format!("data: {}\n", "é".repeat(1_600_000));
    assert!(configs::yaml_usages(unicode.as_bytes(), &cx).is_ok());
    let too_large = format!("data: {}\n", "x".repeat(3 * 1024 * 1024));
    let error = configs::yaml_usages(too_large.as_bytes(), &cx).unwrap_err();
    assert!(error.to_string().contains("3145728 code points"));
    assert!(error.to_string().contains("input.yml"));
}

#[test]
fn underindented_36kb_flow_completes_within_bound() {
    let input = format!("a:\n{}", "  b: [\n]\n".repeat(4000));
    assert_eq!(input.len(), 36_003);
    let started = Instant::now();
    let usages = configs::yaml_usages(input.as_bytes(), &RunCx::default()).unwrap();
    assert!(usages.is_empty());
    assert!(started.elapsed() < Duration::from_secs(5));
}

// Wall-clock bounds well above the linear time (about 2 s in debug), for a loaded runner; a quadratic expansion
// takes about a minute at 88 574 usages.
fn bounded_alias_expansion(input: &[u8], expected: usize, bound: Duration) {
    let started = Instant::now();
    let usages = configs::yaml_usages(input, &RunCx::default()).unwrap();
    assert_eq!(usages.len(), expected);
    assert!(usages.iter().all(|u| u.id.0.as_ref() == "fx:x"));
    assert!(started.elapsed() < bound);
}

#[test]
fn nested_aliases_278_bytes_complete_within_bound() {
    bounded_alias_expansion(
        include_bytes!("nested-aliases-10.yml"),
        88_574,
        Duration::from_secs(30),
    );
}

#[test]
fn nested_aliases_307_bytes_complete_within_bound() {
    bounded_alias_expansion(
        include_bytes!("nested-aliases-11.yml"),
        265_721,
        Duration::from_secs(60),
    );
}

#[test]
fn nonspecific_and_verbatim_string_tags_keep_usages() {
    let input = b"a: ! \"<font:fx:ns>\"\nb: !<tag:yaml.org,2002:str> \"<font:fx:v>\"\nc: ! <font:fx:plain>\n";
    let usages = configs::yaml_usages(input, &RunCx::default()).unwrap();
    let ids: Vec<_> = usages.iter().map(|u| u.id.0.as_ref()).collect();
    assert_eq!(ids, ["fx:ns", "fx:v", "fx:plain"]);
}

#[test]
fn underindented_flow_continuation_accepts_tab() {
    let input = b"a:\n  b: [x,\n\t\"<font:fx:tab>\"]\n";
    let usages = configs::yaml_usages(input, &RunCx::default()).unwrap();
    assert_eq!(usages.len(), 1);
    assert_eq!(usages[0].id.0.as_ref(), "fx:tab");
}
