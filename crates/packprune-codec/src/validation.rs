use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use rayon::iter::{IntoParallelRefIterator, ParallelIterator};

use crate::{audio, glsl, minify, png, properties};

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn deflated_len(bytes: &[u8]) -> usize {
    let mut encoder = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap().len()
}

#[test]
#[ignore = "requires PACKPRUNE_CODEC_CORPUS (a directory of pack files)"]
fn corpus_equivalence() {
    let root = PathBuf::from(std::env::var_os("PACKPRUNE_CODEC_CORPUS").unwrap());
    let effort = std::env::var("PACKPRUNE_CODEC_EFFORT").map_or(255, |v| v.parse().unwrap());
    let mut paths = Vec::new();
    files(&root, &mut paths);
    paths.sort();
    assert!(!paths.is_empty());
    let results: Vec<_> = paths
        .par_iter()
        .filter_map(|path| {
            let extension = path.extension()?.to_str()?;
            let kind = match extension {
                "png" => "PNG",
                "ogg" => "Ogg",
                "json" | "mcmeta" => "JSON",
                "vsh" | "fsh" | "glsl" => "GLSL",
                _ => return None,
            };
            let bytes = fs::read(path).unwrap();
            let result = match kind {
                "PNG" => png::optimise(
                    &bytes,
                    &png::Options {
                        effort,
                        ..Default::default()
                    },
                ),
                "Ogg" => audio::optimise(&bytes),
                "JSON" => minify::json(&bytes),
                "GLSL" => glsl::minify(&bytes),
                _ => unreachable!(),
            };
            let (out, fallback) = match result {
                Ok(out) => (out, false),
                Err(error) => {
                    println!("fallback {}: {error}", path.display());
                    (bytes.clone(), true)
                }
            };
            assert!(out.len() <= bytes.len(), "{}", path.display());
            if !fallback {
                match kind {
                    "PNG" => assert_eq!(
                        png::decode(&bytes).unwrap(),
                        png::decode(&out).unwrap(),
                        "{}",
                        path.display()
                    ),
                    "Ogg" => assert_eq!(
                        audio::decode(&bytes).unwrap(),
                        audio::decode(&out).unwrap(),
                        "{}",
                        path.display()
                    ),
                    "JSON" => assert_eq!(
                        minify::json(&bytes).unwrap(),
                        minify::json(&out).unwrap(),
                        "{}",
                        path.display()
                    ),
                    "GLSL" => assert_eq!(
                        glsl::tokenize(&bytes).unwrap(),
                        glsl::tokenize(&out).unwrap(),
                        "{}",
                        path.display()
                    ),
                    _ => unreachable!(),
                }
            } else {
                assert_eq!(out, bytes);
            }
            Some((
                kind,
                bytes.len(),
                out.len(),
                deflated_len(&bytes),
                deflated_len(&out),
                usize::from(fallback),
                usize::from(out == bytes),
            ))
        })
        .collect();
    let mut totals = BTreeMap::<_, [usize; 7]>::new();
    for (kind, before, after, zip_before, zip_after, fallback, unchanged) in results {
        let row = totals.entry(kind).or_default();
        for (sum, value) in row
            .iter_mut()
            .zip([1, before, after, zip_before, zip_after, fallback, unchanged])
        {
            *sum += value;
        }
    }
    for (
        kind,
        [
            count,
            before,
            after,
            zip_before,
            zip_after,
            fallback,
            unchanged,
        ],
    ) in totals
    {
        println!(
            "{kind}: files={count} raw={before}->{after} saved={} deflate9={zip_before}->{zip_after} saved={} fallbacks={fallback} unchanged={unchanged}",
            before - after,
            zip_before as i64 - zip_after as i64
        );
    }
}

#[test]
#[ignore = "subprocess helper; requires PACKPRUNE_CODEC_PROCESS_INPUT and OUTPUT"]
fn optimise_in_process() {
    let input = fs::read(std::env::var_os("PACKPRUNE_CODEC_PROCESS_INPUT").unwrap()).unwrap();
    fs::write(
        std::env::var_os("PACKPRUNE_CODEC_PROCESS_OUTPUT").unwrap(),
        audio::optimise(&input).unwrap(),
    )
    .unwrap();
}

#[test]
fn ogg_is_identical_across_processes() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.ogg");
    fs::write(&input, audio::encode(&audio::tone(48000, 1), 0.4).unwrap()).unwrap();
    for n in 0..2 {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "validation::optimise_in_process", "--ignored"])
            .env("PACKPRUNE_CODEC_PROCESS_INPUT", &input)
            .env(
                "PACKPRUNE_CODEC_PROCESS_OUTPUT",
                dir.path().join(format!("{n}.ogg")),
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
    assert_eq!(
        fs::read(dir.path().join("0.ogg")).unwrap(),
        fs::read(dir.path().join("1.ogg")).unwrap()
    );
}

