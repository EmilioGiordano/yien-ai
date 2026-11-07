//! A scripted backend for tests that need a PTY but not a process.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::{
    ChildControl, ExitStatus, INPUT_QUEUE_CAPACITY, OUTPUT_QUEUE_CAPACITY, PtyBackend, PtyControl,
    PtyError, PtyEvent, PtySession, SpawnSpec, WriterMsg, check_size,
};

/// The exit code a killed fake child reports, as `TerminateProcess` does in
/// [`crate::PortablePty`].
///
/// ```
/// use yien_pty::{FakePty, PtyBackend, PtyEvent, SpawnSpec, KILLED_EXIT_CODE};
/// let session = FakePty::new().spawn(SpawnSpec::new("sh")).unwrap();
/// session.kill().unwrap();
/// assert!(matches!(session.events().try_recv(), Ok(PtyEvent::Exited(s)) if s.code == Some(KILLED_EXIT_CODE)));
/// ```
pub const KILLED_EXIT_CODE: u32 = 1;

/// A backend whose children are driven by the test through [`FakeChild`].
///
/// ```
/// use std::time::Duration;
/// use yien_pty::{FakeInput, FakePty, PtyBackend, PtyEvent, SpawnSpec};
///
/// let backend = FakePty::new();
/// let session = backend.spawn(SpawnSpec::new("claude")).unwrap();
/// let child = backend.take_spawned().pop().unwrap();
///
/// session.write(b"hi".to_vec()).unwrap();
/// assert_eq!(child.recv_input(Duration::from_secs(1)), Some(FakeInput::Bytes(b"hi".to_vec())));
/// child.output(b"hello");
/// child.exit(0);
/// let events: Vec<_> = session.events().try_iter().collect();
/// assert_eq!(events.len(), 2);
/// ```
#[derive(Debug, Clone, Default)]
pub struct FakePty {
    spawned: Arc<Mutex<Vec<FakeChild>>>,
    refuse: Arc<AtomicBool>,
    batch: Arc<AtomicBool>,
    stall: Arc<AtomicBool>,
}

impl FakePty {
    /// A backend that accepts every spawn.
    ///
    /// ```
    /// let backend = yien_pty::FakePty::new();
    /// assert!(backend.take_spawned().is_empty());
    /// ```
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes the following spawns fail with [`PtyError::Spawn`], as a missing
    /// program would.
    ///
    /// ```
    /// use yien_pty::{FakePty, PtyBackend, SpawnSpec};
    /// let backend = FakePty::new();
    /// backend.refuse_spawns(true);
    /// assert!(backend.spawn(SpawnSpec::new("codex")).is_err());
    /// ```
    pub fn refuse_spawns(&self, refuse: bool) {
        self.refuse.store(refuse, Ordering::SeqCst);
    }

    /// Makes the following programs count as batch files: a spawn with an
    /// argument `cmd.exe` would interpret fails with
    /// [`PtyError::UnsafeBatchArgument`], as a `.cmd` install would.
    ///
    /// ```
    /// use yien_pty::{FakePty, PtyBackend, PtyError, SpawnSpec};
    /// let backend = FakePty::new();
    /// backend.batch_programs(true);
    /// let err = backend.spawn(SpawnSpec::new("claude").arg("C:/John (Work)")).unwrap_err();
    /// assert!(matches!(err, PtyError::UnsafeBatchArgument { .. }));
    /// assert!(backend.spawn(SpawnSpec::new("claude").arg("C:/John")).is_ok());
    /// ```
    pub fn batch_programs(&self, batch: bool) {
        self.batch.store(batch, Ordering::SeqCst);
    }

    /// Makes the following children never confirm that their pseudo terminal
    /// closed, as a console stuck in `ClosePseudoConsole` would. They are
    /// still killed.
    ///
    /// ```
    /// use std::time::Duration;
    /// use yien_pty::{FakePty, PtyBackend, PtyError, SpawnSpec};
    /// let backend = FakePty::new();
    /// backend.stall_closes(true);
    /// let session = backend.spawn(SpawnSpec::new("sh")).unwrap();
    /// let closed = session.into_parts().0.close(Duration::from_millis(10));
    /// assert!(matches!(closed, Err(PtyError::CloseTimedOut)));
    /// assert!(backend.take_spawned()[0].was_killed());
    /// ```
    pub fn stall_closes(&self, stall: bool) {
        self.stall.store(stall, Ordering::SeqCst);
    }

