//! Pseudo terminals for YIEN: spawn a child process behind a PTY and move its
//! bytes in and out, unaltered and never dropped (RF-AGE-02, plan ADR-05 and
//! ADR-14).
//!
//! Every session runs three blocking threads: a reader that forwards output, a
//! writer that owns the master and applies input and resizes, and a waiter
//! that watches the child's exit. Callers talk to them through channels, so
//! nothing here ever blocks an async runtime.
//!
//! ```no_run
//! use std::time::Duration;
//! use yien_pty::{PortablePty, PtyBackend, PtyEvent, SpawnSpec};
//!
//! let session = PortablePty::new().spawn(SpawnSpec::new("cmd.exe").arg("/k"))?;
//! session.write(b"echo hi\r".to_vec())?;
//! while let Ok(event) = session.events().recv_timeout(Duration::from_secs(5)) {
//!     match event {
//!         PtyEvent::Output(bytes) => { /* feed the VT engine, write its replies back */ }
//!         PtyEvent::Exited(status) => { println!("exited: {status:?}"); break; }
//!     }
//! }
//! # Ok::<(), yien_pty::PtyError>(())
//! ```

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::time::Duration;

#[cfg(any(test, feature = "fake"))]
mod fake;
mod portable;
mod program;
mod pump;

#[cfg(any(test, feature = "fake"))]
pub use fake::{FakeChild, FakeInput, FakePty, KILLED_EXIT_CODE};
pub use portable::PortablePty;

/// Writes and resizes waiting for the writer thread; beyond this,
/// [`PtySession::write`] returns [`PtyError::InputQueueFull`] instead of
/// blocking.
///
/// ```
/// assert!(yien_pty::INPUT_QUEUE_CAPACITY > 0);
/// ```
pub const INPUT_QUEUE_CAPACITY: usize = 256;
/// Output chunks buffered before the reader thread stops reading, which in
/// turn stalls the child: the PTY's natural backpressure (plan §4.2).
///
/// ```
/// assert!(yien_pty::OUTPUT_QUEUE_CAPACITY > 0);
/// ```
pub const OUTPUT_QUEUE_CAPACITY: usize = 64;

/// Starts children in pseudo terminals.
///
/// The seam between YIEN and the platform PTY: [`PortablePty`] for real
/// processes, `FakePty` (feature `fake`) for tests.
///
/// ```no_run
/// use yien_pty::{PortablePty, PtyBackend, SpawnSpec};
///
/// fn open(backend: &dyn PtyBackend) -> Result<(), yien_pty::PtyError> {
///     let session = backend.spawn(SpawnSpec::new("pwsh.exe").size(120, 30))?;
///     println!("pid {:?}", session.pid());
///     Ok(())
/// }
/// open(&PortablePty::new()).unwrap();
/// ```
pub trait PtyBackend: Send + Sync {
    /// Starts `spec.program` in a new pseudo terminal.
    ///
    /// ```no_run
    /// use yien_pty::{PortablePty, PtyBackend, SpawnSpec};
    /// let session = PortablePty::new().spawn(SpawnSpec::new("sh").args(["-c", "echo hi"]))?;
    /// # Ok::<(), yien_pty::PtyError>(())
    /// ```
    fn spawn(&self, spec: SpawnSpec) -> Result<PtySession, PtyError>;
}

/// What to run and in what terminal size.
///
/// The child inherits YIEN's environment; `env` adds or overrides variables.
///
/// ```
/// use yien_pty::SpawnSpec;
///
/// let spec = SpawnSpec::new("claude").arg("--continue").env("TERM", "xterm-256color").size(100, 30);
/// assert_eq!((spec.cols, spec.rows), (100, 30));
/// ```
///
/// Its `Debug` shows the number of arguments and only the names of the
/// variables, since both can carry tokens (plan §5.4).
#[derive(Clone, PartialEq, Eq)]
pub struct SpawnSpec {
    /// The program, found through `PATH` when it is not a path.
    pub program: OsString,
    /// Arguments after the program.
    pub args: Vec<OsString>,
    /// Working directory; `None` inherits the platform default.
    pub cwd: Option<PathBuf>,
    /// Variables set on top of the inherited environment, in order.
    pub env: Vec<(OsString, OsString)>,
    /// Terminal width in cells, at least 1.
    pub cols: u16,
    /// Terminal height in cells, at least 1.
    pub rows: u16,
}

