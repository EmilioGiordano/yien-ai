//! The user's default shell, launched in new panes without a profile, and
//! the shells installed on this machine, any of which a pane may start.

use std::path::{Path, PathBuf};

use yien_proto::ShellKind;

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
    /// Which known shell it is, when YIEN found it as one; a shell given by
    /// program name alone is recognised by that name.
    pub kind: Option<ShellKind>,
}

/// Git Bash's `PROMPT_COMMAND`: `D` with the status after a command, `A`,
/// the folder as `OSC 9;9` when it is on a drive (`/c/x` is `C:/x`), and `B`
/// at the end of `PS1`. `PS0` writes `C`; its subscript sets the ran flag
/// without printing. `C` needs YIEN's `EXIT` trap: an `exit` after a `C`
/// with no `D` ends the pane in error, so a user's own trap means no `C`.
/// `$?` goes back to the user's prompt; BEL ends every sequence, so no
/// backslash goes through `PS0`'s two rounds of escapes.
const BASH_HOOK: &str = "__yien_s=$?;\
__yien_prompt(){ \
if [[ ${__yien_r-} == 1 ]];then __yien_r=;printf '\\e]133;D;%s\\a' \"$__yien_s\";fi;\
printf '\\e]133;A\\a';\
if [[ $PWD =~ ^/([A-Za-z])(/.*)?$ ]];then \
printf '\\e]9;9;\"%s\"\\a' \"${BASH_REMATCH[1]^}:${BASH_REMATCH[2]:-/}\";fi;\
[[ ${PS1-} == *'133;B'* ]]||PS1=\"${PS1-}\"'\\[\\e]133;B\\a\\]';\
if [[ -z ${__yien_x-} ]];then \
if [[ -z $(trap -p EXIT) ]];then __yien_x=1;\
trap '[[ ${__yien_r-} == 1 ]]&&printf \"\\e]133;D\\a\"' EXIT;else __yien_x=2;fi;fi;\
[[ $__yien_x != 1 || ${PS0-} == *'133;C'* ]]||PS0=\"${PS0-}\"'${__yien_c[__yien_r=1]-}\\e]133;C\\a';\
return $__yien_s;};\
__yien_prompt";

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

/// cmd's prompt start mark (OSC 133 A). It only ends a working turn, which
/// cmd never starts, and lets the terminal drop keyboard flags a killed
/// program left behind.
const CMD_PROMPT_MARK: &str = "$E]133;A$E\\";

/// cmd's own prompt when `PROMPT` is not set.
const CMD_DEFAULT_PROMPT: &str = "$P$G";

/// How a shell is set up to report its folder and mark its commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    PowerShell,
    Cmd,
    Bash,
}

