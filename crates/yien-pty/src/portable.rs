//! The real backend: `portable-pty` over ConPTY on Windows and `openpty` on
//! unix (plan ADR-05).

use std::ffi::OsStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::console::Console;
use crate::pump::{ReaderShared, run_reader, run_waiter, run_writer};
use crate::{
    ChildControl, Drain, ExitStatus, INPUT_QUEUE_CAPACITY, OUTPUT_QUEUE_CAPACITY, PtyBackend,
    PtyControl, PtyError, PtySession, SpawnSpec, check_size,
};

/// Spawns real processes through `portable-pty`: the system ConPTY on
/// Windows, `openpty` on unix.
///
/// ```no_run
/// use std::time::Duration;
/// use yien_pty::{Drain, PortablePty, PtyBackend, SpawnSpec};
///
/// let backend = PortablePty::new().with_drain(Drain { quiet: Duration::from_millis(50), ..Drain::default() });
/// let session = backend.spawn(SpawnSpec::new("cmd.exe").args(["/c", "echo hello"]))?;
/// # Ok::<(), yien_pty::PtyError>(())
/// ```
#[derive(Debug, Clone, Default)]
pub struct PortablePty {
    drain: Drain,
}

impl PortablePty {
    /// A backend with the default [`Drain`].
    ///
    /// ```
    /// let backend = yien_pty::PortablePty::new();
    /// ```
    pub fn new() -> Self {
        Self::default()
    }

    /// Changes how long a session keeps reading after its child exits.
    ///
    /// ```
    /// use std::time::Duration;
    /// let backend = yien_pty::PortablePty::new()
    ///     .with_drain(yien_pty::Drain { quiet: Duration::from_millis(200), cap: Duration::from_secs(5) });
    /// ```
    pub fn with_drain(mut self, drain: Drain) -> Self {
        self.drain = drain;
        self
    }
}

fn pty_size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn command(spec: &SpawnSpec) -> CommandBuilder {
    let mut cmd = CommandBuilder::new(&spec.program);
    cmd.args(&spec.args);
    if let Some(dir) = &spec.cwd {
        cmd.cwd(dir);
    }
    for key in &spec.env_remove {
        cmd.env_remove(key);
    }
    for (key, value) in &spec.env {
        cmd.env(key, value);
    }
    cmd
}

/// The builder for `spec` with its program resolved against the `PATH` the
/// builder gives the child (on Windows the registry's, read at each spawn,
/// so a program installed while YIEN runs is found).
fn prepare(spec: SpawnSpec) -> Result<(SpawnSpec, CommandBuilder), PtyError> {
    let mut cmd = command(&spec);
    let spec = crate::program::resolve(spec, |key| cmd.get_env(key).map(OsStr::to_owned))?;
    if let Some(program) = cmd.get_argv_mut().first_mut() {
        program.clone_from(&spec.program);
    }
    Ok((spec, cmd))
}

type Master = Box<dyn MasterPty + Send>;

struct PortableChild {
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    pid: Option<u32>,
    exited: Arc<AtomicBool>,
    console: Arc<Console<Master>>,
}

impl ChildControl for PortableChild {
    fn kill(&self) -> Result<(), PtyError> {
        if self.exited.load(Ordering::SeqCst) {
            return Ok(());
        }
        let result = self
            .killer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .kill();
        // portable-pty 0.9's Windows killer inverts TerminateProcess's result:
        // success comes back as a stale OS error and failure as Ok. The result
        // says nothing there; `Exited` is the confirmation.
        if cfg!(windows) {
            return Ok(());
        }
        match result {
            Ok(()) => Ok(()),
            // Terminating a process that exited meanwhile can fail; that is
            // the outcome the caller wanted.
            Err(_) if self.exited.load(Ordering::SeqCst) => Ok(()),
            Err(e) => Err(PtyError::Kill(e)),
        }
    }

    fn pid(&self) -> Option<u32> {
        self.pid
    }

    fn start_close(&self) -> Result<(), PtyError> {
        let killed = self.kill();
        self.console
            .close_in_background()
            .map_err(PtyError::Close)?;
        killed
    }

    fn is_closed(&self) -> bool {
        self.console.is_closed()
    }

    fn wait_closed(&self, timeout: Duration) -> bool {
        self.console.wait(timeout)
    }
}

