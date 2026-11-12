//! Text programs asked to put on the clipboard, one waiting slot per pane.
//! The UI is told once per waiting write and takes the latest text, so a
//! program looping large writes while the UI is stalled holds one write per
//! pane in memory, not one per queued event.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use yien_proto::PaneId;

#[derive(Debug, Default)]
pub(crate) struct Clipboards(Mutex<HashMap<PaneId, String>>);

impl Clipboards {
    fn slots(&self) -> MutexGuard<'_, HashMap<PaneId, String>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Stores `text` as the pane's latest write. Returns whether the slot was
    /// empty, that is, whether the UI is owed a notice.
    pub fn put(&self, pane: PaneId, text: String) -> bool {
        self.slots().insert(pane, text).is_none()
    }

    /// Takes the pane's waiting write; the next write is announced again.
    pub fn take(&self, pane: PaneId) -> Option<String> {
        self.slots().remove(&pane)
    }

    /// Drops what a closed pane left waiting.
    pub fn forget(&self, pane: PaneId) {
        self.slots().remove(&pane);
    }

    #[cfg(test)]
    fn waiting(&self) -> usize {
        self.slots().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rf_pan_09_a_burst_leaves_one_write_per_pane_and_one_notice() {
        let slots = Clipboards::default();
        let (a, b) = (PaneId::new(1), PaneId::new(2));
        assert!(slots.put(a, "first".into()));
        for i in 0..1000 {
            assert!(!slots.put(a, format!("again {i}")), "no further notice");
        }
        assert!(slots.put(b, "other pane".into()));
        assert_eq!(slots.waiting(), 2);
        assert_eq!(slots.take(a).as_deref(), Some("again 999"));
        assert_eq!(slots.take(a), None);
        assert!(slots.put(a, "after the take".into()), "told again");
    }

    #[test]
    fn rf_pan_09_a_closed_pane_keeps_nothing() {
        let slots = Clipboards::default();
        slots.put(PaneId::new(1), "x".into());
        slots.forget(PaneId::new(1));
        assert_eq!(slots.waiting(), 0);
    }
}