impl ShellCommand {
    /// A program without arguments.
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
            kind: None,
        }
    }

    /// The shell `kind`, run from `program`. Git Bash starts as an
    /// interactive login shell, as its own shortcut starts it.
    ///
    /// ```
    /// use yien_core::ShellCommand;
    /// use yien_proto::ShellKind;
    ///
    /// let bash = ShellCommand::of(ShellKind::GitBash, "C:/Git/bin/bash.exe");
    /// assert_eq!(bash.args, ["--login", "-i"]);
    /// assert_eq!(bash.kind, Some(ShellKind::GitBash));
    /// assert!(ShellCommand::of(ShellKind::Cmd, "cmd.exe").args.is_empty());
    /// ```
    pub fn of(kind: ShellKind, program: impl Into<String>) -> Self {
        let mut shell = Self::new(program);
        shell.kind = Some(kind);
        if kind == ShellKind::GitBash {
            shell.args = vec!["--login".to_owned(), "-i".to_owned()];
        }
        shell
    }

    /// How YIEN sets it up; `None` for a shell it does not know, or one
    /// given arguments of its own.
    fn family(&self) -> Option<Family> {
        match self.kind {
            Some(ShellKind::Cmd) => Some(Family::Cmd),
            Some(ShellKind::PowerShell | ShellKind::Pwsh) => Some(Family::PowerShell),
            Some(ShellKind::GitBash) => Some(Family::Bash),
            None if !self.args.is_empty() => None,
            None => match self.display_name().to_ascii_lowercase().as_str() {
                "powershell" | "pwsh" => Some(Family::PowerShell),
                "cmd" => Some(Family::Cmd),
                _ => None,
            },
        }
    }

    /// The menu name of the known shell it is (`PowerShell 7`), by its kind
    /// or its program name; `None` for any other program.
    pub(crate) fn kind_name(&self) -> Option<&'static str> {
        let kind =
            self.kind
                .or_else(|| match self.display_name().to_ascii_lowercase().as_str() {
                    "cmd" => Some(ShellKind::Cmd),
                    "powershell" => Some(ShellKind::PowerShell),
                    "pwsh" => Some(ShellKind::Pwsh),
                    _ => None,
                })?;
        Some(kind.name())
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
    /// cannot tell a command starts. cmd gets the report and a prompt start
    /// mark in front of `inherited_prompt`, the `PROMPT` it would have used.
    /// Git Bash gets both through `PROMPT_COMMAND`, after its rc files.
    /// A shell with arguments of its own, or
    /// any other shell, is returned unchanged. Nothing is written to disk.
    ///
    /// ```
    /// use yien_core::ShellCommand;
    /// use yien_proto::ShellKind;
    ///
    /// let pwsh = ShellCommand::new("pwsh.exe").reporting_cwd(None);
    /// assert_eq!(&pwsh.args[..3], ["-NoLogo", "-NoExit", "-Command"]);
    ///
    /// let cmd = ShellCommand::new("cmd.exe").reporting_cwd(Some("$G"));
    /// assert_eq!(cmd.env[0].0, "PROMPT");
    /// assert!(cmd.env[0].1.ends_with("$G"));
    ///
    /// let bash = ShellCommand::of(ShellKind::GitBash, "bash.exe").reporting_cwd(None);
    /// assert_eq!(bash.env[0].0, "PROMPT_COMMAND");
    /// ```
    pub fn reporting_cwd(mut self, inherited_prompt: Option<&str>) -> Self {
        match self.family() {
            None => {}
            Some(Family::PowerShell) => {
                self.args = ["-NoLogo", "-NoExit", "-Command", POWERSHELL_HOOK]
                    .map(str::to_owned)
                    .to_vec();
            }
            Some(Family::Bash) => {
                self.env
                    .push(("PROMPT_COMMAND".to_owned(), BASH_HOOK.to_owned()));
            }
            Some(Family::Cmd) => {
                // A YIEN started from a YIEN pane inherits the report already,
                // with or without the mark (older YIENs sent none).
                let own = inherited_prompt.unwrap_or_default();
                let own = own.strip_prefix(CMD_CWD_PROMPT).unwrap_or(own);
                let own = own.strip_prefix(CMD_PROMPT_MARK).unwrap_or(own);
                let own = if own.is_empty() {
                    CMD_DEFAULT_PROMPT
                } else {
                    own
                };
                let prompt = format!("{CMD_CWD_PROMPT}{CMD_PROMPT_MARK}{own}");
                self.env.push(("PROMPT".to_owned(), prompt));
            }
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

/// The shells found on this machine, each of which a pane can start
/// (RF-PAN-18, RF-ESP-03), in [`ShellKind::ALL`] order.
///
/// ```
/// use yien_core::InstalledShells;
/// use yien_proto::ShellKind;
///
/// let shells = InstalledShells::from_programs([(ShellKind::Cmd, "cmd.exe".to_owned())]);
/// assert_eq!(shells.get(ShellKind::Cmd).unwrap().program, "cmd.exe");
/// assert!(shells.get(ShellKind::GitBash).is_none());
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstalledShells {
    shells: Vec<ShellCommand>,
}

impl InstalledShells {
    /// The shells of this machine: on `PATH` or in their standard install
    /// folders; Git Bash only where Git for Windows put it, never a
    /// `bash.exe` from `PATH`, which may be WSL's. None off Windows. Reads
    /// the environment and the file system, so call it once, outside the
    /// async runtime.
    pub fn find() -> Self {
        if !cfg!(windows) {
            return Self::default();
        }
        let path = std::env::var_os("PATH");
        let which = |program: &str| {
            yien_which::resolve(program.as_ref(), path.as_deref(), "", Path::is_file).ok()
        };
        find_shells(&|name| std::env::var(name).ok(), &which, &Path::is_file)
    }

    /// Each kind run from its program; a kind given twice keeps the first.
    pub fn from_programs(found: impl IntoIterator<Item = (ShellKind, String)>) -> Self {
        let mut found: Vec<(ShellKind, String)> = found.into_iter().collect();
        let shells = ShellKind::ALL
            .into_iter()
            .filter_map(|kind| {
                let at = found.iter().position(|(k, _)| *k == kind)?;
                Some(ShellCommand::of(kind, found.swap_remove(at).1))
            })
            .collect();
        Self { shells }
    }

    /// The shell of `kind`, if it is installed.
    pub fn get(&self, kind: ShellKind) -> Option<&ShellCommand> {
        self.shells.iter().find(|s| s.kind == Some(kind))
    }

    /// The installed kinds, in menu order.
    pub fn kinds(&self) -> impl Iterator<Item = ShellKind> + '_ {
        self.shells.iter().filter_map(|s| s.kind)
    }
}

/// Where each kind is looked for: `var` reads the environment, `which`
/// finds a program on `PATH`, `is_file` says whether a file exists.
fn find_shells(
    var: &dyn Fn(&str) -> Option<String>,
    which: &dyn Fn(&str) -> Option<PathBuf>,
    is_file: &dyn Fn(&Path) -> bool,
) -> InstalledShells {
    let under = |root: &str, rest: &[&str]| -> Option<PathBuf> {
        let mut path = PathBuf::from(var(root)?);
        path.extend(rest);
        is_file(&path).then_some(path)
    };
    let shown = |path: PathBuf| path.to_string_lossy().into_owned();
    // A program found on `PATH` keeps its bare name, as the default shell does.
    let on_path = |program: &str| which(program).map(|_| program.to_owned());
    let system = |rest: &[&str]| {
        let mut parts = vec!["System32"];
        parts.extend(rest);
        under("SystemRoot", &parts).map(shown)
    };
    let program_files = |rest: &[&str]| {
        ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"]
            .into_iter()
            .find_map(|root| under(root, rest))
            .map(shown)
    };
    let mut found = Vec::new();
    let cmd = on_path("cmd.exe").or_else(|| system(&["cmd.exe"]));
    found.extend(cmd.map(|p| (ShellKind::Cmd, p)));
    let powershell = on_path("powershell.exe")
        .or_else(|| system(&["WindowsPowerShell", "v1.0", "powershell.exe"]));
    found.extend(powershell.map(|p| (ShellKind::PowerShell, p)));
    let pwsh = on_path("pwsh.exe").or_else(|| program_files(&["PowerShell", "7", "pwsh.exe"]));
    found.extend(pwsh.map(|p| (ShellKind::Pwsh, p)));
    let git_bash = program_files(&["Git", "bin", "bash.exe"])
        .or_else(|| under("LOCALAPPDATA", &["Programs", "Git", "bin", "bash.exe"]).map(shown))
        .or_else(|| {
            let git = which("git.exe")?;
            git_root(&git, is_file).map(|root| shown(root.join("bin").join("bash.exe")))
        });
    found.extend(git_bash.map(|p| (ShellKind::GitBash, p)));
    InstalledShells::from_programs(found)
}

/// The Git for Windows folder `git` (a `git.exe` found on `PATH`) was
/// installed in: `<root>\cmd\git.exe`, `<root>\bin\git.exe` or
/// `<root>\mingw64\bin\git.exe`, when `<root>` holds both bashes.
fn git_root(git: &Path, is_file: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
    let dir = git.parent()?;
    let name = dir.file_name()?.to_string_lossy().to_ascii_lowercase();
    if name != "cmd" && name != "bin" {
        return None;
    }
    let mut root = dir.parent()?;
    let toolchain = root
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase());
    if name == "bin"
        && let Some("mingw64" | "mingw32" | "clangarm64") = toolchain.as_deref()
    {
        root = root.parent()?;
    }
    let holds = |rest: &[&str]| {
        let mut path = root.to_path_buf();
        path.extend(rest);
        is_file(&path)
    };
    (holds(&["bin", "bash.exe"]) && holds(&["usr", "bin", "bash.exe"])).then(|| root.to_path_buf())
}

