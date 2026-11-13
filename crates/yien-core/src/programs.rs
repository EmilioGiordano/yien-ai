//! The agent CLI the user typed in a shell pane, so its head names it
//! (RF-PAN-12). One poller looks under every shell pane at once.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::time::MissedTickBehavior;
use yien_proto::PaneId;

use crate::report::Reports;
use crate::workspace::PaneEvent;

/// Finds the known program running under each shell process id, in order.
///
/// ```
/// let probe: yien_core::ProgramProbe = yien_platform::programs::programs_under;
/// assert_eq!(probe(&[]), Vec::<Option<&str>>::new());
/// ```
pub type ProgramProbe = fn(&[u32]) -> Vec<Option<&'static str>>;

/// How often the shells are looked under.
pub(crate) const POLL: Duration = Duration::from_secs(1);

/// The shell panes running now: their shell's process id and launch number.
#[derive(Debug, Default)]
pub(crate) struct Shells(Mutex<HashMap<PaneId, (u32, u64)>>);

impl Shells {
    fn map(&self) -> MutexGuard<'_, HashMap<PaneId, (u32, u64)>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Looks under `pid` for `pane`'s launch `launch` until the guard drops.
    pub fn watch(self: &Arc<Self>, pane: PaneId, pid: u32, launch: u64) -> Watched {
        self.map().insert(pane, (pid, launch));
        Watched {
            shells: Arc::clone(self),
            pane,
            pid,
            launch,
        }
    }

    fn list(&self) -> Vec<(PaneId, u32, u64)> {
        self.map()
            .iter()
            .map(|(pane, (pid, launch))| (*pane, *pid, *launch))
            .collect()
    }
}

/// A shell being looked under; dropping it stops that.
#[derive(Debug)]
pub(crate) struct Watched {
    shells: Arc<Shells>,
    pane: PaneId,
    pid: u32,
    launch: u64,
}

impl Drop for Watched {
    fn drop(&mut self) {
        let mut map = self.shells.map();
        // The pane's next launch may have registered its own shell already,
        // possibly under the same pid once Windows reused it.
        if map.get(&self.pane) == Some(&(self.pid, self.launch)) {
            map.remove(&self.pane);
        }
    }
}

