//! When to ask GitHub again, and what the state dir keeps between starts:
//! the time of the last answer and the newest release it gave, so a start
//! within the day shows the notice again without a request.

use std::time::Duration;

use semver::Version;
use serde::{Deserialize, Serialize};

use super::release::Release;

/// The least time between two answered checks.
pub(crate) const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// The least time before a failed check is tried again (offline, rate
/// limited); not kept across starts.
pub(crate) const RETRY_AFTER: Duration = Duration::from_secs(60 * 60);

/// The file under the state dir.
pub(crate) const STATE_FILE: &str = "update.json";

/// What the state dir keeps.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Saved {
    /// Seconds since the Unix epoch of the last answered check.
    pub(crate) checked_at: Option<u64>,
    /// The newest installable release that answer gave, whatever its version.
    pub(crate) newest: Option<Release>,
}

impl Saved {
    /// The saved state; anything unreadable is a fresh start.
    pub(crate) fn from_bytes(bytes: Option<&[u8]>) -> Self {
        bytes
            .and_then(|b| serde_json::from_slice(b).ok())
            .unwrap_or_default()
    }

    /// The saved release when it is newer than `current`.
    pub(crate) fn offer(&self, current: &Version) -> Option<&Release> {
        self.newest
            .as_ref()
            .filter(|r| r.semver().is_some_and(|v| &v > current))
    }
}

/// What the checker does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Next {
    /// The setting is off: no request, wait for it to change.
    Off,
    /// Ask GitHub now.
    Check,
    /// Wait this long, then decide again.
    Wait(Duration),
}

/// Decides from the last answered check, the last failure in this run and
/// the time now, all in seconds since the epoch. A check time in the
/// future (a clock set back) counts as due, so a wrong clock cannot stop
/// the checks.
pub(crate) fn next(
    enabled: bool,
    checked_at: Option<u64>,
    failed_at: Option<u64>,
    now: u64,
) -> Next {
    if !enabled {
        return Next::Off;
    }
    let left = |since: Option<u64>, every: Duration| -> u64 {
        match since {
            Some(at) if at <= now => every.as_secs().saturating_sub(now - at),
            _ => 0,
        }
    };
    let wait = left(checked_at, CHECK_EVERY).max(left(failed_at, RETRY_AFTER));
    if wait == 0 {
        Next::Check
    } else {
        Next::Wait(Duration::from_secs(wait))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::release::releases;
    use crate::update::release::tests::entry;

    const DAY: u64 = 24 * 60 * 60;
    const T: u64 = 1_800_000_000;

    #[test]
    fn at_most_one_check_a_day() {
        assert_eq!(next(true, None, None, T), Next::Check, "never checked");
        assert_eq!(
            next(true, Some(T), None, T + 10),
            Next::Wait(Duration::from_secs(DAY - 10))
        );
        assert_eq!(
            next(true, Some(T), None, T + DAY - 1),
            Next::Wait(Duration::from_secs(1))
        );
        assert_eq!(next(true, Some(T), None, T + DAY), Next::Check);
        assert_eq!(next(true, Some(T), None, T + 3 * DAY), Next::Check);
    }

    #[test]
    fn off_means_no_check_whatever_the_time() {
        assert_eq!(next(false, None, None, T), Next::Off);
        assert_eq!(next(false, Some(T - 9 * DAY), None, T), Next::Off);
    }

    #[test]
    fn a_failure_waits_an_hour_and_an_answer_a_day() {
        assert_eq!(
            next(true, None, Some(T), T + 60),
            Next::Wait(Duration::from_secs(3600 - 60))
        );
        assert_eq!(next(true, None, Some(T), T + 3600), Next::Check);
        assert_eq!(
            next(true, Some(T), Some(T + 3600), T + 7200),
            Next::Wait(Duration::from_secs(DAY - 7200)),
            "the longer wait wins"
        );
    }

    #[test]
    fn a_clock_set_back_does_not_block_checks() {
        assert_eq!(next(true, Some(T + 5 * DAY), None, T), Next::Check);
        assert_eq!(next(true, None, Some(T + 60), T), Next::Check);
    }

    #[test]
    fn the_saved_release_survives_a_restart_and_is_offered_only_while_newer() {
        let json = serde_json::to_vec(&serde_json::Value::Array(vec![entry(
            "v0.2.0", false, true,
        )]))
        .unwrap();
        let saved = Saved {
            checked_at: Some(T),
            newest: releases(&json).unwrap().pop(),
        };
        let bytes = serde_json::to_vec(&saved).unwrap();
        let back = Saved::from_bytes(Some(&bytes));
        assert_eq!(back, saved);
        let current = |v: &str| Version::parse(v).unwrap();
        assert_eq!(
            back.offer(&current("0.1.0")).map(|r| r.version.as_str()),
            Some("0.2.0")
        );
        assert_eq!(back.offer(&current("0.2.0")), None, "installed since");
        assert_eq!(Saved::from_bytes(None), Saved::default());
        assert_eq!(Saved::from_bytes(Some(b"{garbage")), Saved::default());
    }
}
