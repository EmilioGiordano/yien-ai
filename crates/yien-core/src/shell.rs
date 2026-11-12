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
/// folders as `OSC 9;9`, and PSReadLine's line reader, for OSC 133 marks.
/// No `"` (PowerShell re-joins quoted arguments its own way). The user's
/// prompt and PSReadLine run first: any statement before them resets `$?`.
/// Constrained language mode refuses method calls, so it gets nothing; any
/// other failure takes its error back out of `$Error`. Wrapping a wrapper
/// of PSReadLine could call itself. Only a line read after YIEN's prompt
/// gets a `C`: a prompt replaced later would never write the `D` that ends
/// it. `exit` with a `C` would show an error.
const POWERSHELL_HOOK: &str = "if ($function:prompt) { \
    $global:__YienPrompt = $function:prompt; \
    $global:__YienRan = $false; \
    $global:__YienArmed = $false; \
    function global:prompt { \
        try { \
            $s, $p = $?, (& $global:__YienPrompt); \
            $global:__YienArmed = $true \
        } finally { \
            try { \
                if ($executionContext.SessionState.LanguageMode -eq 'FullLanguage') { \
                    $e = [char]27; \
                    $o = ''; \
                    $v = Get-Variable s -Scope 0 -ErrorAction Ignore; \
                    if ($global:__YienRan) { \
                        $global:__YienRan = $false; \
                        $o = $e + ']133;D;' + [int](-not ($v -and $s)) + $e + '\\' \
                    }; \
                    $o += $e + ']133;A' + $e + '\\'; \
                    $l = $executionContext.SessionState.Path.CurrentLocation; \
                    if ($l.Provider.Name -eq 'FileSystem') { \
                        $o += $e + ']9;9;' + [char]34 + $l.ProviderPath + [char]34 + $e + '\\' \
                    }; \
                    $Host.UI.Write($o); \
                    if ($v -and $p -is [string]) { $p += $e + ']133;B' + $e + '\\' }; \
                    $r = $function:PSConsoleHostReadLine; \
                    if ($r -match 'PSConsoleReadLine\\]::ReadLine') { \
                        $global:__YienReadLine = $r; \
                        function global:PSConsoleHostReadLine { \
                            $c = & $global:__YienReadLine; \
                            try { \
                                if ($global:__YienArmed) { \
                                    $global:__YienArmed = $false; \
                                    if ($c -match '\\S' -and $c -notmatch '^\\s*exit(\\s|;|\\(|$)') { \
                                        $global:__YienRan = $true; \
                                        $Host.UI.Write([char]27 + ']133;C' + [char]27 + '\\') \
                                    } \
                                } elseif ($global:__YienRan) { \
                                    $global:__YienRan = $false; \
                                    $Host.UI.Write([char]27 + ']133;D' + [char]27 + '\\') \
                                } \
                            } catch { \
                                if ($global:Error.Count -gt 0 -and $global:Error[0].Exception -eq $_.Exception) { \
                                    $global:Error.RemoveAt(0) \
                                } \
                            }; \
                            $c \
                        } \
                    } \
                } \
            } catch { \
                if ($global:Error.Count -gt 0 -and $global:Error[0].Exception -eq $_.Exception) { \
                    $global:Error.RemoveAt(0) \
                } \
            } \
        }; \
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
    /// loads. PowerShell also marks each command's start and end with
    /// OSC 133, so its pane is working while one runs (RF-AGE-05); cmd
    /// cannot tell a command starts. cmd gets the report in front of
    /// `inherited_prompt`, the `PROMPT` it would have used. A shell with arguments of its own, or
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
                self.args = ["-NoLogo", "-NoExit", "-Command", POWERSHELL_HOOK]
                    .map(str::to_owned)
                    .to_vec();
            }
            "cmd" => {
                let own = inherited_prompt
                    .filter(|p| !p.is_empty())
                    .unwrap_or(CMD_DEFAULT_PROMPT);
                // A YIEN started from a YIEN pane inherits the report already.
                let prompt = if own.starts_with(CMD_CWD_PROMPT) {
                    own.to_owned()
                } else {
                    format!("{CMD_CWD_PROMPT}{own}")
                };
                self.env.push(("PROMPT".to_owned(), prompt));
            }
            _ => {}
        }
        self
    }
}

/// The user's shell: `pwsh` when it is on `PATH`, else `powershell`, else
/// `cmd` on Windows; `$SHELL`, else `sh`, elsewhere. Reads the environment
/// and the file system, so call it once, outside the async runtime.
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

/// The shell as configured and the same shell reporting its folder, so the
/// report can be switched for panes started later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShellChoice {
    plain: ShellCommand,
    reporting: ShellCommand,
}

