//! Each pane's screen on disk (RF-SES-10): `<pane>.vt` holds the screen and
//! the newest scrollback rows as VT text, written every
//! [`SCREEN_INTERVAL`](crate::SCREEN_INTERVAL) for panes whose screen changed
//! and once more on quit, and fed back into the pane's engine when it is
//! restored. Not part of the saved session: losing a file loses only that
//! pane's old text. Never logged.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use yien_platform::state_store::FileStore;
use yien_proto::PaneId;
use yien_session::{Store, StoreError};
use yien_vt::{DumpLimits, is_screen_dump};

use crate::Clock;
use crate::pane::{Engine, lock};

/// How often changed screens are written.
pub const SCREEN_INTERVAL: Duration = Duration::from_secs(30);

/// What one pane's file keeps at most.
pub const SCREEN_LIMITS: DumpLimits = DumpLimits {
    max_rows: 2_000,
    max_bytes: 1024 * 1024,
};

/// What every pane's file keeps together at most; with more panes than
/// this holds at [`SCREEN_LIMITS`], each keeps a share.
pub const SCREENS_TOTAL_BYTES: usize = 32 * 1024 * 1024;

/// The biggest file read back: a dump and its cursor moves.
const MAX_FILE: u64 = SCREEN_LIMITS.max_bytes as u64 + 64;

/// How often the writer looks at the clock; the clock is injected, so it
/// cannot wake the writer itself.
const POLL: Duration = Duration::from_millis(250);

/// The files screens are kept in.
///
/// ```
/// use yien_core::ScreenFiles;
/// use yien_platform::state_store::FileStore;
///
/// fn takes(_: impl ScreenFiles) {}
/// takes(FileStore::new("screens"));
/// ```
pub trait ScreenFiles: Send {
    /// The file's bytes; `None` when it does not exist or holds more than
    /// `limit`.
    fn read(&self, name: &str, limit: u64) -> Result<Option<Vec<u8>>, StoreError>;
    /// Replaces the file atomically.
    fn write(&mut self, name: &str, bytes: &[u8]) -> Result<(), StoreError>;
    /// Removes the file; a missing one is not an error.
    fn remove(&mut self, name: &str) -> Result<(), StoreError>;
    /// Every file's name.
    fn list(&self) -> Result<Vec<String>, StoreError>;
}

impl ScreenFiles for FileStore {
    fn read(&self, name: &str, limit: u64) -> Result<Option<Vec<u8>>, StoreError> {
        self.read_at_most(name, limit)
    }

    fn write(&mut self, name: &str, bytes: &[u8]) -> Result<(), StoreError> {
        self.write_atomic(name, bytes)
    }

    fn remove(&mut self, name: &str) -> Result<(), StoreError> {
        FileStore::remove(self, name)
    }

    fn list(&self) -> Result<Vec<String>, StoreError> {
        FileStore::list(self)
    }
}

/// Where the core keeps pane screens. Cheap to clone; clones share the files.
///
/// ```
/// use yien_core::ScreenStore;
/// use yien_platform::state_store::FileStore;
///
/// let store = ScreenStore::new(FileStore::new("screens"));
/// let config = yien_core::CoreConfig { screens: Some(store), ..Default::default() };
/// assert!(config.screens.is_some());
/// ```
#[derive(Clone)]
pub struct ScreenStore {
    files: Arc<Mutex<Box<dyn ScreenFiles>>>,
}

impl ScreenStore {
    /// Wraps the files screens are read from and written to.
    pub fn new(files: impl ScreenFiles + 'static) -> Self {
        Self {
            files: Arc::new(Mutex::new(Box::new(files))),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Box<dyn ScreenFiles>> {
        self.files.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The saved screen of `pane`, when there is one that holds only what a
    /// dump writes. Blocking.
    pub(crate) fn read(&self, pane: PaneId) -> Option<Vec<u8>> {
        match self.lock().read(&file_name(pane), MAX_FILE) {
            Ok(Some(bytes)) if !bytes.is_empty() && is_screen_dump(&bytes) => Some(bytes),
            Ok(Some(_)) => {
                tracing::warn!(
                    pane = pane.get(),
                    "a saved screen was not a dump; not shown"
                );
                None
            }
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(pane = pane.get(), "a saved screen could not be read: {e}");
                None
            }
        }
    }
}

impl fmt::Debug for ScreenStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScreenStore").finish_non_exhaustive()
    }
}

fn file_name(pane: PaneId) -> String {
    format!("{}.vt", pane.get())
}

