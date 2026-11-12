//! The user's default shell, launched in new panes without a profile.

use std::path::Path;

/// A program, its arguments and the variables it starts with.
///
/// ```
/// let shell = yien_core::ShellCommand::new("pwsh.exe");
/// assert_eq!(shell.program, "pwsh.exe");
/// assert!(shell.args.is_empty() && shell.env.is_empty());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellCommand {
    /// The program, found through `PATH` when it is not a path.
    pub program: String,
    /// Its arguments.
    pub args: Vec<String>,
    /// Variables set for it, over the ones YIEN inherited.
    pub env: Vec<(String, String)>,
}

/// Wraps the `prompt` the user's profile left so it also reports FileSystem
/// folders as `OSC 9;9`. No `"` (PowerShell re-joins quoted arguments its own
/// way); the user's prompt runs first so it still sees `$?`, and `try` keeps
/// it when constrained language mode refuses the host call.
const POWERSHELL_CWD_HOOK: &str = "if ($function:prompt) { \
    $global:__YienPrompt = $function:prompt; \
    function global:prompt { \
        $p = & $global:__YienPrompt; \
        try { \
            $l = $executionContext.SessionState.Path.CurrentLocation; \
            if ($l.Provider.Name -eq 'FileSystem') { \
                $Host.UI.Write([char]27 + ']9;9;' + [char]34 + $l.ProviderPath + [char]34 + [char]27 + '\\') \
            } \
        } catch {}; \
        $p \
    } \
}";

/// cmd's prompt codes for the same report: `$E` is ESC, `$P` the folder.
const CMD_CWD_PROMPT: &str = "$E]9;9;\"$P\"$E\\";

/// cmd's own prompt when `PROMPT` is not set.
const CMD_DEFAULT_PROMPT: &str = "$P$G";

impl ShellCommand {
    /// A program without arguments.
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
        }
    }

    /// The name a pane shows for it: the file name without extension.
    pub(crate) fn display_name(&self) -> String {
        Path::new(&self.program)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.program.clone())
    }

    /// The same shell, set up to report its folder at every prompt, so a
    /// pane follows `cd` (RF-SES-01). PowerShell (5.1 and 7) starts with a
    /// command that wraps the prompt its profile left; the profile still
    /// loads. cmd gets the report in front of `inherited_prompt`, the
    /// `PROMPT` it would have used. A shell with arguments of its own, or
    /// any other shell, is returned unchanged. Nothing is written to disk.
    ///
    /// ```
    /// use yien_core::ShellCommand;
    ///
    /// let pwsh = ShellCommand::new("pwsh.exe").reporting_cwd(None);
    /// assert_eq!(&pwsh.args[..3], ["-NoLogo", "-NoExit", "-Command"]);
    ///
    /// let cmd = ShellCommand::new("cmd.exe").reporting_cwd(Some("$G"));
    /// assert_eq!(cmd.env[0].0, "PROMPT");
    /// assert!(cmd.env[0].1.ends_with("$G"));
    /// ```
    pub fn reporting_cwd(mut self, inherited_prompt: Option<&str>) -> Self {
        if !self.args.is_empty() {
            return self;
        }
        match self.display_name().to_ascii_lowercase().as_str() {
            "powershell" | "pwsh" => {
                self.args = ["-NoLogo", "-NoExit", "-Command", POWERSHELL_CWD_HOOK]
                    .map(str::to_owned)
                    .to_vec();
            }
            "cmd" => {
                let own = inherited_prompt
                    .filter(|p| !p.is_empty())
                    .unwrap_or(CMD_DEFAULT_PROMPT);
                self.env
                    .push(("PROMPT".to_owned(), format!("{CMD_CWD_PROMPT}{own}")));
            }
            _ => {}
        }
        self
    }
}