impl fmt::Debug for SpawnSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let env_keys: Vec<_> = self.env.iter().map(|(key, _)| key).collect();
        f.debug_struct("SpawnSpec")
            .field("program", &self.program)
            .field("args", &format_args!("{} args", self.args.len()))
            .field("cwd", &self.cwd)
            .field("env_keys", &env_keys)
            .field("cols", &self.cols)
            .field("rows", &self.rows)
            .finish()
    }
}

impl SpawnSpec {
    /// A spec for `program` with no arguments in an 80x24 terminal.
    ///
    /// ```
    /// let spec = yien_pty::SpawnSpec::new("sh");
    /// assert_eq!((spec.cols, spec.rows), (80, 24));
    /// ```
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            cols: 80,
            rows: 24,
        }
    }

    /// Adds one argument.
    ///
    /// ```
    /// let spec = yien_pty::SpawnSpec::new("cmd.exe").arg("/c").arg("dir");
    /// assert_eq!(spec.args.len(), 2);
    /// ```
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Adds several arguments.
    ///
    /// ```
    /// let spec = yien_pty::SpawnSpec::new("sh").args(["-c", "echo hi"]);
    /// assert_eq!(spec.args.len(), 2);
    /// ```
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Sets the working directory.
    ///
    /// ```
    /// let spec = yien_pty::SpawnSpec::new("sh").cwd("/tmp");
    /// assert!(spec.cwd.is_some());
    /// ```
    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    /// Sets one environment variable for the child.
    ///
    /// ```
    /// let spec = yien_pty::SpawnSpec::new("sh").env("YIEN_PANE", "7");
    /// assert_eq!(spec.env.len(), 1);
    /// ```
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Sets the terminal size in cells.
    ///
    /// ```
    /// let spec = yien_pty::SpawnSpec::new("sh").size(132, 43);
    /// assert_eq!((spec.cols, spec.rows), (132, 43));
    /// ```
    pub fn size(mut self, cols: u16, rows: u16) -> Self {
        self.cols = cols;
        self.rows = rows;
        self
    }
}

/// Something the child did.
///
/// ```
/// use yien_pty::{ExitStatus, PtyEvent};
/// let events = [PtyEvent::Output(b"hi".to_vec()), PtyEvent::Exited(ExitStatus::from_code(0))];
/// assert!(matches!(events[1], PtyEvent::Exited(s) if s.success()));
/// ```
///
/// Its `Debug` shows only the length of output, so terminal content never
/// reaches a log (plan §5.4).
#[derive(Clone, PartialEq, Eq)]
pub enum PtyEvent {
    /// Bytes exactly as the PTY produced them.
    Output(Vec<u8>),
    /// The child exited and its output went quiet. Normally the last event;
    /// bytes that still arrive after it are forwarded, never dropped.
    Exited(ExitStatus),
}

impl fmt::Debug for PtyEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Output(bytes) => write!(f, "Output({} bytes)", bytes.len()),
            Self::Exited(status) => f.debug_tuple("Exited").field(status).finish(),
        }
    }
}

/// How the child ended.
///
/// ```
/// let status = yien_pty::ExitStatus::from_code(0);
/// assert!(status.success());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitStatus {
    /// The exit code, or `None` when the OS could not report it. Windows codes
    /// span the full `u32` range (`0xC000013A` is Ctrl+C).
    pub code: Option<u32>,
}

impl ExitStatus {
    /// A status with a known exit code.
    ///
    /// ```
    /// assert_eq!(yien_pty::ExitStatus::from_code(3).code, Some(3));
    /// ```
    pub fn from_code(code: u32) -> Self {
        Self { code: Some(code) }
    }

    /// Whether the child exited with code 0.
    ///
    /// ```
    /// assert!(!yien_pty::ExitStatus::from_code(1).success());
    /// ```
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// How long to keep reading after the child exits (see [`PortablePty::with_drain`]).
///
/// ConPTY keeps the master open after the child exits, so end of output can
/// only be inferred: the session ends after `quiet` without new bytes, or
/// once the reader has waited for bytes for `cap` in total, so a grandchild
/// that keeps writing cannot hold the session open.
///
/// Both count only time the reader spends waiting in `read`, never time it
/// spends blocked on a consumer that is behind. `cap` therefore does not
/// bound wall time: a consumer that stops reading delays `Exited` as long as
/// it stays behind, and every byte read before the exit still comes first.
///
/// ```
/// use std::time::Duration;
/// let drain = yien_pty::Drain { quiet: Duration::from_millis(50), ..Default::default() };
/// assert!(drain.cap > drain.quiet);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Drain {
    /// Silence that counts as the end of output.
    pub quiet: Duration,
    /// Time spent waiting in `read` after exit before [`PtyEvent::Exited`] is
    /// sent anyway.
    pub cap: Duration,
}