/// Every shell in its two forms, so `terminal.shell` and
/// `terminal.shell_integration` can change for panes started later.
#[derive(Debug, Clone)]
pub(crate) struct ShellSetup {
    /// YIEN's own pick, for no choice and for a choice not installed.
    auto: ShellChoice,
    kinds: Vec<(ShellKind, ShellChoice)>,
    /// Off Windows `terminal.shell` is ignored: panes run `$SHELL`.
    honours_choice: bool,
}

impl ShellSetup {
    /// Both forms of `auto` and of every installed shell; cmd's report goes
    /// in front of `inherited_prompt`.
    pub fn new(
        auto: &ShellCommand,
        installed: &InstalledShells,
        inherited_prompt: Option<&str>,
        honours_choice: bool,
    ) -> Self {
        Self {
            auto: ShellChoice::new(auto, inherited_prompt),
            kinds: installed
                .shells
                .iter()
                .filter_map(|s| Some((s.kind?, ShellChoice::new(s, inherited_prompt))))
                .collect(),
            honours_choice,
        }
    }

    /// The shell panes run when no profile names one, and, when it is not
    /// `choice`, why.
    pub fn default_shell(
        &self,
        choice: Option<ShellKind>,
        integration: bool,
    ) -> (ShellCommand, Option<String>) {
        let auto = self.auto.pick(integration);
        let Some(kind) = choice.filter(|_| self.honours_choice) else {
            return (auto.clone(), None);
        };
        match self.kind(kind, integration) {
            Some(shell) => (shell.clone(), None),
            None => (
                auto.clone(),
                Some(format!(
                    "terminal.shell: {} was not found; new panes run {} instead.",
                    kind.name(),
                    auto.display_name()
                )),
            ),
        }
    }

