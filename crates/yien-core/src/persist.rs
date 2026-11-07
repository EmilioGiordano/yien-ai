//! The saved session (RF-SES-01, RF-SES-02): loading it at start, and a
//! writer thread that owns the store and writes the newest snapshot a short
//! while after the last change, so no tokio worker or UI thread waits on disk.

use std::fmt;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc;
use yien_proto::CoreMsg;
use yien_session::{
    CURRENT_FILE, LoadReport, SaveError, SessionSnapshot, Store, StoreError, generation_name, load,
    preserve_newer, save,
};

use crate::workspace::{Disk, folders};

/// How long after the last change the workspace is written (plan §7.1).
pub const SAVE_DEBOUNCE: Duration = Duration::from_millis(250);

/// The longest a change waits to be written while changes keep coming
/// (RNF-09).
const SAVE_AT_MOST_AFTER: Duration = Duration::from_secs(2);

/// Where the core keeps the saved session. Cheap to clone; clones share the
/// store, and only one write runs at a time.
///
/// ```
/// use yien_core::SessionStore;
///
/// let store = SessionStore::new(yien_session::MemStore::new());
/// let config = yien_core::CoreConfig { session: Some(store), ..Default::default() };
/// assert!(config.session.is_some());
/// ```
#[derive(Clone)]
pub struct SessionStore {
    slot: Arc<Mutex<Slot>>,
}

struct Slot {
    store: Box<dyn Store + Send>,
    /// The newest snapshot version on disk, so an older one never replaces it.
    written: u64,
}

