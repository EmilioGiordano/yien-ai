//! A cheap watcher for the active tab: polls a few files in the git directory
//! and calls back when they settle after a change, plus a slow periodic refresh
//! for edits in the work tree, which leave the git directory untouched.

use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use crate::Repo;

/// How often the watcher looks at the git directory by default.
pub const DEFAULT_INTERVAL: Duration = Duration::from_millis(500);

/// How often the watcher asks for a refresh by default, even with no git change.
pub const DEFAULT_REFRESH: Duration = Duration::from_secs(5);

const MIN_INTERVAL: Duration = Duration::from_millis(10);
const MAX_HEAD_BYTES: u64 = 4096;

/// Why the watcher called back. Either way the caller re-reads [`crate::Git::status`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// HEAD, the index or the branch's refs changed and then stayed still for one interval.
    Git,
    /// The periodic refresh came due; work tree edits show up this way.
    Refresh,
}

/// Timing of a [`GitWatcher`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatchOptions {
    /// How often the git directory is polled. Values under 10 ms are raised to 10 ms.
    pub interval: Duration,
    /// How often [`Change::Refresh`] fires when nothing else did; `None` turns it off.
    pub refresh: Option<Duration>,
}

impl Default for WatchOptions {
    fn default() -> Self {
        Self {
            interval: DEFAULT_INTERVAL,
            refresh: Some(DEFAULT_REFRESH),
        }
    }
}

/// Watches one repository on its own thread until dropped.
///
/// It reads `HEAD` (content), and the size and mtime of the index, the current
/// branch's ref, `packed-refs` and `FETCH_HEAD`. It never writes anything.
/// Keep one for the active tab and drop it when the tab changes.
///
/// ```no_run
/// use std::path::Path;
/// use yien_git::{Git, GitWatcher, WatchOptions};
/// let git = Git::new();
/// let repo = git.discover(Path::new("."))?;
/// let (tx, rx) = std::sync::mpsc::channel();
/// let _watcher = GitWatcher::spawn(&repo, WatchOptions::default(), move |change| {
///     let _ = tx.send(change);
/// })?;
/// for _change in rx {
///     let status = git.status(&repo.root)?;
///     println!("{}", status.head.label());
/// }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct GitWatcher {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl GitWatcher {
    /// Starts watching `repo`. `on_change` runs on the watcher thread and should
    /// return quickly, for example by sending on a channel.
    ///
    /// Fails only if the OS cannot start a thread.
    pub fn spawn(
        repo: &Repo,
        options: WatchOptions,
        mut on_change: impl FnMut(Change) + Send + 'static,
    ) -> std::io::Result<Self> {
        let git_dir = repo.git_dir.clone();
        let common_dir = repo.common_dir.clone();
        let interval = options.interval.max(MIN_INTERVAL);
        let refresh = options.refresh;
        let (stop, stop_rx) = mpsc::channel::<()>();
        let thread = thread::Builder::new()
            .name("yien-git-watch".to_owned())
            .spawn(move || {
                let mut poller = Poller::new(
                    read_stamps(&git_dir, &common_dir),
                    Instant::now(),
                    interval / 2,
                    refresh,
                );
                loop {
                    match stop_rx.recv_timeout(interval) {
                        Err(RecvTimeoutError::Timeout) => {}
                        Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
                    }
                    let stamps = read_stamps(&git_dir, &common_dir);
                    if let Some(change) = poller.tick(stamps, Instant::now()) {
                        on_change(change);
                    }
                }
            })?;
        Ok(Self {
            stop: Some(stop),
            thread: Some(thread),
        })
    }
}

impl Drop for GitWatcher {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            // Dropped from inside its own callback: joining would wait on itself.
            if thread.thread().id() != thread::current().id() {
                let _ = thread.join();
            }
        }
    }
}

/// Settles changes: a change is reported once the stamps stayed the same for
/// `quiet`, and a refresh once `refresh` passed since the last report.
#[derive(Debug)]
struct Poller<S> {
    seen: S,
    pending: bool,
    last_change: Instant,
    last_report: Instant,
    quiet: Duration,
    refresh: Option<Duration>,
}

impl<S: PartialEq> Poller<S> {
    fn new(initial: S, now: Instant, quiet: Duration, refresh: Option<Duration>) -> Self {
        Self {
            seen: initial,
            pending: false,
            last_change: now,
            last_report: now,
            quiet,
            refresh,
        }
    }

    fn tick(&mut self, stamps: S, now: Instant) -> Option<Change> {
        if stamps != self.seen {
            self.seen = stamps;
            self.pending = true;
            self.last_change = now;
        } else if self.pending && now.saturating_duration_since(self.last_change) >= self.quiet {
            return Some(self.report(Change::Git, now));
        }
        match self.refresh {
            Some(every) if now.saturating_duration_since(self.last_report) >= every => {
                Some(self.report(Change::Refresh, now))
            }
            _ => None,
        }
    }

    fn report(&mut self, change: Change, now: Instant) -> Change {
        self.pending = false;
        self.last_report = now;
        change
    }
}

type FileStamp = (Option<SystemTime>, u64);

#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamps {
    head: Option<Vec<u8>>,
    files: [Option<FileStamp>; 5],
}

fn read_stamps(git_dir: &Path, common_dir: &Path) -> Stamps {
    let head = read_head(&git_dir.join("HEAD"));
    let branch_ref = head
        .as_deref()
        .and_then(branch_ref_path)
        .map(|r| common_dir.join(r));
    Stamps {
        files: [
            stamp(&git_dir.join("index")),
            stamp(&common_dir.join("packed-refs")),
            branch_ref.as_deref().and_then(stamp),
            stamp(&git_dir.join("FETCH_HEAD")),
            stamp(&common_dir.join("FETCH_HEAD")),
        ],
        head,
    }
}

