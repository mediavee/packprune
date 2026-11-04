//! The input hashes of `report.json`: a file's SHA-256, a directory's over its sorted
//! relative paths and file hashes, so the hash does not depend on the file system.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

pub fn sha256(path: &Path) -> io::Result<String> {
    if path.is_dir() {
        return tree(path);
    }
    Ok(hex(&file(path)?))
}

fn file(path: &Path) -> io::Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    let mut input = File::open(path)?;
    let mut buffer = vec![0; 1 << 16];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

fn tree(root: &Path) -> io::Result<String> {
    let mut files = BTreeMap::new();
    collect(root, root, &mut files)?;
    let mut hasher = Sha256::new();
    for (name, path) in files {
        hasher.update(name.as_bytes());
        hasher.update([0]);
        hasher.update(file(&path)?);
    }
    Ok(hex(&hasher.finalize().into()))
}

fn collect(root: &Path, dir: &Path, files: &mut BTreeMap<String, PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect(root, &path, files)?;
        } else if path.is_file() {
            let relative = path.strip_prefix(root).expect("below the root it walks");
            let name: Vec<String> = relative
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect();
            files.insert(name.join("/"), path);
        }
    }
    Ok(())
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `printf abc | sha256sum`.
    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn a_file_is_its_sha256() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a"), b"abc").unwrap();
        assert_eq!(sha256(&dir.path().join("a")).unwrap(), ABC);
    }

    #[test]
    fn a_directory_hashes_its_sorted_paths_and_file_hashes() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/b"), b"abc").unwrap();
        fs::write(dir.path().join("a"), b"abc").unwrap();
        let mut expected = Sha256::new();
        for name in ["a", "sub/b"] {
            expected.update(name.as_bytes());
            expected.update([0]);
            expected.update(Sha256::digest(b"abc"));
        }
        assert_eq!(
            sha256(dir.path()).unwrap(),
            hex(&expected.finalize().into())
        );
    }
}
