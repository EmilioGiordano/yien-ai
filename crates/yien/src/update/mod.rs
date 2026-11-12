//! The update check and install: a thread that asks GitHub at most once a
//! day for a newer YIEN while `update.check` is on, the window's host that
//! downloads and checks an installer when the user asks, and the start of
//! that installer once YIEN has quit and saved its session (RNF-21).

mod checksum;
mod download;
mod release;
mod schedule;

use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use semver::Version;
use tokio::sync::mpsc;
use yien_platform::state_store::FileStore;
use yien_session::Store;
use yien_ui::{ConfigUpdate, UpdateHost, UpdateOffer, Updates};

use download::{Downloaded, Fetch, Web};
use release::Release;
use schedule::{Next, STATE_FILE, Saved};

/// How long after the start the first check waits, so it never competes
/// with the window opening and the session restoring.
const START_DELAY: Duration = Duration::from_secs(10);

/// The most bytes of the state file read.
const STATE_LIMIT: u64 = 1 << 20;

/// The checker, the window's host and the quit step share this.
pub(crate) struct Update {
    shared: Arc<Shared>,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    /// The state dir; `None` when there is none, which turns updates off.
    dir: Option<PathBuf>,
}

#[derive(Default)]
struct State {
    /// `update.check`, `None` until the first valid reading.
    enabled: Option<bool>,
    /// YIEN is quitting.
    stopped: bool,
    /// The release last offered to the window.
    offered: Option<Release>,
    /// The checked installer, once downloaded.
    downloaded: Option<Downloaded>,
    /// Start the installer at quit.
    armed: bool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Update {
    /// Starts the checker thread; the window gets the offers and a host.
    pub(crate) fn start() -> (Self, Updates) {
        let dir = yien_platform::paths::state_dir()
            .inspect_err(|e| tracing::warn!("no data dir, update checks are off: {e}"))
            .ok();
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            dir,
        });
        let (send, offers) = mpsc::unbounded_channel();
        let worker = Arc::clone(&shared);
        let spawned = std::thread::Builder::new()
            .name("yien-update".into())
            .spawn(move || run(&worker, &Web { loopback: false }, &send));
        if let Err(error) = spawned {
            tracing::warn!("the update check did not start: {error}");
        }
        let host = Box::new(Host {
            shared: Arc::clone(&shared),
        });
        (Self { shared }, Updates { offers, host })
    }

    /// A reading of the settings file; an invalid one keeps the last value.
    pub(crate) fn reading(&self, update: &ConfigUpdate) {
        let ConfigUpdate::Loaded(loaded) = update else {
            return;
        };
        let mut state = self.shared.lock();
        if state.enabled != Some(loaded.config.update_check) {
            state.enabled = Some(loaded.config.update_check);
            self.shared.wake.notify_all();
        }
    }

    /// YIEN is quitting, its session saved and its panes closed: the
    /// checker stops, and the installer starts when the user asked for it.
    pub(crate) fn quit(&self) {
        let installer = {
            let mut state = self.shared.lock();
            state.stopped = true;
            self.shared.wake.notify_all();
            to_launch(&state)
        };
        if let Some(installer) = installer {
            match download::launch(&installer) {
                Ok(()) => tracing::info!("the YIEN installer started"),
                Err(error) => tracing::warn!("the YIEN installer did not start: {error}"),
            }
        }
    }
}

/// The installer to start at quit: only a checked download the user asked
/// to install, with no quit cancelled since.
fn to_launch(state: &State) -> Option<Downloaded> {
    state.downloaded.clone().filter(|_| state.armed)
}

/// The window's side: downloads off the UI thread.
struct Host {
    shared: Arc<Shared>,
}

