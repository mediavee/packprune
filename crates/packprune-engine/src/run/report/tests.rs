use std::collections::{BTreeMap, BTreeSet};

use packprune_graph::{FileEntry, Reached, Status};
use packprune_pack::{FileKind, PhysPath};
use packprune_report::cost::CostError;

use super::*;
use crate::run::tests::{Fixture, MODEL, ORPHAN, files, path as under};
use crate::run::{Artefact, CacheAt, DropIn, Request, Squashed, optimise, resolve, squash};

fn file(status: Status, bytes: u64) -> FileEntry {
    FileEntry {
        kind: FileKind::Model,
        bytes,
        status,
        root: None,
        atlases: BTreeSet::new(),
    }
}

fn path(text: &str) -> PhysPath {
    PhysPath::new(text).unwrap()
}

#[test]
fn dropped_bytes_are_grouped_by_the_status_that_dropped_them() {
    let mut reached = Reached::default();
    for (name, status, bytes) in [
        ("a", Status::Orphan, 10),
        ("b", Status::Orphan, 5),
        ("c", Status::Unused, 7),
        ("d", Status::DeadBlockstate, 3),
        ("e", Status::Reached, 100),
    ] {
        reached.files.insert(path(name), file(status, bytes));
    }
    let first: BTreeSet<PhysPath> = ["a", "b", "c", "d"].into_iter().map(path).collect();

    let lines = dropped(&first, &reached);

    let shown: Vec<(&str, usize, u64)> = lines
        .iter()
        .map(|line| (line.reason.as_str(), line.files, line.bytes))
        .collect();
    assert_eq!(
        shown,
        [
            ("dead_blockstate", 1, 3),
            ("orphan", 2, 15),
            ("unused", 1, 7)
        ]
    );
    let repruned: BTreeMap<PhysPath, u64> = [(path("x"), 4), (path("y"), 6)].into();
    let line = reprune_line(&repruned).unwrap();
    assert_eq!(
        (line.reason.as_str(), line.files, line.bytes),
        ("reprune", 2, 10)
    );
    assert!(reprune_line(&BTreeMap::new()).is_none());
}

fn budgets(zip: Option<u64>, atlas: Option<u64>) -> ConfiguredBudgets {
    ConfiguredBudgets {
        zip_bytes: zip,
        atlas_gpu_mib: atlas,
        font_mib: None,
    }
}

#[test]
fn a_memory_budget_that_cannot_be_measured_refuses_instead_of_passing() {
    let unmeasured: Result<Cost, CostError> = Err(CostError::NoClientFormat);

    let error = check_budgets(&budgets(None, Some(10)), Some(1), &unmeasured).unwrap_err();

    assert!(error.is_refusal(), "{error}");
    assert!(error.to_string().contains("atlas_gpu_mib"), "{error}");
    let (exceeded, notes) = check_budgets(&budgets(Some(10), None), Some(1), &unmeasured).unwrap();
    assert!(exceeded.is_empty() && notes.is_empty());
}

#[test]
fn a_zip_budget_on_a_directory_is_not_checked_and_says_so() {
    let measured: Result<Cost, CostError> = Err(CostError::NoClientFormat);

    let (exceeded, notes) = check_budgets(&budgets(Some(1), None), None, &measured).unwrap();

    assert!(exceeded.is_empty());
    assert!(notes[0].contains("zip_bytes"), "{notes:?}");
}

fn request(fixture: &Fixture, input: &Utf8Path, out: &str) -> Request {
    fixture.request(input, out)
}

#[test]
fn check_analyses_the_input_and_writes_nothing() {
    let fixture = Fixture::new();
    let input = fixture.pack("in", &files());
    let request = request(&fixture, &input, "out.zip");

    let checked = check(&request).unwrap();

    assert!(checked.has_findings(), "the orphan is removable");
    let report = &checked.bundle.analysis.report;
    assert_eq!(report.removable, [ORPHAN]);
    assert_eq!(report.summary.orphans, 1);
    assert!(!request.output.exists());
    let cost = checked.bundle.run.unwrap().cost.unwrap();
    assert!(cost.input.is_some() && cost.output.is_none());
}