    /// Every installed shell as panes start it.
    pub fn installed(&self, integration: bool) -> InstalledShells {
        InstalledShells {
            shells: self
                .kinds
                .iter()
                .map(|(_, choice)| choice.pick(integration).clone())
                .collect(),
        }
    }

    fn kind(&self, kind: ShellKind, integration: bool) -> Option<&ShellCommand> {
        self.kinds
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, choice)| choice.pick(integration))
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
            [(
                "PROMPT".to_owned(),
                "$E]9;9;\"$P\"$E\\$E]133;A$E\\$P$G".to_owned()
            )]
        );
        let custom = ShellCommand::new("C:/Windows/System32/CMD.EXE")
            .reporting_cwd(Some("$T $P [it's \"mine\"]$G"));
        assert_eq!(
            custom.env[0].1,
            "$E]9;9;\"$P\"$E\\$E]133;A$E\\$T $P [it's \"mine\"]$G"
        );
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
        assert_eq!(inherited.matches("]133;A").count(), 1);
        // From a YIEN that sent the report without the mark.
        let older = ShellCommand::new("cmd.exe").reporting_cwd(Some("$E]9;9;\"$P\"$E\\$T$G"));
        assert_eq!(older.env[0].1, inherited);
        // A prompt that was only the report keeps cmd's own after it.
        let bare = ShellCommand::new("cmd.exe").reporting_cwd(Some("$E]9;9;\"$P\"$E\\"));
        assert!(bare.env[0].1.ends_with("$E]133;A$E\\$P$G"));
    }

    /// cmd's prompt as it prints it in `C:\x`, twice: `$E` is ESC.
    fn cmd_prompt_output() -> Vec<u8> {
        let shell = ShellCommand::new("cmd.exe").reporting_cwd(None);
        let once = shell.env[0]
            .1
            .replace("$E", "\x1b")
            .replace("$P", "C:\\x")
            .replace("$G", ">");
        format!("{once}dir\r\n listing\r\n{once}").into_bytes()
    }

    #[test]
    fn rf_age_02_cmd_marks_each_prompt_start() {
        let found = yien_vt::osc_scan::OscScanner::new().scan(&cmd_prompt_output());
        let starts = found
            .iter()
            .filter(|s| **s == yien_vt::OscSignal::Prompt(yien_vt::PromptMark::PromptStart))
            .count();
        assert_eq!(starts, 2);
    }

    #[test]
    fn rf_age_05_the_cmd_prompt_mark_leaves_a_shell_pane_idle() {
        use yien_agent::{AgentState, Effect, Machine, MachineConfig, Signal, Source};
        let now = std::time::Duration::ZERO;
        let mut machine = Machine::new(MachineConfig::new(&[Source::Osc133], None));
        machine = machine.step(&Signal::Spawned, now).0;
        for seen in [false, true] {
            machine = machine.step(&Signal::Seen(seen), now).0;
            for found in yien_vt::osc_scan::OscScanner::new().scan(&cmd_prompt_output()) {
                let Some(osc) = crate::convert::osc_signal(&found) else {
                    continue;
                };
                let (next, effects) = machine.step(&Signal::Osc(osc), now);
                machine = next;
                assert!(
                    !effects
                        .iter()
                        .any(|e| matches!(e, Effect::Transition { .. })),
                    "{effects:?}"
                );
            }
            assert_eq!(machine.state(), AgentState::Idle);
            assert!(!machine.signal_lost());
        }
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

    #[test]
    fn rf_ses_01_each_shell_kind_gets_its_own_hook() {
        let pwsh = ShellCommand::of(ShellKind::Pwsh, "C:/PS/pwsh.exe").reporting_cwd(None);
        let powershell = ShellCommand::of(ShellKind::PowerShell, "powershell.exe");
        assert_eq!(pwsh.args, powershell.reporting_cwd(None).args);
        assert_eq!(pwsh.args[3], POWERSHELL_HOOK);
        let cmd = ShellCommand::of(ShellKind::Cmd, "cmd.exe").reporting_cwd(None);
        assert_eq!(
            cmd.env,
            ShellCommand::new("cmd.exe").reporting_cwd(None).env
        );
        let bash = ShellCommand::of(ShellKind::GitBash, "C:/Git/bin/bash.exe");
        assert_eq!(bash.args, ["--login", "-i"]);
        assert!(bash.env.is_empty());
        let hooked = bash.clone().reporting_cwd(Some("$P$G"));
        assert_eq!(hooked.args, bash.args, "its own arguments stay");
        assert_eq!(
            hooked.env,
            [("PROMPT_COMMAND".to_owned(), BASH_HOOK.to_owned())]
        );
        assert_eq!(hooked.display_name(), "bash");
    }

    #[test]
    fn rf_age_05_the_bash_hook_marks_prompts_commands_and_the_folder() {
        // One line, so it survives being an environment variable.
        assert!(!BASH_HOOK.contains(['\n', '\r', '\0']));
        for mark in ["133;A", "133;B", "133;C", "133;D;%s", "]9;9;"] {
            assert!(BASH_HOOK.contains(mark), "{mark}");
        }
        // `$?` is read before anything runs and handed back at the end.
        assert!(BASH_HOOK.starts_with("__yien_s=$?;"));
        assert!(BASH_HOOK.contains("return $__yien_s;"));
        // `C` only where YIEN's EXIT trap writes the `D` an `exit` leaves out.
        assert!(BASH_HOOK.contains("[[ -z $(trap -p EXIT) ]]"));
        assert!(BASH_HOOK.contains("[[ $__yien_x != 1 ||"));
        // Every variable it reads has a default, for `set -u`.
        assert!(BASH_HOOK.contains("${__yien_r-}"));
        assert!(BASH_HOOK.contains("${PS1-}"));
        assert!(BASH_HOOK.contains("${PS0-}"));
    }

    /// Finds shells on a fake machine: `files` exist, `path` folders hold
    /// what `which` finds. Paths are compared with forward slashes.
    fn find_on(files: &[&str], on_path: &[&str]) -> Vec<(ShellKind, String)> {
        let slash = |p: &Path| p.to_string_lossy().replace('\\', "/");
        let var = |name: &str| {
            let value = match name {
                "SystemRoot" => "C:/Windows",
                "ProgramFiles" => "C:/Program Files",
                "ProgramW6432" => "C:/Program Files",
                "ProgramFiles(x86)" => "C:/Program Files (x86)",
                "LOCALAPPDATA" => "C:/Users/me/AppData/Local",
                _ => return None,
            };
            Some(value.to_owned())
        };
        let is_file = |p: &Path| files.contains(&slash(p).as_str());
        let which = |program: &str| {
            on_path
                .iter()
                .find(|p| p.ends_with(&format!("/{program}")))
                .map(PathBuf::from)
        };
        let found = find_shells(&var, &which, &is_file);
        ShellKind::ALL
            .into_iter()
            .filter_map(|k| Some((k, slash(Path::new(&found.get(k)?.program)))))
            .collect()
    }

    const GIT_BASH: &str = "C:/Program Files/Git/bin/bash.exe";

    #[test]
    fn rf_pan_18_shells_on_path_keep_their_bare_names() {
        let found = find_on(
            &[GIT_BASH],
            &[
                "C:/Windows/System32/cmd.exe",
                "C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe",
                "C:/Program Files/PowerShell/7/pwsh.exe",
            ],
        );
        assert_eq!(
            found,
            [
                (ShellKind::Cmd, "cmd.exe".to_owned()),
                (ShellKind::PowerShell, "powershell.exe".to_owned()),
                (ShellKind::Pwsh, "pwsh.exe".to_owned()),
                (ShellKind::GitBash, GIT_BASH.to_owned()),
            ]
        );
    }

    #[test]
    fn rf_pan_18_shells_off_path_are_found_where_they_install() {
        let found = find_on(
            &[
                "C:/Windows/System32/cmd.exe",
                "C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe",
                "C:/Program Files/PowerShell/7/pwsh.exe",
                "C:/Users/me/AppData/Local/Programs/Git/bin/bash.exe",
            ],
            &[],
        );
        let programs: Vec<&str> = found.iter().map(|(_, p)| p.as_str()).collect();
        assert_eq!(
            programs,
            [
                "C:/Windows/System32/cmd.exe",
                "C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe",
                "C:/Program Files/PowerShell/7/pwsh.exe",
                "C:/Users/me/AppData/Local/Programs/Git/bin/bash.exe",
            ]
        );
    }

    #[test]
    fn rf_pan_18_a_missing_shell_is_not_listed() {
        let found = find_on(&[], &["C:/Windows/System32/cmd.exe"]);
        assert_eq!(found, [(ShellKind::Cmd, "cmd.exe".to_owned())]);
    }

    #[test]
    fn rf_pan_18_git_bash_is_found_from_the_git_on_path() {
        for (git, root) in [
            ("D:/Tools/Git/cmd/git.exe", "D:/Tools/Git"),
            ("D:/Tools/Git/bin/git.exe", "D:/Tools/Git"),
            ("D:/Tools/Git/mingw64/bin/git.exe", "D:/Tools/Git"),
        ] {
            let bash = format!("{root}/bin/bash.exe");
            let inner = format!("{root}/usr/bin/bash.exe");
            let found = find_on(&[bash.as_str(), inner.as_str()], &[git]);
            assert_eq!(found, [(ShellKind::GitBash, bash.clone())], "{git}");
            // Without Git's own bash next to it, it is some other bash.
            assert!(find_on(&[bash.as_str()], &[git]).is_empty(), "{git}");
        }
        // A `git.exe` in any other folder names no install.
        let found = find_on(
            &["D:/Tools/bin/bash.exe", "D:/Tools/usr/bin/bash.exe"],
            &["D:/Tools/shims/git.exe"],
        );
        assert!(found.is_empty());
    }

    #[test]
    fn rf_pan_18_a_bash_on_path_is_never_taken_for_git_bash() {
        // `C:\Windows\System32\bash.exe` starts WSL.
        let found = find_on(&[], &["C:/Windows/System32/bash.exe"]);
        assert!(found.is_empty());
    }

    fn installed() -> InstalledShells {
        InstalledShells::from_programs([
            (ShellKind::GitBash, "C:/Git/bin/bash.exe".to_owned()),
            (ShellKind::Cmd, "cmd.exe".to_owned()),
        ])
    }

    #[test]
    fn rf_pan_18_installed_shells_keep_menu_order() {
        let shells = installed();
        let kinds: Vec<ShellKind> = shells.kinds().collect();
        assert_eq!(kinds, [ShellKind::Cmd, ShellKind::GitBash]);
        assert_eq!(
            shells.get(ShellKind::GitBash).unwrap().args,
            ["--login", "-i"]
        );
        assert!(shells.get(ShellKind::Pwsh).is_none());
        let twice = InstalledShells::from_programs([
            (ShellKind::Cmd, "a.exe".to_owned()),
            (ShellKind::Cmd, "b.exe".to_owned()),
        ]);
        assert_eq!(twice.get(ShellKind::Cmd).unwrap().program, "a.exe");
        assert_eq!(twice.kinds().count(), 1);
    }

    #[test]
    fn rf_cfg_01_the_setting_picks_the_default_shell() {
        let auto = ShellCommand::new("pwsh.exe");
        let setup = ShellSetup::new(&auto, &installed(), None, true);
        let (shell, notice) = setup.default_shell(None, true);
        assert_eq!((shell.program.as_str(), notice), ("pwsh.exe", None));
        assert_eq!(shell.args[2], "-Command", "the auto pick reports too");

        let (shell, notice) = setup.default_shell(Some(ShellKind::Cmd), true);
        assert_eq!((shell.program.as_str(), notice), ("cmd.exe", None));
        assert_eq!(shell.env[0].0, "PROMPT");
        let (plain, _) = setup.default_shell(Some(ShellKind::Cmd), false);
        assert!(plain.env.is_empty());

        let (shell, _) = setup.default_shell(Some(ShellKind::GitBash), true);
        assert_eq!(shell.env[0].0, "PROMPT_COMMAND");
        let (plain, _) = setup.default_shell(Some(ShellKind::GitBash), false);
        assert!(plain.env.is_empty());
        assert_eq!(plain.args, ["--login", "-i"]);
    }

    #[test]
    fn rf_cfg_01_a_default_shell_not_installed_falls_back_with_a_notice() {
        let auto = ShellCommand::new("powershell.exe");
        let setup = ShellSetup::new(&auto, &installed(), None, true);
        let (shell, notice) = setup.default_shell(Some(ShellKind::Pwsh), true);
        assert_eq!(shell, auto.clone().reporting_cwd(None));
        assert_eq!(
            notice.as_deref(),
            Some("terminal.shell: PowerShell 7 was not found; new panes run powershell instead.")
        );
    }

    #[test]
    fn rf_cfg_01_off_windows_the_setting_is_ignored() {
        let auto = ShellCommand::new("/bin/zsh");
        let setup = ShellSetup::new(&auto, &InstalledShells::default(), None, false);
        for kind in ShellKind::ALL {
            assert_eq!(setup.default_shell(Some(kind), true), (auto.clone(), None));
        }
    }

    #[test]
    fn rf_ses_01_installed_shells_follow_the_integration_switch() {
        let setup = ShellSetup::new(&ShellCommand::new("sh"), &installed(), Some("$T$G"), true);
        let on = setup.installed(true);
        let off = setup.installed(false);
        assert_eq!(off, installed());
        assert!(on.get(ShellKind::Cmd).unwrap().env[0].1.ends_with("$T$G"));
        assert_eq!(
            on.get(ShellKind::GitBash).unwrap().env[0].0,
            "PROMPT_COMMAND"
        );
        assert_eq!(
            on.kinds().collect::<Vec<_>>(),
            off.kinds().collect::<Vec<_>>()
        );
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

    use yien_agent::{AgentState, Effect, Machine, Signal};
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
        let profiles = yien_agent::builtin_profiles().unwrap();
        let shell = profiles.iter().find(|p| p.id == "shell").unwrap();
        let mut machine = Machine::new(shell.select_rule(None).rule.machine_config());
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

/// Real Git Bash through ConPTY, as panes run it, when this machine has it.
#[cfg(all(test, windows))]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers outside #[test] fns are not covered by clippy.toml's in-test allowances"
)]
mod live_bash {
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::{Duration, Instant};

