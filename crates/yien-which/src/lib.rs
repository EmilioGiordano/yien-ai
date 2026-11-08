//! Finds the file a program name means, the way a shell would, without ever
//! searching the current folder.
//!
//! A bare name is looked up in each absolute `PATH` folder in order; empty,
//! `.` and other relative entries are skipped, since the OS would resolve
//! them against YIEN's own working folder. On Windows each `PATHEXT`
//! extension is tried in order when the name has none of them. An absolute
//! path is used as given, and a relative path with folders in it is refused.
//!
//! Pure: the caller reads the environment and says which files exist.
//!
//! ```
//! use std::ffi::OsStr;
//! use std::path::{Path, PathBuf};
//!
//! let root = if cfg!(windows) { "C:/" } else { "/" };
//! let bin = Path::new(root).join("bin");
//! let path = std::env::join_paths([PathBuf::from("."), bin.clone()]).unwrap();
//! let exists = |p: &Path| p == bin.join("claude.exe") || p == Path::new("claude.exe");
//!
//! let found = yien_which::resolve(OsStr::new("claude"), Some(&path), ".EXE;.CMD", exists);
//! assert_eq!(found, Ok(bin.join("claude.exe")));
//! ```

#![forbid(unsafe_code)]

use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};

/// The extensions Windows tries when `PATHEXT` is unset.
///
/// ```
/// assert!(yien_which::DEFAULT_PATHEXT.contains(".EXE"));
/// ```
pub const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

/// Why a program name gives no file.
///
/// ```
/// use std::ffi::OsStr;
/// let err = yien_which::resolve(OsStr::new("claude"), None, "", |_| true).unwrap_err();
/// assert_eq!(err, yien_which::WhichError::NotFound);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WhichError {
    /// The name is empty.
    #[error("the program name is empty")]
    Empty,
    /// A bare name found in no absolute `PATH` folder.
    #[error("not found on PATH (the current folder is not searched)")]
    NotFound,
    /// A relative path with folders in it (`bin/tool`, `.\tool`, `\tool`,
    /// `C:tool`); it would resolve against the current folder.
    #[error("a relative path; give a bare name found on PATH or a full path")]
    Relative,
}

/// The file `program` names.
///
/// `path` is the `PATH` variable; `pathext` the `;`-separated extensions to
/// try, `PATHEXT` on Windows and empty elsewhere. `exists` says whether a
/// candidate is a file that can be run (on Unix, one with an execute bit).
///
/// - An absolute path is returned as given, or with the first `pathext`
///   extension that exists when it has none of them.
/// - A bare name is looked up in each absolute `path` folder in order. With
///   one of the `pathext` extensions, or with `pathext` empty, it is tried as
///   is; otherwise only with each extension in order, so npm's extensionless
///   shell script next to its `.cmd` shim is never picked.
///
/// ```
/// use std::ffi::OsStr;
/// use std::path::Path;
///
/// let root = if cfg!(windows) { "C:/" } else { "/" };
/// let tool = Path::new(root).join("opt").join("tool");
/// let found = yien_which::resolve(tool.as_os_str(), None, "", |_| false);
/// assert_eq!(found, Ok(tool));
///
/// let relative = Path::new("bin").join("tool");
/// let err = yien_which::resolve(relative.as_os_str(), None, "", |_| true);
/// assert_eq!(err, Err(yien_which::WhichError::Relative));
/// ```
pub fn resolve(
    program: &OsStr,
    path: Option<&OsStr>,
    pathext: &str,
    exists: impl Fn(&Path) -> bool,
) -> Result<PathBuf, WhichError> {
    if program.is_empty() {
        return Err(WhichError::Empty);
    }
    let name = Path::new(program);
    let exts = extensions(pathext);
    let has_ext = exts.is_empty()
        || name
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|e| exts.contains(&format!(".{}", e.to_ascii_lowercase())));
    let find = |base: PathBuf| -> Option<PathBuf> {
        if has_ext {
            return exists(&base).then_some(base);
        }
        exts.iter().find_map(|ext| {
            let mut file = OsString::from(base.as_os_str());
            file.push(ext);
            let file = PathBuf::from(file);
            exists(&file).then_some(file)
        })
    };
    if name.is_absolute() {
        return Ok(find(name.to_path_buf()).unwrap_or_else(|| name.to_path_buf()));
    }
    if !matches!(
        name.components().collect::<Vec<_>>()[..],
        [Component::Normal(_)]
    ) {
        return Err(WhichError::Relative);
    }
    let path = path.ok_or(WhichError::NotFound)?;
    std::env::split_paths(path)
        .filter(|dir| dir.is_absolute())
        .find_map(|dir| find(dir.join(name)))
        .ok_or(WhichError::NotFound)
}

