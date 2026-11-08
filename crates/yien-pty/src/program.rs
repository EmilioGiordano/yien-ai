//! Finds the program to spawn the way a shell would, through
//! [`yien_which::resolve`]: a bare name is looked up only in absolute `PATH`
//! folders, on Windows with each `PATHEXT` extension, so `claude` finds
//! `claude.exe` and `opencode` finds `opencode.cmd` (RF-AGE-02).
//!
//! portable-pty always gets the absolute path of a file that exists. Its own
//! lookup picks npm's extensionless shell script first, which Windows cannot
//! start; it walks relative `PATH` entries, falls back to the bare name, which
//! `CreateProcessW` resolves against YIEN's working folder, and swaps the
//! extension of a missing absolute path for one that exists.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use yien_which::WhichError;

use crate::{PtyError, SpawnSpec};

/// Characters `cmd.exe` interprets inside a batch file's arguments. Passing
/// them to a `.cmd` or `.bat` could run commands (CVE-2024-24576).
const BATCH_UNSAFE: &[char] = &['"', '%', '!', '^', '&', '|', '<', '>', '(', ')', '\n', '\r'];

/// Why a program cannot be started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProgramError {
    /// A batch file would receive an argument `cmd.exe` interprets.
    UnsafeBatchArgument {
        /// The batch file.
        program: String,
    },
    /// A bare name found in no absolute `PATH` folder.
    NotFound {
        /// The name as given.
        program: String,
    },
    /// A relative path with folders in it.
    Relative {
        /// The path as given.
        program: String,
    },
    /// An absolute path that names no file.
    Missing {
        /// The path as given.
        program: String,
    },
    /// No program at all.
    Empty,
}

impl From<ProgramError> for PtyError {
    fn from(e: ProgramError) -> Self {
        match e {
            ProgramError::UnsafeBatchArgument { program } => {
                PtyError::UnsafeBatchArgument { program }
            }
            ProgramError::NotFound { program } => PtyError::NotFound { program },
            ProgramError::Relative { program } => PtyError::RelativeProgram { program },
            ProgramError::Missing { program } => PtyError::MissingProgram { program },
            ProgramError::Empty => PtyError::EmptyProgram,
        }
    }
}

/// The spec with its program replaced by the absolute path to run. `env`
/// gives the child's variables: portable-pty's builder, which on Windows
/// takes `PATH` from the registry rather than from YIEN's own environment.
/// Reads the disk.
pub(crate) fn resolve(
    spec: SpawnSpec,
    env: impl Fn(&str) -> Option<OsString>,
) -> Result<SpawnSpec, ProgramError> {
    let pathext = if cfg!(windows) {
        env("PATHEXT")
            .and_then(|e| e.into_string().ok())
            .unwrap_or_else(|| yien_which::DEFAULT_PATHEXT.into())
    } else {
        String::new()
    };
    let path = env("PATH");
    let found = yien_which::resolve(&spec.program, path.as_deref(), &pathext, runnable);
    with_program(spec, found)
}

/// Whether `path` is a file the OS can start; on Unix, one with an execute
/// bit, as `which` checks.
fn runnable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

fn with_program(
    mut spec: SpawnSpec,
    found: Result<PathBuf, WhichError>,
) -> Result<SpawnSpec, ProgramError> {
    let program = spec.program.to_string_lossy().into_owned();
    let found = match found {
        Ok(found) => found,
        Err(WhichError::Relative) => return Err(ProgramError::Relative { program }),
        Err(WhichError::NotFound) => return Err(ProgramError::NotFound { program }),
        Err(WhichError::Missing) => return Err(ProgramError::Missing { program }),
        Err(WhichError::Empty) => return Err(ProgramError::Empty),
    };
    if cfg!(windows) && is_batch(&found) && spec.args.iter().any(|a| batch_unsafe(a)) {
        return Err(ProgramError::UnsafeBatchArgument {
            program: found.to_string_lossy().into_owned(),
        });
    }
    spec.program = found.into_os_string();
    Ok(spec)
}

/// Whether Windows runs `path` through cmd.exe. Windows drops trailing dots
/// and spaces from a file name, so `agent.cmd.` is `agent.cmd`.
fn is_batch(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let name = name.trim_end_matches(['.', ' ']);
    name.ends_with(".cmd") || name.ends_with(".bat")
}