impl ShellChoice {
    /// Both forms of `shell`; cmd's report goes in front of
    /// `inherited_prompt`.
    pub fn new(shell: &ShellCommand, inherited_prompt: Option<&str>) -> Self {
        Self {
            plain: shell.clone(),
            reporting: shell.clone().reporting_cwd(inherited_prompt),
        }
    }

    /// The shell new panes run.
    pub fn pick(&self, report_cwd: bool) -> &ShellCommand {
        if report_cwd {
            &self.reporting
        } else {
            &self.plain
        }
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
        let mut programs = vec![
            "pwsh.exe",
            "powershell.exe",
            "PowerShell.EXE",
            "C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe",
        ];
        // A backslash separates path parts only on Windows.
        if cfg!(windows) {
            programs.push(r"C:\Program Files\PowerShell\7\pwsh.exe");
        }
        for program in programs {
            let shell = ShellCommand::new(program).reporting_cwd(Some("$G"));
            assert_eq!(shell.program, program);
            assert_eq!(
                shell.args,
                ["-NoLogo", "-NoExit", "-Command", POWERSHELL_HOOK],
                "{program}"
            );
            assert!(shell.env.is_empty(), "{program}");
        }
    }

    #[test]
    fn rf_ses_01_the_hook_survives_windows_argument_quoting() {
        // A `"` would need escaping that PowerShell re-joins differently;
        // a line break or NUL cannot travel in one argument.
        assert!(!POWERSHELL_HOOK.contains(['"', '\n', '\r', '\0']));
        // No trailing backslash, which would escape the closing quote.
        assert!(!POWERSHELL_HOOK.ends_with('\\'));
        assert!(POWERSHELL_HOOK.contains("'FileSystem'"));
        assert!(POWERSHELL_HOOK.contains("']9;9;'"));
    }

