use std::fmt::Write;
use std::path::Path;

use super::*;

/// One case's dump, in the line format of `cases/expected.txt`: the range, the overlays, then the views without a jar and with the
/// 26.2 jar's format.
fn dump(name: &str, bytes: &[u8]) -> String {
    let mut out = format!("case\t{name}\n");
    let meta = match PackMeta::parse(bytes) {
        Ok(meta) => meta,
        Err(_) => {
            out.push_str("error\n");
            return out;
        }
    };
    let range =
        |range: &FormatRange| format!("{}..{}", show_format(range.min), show_format(range.max));
    writeln!(
        out,
        "formats\t{}",
        meta.formats.as_ref().map_or("-".into(), range)
    )
    .unwrap();
    for overlay in &meta.overlays {
        writeln!(
            out,
            "overlay\t{}\t{}",
            overlay.directory,
            range(&overlay.formats)
        )
        .unwrap();
    }
    for jar in [None, Some((88, 0))] {
        for view in meta.views(jar) {
            writeln!(
                out,
                "view\t{}\t{}\t{}\t{}\t{}",
                jar.map_or("-".into(), |jar| show_format(jar).to_string()),
                view.id,
                show_format(view.format),
                view.overlays.join(","),
                view.verified
            )
            .unwrap();
        }
    }
    out
}

/// Every case of `cases/` against the golden `cases/expected.txt`. Among them, 130 overlays of distinct ranges give
/// 130 views.
#[test]
fn parse_and_views_match_the_golden_cases() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/meta/cases");
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".mcmeta"))
        .collect();
    names.sort();
    let ours: String = names
        .iter()
        .map(|name| dump(name, &std::fs::read(dir.join(name)).unwrap()))
        .collect();
    let expected = std::fs::read_to_string(dir.join("expected.txt")).unwrap();
    for (line, (ours, theirs)) in ours.lines().zip(expected.lines()).enumerate() {
        assert_eq!(ours, theirs, "line {}", line + 1);
    }
    assert_eq!(ours.lines().count(), expected.lines().count());
    assert_eq!(names.len(), 43);
}

#[test]
fn one_hundred_thirty_overlays_give_one_hundred_thirty_views() {
    let entries: Vec<String> = (1..=130)
        .map(|i| format!(r#"{{"formats":{i},"directory":"o{i}"}}"#))
        .collect();
    let json = format!(
        r#"{{"pack":{{"min_format":1,"max_format":130}},"overlays":{{"entries":[{}]}}}}"#,
        entries.join(",")
    );
    let meta = PackMeta::parse(json.as_bytes()).unwrap();
    let views = meta.views(Some((88, 0)));
    assert_eq!(views.len(), 130);
    assert_eq!(views.iter().filter(|view| view.verified).count(), 1);
    assert_eq!(views[87].overlays, ["o88"]);
}

/// No client has a negative format and a `Format` is unsigned, so one is refused.
#[test]
fn negative_format_is_refused() {
    assert!(PackMeta::parse(br#"{"pack":{"pack_format":-1}}"#).is_err());
    assert!(PackMeta::parse(br#"{"pack":{"min_format":[1,-2]}}"#).is_err());
}

#[test]
fn format_steps_and_parse() {
    assert_eq!(next_format((88, 0)), (88, 1));
    assert_eq!(next_format((88, ANY_MINOR)), (89, 0));
    assert_eq!(next_format((MAX_MAJOR, ANY_MINOR)), (MAX_MAJOR, ANY_MINOR));
    assert_eq!(previous_format((88, 0)), (87, ANY_MINOR));
    assert_eq!(previous_format((88, ANY_MINOR)), (88, ANY_MINOR - 1));
    assert_eq!(previous_format((0, 0)), (0, 0));
    assert_eq!(parse_format("88.0"), Some((88, 0)));
    assert_eq!(parse_format("88"), Some((88, 0)));
    assert_eq!(parse_format("unknown"), None);
    assert_eq!(show_format((88, ANY_MINOR)).to_string(), "88.*");
}

#[test]
fn read_names_the_pack_when_mcmeta_is_missing_or_unreadable() {
    let pack = |files: &[(&str, &[u8])]| crate::PackInput {
        name: "p.zip".into(),
        files: files
            .iter()
            .map(|(path, bytes)| {
                (
                    crate::PhysPath::new(path).unwrap(),
                    crate::Entry {
                        blob: (*bytes).into(),
                        digest: crate::Digest([0; 32]),
                    },
                )
            })
            .collect(),
        digest: crate::Digest([0; 32]),
    };
    let missing = PackMeta::read(&pack(&[("pack.png", b"")])).unwrap_err();
    assert_eq!(
        missing.to_string(),
        "p.zip: no pack.mcmeta at the root, not a resource pack"
    );
    let unreadable = PackMeta::read(&pack(&[("pack.mcmeta", b"{")])).unwrap_err();
    assert!(
        unreadable
            .to_string()
            .starts_with("p.zip: unreadable pack.mcmeta: ")
    );
    let meta =
        PackMeta::read(&pack(&[("pack.mcmeta", br#"{"pack":{"pack_format":15}}"#)])).unwrap();
    assert_eq!(
        meta.formats,
        Some(FormatRange {
            min: (15, 0),
            max: (15, ANY_MINOR)
        })
    );
}

#[test]
fn overlay_range_reads_an_entry_without_a_pack_container() {
    for (entry, min, max) in [
        (r#"{"formats":34}"#, (34, 0), (34, ANY_MINOR)),
        (r#"{"formats":[34,41]}"#, (34, 0), (41, ANY_MINOR)),
        (
            r#"{"formats":{"min_inclusive":"34","max_inclusive":41.9}}"#,
            (34, 0),
            (41, ANY_MINOR),
        ),
        (
            r#"{"formats":[1,2],"min_format":[34,3],"max_format":[41,2]}"#,
            (34, 3),
            (41, 2),
        ),
        (r#"{"min_format":34}"#, (34, 0), (MAX_MAJOR, ANY_MINOR)),
        (r#"{"max_format":41}"#, (0, 0), (41, ANY_MINOR)),
    ] {
        let node = json::parse(entry.as_bytes()).unwrap();
        assert_eq!(
            overlay_range(&node).unwrap(),
            FormatRange { min, max },
            "{entry}"
        );
    }
    for (entry, error) in [
        (
            r#"{"directory":"ov"}"#,
            "pack.mcmeta: overlay ov without formats",
        ),
        (r#"{"formats":false}"#, "pack.mcmeta: unreadable formats"),
        (
            r#"{"min_format":false,"formats":34}"#,
            "pack.mcmeta: unreadable format",
        ),
    ] {
        let node = json::parse(entry.as_bytes()).unwrap();
        assert_eq!(overlay_range(&node).unwrap_err(), error);
    }
}
