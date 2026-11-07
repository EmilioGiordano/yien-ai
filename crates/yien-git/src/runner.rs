//! Runs the `git` CLI read-only, with a timeout, no console window and capped output.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::GitError;

/// How long one git call may take by default before it is killed.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);

/// The most stdout one git call may print before it is stopped.
pub const MAX_OUTPUT: usize = 32 * 1024 * 1024;

const MAX_STDERR: usize = 64 * 1024;
const MAX_MESSAGE_CHARS: usize = 300;
const POLL: Duration = Duration::from_millis(2);
/// Time to collect the pipes after git exits; a grandchild can hold them open.
const PIPE_GRACE: Duration = Duration::from_millis(500);

/// Inherited variables that would point git at another repository than the one asked about.
const REPO_OVERRIDES: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
];

/// How to run git: which program and how long to wait for it.
///
/// Every call is read-only: no optional locks (so `status` never takes
/// `index.lock` from the user's own git), no fsmonitor program, no prompts.
///
/// ```no_run
/// use std::time::Duration;
/// let git = yien_git::Git::new().with_timeout(Duration::from_secs(1));
/// let info = git.info(std::path::Path::new("."))?;
/// println!("{:?}", info.status.head);
/// # Ok::<(), yien_git::GitError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Git {
    program: PathBuf,
    timeout: Duration,
}

impl Default for Git {
    fn default() -> Self {
        Self {
            program: PathBuf::from("git"),
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

impl Git {
    /// `git` from `PATH`, with [`DEFAULT_TIMEOUT`].
    ///
    /// ```
    /// let git = yien_git::Git::new();
    /// assert_eq!(git.timeout(), yien_git::DEFAULT_TIMEOUT);
    /// ```
    pub fn new() -> Self {
        Self::default()
    }

    /// Uses another git executable.
    ///
    /// ```
    /// let git = yien_git::Git::new().with_program("C:/Program Files/Git/cmd/git.exe");
    /// # let _ = git;
    /// ```
    pub fn with_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.program = program.into();
        self
    }

    /// Changes how long one call may take.
    ///
    /// ```
    /// use std::time::Duration;
    /// let git = yien_git::Git::new().with_timeout(Duration::from_millis(500));
    /// assert_eq!(git.timeout(), Duration::from_millis(500));
    /// ```
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// How long one call may take.
    ///
    /// ```
    /// assert!(yien_git::Git::new().timeout().as_secs() > 0);
    /// ```
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Runs `git <args>` in `dir` and returns its stdout.
    pub(crate) fn run(&self, dir: &Path, args: &[&str]) -> Result<Vec<u8>, GitError> {
        self.run_with(dir, &[], args)
    }

    /// Like [`Git::run`], with extra `-c <key>=<value>` settings before the subcommand.
    pub(crate) fn run_with(
        &self,
        dir: &Path,
        config: &[String],
        args: &[&str],
    ) -> Result<Vec<u8>, GitError> {
        let cmd = self.command(dir, config, args);
        let subcommand = args.first().copied().unwrap_or_default();
        let output = match run_command(cmd, self.timeout, MAX_OUTPUT) {
            Ok(output) => output,
            Err(err) => {
                tracing::debug!(subcommand, error = %err, "git call failed");
                return Err(err);
            }
        };
        if output.status.success() {
            return Ok(output.stdout);
        }
        Err(classify_failure(
            output.status.code(),
            &String::from_utf8_lossy(&output.stderr),
        ))
    }

    fn command(&self, dir: &Path, config: &[String], args: &[&str]) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.arg("-C")
            .arg(dir)
            .args(["--no-optional-locks", "-c", "core.fsmonitor=false"]);
        for setting in config {
            cmd.arg("-c").arg(setting);
        }
        cmd.args(args)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0")
            // Untranslated messages, so they can be recognised in classify_failure.
            .env("LC_ALL", "C");
        for var in REPO_OVERRIDES {
            cmd.env_remove(var);
        }
        cmd
    }
}

/// Turns a failed call's exit code and stderr (untranslated, `LC_ALL=C`) into an error.
fn classify_failure(code: Option<i32>, stderr: &str) -> GitError {
    if stderr.contains("not a git repository") {
        return GitError::NotARepository;
    }
    GitError::Failed {
        code,
        message: first_line(stderr),
    }
}

fn first_line(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    line.trim().chars().take(MAX_MESSAGE_CHARS).collect()
}

#[derive(Debug)]
pub(crate) struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Runs `cmd` with no stdin and no console window, killing it after `timeout`.
/// Stdout beyond `max_stdout` bytes is an error; the pipe is closed so the child stops.
pub(crate) fn run_command(
    mut cmd: Command,
    timeout: Duration,
    max_stdout: usize,
) -> Result<Output, GitError> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_console(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => GitError::GitNotFound,
        _ => GitError::Io(e),
    })?;
    let stdout = child.stdout.take().map(|pipe| drain(pipe, max_stdout));
    let stderr = child.stderr.take().map(|pipe| drain(pipe, MAX_STDERR));

    let status = wait_until(&mut child, Instant::now() + timeout).map_err(|e| match e {
        WaitError::Timeout => GitError::Timeout(timeout),
        WaitError::Io(e) => GitError::Io(e),
    })?;

    let stdout = collect(stdout).ok_or(GitError::Timeout(timeout))?;
    let stderr = collect(stderr).unwrap_or_default();
    if stdout.len() > max_stdout {
        return Err(GitError::OutputTooLarge(max_stdout));
    }
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