impl Default for Drain {
    fn default() -> Self {
        Self {
            quiet: Duration::from_millis(100),
            cap: Duration::from_secs(2),
        }
    }
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Why a PTY operation failed.
///
/// ```
/// use yien_pty::PtyError;
/// let err = PtyError::InvalidSize { cols: 0, rows: 24 };
/// assert_eq!(err.to_string(), "invalid terminal size 0x24; both must be at least 1");
/// ```
#[derive(Debug, thiserror::Error)]
pub enum PtyError {
    /// A size with zero columns or rows. A caller defect.
    #[error("invalid terminal size {cols}x{rows}; both must be at least 1")]
    InvalidSize {
        /// Requested columns.
        cols: u16,
        /// Requested rows.
        rows: u16,
    },
    /// The OS could not create a pseudo terminal.
    #[error("could not open a pseudo terminal")]
    Open(#[source] BoxError),
    /// The program could not be started: missing, not executable, bad cwd.
    #[error("could not start {program}")]
    Spawn {
        /// The program as given, lossily converted.
        program: String,
        /// The OS error.
        #[source]
        source: BoxError,
    },
    /// The PTY opened but its pipes or threads could not be set up.
    #[error("could not set up the pseudo terminal")]
    Setup(#[source] BoxError),
    /// The input queue is full; the bytes are handed back, nothing was sent.
    /// Transient: retry once the child reads.
    #[error("the input queue is full")]
    InputQueueFull(ReturnedBytes),
    /// The input queue is full, so the resize was not queued. Transient.
    #[error("the input queue is full; resize not applied")]
    ResizeQueueFull,
    /// The session has ended; the writer no longer accepts input or resizes.
    #[error("the session is closed")]
    Closed,
    /// The OS refused to terminate the child.
    #[error("could not terminate the child")]
    Kill(#[source] std::io::Error),
}

/// Input refused by a full queue, given back to the caller.
///
/// Its `Debug` shows only the length, so terminal content never reaches a log.
///
/// ```
/// let returned = yien_pty::ReturnedBytes::new(b"ls\r".to_vec());
/// assert_eq!(format!("{returned:?}"), "ReturnedBytes(3 bytes)");
/// assert_eq!(returned.into_bytes(), b"ls\r");
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct ReturnedBytes(Vec<u8>);

impl ReturnedBytes {
    /// Wraps refused bytes.
    ///
    /// ```
    /// assert!(yien_pty::ReturnedBytes::new(Vec::new()).into_bytes().is_empty());
    /// ```
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// The refused bytes, unchanged.
    ///
    /// ```
    /// assert_eq!(yien_pty::ReturnedBytes::new(vec![1]).into_bytes(), [1]);
    /// ```
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

impl fmt::Debug for ReturnedBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ReturnedBytes({} bytes)", self.0.len())
    }
}

/// What the writer thread does, in order.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum WriterMsg {
    Input(Vec<u8>),
    Resize { cols: u16, rows: u16 },
}

impl WriterMsg {
    /// The bytes of an `Input`; a `Resize` carries none.
    fn into_input(self) -> Vec<u8> {
        match self {
            Self::Input(bytes) => bytes,
            Self::Resize { .. } => Vec::new(),
        }
    }
}

impl fmt::Debug for WriterMsg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Input(bytes) => write!(f, "Input({} bytes)", bytes.len()),
            Self::Resize { cols, rows } => write!(f, "Resize({cols}x{rows})"),
        }
    }
}

/// The platform side of a session that is not a byte stream.
pub(crate) trait ChildControl: Send + Sync {
    fn kill(&self) -> Result<(), PtyError>;
    fn pid(&self) -> Option<u32>;
    /// Called from `Drop`, which must never block.
    fn kill_on_drop(&self) {
        let _ = self.kill();
    }
}

pub(crate) fn check_size(cols: u16, rows: u16) -> Result<(), PtyError> {
    if cols == 0 || rows == 0 {
        return Err(PtyError::InvalidSize { cols, rows });
    }
    Ok(())
}

