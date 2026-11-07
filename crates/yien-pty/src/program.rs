//! Finds the program to spawn the way a shell would: on Windows a bare name
//! is looked up on `PATH` with each `PATHEXT` extension, so `claude` finds
//! `claude.exe` and `opencode` finds `opencode.cmd` (RF-AGE-02).
//! portable-pty's own lookup picks npm's extensionless shell script first,
//! which Windows cannot start.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use crate::SpawnSpec;

/// Extensions tried when `PATHEXT` is unset.
const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

/// Characters `cmd.exe` interprets inside a batch file's arguments. Passing
/// them to a `.cmd` or `.bat` could run commands (CVE-2024-24576).
const BATCH_UNSAFE: &[char] = &['"', '%', '!', '^', '&', '|', '<', '>', '(', ')', '\n', '\r'];

/// Why a program cannot be started.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ProgramError {
    /// A batch file would receive an argument `cmd.exe` interprets.
    #[error(
        "{program} is a batch file and one of its arguments has a character cmd.exe would interpret"
    )]
    UnsafeBatchArgument {
        /// The batch file.
        program: String,
    },
}

/// The spec with its program replaced by the full path a shell would run.
/// A program that is a path, or that is not found, is left as it is; the
/// spawn then reports it missing. Reads the environment and the disk.
pub(crate) fn resolve(spec: SpawnSpec) -> Result<SpawnSpec, ProgramError> {
    if !cfg!(windows) {
        return Ok(spec);
    }
    let found = find(
        &spec.program,
        std::env::var_os("PATH").as_deref(),
        std::env::var_os("PATHEXT").as_deref(),
        |p| p.is_file(),
    );
    with_program(spec, found)
}

impl ProgramError {
    /// The program it is about.
    pub(crate) fn program(&self) -> &str {
        match self {
            Self::UnsafeBatchArgument { program } => program,
        }
    }
}

fn with_program(mut spec: SpawnSpec, found: Option<PathBuf>) -> Result<SpawnSpec, ProgramError> {
    let Some(found) = found else {
        return Ok(spec);
    };
    if is_batch(&found) && spec.args.iter().any(|a| batch_unsafe(a)) {
        return Err(ProgramError::UnsafeBatchArgument {
            program: found.to_string_lossy().into_owned(),
        });
    }
    spec.program = found.into_os_string();
    Ok(spec)
}