impl UpdateHost for Host {
    fn download(&self, version: &str, done: Box<dyn FnOnce(Result<(), String>) + Send>) {
        let release = self
            .shared
            .lock()
            .offered
            .clone()
            .filter(|r| r.version == version);
        let (Some(release), Some(dir)) = (release, self.shared.dir.clone()) else {
            return done(Err("the update is no longer offered".into()));
        };
        let shared = Arc::clone(&self.shared);
        let spawned = std::thread::Builder::new()
            .name("yien-update-download".into())
            .spawn(move || {
                tracing::info!(version = %release.version, "downloading the YIEN installer");
                let result =
                    download::download(&Web { loopback: false }, &release, &dir.join("updates"));
                match result {
                    Ok(installer) => {
                        tracing::info!("the YIEN installer matched its SHA-256");
                        shared.lock().downloaded = Some(installer);
                        done(Ok(()));
                    }
                    Err(error) => done(Err(error)),
                }
            });
        if let Err(error) = spawned {
            tracing::warn!("the download did not start: {error}");
        }
    }

    fn install_on_quit(&self, armed: bool) {
        self.shared.lock().armed = armed;
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn current() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or_else(|_| Version::new(0, 0, 0))
}

/// The checker: waits for the window, then offers the saved release and asks
/// GitHub whenever the schedule says so, while the setting is on.
fn run(shared: &Shared, fetch: &dyn Fetch, offers: &mpsc::UnboundedSender<UpdateOffer>) {
    let Some(dir) = shared.dir.clone() else {
        return;
    };
    let mut store = FileStore::new(dir);
    let current = current();
    if !wait(shared, Some(START_DELAY)) {
        return;
    }
    let mut saved = Saved::from_bytes(
        store
            .read_at_most(STATE_FILE, STATE_LIMIT)
            .ok()
            .flatten()
            .as_deref(),
    );
    let mut failed_at = None;
    loop {
        let enabled = shared.lock().enabled == Some(true);
        if enabled {
            offer(shared, saved.offer(&current), offers);
        }
        let timeout = match schedule::next(enabled, saved.checked_at, failed_at, now()) {
            Next::Off => None,
            Next::Wait(wait) => Some(wait),
            Next::Check => {
                match download::check(fetch, &current, now()) {
                    Ok(answer) => {
                        saved = answer;
                        failed_at = None;
                        tracing::debug!(
                            newer = ?saved.newest.as_ref().map(|r| &r.version),
                            "update check answered"
                        );
                        if let Ok(bytes) = serde_json::to_vec(&saved)
                            && let Err(error) = store.write_atomic(STATE_FILE, &bytes)
                        {
                            tracing::debug!("the update check time was not saved: {error}");
                        }
                    }
                    Err(error) => {
                        // Offline is normal: no notice, only a trace.
                        tracing::debug!("update check failed: {error}");
                        failed_at = Some(now());
                    }
                }
                continue;
            }
        };
        if !wait(shared, timeout) {
            return;
        }
    }
}

/// Sends `release` to the window unless it was the last one sent.
fn offer(shared: &Shared, release: Option<&Release>, offers: &mpsc::UnboundedSender<UpdateOffer>) {
    let Some(release) = release else { return };
    let mut state = shared.lock();
    if state.offered.as_ref() == Some(release) {
        return;
    }
    state.offered = Some(release.clone());
    let _ = offers.send(UpdateOffer {
        version: release.version.clone(),
        notes: release.notes.clone(),
    });
}

/// Sleeps until `timeout` passes (forever when `None`), the setting changes
/// or YIEN quits; `false` once quitting.
fn wait(shared: &Shared, timeout: Option<Duration>) -> bool {
    let state = shared.lock();
    if state.stopped {
        return false;
    }
    let enabled = state.enabled;
    let changed = |s: &mut State| !s.stopped && s.enabled == enabled;
    let state = match timeout {
        Some(timeout) => {
            shared
                .wake
                .wait_timeout_while(state, timeout, changed)
                .unwrap_or_else(PoisonError::into_inner)
                .0
        }
        None => shared
            .wake
            .wait_while(state, changed)
            .unwrap_or_else(PoisonError::into_inner),
    };
    !state.stopped
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct Counting(AtomicUsize);

    impl Fetch for Counting {
        fn fetch(&self, _: &str, _: u64, _: &mut dyn Write) -> Result<u64, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err("offline".into())
        }
    }

