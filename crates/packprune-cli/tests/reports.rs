//! `check`, `report`, the report files of `optimise`, budgets and `-v`, through the `packprune` binary on the fixtures'
//! client (`tests/fixtures/vanilla/`, zipped into a jar as the engine's tests do).

#![expect(
    clippy::unwrap_used,
    reason = "fixture helpers: a failed setup step fails the test"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const META: &str = r#"{"pack": {"min_format": 88, "max_format": 88, "description": "t"}}"#;
const ITEM: &str = r#"{ "model": { "type": "minecraft:model", "model": "fx:item/box" } }"#;
const MODEL: &str = r##"{ "credit": "made by hand", "textures": { "particle": "minecraft:block/stone" }, "elements": [ { "from": [ 0.0, 0, 0 ], "to": [ 16, 16, 16 ], "faces": { "north": { "texture": "#particle" } } } ] }"##;
const ORPHAN: &str = r#"{ "textures": { "particle": "minecraft:block/stone" } }"#;

const CLEAN: &[(&str, &str)] = &[
    ("pack.mcmeta", META),
    ("assets/fx/items/box.json", ITEM),
    ("assets/fx/models/item/box.json", MODEL),
];

struct Work {
    dir: tempfile::TempDir,
}

impl Work {
    fn new() -> Self {
        let work = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        let mut jar = Vec::new();
        collect(
            &repo().join("tests/fixtures/vanilla"),
            Path::new(""),
            &mut jar,
        );
        fs::write(work.path("client.jar"), stored_zip(&jar)).unwrap();
        work
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// A pack directory holding `files`.
    fn pack(&self, name: &str, files: &[(&str, &str)]) -> PathBuf {
        for (file, text) in files {
            let target = self.path(name).join(file);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, text).unwrap();
        }
        self.path(name)
    }

    /// The same files as a zip.
    fn pack_zip(&self, name: &str, files: &[(&str, &str)]) -> PathBuf {
        let entries: Vec<(String, Vec<u8>)> = files
            .iter()
            .map(|(file, text)| ((*file).to_owned(), text.as_bytes().to_vec()))
            .collect();
        fs::write(self.path(name), stored_zip(&entries)).unwrap();
        self.path(name)
    }

    fn config(&self, name: &str, budgets: &str) -> PathBuf {
        let path = self.path(name);
        fs::write(&path, format!("version = 2\n[budgets]\n{budgets}\n")).unwrap();
        path
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_packprune"))
            .args(args)
            .arg("--client-jar")
            .arg(self.path("client.jar"))
            .env("HOME", self.path("home"))
            .env_remove("PACKPRUNE_LOG")
            .env_remove("PACKPRUNE_CACHE")
            .output()
            .unwrap()
    }
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn collect(root: &Path, relative: &Path, out: &mut Vec<(String, Vec<u8>)>) {
    let mut entries: Vec<_> = fs::read_dir(root.join(relative))
        .unwrap()
        .map(|entry| entry.unwrap())
        .collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = relative.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            collect(root, &path, out);
        } else {
            let name = path.to_str().unwrap().replace('\\', "/");
            out.push((name, fs::read(root.join(&path)).unwrap()));
        }
    }
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// A zip of stored entries: the writer under test is not needed to make an input.
fn stored_zip(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut zip = Vec::new();
    let mut directory = Vec::new();
    for (name, data) in entries {
        let offset = zip.len() as u32;
        let (crc, size, length) = (crc32(data), data.len() as u32, name.len() as u16);
        zip.extend(0x0403_4b50_u32.to_le_bytes());
        zip.extend([20, 0, 0, 0, 0, 0, 0, 0, 0x21, 0]);
        zip.extend(crc.to_le_bytes());
        zip.extend(size.to_le_bytes().repeat(2));
        zip.extend(length.to_le_bytes());
        zip.extend([0, 0]);
        zip.extend(name.as_bytes());
        zip.extend(data);

        directory.extend(0x0201_4b50_u32.to_le_bytes());
        directory.extend([20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0x21, 0]);
        directory.extend(crc.to_le_bytes());
        directory.extend(size.to_le_bytes().repeat(2));
        directory.extend(length.to_le_bytes());
        directory.extend([0; 12]);
        directory.extend(offset.to_le_bytes());
        directory.extend(name.as_bytes());
    }
    let start = zip.len() as u32;
    zip.extend(&directory);
    zip.extend(0x0605_4b50_u32.to_le_bytes());
    zip.extend([0; 4]);
    zip.extend((entries.len() as u16).to_le_bytes().repeat(2));
    zip.extend((directory.len() as u32).to_le_bytes());
    zip.extend(start.to_le_bytes());
    zip.extend([0, 0]);
    zip
}