/// The file `program` names on `path`, trying each `pathext` extension.
/// Only bare names are looked up. A file without one of those extensions is
/// never picked: npm puts an extensionless shell script next to its `.cmd`
/// shim, and Windows cannot start it.
fn find(
    program: &OsStr,
    path: Option<&OsStr>,
    pathext: Option<&OsStr>,
    is_file: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let name = Path::new(program);
    if program.is_empty() || name.components().count() != 1 || name.has_root() {
        return None;
    }
    let exts: Vec<String> = pathext
        .and_then(OsStr::to_str)
        .unwrap_or(DEFAULT_PATHEXT)
        .split(';')
        .map(|e| e.trim().to_ascii_lowercase())
        .filter(|e| e.starts_with('.') && e.len() > 1)
        .collect();
    let has_ext = name
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|e| exts.contains(&format!(".{}", e.to_ascii_lowercase())));
    for dir in std::env::split_paths(path?) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        if has_ext {
            let candidate = dir.join(name);
            if is_file(&candidate) {
                return Some(candidate);
            }
            continue;
        }
        for ext in &exts {
            let mut file = OsString::from(program);
            file.push(ext);
            let candidate = dir.join(file);
            if is_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

fn is_batch(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
}

fn batch_unsafe(arg: &OsStr) -> bool {
    arg.to_string_lossy().contains(BATCH_UNSAFE)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Drive letters and `;` separators make these paths Windows only.
    #[cfg(windows)]
    mod lookup {
        use std::collections::HashSet;

        use super::*;

        fn files(list: &[&str]) -> impl Fn(&Path) -> bool {
            let set: HashSet<PathBuf> = list.iter().map(PathBuf::from).collect();
            move |p| set.contains(p)
        }

        fn path(dirs: &[&str]) -> OsString {
            std::env::join_paths(dirs).unwrap()
        }

        #[test]
        fn rf_age_02_a_bare_name_finds_the_exe_on_path() {
            let path = path(&["C:/Tools", "C:/Users/u/.local/bin"]);
            let found = find(
                OsStr::new("claude"),
                Some(&path),
                Some(OsStr::new(".COM;.EXE;.BAT;.CMD")),
                files(&["C:/Users/u/.local/bin/claude.exe"]),
            );
            assert_eq!(
                found,
                Some(PathBuf::from("C:/Users/u/.local/bin").join("claude.exe"))
            );
        }

        #[test]
        fn rf_age_02_an_npm_shim_finds_the_cmd_not_the_shell_script() {
            let path = path(&["C:/npm"]);
            let found = find(
                OsStr::new("opencode"),
                Some(&path),
                Some(OsStr::new(".EXE;.CMD")),
                files(&["C:/npm/opencode", "C:/npm/opencode.cmd"]),
            );
            assert_eq!(found, Some(PathBuf::from("C:/npm").join("opencode.cmd")));
        }

        #[test]
        fn path_order_and_extension_order_decide() {
            let path = path(&["C:/a", "C:/b"]);
            let exists = files(&["C:/a/x.cmd", "C:/a/x.exe", "C:/b/x.exe"]);
            let found = find(
                OsStr::new("x"),
                Some(&path),
                Some(OsStr::new(".EXE;.CMD")),
                exists,
            );
            assert_eq!(found, Some(PathBuf::from("C:/a").join("x.exe")));
        }

        #[test]
        fn a_name_with_its_extension_is_looked_up_as_is() {
            let path = path(&["C:/w"]);
            let found = find(
                OsStr::new("pwsh.exe"),
                Some(&path),
                None,
                files(&["C:/w/pwsh.exe", "C:/w/pwsh.exe.exe"]),
            );
            assert_eq!(found, Some(PathBuf::from("C:/w").join("pwsh.exe")));
        }

        #[test]
        fn paths_and_missing_programs_are_left_alone() {
            let path = path(&["C:/w"]);
            let any = |_: &Path| true;
            assert_eq!(
                find(OsStr::new("C:/x/claude.exe"), Some(&path), None, any),
                None
            );
            assert_eq!(find(OsStr::new("sub/claude"), Some(&path), None, any), None);
            assert_eq!(find(OsStr::new(""), Some(&path), None, any), None);
            assert_eq!(find(OsStr::new("claude"), None, None, any), None);
            assert_eq!(
                find(OsStr::new("claude"), Some(&path), None, files(&[])),
                None
            );
        }
    }

    #[test]
    fn a_batch_file_refuses_arguments_cmd_would_interpret() {
        let spec = SpawnSpec::new("opencode").args(["--settings", "C:/a b/x.json"]);
        let ok = with_program(spec, Some(PathBuf::from("C:/npm/opencode.cmd"))).unwrap();
        assert_eq!(ok.program, OsString::from("C:/npm/opencode.cmd"));

        let spec = SpawnSpec::new("opencode").arg("a&calc");
        let err = with_program(spec, Some(PathBuf::from("C:/npm/opencode.CMD"))).unwrap_err();
        assert!(matches!(err, ProgramError::UnsafeBatchArgument { .. }));

        let spec = SpawnSpec::new("claude").arg("a&b");
        let exe = with_program(spec, Some(PathBuf::from("C:/bin/claude.exe"))).unwrap();
        assert_eq!(exe.program, OsString::from("C:/bin/claude.exe"));
    }

    #[test]
    fn a_program_not_found_keeps_its_name() {
        let spec = with_program(SpawnSpec::new("claude"), None).unwrap();
        assert_eq!(spec.program, OsString::from("claude"));
    }
}