/// The input side of a session: write, resize, kill. `Send + Sync`, so it can
/// live apart from the event receiver (see [`PtySession::into_parts`]).
///
/// Dropping it terminates the child if it is still running.
///
/// ```no_run
/// use yien_pty::{PortablePty, PtyBackend, SpawnSpec};
///
/// let (control, events) = PortablePty::new().spawn(SpawnSpec::new("sh"))?.into_parts();
/// std::thread::spawn(move || for event in events { let _ = event; });
/// control.write(b"exit\n".to_vec())?;
/// # Ok::<(), yien_pty::PtyError>(())
/// ```
pub struct PtyControl {
    input: SyncSender<WriterMsg>,
    child: Box<dyn ChildControl>,
}

impl PtyControl {
    pub(crate) fn new(input: SyncSender<WriterMsg>, child: Box<dyn ChildControl>) -> Self {
        Self { input, child }
    }

    /// Queues bytes for the child without waiting for them to be written.
    ///
    /// When [`INPUT_QUEUE_CAPACITY`] writes are already waiting (a large paste
    /// into a busy child), returns [`PtyError::InputQueueFull`] with the bytes
    /// handed back: nothing is dropped, and the caller decides when to retry.
    ///
    /// ```no_run
    /// # let session = yien_pty::PtyBackend::spawn(&yien_pty::PortablePty::new(), yien_pty::SpawnSpec::new("sh"))?;
    /// session.control().write(b"echo hi\n".to_vec())?;
    /// # Ok::<(), yien_pty::PtyError>(())
    /// ```
    pub fn write(&self, bytes: Vec<u8>) -> Result<(), PtyError> {
        match self.input.try_send(WriterMsg::Input(bytes)) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(msg)) => {
                Err(PtyError::InputQueueFull(ReturnedBytes(msg.into_input())))
            }
            Err(TrySendError::Disconnected(_)) => Err(PtyError::Closed),
        }
    }

    /// Queues a resize, applied after the input queued before it.
    ///
    /// The writer applies it and an OS failure there is not reported back, so
    /// `Ok` means queued.
    ///
    /// ```no_run
    /// # let session = yien_pty::PtyBackend::spawn(&yien_pty::PortablePty::new(), yien_pty::SpawnSpec::new("sh"))?;
    /// session.control().resize(120, 40)?;
    /// # Ok::<(), yien_pty::PtyError>(())
    /// ```
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), PtyError> {
        check_size(cols, rows)?;
        match self.input.try_send(WriterMsg::Resize { cols, rows }) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(PtyError::ResizeQueueFull),
            Err(TrySendError::Disconnected(_)) => Err(PtyError::Closed),
        }
    }

    /// Terminates the child. A child that already exited is not an error.
    /// [`PtyEvent::Exited`] still follows through the event receiver.
    ///
    /// ```no_run
    /// # let session = yien_pty::PtyBackend::spawn(&yien_pty::PortablePty::new(), yien_pty::SpawnSpec::new("sh"))?;
    /// session.control().kill()?;
    /// # Ok::<(), yien_pty::PtyError>(())
    /// ```
    pub fn kill(&self) -> Result<(), PtyError> {
        self.child.kill()
    }

    /// The child's process id, when the OS reports one.
    ///
    /// ```no_run
    /// # let session = yien_pty::PtyBackend::spawn(&yien_pty::PortablePty::new(), yien_pty::SpawnSpec::new("sh"))?;
    /// println!("{:?}", session.control().pid());
    /// # Ok::<(), yien_pty::PtyError>(())
    /// ```
    pub fn pid(&self) -> Option<u32> {
        self.child.pid()
    }
}

impl Drop for PtyControl {
    fn drop(&mut self) {
        self.child.kill_on_drop();
    }
}

impl fmt::Debug for PtyControl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PtyControl")
            .field("pid", &self.pid())
            .finish_non_exhaustive()
    }
}

/// A running child: its events and its input side.
///
/// Answer the terminal queries in the output promptly: ConPTY sends
/// `ESC [ 6 n` at startup and the child does not run until the reply is
/// written back.
///
/// ```no_run
/// use std::time::Duration;
/// use yien_pty::{PortablePty, PtyBackend, PtyEvent, SpawnSpec};
///
/// let session = PortablePty::new().spawn(SpawnSpec::new("sh").args(["-c", "exit 3"]))?;
/// loop {
///     match session.events().recv_timeout(Duration::from_secs(5)) {
///         Ok(PtyEvent::Exited(status)) => { assert_eq!(status.code, Some(3)); break; }
///         Ok(PtyEvent::Output(_)) => {}
///         Err(_) => break,
///     }
/// }
/// # Ok::<(), yien_pty::PtyError>(())
/// ```
#[derive(Debug)]
pub struct PtySession {
    control: PtyControl,
    events: Receiver<PtyEvent>,
}

