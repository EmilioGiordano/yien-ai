//! The user's default shell, launched in new panes without a profile.

use std::path::Path;

/// A program and its arguments.
///
/// ```
/// let shell = yien_core::ShellCommand::new("pwsh.exe");
/// assert_eq!(shell.program, "pwsh.exe");
/// assert!(shell.args.is_empty());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellCommand {
    /// The program, found through `PATH` when it is not a path.
    pub program: String,
    /// Its arguments.
    pub args: Vec<String>,
}

impl ShellCommand {
    /// A program without arguments.
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
        }
    }

    /// The name a pane shows for it: the file name without extension.
    pub(crate) fn display_name(&self) -> String {
        Path::new(&self.program)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.program.clone())
    }
}

/// The user's shell: `pwsh` when it is on `PATH`, else `powershell`, else
/// `cmd` on Windows; `$SHELL`, else `sh`, elsewhere. Reads the environment and
/// the file system, so call it once, outside the async runtime.
///
/// ```
/// let shell = yien_core::default_shell();
/// assert!(!shell.program.is_empty());
/// ```
pub fn default_shell() -> ShellCommand {
    pick_shell(
        cfg!(windows),
        on_path,
        std::env::var("SHELL").ok().as_deref(),
    )
}

fn pick_shell(
    windows: bool,
    found: impl Fn(&str) -> bool,
    env_shell: Option<&str>,
) -> ShellCommand {
    if windows {
        let program = ["pwsh.exe", "powershell.exe"]
            .into_iter()
            .find(|p| found(p))
            .unwrap_or("cmd.exe");
        return ShellCommand::new(program);
    }
    match env_shell {
        Some(shell) if !shell.trim().is_empty() => ShellCommand::new(shell),
        _ => ShellCommand::new("sh"),
    }
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_prefers_pwsh_then_powershell_then_cmd() {
        assert_eq!(pick_shell(true, |_| true, None).program, "pwsh.exe");
        assert_eq!(
            pick_shell(true, |p| p == "powershell.exe", None).program,
            "powershell.exe"
        );
        assert_eq!(
            pick_shell(true, |_| false, Some("/bin/zsh")).program,
            "cmd.exe"
        );
    }

    #[test]
    fn unix_uses_shell_variable_or_sh() {
        assert_eq!(
            pick_shell(false, |_| true, Some("/bin/zsh")).program,
            "/bin/zsh"
        );
        assert_eq!(pick_shell(false, |_| true, Some("  ")).program, "sh");
        assert_eq!(pick_shell(false, |_| true, None).program, "sh");
    }

    #[test]
    fn display_name_drops_directory_and_extension() {
        assert_eq!(
            ShellCommand::new("C:/Tools/pwsh.exe").display_name(),
            "pwsh"
        );
        assert_eq!(ShellCommand::new("/bin/zsh").display_name(), "zsh");
    }
}
