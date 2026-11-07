//! Every pane's pseudo terminal that may still be open, so the quit path can
//! close them all without waiting on the service or the pane actors.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use yien_proto::PaneId;
use yien_pty::PtyCloser;

#[derive(Debug, Default)]
pub(crate) struct Consoles {
    state: Mutex<State>,
    spawned: Condvar,
}

#[derive(Debug, Default)]
struct State {
    /// Each session not known to be closed, with the pane it runs in.
    open: Vec<(PaneId, PtyCloser)>,
    /// Spawns under way: their sessions are not registered yet.
    spawning: usize,
    /// The pane of each spawn under way, repeats included.
    spawning_panes: Vec<PaneId>,
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
    pane: PaneId,
}

impl Consoles {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Announces a spawn. `None` once shut: no program may start while the
    /// app quits.
    pub fn begin(self: &Arc<Self>, pane: PaneId) -> Option<Spawning> {
        let mut state = self.state();
        if state.shut {
            return None;
        }
        state.spawning += 1;
        state.spawning_panes.push(pane);
        Some(Spawning {
            consoles: Arc::clone(self),
            pane,
        })
    }

    /// Stops taking spawns and hands over the sessions not closed yet,
    /// including ones whose program exited but whose console is still
    /// closing.
    pub fn shut(&self) -> Vec<PtyCloser> {
        let mut state = self.state();
        state.shut = true;
        let open = std::mem::take(&mut state.open);
        open.into_iter()
            .map(|(_, c)| c)
            .filter(|c| !c.is_closed())
            .collect()
    }

    /// The sessions of `panes` not known to be closed, once the spawns of
    /// those panes under way have registered theirs or ended (waiting at
    /// most `timeout`), so a worktree is removed only once the programs that
    /// ran in it have ended. Blocks: call it off the runtime's workers.
    pub fn closers_after_spawns(&self, panes: &[PaneId], timeout: Duration) -> Vec<PtyCloser> {
        let deadline = Instant::now() + timeout;
        let mut state = self.state();
        while state.spawning_panes.iter().any(|p| panes.contains(p)) {
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
        state
            .open
            .iter()
            .filter(|(pane, c)| panes.contains(pane) && !c.is_closed())
            .map(|(_, c)| c.clone())
            .collect()
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
        state.open.retain(|(_, c)| !c.is_closed());
        state.open.push((self.pane, closer));
    }
}

impl Drop for Spawning {
    fn drop(&mut self) {
        let mut state = self.consoles.state();
        state.spawning = state.spawning.saturating_sub(1);
        if let Some(at) = state.spawning_panes.iter().position(|p| *p == self.pane) {
            state.spawning_panes.swap_remove(at);
        }
        drop(state);
        self.consoles.spawned.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use yien_pty::{FakePty, PtyBackend, PtySession, SpawnSpec};

    use super::*;

    fn spawn(backend: &FakePty, consoles: &Arc<Consoles>) -> PtySession {
        let spawning = consoles.begin(PaneId::new(1)).unwrap();
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
        assert!(consoles.begin(PaneId::new(1)).is_none());
        assert!(consoles.shut().is_empty());
    }

    #[test]
    fn a_spawn_under_way_at_the_shut_is_waited_for_and_closed() {
        let backend = FakePty::new();
        let consoles = Arc::new(Consoles::default());
        let spawning = consoles.begin(PaneId::new(1)).unwrap();
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
    fn closers_are_found_by_pane_until_closed() {
        let backend = FakePty::new();
        let consoles = Arc::new(Consoles::default());
        let spawning = consoles.begin(PaneId::new(7)).unwrap();
        let session = backend.spawn(SpawnSpec::new("sh")).unwrap();
        spawning.add(session.control().closer());
        let _other = spawn(&backend, &consoles);

        let of = |pane: u64| consoles.closers_after_spawns(&[PaneId::new(pane)], Duration::ZERO);
        assert_eq!(of(7).len(), 1);
        assert!(of(9).is_empty());
        session
            .control()
            .closer()
            .close(Duration::from_secs(1))
            .unwrap();
        assert!(of(7).is_empty());
    }

    #[test]
    fn closers_wait_for_a_spawn_of_their_pane_under_way() {
        let backend = FakePty::new();
        let consoles = Arc::new(Consoles::default());
        let spawning = consoles.begin(PaneId::new(3)).unwrap();
        let _other = consoles.begin(PaneId::new(4)).unwrap();
        let waiter = {
            let consoles = Arc::clone(&consoles);
            std::thread::spawn(move || {
                consoles.closers_after_spawns(&[PaneId::new(3)], Duration::from_secs(5))
            })
        };
        std::thread::sleep(Duration::from_millis(50));
        let session = backend.spawn(SpawnSpec::new("sh")).unwrap();
        spawning.add(session.control().closer());
        drop(spawning);
        assert_eq!(
            waiter.join().unwrap().len(),
            1,
            "the late session is waited for"
        );
        let start = Instant::now();
        assert!(
            consoles
                .closers_after_spawns(&[PaneId::new(4)], Duration::from_millis(100))
                .is_empty()
        );
        assert!(start.elapsed() < Duration::from_secs(2));
        drop(session);
    }

    #[test]
    fn waiting_for_a_spawn_that_never_ends_is_bounded() {
        let consoles = Arc::new(Consoles::default());
        let _spawning = consoles.begin(PaneId::new(1)).unwrap();
        consoles.shut();
        let start = Instant::now();
        let late = consoles.wait_spawns(Duration::from_millis(100));
        assert_eq!((late.closers.len(), late.spawning), (0, 1));
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