impl PtyBackend for PortablePty {
    fn spawn(&self, spec: SpawnSpec) -> Result<PtySession, PtyError> {
        check_size(spec.cols, spec.rows)?;
        let (spec, cmd) = prepare(spec)?;
        let pair = native_pty_system()
            .openpty(pty_size(spec.cols, spec.rows))
            .map_err(|e| PtyError::Open(e.into()))?;
        let mut child = pair.slave.spawn_command(cmd).map_err(|e| PtyError::Spawn {
            program: spec.program.to_string_lossy().into_owned(),
            source: e.into(),
        })?;
        // While the slave is open the master never sees end of stream on unix.
        drop(pair.slave);

        let killer = child.clone_killer();
        let mut setup_killer = child.clone_killer();
        let pid = child.process_id();
        let exited = Arc::new(AtomicBool::new(false));
        let mut fail = |e: PtyError| {
            let _ = setup_killer.kill();
            e
        };

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| fail(PtyError::Setup(e.into())))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| fail(PtyError::Setup(e.into())))?;

        let (events_tx, events) = sync_channel(OUTPUT_QUEUE_CAPACITY);
        let (input, input_rx) = sync_channel(INPUT_QUEUE_CAPACITY);
        let shared = Arc::new(ReaderShared::default());

        // The console is never closed on the writer alone: a writer stuck in a
        // write the child never reads would keep it open. Whoever gets there
        // first, the writer after its last message or a close, drops the
        // master off the caller's thread, since ClosePseudoConsole can block.
        let console = Arc::new(Console::new(pair.master));
        {
            let console = Arc::clone(&console);
            thread::Builder::new()
                .name("yien-pty-writer".into())
                .spawn(move || {
                    run_writer(
                        writer,
                        |cols, rows| {
                            console.with_master(|m| {
                                let _ = m.resize(pty_size(cols, rows));
                            });
                        },
                        input_rx,
                    );
                    console.close();
                })
                .map_err(|e| fail(PtyError::Setup(e.into())))?;
        }
        {
            let (events_tx, shared) = (events_tx.clone(), Arc::clone(&shared));
            thread::Builder::new()
                .name("yien-pty-reader".into())
                .spawn(move || run_reader(reader, events_tx, shared))
                .map_err(|e| fail(PtyError::Setup(e.into())))?;
        }
        {
            let exited = Arc::clone(&exited);
            let drain = self.drain;
            let console = Arc::clone(&console);
            // Not `try_wait`: it reports exit code 259 (STILL_ACTIVE) and a
            // failed status query as "still running" on Windows.
            let wait = move || {
                let status = child.wait();
                console.mark_exited();
                status.map(|s| ExitStatus::from_code(s.exit_code()))
            };
            thread::Builder::new()
                .name("yien-pty-waiter".into())
                .spawn(move || run_waiter(wait, events_tx, shared, exited, drain))
                .map_err(|e| fail(PtyError::Setup(e.into())))?;
        }

        let control = PtyControl::new(
            input,
            Arc::new(PortableChild {
                killer: Mutex::new(killer),
                pid,
                exited,
                console,
            }),
        );
        Ok(PtySession::new(control, events))
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn removed_variables_are_not_inherited_and_set_ones_still_are() {
        let spec = SpawnSpec::new("sh")
            .env_remove("PATH")
            .env_remove("YIEN_TEST_VAR")
            .env("YIEN_TEST_VAR", "1");
        let cmd = command(&spec);
        assert_eq!(cmd.get_env("PATH"), None);
        assert_eq!(cmd.get_env("YIEN_TEST_VAR"), Some(OsStr::new("1")));
    }

    fn temp_tool(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("yien-pty-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::path::absolute(dir).unwrap();
        let tool = dir.join(if cfg!(windows) { "tool.exe" } else { "tool" });
        std::fs::write(&tool, "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        (dir, tool)
    }

    #[test]
    fn the_program_is_found_on_the_path_the_builder_gives_the_child() {
        let (dir, tool) = temp_tool("builder-path");
        let spec = SpawnSpec::new("tool")
            .env("PATH", ".")
            .env("PATH", dir.as_os_str());
        let (spec, cmd) = prepare(spec).unwrap();
        assert_eq!(spec.program, tool.as_os_str());
        assert_eq!(cmd.get_argv()[0], tool.as_os_str());

        let spec = SpawnSpec::new("tool").env("PATH", ".");
        assert!(matches!(prepare(spec), Err(PtyError::NotFound { .. })));
        let spec = SpawnSpec::new("tool").env_remove("PATH");
        assert!(matches!(prepare(spec), Err(PtyError::NotFound { .. })));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_builder_runs_exactly_the_checked_file() {
        let (dir, tool) = temp_tool("checked-file");
        let (_, cmd) = prepare(SpawnSpec::new(tool.as_os_str()).arg("x")).unwrap();
        assert_eq!(
            cmd.get_argv(),
            &vec![tool.clone().into_os_string(), "x".into()]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_system_shell_resolves_to_an_absolute_path() {
        let shell = if cfg!(windows) { "cmd.exe" } else { "sh" };
        let (spec, _) = prepare(SpawnSpec::new(shell)).unwrap();
        assert!(
            std::path::Path::new(&spec.program).is_absolute(),
            "{:?}",
            spec.program
        );
    }
}