/// What each pane runs now, as `(pane, launch, program)`.
type Found = Vec<(PaneId, u64, Option<&'static str>)>;

/// The entries of `now` that differ from what was last reported, which
/// becomes `now`. A launch not reported yet counts as running nothing, as the
/// workspace clears the program when a launch starts.
pub(crate) fn changes(
    last: &mut HashMap<PaneId, (u64, Option<&'static str>)>,
    now: Found,
) -> Found {
    let mut changed = Vec::new();
    let mut next = HashMap::with_capacity(now.len());
    for (pane, launch, program) in now {
        let was = match last.get(&pane) {
            Some((l, p)) if *l == launch => *p,
            _ => None,
        };
        if was != program {
            changed.push((pane, launch, program));
        }
        next.insert(pane, (launch, program));
    }
    *last = next;
    changed
}

/// Looks under every watched shell each [`POLL`], off the runtime's workers,
/// and reports what changed. Takes no snapshot while no shell pane runs.
pub(crate) async fn poll(probe: ProgramProbe, shells: Arc<Shells>, reports: Arc<Reports>) {
    let mut tick = tokio::time::interval(POLL);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut last = HashMap::new();
    loop {
        tick.tick().await;
        let watched = shells.list();
        if watched.is_empty() {
            last.clear();
            continue;
        }
        let pids: Vec<u32> = watched.iter().map(|(_, pid, _)| *pid).collect();
        let Ok(found) = tokio::task::spawn_blocking(move || probe(&pids)).await else {
            continue;
        };
        let now = watched
            .into_iter()
            .zip(found)
            .map(|((pane, _, launch), program)| (pane, launch, program))
            .collect();
        for (pane, launch, program) in changes(&mut last, now) {
            reports.push(
                pane,
                PaneEvent::Program {
                    launch,
                    program: program.map(str::to_owned),
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use yien_proto::PaneId;

    use super::{POLL, Shells, changes, poll};
    use crate::report::Reports;
    use crate::workspace::PaneEvent;

    fn pane(n: u64) -> PaneId {
        PaneId::new(n)
    }

    #[test]
    fn rf_pan_12_only_changes_are_reported() {
        let mut last = HashMap::new();
        assert!(changes(&mut last, vec![(pane(1), 1, None)]).is_empty());
        let started = vec![(pane(1), 1, Some("claude"))];
        assert_eq!(changes(&mut last, started.clone()), started);
        assert!(changes(&mut last, started).is_empty());
        let ended = vec![(pane(1), 1, None)];
        assert_eq!(changes(&mut last, ended.clone()), ended);
    }

    #[test]
    fn rf_pan_12_a_new_launch_starts_from_nothing() {
        let mut last = HashMap::new();
        changes(&mut last, vec![(pane(1), 1, Some("codex"))]);
        assert!(changes(&mut last, vec![(pane(1), 2, None)]).is_empty());
        let again = vec![(pane(1), 3, Some("codex"))];
        assert_eq!(changes(&mut last, again.clone()), again);
    }

    #[test]
    fn rf_pan_12_a_pane_no_longer_watched_is_forgotten() {
        let mut last = HashMap::new();
        changes(&mut last, vec![(pane(1), 1, Some("claude"))]);
        changes(&mut last, vec![]);
        let back = vec![(pane(1), 1, Some("claude"))];
        assert_eq!(changes(&mut last, back.clone()), back);
    }

    fn claude_everywhere(shells: &[u32]) -> Vec<Option<&'static str>> {
        vec![Some("claude"); shells.len()]
    }

    #[test]
    fn rf_pan_12_the_poller_reports_a_program_once_and_forgets_an_unwatched_pane() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let shells = Arc::new(Shells::default());
            let reports = Arc::new(Reports::default());
            let watched = shells.watch(pane(1), 42, 3);
            let poller = tokio::spawn(poll(
                claude_everywhere,
                Arc::clone(&shells),
                Arc::clone(&reports),
            ));
            let first = reports.next().await;
            assert_eq!(
                first.get(&pane(1)).map(Vec::as_slice),
                Some(
                    &[PaneEvent::Program {
                        launch: 3,
                        program: Some("claude".to_owned()),
                    }][..]
                )
            );
            let quiet = |what: &'static str| {
                let reports = Arc::clone(&reports);
                async move {
                    let waited = tokio::time::timeout(POLL * 2, reports.next()).await;
                    assert!(waited.is_err(), "{what} was reported again");
                }
            };
            // Two more polls see the same program while the pane is watched.
            quiet("a program already reported").await;
            drop(watched);
            quiet("a pane no longer watched").await;
            poller.abort();
            assert!(shells.list().is_empty());
        });
    }

    #[test]
    fn a_dropped_guard_leaves_the_next_launchs_shell_watched() {
        let shells = Arc::new(Shells::default());
        let old = shells.watch(pane(1), 10, 1);
        let new = shells.watch(pane(1), 20, 2);
        drop(old);
        assert_eq!(shells.list(), vec![(pane(1), 20, 2)]);
        drop(new);
        assert!(shells.list().is_empty());
    }

    #[test]
    fn a_late_guard_leaves_a_new_launch_on_a_reused_pid_watched() {
        let shells = Arc::new(Shells::default());
        let old = shells.watch(pane(1), 10, 1);
        let new = shells.watch(pane(1), 10, 2);
        drop(old);
        assert_eq!(shells.list(), vec![(pane(1), 10, 2)]);
        drop(new);
        assert!(shells.list().is_empty());
    }
}