fn code(output: &Output) -> i32 {
    output.status.code().expect("exited normally")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn read(path: &Path) -> String {
    text(&fs::read(path).unwrap())
}

#[test]
fn check_exits_1_over_a_budget_and_0_within_it() {
    let work = Work::new();
    let pack = work.pack_zip("pack.zip", CLEAN);
    let size = fs::metadata(&pack).unwrap().len();
    let pack = pack.to_str().unwrap();

    let within = work.config("within.toml", &format!("zip_bytes = {size}"));
    let over = work.config("over.toml", &format!("zip_bytes = {}", size - 1));

    let ok = work.run(&["check", pack, "--config", within.to_str().unwrap()]);
    assert_eq!(code(&ok), 0, "{}{}", text(&ok.stdout), text(&ok.stderr));
    let exceeded = work.run(&["check", pack, "--config", over.to_str().unwrap()]);
    assert_eq!(code(&exceeded), 1, "{}", text(&exceeded.stderr));
    assert!(
        text(&exceeded.stdout)
            .contains(&format!("budget zip_bytes exceeded: {size} > {}", size - 1)),
        "{}",
        text(&exceeded.stdout)
    );
}

#[test]
fn check_exits_1_over_a_client_memory_budget_and_0_within_it() {
    let work = Work::new();
    let pack = work.pack("pack", CLEAN);
    let pack = pack.to_str().unwrap();

    let within = work.config("within.toml", "atlas_gpu_mib = 1024\nfont_mib = 1024");
    let over = work.config("over.toml", "atlas_gpu_mib = 0");

    let ok = work.run(&["check", pack, "--config", within.to_str().unwrap()]);
    assert_eq!(code(&ok), 0, "{}{}", text(&ok.stdout), text(&ok.stderr));
    let exceeded = work.run(&["check", pack, "--config", over.to_str().unwrap()]);
    assert_eq!(
        code(&exceeded),
        1,
        "{}{}",
        text(&exceeded.stdout),
        text(&exceeded.stderr)
    );
    assert!(text(&exceeded.stdout).contains("budget atlas_gpu_mib exceeded"));
}

#[test]
fn check_lists_findings_exits_1_and_writes_nothing() {
    let work = Work::new();
    let pack = work.pack(
        "pack",
        &[CLEAN, &[("assets/fx/models/item/orphan.json", ORPHAN)]].concat(),
    );

    let output = work.run(&["check", pack.to_str().unwrap()]);

    assert_eq!(code(&output), 1, "{}", text(&output.stderr));
    assert!(text(&output.stdout).contains("orphan: assets/fx/models/item/orphan.json"));
    let names: Vec<_> = fs::read_dir(work.dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(names.iter().all(|name| name != "pack.report"), "{names:?}");
}

#[test]
fn optimise_exits_1_over_a_budget_but_still_ships_the_zip() {
    let work = Work::new();
    let pack = work.pack("pack", CLEAN);
    let pack = pack.to_str().unwrap();
    let first = work.path("first.zip");
    let ok = work.run(&["optimise", pack, "-o", first.to_str().unwrap()]);
    assert_eq!(code(&ok), 0, "{}", text(&ok.stderr));
    let size = fs::metadata(&first).unwrap().len();

    let within = work.config("within.toml", &format!("zip_bytes = {size}"));
    let out = work.path("within.zip");
    let at_limit = work.run(&[
        "optimise",
        pack,
        "-o",
        out.to_str().unwrap(),
        "--config",
        within.to_str().unwrap(),
    ]);
    assert_eq!(code(&at_limit), 0, "{}", text(&at_limit.stderr));

    let over = work.config("over.toml", &format!("zip_bytes = {}", size - 1));
    let out = work.path("over.zip");
    let exceeded = work.run(&[
        "optimise",
        pack,
        "-o",
        out.to_str().unwrap(),
        "--config",
        over.to_str().unwrap(),
    ]);
    assert_eq!(code(&exceeded), 1, "{}", text(&exceeded.stderr));
    assert!(out.is_file(), "a budget exceeded ships the artefact");
    assert!(text(&exceeded.stdout).contains("budget zip_bytes exceeded"));
}

#[test]
fn optimise_writes_the_report_files_beside_the_zip() {
    let work = Work::new();
    let pack = work.pack(
        "pack",
        &[
            CLEAN,
            &[
                ("assets/fx/models/item/orphan.json", ORPHAN),
                ("README.txt", "x"),
            ],
        ]
        .concat(),
    );
    let out = work.path("out.zip");

    let output = work.run(&[
        "optimise",
        pack.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);

    assert_eq!(code(&output), 0, "{}", text(&output.stderr));
    let report = work.path("out.zip.report");
    for file in [
        "report.json",
        "report.md",
        "graph.json",
        "passes.json",
        "run.json",
    ] {
        assert!(report.join(file).is_file(), "{file}");
    }
    let analysis = read(&report.join("report.json"));
    assert!(analysis.contains("\"schema\": 4"), "{analysis}");
    assert!(
        analysis.contains("\"removable\": [\n    \"assets/fx/models/item/orphan.json\"\n  ]"),
        "{analysis}"
    );
    let run = read(&report.join("run.json"));
    let orphan = format!(
        "\"reason\": \"orphan\",\n      \"files\": 1,\n      \"bytes\": {}",
        ORPHAN.len()
    );
    assert!(run.contains(&orphan), "{run}");
    let zip_bytes = format!("\"zipBytes\": {}", fs::metadata(&out).unwrap().len());
    assert!(run.contains(&zip_bytes), "{run}");
    assert_eq!(
        run.matches("\"atlases\": [").count(),
        2,
        "input and output cost"
    );
    assert!(read(&report.join("report.md")).contains("## Client memory"));
}

#[test]
fn report_writes_the_files_and_renders_them_again() {
    let work = Work::new();
    let pack = work.pack("pack", CLEAN);
    let out = work.path("reports");

    let output = work.run(&[
        "report",
        pack.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);

    assert_eq!(
        code(&output),
        0,
        "{}{}",
        text(&output.stdout),
        text(&output.stderr)
    );
    for file in ["report.json", "report.md", "graph.json", "run.json"] {
        assert!(out.join(file).is_file(), "{file}");
    }
    assert_eq!(read(&out.join("passes.json")), "{\n  \"passes\": [ ]\n}\n");
    let rendered = work.path("rendered");
    let again = work.run(&[
        "report",
        out.to_str().unwrap(),
        "--out",
        rendered.to_str().unwrap(),
    ]);
    assert_eq!(code(&again), 0, "{}", text(&again.stderr));
    let first = read(&out.join("report.md"));
    assert!(first.contains("## Client memory"), "{first}");
    assert_eq!(read(&rendered.join("report.md")), first);
}

#[test]
fn rendering_the_report_of_an_optimise_run_keeps_the_sections_of_the_run() {
    let work = Work::new();
    let pack = work.pack(
        "pack",
        &[CLEAN, &[("assets/fx/models/item/orphan.json", ORPHAN)]].concat(),
    );
    let out = work.path("out.zip");
    let optimised = work.run(&[
        "optimise",
        pack.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert_eq!(code(&optimised), 0, "{}", text(&optimised.stderr));
    let report = work.path("out.zip.report");
    let original = read(&report.join("report.md"));
    for section in ["## Client memory", "## Run", "### Dropped", "### Passes"] {
        assert!(original.contains(section), "{section}");
    }

    let in_place = work.run(&[
        "report",
        report.to_str().unwrap(),
        "--out",
        report.to_str().unwrap(),
    ]);

    assert_eq!(code(&in_place), 0, "{}", text(&in_place.stderr));
    assert_eq!(read(&report.join("report.md")), original);
}

#[test]
fn report_refuses_an_out_inside_the_pack() {
    let work = Work::new();
    let pack = work.pack("pack", CLEAN);
    let inside = pack.join("reports");

    let output = work.run(&[
        "report",
        pack.to_str().unwrap(),
        "--out",
        inside.to_str().unwrap(),
    ]);

    assert_eq!(code(&output), 2, "{}", text(&output.stderr));
    assert!(!inside.exists());
}

#[cfg(unix)]
#[test]
fn report_refuses_an_out_that_is_a_link_to_the_pack() {
    let work = Work::new();
    let pack = work.pack("pack", CLEAN);
    let link = work.path("linked-out");
    std::os::unix::fs::symlink(&pack, &link).unwrap();
    let before = fs::read_dir(&pack).unwrap().count();

    let output = work.run(&[
        "report",
        pack.to_str().unwrap(),
        "--out",
        link.to_str().unwrap(),
    ]);

    assert_eq!(code(&output), 2, "{}", text(&output.stderr));
    assert_eq!(fs::read_dir(&pack).unwrap().count(), before);
}

#[test]
fn optimise_keeps_the_old_zip_when_its_report_cannot_be_written() {
    let work = Work::new();
    let pack = work.pack("pack", CLEAN);
    let out = work.path("out.zip");
    fs::write(&out, "the old zip").unwrap();
    fs::write(work.path("out.zip.report"), "in the way").unwrap();

    let output = work.run(&[
        "optimise",
        pack.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);

    assert_eq!(code(&output), 3, "{}", text(&output.stderr));
    assert_eq!(read(&out), "the old zip");
}

#[test]
fn check_leaves_no_cache_behind() {
    let work = Work::new();
    let pack = work.pack("pack", CLEAN);
    let config = work.path("packprune.toml");
    fs::write(&config, "version = 2\n[cache]\ndir = 'cache'\n").unwrap();

    let output = work.run(&[
        "check",
        pack.to_str().unwrap(),
        "--config",
        config.to_str().unwrap(),
    ]);

    assert_eq!(code(&output), 0, "{}", text(&output.stderr));
    assert!(!work.path("cache").exists());
}

/// The drop-in as Nexo runs it: the settings file as the only argument, the config beside it by `PACKPRUNE_CONFIG`.
fn drop_in(work: &Work, config: &str) -> Output {
    drop_in_of(work, "pack", config)
}

/// The same with the pack in the directory `pack` of the work directory.
fn drop_in_of(work: &Work, pack: &str, config: &str) -> Output {
    let pack = work.pack(pack, CLEAN);
    let config_path = work.path("server/packprune.toml");
    fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    fs::write(
        &config_path,
        format!(
            "version = 2\n[vanilla]\nclient_jar = '{}'\n{config}",
            work.path("client.jar").display()
        ),
    )
    .unwrap();
    let settings = work.path("packsquash.toml");
    fs::write(
        &settings,
        format!(
            "pack_directory = '{}'\noutput_file_path = '{}'\n",
            pack.display(),
            work.path("cache-a.zip").display()
        ),
    )
    .unwrap();
    Command::new(env!("CARGO_BIN_EXE_packprune"))
        .arg(&settings)
        .env("PACKPRUNE_CONFIG", &config_path)
        .env("HOME", work.path("home"))
        .env_remove("PACKPRUNE_LOG")
        .env_remove("PACKPRUNE_CACHE")
        .output()
        .unwrap()
}

#[test]
fn the_drop_in_keeps_its_reports_in_reports_beside_the_config() {
    let work = Work::new();

    let output = drop_in(&work, "");

    assert_eq!(code(&output), 0, "{}", text(&output.stdout));
    assert!(work.path("cache-a.zip").is_file());
    let report = work.path("server/reports/latest");
    assert!(report.join("report.json").is_file());
    assert!(report.join("run.json").is_file());
}

#[test]
fn the_drop_in_reports_where_the_config_says() {
    let work = Work::new();

    let output = drop_in(&work, "[report]\ndir = 'plugins/reports'\n");

    assert_eq!(code(&output), 0, "{}", text(&output.stdout));
    assert!(
        work.path("server/plugins/reports/latest/report.json")
            .is_file()
    );
    assert!(!work.path("server/reports").exists());
}

#[test]
fn a_report_the_drop_in_cannot_write_is_a_star_line_and_the_pack_still_ships() {
    let work = Work::new();
    fs::create_dir_all(work.path("server")).unwrap();
    fs::write(
        work.path("server/reports"),
        "a file where the directory goes",
    )
    .unwrap();

    let output = drop_in(&work, "");

    assert_eq!(code(&output), 0, "{}", text(&output.stdout));
    assert!(work.path("cache-a.zip").is_file());
    let stdout = text(&output.stdout);
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("* packprune: report not written")),
        "{stdout}"
    );
    assert_eq!(text(&output.stderr), "");
}

#[test]
fn a_report_directory_inside_the_pack_costs_the_pack_nothing() {
    let work = Work::new();
    work.pack(
        "pack",
        &[
            ("reports/old-build/report.json", "{}"),
            ("reports/old-build/source.txt", "mine"),
        ],
    );

    let output = drop_in(&work, "[report]\ndir = '../pack/reports'\n");

    assert_eq!(code(&output), 0, "{}", text(&output.stdout));
    assert_eq!(
        read(&work.path("pack/reports/old-build/source.txt")),
        "mine"
    );
    assert!(!work.path("pack/reports/latest").exists());
    let stdout = text(&output.stdout);
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("* packprune: report not written")),
        "{stdout}"
    );
}

#[test]
fn a_pack_in_the_report_directory_is_left_alone() {
    let work = Work::new();
    let sources = [
        ("report.json", "{}"),
        ("source.txt", "mine"),
        (".packprune-report", "packprune report\nname=source-pack\n"),
    ];
    work.pack("server/reports/source-pack", &sources);

    let output = drop_in_of(&work, "server/reports/source-pack", "");

    assert_eq!(code(&output), 0, "{}", text(&output.stdout));
    for (file, content) in sources {
        let kept = read(&work.path("server/reports/source-pack").join(file));
        assert_eq!(kept, content, "{file}");
    }
    assert!(work.path("server/reports/latest/report.json").is_file());
}

#[cfg(unix)]
#[test]
fn a_link_at_a_report_file_keeps_its_target_in_every_command() {
    let work = Work::new();
    let target = work.path("operator-file");
    fs::write(&target, "irreplaceable").unwrap();
    let report = work.path("server/reports/latest");
    fs::create_dir_all(&report).unwrap();
    std::os::unix::fs::symlink(&target, report.join("report.json")).unwrap();
    let drop = drop_in(&work, "");
    assert_eq!(code(&drop), 0, "{}", text(&drop.stdout));
    assert_eq!(read(&target), "irreplaceable");
    assert!(read(&report.join("report.json")).contains("\"schema\": 4"));

    // `optimise` and `report --out`, the same rule.
    let pack = work.path("pack");
    let out = work.path("o.zip");
    let linked = work.path("o.zip.report");
    fs::create_dir(&linked).unwrap();
    std::os::unix::fs::symlink(&target, linked.join("report.md")).unwrap();
    let output = work.run(&[
        "optimise",
        pack.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert_eq!(code(&output), 0, "{}", text(&output.stderr));
    assert_eq!(read(&target), "irreplaceable");
    let rendered = work.path("rendered");
    fs::create_dir(&rendered).unwrap();
    std::os::unix::fs::symlink(&target, rendered.join("report.md")).unwrap();
    let again = work.run(&[
        "report",
        linked.to_str().unwrap(),
        "--out",
        rendered.to_str().unwrap(),
    ]);
    assert_eq!(code(&again), 0, "{}", text(&again.stderr));
    assert_eq!(read(&target), "irreplaceable");
    assert!(read(&rendered.join("report.md")).contains("## Run"));
}

const OPERATOR: &str = "irreplaceable operator data, packprune never wrote this file";
const NO_PASSES: &str = "[profiles.default]\npasses = 'none'\n";

#[test]
fn a_run_without_passes_replaces_a_passes_json_it_finds_in_the_drop_in_report() {
    let work = Work::new();
    let latest = work.path("server/reports/latest");
    fs::create_dir_all(&latest).unwrap();
    fs::write(latest.join("passes.json"), OPERATOR).unwrap();

    let output = drop_in(&work, NO_PASSES);

    assert_eq!(code(&output), 0, "{}", text(&output.stdout));
    assert_eq!(
        read(&latest.join("passes.json")),
        "{\n  \"passes\": [ ]\n}\n"
    );
}

#[test]
fn a_run_without_passes_replaces_a_passes_json_it_finds_beside_optimise_and_report() {
    let work = Work::new();
    let pack = work.pack("pack", CLEAN);
    let config = work.path("packprune.toml");
    fs::write(&config, format!("version = 2\n{NO_PASSES}")).unwrap();
    let out = work.path("out.zip");
    let report = work.path("out.zip.report");
    fs::create_dir(&report).unwrap();
    fs::write(report.join("passes.json"), OPERATOR).unwrap();

    let optimised = work.run(&[
        "optimise",
        pack.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--config",
        config.to_str().unwrap(),
    ]);
    assert_eq!(code(&optimised), 0, "{}", text(&optimised.stderr));
    assert_eq!(
        read(&report.join("passes.json")),
        "{\n  \"passes\": [ ]\n}\n"
    );

    let dest = work.path("destination");
    fs::create_dir(&dest).unwrap();
    fs::write(dest.join("passes.json"), OPERATOR).unwrap();
    let reported = work.run(&[
        "report",
        pack.to_str().unwrap(),
        "--out",
        dest.to_str().unwrap(),
        "--config",
        config.to_str().unwrap(),
    ]);
    assert_eq!(code(&reported), 0, "{}", text(&reported.stderr));
    assert_eq!(read(&dest.join("passes.json")), "{\n  \"passes\": [ ]\n}\n");
}

#[test]
fn verbose_prints_every_stage_with_its_wall_time() {
    let work = Work::new();
    let pack = work.pack("pack", CLEAN);
    let out = work.path("out.zip");

    let quiet = work.run(&[
        "optimise",
        pack.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    let output = work.run(&[
        "-v",
        "optimise",
        pack.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);

    assert_eq!(code(&output), 0, "{}", text(&output.stderr));
    assert!(
        !text(&quiet.stderr).contains("elapsed"),
        "{}",
        text(&quiet.stderr)
    );
    let log = text(&output.stderr);
    for stage in [
        "read",
        "vanilla",
        "sources",
        "analyse",
        "report",
        "cost (input)",
        "prune",
        "lossless passes",
        "file optimisers",
        "write",
        "self-check",
        "cost (output)",
        "write report",
    ] {
        let line = log
            .lines()
            .find(|line| line.contains(&format!("step=\"{stage}\"")))
            .unwrap_or_else(|| panic!("no wall time for {stage}:\n{log}"));
        assert!(line.contains("elapsed"), "{line}");
    }
}

const WITH_ORPHAN: &[(&str, &str)] = &[
    ("pack.mcmeta", META),
    ("assets/fx/items/box.json", ITEM),
    ("assets/fx/models/item/box.json", MODEL),
    ("assets/fx/models/item/orphan.json", ORPHAN),
    ("README.txt", "x"),
];

const ORPHAN_PATH: &str = "assets/fx/models/item/orphan.json";

const KEEP: &str = "version: 1\nkeep:\n  - texture: \"fx:nothing/*\"\n    why: \"stale\"\n";

impl Work {
    /// The `report` an operator reviews before applying it: `report.json` of `pack` read against `extra` inputs.
    fn reviewed(&self, pack: &Path, extra: &[&str]) -> PathBuf {
        let dir = self.path("reviewed");
        let mut args = vec![
            "report",
            pack.to_str().unwrap(),
            "--out",
            dir.to_str().unwrap(),
        ];
        args.extend(extra);
        let output = self.run(&args);
        assert!(code(&output) <= 1, "{}", text(&output.stderr));
        dir.join("report.json")
    }

    fn optimise_expecting(&self, pack: &Path, report: &Path, out: &Path, extra: &[&str]) -> Output {
        let mut args = vec![
            "optimise",
            pack.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
            "--expect-report",
            report.to_str().unwrap(),
        ];
        args.extend(extra);
        self.run(&args)
    }
}

/// Exit 2, `needles` named on stderr, and nothing written beside `out`.
fn assert_refused(output: &Output, out: &Path, needles: &[&str]) {
    let stderr = text(&output.stderr);
    assert_eq!(code(output), 2, "{}{stderr}", text(&output.stdout));
    for needle in needles {
        assert!(stderr.contains(needle), "{needle}:\n{stderr}");
    }
    assert!(!out.exists(), "a refused run writes no zip");
    assert!(
        !out.with_extension("zip.report").exists(),
        "a refused run writes no report"
    );
}

#[test]
fn expect_report_runs_when_the_report_is_of_the_same_inputs() {
    let work = Work::new();
    let pack = work.pack("pack", WITH_ORPHAN);
    let keep = work.path("keep.yml");
    fs::write(&keep, KEEP).unwrap();
    let keep = keep.to_str().unwrap();
    let report = work.reviewed(&pack, &["--keep", keep]);
    let out = work.path("out.zip");

    let output = work.optimise_expecting(&pack, &report, &out, &["--keep", keep]);

    assert_eq!(code(&output), 0, "{}", text(&output.stderr));
    assert!(out.is_file());
    assert!(text(&output.stdout).contains("dropped 1 files"));
}

#[test]
fn expect_report_refuses_a_pack_file_that_changed() {
    let work = Work::new();
    let pack = work.pack("pack", WITH_ORPHAN);
    let report = work.reviewed(&pack, &[]);
    fs::write(pack.join("README.txt"), "changed").unwrap();
    let out = work.path("out.zip");

    let output = work.optimise_expecting(&pack, &report, &out, &[]);

    assert_refused(&output, &out, &["pack differs"]);
}

#[test]
fn expect_report_refuses_a_source_that_changed() {
    let work = Work::new();
    let pack = work.pack("pack", WITH_ORPHAN);
    let keep = work.path("keep.yml");
    fs::write(&keep, KEEP).unwrap();
    let report = work.reviewed(&pack, &["--keep", keep.to_str().unwrap()]);
    fs::write(&keep, KEEP.replace("nothing", "elsewhere")).unwrap();
    let out = work.path("out.zip");

    let output = work.optimise_expecting(&pack, &report, &out, &["--keep", keep.to_str().unwrap()]);

    assert_refused(
        &output,
        &out,
        &[
            "the report has source keep keep.yml",
            "this run has source keep keep.yml",
        ],
    );
}

#[test]
fn expect_report_refuses_a_source_the_report_was_made_without() {
    let work = Work::new();
    let pack = work.pack("pack", WITH_ORPHAN);
    let report = work.reviewed(&pack, &[]);
    let keep = work.path("keep.yml");
    fs::write(&keep, KEEP).unwrap();
    let out = work.path("out.zip");

    let output = work.optimise_expecting(&pack, &report, &out, &["--keep", keep.to_str().unwrap()]);

    assert_refused(&output, &out, &["this run has source keep keep.yml"]);
}

#[test]
fn expect_report_refuses_a_file_to_drop_the_report_does_not_list() {
    let work = Work::new();
    let pack = work.pack("pack", WITH_ORPHAN);
    let report = work.reviewed(&pack, &[]);
    let listed = format!("\"removable\": [\n    \"{ORPHAN_PATH}\"\n  ]");
    let edited = read(&report).replace(&listed, "\"removable\": []");
    assert_ne!(edited, read(&report), "the report lists the orphan");
    fs::write(&report, edited).unwrap();
    let out = work.path("out.zip");

    let output = work.optimise_expecting(&pack, &report, &out, &[]);

    assert_refused(
        &output,
        &out,
        &[&format!(
            "this run drops {ORPHAN_PATH}, the report keeps it"
        )],
    );
}

#[test]
fn expect_report_refuses_a_file_the_report_drops_and_the_run_keeps() {
    let work = Work::new();
    let pack = work.pack("pack", WITH_ORPHAN);
    let report = work.reviewed(&pack, &[]);
    let listed = format!("\"removable\": [\n    \"{ORPHAN_PATH}\"\n  ]");
    let edited = read(&report).replace(
        &listed,
        &format!(
            "\"removable\": [\n    \"{ORPHAN_PATH}\",\n    \"assets/fx/models/item/box.json\"\n  ]"
        ),
    );
    assert_ne!(edited, read(&report), "the report lists the orphan");
    fs::write(&report, edited).unwrap();
    let out = work.path("out.zip");

    let output = work.optimise_expecting(&pack, &report, &out, &[]);

    assert_refused(
        &output,
        &out,
        &["the report drops assets/fx/models/item/box.json, this run keeps it"],
    );
}

#[test]
fn expect_report_refuses_a_report_it_cannot_read() {
    let work = Work::new();
    let pack = work.pack("pack", WITH_ORPHAN);
    let report = work.reviewed(&pack, &[]);
    let out = work.path("out.zip");

    let absent = work.path("absent.json");
    let output = work.optimise_expecting(&pack, &absent, &out, &[]);
    assert_refused(&output, &out, &["--expect-report", "no such file"]);

    let notes = work.path("notes.json");
    fs::write(&notes, "{\"hello\": 1}").unwrap();
    let output = work.optimise_expecting(&pack, &notes, &out, &[]);
    assert_refused(
        &output,
        &out,
        &["not a packprune report", "no schema field"],
    );

    fs::write(&notes, "not json").unwrap();
    let output = work.optimise_expecting(&pack, &notes, &out, &[]);
    assert_refused(&output, &out, &["not JSON"]);

    let future = work.path("future.json");
    fs::write(
        &future,
        read(&report).replace("\"schema\": 4", "\"schema\": 5"),
    )
    .unwrap();
    let output = work.optimise_expecting(&pack, &future, &out, &[]);
    assert_refused(&output, &out, &["schema 5"]);
}

#[test]
fn expect_report_refuses_a_client_jar_that_changed() {
    let work = Work::new();
    let pack = work.pack("pack", WITH_ORPHAN);
    let report = work.reviewed(&pack, &[]);
    let mut jar = Vec::new();
    collect(
        &repo().join("tests/fixtures/vanilla"),
        Path::new(""),
        &mut jar,
    );
    jar.push(("assets/minecraft/extra.txt".to_owned(), b"x".to_vec()));
    fs::write(work.path("client.jar"), stored_zip(&jar)).unwrap();
    let out = work.path("out.zip");

    let output = work.optimise_expecting(&pack, &report, &out, &[]);

    assert_refused(&output, &out, &["client jar differs"]);
}

#[test]
fn expect_report_refuses_an_asset_index_the_report_was_made_without() {
    let work = Work::new();
    let pack = work.pack("pack", WITH_ORPHAN);
    let report = work.reviewed(&pack, &[]);
    let index = work.path("index.json");
    fs::write(&index, "{\"objects\": {}}").unwrap();
    let out = work.path("out.zip");

    let output = work.optimise_expecting(
        &pack,
        &report,
        &out,
        &["--asset-index", index.to_str().unwrap()],
    );

    assert_refused(&output, &out, &["asset index differs: the report has none"]);
}

#[test]
fn expect_report_refuses_a_report_whose_source_inputs_have_no_blake3() {
    let work = Work::new();
    let pack = work.pack("pack", WITH_ORPHAN);
    let keep = work.path("keep.yml");
    fs::write(&keep, KEEP).unwrap();
    let report = work.reviewed(&pack, &["--keep", keep.to_str().unwrap()]);
    let one = work.path("one.json");
    fs::write(&one, read(&report).replace("\"blake3\"", "\"sha256\"")).unwrap();
    let out = work.path("out.zip");

    let output = work.optimise_expecting(&pack, &one, &out, &["--keep", keep.to_str().unwrap()]);

    assert_refused(&output, &out, &["not a packprune report", "blake3"]);
}
