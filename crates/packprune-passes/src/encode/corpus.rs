//! Every optimiser on every file of a real pack named by environment variables, ignored by default
//! (`cargo test -p packprune-passes --release -- --ignored encode::corpus`).
//!
//! - `CORPUS_PACK`: the pack, a folder or zip.
//! - `CORPUS_TSV`: where to write one line per file an optimiser applies to: path, optimiser, bytes in, bytes out.

use std::fmt::Write as _;
use std::time::Instant;
use std::{env, fs};

use packprune_pack::{Layout, Limits, PackInput, PackMeta, Quirks, VanillaIndex};
use packprune_pass::FileCx;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};

use super::optimisers;

fn var(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("set {name}"))
}

#[test]
#[ignore = "needs CORPUS_PACK and CORPUS_TSV"]
fn corpus_optimisers() {
    let pack = PackInput::open(var("CORPUS_PACK").as_str().as_ref(), &Limits::default()).unwrap();
    let layout = Layout::new(
        &pack,
        PackMeta::read(&pack).unwrap(),
        &VanillaIndex::default(),
    );
    let optimisers = optimisers();
    let start = Instant::now();
    let lines: Vec<String> = pack
        .files
        .par_iter()
        .flat_map_iter(|(path, entry)| {
            let kind = layout.kinds[path];
            let cx = FileCx {
                kind,
                path: path.clone(),
                quirks: Quirks::default(),
            };
            optimisers
                .iter()
                .filter(|optimiser| optimiser.applies(kind, path))
                .map(|optimiser| {
                    let out = optimiser
                        .optimise(&entry.blob, &cx, &toml::Table::new())
                        .unwrap()
                        .map_or(entry.blob.len(), |optimised| optimised.bytes.len());
                    format!(
                        "{path}\t{}\t{}\t{out}\n",
                        optimiser.name(),
                        entry.blob.len()
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect();
    let mut tsv = String::new();
    for line in &lines {
        tsv.write_str(line).unwrap();
    }
    fs::write(var("CORPUS_TSV"), tsv).unwrap();
    eprintln!(
        "{}: {} files optimised in {:?}",
        pack.name,
        lines.len(),
        start.elapsed()
    );
}
