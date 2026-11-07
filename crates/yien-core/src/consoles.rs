//! Every pane's pseudo terminal that may still be open, so the quit path can
//! close them all without waiting on the service or the pane actors.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use yien_pty::PtyCloser;

#[derive(Debug, Default)]
pub(crate) struct Consoles {
    state: Mutex<State>,
    spawned: Condvar,
}

#[derive(Debug, Default)]
struct State {
    open: Vec<PtyCloser>,
    /// Spawns under way: their sessions are not registered yet.
    spawning: usize,
    /// Sessions registered after the shut, already closing.
    late: Vec<PtyCloser>,
    shut: bool,
}

/// What [`Consoles::wait_spawns`] found.
#[derive(Debug)]
pub(crate) struct Late {
    /// Sessions that registered after the shut; their close has started.
    pub closers: Vec<PtyCloser>,
    /// Spawns still under way when the time ran out.
    pub spawning: usize,
}

/// A spawn under way; the shutdown waits for it to register its session or
/// end. Dropping it ends it.
#[derive(Debug)]
pub(crate) struct Spawning {
    consoles: Arc<Consoles>,
}

impl Consoles {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Announces a spawn. `None` once shut: no program may start while the
    /// app quits.
    pub fn begin(self: &Arc<Self>) -> Option<Spawning> {
        let mut state = self.state();
        if state.shut {
            return None;
        }
        state.spawning += 1;
        Some(Spawning {
            consoles: Arc::clone(self),
        })
    }

    /// Stops taking spawns and hands over the sessions not closed yet,
    /// including ones whose program exited but whose console is still
    /// closing.
    pub fn shut(&self) -> Vec<PtyCloser> {
        let mut state = self.state();
        state.shut = true;
        let mut open = std::mem::take(&mut state.open);
        open.retain(|c| !c.is_closed());
        open
    }

    /// Waits at most `timeout` for the spawns under way at the shut, and
    /// hands over the sessions they registered since.
    pub fn wait_spawns(&self, timeout: Duration) -> Late {
        let deadline = Instant::now() + timeout;
        let mut state = self.state();
        while state.spawning > 0 {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            state = self
                .spawned
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        Late {
            closers: std::mem::take(&mut state.late),
            spawning: state.spawning,
        }
    }
}

impl Spawning {
    /// Registers the spawned session. After the shut its close starts at
    /// once, and the shutdown still waits for it.
    pub fn add(&self, closer: PtyCloser) {
        let mut state = self.consoles.state();
        if state.shut {
            if let Err(e) = closer.start() {
                tracing::warn!("a pane started during the shutdown did not start closing: {e}");
            }
            state.late.push(closer);
            return;
        }
        state.open.retain(|c| !c.is_closed());
        state.open.push(closer);
    }
}

impl Drop for Spawning {
    fn drop(&mut self) {
        let mut state = self.consoles.state();
        state.spawning = state.spawning.saturating_sub(1);
        drop(state);
        self.consoles.spawned.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use yien_pty::{FakePty, PtyBackend, PtySession, SpawnSpec};

    use super::*;

    fn spawn(backend: &FakePty, consoles: &Arc<Consoles>) -> PtySession {
        let spawning = consoles.begin().unwrap();
        let session = backend.spawn(SpawnSpec::new("sh")).unwrap();
        spawning.add(session.control().closer());
        session
    }

    #[test]
    fn closed_sessions_are_dropped_and_nothing_spawns_after_the_shut() {
        let backend = FakePty::new();
        let consoles = Arc::new(Consoles::default());
        let first = spawn(&backend, &consoles);
        let _second = spawn(&backend, &consoles);
        first
            .control()
            .closer()
            .close(Duration::from_secs(1))
            .unwrap();

        assert_eq!(consoles.shut().len(), 1);
        assert!(consoles.begin().is_none());
        assert!(consoles.shut().is_empty());
    }

    #[test]
    fn a_spawn_under_way_at_the_shut_is_waited_for_and_closed() {
        let backend = FakePty::new();
        let consoles = Arc::new(Consoles::default());
        let spawning = consoles.begin().unwrap();
        assert!(consoles.shut().is_empty());

        let waiter = {
            let consoles = Arc::clone(&consoles);
            std::thread::spawn(move || consoles.wait_spawns(Duration::from_secs(5)))
        };
        std::thread::sleep(Duration::from_millis(50));
        let session = backend.spawn(SpawnSpec::new("sh")).unwrap();
        spawning.add(session.control().closer());
        drop(spawning);

        let late = waiter.join().unwrap();
        assert_eq!((late.closers.len(), late.spawning), (1, 0));
        let child = &backend.take_spawned()[0];
        assert!(child.was_closed() && child.was_killed());
        assert!(late.closers[0].wait(Duration::from_secs(1)));
        drop(session);
    }

    #[test]
    fn waiting_for_a_spawn_that_never_ends_is_bounded() {
        let consoles = Arc::new(Consoles::default());
        let _spawning = consoles.begin().unwrap();
        consoles.shut();
        let start = Instant::now();
        let late = consoles.wait_spawns(Duration::from_millis(100));
        assert_eq!((late.closers.len(), late.spawning), (0, 1));
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