    use yien_agent::{AgentState, Effect, Machine, Signal};
    use yien_proto::ShellKind;
    use yien_pty::{PortablePty, PtyBackend, PtyEvent, SpawnSpec};
    use yien_vt::osc_scan::OscScanner;
    use yien_vt::{AlacrittyEngine, VtConfig, VtEngine};

    use super::{BASH_HOOK, InstalledShells};

    fn count(raw: &[u8], needle: &str) -> usize {
        String::from_utf8_lossy(raw).matches(needle).count()
    }

    /// Git Bash without the developer's rc files, with `prompt_command`,
    /// typing each command once the previous prompt is drawn (`ready`
    /// counts how many are), until it exits. `None` when this machine has
    /// no Git Bash.
    fn run(prompt_command: Option<&str>, ready: &str, commands: &[&str]) -> Option<Vec<u8>> {
        let bash = InstalledShells::find().get(ShellKind::GitBash)?.clone();
        let mut spec = SpawnSpec::new(&bash.program)
            .args(["--noprofile", "--norc", "-i"])
            .cwd(r"C:\Windows")
            .size(100, 30)
            .env("PS1", "B> ");
        if let Some(prompt_command) = prompt_command {
            spec = spec.env("PROMPT_COMMAND", prompt_command);
        }
        let session = PortablePty::new().spawn(spec).unwrap();
        let mut engine = AlacrittyEngine::new(VtConfig::default(), 100, 30);
        let mut raw = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut typed = 0;
        loop {
            match session.events().recv_timeout(Duration::from_millis(50)) {
                Ok(PtyEvent::Output(bytes)) => {
                    raw.extend_from_slice(&bytes);
                    let out = engine.feed(&bytes);
                    if !out.replies.is_empty() {
                        session.write(out.replies).unwrap();
                    }
                }
                Ok(PtyEvent::Exited(_)) => return Some(raw),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => panic!("the session closed"),
            }
            if typed < commands.len() && count(&raw, ready) > typed {
                session
                    .write(format!("{}\r", commands[typed]).into_bytes())
                    .unwrap();
                typed += 1;
            }
            let seen = String::from_utf8_lossy(&raw).into_owned();
            assert!(Instant::now() < deadline, "timed out; output: {seen:?}");
        }
    }

