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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
    /// To the window: an offer, or `None` to withdraw it.
    offers: mpsc::UnboundedSender<Option<UpdateOffer>>,
}

#[derive(Default)]
struct State {
    /// `update.check`, `None` until the first valid reading.
    enabled: Option<bool>,
    /// YIEN is quitting.
    stopped: bool,
    /// The newest release above this version the checker knows of.
    latest: Option<Release>,
    /// The release last offered to the window.
    offered: Option<Release>,
    /// The checked installer, once downloaded.
    downloaded: Option<Downloaded>,
    /// Start the installer at quit.
    armed: bool,
}

impl Shared {
    fn new(dir: Option<PathBuf>) -> (Arc<Self>, mpsc::UnboundedReceiver<Option<UpdateOffer>>) {
        let (offers, receive) = mpsc::unbounded_channel();
        let shared = Arc::new(Self {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            dir,
            offers,
        });
        (shared, receive)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sends the newest known release to the window unless it is the one
    /// offered already, or the setting is off.
    fn offer_latest(&self) {
        let mut state = self.lock();
        let Some(release) = state.latest.clone() else {
            return;
        };
        if state.enabled != Some(true) || state.offered.as_ref() == Some(&release) {
            return;
        }
        state.offered = Some(release.clone());
        let _ = self.offers.send(Some(UpdateOffer {
            version: release.version,
            notes: release.notes,
        }));
    }
}

impl Update {
    /// Starts the checker thread; the window gets the offers and a host.
    pub(crate) fn start() -> (Self, Updates) {
        let dir = yien_platform::paths::state_dir()
            .inspect_err(|e| tracing::warn!("no data dir, update checks are off: {e}"))
            .ok();
        let (shared, offers) = Shared::new(dir);
        let worker = Arc::clone(&shared);
        let spawned = std::thread::Builder::new()
            .name("yien-update".into())
            .spawn(move || run(&worker, &Web { loopback: false }, START_DELAY));
        if let Err(error) = spawned {
            tracing::warn!("the update check did not start: {error}");
        }
        let host = Box::new(Host {
            shared: Arc::clone(&shared),
        });
        (Self { shared }, Updates { offers, host })
    }

    /// A reading of the settings file; an invalid one keeps the last value.
    /// Turning the check off withdraws the offer and any install set up.
    pub(crate) fn reading(&self, update: &ConfigUpdate) {
        let ConfigUpdate::Loaded(loaded) = update else {
            return;
        };
        let on = loaded.config.update_check;
        let mut state = self.shared.lock();
        if state.enabled == Some(on) {
            return;
        }
        state.enabled = Some(on);
        if !on {
            state.armed = false;
            state.downloaded = None;
            if state.offered.take().is_some() {
                let _ = self.shared.offers.send(None);
            }
        }
        self.shared.wake.notify_all();
    }