fn generated_properties_cases() -> Vec<Vec<u8>> {
    let mut state = 0x12345678u32;
    let mut next = || {
        state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        state as usize
    };
    let keys = [
        r"a\ b",
        r"a\:b",
        r"a\=b",
        r"\u0061",
        "same",
        "",
        r"back\\slash",
    ];
    let values = [
        r"\t\n\r\f",
        r"\u00e9",
        r"\uD83D\uDE00",
        r"a\q",
        "",
        "one\\\n \ttwo",
        "value  ",
        "\\\\",
        "\\\n#continued",
    ];
    let separators = ["=", ":", " \t", " \t= \t", " : "];
    let endings = ["\n", "\r", "\r\n"];
    let mut cases = Vec::new();
    for _ in 0..1024 {
        let mut input = b" \t# comment\\\n! another\r\n".to_vec();
        for _ in 0..8 {
            input.extend_from_slice(
                format!(
                    " {}{}{}{}",
                    keys[next() % keys.len()],
                    separators[next() % separators.len()],
                    values[next() % values.len()],
                    endings[next() % endings.len()]
                )
                .as_bytes(),
            );
        }
        cases.push(input);
    }
    cases
}

fn properties_output(cases: &[Vec<u8>]) -> String {
    let mut expected = String::new();
    for input in cases {
        let entries = properties::parse_checked(input).unwrap();
        let map: BTreeMap<_, _> = entries.into_iter().map(|p| (p.key, p.value)).collect();
        let hex = |text: &str| {
            text.as_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        };
        for (key, value) in map {
            expected.push_str(&format!("{}={};", hex(&key), hex(&value)));
        }
        expected.push('\n');
    }
    expected
}

#[test]
fn properties_match_java_golden() {
    let expected = include_str!("fixtures/properties-java.txt");
    assert_eq!(expected.lines().count(), 1024);
    for (case, (java, rust)) in expected
        .lines()
        .zip(properties_output(&generated_properties_cases()).lines())
        .enumerate()
    {
        assert_eq!(java, rust, "generated case {case}");
    }
}

#[test]
#[ignore = "requires java (source-file launcher)"]
fn properties_agree_with_java() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("PropertiesReference.java");
    fs::write(&source, r#"
import java.io.*;
import java.nio.file.*;
import java.nio.charset.StandardCharsets;
import java.util.*;
class PropertiesReference {
    public static void main(String[] args) throws Exception {
        try (var input = new DataInputStream(Files.newInputStream(Path.of(args[0])))) {
            while (input.available() > 0) {
                var p = new Properties();
                p.load(new ByteArrayInputStream(input.readNBytes(input.readInt())));
                for (var key : new TreeSet<>(p.stringPropertyNames())) {
                    System.out.print(HexFormat.of().formatHex(key.getBytes(StandardCharsets.UTF_8)) + "="
                        + HexFormat.of().formatHex(p.getProperty(key).getBytes(StandardCharsets.UTF_8)) + ";");
                }
                System.out.println();
            }
        }
    }
}
"#).unwrap();
    let mut cases = Vec::new();
    for input in generated_properties_cases() {
        cases.extend_from_slice(&(input.len() as u32).to_be_bytes());
        cases.extend_from_slice(&input);
    }
    let data = dir.path().join("cases.bin");
    fs::write(&data, cases).unwrap();
    let output = Command::new("java")
        .arg(&source)
        .arg(&data)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let actual = String::from_utf8(output.stdout).unwrap();
    assert_eq!(actual.lines().count(), 1024);
    if let Some(path) = std::env::var_os("PACKPRUNE_CODEC_PROPERTIES_GOLDEN") {
        fs::write(path, &actual).unwrap();
    }
    assert_eq!(actual, include_str!("fixtures/properties-java.txt"));
}

#[test]
#[ignore = "requires PACKPRUNE_CODEC_STB (client stb decoder executable: input.png output.rgba)"]
fn png_matches_client_stb() {
    use ::png::{BitDepth, ColorType, Encoder, ScaledFloat};

    let program = std::env::var_os("PACKPRUNE_CODEC_STB").unwrap();
    let dir = tempfile::tempdir().unwrap();
    for (depth, data) in [
        (BitDepth::Eight, vec![32, 128, 240, 0, 18, 128, 171, 255]),
        (
            BitDepth::Sixteen,
            vec![
                0x12, 0xff, 0x80, 0xfe, 0xab, 0xcd, 0xff, 0, 0x12, 0x80, 0, 0xff, 0xff, 0xfe, 0, 0,
            ],
        ),
    ] {
        let input = dir.path().join("input.png");
        let output = dir.path().join("pixels.rgba");
        let mut bytes = Vec::new();
        {
            let mut encoder = Encoder::new(&mut bytes, 2, 1);
            encoder.set_color(ColorType::Rgba);
            encoder.set_depth(depth);
            encoder.set_source_gamma(ScaledFloat::new(1.0));
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&data)
                .unwrap();
        }
        for png in [
            &bytes,
            &png::optimise(&bytes, &png::Options::default()).unwrap(),
        ] {
            fs::write(&input, png).unwrap();
            let result = Command::new(&program)
                .arg(&input)
                .arg(&output)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert_eq!(fs::read(&output).unwrap(), png::decode(png).unwrap().rgba);
        }
    }
}
