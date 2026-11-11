//! Real children through `PortablePty`: ConPTY on Windows, `openpty` on unix.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers outside #[test] fns are not covered by clippy.toml's in-test allowances"
)]

use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use yien_pty::{
    Drain, ExitStatus, PortablePty, PtyBackend, PtyError, PtyEvent, PtySession, SpawnSpec,
};

const TIMEOUT: Duration = Duration::from_secs(20);
const CURSOR_QUERY: &[u8] = b"\x1b[6n";
const CURSOR_REPLY: &[u8] = b"\x1b[1;1R";

fn backend() -> PortablePty {
    PortablePty::new().with_drain(Drain {
        quiet: Duration::from_millis(100),
        cap: Duration::from_secs(2),
    })
}

fn count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    count(haystack, needle) > 0
}

/// Reads events, standing in for the VT engine: ConPTY's startup cursor query
/// blocks the child until it is answered, even when split across reads.
struct Driver {
    session: PtySession,
    output: Vec<u8>,
    answered: usize,
    status: Option<ExitStatus>,
}

impl Driver {
    fn new(session: PtySession) -> Self {
        Self {
            session,
            output: Vec::new(),
            answered: 0,
            status: None,
        }
    }

    /// Pumps until `done` holds for the output so far, or the child exits.
    fn until(&mut self, done: impl Fn(&[u8]) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        while self.status.is_none() && !done(&self.output) {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero(),
                "timed out; output so far: {:?}",
                String::from_utf8_lossy(&self.output)
            );
            match self.session.events().recv_timeout(left) {
                Ok(PtyEvent::Output(bytes)) => {
                    self.output.extend(bytes);
                    let queries = count(&self.output, CURSOR_QUERY);
                    while self.answered < queries {
                        self.session.write(CURSOR_REPLY.to_vec()).unwrap();
                        self.answered += 1;
                    }
                }
                Ok(PtyEvent::Exited(status)) => self.status = Some(status),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => panic!("the session closed without an exit"),
            }
        }
    }

    fn until_exit(&mut self) -> ExitStatus {
        self.until(|_| false);
        self.status.expect("the loop only ends on exit or timeout")
    }

    fn write(&self, text: &str) {
        self.session.write(text.as_bytes().to_vec()).unwrap();
    }
}

#[cfg(windows)]
fn run(script: &str) -> SpawnSpec {
    SpawnSpec::new("cmd.exe").args(["/d", "/c", script])
}

#[cfg(unix)]
fn run(script: &str) -> SpawnSpec {
    SpawnSpec::new("sh").args(["-c", script])
}

#[cfg(windows)]
fn shell() -> SpawnSpec {
    SpawnSpec::new("cmd.exe").arg("/d")
}

#[cfg(unix)]
fn shell() -> SpawnSpec {
    SpawnSpec::new("sh")
}

#[cfg(windows)]
const ENTER: &str = "\r";
#[cfg(unix)]
const ENTER: &str = "\n";

/// Starts an interactive shell and waits until it reads input.
fn interactive() -> Driver {
    let mut driver = Driver::new(backend().spawn(shell().size(120, 30)).unwrap());
    if cfg!(windows) {
        driver.until(|out| contains(out, b">"));
    }
    driver
}

#[test]
fn rf_age_02_child_output_arrives_then_its_exit() {
    let mut driver = Driver::new(backend().spawn(run("echo hello")).unwrap());
    let status = driver.until_exit();
    assert!(contains(&driver.output, b"hello"));
    assert_eq!(status, ExitStatus::from_code(0));
}

#[cfg(windows)]
#[test]
fn rf_pan_06_bundled_conpty_spawns_cmd_echo() {
    if std::env::var_os("YIEN_REQUIRE_BUNDLED_CONPTY").is_none() {
        return;
    }
    let exe_dir = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    assert!(exe_dir.join("conpty.dll").is_file());
    assert!(exe_dir.join("OpenConsole.exe").is_file());
    let mut driver = Driver::new(backend().spawn(run("echo BUNDLED_CONPTY_OK")).unwrap());
    assert!(driver.until_exit().success());
    assert!(contains(&driver.output, b"BUNDLED_CONPTY_OK"));
    println!(
        "cmd /d /c echo BUNDLED_CONPTY_OK output: {}",
        String::from_utf8_lossy(&driver.output).trim()
    );
}