/// The pane a file belongs to, for the files this module writes (a stray
/// temp file included); `None` for anything else, which is never touched.
fn file_pane(name: &str) -> Option<u64> {
    let stem = name
        .strip_suffix(".vt.tmp")
        .or_else(|| name.strip_suffix(".vt"))?;
    stem.parse()
        .ok()
        .filter(|_| stem.bytes().all(|b| b.is_ascii_digit()))
}

/// A pane's live screen as the writer reads it: the engine its actor runs
/// now, and a count of changes the writer compares with what it wrote.
#[derive(Default)]
pub(crate) struct ScreenSource {
    engine: Mutex<Option<Engine>>,
    changes: AtomicU64,
}

impl ScreenSource {
    /// The actor started a new engine.
    pub fn attach(&self, engine: Engine) {
        *self.engine.lock().unwrap_or_else(PoisonError::into_inner) = Some(engine);
        self.changed();
    }

    /// The screen may have changed.
    pub fn changed(&self) {
        self.changes.fetch_add(1, Ordering::Release);
    }

    fn changes(&self) -> u64 {
        self.changes.load(Ordering::Acquire)
    }

    /// The dump of the engine; `None` before the program first started.
    fn dump(&self, limits: DumpLimits) -> Option<Vec<u8>> {
        let engine = self
            .engine
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()?;
        let dump = lock(&engine).screen_dump(limits);
        Some(dump)
    }
}

/// The writer thread and the panes it writes.
pub(crate) struct Screens {
    shared: Arc<Shared>,
    store: ScreenStore,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

#[derive(Default)]
struct State {
    panes: HashMap<PaneId, Entry>,
    /// Panes closed since the writer last looked; their files go.
    gone: Vec<PaneId>,
    stopped: bool,
}

struct Entry {
    source: Arc<ScreenSource>,
    /// The change count of the screen last written.
    written: u64,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// One pane's screen to write, taken from the state so no lock is held
/// while encoding or writing.
struct Job {
    pane: PaneId,
    source: Arc<ScreenSource>,
    written: u64,
}

/// Why the quit save did not write every changed screen.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ScreenSaveError {
    #[error("no time was left for the screens")]
    NoTime,
    #[error("the screens took too long; the rest keep their old files")]
    TimedOut,
    #[error("could not start saving the screens: {0}")]
    NoThread(#[source] std::io::Error),
}

impl Screens {
    /// Starts the writer thread; `None` when it cannot start.
    pub fn start(
        store: ScreenStore,
        interval: Duration,
        clock: Arc<dyn Clock>,
    ) -> Option<Arc<Self>> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
        });
        let thread = {
            let (shared, store) = (Arc::clone(&shared), store.clone());
            std::thread::Builder::new()
                .name("yien-screens".to_owned())
                .spawn(move || write_loop(&shared, &store, interval, clock.as_ref()))
        };
        if let Err(e) = thread {
            tracing::warn!("pane screens will not be saved: {e}");
            return None;
        }
        Some(Arc::new(Self { shared, store }))
    }

    /// The saved screen of `pane`. Blocking.
    pub fn read(&self, pane: PaneId) -> Option<Vec<u8>> {
        self.store.read(pane)
    }

    /// Starts writing `pane`'s screen.
    pub fn track(&self, pane: PaneId, source: Arc<ScreenSource>) {
        let mut state = self.shared.lock();
        state.gone.retain(|p| *p != pane);
        state.panes.insert(pane, Entry { source, written: 0 });
    }

    /// The pane closed: its file goes.
    pub fn forget(&self, pane: PaneId) {
        let mut state = self.shared.lock();
        if state.panes.remove(&pane).is_some() {
            state.gone.push(pane);
            drop(state);
            self.shared.wake.notify_all();
        }
    }

    /// Stops the thread, writing nothing more.
    pub fn stop(&self) {
        self.shared.lock().stopped = true;
        self.shared.wake.notify_all();
    }

    /// Stops the thread and writes every changed screen now, on a thread of
    /// its own, so the caller waits at most `timeout`. Screens left when the
    /// time runs out are not written: the old files stay. Returns how many
    /// were written.
    pub fn save_now(&self, timeout: Duration) -> Result<usize, ScreenSaveError> {
        let deadline = Instant::now() + timeout;
        let (jobs, gone, known) = {
            let mut state = self.shared.lock();
            state.stopped = true;
            (jobs(&state), std::mem::take(&mut state.gone), known(&state))
        };
        self.shared.wake.notify_all();
        if timeout.is_zero() {
            return Err(ScreenSaveError::NoTime);
        }
        let (done, result) = std::sync::mpsc::channel();
        let store = self.store.clone();
        std::thread::Builder::new()
            .name("yien-screens-quit".to_owned())
            .spawn(move || {
                remove_gone(&store, &gone);
                let (written, left) = write_jobs(&store, jobs, Some(deadline));
                if left == 0 {
                    prune(&store, &known);
                }
                let _ = done.send((written.len(), left));
            })
            .map_err(ScreenSaveError::NoThread)?;
        match result.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok((written, 0)) => Ok(written),
            Ok(_) | Err(_) => Err(ScreenSaveError::TimedOut),
        }
    }
}