    fn marks(raw: &[u8], kind: char) -> usize {
        count(raw, &format!("\x1b]133;{kind}"))
    }

    fn reports(raw: &[u8]) -> Vec<String> {
        let text = String::from_utf8_lossy(raw).into_owned();
        text.split("\x1b]9;9;")
            .skip(1)
            .map(|s| s.split('\x07').next().unwrap_or("").to_owned())
            .collect()
    }

    fn finished_codes(raw: &[u8]) -> Vec<String> {
        let text = String::from_utf8_lossy(raw).into_owned();
        text.split("\x1b]133;D;")
            .skip(1)
            .map(|s| s.split('\x07').next().unwrap_or("").to_owned())
            .collect()
    }

    /// The states a seen shell pane goes through on `raw`, then on its exit.
    fn shell_states(raw: &[u8]) -> Vec<AgentState> {
        let now = Duration::ZERO;
        let profiles = yien_agent::builtin_profiles().unwrap();
        let shell = profiles.iter().find(|p| p.id == "shell").unwrap();
        let mut machine = Machine::new(shell.select_rule(None).rule.machine_config());
        machine = machine.step(&Signal::Spawned, now).0;
        machine = machine.step(&Signal::Seen(true), now).0;
        let mut signals: Vec<Signal> = OscScanner::new()
            .scan(raw)
            .iter()
            .filter_map(crate::convert::osc_signal)
            .map(Signal::Osc)
            .collect();
        signals.push(Signal::Exited { code: Some(0) });
        let mut states = Vec::new();
        for signal in signals {
            let (next, effects) = machine.step(&signal, now);
            machine = next;
            for effect in effects {
                if let Effect::Transition { to, .. } = effect {
                    states.push(to);
                }
            }
        }
        states
    }