    /// YIEN is quitting, its panes closed: the checker stops, and the
    /// installer starts when the user asked for it and the session was
    /// saved (RNF-21).
    pub(crate) fn quit(&self, session_saved: bool) {
        let installer = {
            let mut state = self.shared.lock();
            state.stopped = true;
            self.shared.wake.notify_all();
            to_launch(&state)
        };
        let Some(installer) = installer else { return };
        if !session_saved {
            tracing::warn!("the session was not saved, so the YIEN installer was not started");
            return;
        }
        match download::launch(&installer) {
            Ok(()) => tracing::info!("the YIEN installer started"),
            Err(error) => tracing::warn!("the YIEN installer did not start: {error}"),
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

type Done = Box<dyn FnOnce(Result<(), String>) + Send>;

/// A download failed or was refused: the window gets the newest release
/// again, which may be newer than the one it tried.
fn failed(shared: &Shared, error: String, done: Done) {
    shared.lock().offered = None;
    done(Err(error));
    shared.offer_latest();
}

impl UpdateHost for Host {
    fn download(&self, version: &str, done: Done) {
        let (enabled, release) = {
            let state = self.shared.lock();
            let release = state.offered.clone().filter(|r| r.version == version);
            (state.enabled == Some(true), release)
        };
        if !enabled {
            return done(Err("update checks are off in config.toml".into()));
        }
        let (Some(release), Some(dir)) = (release, self.shared.dir.clone()) else {
            return failed(&self.shared, "the update is no longer offered".into(), done);
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
                        let mut state = shared.lock();
                        if state.enabled != Some(true) {
                            drop(state);
                            return done(Err("update checks were turned off".into()));
                        }
                        tracing::info!("the YIEN installer matched its SHA-256");
                        state.downloaded = Some(installer);
                        drop(state);
                        done(Ok(()));
                    }
                    Err(error) => failed(&shared, error, done),
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

/// The checker: waits `start_delay` for the window, then offers the saved
/// release and asks GitHub whenever the schedule says so, while the setting
/// is on.
fn run(shared: &Shared, fetch: &dyn Fetch, start_delay: Duration) {
    let Some(dir) = shared.dir.clone() else {
        return;
    };
    let mut store = FileStore::new(dir);
    let current = current();
    if !pause(shared, start_delay) {
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
        shared.lock().latest = saved.offer(&current).cloned();
        shared.offer_latest();
        let seen = shared.lock().enabled;
        let timeout = match schedule::next(seen == Some(true), saved.checked_at, failed_at, now()) {
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
        if !wait(shared, timeout, seen) {
            return;
        }
    }
}

/// Sleeps `delay` whatever the setting does; `false` when YIEN quits first.
fn pause(shared: &Shared, delay: Duration) -> bool {
    let deadline = Instant::now() + delay;
    let mut state = shared.lock();
    loop {
        if state.stopped {
            return false;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return true;
        }
        state = shared
            .wake
            .wait_timeout(state, left)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
}

/// Sleeps until `timeout` passes (forever when `None`), the setting differs
/// from `seen` or YIEN quits; `false` once quitting.
fn wait(shared: &Shared, timeout: Option<Duration>, seen: Option<bool>) -> bool {
    let state = shared.lock();
    let unchanged = |s: &mut State| !s.stopped && s.enabled == seen;
    let state = match timeout {
        Some(timeout) => {
            shared
                .wake
                .wait_timeout_while(state, timeout, unchanged)
                .unwrap_or_else(PoisonError::into_inner)
                .0
        }
        None => shared
            .wake
            .wait_while(state, unchanged)
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

    fn loaded(text: &str) -> ConfigUpdate {
        ConfigUpdate::Loaded(yien_config::parse(text).unwrap())
    }

    fn a_release() -> Release {
        let json = serde_json::to_vec(&serde_json::Value::Array(vec![release::tests::entry(
            "v0.2.0", false, true,
        )]))
        .unwrap();
        release::releases(&json).unwrap().pop().unwrap()
    }

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("yien-update-{name}-{}", std::process::id()))
    }

    fn update(shared: &Arc<Shared>) -> Update {
        Update {
            shared: Arc::clone(shared),
        }
    }

    #[test]
    fn the_setting_reaches_the_checker_and_an_invalid_file_keeps_it() {
        let (shared, _offers) = Shared::new(None);
        let update = update(&shared);
        update.reading(&loaded("update.check = false"));
        assert_eq!(shared.lock().enabled, Some(false));
        update.reading(&ConfigUpdate::Unreadable("gone".into()));
        assert_eq!(shared.lock().enabled, Some(false));
        update.reading(&loaded(""));
        assert_eq!(shared.lock().enabled, Some(true), "on by default");
    }

    #[test]
    fn the_start_delay_holds_whatever_the_setting_does_and_ends_on_quit() {
        let (shared, _offers) = Shared::new(None);
        let started = Instant::now();
        let sleeper = {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || pause(&shared, Duration::from_millis(400)))
        };
        std::thread::sleep(Duration::from_millis(50));
        let update = update(&shared);
        update.reading(&loaded("update.check = true"));
        update.reading(&loaded("update.check = false"));
        assert!(sleeper.join().unwrap());
        assert!(started.elapsed() >= Duration::from_millis(400));

        let sleeper = {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || pause(&shared, Duration::from_secs(60)))
        };
        std::thread::sleep(Duration::from_millis(50));
        update.quit(true);
        assert!(!sleeper.join().unwrap(), "quitting ends it at once");
    }

    #[test]
    fn a_wait_ends_when_the_setting_changes_or_yien_quits() {
        let (shared, _offers) = Shared::new(None);
        shared.lock().enabled = Some(false);
        let waiter = {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || wait(&shared, None, Some(false)))
        };
        std::thread::sleep(Duration::from_millis(50));
        let update = update(&shared);
        update.reading(&loaded("update.check = true"));
        assert!(waiter.join().unwrap(), "woken by the change");
        update.quit(true);
        assert!(
            !wait(&shared, None, Some(true)),
            "returns at once after quit"
        );
    }

    #[test]
    fn with_the_setting_off_no_request_is_ever_made_past_the_start_delay() {
        let dir = temp("off");
        let (shared, _offers) = Shared::new(Some(dir.clone()));
        shared.lock().enabled = Some(false);
        let fetch = Arc::new(Counting(AtomicUsize::new(0)));
        let runner = {
            let (shared, fetch) = (Arc::clone(&shared), Arc::clone(&fetch));
            std::thread::spawn(move || run(&shared, &*fetch, Duration::from_millis(50)))
        };
        // Well past the delay: the checker sits in its `Next::Off` wait.
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(fetch.0.load(Ordering::SeqCst), 0);
        update(&shared).quit(true);
        runner.join().unwrap();
        assert_eq!(fetch.0.load(Ordering::SeqCst), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn turning_the_setting_on_past_the_delay_makes_one_request() {
        let dir = temp("on");
        let (shared, _offers) = Shared::new(Some(dir.clone()));
        shared.lock().enabled = Some(false);
        let fetch = Arc::new(Counting(AtomicUsize::new(0)));
        let runner = {
            let (shared, fetch) = (Arc::clone(&shared), Arc::clone(&fetch));
            std::thread::spawn(move || run(&shared, &*fetch, Duration::from_millis(50)))
        };
        std::thread::sleep(Duration::from_millis(200));
        let update = update(&shared);
        update.reading(&loaded("update.check = true"));
        let deadline = Instant::now() + Duration::from_secs(5);
        while fetch.0.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(100));
        update.quit(true);
        runner.join().unwrap();
        assert_eq!(fetch.0.load(Ordering::SeqCst), 1, "then it waits an hour");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_offer_is_sent_once_and_withdrawn_when_the_setting_goes_off() {
        let (shared, mut offers) = Shared::new(None);
        let update = update(&shared);
        update.reading(&loaded(""));
        shared.lock().latest = Some(a_release());
        shared.offer_latest();
        shared.offer_latest();
        assert_eq!(offers.try_recv().unwrap().unwrap().version, "0.2.0");
        assert!(offers.try_recv().is_err());
        update.reading(&loaded("update.check = false"));
        assert_eq!(offers.try_recv().unwrap(), None, "withdrawn");
        shared.offer_latest();
        assert!(offers.try_recv().is_err(), "nothing offered while off");
        update.reading(&loaded(""));
        shared.offer_latest();
        assert!(offers.try_recv().unwrap().is_some(), "offered again");
    }

    #[test]
    fn a_download_is_refused_while_the_setting_is_off() {
        let (shared, _offers) = Shared::new(Some(std::env::temp_dir()));
        update(&shared).reading(&loaded("update.check = false"));
        shared.lock().offered = Some(a_release());
        let host = Host { shared };
        let (send, recv) = std::sync::mpsc::channel();
        host.download("0.2.0", Box::new(move |r| send.send(r).unwrap()));
        assert!(recv.recv().unwrap().unwrap_err().contains("off"));
    }

    #[test]
    fn a_refused_download_offers_the_newest_release_again() {
        let (shared, mut offers) = Shared::new(Some(std::env::temp_dir()));
        update(&shared).reading(&loaded(""));
        shared.lock().latest = Some(a_release());
        let host = Host {
            shared: Arc::clone(&shared),
        };
        let (send, recv) = std::sync::mpsc::channel();
        host.download("9.9.9", Box::new(move |r| send.send(r).unwrap()));
        assert!(recv.recv().unwrap().is_err());
        assert_eq!(offers.try_recv().unwrap().unwrap().version, "0.2.0");
    }

    #[test]
    fn the_installer_starts_only_when_downloaded_and_armed() {
        let (shared, _offers) = Shared::new(None);
        let host = Host {
            shared: Arc::clone(&shared),
        };
        host.install_on_quit(true);
        assert_eq!(to_launch(&shared.lock()), None, "nothing downloaded");
        let installer = Downloaded {
            path: PathBuf::from("setup.exe"),
            digest: "00".into(),
        };
        shared.lock().downloaded = Some(installer.clone());
        assert_eq!(to_launch(&shared.lock()), Some(installer));
        host.install_on_quit(false);
        assert_eq!(to_launch(&shared.lock()), None, "the quit was cancelled");
    }

    #[test]
    fn turning_the_setting_off_drops_an_install_set_up() {
        let (shared, _offers) = Shared::new(None);
        let update = update(&shared);
        update.reading(&loaded(""));
        {
            let mut state = shared.lock();
            state.downloaded = Some(Downloaded {
                path: PathBuf::from("setup.exe"),
                digest: "00".into(),
            });
            state.armed = true;
        }
        update.reading(&loaded("update.check = false"));
        assert_eq!(to_launch(&shared.lock()), None);
    }
}