/// The `pathext` extensions, lowercased, each starting with a dot.
fn extensions(pathext: &str) -> Vec<String> {
    pathext
        .split(';')
        .map(|e| e.trim().to_ascii_lowercase())
        .filter(|e| e.starts_with('.') && e.len() > 1)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn root() -> PathBuf {
        PathBuf::from(if cfg!(windows) { "C:/" } else { "/" })
    }

    fn abs(dir: &str) -> PathBuf {
        root().join(dir)
    }

    fn files(list: &[PathBuf]) -> impl Fn(&Path) -> bool + use<> {
        let set: HashSet<PathBuf> = list.iter().cloned().collect();
        move |p| set.contains(p)
    }

    fn path(dirs: &[PathBuf]) -> OsString {
        std::env::join_paths(dirs).unwrap()
    }

    #[test]
    fn a_bare_name_finds_the_exe_on_path() {
        let path = path(&[abs("tools"), abs("home/bin")]);
        let exe = abs("home/bin").join("claude.exe");
        let found = resolve(
            OsStr::new("claude"),
            Some(&path),
            ".COM;.EXE;.BAT;.CMD",
            files(std::slice::from_ref(&exe)),
        );
        assert_eq!(found, Ok(exe));
    }

    #[test]
    fn an_npm_shim_finds_the_cmd_not_the_shell_script() {
        let npm = abs("npm");
        let path = path(std::slice::from_ref(&npm));
        let exists = files(&[npm.join("opencode"), npm.join("opencode.cmd")]);
        let found = resolve(OsStr::new("opencode"), Some(&path), ".EXE;.CMD", exists);
        assert_eq!(found, Ok(npm.join("opencode.cmd")));
    }

    #[test]
    fn path_order_comes_before_pathext_order() {
        let (a, b) = (abs("a"), abs("b"));
        let path = path(&[a.clone(), b.clone()]);
        let exists = files(&[a.join("x.cmd"), b.join("x.exe")]);
        let found = resolve(OsStr::new("x"), Some(&path), ".EXE;.CMD", &exists);
        assert_eq!(found, Ok(a.join("x.cmd")));
        let exists = files(&[a.join("x.cmd"), a.join("x.exe")]);
        let found = resolve(OsStr::new("x"), Some(&path), ".EXE;.CMD", &exists);
        assert_eq!(found, Ok(a.join("x.exe")));
        let found = resolve(OsStr::new("x"), Some(&path), ".CMD;.EXE", &exists);
        assert_eq!(found, Ok(a.join("x.cmd")));
    }

    #[test]
    fn a_name_with_a_pathext_extension_is_looked_up_as_is() {
        let w = abs("w");
        let path = path(std::slice::from_ref(&w));
        let exists = files(&[w.join("pwsh.exe"), w.join("pwsh.exe.exe")]);
        let found = resolve(OsStr::new("pwsh.exe"), Some(&path), DEFAULT_PATHEXT, exists);
        assert_eq!(found, Ok(w.join("pwsh.exe")));
        let exists = files(&[w.join("PWSH.EXE")]);
        let found = resolve(OsStr::new("PWSH.EXE"), Some(&path), ".exe", exists);
        assert_eq!(found, Ok(w.join("PWSH.EXE")));
    }

    #[test]
    fn a_name_with_another_extension_gets_pathext_appended() {
        let w = abs("w");
        let path = path(std::slice::from_ref(&w));
        let exists = files(&[w.join("python3.12"), w.join("python3.12.exe")]);
        let found = resolve(OsStr::new("python3.12"), Some(&path), ".EXE", exists);
        assert_eq!(found, Ok(w.join("python3.12.exe")));
    }

    #[test]
    fn without_pathext_a_bare_name_is_looked_up_as_is() {
        let bin = abs("usr/bin");
        let path = path(std::slice::from_ref(&bin));
        let found = resolve(OsStr::new("sh"), Some(&path), "", files(&[bin.join("sh")]));
        assert_eq!(found, Ok(bin.join("sh")));
    }

    #[test]
    fn relative_and_empty_path_entries_are_skipped() {
        let bin = abs("bin");
        let planted = [
            PathBuf::from("claude.exe"),
            PathBuf::from(".").join("claude.exe"),
            PathBuf::from("bin").join("claude.exe"),
            PathBuf::from("..").join("claude.exe"),
        ];
        let exists = files(&planted);
        for dirs in [
            vec![PathBuf::from(".")],
            vec![PathBuf::from("bin")],
            vec![PathBuf::from("..")],
            vec![PathBuf::new()],
        ] {
            let path = path(&dirs);
            let found = resolve(OsStr::new("claude"), Some(&path), ".EXE", &exists);
            assert_eq!(found, Err(WhichError::NotFound), "{dirs:?}");
        }
        let mut all = planted.to_vec();
        all.push(bin.join("claude.exe"));
        let path = path(&[PathBuf::from("."), PathBuf::from("bin"), bin.clone()]);
        let found = resolve(OsStr::new("claude"), Some(&path), ".EXE", files(&all));
        assert_eq!(found, Ok(bin.join("claude.exe")));
    }

    #[test]
    fn a_missing_or_empty_name_is_an_error() {
        let path = path(&[abs("w")]);
        let none = |_: &Path| false;
        assert_eq!(
            resolve(OsStr::new("claude"), Some(&path), ".EXE", none),
            Err(WhichError::NotFound)
        );
        assert_eq!(
            resolve(OsStr::new("claude"), None, ".EXE", |_| true),
            Err(WhichError::NotFound)
        );
        assert_eq!(
            resolve(OsStr::new(""), Some(&path), ".EXE", |_| true),
            Err(WhichError::Empty)
        );
    }

    #[test]
    fn a_relative_path_with_folders_is_refused() {
        let path = path(&[abs("w")]);
        let mut names = vec![
            PathBuf::from("bin").join("claude"),
            PathBuf::from(".").join("claude"),
            PathBuf::from(".."),
            PathBuf::from("."),
        ];
        if cfg!(windows) {
            names.push(PathBuf::from(r"\tools\claude.exe"));
            names.push(PathBuf::from("C:claude.exe"));
        }
        for name in names {
            assert_eq!(
                resolve(name.as_os_str(), Some(&path), ".EXE", |_| true),
                Err(WhichError::Relative),
                "{name:?}"
            );
        }
    }

    #[test]
    fn an_absolute_path_is_used_as_given() {
        let tool = abs("opt").join("tool.exe");
        let found = resolve(tool.as_os_str(), None, ".EXE", |_| false);
        assert_eq!(found, Ok(tool.clone()));
        let found = resolve(
            tool.as_os_str(),
            None,
            ".EXE",
            files(std::slice::from_ref(&tool)),
        );
        assert_eq!(found, Ok(tool));
    }

    #[test]
    fn an_absolute_path_without_an_extension_takes_the_first_that_exists() {
        let ed = abs("opt").join("ed");
        let cmd = abs("opt").join("ed.cmd");
        let found = resolve(
            ed.as_os_str(),
            None,
            ".EXE;.CMD",
            files(std::slice::from_ref(&cmd)),
        );
        assert_eq!(found, Ok(cmd));
        let found = resolve(ed.as_os_str(), None, ".EXE;.CMD", |_| false);
        assert_eq!(found, Ok(ed));
    }
}