pub(crate) fn batch_unsafe(arg: &OsStr) -> bool {
    arg.to_string_lossy().contains(BATCH_UNSAFE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("yien-pty-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::path::absolute(dir).unwrap()
    }

    #[cfg(windows)]
    #[test]
    fn a_batch_file_refuses_arguments_cmd_would_interpret() {
        let spec = SpawnSpec::new("opencode").args(["--settings", "C:/a b/x.json"]);
        let ok = with_program(spec, Ok(PathBuf::from("C:/npm/opencode.cmd"))).unwrap();
        assert_eq!(ok.program, OsString::from("C:/npm/opencode.cmd"));

        let spec = SpawnSpec::new("opencode").arg("a&calc");
        let err = with_program(spec, Ok(PathBuf::from("C:/npm/opencode.CMD"))).unwrap_err();
        assert!(matches!(err, ProgramError::UnsafeBatchArgument { .. }));

        let spec = SpawnSpec::new("claude").arg("a&b");
        let exe = with_program(spec, Ok(PathBuf::from("C:/bin/claude.exe"))).unwrap();
        assert_eq!(exe.program, OsString::from("C:/bin/claude.exe"));
    }

    #[test]
    fn trailing_dots_and_spaces_do_not_hide_a_batch_file() {
        for name in ["agent.cmd.", "agent.CMD ", "agent.bat. .", "AGENT.Bat"] {
            assert!(is_batch(Path::new(name)), "{name:?}");
        }
        for name in ["agent.exe", "agent.cmdx", "agent", "agent.exe."] {
            assert!(!is_batch(Path::new(name)), "{name:?}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn an_absolute_batch_file_from_a_profile_is_checked_too() {
        let dir = temp_dir("abs-batch");
        let cmd = dir.join("agent.cmd");
        std::fs::write(&cmd, "").unwrap();
        let spec = SpawnSpec::new(cmd.as_os_str()).arg("a&calc");
        let found = yien_which::resolve(&spec.program, None, ".EXE;.CMD", runnable);
        assert_eq!(found, Ok(cmd.clone()));
        let err = with_program(spec, found).unwrap_err();
        assert!(matches!(err, ProgramError::UnsafeBatchArgument { .. }));
        // Such a name has no PATHEXT extension, so it is never found as is.
        for given in [dir.join("agent.cmd."), dir.join("agent.CMD ")] {
            let spec = SpawnSpec::new(given.as_os_str()).arg("a&calc");
            let err = resolve(spec, |_| None).unwrap_err();
            assert!(matches!(err, ProgramError::Missing { .. }), "{given:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_program_not_found_is_an_error_not_a_bare_name() {
        let err = with_program(SpawnSpec::new("claude"), Err(WhichError::NotFound)).unwrap_err();
        assert_eq!(
            err,
            ProgramError::NotFound {
                program: "claude".into()
            }
        );
        let pty: PtyError = err.into();
        assert_eq!(
            pty.to_string(),
            "claude was not found on PATH (the current folder is not searched)"
        );
        let err = with_program(SpawnSpec::new(""), Err(WhichError::Empty)).unwrap_err();
        assert_eq!(err, ProgramError::Empty);
        assert_eq!(PtyError::from(err).to_string(), "no program was given");
    }

    #[test]
    fn a_relative_path_with_folders_is_refused() {
        let spec = SpawnSpec::new(Path::new("bin").join("claude").as_os_str());
        let found = yien_which::resolve(&spec.program, None, "", |_| true);
        let err = with_program(spec, found).unwrap_err();
        assert!(matches!(err, ProgramError::Relative { .. }));
        assert!(matches!(
            PtyError::from(err),
            PtyError::RelativeProgram { .. }
        ));
    }

    #[test]
    fn a_missing_absolute_program_is_refused_not_swapped() {
        let dir = temp_dir("absolute");
        let missing = dir.join("agent.exe");
        std::fs::write(dir.join("agent.cmd"), "").unwrap();
        let spec = SpawnSpec::new(missing.as_os_str());
        let err = resolve(spec, |_| None).unwrap_err();
        assert!(matches!(err, ProgramError::Missing { .. }), "{err:?}");
        assert!(matches!(
            PtyError::from(err),
            PtyError::MissingProgram { .. }
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `cargo test` runs in the crate folder, which holds `Cargo.toml`.
    #[test]
    fn a_file_in_the_current_folder_is_never_chosen() {
        assert!(Path::new("Cargo.toml").is_file());
        for dirs in [vec![PathBuf::from(".")], vec![PathBuf::new()]] {
            let path = std::env::join_paths(&dirs).unwrap();
            let is_file = |p: &Path| p.is_file();
            let found = yien_which::resolve(OsStr::new("Cargo"), Some(&path), ".TOML", is_file);
            assert_eq!(found, Err(WhichError::NotFound), "{dirs:?}");
            let found = yien_which::resolve(OsStr::new("Cargo.toml"), Some(&path), "", is_file);
            assert_eq!(found, Err(WhichError::NotFound), "{dirs:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn on_unix_only_files_with_an_execute_bit_are_runnable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("exec-bit");
        let tool = dir.join("tool");
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o644)).unwrap();
        let path = OsString::from(dir.as_os_str());
        let find = || yien_which::resolve(OsStr::new("tool"), Some(&path), "", runnable);
        assert_eq!(find(), Err(WhichError::NotFound));
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(find(), Ok(tool));
        assert!(!runnable(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
