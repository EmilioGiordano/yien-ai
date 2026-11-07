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

/// Where the saved session lives: `state` under [`data_dir`] (plan §7.1).
///
/// ```no_run
/// let state = yien_platform::paths::state_dir()?;
/// assert!(state.ends_with("state"));
/// # Ok::<(), yien_platform::paths::PathError>(())
/// ```
pub fn state_dir() -> Result<PathBuf, PathError> {
    data_dir().map(|dir| dir.join("state"))
}

/// The folder the first project opens on when there is no saved session, read
/// from this process: see [`pick_start_dir`]. `None` when neither the current
/// folder nor the home folder can be found.
///
/// ```no_run
/// let dir = yien_platform::paths::start_dir();
/// assert!(dir.is_some());
/// ```
pub fn start_dir() -> Option<PathBuf> {
    let windows_dir = ["SystemRoot", "windir"]
        .into_iter()
        .filter_map(std::env::var_os)
        .find(|v| !v.is_empty())
        .map(PathBuf::from);
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(PathBuf::from));
    pick_start_dir(StartDirs {
        cwd: std::env::current_dir().ok(),
        windows_dir,
        exe_dir,
        home: home_dir(),
    })
}

/// The user's home folder.
///
/// ```no_run
/// assert!(yien_platform::paths::home_dir().is_some());
/// ```
pub fn home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}

/// What [`pick_start_dir`] decides from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StartDirs {
    /// The process's current folder.
    pub cwd: Option<PathBuf>,
    /// The Windows folder (`%SystemRoot%`), `None` elsewhere.
    pub windows_dir: Option<PathBuf>,
    /// The folder holding the YIEN executable.
    pub exe_dir: Option<PathBuf>,
    /// The user's home folder.
    pub home: Option<PathBuf>,
}

/// The folder the first project opens on when there is no saved session: the
/// current folder, unless it is one a double-click from Explorer or a shortcut
/// leaves behind (the Windows folder, `System32`, `SysWOW64`, or the folder
/// holding the executable), in which case the home folder. Paths compare
/// without regard to case, separators, a trailing separator or a `\\?\` prefix.
///
/// ```
/// use std::path::PathBuf;
/// use yien_platform::paths::{StartDirs, pick_start_dir};
///
/// let dirs = StartDirs {
///     cwd: Some(PathBuf::from(r"C:\WINDOWS\system32")),
///     windows_dir: Some(PathBuf::from(r"C:\Windows")),
///     exe_dir: Some(PathBuf::from(r"C:\Tools\yien")),
///     home: Some(PathBuf::from(r"C:\Users\me")),
/// };
/// assert_eq!(pick_start_dir(dirs), Some(PathBuf::from(r"C:\Users\me")));
/// ```
pub fn pick_start_dir(dirs: StartDirs) -> Option<PathBuf> {
    let Some(cwd) = dirs.cwd else {
        return dirs.home;
    };
    let mut refused: Vec<PathBuf> = dirs.exe_dir.into_iter().collect();
    if let Some(windows) = dirs.windows_dir {
        refused.push(windows.join("System32"));
        refused.push(windows.join("SysWOW64"));
        refused.push(windows);
    }
    let key = comparable(&cwd);
    if refused.iter().any(|dir| comparable(dir) == key) {
        return dirs.home.or(Some(cwd));
    }
    Some(cwd)
}

fn comparable(path: &std::path::Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/").to_lowercase();
    let text = text.strip_prefix("//?/").unwrap_or(&text);
    text.trim_end_matches('/').to_owned()
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

    fn windows(cwd: &str) -> StartDirs {
        StartDirs {
            cwd: Some(PathBuf::from(cwd)),
            windows_dir: Some(PathBuf::from(r"C:\Windows")),
            exe_dir: Some(PathBuf::from(r"D:\Apps\yien")),
            home: Some(PathBuf::from(r"C:\Users\me")),
        }
    }

    #[test]
    fn system_folders_and_the_exe_folder_open_home() {
        for cwd in [
            r"C:\WINDOWS\system32",
            r"C:\Windows\System32\",
            "C:/Windows/SysWOW64",
            r"c:\windows",
            r"\\?\C:\Windows\System32",
            r"D:\Apps\yien",
            r"d:\apps\YIEN\",
        ] {
            assert_eq!(
                pick_start_dir(windows(cwd)),
                Some(PathBuf::from(r"C:\Users\me")),
                "{cwd}"
            );
        }
    }

    #[test]
    fn any_other_folder_is_kept() {
        for cwd in [
            r"C:\Code\yien",
            r"C:\Windows\Temp",
            r"D:\Apps\yien\sub",
            r"C:\",
        ] {
            assert_eq!(
                pick_start_dir(windows(cwd)),
                Some(PathBuf::from(cwd)),
                "{cwd}"
            );
        }
    }

    #[test]
    fn missing_folders_fall_back_sensibly() {
        let no_cwd = StartDirs {
            cwd: None,
            ..windows("")
        };
        assert_eq!(pick_start_dir(no_cwd), Some(PathBuf::from(r"C:\Users\me")));
        let no_home = StartDirs {
            home: None,
            ..windows(r"C:\Windows\System32")
        };
        assert_eq!(
            pick_start_dir(no_home),
            Some(PathBuf::from(r"C:\Windows\System32"))
        );
        let unix = StartDirs {
            cwd: Some(PathBuf::from("/home/me/code")),
            home: Some(PathBuf::from("/home/me")),
            ..StartDirs::default()
        };
        assert_eq!(pick_start_dir(unix), Some(PathBuf::from("/home/me/code")));
        assert_eq!(pick_start_dir(StartDirs::default()), None);
    }
}