/// The user's shell: `pwsh` when it is on `PATH`, else `powershell`, else
/// `cmd` on Windows; `$SHELL`, else `sh`, elsewhere. With `report_cwd` it
/// reports its folder at each prompt ([`ShellCommand::reporting_cwd`]).
/// Reads the environment and the file system, so call it once, outside the
/// async runtime.
///
/// ```
/// let shell = yien_core::default_shell(false);
/// assert!(!shell.program.is_empty());
/// ```
pub fn default_shell(report_cwd: bool) -> ShellCommand {
    let shell = pick_shell(
        cfg!(windows),
        on_path,
        std::env::var("SHELL").ok().as_deref(),
    );
    if report_cwd {
        shell.reporting_cwd(std::env::var("PROMPT").ok().as_deref())
    } else {
        shell
    }
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

/// Whether the pane spawn will find `program`: only absolute `PATH` folders
/// count, as in `yien-pty`.
fn on_path(program: &str) -> bool {
    let path = std::env::var_os("PATH");
    yien_which::resolve(program.as_ref(), path.as_deref(), "", Path::is_file).is_ok()
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

    #[test]
    fn rf_ses_01_powershell_starts_with_the_prompt_hook() {
        for program in [
            "pwsh.exe",
            "powershell.exe",
            "PowerShell.EXE",
            r"C:\Program Files\PowerShell\7\pwsh.exe",
            "C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe",
        ] {
            let shell = ShellCommand::new(program).reporting_cwd(Some("$G"));
            assert_eq!(shell.program, program);
            assert_eq!(
                shell.args,
                ["-NoLogo", "-NoExit", "-Command", POWERSHELL_CWD_HOOK],
                "{program}"
            );
            assert!(shell.env.is_empty(), "{program}");
        }
    }

    #[test]
    fn rf_ses_01_the_hook_survives_windows_argument_quoting() {
        // A `"` would need escaping that PowerShell re-joins differently;
        // a line break or NUL cannot travel in one argument.
        assert!(!POWERSHELL_CWD_HOOK.contains(['"', '\n', '\r', '\0']));
        // No trailing backslash, which would escape the closing quote.
        assert!(!POWERSHELL_CWD_HOOK.ends_with('\\'));
        assert!(POWERSHELL_CWD_HOOK.contains("'FileSystem'"));
        assert!(POWERSHELL_CWD_HOOK.contains("']9;9;'"));
    }

    #[test]
    fn rf_ses_01_cmd_reports_in_front_of_its_own_prompt() {
        let cmd = ShellCommand::new("cmd.exe").reporting_cwd(None);
        assert!(cmd.args.is_empty());
        assert_eq!(
            cmd.env,
            [("PROMPT".to_owned(), "$E]9;9;\"$P\"$E\\$P$G".to_owned())]
        );
        let custom = ShellCommand::new("C:/Windows/System32/CMD.EXE")
            .reporting_cwd(Some("$T $P [it's \"mine\"]$G"));
        assert_eq!(custom.env[0].1, "$E]9;9;\"$P\"$E\\$T $P [it's \"mine\"]$G");
        let empty = ShellCommand::new("cmd.exe").reporting_cwd(Some(""));
        assert!(empty.env[0].1.ends_with("$E\\$P$G"));
    }

    #[test]
    fn a_shell_with_its_own_arguments_or_another_shell_is_unchanged() {
        let mut own = ShellCommand::new("pwsh.exe");
        own.args = vec!["-NoProfile".into()];
        assert_eq!(own.clone().reporting_cwd(None), own);
        for program in ["sh", "/bin/zsh", "nu.exe", "bash.exe"] {
            let shell = ShellCommand::new(program);
            assert_eq!(shell.clone().reporting_cwd(Some("$P")), shell);
        }
    }
}

/// Real Windows PowerShell 5.1 through ConPTY, as panes run it.
#[cfg(all(test, windows))]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers outside #[test] fns are not covered by clippy.toml's in-test allowances"
)]
mod live {
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::{Duration, Instant};

    use yien_pty::{PortablePty, PtyBackend, PtyEvent, SpawnSpec};
    use yien_vt::{AlacrittyEngine, VtConfig, VtEngine};