#[test]
fn check_exceeds_a_zip_budget_by_the_size_of_the_input_zip() {
    let fixture = Fixture::new();
    let input = fixture.pack("in", &files());
    let zip = under(&fixture.dir, "in.zip");
    let optimised = optimise(&request(&fixture, &input, "in.zip")).unwrap();
    let size = optimised.written.bytes;
    let mut request = request(&fixture, &zip, "unused.zip");

    request.budgets = budgets(Some(size), None);
    assert!(check(&request).unwrap().exceeded.is_empty());
    request.budgets = budgets(Some(size - 1), None);
    let exceeded = check(&request).unwrap().exceeded;
    assert_eq!(
        (exceeded[0].budget, exceeded[0].limit, exceeded[0].measured),
        ("zip_bytes", size - 1, size)
    );
}

#[test]
fn the_summary_holds_what_the_report_files_list() {
    let fixture = Fixture::new();
    let input = fixture.pack("in", &files());
    let request = request(&fixture, &input, "out.zip");

    let summary = optimise(&request).unwrap();
    let bundle = summary.bundle();

    assert_eq!(bundle.analysis.report.removable, [ORPHAN]);
    let run = bundle.run.unwrap();
    let orphan = run
        .dropped
        .iter()
        .find(|line| line.reason == "orphan")
        .unwrap();
    assert_eq!(
        (orphan.files, orphan.bytes),
        (
            1,
            files().iter().find(|(p, _)| *p == ORPHAN).unwrap().1.len() as u64
        )
    );
    assert_eq!(run.output.unwrap().zip_bytes, summary.written.bytes);
    assert!(run.on_zip.is_none() || run.on_zip.unwrap().differing_keys == 0);
    let passes = bundle.passes.expect("passes ran");
    assert!(passes.oracle.is_some());
    assert!(passes.passes.iter().any(|pass| pass.pass == "drop-unread"));
    assert!(
        report_dir(&summary.output)
            .as_str()
            .ends_with("out.zip.report")
    );
}

fn drop_in(fixture: &Fixture, input: &Utf8Path, output: &str, reports: &Utf8Path) -> DropIn {
    DropIn {
        input: input.to_owned(),
        output: under(&fixture.dir, output),
        profile: Default::default(),
        limits: Default::default(),
        quirks: Default::default(),
        reports: Some(reports.to_owned()),
    }
}

/// Runs the drop-in for `output`, a zip in the `cache` directory, reporting under `reports`.
fn squash_into(fixture: &Fixture, input: &Utf8Path, output: &str, reports: &Utf8Path) -> Squashed {
    let request = fixture.request(input, output);
    squash(&drop_in(fixture, input, output, reports), Ok(request)).unwrap()
}

/// The warnings about the report files themselves, apart from the analysis's notes.
fn warnings(squashed: &Squashed) -> Vec<&String> {
    let all = &squashed.summary.as_ref().unwrap().warnings;
    all.iter()
        .filter(|line| line.starts_with("report not written"))
        .collect()
}

