//! Usage sources, one module each, and the registry that opens them by id.

pub mod configs;
pub mod datapacks;
pub mod keep;
pub mod nexo;

use camino::Utf8Path;
use packprune_graph::{SourceError, UsageSource};
use packprune_pack::RunCx;

/// Opens a source on its input; a source interns the ids it reads in the run's interner.
pub type Open = fn(&Utf8Path, &RunCx) -> Result<Box<dyn UsageSource>, SourceError>;

pub struct SourceProvider {
    pub id: &'static str,
    pub open: Open,
}

pub static PROVIDERS: &[SourceProvider] = &[
    SourceProvider {
        id: "keep",
        open: keep::open,
    },
    SourceProvider {
        id: "configs",
        open: configs::open,
    },
    SourceProvider {
        id: "datapacks",
        open: datapacks::open,
    },
    SourceProvider {
        id: "nexo",
        open: nexo::open,
    },
];

#[cfg(test)]
mod parse_tests;
#[cfg(test)]
mod tests;

mod document;
mod memo;
mod rules;

use camino::Utf8PathBuf;
use packprune_graph::Usage;
use packprune_pack::Digest;

fn refused(file: impl ToString, why: impl ToString) -> SourceError {
    SourceError::Refused(file.to_string(), why.to_string())
}

struct Source {
    inputs: Vec<Utf8PathBuf>,
    root: Utf8PathBuf,
    cx: RunCx,
    collect: fn(&Utf8Path, &RunCx) -> Result<Vec<Usage>, SourceError>,
    digest: fn(&Utf8Path, &RunCx) -> Result<Digest, SourceError>,
}

impl UsageSource for Source {
    fn inputs(&self) -> &[Utf8PathBuf] {
        &self.inputs
    }

    fn input_digest(&self, input: &Utf8Path) -> Result<Digest, SourceError> {
        if !self.inputs.iter().any(|p| p == input) {
            return Err(refused(input, "not an input of this source"));
        }
        (self.digest)(input, &self.cx)
    }

    fn collect(&self) -> Result<Vec<Usage>, SourceError> {
        (self.collect)(&self.root, &self.cx)
    }
}

fn source(
    path: &Utf8Path,
    cx: &RunCx,
    collect: fn(&Utf8Path, &RunCx) -> Result<Vec<Usage>, SourceError>,
    digest: fn(&Utf8Path, &RunCx) -> Result<Digest, SourceError>,
) -> Box<dyn UsageSource> {
    Box::new(Source {
        inputs: vec![path.to_owned()],
        root: path.to_owned(),
        cx: cx.clone(),
        collect,
        digest,
    })
}

fn files(
    root: &Utf8Path,
    skip: &dyn Fn(&Utf8Path) -> bool,
) -> Result<Vec<Utf8PathBuf>, SourceError> {
    fn walk(
        root: &Utf8Path,
        skip: &dyn Fn(&Utf8Path) -> bool,
        out: &mut Vec<Utf8PathBuf>,
    ) -> Result<(), SourceError> {
        if skip(root) {
            return Ok(());
        }
        let meta = std::fs::symlink_metadata(root)?;
        if meta.file_type().is_symlink() {
            if root.is_file() {
                out.push(root.to_owned());
            }
            return Ok(());
        }
        if meta.is_dir() {
            for child in std::fs::read_dir(root)? {
                let path = Utf8PathBuf::from_path_buf(child?.path())
                    .map_err(|p| refused(p.display(), "not a UTF-8 path"))?;
                walk(&path, skip, out)?;
            }
        } else if meta.is_file() {
            out.push(root.to_owned());
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(root, skip, &mut out)?;
    out.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    Ok(out)
}

/// The path of `file` under `root`, which it was found under.
fn relative<'a>(file: &'a Utf8Path, root: &Utf8Path) -> &'a str {
    file.strip_prefix(root)
        .expect("a file found under the root lies under it")
        .as_str()
}

fn hash_part(hash: &mut blake3::Hasher, bytes: &[u8]) {
    hash.update(&(bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}

fn digest_files(root: &Utf8Path, mut selected: Vec<Utf8PathBuf>) -> Result<Digest, SourceError> {
    selected.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    selected.dedup();
    let mut hash = blake3::Hasher::new();
    for file in selected {
        hash_part(&mut hash, relative(&file, root).as_bytes());
        hash_part(&mut hash, blake3::hash(&std::fs::read(file)?).as_bytes());
    }
    Ok(Digest(*hash.finalize().as_bytes()))
}
