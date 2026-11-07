//! Every pane's pseudo terminal that may still be open, so the quit path can
//! close them all without waiting on the service or the pane actors.

use std::sync::{Mutex, MutexGuard, PoisonError};

use yien_pty::PtyCloser;

#[derive(Debug, Default)]
pub(crate) struct Consoles(Mutex<State>);

#[derive(Debug, Default)]
struct State {
    open: Vec<PtyCloser>,
    shut: bool,
}

impl Consoles {
    fn state(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Keeps a new session's closer for [`Consoles::shut`]. Once shut, the
    /// session is closed at once instead: a pane must not start a program
    /// while the app quits.
    pub fn add(&self, closer: PtyCloser) {
        let mut state = self.state();
        if state.shut {
            drop(state);
            let _ = closer.start();
            return;
        }
        state.open.retain(|c| !c.is_closed());
        state.open.push(closer);
    }

    /// Stops taking sessions and hands over those not closed yet, including
    /// ones whose program exited but whose console is still closing.
    pub fn shut(&self) -> Vec<PtyCloser> {
        let mut state = self.state();
        state.shut = true;
        let mut open = std::mem::take(&mut state.open);
        open.retain(|c| !c.is_closed());
        open
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use yien_pty::{FakePty, PtyBackend, SpawnSpec};

    use super::*;

    #[test]
    fn closed_sessions_are_dropped_and_sessions_after_the_shut_are_closed_at_once() {
        let backend = FakePty::new();
        let consoles = Consoles::default();
        let first = backend.spawn(SpawnSpec::new("sh")).unwrap();
        consoles.add(first.control().closer());
        let second = backend.spawn(SpawnSpec::new("sh")).unwrap();
        consoles.add(second.control().closer());
        first.control().closer().close(Duration::from_secs(1)).unwrap();

        let open = consoles.shut();
        assert_eq!(open.len(), 1);
        let late = backend.spawn(SpawnSpec::new("sh")).unwrap();
        consoles.add(late.control().closer());
        let children = backend.take_spawned();
        assert!(!children[1].was_closed());
        assert!(children[2].was_closed() && children[2].was_killed());
        assert!(consoles.shut().is_empty());
        drop((second, late));
    }
}