#[test]
fn rf_age_02_input_reaches_an_interactive_child() {
    let mut driver = interactive();
    if cfg!(windows) {
        driver.write(&format!("set /a 1234*5{ENTER}"));
    } else {
        driver.write(&format!("echo $((1234*5)){ENTER}"));
    }
    driver.until(|out| contains(out, b"6170"));
    assert!(driver.status.is_none(), "the shell exited early");

    driver.write(&format!("exit{ENTER}"));
    assert!(driver.until_exit().success());
}

#[test]
fn a_failing_command_reports_its_exit_code() {
    let mut driver = Driver::new(backend().spawn(run("exit 3")).unwrap());
    assert_eq!(driver.until_exit(), ExitStatus::from_code(3));
    driver.session.kill().unwrap();
}

#[test]
#[cfg(windows)]
fn exit_code_259_is_an_exit_not_still_running() {
    let mut driver = Driver::new(backend().spawn(run("exit 259")).unwrap());
    assert_eq!(driver.until_exit(), ExitStatus::from_code(259));
}

#[test]
fn resize_is_accepted_while_running() {
    let mut driver = interactive();
    driver.session.resize(100, 40).unwrap();
    driver.session.resize(40, 10).unwrap();
    driver.write(&format!("exit{ENTER}"));
    assert!(driver.until_exit().success());
}

#[test]
fn kill_ends_a_running_child() {
    let mut driver = interactive();
    assert!(driver.session.pid().is_some());
    driver.session.kill().unwrap();
    assert!(!driver.until_exit().success());
}

#[test]
fn spec_env_and_cwd_reach_the_child() {
    let dir = std::env::temp_dir().join("yien-pty-cwd-check");
    std::fs::create_dir_all(&dir).unwrap();
    let script = if cfg!(windows) {
        "echo %YIEN_PTY_CHECK% & cd"
    } else {
        "echo $YIEN_PTY_CHECK; pwd"
    };
    let spec = run(script)
        .env("YIEN_PTY_CHECK", "marker-7731")
        .cwd(&dir)
        .size(250, 30);
    let mut driver = Driver::new(backend().spawn(spec).unwrap());
    assert!(driver.until_exit().success());
    assert!(contains(&driver.output, b"marker-7731"));
    assert!(contains(&driver.output, b"yien-pty-cwd-check"));
}

#[test]
fn a_missing_program_is_not_found_before_spawning() {
    match backend().spawn(SpawnSpec::new("yien-no-such-program-4417")) {
        Err(PtyError::NotFound { program }) => assert_eq!(program, "yien-no-such-program-4417"),
        Err(other) => panic!("expected NotFound, got {other:?}"),
        Ok(_) => panic!("a missing program spawned"),
    }
}

#[test]
fn a_missing_absolute_program_is_refused_before_spawning() {
    let missing = std::env::temp_dir()
        .join("yien-no-such-dir-4417")
        .join("prog");
    match backend().spawn(SpawnSpec::new(missing.as_os_str())) {
        Err(PtyError::MissingProgram { .. }) => {}
        Err(other) => panic!("expected MissingProgram, got {other:?}"),
        Ok(_) => panic!("a missing program spawned"),
    }
}

#[test]
fn a_relative_program_path_is_refused() {
    let relative = std::path::Path::new("bin").join("yien-prog");
    assert!(matches!(
        backend().spawn(SpawnSpec::new(relative.as_os_str())),
        Err(PtyError::RelativeProgram { .. })
    ));
}

#[test]
fn a_zero_size_is_refused_before_spawning() {
    assert!(matches!(
        backend().spawn(run("echo hi").size(0, 24)),
        Err(PtyError::InvalidSize { cols: 0, rows: 24 })
    ));
}

#[test]
fn dropping_a_session_does_not_block() {
    let driver = interactive();
    let start = Instant::now();
    drop(driver);
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[test]
fn close_ends_the_child_and_reports_the_terminal_closed() {
    let mut driver = interactive();
    let closer = driver.session.control().closer();
    assert!(!closer.is_closed());
    closer.close(Duration::from_secs(5)).unwrap();
    assert!(closer.is_closed());
    assert!(!driver.until_exit().success());
}
