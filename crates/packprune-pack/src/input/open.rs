use std::fs::File;
use std::io;
#[cfg(unix)]
use std::path::Path;

use super::InputError;
#[cfg(unix)]
use super::refused;
use crate::PhysPath;

#[cfg(not(unix))]
pub(super) use super::portable::PortableRoot as Root;

/// Where the files of a walked directory are opened from.
pub(super) trait Opener: Sync {
    /// Opens a regular file of `size` bytes; a link, a special file or another size is refused by name.
    fn open_regular(&self, path: &PhysPath, size: u64) -> Result<File, InputError>;
}

/// The pack directory held by descriptor: a file is opened relative to it, one component at a time without
/// following a link, so nothing swapped into the tree after the walk can lead the read outside the pack.
#[cfg(unix)]
pub(super) struct Root(File);

#[cfg(unix)]
impl Root {
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        File::open(path).map(Self)
    }
}

#[cfg(unix)]
impl Opener for Root {
    fn open_regular(&self, path: &PhysPath, size: u64) -> Result<File, InputError> {
        use std::ffi::CString;
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

        let mut parent: Option<OwnedFd> = None;
        let mut names = path.as_str().split('/').peekable();
        while let Some(name) = names.next() {
            let last = names.peek().is_none();
            // A FIFO would block an open for reading until a writer comes: O_NONBLOCK returns at once and the
            // type is checked on the descriptor.
            let flags = libc::O_RDONLY
                | libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | if last {
                    libc::O_NONBLOCK
                } else {
                    libc::O_DIRECTORY
                };
            let name = CString::new(name).expect("a validated path has no NUL");
            let dir = parent
                .as_ref()
                .map_or_else(|| self.0.as_raw_fd(), AsRawFd::as_raw_fd);
            // SAFETY: `dir` is an open descriptor owned by `self` or `parent`, and `name` is NUL-terminated.
            let fd = unsafe { libc::openat(dir, name.as_ptr(), flags) };
            if fd < 0 {
                return Err(open_error(path, io::Error::last_os_error()));
            }
            // SAFETY: `openat` returned a descriptor nothing else owns.
            let fd = unsafe { OwnedFd::from_raw_fd(fd) };
            if last {
                return checked(File::from(fd), path, size);
            }
            parent = Some(fd);
        }
        unreachable!("a validated path has a component")
    }
}

#[cfg(unix)]
fn open_error(path: &PhysPath, error: io::Error) -> InputError {
    match error.raw_os_error() {
        Some(libc::ELOOP | libc::ENOTDIR) => {
            refused(path.as_str(), "symlink, or entry replaced after the walk")
        }
        Some(libc::ENXIO) => refused(path.as_str(), "not a regular file"),
        _ => context(path, error),
    }
}

#[cfg(unix)]
fn checked(file: File, path: &PhysPath, size: u64) -> Result<File, InputError> {
    let metadata = file.metadata().map_err(|error| context(path, error))?;
    if !metadata.is_file() {
        return Err(refused(path.as_str(), "not a regular file"));
    }
    if metadata.len() != size {
        return Err(refused(path.as_str(), "size changed while reading"));
    }
    Ok(file)
}

pub(super) fn context(path: &PhysPath, error: io::Error) -> InputError {
    InputError::Io(io::Error::new(error.kind(), format!("{path}: {error}")))
}