    /// The children spawned since the last call, oldest first.
    ///
    /// ```
    /// use yien_pty::{FakePty, PtyBackend, SpawnSpec};
    /// let backend = FakePty::new();
    /// let _session = backend.spawn(SpawnSpec::new("sh")).unwrap();
    /// assert_eq!(backend.take_spawned()[0].spec().program, "sh");
    /// ```
    pub fn take_spawned(&self) -> Vec<FakeChild> {
        std::mem::take(&mut *self.spawned.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

impl PtyBackend for FakePty {
    fn spawn(&self, spec: SpawnSpec) -> Result<PtySession, PtyError> {
        check_size(spec.cols, spec.rows)?;
        if self.refuse.load(Ordering::SeqCst) {
            return Err(PtyError::Spawn {
                program: spec.program.to_string_lossy().into_owned(),
                source: "refused by FakePty".into(),
            });
        }
        if self.batch.load(Ordering::SeqCst)
            && spec.args.iter().any(|a| crate::program::batch_unsafe(a))
        {
            return Err(PtyError::UnsafeBatchArgument {
                program: spec.program.to_string_lossy().into_owned(),
            });
        }
        let (events_tx, events) = sync_channel(OUTPUT_QUEUE_CAPACITY);
        let (input, input_rx) = sync_channel(INPUT_QUEUE_CAPACITY);
        let state = Arc::new(FakeState {
            events: events_tx,
            exited: AtomicBool::new(false),
            killed: AtomicBool::new(false),
            close_asked: AtomicBool::new(false),
            stall: self.stall.load(Ordering::SeqCst),
            closed: Mutex::new(false),
            closed_changed: Condvar::new(),
        });
        let child = FakeChild {
            spec,
            state: Arc::clone(&state),
            input: Arc::new(Mutex::new(input_rx)),
        };
        self.spawned
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(child);
        let control = PtyControl::new(input, Arc::new(FakeControl(state)));
        Ok(PtySession::new(control, events))
    }
}

#[derive(Debug)]
struct FakeState {
    events: SyncSender<PtyEvent>,
    exited: AtomicBool,
    killed: AtomicBool,
    close_asked: AtomicBool,
    /// Never confirm the close (see [`FakePty::stall_closes`]).
    stall: bool,
    closed: Mutex<bool>,
    closed_changed: Condvar,
}

impl FakeState {
    /// Sends `Exited` once; later calls do nothing. `wait` blocks on a full
    /// queue; without it a full queue hands the event to a short-lived thread,
    /// so the caller returns at once and the event is still delivered.
    fn exit(&self, status: ExitStatus, wait: bool) {
        if !self.exited.swap(true, Ordering::SeqCst) {
            let event = PtyEvent::Exited(status);
            if wait {
                let _ = self.events.send(event);
            } else if let Err(TrySendError::Full(event)) = self.events.try_send(event) {
                let events = self.events.clone();
                std::thread::spawn(move || events.send(event));
            }
        }
    }
}

struct FakeControl(Arc<FakeState>);

impl ChildControl for FakeControl {
    fn kill(&self) -> Result<(), PtyError> {
        if !self.0.exited.load(Ordering::SeqCst) {
            self.0.killed.store(true, Ordering::SeqCst);
            self.0.exit(ExitStatus::from_code(KILLED_EXIT_CODE), false);
        }
        Ok(())
    }

    fn pid(&self) -> Option<u32> {
        None
    }

    fn start_close(&self) -> Result<(), PtyError> {
        self.kill()?;
        self.0.close_asked.store(true, Ordering::SeqCst);
        if !self.0.stall {
            *self.0.closed.lock().unwrap_or_else(PoisonError::into_inner) = true;
            self.0.closed_changed.notify_all();
        }
        Ok(())
    }

    fn is_closed(&self) -> bool {
        *self.0.closed.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wait_closed(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut closed = self.0.closed.lock().unwrap_or_else(PoisonError::into_inner);
        while !*closed {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            closed = self
                .0
                .closed_changed
                .wait_timeout(closed, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }
}

/// Something the session sent to a fake child.
///
/// ```
/// use yien_pty::FakeInput;
/// let input = FakeInput::Resize { cols: 80, rows: 24 };
/// assert_ne!(input, FakeInput::Bytes(Vec::new()));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeInput {
    /// Bytes from [`PtySession::write`].
    Bytes(Vec<u8>),
    /// A [`PtySession::resize`].
    Resize {
        /// New columns.
        cols: u16,
        /// New rows.
        rows: u16,
    },
}

/// The test's side of one fake child: script its output and exit, read what
/// the session sent it.
///
/// ```
/// use yien_pty::{FakePty, PtyBackend, PtyEvent, SpawnSpec};
///
/// let backend = FakePty::new();
/// let session = backend.spawn(SpawnSpec::new("sh")).unwrap();
/// let child = backend.take_spawned().remove(0);
/// child.exit(2);
/// assert!(matches!(session.events().try_recv(), Ok(PtyEvent::Exited(s)) if s.code == Some(2)));
/// ```
#[derive(Debug, Clone)]
pub struct FakeChild {
    spec: SpawnSpec,
    state: Arc<FakeState>,
    input: Arc<Mutex<Receiver<WriterMsg>>>,
}

impl FakeChild {
    /// The spec the session was spawned with.
    ///
    /// ```
    /// use yien_pty::{FakePty, PtyBackend, SpawnSpec};
    /// let backend = FakePty::new();
    /// let _s = backend.spawn(SpawnSpec::new("sh").size(90, 20)).unwrap();
    /// assert_eq!(backend.take_spawned()[0].spec().cols, 90);
    /// ```
    pub fn spec(&self) -> &SpawnSpec {
        &self.spec
    }

    /// Emits output, waiting while [`crate::OUTPUT_QUEUE_CAPACITY`] chunks are
    /// unread, as a real child stalls on a full PTY.
    ///
    /// ```
    /// use yien_pty::{FakePty, PtyBackend, PtyEvent, SpawnSpec};
    /// let backend = FakePty::new();
    /// let session = backend.spawn(SpawnSpec::new("sh")).unwrap();
    /// backend.take_spawned()[0].output(b"$ ");
    /// assert_eq!(session.events().try_recv(), Ok(PtyEvent::Output(b"$ ".to_vec())));
    /// ```
    pub fn output(&self, bytes: &[u8]) {
        let _ = self.state.events.send(PtyEvent::Output(bytes.to_vec()));
    }

    /// Ends the child with `code`. Only the first exit or kill counts.
    ///
    /// ```
    /// use yien_pty::{FakePty, PtyBackend, SpawnSpec};
    /// let backend = FakePty::new();
    /// let session = backend.spawn(SpawnSpec::new("sh")).unwrap();
    /// let child = backend.take_spawned().remove(0);
    /// child.exit(0);
    /// child.exit(5);
    /// assert_eq!(session.events().try_iter().count(), 1);
    /// ```
    pub fn exit(&self, code: u32) {
        self.state.exit(ExitStatus::from_code(code), true);
    }

    /// The next write or resize the session sent, or `None` after `timeout`.
    ///
    /// ```
    /// use std::time::Duration;
    /// use yien_pty::{FakeInput, FakePty, PtyBackend, SpawnSpec};
    /// let backend = FakePty::new();
    /// let session = backend.spawn(SpawnSpec::new("sh")).unwrap();
    /// session.resize(100, 30).unwrap();
    /// let child = backend.take_spawned().remove(0);
    /// assert_eq!(child.recv_input(Duration::from_secs(1)), Some(FakeInput::Resize { cols: 100, rows: 30 }));
    /// ```
    pub fn recv_input(&self, timeout: Duration) -> Option<FakeInput> {
        let input = self.input.lock().unwrap_or_else(PoisonError::into_inner);
        match input.recv_timeout(timeout) {
            Ok(WriterMsg::Input(bytes)) => Some(FakeInput::Bytes(bytes)),
            Ok(WriterMsg::Resize { cols, rows }) => Some(FakeInput::Resize { cols, rows }),
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => None,
        }
    }

    /// Whether the session killed the child, explicitly or by being dropped.
    ///
    /// ```
    /// use yien_pty::{FakePty, PtyBackend, SpawnSpec};
    /// let backend = FakePty::new();
    /// drop(backend.spawn(SpawnSpec::new("sh")).unwrap());
    /// assert!(backend.take_spawned()[0].was_killed());
    /// ```
    pub fn was_killed(&self) -> bool {
        self.state.killed.load(Ordering::SeqCst)
    }

    /// Whether the session was asked to close its pseudo terminal, through
    /// a close or by being dropped.
    ///
    /// ```
    /// use std::time::Duration;
    /// use yien_pty::{FakePty, PtyBackend, SpawnSpec};
    /// let backend = FakePty::new();
    /// let session = backend.spawn(SpawnSpec::new("sh")).unwrap();
    /// let child = backend.take_spawned().remove(0);
    /// assert!(!child.was_closed());
    /// session.control().closer().close(Duration::from_secs(1)).unwrap();
    /// assert!(child.was_closed());
    /// ```
    pub fn was_closed(&self) -> bool {
        self.state.close_asked.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ReturnedBytes;

    const TIMEOUT: Duration = Duration::from_secs(5);

    fn spawn_one() -> (PtySession, FakeChild) {
        let backend = FakePty::new();
        let session = backend.spawn(SpawnSpec::new("sh")).unwrap();
        let child = backend.take_spawned().remove(0);
        (session, child)
    }

    #[test]
    fn scripted_output_and_exit_arrive_in_order() {
        let (session, child) = spawn_one();
        child.output(b"one");
        child.output(b"two");
        child.exit(4);
        let events: Vec<_> = (0..3)
            .map(|_| session.events().recv_timeout(TIMEOUT).unwrap())
            .collect();
        assert_eq!(
            events,
            [
                PtyEvent::Output(b"one".to_vec()),
                PtyEvent::Output(b"two".to_vec()),
                PtyEvent::Exited(ExitStatus::from_code(4)),
            ]
        );
    }

    #[test]
    fn a_full_input_queue_hands_the_bytes_back() {
        let (session, child) = spawn_one();
        for _ in 0..INPUT_QUEUE_CAPACITY {
            session.write(b"x".to_vec()).unwrap();
        }
        match session.write(b"kept".to_vec()) {
            Err(PtyError::InputQueueFull(bytes)) => {
                assert_eq!(bytes, ReturnedBytes::new(b"kept".to_vec()))
            }
            other => panic!("expected InputQueueFull, got {other:?}"),
        }
        assert!(matches!(
            session.resize(10, 10),
            Err(PtyError::ResizeQueueFull)
        ));
        assert_eq!(
            child.recv_input(TIMEOUT),
            Some(FakeInput::Bytes(b"x".to_vec()))
        );
        session.write(b"fits".to_vec()).unwrap();
    }

    #[test]
    fn input_and_resizes_reach_the_child_in_order() {
        let (session, child) = spawn_one();
        session.write(b"a".to_vec()).unwrap();
        session.resize(120, 40).unwrap();
        session.write(b"b".to_vec()).unwrap();
        assert_eq!(
            child.recv_input(TIMEOUT),
            Some(FakeInput::Bytes(b"a".to_vec()))
        );
        assert_eq!(
            child.recv_input(TIMEOUT),
            Some(FakeInput::Resize {
                cols: 120,
                rows: 40
            })
        );
        assert_eq!(
            child.recv_input(TIMEOUT),
            Some(FakeInput::Bytes(b"b".to_vec()))
        );
        assert_eq!(child.recv_input(Duration::from_millis(20)), None);
    }

    #[test]
    fn a_zero_size_is_refused() {
        let (session, _child) = spawn_one();
        assert!(matches!(
            session.resize(0, 10),
            Err(PtyError::InvalidSize { cols: 0, rows: 10 })
        ));
        assert!(matches!(
            FakePty::new().spawn(SpawnSpec::new("sh").size(80, 0)),
            Err(PtyError::InvalidSize { .. })
        ));
    }

    #[test]
    fn kill_reports_an_exit_once_and_is_harmless_after_exit() {
        let (session, child) = spawn_one();
        session.kill().unwrap();
        session.kill().unwrap();
        assert!(child.was_killed());
        assert_eq!(
            session.events().recv_timeout(TIMEOUT).unwrap(),
            PtyEvent::Exited(ExitStatus::from_code(KILLED_EXIT_CODE))
        );
        assert!(session.events().try_recv().is_err());

        let (session, child) = spawn_one();
        child.exit(0);
        session.kill().unwrap();
        assert!(!child.was_killed());
    }

    #[test]
    fn a_kill_on_a_full_queue_returns_at_once_and_still_delivers_exited() {
        let (session, child) = spawn_one();
        for _ in 0..OUTPUT_QUEUE_CAPACITY {
            child.output(b"x");
        }
        let start = std::time::Instant::now();
        session.kill().unwrap();
        assert!(start.elapsed() < Duration::from_secs(1));
        let mut outputs = 0;
        loop {
            match session.events().recv_timeout(TIMEOUT).unwrap() {
                PtyEvent::Output(_) => outputs += 1,
                PtyEvent::Exited(status) => {
                    assert_eq!(status, ExitStatus::from_code(KILLED_EXIT_CODE));
                    break;
                }
            }
        }
        assert_eq!(outputs, OUTPUT_QUEUE_CAPACITY);
        assert!(child.was_killed());
    }

    #[test]
    fn writes_after_the_child_is_gone_report_closed() {
        let (session, child) = spawn_one();
        session.write(b"ok".to_vec()).unwrap();
        drop(child);
        assert!(matches!(
            session.write(b"late".to_vec()),
            Err(PtyError::Closed)
        ));
        assert!(matches!(session.resize(80, 24), Err(PtyError::Closed)));
    }

    #[test]
    fn a_refused_spawn_names_the_program() {
        let backend = FakePty::new();
        backend.refuse_spawns(true);
        match backend.spawn(SpawnSpec::new("codex")) {
            Err(PtyError::Spawn { program, .. }) => assert_eq!(program, "codex"),
            other => panic!("expected Spawn, got {other:?}"),
        }
        assert!(backend.take_spawned().is_empty());
    }
}