enum WaitError {
    Timeout,
    Io(std::io::Error),
}

fn wait_until(child: &mut Child, deadline: Instant) -> Result<ExitStatus, WaitError> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(e) => {
                kill(child);
                return Err(WaitError::Io(e));
            }
        }
        if Instant::now() >= deadline {
            kill(child);
            return Err(WaitError::Timeout);
        }
        thread::sleep(POLL);
    }
}

fn kill(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Reads up to `cap + 1` bytes on its own thread, so a full pipe never stalls
/// the child, then drops the pipe.
fn drain(pipe: impl Read + Send + 'static, cap: usize) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = Vec::new();
        let limit = u64::try_from(cap).unwrap_or(u64::MAX).saturating_add(1);
        let _ = pipe.take(limit).read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    rx
}

fn collect(rx: Option<mpsc::Receiver<Vec<u8>>>) -> Option<Vec<u8>> {
    match rx {
        Some(rx) => rx.recv_timeout(PIPE_GRACE).ok(),
        None => Some(Vec::new()),
    }
}

#[cfg(windows)]
fn hide_console(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console(_cmd: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    fn sleeper() -> Command {
        if cfg!(windows) {
            let mut cmd = Command::new("ping");
            cmd.args(["-n", "10", "127.0.0.1"]);
            cmd
        } else {
            let mut cmd = Command::new("sleep");
            cmd.arg("10");
            cmd
        }
    }

    #[test]
    fn missing_program_is_git_not_found() {
        let git = Git::new().with_program("yien-no-such-git-binary");
        let err = git.run(Path::new("."), &["status"]).unwrap_err();
        assert!(matches!(err, GitError::GitNotFound), "{err:?}");
    }

    #[test]
    fn slow_command_is_killed_at_the_timeout() {
        let start = Instant::now();
        let err = run_command(sleeper(), Duration::from_millis(100), 1024).unwrap_err();
        assert!(matches!(err, GitError::Timeout(_)), "{err:?}");
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn output_over_the_cap_is_an_error() {
        if !git_available() {
            eprintln!("git not found, skipping");
            return;
        }
        let mut cmd = Command::new("git");
        cmd.arg("--version");
        let err = run_command(cmd, DEFAULT_TIMEOUT, 4).unwrap_err();
        assert!(matches!(err, GitError::OutputTooLarge(4)), "{err:?}");
    }

    #[test]
    fn output_under_the_cap_is_returned() {
        if !git_available() {
            eprintln!("git not found, skipping");
            return;
        }
        let mut cmd = Command::new("git");
        cmd.arg("--version");
        let out = run_command(cmd, DEFAULT_TIMEOUT, MAX_OUTPUT).unwrap();
        assert!(out.status.success());
        assert!(out.stdout.starts_with(b"git version"));
    }

    #[test]
    fn outside_a_repository_is_not_a_repository() {
        if !git_available() {
            eprintln!("git not found, skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("yien-git-norepo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut git = Git::new();
        git.timeout = Duration::from_secs(10);
        let result = git.run(&dir, &["rev-parse", "--show-toplevel"]);
        let _ = std::fs::remove_dir_all(&dir);
        match result {
            Err(GitError::NotARepository) => {}
            Ok(out) => eprintln!(
                "temp dir is inside a repo ({}), skipping",
                String::from_utf8_lossy(&out)
            ),
            Err(other) => panic!("{other:?}"),
        }
    }

    #[test]
    fn failures_are_classified() {
        assert!(matches!(
            classify_failure(
                Some(128),
                "fatal: not a git repository (or any of the parent directories): .git\n"
            ),
            GitError::NotARepository
        ));
        match classify_failure(Some(129), "\nerror: unknown option `x'\nusage: ...") {
            GitError::Failed { code, message } => {
                assert_eq!(code, Some(129));
                assert_eq!(message, "error: unknown option `x'");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn inherited_repository_variables_are_removed() {
        let cmd = Git::new().command(Path::new("."), &["filter.x.clean=".to_owned()], &["status"]);
        let envs: Vec<_> = cmd.get_envs().collect();
        for var in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_COMMON_DIR",
        ] {
            assert!(
                envs.contains(&(std::ffi::OsStr::new(var), None)),
                "{var} is not removed"
            );
        }
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let at = args.iter().position(|a| a == "filter.x.clean=").unwrap();
        assert_eq!(args[at - 1], "-c");
        assert!(at < args.iter().position(|a| a == "status").unwrap());
    }

    #[test]
    fn first_line_skips_blanks_and_caps_length() {
        assert_eq!(first_line("\n  fatal: x\nmore"), "fatal: x");
        assert_eq!(first_line(&"a".repeat(1000)).len(), MAX_MESSAGE_CHARS);
        assert_eq!(first_line(""), "");
    }
}
