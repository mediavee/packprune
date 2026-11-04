use std::borrow::Borrow;
use std::fmt;
use std::sync::Arc;

#[cfg(test)]
mod tests;

/// A path as stored in the pack, overlay folder included.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PhysPath(Arc<str>);

/// A path as the client asks for it (`assets/...`), whatever layer holds it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LogicalPath(Arc<str>);

/// The root of the pack or one overlay directory.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Layer {
    Root,
    Overlay(Box<str>),
}

/// A path that breaks the rules of [`packprune_zip::validate_path`], or names a directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("unsafe path")]
pub struct PathError;

fn check(path: &str) -> Result<(), PathError> {
    // `validate_path` takes a trailing `/` for a zip's directory markers; a file path has none.
    if path.ends_with('/') || packprune_zip::validate_path(path).is_err() {
        return Err(PathError);
    }
    Ok(())
}

impl PhysPath {
    pub fn new(path: &str) -> Result<Self, PathError> {
        check(path).map(|()| Self(path.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl LogicalPath {
    pub fn new(path: &str) -> Result<Self, PathError> {
        check(path).map(|()| Self(path.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A path orders, compares and hashes as its text, so a map keyed by paths is searched by text without making one.
impl Borrow<str> for PhysPath {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PhysPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Display for LogicalPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