fn read_head(path: &Path) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(MAX_HEAD_BYTES)
        .read_to_end(&mut buf)
        .ok()?;
    Some(buf)
}

fn stamp(path: &Path) -> Option<FileStamp> {
    let meta = fs::metadata(path).ok()?;
    Some((meta.modified().ok(), meta.len()))
}

/// The loose ref file a symbolic HEAD names (`ref: refs/heads/x`), relative to
/// the common dir. HEAD is untrusted: anything that could leave the git directory is refused.
fn branch_ref_path(head: &[u8]) -> Option<PathBuf> {
    let text = std::str::from_utf8(head).ok()?.trim();
    let target = text.strip_prefix("ref:")?.trim();
    if !target.starts_with("refs/") {
        return None;
    }
    let path = PathBuf::from(target);
    path.components()
        .all(|c| matches!(c, Component::Normal(_)))
        .then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn dropping_the_watcher_from_its_own_callback_stops_it() {
        use std::sync::{Arc, Mutex};

        let dir = std::env::temp_dir().join(format!("yien-git-selfdrop-{}", std::process::id()));
        let repo = Repo {
            root: dir.clone(),
            git_dir: dir.clone(),
            common_dir: dir.clone(),
            linked_worktree: false,
            main_worktree: None,
        };
        let slot: Arc<Mutex<Option<GitWatcher>>> = Arc::new(Mutex::new(None));
        let (tx, rx) = mpsc::channel();
        let options = WatchOptions {
            interval: 10 * MS,
            refresh: Some(30 * MS),
        };
        let in_callback = Arc::clone(&slot);
        let watcher = GitWatcher::spawn(&repo, options, move |_| {
            let dropped = in_callback.lock().unwrap().take().is_some();
            let _ = tx.send(dropped);
        })
        .unwrap();
        *slot.lock().unwrap() = Some(watcher);

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if rx.recv_timeout(remaining).unwrap() {
                break;
            }
        }
        // The thread ended: its callback, and with it the sender, is gone.
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        );
    }

    #[test]
    fn change_is_reported_once_it_settles() {
        let t0 = Instant::now();
        let mut p = Poller::new(1, t0, 50 * MS, None);
        assert_eq!(p.tick(1, t0 + 100 * MS), None);
        assert_eq!(p.tick(2, t0 + 200 * MS), None);
        assert_eq!(p.tick(3, t0 + 220 * MS), None, "still changing");
        assert_eq!(p.tick(3, t0 + 240 * MS), None, "not quiet long enough");
        assert_eq!(p.tick(3, t0 + 270 * MS), Some(Change::Git));
        assert_eq!(p.tick(3, t0 + 400 * MS), None, "reported once");
    }

    #[test]
    fn refresh_fires_on_schedule_and_resets_on_report() {
        let t0 = Instant::now();
        let mut p = Poller::new(1, t0, 10 * MS, Some(1000 * MS));
        assert_eq!(p.tick(1, t0 + 999 * MS), None);
        assert_eq!(p.tick(1, t0 + 1000 * MS), Some(Change::Refresh));
        assert_eq!(p.tick(2, t0 + 1500 * MS), None);
        assert_eq!(p.tick(2, t0 + 1600 * MS), Some(Change::Git));
        assert_eq!(
            p.tick(2, t0 + 2500 * MS),
            None,
            "a git report restarts the refresh clock"
        );
        assert_eq!(p.tick(2, t0 + 2600 * MS), Some(Change::Refresh));
    }

    #[test]
    fn refresh_covers_a_pending_change() {
        let t0 = Instant::now();
        let mut p = Poller::new(1, t0, 10 * MS, Some(100 * MS));
        assert_eq!(p.tick(2, t0 + 100 * MS), Some(Change::Refresh));
        assert_eq!(p.tick(2, t0 + 150 * MS), None, "already refreshed");
    }

    #[test]
    fn branch_ref_path_accepts_only_refs_inside_the_git_dir() {
        assert_eq!(
            branch_ref_path(b"ref: refs/heads/feat/x\n"),
            Some(PathBuf::from("refs/heads/feat/x"))
        );
        assert_eq!(branch_ref_path(b"0123456789abcdef\n"), None);
        assert_eq!(branch_ref_path(b"ref: refs/../../etc/passwd"), None);
        assert_eq!(branch_ref_path(b"ref: /etc/passwd"), None);
        assert_eq!(branch_ref_path(b"ref: HEAD"), None);
        assert_eq!(branch_ref_path(b"ref: refs/heads/\xff"), None);
        assert_eq!(branch_ref_path(b""), None);
    }

    #[test]
    fn stamps_follow_head_content_and_the_branch_ref() {
        let dir = std::env::temp_dir().join(format!("yien-git-stamps-{}", std::process::id()));
        let refs = dir.join("refs/heads");
        fs::create_dir_all(&refs).unwrap();
        fs::write(dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let a = read_stamps(&dir, &dir);
        assert_eq!(a.files[2], None, "branch ref not there yet");

        fs::write(refs.join("main"), "0123\n").unwrap();
        let b = read_stamps(&dir, &dir);
        assert_ne!(a, b);
        assert!(b.files[2].is_some());

        fs::write(dir.join("HEAD"), "ref: refs/heads/other\n").unwrap();
        let c = read_stamps(&dir, &dir);
        assert_ne!(b, c);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_git_dir_reads_as_empty_stamps() {
        let s = read_stamps(
            Path::new("/yien/no/such/dir"),
            Path::new("/yien/no/such/dir"),
        );
        assert_eq!(s.head, None);
        assert!(s.files.iter().all(Option::is_none));
    }
}