    #[test]
    fn rf_age_05_the_powershell_hook_marks_prompts_and_commands() {
        // `exit(3)` ends the shell as `exit 3` does.
        assert!(POWERSHELL_HOOK.contains("'^\\s*exit(\\s|;|\\(|$)'"));
        for mark in ["']133;D;'", "']133;A'", "']133;B'", "']133;C'"] {
            assert_eq!(POWERSHELL_HOOK.matches(mark).count(), 1, "{mark}");
        }
        // It still wraps the prompt, calling it before anything resets `$?`.
        assert!(POWERSHELL_HOOK.contains("function global:prompt"));
        assert!(POWERSHELL_HOOK.contains("$s, $p = $?, (& $global:__YienPrompt);"));
        // `C` comes from PSReadLine's own line reader, called first.
        assert!(POWERSHELL_HOOK.contains("function global:PSConsoleHostReadLine"));
        assert!(POWERSHELL_HOOK.contains("{ $c = & $global:__YienReadLine;"));
        assert!(POWERSHELL_HOOK.contains("'PSConsoleReadLine\\]::ReadLine'"));
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
    fn rf_ses_01_a_nested_cmd_gets_the_report_once() {
        let outer = ShellCommand::new("cmd.exe").reporting_cwd(Some("$T$G"));
        let inherited = outer.env[0].1.clone();
        let inner = ShellCommand::new("cmd.exe").reporting_cwd(Some(&inherited));
        assert_eq!(inner.env[0].1, inherited);
        assert_eq!(inherited.matches("]9;9;").count(), 1);
    }

    #[test]
    fn rf_ses_01_the_choice_switches_between_plain_and_reporting() {
        let choice = ShellChoice::new(&ShellCommand::new("pwsh.exe"), None);
        assert!(choice.pick(false).args.is_empty());
        assert_eq!(choice.pick(true).args[2], "-Command");
        let other = ShellChoice::new(&ShellCommand::new("sh"), None);
        assert_eq!(other.pick(true), other.pick(false));
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

    use yien_agent::{AgentState, Effect, Machine, MachineConfig, Signal, Source};
    use yien_pty::{PortablePty, PtyBackend, PtyEvent, SpawnSpec};
    use yien_vt::osc_scan::OscScanner;
    use yien_vt::{AlacrittyEngine, VtConfig, VtEngine};

    use super::POWERSHELL_HOOK;

    /// Keeps the typed test commands out of the developer's own history file.
    const NO_HISTORY: &str =
        "try { Set-PSReadLineOption -HistorySaveStyle SaveNothing -ErrorAction Stop } catch {}";

    /// A prompt that returns the last command's `$?`, `$LASTEXITCODE` and the
    /// number of errors in `$Error`.
    const FIXED_PROMPT: &str = "function global:prompt { \
        'P[' + $? + '|' + $LASTEXITCODE + ']E' + $global:Error.Count + '> ' }";

    /// A prompt written as profiles often do: it reads `$?` and
    /// `$LASTEXITCODE` first, draws with `Write-Host` and returns a space.
    const HOST_PROMPT: &str = "function global:prompt { \
        $ok = $?; $code = $LASTEXITCODE; \
        Write-Host ('S[' + $ok + '|' + $code + ']E' + $global:Error.Count + '>') \
            -NoNewline -ForegroundColor Green; \
        ' ' }";

    /// A prompt with OSC 133 marks of its own, as oh-my-posh or Windows
    /// Terminal's setup writes them: `D` and `A` before it, `B` after.
    const MARKED_PROMPT: &str = "function global:prompt { \
        $ok = $?; $code = $LASTEXITCODE; \
        [char]27 + ']133;D' + [char]7 + [char]27 + ']133;A' + [char]7 + \
        'O[' + $ok + '|' + $code + ']E' + $global:Error.Count + '> ' + \
        [char]27 + ']133;B' + [char]7 }";

    struct Run {
        raw: Vec<u8>,
        screen: Vec<String>,
    }

    fn count(raw: &[u8], needle: &str) -> usize {
        String::from_utf8_lossy(raw).matches(needle).count()
    }

    /// Runs `prompt`, with the hook when `hooked`, then each command once the
    /// one before it has drawn `first` or its own prompt; waits for the output
    /// to settle and returns the bytes and the screen. With no commands
    /// nothing is typed, so PSReadLine is left to load as in a real pane.
    fn run(prompt: &str, hooked: bool, first: &str, commands: &[(&str, &str)]) -> Run {
        let mut script = if commands.is_empty() {
            prompt.to_owned()
        } else {
            format!("{NO_HISTORY}; {prompt}")
        };
        if hooked {
            script = format!("{script}; {POWERSHELL_HOOK}");
        }
        let spec = SpawnSpec::new("powershell.exe")
            .args(["-NoLogo", "-NoProfile", "-NoExit", "-Command", &script])
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
        pump(&mut raw, &|raw| count(raw, first) > 0, false);
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

    fn reports(raw: &[u8]) -> Vec<String> {
        let text = String::from_utf8_lossy(raw).into_owned();
        text.split("\x1b]9;9;")
            .skip(1)
            .map(|s| s.split("\x1b\\").next().unwrap_or("").to_owned())
            .collect()
    }

    /// How many OSC 133 marks of `kind` (`A`, `B`, `C` or `D`) `raw` holds.
    fn marks(raw: &[u8], kind: char) -> usize {
        count(raw, &format!("\x1b]133;{kind}"))
    }

    /// The exit codes of the `D` marks that carry one, in order.
    fn finished_codes(raw: &[u8]) -> Vec<String> {
        let text = String::from_utf8_lossy(raw).into_owned();
        text.split("\x1b]133;D;")
            .skip(1)
            .map(|s| s.split(['\x1b', '\x07']).next().unwrap_or("").to_owned())
            .collect()
    }

    /// The states a seen shell pane goes through on `raw`, as the core reads
    /// it: OSC scanner, signal mapping, the shell rule's state machine.
    fn shell_states(raw: &[u8]) -> Vec<AgentState> {
        let now = Duration::ZERO;
        let mut machine = Machine::new(MachineConfig::new(&[Source::Osc133], None));
        machine = machine.step(&Signal::Spawned, now).0;
        machine = machine.step(&Signal::Seen(true), now).0;
        let mut states = Vec::new();
        for found in OscScanner::new().scan(raw) {
            let Some(osc) = crate::convert::osc_signal(&found) else {
                continue;
            };
            let (next, effects) = machine.step(&Signal::Osc(osc), now);
            machine = next;
            for effect in effects {
                if let Effect::Transition { to, .. } = effect {
                    states.push(to);
                }
            }
        }
        states
    }

    /// Every prompt's runs: a failing cmdlet, a native exit code, the
    /// registry and back to a folder. `code` is `$LASTEXITCODE` before any
    /// native command ran.
    fn commands(tag: &'static str, code: &str) -> [(&'static str, String); 4] {
        [
            ("Get-Item nope-yien-missing", format!("{tag}[False|{code}]")),
            ("cmd /c exit 3", format!("{tag}[False|3]")),
            ("cd HKLM:\\", format!("{tag}[True|3]")),
            ("cd C:\\", format!("{tag}[True|3]")),
        ]
    }

    /// The same screen with and without the hook, one report per prompt
    /// except in the registry, and with the hook one more `A` and `B` per
    /// prompt and one more `C` and `D` per command, so the pane is working
    /// exactly while each command runs.
    fn same_prompt_with_the_hook(prompt: &str, tag: &'static str, code: &str) {
        let commands = commands(tag, code);
        let commands: Vec<(&str, &str)> = commands.iter().map(|(c, p)| (*c, p.as_str())).collect();
        let first = format!("{tag}[True|{code}]");
        let plain = run(prompt, false, &first, &commands);
        let hooked = run(prompt, true, &first, &commands);
        assert_eq!(hooked.screen, plain.screen);
        assert!(reports(&plain.raw).is_empty());
        assert_eq!(
            reports(&hooked.raw),
            [
                r#""C:\Windows""#,
                r#""C:\Windows""#,
                r#""C:\Windows""#,
                r#""C:\""#
            ]
        );
        let added = |kind| marks(&hooked.raw, kind) - marks(&plain.raw, kind);
        assert_eq!(
            ['A', 'B', 'C', 'D'].map(added),
            [5, 5, 4, 4],
            "{:?}",
            String::from_utf8_lossy(&hooked.raw)
        );
        assert!(finished_codes(&plain.raw).is_empty());
        assert_eq!(finished_codes(&hooked.raw), ["1", "1", "0", "0"]);
        let working_then_idle = [AgentState::Working, AgentState::Idle];
        assert_eq!(shell_states(&hooked.raw), working_then_idle.repeat(4));
    }

    #[test]
    fn rf_ses_01_rf_age_05_the_hook_leaves_the_users_prompt_as_it_was() {
        same_prompt_with_the_hook(FIXED_PROMPT, "P", "");
    }

    #[test]
    fn rf_ses_01_rf_age_05_the_hook_keeps_a_write_host_prompt_and_its_status() {
        same_prompt_with_the_hook(HOST_PROMPT, "S", "");
    }

    #[test]
    fn rf_age_05_marks_the_users_prompt_already_writes_keep_the_states_right() {
        same_prompt_with_the_hook(MARKED_PROMPT, "O", "");
    }

    /// What YIEN's prompt writes at each prompt, whatever the user's prompt did.
    const OUR_PROMPT_MARK: &str = "\x1b]133;A\x1b\\";

    #[test]
    fn rf_age_06_a_prompt_replaced_later_leaves_the_pane_idle() {
        let commands = [
            ("function global:prompt { 'X' + '> ' }", "X> "),
            ("cmd /c exit 0", "X> "),
            ("Get-Item nope-yien-missing", "X> "),
        ];
        let run = run(FIXED_PROMPT, true, "P[True|]", &commands);
        // The command that replaced the prompt was marked; its end shows when
        // the next line is read, and nothing after it is marked.
        assert_eq!(marks(&run.raw, 'C'), 1);
        assert_eq!(
            shell_states(&run.raw),
            [AgentState::Working, AgentState::Idle]
        );
    }

    #[test]
    fn rf_age_06_a_prompt_that_throws_still_ends_the_command() {
        let prompt = "function global:prompt { if ($global:boom) { throw 'boom' }; 'T> ' }";
        let commands = [
            ("$global:boom = 1", OUR_PROMPT_MARK),
            ("cmd /c exit 0", OUR_PROMPT_MARK),
        ];
        let run = run(prompt, true, "T> ", &commands);
        assert_eq!(marks(&run.raw, 'C'), 1);
        assert_eq!(finished_codes(&run.raw), ["1"]);
        assert_eq!(
            shell_states(&run.raw),
            [AgentState::Working, AgentState::Idle]
        );
    }

    #[test]
    fn rf_age_06_a_line_reader_wrapped_by_another_integration_is_left_alone() {
        let prompt = format!(
            "$global:__Other = $function:PSConsoleHostReadLine; \
             function global:PSConsoleHostReadLine {{ $global:__Other.Invoke() }}; {FIXED_PROMPT}"
        );
        let commands = commands("P", "");
        let commands: Vec<(&str, &str)> = commands.iter().map(|(c, p)| (*c, p.as_str())).collect();
        let run = run(&prompt, true, "P[True|]", &commands);
        assert_eq!(marks(&run.raw, 'C'), 0);
        assert_eq!(marks(&run.raw, 'D'), 0);
        assert_eq!(marks(&run.raw, 'A'), 5);
        assert!(shell_states(&run.raw).is_empty());
    }

    #[test]
    fn rf_age_05_the_first_command_of_a_new_pane_is_marked() {
        // The user's prompt runs right before the hook wraps the line reader,
        // so PSReadLine must be there at the first prompt.
        let prompt =
            "function global:prompt { 'R[' + [bool]$function:PSConsoleHostReadLine + ']> ' }";
        let first = run(prompt, true, "]> ", &[]);
        let text = String::from_utf8_lossy(&first.raw).into_owned();
        assert!(text.contains("R[True]> "), "{text:?}");
    }

    #[test]
    fn rf_age_05_the_hook_works_under_strict_mode() {
        let prompt =
            format!("Set-StrictMode -Version Latest; $global:LASTEXITCODE = 0; {FIXED_PROMPT}");
        same_prompt_with_the_hook(&prompt, "P", "0");
    }
}
