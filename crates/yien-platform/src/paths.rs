//! Per-user directories YIEN owns. Nothing outside them is ever written (RNF-14).

use std::ffi::OsString;
use std::path::PathBuf;

/// Environment variable that replaces [`data_dir`], for a throwaway profile in
/// tests and UI validation.
pub const DATA_DIR_ENV: &str = "YIEN_DATA_DIR";

/// Why a YIEN directory could not be found.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PathError {
    /// The OS reports no per-user local data directory and no override is set.
    #[error("no per-user local data directory, and {DATA_DIR_ENV} is not set")]
    NoDataDir,
}

/// YIEN's per-user data directory: `$YIEN_DATA_DIR` when set and not empty,
/// otherwise `YIEN` under the OS local data dir (`%LOCALAPPDATA%` on Windows).
///
/// The directory is not created.
///
/// ```no_run
/// let dir = yien_platform::paths::data_dir()?;
/// assert!(dir.is_absolute() || std::env::var_os("YIEN_DATA_DIR").is_some());
/// # Ok::<(), yien_platform::paths::PathError>(())
/// ```
pub fn data_dir() -> Result<PathBuf, PathError> {
    resolve_data_dir(std::env::var_os(DATA_DIR_ENV), dirs::data_local_dir())
}

/// Where the log files go: `logs` under [`data_dir`].
///
/// ```no_run
/// let logs = yien_platform::paths::logs_dir()?;
/// assert!(logs.ends_with("logs"));
/// # Ok::<(), yien_platform::paths::PathError>(())
/// ```
pub fn logs_dir() -> Result<PathBuf, PathError> {
    resolve_logs_dir(std::env::var_os(DATA_DIR_ENV), dirs::data_local_dir())
}

fn resolve_logs_dir(
    override_dir: Option<OsString>,
    local_data: Option<PathBuf>,
) -> Result<PathBuf, PathError> {
    resolve_data_dir(override_dir, local_data).map(|dir| dir.join("logs"))
}

fn resolve_data_dir(
    override_dir: Option<OsString>,
    local_data: Option<PathBuf>,
) -> Result<PathBuf, PathError> {
    match override_dir {
        Some(dir) if !dir.is_empty() => Ok(PathBuf::from(dir)),
        _ => local_data
            .map(|base| base.join("YIEN"))
            .ok_or(PathError::NoDataDir),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_override_wins_over_the_os_dir() {
        let dir = resolve_data_dir(
            Some(OsString::from("D:/scratch/profile")),
            Some(PathBuf::from("C:/Users/u/AppData/Local")),
        );
        assert_eq!(dir, Ok(PathBuf::from("D:/scratch/profile")));
    }

    #[test]
    fn without_override_it_is_yien_under_the_os_dir() {
        let dir = resolve_data_dir(None, Some(PathBuf::from("C:/Users/u/AppData/Local")));
        assert_eq!(
            dir,
            Ok(PathBuf::from("C:/Users/u/AppData/Local").join("YIEN"))
        );
    }

    #[test]
    fn an_empty_override_is_ignored() {
        let dir = resolve_data_dir(Some(OsString::new()), Some(PathBuf::from("/home/u/.local")));
        assert_eq!(dir, Ok(PathBuf::from("/home/u/.local").join("YIEN")));
    }

    #[test]
    fn no_os_dir_and_no_override_is_an_error() {
        assert_eq!(resolve_data_dir(None, None), Err(PathError::NoDataDir));
        assert_eq!(
            resolve_data_dir(Some(OsString::new()), None),
            Err(PathError::NoDataDir)
        );
    }

    #[test]
    fn the_override_needs_no_os_dir() {
        let dir = resolve_data_dir(Some(OsString::from("/tmp/p")), None);
        assert_eq!(dir, Ok(PathBuf::from("/tmp/p")));
    }

    #[test]
    fn logs_follow_the_override_and_the_os_dir() {
        assert_eq!(
            resolve_logs_dir(Some(OsString::from("D:/scratch/profile")), None),
            Ok(PathBuf::from("D:/scratch/profile").join("logs"))
        );
        assert_eq!(
            resolve_logs_dir(None, Some(PathBuf::from("C:/Users/u/AppData/Local"))),
            Ok(PathBuf::from("C:/Users/u/AppData/Local")
                .join("YIEN")
                .join("logs"))
        );
        assert_eq!(resolve_logs_dir(None, None), Err(PathError::NoDataDir));
    }
}
