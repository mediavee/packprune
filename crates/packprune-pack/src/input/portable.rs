use std::fs::{self, File, Metadata};
use std::io;
use std::path::{Path, PathBuf};

use super::open::{Opener, context};
use super::{InputError, refused};
use crate::PhysPath;

/// The pack directory by path, for targets without `openat`: every component is checked from the root down
/// right before and right after the open, and the opened file must be the one checked.
pub(super) struct PortableRoot {
    root: PathBuf,
    /// Let a test change the tree just before and just after the open, which no other test can time.
    #[cfg(test)]
    pub(super) before_open: Option<Box<dyn Fn() + Send + Sync>>,
    #[cfg(test)]
    pub(super) after_open: Option<Box<dyn Fn() + Send + Sync>>,
}

impl PortableRoot {
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        Ok(Self {
            root: path.to_owned(),
            #[cfg(test)]
            before_open: None,
            #[cfg(test)]
            after_open: None,
        })
    }

    /// The leaf's metadata once every component is a plain directory and the leaf a plain file of `size` bytes.
    fn checked(&self, path: &PhysPath, size: u64) -> Result<Metadata, InputError> {
        let mut current = self.root.clone();
        let mut names = path.as_str().split('/').peekable();
        while let Some(name) = names.next() {
            current.push(name);
            let metadata = fs::symlink_metadata(&current).map_err(|error| context(path, error))?;
            let leaf = names.peek().is_none();
            if is_link(&metadata) || (!leaf && !metadata.is_dir()) {
                return Err(refused(
                    path.as_str(),
                    "symlink, or entry replaced after the walk",
                ));
            }
            if leaf {
                if !metadata.is_file() {
                    return Err(refused(path.as_str(), "not a regular file"));
                }
                if metadata.len() != size {
                    return Err(refused(path.as_str(), "size changed while reading"));
                }
                return Ok(metadata);
            }
        }
        unreachable!("a validated path has a component")
    }
}

impl Opener for PortableRoot {
    fn open_regular(&self, path: &PhysPath, size: u64) -> Result<File, InputError> {
        let before = self.checked(path, size)?;
        #[cfg(test)]
        if let Some(hook) = &self.before_open {
            hook();
        }
        let file =
            File::open(self.root.join(path.as_str())).map_err(|error| context(path, error))?;
        #[cfg(test)]
        if let Some(hook) = &self.after_open {
            hook();
        }
        // What stays open: a swap undone between the last check and the read; only descriptors close that.
        let after = self.checked(path, size)?;
        let opened = file.metadata().map_err(|error| context(path, error))?;
        if !same(&before, &after) || !same(&before, &opened) {
            return Err(refused(path.as_str(), "entry replaced while opening"));
        }
        Ok(file)
    }
}

fn is_link(metadata: &Metadata) -> bool {
    metadata.is_symlink() || is_reparse_point(metadata)
}

#[cfg(windows)]
fn is_reparse_point(metadata: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_: &Metadata) -> bool {
    false
}

/// `file_index`, the Windows file identity, is unstable: size and modification time stand for it.
fn same(a: &Metadata, b: &Metadata) -> bool {
    a.len() == b.len() && a.modified().ok() == b.modified().ok()
}
