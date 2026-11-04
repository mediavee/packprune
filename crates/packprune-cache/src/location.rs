//! Where the store lives: `PACKPRUNE_CACHE` wins; otherwise `cache.dir` is relative
//! to the config file's directory, or to the executable's in the drop-in.

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;

use camino::{Utf8Path, Utf8PathBuf};

use crate::CacheError;

pub const ENV_VAR: &str = "PACKPRUNE_CACHE";

/// `override_dir` (see [`env_override`]) as given, else `configured` under `base`; an absolute `configured` stays
/// as it is.
pub fn resolve_dir(
    override_dir: Option<&Utf8Path>,
    configured: &Utf8Path,
    base: &Utf8Path,
) -> Utf8PathBuf {
    match override_dir {
        Some(dir) => dir.to_owned(),
        None => base.join(configured),
    }
}

/// The directory `PACKPRUNE_CACHE` names; an unset or empty variable is no override.
pub fn env_override() -> Result<Option<Utf8PathBuf>, CacheError> {
    parse_override(std::env::var_os(ENV_VAR))
}

pub(crate) fn parse_override(value: Option<OsString>) -> Result<Option<Utf8PathBuf>, CacheError> {
    match value {
        Some(value) if !value.is_empty() => utf8(value.into()).map(Some),
        _ => Ok(None),
    }
}

/// The directory of the running executable: the base in the drop-in.
pub fn executable_dir() -> Result<Utf8PathBuf, CacheError> {
    let exe = utf8(std::env::current_exe()?)?;
    exe.parent().map(Utf8Path::to_owned).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "the executable has no parent directory",
        )
        .into()
    })
}

fn utf8(path: PathBuf) -> Result<Utf8PathBuf, CacheError> {
    Utf8PathBuf::from_path_buf(path).map_err(|p| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("not UTF-8: {}", p.display()),
        )
        .into()
    })
}