impl SessionStore {
    /// Wraps the store the session is read from and written to.
    pub fn new(store: impl Store + Send + 'static) -> Self {
        Self {
            slot: Arc::new(Mutex::new(Slot {
                store: Box::new(store),
                written: 0,
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl fmt::Debug for SessionStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionStore").finish_non_exhaustive()
    }
}

/// Why [`CoreHandle::save_session`](crate::CoreHandle::save_session) did not
/// write the session.
#[derive(Debug, thiserror::Error)]
pub enum SessionSaveError {
    /// The write did not finish in the time allowed; it goes on in the
    /// background, and until it lands the previous saved session stays whole.
    #[error("saving the session took too long; it goes on in the background")]
    TimedOut,
    /// No thread could be started for the write.
    #[error("could not start saving the session: {0}")]
    NoThread(#[source] std::io::Error),
    /// The write failed; the previous saved session is untouched.
    #[error("could not save the session: {0}")]
    Failed(#[from] SaveError),
}

/// What [`open`] found.
#[derive(Debug, Default)]
pub(crate) struct Opened {
    /// The session to restore, if any with at least one tab.
    pub snapshot: Option<SessionSnapshot>,
    /// What the user should know: a damaged file, a newer one set aside.
    pub notices: Vec<String>,
    /// Which of the session's folders exist.
    pub disk: Disk,
}

/// Loads the newest good session (RF-SES-06) and checks its folders. A file
/// from a newer YIEN is set aside first, so the next save cannot drop it
/// (plan §5.2). Blocking: call it off the runtime's workers.
pub(crate) fn open(store: &SessionStore) -> Opened {
    let mut slot = store.lock();
    let report = load(&slot.store);
    let mut opened = Opened::default();
    if let Some((_, version)) = report.newer_schema() {
        opened.notices.push(match preserve_newer(&mut slot.store) {
            Ok(names) => format!(
                "The saved session comes from a newer YIEN (schema {version}). It was kept \
                 as {} and YIEN started without it.",
                names.join(", ")
            ),
            Err(e) => format!(
                "The saved session comes from a newer YIEN (schema {version}) and could not \
                 be set aside ({e}). YIEN started without it and will not save over it."
            ),
        });
        return opened;
    }
    drop(slot);
    if let Some(notice) = damage_notice(&report) {
        opened.notices.push(notice);
    }
    let snapshot = report
        .restored
        .map(|r| r.snapshot)
        .filter(|s| s.projects.iter().any(|p| !p.tabs.is_empty()));
    if let Some(snapshot) = &snapshot {
        opened.disk = Disk {
            existing: folders(snapshot)
                .into_iter()
                .filter(|dir| !dir.is_empty() && Path::new(dir).is_dir())
                .collect(),
            home: yien_platform::paths::home_dir().map(|h| h.to_string_lossy().into_owned()),
        };
    }
    opened.snapshot = snapshot;
    opened
}

/// Where [`set_aside`] keeps a saved session that this run did not restore.
pub(crate) const UNRESTORED_FILE: &str = "workspace.unrestored.json";

/// Moves the saved session out of the way before this run's first write, when
/// it was not restored (whole), so the fresh workspace cannot replace it.
/// Returns the new name, or `None` when there was no file. Blocking.
pub(crate) fn set_aside(store: &SessionStore) -> Result<Option<&'static str>, StoreError> {
    let mut slot = store.lock();
    if slot.store.read(CURRENT_FILE)?.is_none() {
        return Ok(None);
    }
    slot.store.rename(CURRENT_FILE, UNRESTORED_FILE)?;
    Ok(Some(UNRESTORED_FILE))
}

fn damage_notice(report: &LoadReport) -> Option<String> {
    if report.skipped.is_empty() {
        return None;
    }
    Some(match &report.restored {
        Some(restored) => format!(
            "The saved session was damaged; YIEN restored the backup {}.",
            generation_name(restored.generation)
        ),
        None => "The saved session could not be read, so YIEN started without it.".to_owned(),
    })
}

/// The writer thread and the newest snapshot it has to write.
pub(crate) struct Writer {
    shared: Arc<Shared>,
    store: SessionStore,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

#[derive(Default)]
struct State {
    latest: Option<SessionSnapshot>,
    /// Counts the snapshots handed in; each write carries its version.
    version: u64,
    /// The version last taken by the thread.
    taken: u64,
    changed_at: Option<Instant>,
    /// When the oldest change not yet taken came in.
    pending_since: Option<Instant>,
    stopped: bool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Writer {
    /// Starts the writer thread. A failed write is told to the user once per
    /// run of failures, through `events`. `None` when the thread cannot start.
    pub fn start(
        store: SessionStore,
        debounce: Duration,
        events: mpsc::Sender<CoreMsg>,
    ) -> Option<Arc<Self>> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
        });
        let thread = {
            let (shared, store) = (Arc::clone(&shared), store.clone());
            std::thread::Builder::new()
                .name("yien-session-save".to_owned())
                .spawn(move || write_loop(&shared, &store, debounce, &events))
        };
        if let Err(e) = thread {
            tracing::warn!("the session will not be saved: {e}");
            return None;
        }
        Some(Arc::new(Self { shared, store }))
    }

    /// Hands in the workspace as it is now; written after the debounce unless
    /// it equals the last one. Ignored once the writer has stopped.
    pub fn update(&self, snapshot: SessionSnapshot) {
        let mut state = self.shared.lock();
        if state.stopped || state.latest.as_ref() == Some(&snapshot) {
            return;
        }
        let now = Instant::now();
        if state.version == state.taken {
            state.pending_since = Some(now);
        }
        state.latest = Some(snapshot);
        state.version += 1;
        state.changed_at = Some(now);
        drop(state);
        self.shared.wake.notify_all();
    }

    /// Stops the thread and writes the newest snapshot now, on a thread of
    /// its own so the caller waits at most `timeout`, disk included. Later
    /// updates are ignored, so what programs do while they are stopped never
    /// reaches the file. `Ok(false)` when there was nothing new to write.
    pub fn save_now(&self, timeout: Duration) -> Result<bool, SessionSaveError> {
        let mut state = self.shared.lock();
        state.stopped = true;
        let pending = state.latest.clone().map(|s| (s, state.version));
        drop(state);
        self.shared.wake.notify_all();
        let Some((snapshot, version)) = pending else {
            return Ok(false);
        };
        let (done, result) = std::sync::mpsc::channel();
        let store = self.store.clone();
        std::thread::Builder::new()
            .name("yien-session-quit-save".to_owned())
            .spawn(move || {
                let _ = done.send(write(store.lock(), snapshot, version));
            })
            .map_err(SessionSaveError::NoThread)?;
        result
            .recv_timeout(timeout)
            .unwrap_or(Err(SessionSaveError::TimedOut))
    }
}

impl Writer {
    /// Stops saving for the rest of the run, writing nothing.
    pub fn stop(&self) {
        self.shared.lock().stopped = true;
        self.shared.wake.notify_all();
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn write_loop(
    shared: &Shared,
    store: &SessionStore,
    debounce: Duration,
    events: &mpsc::Sender<CoreMsg>,
) {
    let mut failing = false;
    loop {
        let mut state = shared.lock();
        let (snapshot, version) = loop {
            if state.stopped {
                return;
            }
            let due = state.changed_at.map(|at| {
                let latest = state.pending_since.map_or(at, |p| p + SAVE_AT_MOST_AFTER);
                (at + debounce).min(latest)
            });
            match due {
                Some(due) if state.version > state.taken => {
                    let now = Instant::now();
                    if now >= due {
                        state.taken = state.version;
                        match state.latest.clone() {
                            Some(s) => break (s, state.version),
                            None => continue,
                        }
                    }
                    state = shared
                        .wake
                        .wait_timeout(state, due - now)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                }
                _ => {
                    state = shared
                        .wake
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner);
                }
            }
        };
        drop(state);
        match write(store.lock(), snapshot, version) {
            Ok(_) => failing = false,
            Err(e) => {
                tracing::warn!("saving the session failed: {e}");
                if !failing {
                    let _ = events.try_send(CoreMsg::Error {
                        pane: None,
                        message: format!("{e}. The last saved session is kept."),
                    });
                }
                failing = true;
            }
        }
    }
}

/// Writes `snapshot` unless a newer version is already on disk.
fn write(
    mut slot: MutexGuard<'_, Slot>,
    mut snapshot: SessionSnapshot,
    version: u64,
) -> Result<bool, SessionSaveError> {
    if slot.written >= version {
        return Ok(false);
    }
    snapshot.saved_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    save(&mut slot.store, &snapshot)?;
    slot.written = version;
    Ok(true)
}