impl PtySession {
    pub(crate) fn new(control: PtyControl, events: Receiver<PtyEvent>) -> Self {
        Self { control, events }
    }

    /// Output and exit, in the order they happened.
    ///
    /// ```no_run
    /// # let session = yien_pty::PtyBackend::spawn(&yien_pty::PortablePty::new(), yien_pty::SpawnSpec::new("sh"))?;
    /// let first = session.events().recv_timeout(std::time::Duration::from_secs(1));
    /// # Ok::<(), yien_pty::PtyError>(())
    /// ```
    pub fn events(&self) -> &Receiver<PtyEvent> {
        &self.events
    }

    /// The input side, to share by reference.
    ///
    /// ```no_run
    /// # let session = yien_pty::PtyBackend::spawn(&yien_pty::PortablePty::new(), yien_pty::SpawnSpec::new("sh"))?;
    /// let control = session.control();
    /// control.resize(100, 30)?;
    /// # Ok::<(), yien_pty::PtyError>(())
    /// ```
    pub fn control(&self) -> &PtyControl {
        &self.control
    }

    /// Splits the session so events can be read on another thread.
    ///
    /// ```no_run
    /// # let session = yien_pty::PtyBackend::spawn(&yien_pty::PortablePty::new(), yien_pty::SpawnSpec::new("sh"))?;
    /// let (control, events) = session.into_parts();
    /// # Ok::<(), yien_pty::PtyError>(())
    /// ```
    pub fn into_parts(self) -> (PtyControl, Receiver<PtyEvent>) {
        (self.control, self.events)
    }

    /// See [`PtyControl::write`].
    ///
    /// ```no_run
    /// # let session = yien_pty::PtyBackend::spawn(&yien_pty::PortablePty::new(), yien_pty::SpawnSpec::new("sh"))?;
    /// session.write(b"ls\n".to_vec())?;
    /// # Ok::<(), yien_pty::PtyError>(())
    /// ```
    pub fn write(&self, bytes: Vec<u8>) -> Result<(), PtyError> {
        self.control.write(bytes)
    }

    /// See [`PtyControl::resize`].
    ///
    /// ```no_run
    /// # let session = yien_pty::PtyBackend::spawn(&yien_pty::PortablePty::new(), yien_pty::SpawnSpec::new("sh"))?;
    /// session.resize(120, 40)?;
    /// # Ok::<(), yien_pty::PtyError>(())
    /// ```
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), PtyError> {
        self.control.resize(cols, rows)
    }

    /// See [`PtyControl::kill`].
    ///
    /// ```no_run
    /// # let session = yien_pty::PtyBackend::spawn(&yien_pty::PortablePty::new(), yien_pty::SpawnSpec::new("sh"))?;
    /// session.kill()?;
    /// # Ok::<(), yien_pty::PtyError>(())
    /// ```
    pub fn kill(&self) -> Result<(), PtyError> {
        self.control.kill()
    }

    /// See [`PtyControl::pid`].
    ///
    /// ```no_run
    /// # let session = yien_pty::PtyBackend::spawn(&yien_pty::PortablePty::new(), yien_pty::SpawnSpec::new("sh"))?;
    /// assert!(session.pid().is_some());
    /// # Ok::<(), yien_pty::PtyError>(())
    /// ```
    pub fn pid(&self) -> Option<u32> {
        self.control.pid()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_shows_args_env_values_or_output() {
        let spec = SpawnSpec::new("claude")
            .arg("--token=secret-arg")
            .env("API_KEY", "secret-env");
        let shown = format!("{spec:?}");
        assert!(!shown.contains("secret"), "{shown}");
        assert!(
            shown.contains("API_KEY") && shown.contains("1 args"),
            "{shown}"
        );

        let output = format!("{:?}", PtyEvent::Output(b"secret-out".to_vec()));
        assert_eq!(output, "Output(10 bytes)");
        let input = format!("{:?}", WriterMsg::Input(b"secret-in".to_vec()));
        assert_eq!(input, "Input(9 bytes)");
    }
}
