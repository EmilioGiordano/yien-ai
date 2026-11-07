//! Test doubles for YIEN. Only ever a dev-dependency: `xtask check-deps`
//! rejects it in any crate's `[dependencies]`.
//!
//! ```
//! use std::time::Duration;
//! use yien_testkit::FakeClock;
//!
//! let clock = FakeClock::new();
//! clock.advance(Duration::from_millis(250));
//! assert_eq!(clock.now(), Duration::from_millis(250));
//! ```

#![forbid(unsafe_code)]

use std::cell::Cell;
use std::time::Duration;

/// A monotonic clock that only moves when a test tells it to.
///
/// Time is the elapsed [`Duration`] since the clock was created, so tests can
/// state exact instants without depending on the wall clock.
///
/// ```
/// use std::time::Duration;
/// use yien_testkit::FakeClock;
///
/// let clock = FakeClock::starting_at(Duration::from_secs(10));
/// clock.advance(Duration::from_secs(1));
/// assert_eq!(clock.now(), Duration::from_secs(11));
/// ```
#[derive(Debug, Default)]
pub struct FakeClock {
    now: Cell<Duration>,
}

impl FakeClock {
    /// A clock at zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// A clock already at `start`.
    pub fn starting_at(start: Duration) -> Self {
        Self {
            now: Cell::new(start),
        }
    }

    /// The current instant.
    pub fn now(&self) -> Duration {
        self.now.get()
    }

    /// Moves the clock forward by `by`. Saturates instead of overflowing, so
    /// the clock never goes backwards.
    pub fn advance(&self, by: Duration) {
        self.now.set(self.now.get().saturating_add(by));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_at_zero() {
        assert_eq!(FakeClock::new().now(), Duration::ZERO);
    }

    #[test]
    fn only_moves_when_advanced() {
        let clock = FakeClock::new();
        let before = clock.now();
        assert_eq!(clock.now(), before);
        clock.advance(Duration::from_millis(5));
        clock.advance(Duration::from_millis(7));
        assert_eq!(clock.now(), Duration::from_millis(12));
    }

    #[test]
    fn advancing_by_zero_keeps_the_instant() {
        let clock = FakeClock::starting_at(Duration::from_secs(3));
        clock.advance(Duration::ZERO);
        assert_eq!(clock.now(), Duration::from_secs(3));
    }

    #[test]
    fn saturates_instead_of_wrapping() {
        let clock = FakeClock::starting_at(Duration::MAX - Duration::from_secs(1));
        clock.advance(Duration::from_secs(10));
        assert_eq!(clock.now(), Duration::MAX);
        clock.advance(Duration::from_secs(10));
        assert_eq!(clock.now(), Duration::MAX);
    }
}
