//! When a watched file is read again. The watcher polls a stamp (modified
//! time and length) and asks the gate; time comes in as a value.

use std::time::Duration;

/// What the watcher does after a poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reload {
    /// Nothing new, or a change still settling.
    Wait,
    /// Read the file now.
    Apply,
}

/// Decides when a changed file is read. A new stamp must be seen on two
/// polls at least `settle` apart before it is applied, so a save still being
/// written is not read half-way, and a stamp is applied once.
///
/// ```
/// use std::time::Duration;
/// use yien_config::{Reload, ReloadGate};
///
/// let ms = Duration::from_millis;
/// let mut gate = ReloadGate::new(Some(1), ms(300));
/// assert_eq!(gate.observe(Some(1), ms(0)), Reload::Wait); // unchanged
/// assert_eq!(gate.observe(Some(2), ms(500)), Reload::Wait); // changed, settling
/// assert_eq!(gate.observe(Some(2), ms(1000)), Reload::Apply);
/// assert_eq!(gate.observe(Some(2), ms(1500)), Reload::Wait); // already applied
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReloadGate<S> {
    applied: S,
    /// What was applied before the last [`Reload::Apply`], for [`Self::retry`].
    previous: Option<S>,
    seen: Option<(S, Duration)>,
    settle: Duration,
}

impl<S: PartialEq + Clone> ReloadGate<S> {
    /// A gate whose current file has stamp `applied`.
    pub fn new(applied: S, settle: Duration) -> Self {
        Self {
            applied,
            previous: None,
            seen: None,
            settle,
        }
    }

    /// The stamp polled at `now`, a time that never goes back.
    pub fn observe(&mut self, stamp: S, now: Duration) -> Reload {
        if stamp == self.applied {
            self.seen = None;
            return Reload::Wait;
        }
        match &self.seen {
            Some((seen, since)) if *seen == stamp => {
                if now.saturating_sub(*since) >= self.settle {
                    self.previous = Some(std::mem::replace(&mut self.applied, stamp));
                    self.seen = None;
                    Reload::Apply
                } else {
                    Reload::Wait
                }
            }
            _ => {
                self.seen = Some((stamp, now));
                Reload::Wait
            }
        }
    }
}

impl<S> ReloadGate<S> {
    /// Undoes the last [`Reload::Apply`]: the read it asked for failed for a
    /// passing reason (the file held open by an editor saving it), so the
    /// same stamp is applied again once it holds for the settle time.
    pub fn retry(&mut self) {
        if let Some(previous) = self.previous.take() {
            self.applied = previous;
            self.seen = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn rf_cfg_01_a_change_applies_once_it_holds_for_the_settle_time() {
        let mut gate = ReloadGate::new(10, ms(300));
        assert_eq!(gate.observe(11, ms(0)), Reload::Wait);
        assert_eq!(gate.observe(11, ms(200)), Reload::Wait, "not settled yet");
        assert_eq!(gate.observe(11, ms(300)), Reload::Apply);
        assert_eq!(gate.observe(11, ms(800)), Reload::Wait, "applied once");
    }

    #[test]
    fn rf_cfg_01_a_save_still_changing_restarts_the_wait() {
        let mut gate = ReloadGate::new(10, ms(300));
        assert_eq!(gate.observe(11, ms(0)), Reload::Wait);
        assert_eq!(gate.observe(12, ms(500)), Reload::Wait, "changed again");
        assert_eq!(gate.observe(12, ms(700)), Reload::Wait);
        assert_eq!(gate.observe(12, ms(800)), Reload::Apply);
    }

    #[test]
    fn rf_cfg_01_one_poll_is_never_enough_even_after_a_long_gap() {
        let mut gate = ReloadGate::new(10, ms(0));
        assert_eq!(gate.observe(11, ms(60_000)), Reload::Wait);
        assert_eq!(gate.observe(11, ms(60_000)), Reload::Apply);
    }

    #[test]
    fn rf_cfg_01_going_back_to_the_applied_file_drops_the_pending_change() {
        let mut gate = ReloadGate::new(10, ms(300));
        assert_eq!(gate.observe(11, ms(0)), Reload::Wait);
        assert_eq!(gate.observe(10, ms(100)), Reload::Wait);
        assert_eq!(gate.observe(11, ms(500)), Reload::Wait, "seen anew");
        assert_eq!(gate.observe(11, ms(800)), Reload::Apply);
    }

    #[test]
    fn rf_cfg_01_a_failed_read_is_retried_on_later_polls() {
        let mut gate = ReloadGate::new(10, ms(300));
        assert_eq!(gate.observe(11, ms(0)), Reload::Wait);
        assert_eq!(gate.observe(11, ms(500)), Reload::Apply);
        gate.retry();
        assert_eq!(gate.observe(11, ms(1000)), Reload::Wait, "settles again");
        assert_eq!(gate.observe(11, ms(1500)), Reload::Apply);
        // Nothing to undo twice.
        gate.retry();
        gate.retry();
        assert_eq!(gate.observe(10, ms(2000)), Reload::Wait);
    }

    #[test]
    fn rf_cfg_01_a_removed_file_is_a_change_too() {
        let mut gate = ReloadGate::new(Some(10), ms(0));
        assert_eq!(gate.observe(None, ms(0)), Reload::Wait);
        assert_eq!(gate.observe(None, ms(500)), Reload::Apply);
        assert_eq!(gate.observe(Some(10), ms(1000)), Reload::Wait);
        assert_eq!(gate.observe(Some(10), ms(1500)), Reload::Apply);
    }
}