impl Drop for Screens {
    fn drop(&mut self) {
        self.stop();
    }
}

fn jobs(state: &State) -> Vec<Job> {
    state
        .panes
        .iter()
        .map(|(pane, entry)| Job {
            pane: *pane,
            source: Arc::clone(&entry.source),
            written: entry.written,
        })
        .collect()
}

fn known(state: &State) -> HashSet<u64> {
    state.panes.keys().map(|p| p.get()).collect()
}

fn write_loop(shared: &Shared, store: &ScreenStore, interval: Duration, clock: &dyn Clock) {
    let mut next = clock.now().saturating_add(interval);
    loop {
        let mut state = shared.lock();
        if state.stopped {
            return;
        }
        if state.gone.is_empty() {
            state = shared
                .wake
                .wait_timeout(state, POLL)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
            if state.stopped {
                return;
            }
        }
        let gone = std::mem::take(&mut state.gone);
        let now = clock.now();
        let due = now >= next;
        let (jobs, known) = if due {
            next = now.saturating_add(interval);
            (jobs(&state), known(&state))
        } else {
            (Vec::new(), HashSet::new())
        };
        drop(state);
        remove_gone(store, &gone);
        if !due {
            continue;
        }
        let (written, _) = write_jobs(store, jobs, None);
        prune(store, &known);
        let mut state = shared.lock();
        for (pane, changes) in written {
            if let Some(entry) = state.panes.get_mut(&pane) {
                entry.written = entry.written.max(changes);
            }
        }
    }
}

fn remove_gone(store: &ScreenStore, gone: &[PaneId]) {
    if gone.is_empty() {
        return;
    }
    let mut files = store.lock();
    for pane in gone {
        if let Err(e) = files.remove(&file_name(*pane)) {
            tracing::warn!(
                pane = pane.get(),
                "a closed pane's screen was not removed: {e}"
            );
        }
    }
}

/// Writes each changed screen until `deadline`; returns the panes written
/// with the change count each was taken at, and how many were left.
fn write_jobs(
    store: &ScreenStore,
    jobs: Vec<Job>,
    deadline: Option<Instant>,
) -> (Vec<(PaneId, u64)>, usize) {
    let jobs: Vec<Job> = jobs
        .into_iter()
        .filter(|job| job.source.changes() != job.written)
        .collect();
    let limits = DumpLimits {
        max_bytes: SCREEN_LIMITS
            .max_bytes
            .min(SCREENS_TOTAL_BYTES / jobs.len().max(1)),
        ..SCREEN_LIMITS
    };
    let total = jobs.len();
    let mut written = Vec::new();
    for (done, job) in jobs.into_iter().enumerate() {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return (written, total - done);
        }
        let changes = job.source.changes();
        let Some(dump) = job.source.dump(limits) else {
            continue;
        };
        let name = file_name(job.pane);
        let mut files = store.lock();
        let result = if dump.is_empty() {
            files.remove(&name)
        } else {
            files.write(&name, &dump)
        };
        drop(files);
        match result {
            Ok(()) => written.push((job.pane, changes)),
            Err(e) => tracing::warn!(pane = job.pane.get(), "a pane's screen was not saved: {e}"),
        }
    }
    (written, 0)
}

/// Removes the screens of panes that no longer exist.
fn prune(store: &ScreenStore, known: &HashSet<u64>) {
    let mut files = store.lock();
    let names = match files.list() {
        Ok(names) => names,
        Err(e) => {
            tracing::warn!("old screens could not be listed: {e}");
            return;
        }
    };
    for name in names {
        if file_pane(&name).is_some_and(|pane| !known.contains(&pane))
            && let Err(e) = files.remove(&name)
        {
            tracing::warn!("an old screen was not removed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_screen_files_belong_to_a_pane() {
        assert_eq!(file_pane("12.vt"), Some(12));
        assert_eq!(file_pane("12.vt.tmp"), Some(12));
        for name in ["workspace.json", "x.vt", "+1.vt", "1.vt.bak", ".vt", "1"] {
            assert_eq!(file_pane(name), None, "{name}");
        }
    }
}