    use super::POWERSHELL_CWD_HOOK;

    /// A prompt that shows the last command's `$?` and `$LASTEXITCODE`.
    const FIXED_PROMPT: &str = "function global:prompt { 'P[' + $? + '|' + $LASTEXITCODE + ']> ' }";

    struct Run {
        raw: Vec<u8>,
        screen: Vec<String>,
    }

    fn count(raw: &[u8], needle: &str) -> usize {
        String::from_utf8_lossy(raw).matches(needle).count()
    }

    /// Runs each command once the one before it has drawn its prompt, waits
    /// for the output to settle, and returns the bytes and the screen.
    fn run(script: &str, commands: &[(&str, &str)]) -> Run {
        let spec = SpawnSpec::new("powershell.exe")
            .args(["-NoLogo", "-NoProfile", "-NoExit", "-Command", script])
            .cwd(r"C:\Windows")
            .size(100, 30);
        let session = PortablePty::new().spawn(spec).unwrap();
        let mut engine = AlacrittyEngine::new(VtConfig::default(), 100, 30);
        let mut raw = Vec::new();
        let mut pump = |raw: &mut Vec<u8>, until: &dyn Fn(&[u8]) -> bool, quiet: bool| {
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                let wait = Duration::from_millis(if quiet { 500 } else { 50 });
                match session.events().recv_timeout(wait) {
                    Ok(PtyEvent::Output(bytes)) => {
                        raw.extend_from_slice(&bytes);
                        let out = engine.feed(&bytes);
                        if !out.replies.is_empty() {
                            session.write(out.replies).unwrap();
                        }
                    }
                    Ok(PtyEvent::Exited(status)) => panic!("powershell exited: {status:?}"),
                    Err(RecvTimeoutError::Timeout) if quiet => return,
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => panic!("the session closed"),
                }
                if !quiet && until(raw) {
                    return;
                }
                let seen = String::from_utf8_lossy(raw).into_owned();
                assert!(Instant::now() < deadline, "timed out; output: {seen:?}");
            }
        };
        pump(&mut raw, &|raw| count(raw, "P[True|]> ") > 0, false);
        for (command, prompt) in commands {
            let before = count(&raw, prompt);
            session.write(format!("{command}\r").into_bytes()).unwrap();
            pump(&mut raw, &|raw| count(raw, prompt) > before, false);
        }
        pump(&mut raw, &|_| true, true);
        let _ = session.control().kill();
        let screen = engine
            .frame()
            .lines
            .iter()
            .map(|row| {
                let text: String = row.iter().map(|c| c.ch).collect();
                text.trim_end().to_owned()
            })
            .collect();
        Run { raw, screen }
    }

    #[test]
    fn rf_ses_01_the_hook_leaves_the_users_prompt_as_it_was() {
        let commands = [
            ("Get-Item nope-yien-missing", "P[False|]> "),
            ("cmd /c exit 3", "P[False|3]> "),
            ("cd HKLM:\\", "P[True|3]> "),
            ("cd C:\\", "P[True|3]> "),
        ];
        let plain = run(FIXED_PROMPT, &commands);
        let hooked = run(&format!("{FIXED_PROMPT}; {POWERSHELL_CWD_HOOK}"), &commands);
        assert_eq!(hooked.screen, plain.screen);
        let reports = |raw: &[u8]| -> Vec<String> {
            let text = String::from_utf8_lossy(raw).into_owned();
            text.split("\x1b]9;9;")
                .skip(1)
                .map(|s| s.split("\x1b\\").next().unwrap_or("").to_owned())
                .collect()
        };
        assert!(reports(&plain.raw).is_empty());
        // One per prompt, none while the location is in the registry.
        assert_eq!(
            reports(&hooked.raw),
            [
                r#""C:\Windows""#,
                r#""C:\Windows""#,
                r#""C:\Windows""#,
                r#""C:\""#
            ]
        );
    }
}