    #[test]
    fn rf_ses_01_rf_age_05_git_bash_marks_its_commands_and_reports_its_folder() {
        let commands = ["false", "cd /c/", "true", "exit"];
        let Some(raw) = run(Some(BASH_HOOK), "\x1b]133;B", &commands) else {
            return;
        };
        let text = String::from_utf8_lossy(&raw).into_owned();
        assert_eq!(
            ['A', 'B', 'C', 'D'].map(|k| marks(&raw, k)),
            [4, 4, 4, 4],
            "{text:?}"
        );
        assert_eq!(
            reports(&raw),
            ["\"C:/Windows\"", "\"C:/Windows\"", "\"C:/\"", "\"C:/\""]
        );
        // The last `D`, written on `exit`, has no status.
        assert_eq!(finished_codes(&raw), ["1", "0", "0"]);
        // Working exactly while each command runs; `exit` leaves no error.
        let working_then_idle = [AgentState::Working, AgentState::Idle];
        assert_eq!(shell_states(&raw), working_then_idle.repeat(4), "{text:?}");
    }

    #[test]
    fn rf_ses_01_git_bash_without_the_hook_writes_no_marks() {
        let Some(raw) = run(None, "B> ", &["exit"]) else {
            return;
        };
        for kind in ['A', 'B', 'C', 'D'] {
            assert_eq!(marks(&raw, kind), 0, "{kind}");
        }
        assert!(reports(&raw).is_empty());
    }

    #[test]
    fn rf_age_06_a_user_exit_trap_keeps_bash_from_marking_commands() {
        // An rc file's trap is there before the hook first runs.
        let hook = format!("trap 'true' EXIT;{BASH_HOOK}");
        let Some(raw) = run(Some(&hook), "\x1b]133;B", &["false", "exit"]) else {
            return;
        };
        let text = String::from_utf8_lossy(&raw).into_owned();
        assert_eq!(
            ['A', 'B', 'C', 'D'].map(|k| marks(&raw, k)),
            [2, 2, 0, 0],
            "{text:?}"
        );
        assert!(shell_states(&raw).is_empty(), "{text:?}");
    }
}