    fn shared(dir: Option<PathBuf>) -> Arc<Shared> {
        Arc::new(Shared {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            dir,
        })
    }

    fn loaded(text: &str) -> ConfigUpdate {
        ConfigUpdate::Loaded(yien_config::parse(text).unwrap())
    }

    #[test]
    fn the_setting_reaches_the_checker_and_an_invalid_file_keeps_it() {
        let update = Update {
            shared: shared(None),
        };
        update.reading(&loaded("update.check = false"));
        assert_eq!(update.shared.lock().enabled, Some(false));
        update.reading(&ConfigUpdate::Unreadable("gone".into()));
        assert_eq!(update.shared.lock().enabled, Some(false));
        update.reading(&loaded(""));
        assert_eq!(update.shared.lock().enabled, Some(true), "on by default");
    }

    #[test]
    fn a_wait_ends_when_the_setting_changes_or_yien_quits() {
        let s = shared(None);
        s.lock().enabled = Some(false);
        let waiter = {
            let s = Arc::clone(&s);
            std::thread::spawn(move || wait(&s, None))
        };
        std::thread::sleep(Duration::from_millis(50));
        let update = Update {
            shared: Arc::clone(&s),
        };
        update.reading(&loaded("update.check = true"));
        assert!(waiter.join().unwrap(), "woken by the change");
        update.quit();
        assert!(!wait(&s, None), "returns at once after quit");
    }

    #[test]
    fn with_the_setting_off_no_request_is_ever_made() {
        let dir = std::env::temp_dir().join(format!("yien-update-off-{}", std::process::id()));
        let s = shared(Some(dir.clone()));
        s.lock().enabled = Some(false);
        let fetch = Arc::new(Counting(AtomicUsize::new(0)));
        let (send, _offers) = mpsc::unbounded_channel();
        let runner = {
            let (s, fetch) = (Arc::clone(&s), Arc::clone(&fetch));
            std::thread::spawn(move || run(&s, &*fetch, &send))
        };
        // Past the start delay would take 10 s; quitting during it must end
        // the thread without a request.
        std::thread::sleep(Duration::from_millis(50));
        Update {
            shared: Arc::clone(&s),
        }
        .quit();
        runner.join().unwrap();
        assert_eq!(fetch.0.load(Ordering::SeqCst), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_offer_is_sent_once_per_release() {
        let s = shared(None);
        let (send, mut offers) = mpsc::unbounded_channel();
        let json = serde_json::to_vec(&serde_json::Value::Array(vec![release::tests::entry(
            "v0.2.0", false, true,
        )]))
        .unwrap();
        let r = release::releases(&json).unwrap().pop().unwrap();
        offer(&s, Some(&r), &send);
        offer(&s, Some(&r), &send);
        offer(&s, None, &send);
        assert_eq!(offers.try_recv().unwrap().version, "0.2.0");
        assert!(offers.try_recv().is_err());
    }

    #[test]
    fn rnf_21_the_installer_starts_only_when_downloaded_and_armed() {
        let s = shared(None);
        let host = Host {
            shared: Arc::clone(&s),
        };
        host.install_on_quit(true);
        assert_eq!(to_launch(&s.lock()), None, "nothing downloaded");
        let installer = Downloaded {
            path: PathBuf::from("setup.exe"),
            digest: "00".into(),
        };
        s.lock().downloaded = Some(installer.clone());
        assert_eq!(to_launch(&s.lock()), Some(installer));
        host.install_on_quit(false);
        assert_eq!(to_launch(&s.lock()), None, "the quit was cancelled");
    }

    #[test]
    fn a_download_of_a_version_not_offered_is_refused_at_once() {
        let host = Host {
            shared: shared(Some(std::env::temp_dir())),
        };
        let (send, recv) = std::sync::mpsc::channel();
        host.download("9.9.9", Box::new(move |r| send.send(r).unwrap()));
        assert!(recv.recv().unwrap().is_err());
    }
}
