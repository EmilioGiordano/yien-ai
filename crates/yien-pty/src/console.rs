//! A session's pseudo console, which any thread can close, and the signal
//! that it is closed and its child has exited.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Default)]
struct State {
    exited: bool,
    closed: bool,
}

/// Owns the master side of a PTY until it is closed. On Windows dropping
/// it calls `ClosePseudoConsole`, which can block until the console's output
/// is drained, so it is dropped on a thread that can afford to wait.
pub(crate) struct Console<M> {
    master: Mutex<Option<M>>,
    state: Mutex<State>,
    changed: Condvar,
    closing: AtomicBool,
}

impl<M: Send + 'static> Console<M> {
    pub fn new(master: M) -> Self {
        Self {
            master: Mutex::new(Some(master)),
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            closing: AtomicBool::new(false),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn update(&self, f: impl FnOnce(&mut State)) {
        f(&mut self.state());
        self.changed.notify_all();
    }

    /// Runs `f` on the master while it is open.
    pub fn with_master(&self, f: impl FnOnce(&M)) {
        let master = self.master.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(master) = master.as_ref() {
            f(master);
        }
    }

    /// Drops the master on the calling thread. Only the caller that drops it
    /// marks the console closed, and only once the drop has returned.
    pub fn close(&self) {
        let master = self
            .master
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(master) = master {
            drop(master);
            self.update(|s| s.closed = true);
        }
    }

    /// Closes the console on a thread of its own; later calls do nothing.
    /// Never blocks.
    pub fn close_in_background(self: &Arc<Self>) -> io::Result<()> {
        if self.closing.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let console = Arc::clone(self);
        let spawned = thread::Builder::new()
            .name("yien-pty-close".into())
            .spawn(move || console.close());
        if spawned.is_err() {
            self.closing.store(false, Ordering::SeqCst);
        }
        spawned.map(drop)
    }

    /// Records that the child has exited.
    pub fn mark_exited(&self) {
        self.update(|s| s.exited = true);
    }

    /// Whether the console is closed and the child has exited.
    pub fn is_closed(&self) -> bool {
        let state = self.state();
        state.closed && state.exited
    }

    /// Waits until [`Console::is_closed`] holds, at most `timeout`.
    pub fn wait(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.state();
        while !(state.closed && state.exited) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            state = self
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::mpsc::{Receiver, sync_channel};

    use super::*;
    use crate::WriterMsg;
    use crate::pump::run_writer;

    /// A master whose drop blocks until the test lets it go, as
    /// `ClosePseudoConsole` does while nobody drains the output.
    struct StuckMaster(Receiver<()>);

    impl Drop for StuckMaster {
        fn drop(&mut self) {
            let _ = self.0.recv();
        }
    }

    #[test]
    fn closing_in_the_background_does_not_wait_for_the_drop_and_wait_is_bounded() {
        let (release, released) = sync_channel(1);
        let console = Arc::new(Console::new(StuckMaster(released)));
        console.mark_exited();
        let start = Instant::now();
        console.close_in_background().unwrap();
        assert!(!console.wait(Duration::from_millis(100)));
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(!console.is_closed());

        release.send(()).unwrap();
        assert!(console.wait(Duration::from_secs(5)));
    }

    #[test]
    fn closed_needs_both_the_console_closed_and_the_child_exited() {
        let console = Arc::new(Console::new(()));
        console.close_in_background().unwrap();
        assert!(!console.wait(Duration::from_millis(50)));
        console.mark_exited();
        assert!(console.wait(Duration::from_secs(5)));
        assert!(console.is_closed());
    }

    #[test]
    fn a_writer_stuck_in_a_write_does_not_hold_the_console_open() {
        /// Blocks in `write` forever, as a pipe whose reader never reads.
        struct Stuck(Receiver<()>);
        impl Write for Stuck {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                let _ = self.0.recv();
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let console = Arc::new(Console::new(()));
        let (input, input_rx) = sync_channel(4);
        let (_unblock, blocked) = sync_channel::<()>(1);
        let writer = {
            let console = Arc::clone(&console);
            thread::spawn(move || {
                run_writer(Stuck(blocked), |_, _| {}, input_rx);
                console.close();
            })
        };
        input.send(WriterMsg::Input(b"x".to_vec())).unwrap();
        console.mark_exited();
        console.close_in_background().unwrap();
        assert!(console.wait(Duration::from_secs(5)));
        assert!(!writer.is_finished());
    }

    #[test]
    fn a_resize_after_the_close_does_nothing() {
        let console = Console::new(7);
        let mut seen = Vec::new();
        console.with_master(|m| seen.push(*m));
        console.close();
        console.with_master(|m| seen.push(*m));
        assert_eq!(seen, [7]);
    }
}