#[test]
fn optimise_keeps_the_old_zip_when_its_report_cannot_be_written() {
    let fixture = Fixture::new();
    let input = fixture.pack("in", &files());
    let request = request(&fixture, &input, "out.zip");
    std::fs::write(&request.output, b"the old zip").unwrap();
    std::fs::write(report_dir(&request.output), b"in the way").unwrap();

    let error = optimise(&request).unwrap_err();

    assert!(error.to_string().contains("out.zip.report"), "{error}");
    assert_eq!(std::fs::read(&request.output).unwrap(), b"the old zip");
    let leftovers: Vec<_> = std::fs::read_dir(fixture.dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.starts_with(".packprune-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn optimise_puts_the_zip_and_its_report_in_place_together() {
    let fixture = Fixture::new();
    let input = fixture.pack("in", &files());
    let request = request(&fixture, &input, "out.zip");

    optimise(&request).unwrap();

    let dir = report_dir(&request.output);
    for file in ["report.json", "report.md", "graph.json", "run.json"] {
        assert!(dir.join(file).is_file(), "{file}");
    }
    let names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(
        names.iter().all(|name| !name.ends_with(".partial")),
        "{names:?}"
    );
}

#[test]
fn the_drop_in_writes_its_report_to_latest_and_none_for_the_original() {
    let fixture = Fixture::new();
    let input = fixture.pack("in", &files());
    let reports = under(&fixture.dir, "reports");

    let original = squash(
        &drop_in(&fixture, &input, "plain.zip", &reports),
        Err("no config".into()),
    )
    .unwrap();
    assert_eq!(original.artefact, Artefact::Original);
    assert!(!reports.exists());

    let squashed = squash_into(&fixture, &input, "out.zip", &reports);

    assert_eq!(squashed.artefact, Artefact::Optimised);
    assert!(warnings(&squashed).is_empty(), "{:?}", warnings(&squashed));
    assert!(reports.join("latest/report.json").is_file());
    assert!(reports.join("latest/run.json").is_file());
}

#[test]
fn two_drop_in_runs_leave_one_report_the_latest() {
    let fixture = Fixture::new();
    let input = fixture.pack("in", &files());
    let reports = under(&fixture.dir, "reports");
    std::fs::create_dir(under(&fixture.dir, "cache")).unwrap();
    squash_into(&fixture, &input, "cache/cache-a.zip", &reports);

    let second = squash_into(&fixture, &input, "cache/cache-b.zip", &reports);

    let names: Vec<_> = std::fs::read_dir(&reports)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, ["latest"]);
    let sha1: String = second
        .written
        .sha1
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let run = std::fs::read_to_string(reports.join("latest/run.json")).unwrap();
    assert!(run.contains(&sha1), "the report is the second run's");
}

#[test]
fn the_drop_in_ships_the_pack_and_warns_when_its_report_cannot_be_written() {
    let fixture = Fixture::new();
    let input = fixture.pack("in", &files());
    let reports = under(&fixture.dir, "reports");
    std::fs::write(&reports, b"a file where the directory goes").unwrap();

    let squashed = squash_into(&fixture, &input, "out.zip", &reports);

    assert!(under(&fixture.dir, "out.zip").is_file());
    assert!(
        warnings(&squashed)
            .iter()
            .any(|line| line.starts_with("report not written")),
        "{:?}",
        warnings(&squashed)
    );
}

#[test]
fn the_drop_in_does_not_write_its_reports_into_the_pack() {
    let fixture = Fixture::new();
    let input = fixture.pack("in", &files());
    let reports = input.join("reports");

    let squashed = squash_into(&fixture, &input, "out.zip", &reports);

    assert!(!reports.exists());
    assert!(
        warnings(&squashed)
            .iter()
            .any(|line| line.contains("is inside the input")),
        "{:?}",
        warnings(&squashed)
    );
}

#[test]
fn the_drop_in_refuses_a_report_folder_that_is_or_holds_the_pack() {
    let fixture = Fixture::new();
    let input = fixture.pack("reports/latest", &files());
    let reports = under(&fixture.dir, "reports");
    let before = kept(&input);

    let squashed = squash_into(&fixture, &input, "out.zip", &reports);

    assert_eq!(kept(&input), before);
    assert!(
        warnings(&squashed)
            .iter()
            .any(|line| line.contains("is the input")),
        "{:?}",
        warnings(&squashed)
    );
    assert!(under(&fixture.dir, "out.zip").is_file());
}

#[cfg(unix)]
#[test]
fn the_drop_in_refuses_a_report_folder_that_is_a_link() {
    let fixture = Fixture::new();
    let input = fixture.pack("in", &files());
    let reports = under(&fixture.dir, "reports");
    let elsewhere = under(&fixture.dir, "elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("mine.txt"), "mine").unwrap();
    std::fs::create_dir(&reports).unwrap();
    std::os::unix::fs::symlink(&elsewhere, reports.join("latest")).unwrap();

    let squashed = squash_into(&fixture, &input, "out.zip", &reports);

    assert_eq!(kept(&elsewhere), ["mine.txt"]);
    assert!(
        warnings(&squashed)
            .iter()
            .any(|line| line.contains("is a link")),
        "{:?}",
        warnings(&squashed)
    );
    assert!(under(&fixture.dir, "out.zip").is_file());
}

#[cfg(unix)]
#[test]
fn the_drop_in_replaces_a_link_at_a_report_file_without_touching_what_it_led_to() {
    let fixture = Fixture::new();
    let input = fixture.pack("in", &files());
    let reports = under(&fixture.dir, "reports");
    let latest = reports.join("latest");
    std::fs::create_dir_all(&latest).unwrap();
    let outside = under(&fixture.dir, "operator-file");
    std::fs::write(&outside, "irreplaceable operator file").unwrap();
    std::os::unix::fs::symlink(&outside, latest.join("report.json")).unwrap();
    std::os::unix::fs::symlink(input.join(MODEL), latest.join("run.json")).unwrap();
    std::fs::write(latest.join("operator-notes.txt"), "mine").unwrap();
    let before = kept(&input);

    let squashed = squash_into(&fixture, &input, "out.zip", &reports);

    assert!(warnings(&squashed).is_empty(), "{:?}", warnings(&squashed));
    assert_eq!(
        std::fs::read_to_string(&outside).unwrap(),
        "irreplaceable operator file"
    );
    assert_eq!(kept(&input), before);
    assert_eq!(
        std::fs::read_to_string(input.join(MODEL)).unwrap(),
        files().iter().find(|(p, _)| *p == MODEL).unwrap().1
    );
    for name in ["report.json", "run.json"] {
        let kind = std::fs::symlink_metadata(latest.join(name))
            .unwrap()
            .file_type();
        assert!(kind.is_file(), "{name}");
    }
    assert_eq!(
        std::fs::read_to_string(latest.join("operator-notes.txt")).unwrap(),
        "mine"
    );
}

#[test]
fn the_drop_in_never_deletes_what_is_around_its_report() {
    let fixture = Fixture::new();
    let mut with_pack_reports = files();
    with_pack_reports.extend([("report.json", "{}"), ("source.txt", "mine")]);
    let input = fixture.pack("server/reports/source-pack", &with_pack_reports);
    let reports = under(&fixture.dir, "server/reports");
    // Folders that look like old reports: a foreign one, a copied marker, a folder with a name a run uses.
    for name in ["foreign", "cache-a", "out"] {
        std::fs::create_dir_all(reports.join(name)).unwrap();
        std::fs::write(reports.join(name).join("report.json"), "{}").unwrap();
        std::fs::write(reports.join(name).join("source.txt"), "mine").unwrap();
        std::fs::write(
            reports.join(name).join(".packprune-report"),
            "packprune report\nname=cache-a\n",
        )
        .unwrap();
    }
    let before = kept(&reports);

    squash_into(&fixture, &input, "out.zip", &reports);

    let after = kept(&reports);
    assert!(before.iter().all(|file| after.contains(file)), "{after:?}");
    assert!(reports.join("latest/report.json").is_file());
}

#[test]
fn check_adds_nothing_to_the_cache() {
    let fixture = Fixture::new();
    let input = fixture.pack("in", &files());
    let cache = under(&fixture.dir, "cache");
    let mut request = request(&fixture, &input, "out.zip");
    request.cache = Some(CacheAt {
        dir: cache.clone(),
        max_bytes: 1 << 30,
    });

    check(&request).unwrap();

    assert!(!cache.exists(), "check wrote cache entries");
    optimise(&request).unwrap();
    assert!(cache.exists(), "the same cache is written by optimise");
}

#[cfg(unix)]
#[test]
fn resolve_follows_a_link_at_any_component_and_keeps_what_does_not_exist() {
    let fixture = Fixture::new();
    let real = under(&fixture.dir, "real");
    std::fs::create_dir(&real).unwrap();
    let link = under(&fixture.dir, "link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let real = real.canonicalize_utf8().unwrap();

    assert_eq!(resolve(&link).unwrap(), real);
    assert_eq!(
        resolve(&link.join("new/dir")).unwrap(),
        real.join("new/dir")
    );
    assert_eq!(
        resolve(&link.join("sub/../other")).unwrap(),
        real.join("other")
    );
}

fn kept(dir: &Utf8Path) -> Vec<String> {
    let mut names: Vec<String> = walk(dir.as_std_path(), dir.as_std_path());
    names.sort();
    names
}

fn walk(root: &std::path::Path, dir: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .flat_map(|entry| {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path)
            } else {
                vec![path.strip_prefix(root).unwrap().display().to_string()]
            }
        })
        .collect()
}

#[cfg(unix)]
#[test]
fn a_report_root_that_is_a_link_into_the_pack_is_refused() {
    let fixture = Fixture::new();
    let mut with_reports = files();
    with_reports.push(("reports/keep.txt", "mine"));
    let input = fixture.pack("in", &with_reports);
    let reports = under(&fixture.dir, "server/reports");
    std::fs::create_dir_all(reports.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(input.join("reports"), &reports).unwrap();
    let before = kept(&input);

    let squashed = squash_into(&fixture, &input, "out.zip", &reports);

    assert_eq!(kept(&input), before);
    assert!(
        warnings(&squashed)
            .iter()
            .any(|line| line.contains("is inside the input")),
        "{:?}",
        warnings(&squashed)
    );
}
