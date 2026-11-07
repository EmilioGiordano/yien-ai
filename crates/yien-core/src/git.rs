//! Git facts for tabs (RF-ESP-04, RF-CTX-04): one thread runs every `git`
//! call, watches the selected tab's repository and reports what it finds.
//!
//! git runs off the runtime: a call can take up to its timeout, and dropping
//! a [`GitWatcher`] joins its thread, so neither may happen on a tokio worker.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use tokio::sync::mpsc as tokio_mpsc;
use yien_git::{
    ChangedFile, FileStatus, Git, GitError, GitInfo, GitWatcher, Head, Upstream, WatchOptions,
};
use yien_proto::{
    GitFacts, GitFile, GitFileStatus, GitHead, GitUpstream, MAX_GIT_FILES, TabGit, TabId,
};

/// How git facts are gathered.
///
/// ```
/// let config = yien_core::GitConfig::default();
/// assert!(config.timeout.as_secs() >= 1);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitConfig {
    /// How the selected tab's repository is watched.
    pub watch: WatchOptions,
    /// The longest one `git` call may take.
    pub timeout: Duration,
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            watch: WatchOptions::default(),
            timeout: yien_git::DEFAULT_TIMEOUT,
        }
    }
}

/// Facts the git thread found about a tab.
#[derive(Debug)]
pub(crate) struct GitReport {
    pub tab: TabId,
    pub git: TabGit,
}

/// Results waiting for the service; the git thread waits for room.
pub(crate) const GIT_REPORTS: usize = 16;

enum Request {
    /// Watch this tab's folder, or nothing.
    Watch(Option<(TabId, PathBuf)>),
    /// Look at a tab's folder once.
    Probe(TabId, PathBuf),
    /// The watcher of generation `.0` saw a change.
    Changed(u64),
    /// The service is gone. Needed because the watcher callbacks keep the
    /// channel open, so the thread never sees it disconnect.
    Stop,
}

/// The service's end of the git thread. Dropping it ends the thread once
/// its current `git` call returns.
pub(crate) struct GitThread {
    requests: mpsc::Sender<Request>,
    watching: Option<(TabId, String)>,
}

impl GitThread {
    /// Starts the thread. `None` when the OS cannot start it; tabs then show
    /// no git facts.
    pub fn start(config: GitConfig) -> Option<(Self, tokio_mpsc::Receiver<GitReport>)> {
        let (requests, rx) = mpsc::channel();
        let (reports, reports_rx) = tokio_mpsc::channel(GIT_REPORTS);
        let wake = requests.clone();
        let spawned = thread::Builder::new()
            .name("yien-git".to_owned())
            .spawn(move || Worker::new(config, wake, reports).run(&rx));
        match spawned {
            Ok(_) => Some((
                Self {
                    requests,
                    watching: None,
                },
                reports_rx,
            )),
            Err(e) => {
                tracing::warn!("git facts are off: could not start their thread: {e}");
                None
            }
        }
    }

    /// Watches `target`'s folder, dropping the previous watch; nothing when it
    /// is the one already watched.
    pub fn watch(&mut self, target: Option<(TabId, &str)>) {
        let current = self
            .watching
            .as_ref()
            .map(|(tab, dir)| (*tab, dir.as_str()));
        if current == target {
            return;
        }
        let wanted = target.map(|(tab, dir)| (tab, dir.to_owned()));
        let request = wanted.as_ref().map(|(tab, dir)| (*tab, PathBuf::from(dir)));
        self.watching = wanted;
        let _ = self.requests.send(Request::Watch(request));
    }

    /// The tab watched now.
    pub fn watched(&self) -> Option<TabId> {
        self.watching.as_ref().map(|(tab, _)| *tab)
    }

    /// Looks at a tab's folder once.
    pub fn probe(&self, tab: TabId, dir: &str) {
        let _ = self.requests.send(Request::Probe(tab, PathBuf::from(dir)));
    }
}

impl Drop for GitThread {
    /// Not joined: the thread may be inside a `git` call, and this runs on a
    /// tokio worker. It ends once that call returns.
    fn drop(&mut self) {
        let _ = self.requests.send(Request::Stop);
    }
}

/// What the git thread watches.
struct Watched {
    tab: TabId,
    dir: PathBuf,
    /// The git directory `watcher` polls; `None` while the folder is not a repository.
    watcher: Option<(PathBuf, GitWatcher)>,
}

struct Worker {
    git: Git,
    options: WatchOptions,
    /// Hands the watcher callbacks a way back to this thread.
    wake: mpsc::Sender<Request>,
    reports: tokio_mpsc::Sender<GitReport>,
    watched: Option<Watched>,
    /// Bumped for every new target and watcher, so a change seen by a
    /// dropped watcher is ignored.
    generation: u64,
}

impl Worker {
    fn new(
        config: GitConfig,
        wake: mpsc::Sender<Request>,
        reports: tokio_mpsc::Sender<GitReport>,
    ) -> Self {
        Self {
            git: Git::new().with_timeout(config.timeout),
            options: config.watch,
            wake,
            reports,
            watched: None,
            generation: 0,
        }
    }

    fn run(mut self, requests: &mpsc::Receiver<Request>) {
        loop {
            // A folder that is not a repository has no watcher; look again
            // now and then, so a `git init` shows up.
            let idle = self.watched.as_ref().is_some_and(|w| w.watcher.is_none());
            let first = match self.options.refresh.filter(|_| idle) {
                Some(every) => match requests.recv_timeout(every) {
                    Ok(request) => Some(request),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return,
                },
                None => match requests.recv() {
                    Ok(request) => Some(request),
                    Err(_) => return,
                },
            };
            let mut refresh = first.is_none();
            let mut probes = Vec::new();
            for request in first.into_iter().chain(requests.try_iter()) {
                match request {
                    Request::Watch(target) => {
                        self.generation += 1;
                        self.watched = target.map(|(tab, dir)| Watched {
                            tab,
                            dir,
                            watcher: None,
                        });
                        refresh = true;
                    }
                    Request::Probe(tab, dir) => probes.push((tab, dir)),
                    Request::Changed(generation) => refresh |= generation == self.generation,
                    Request::Stop => return,
                }
            }
            if refresh && !self.refresh() {
                return;
            }
            for (tab, dir) in probes {
                let git = facts(&self.git, &dir).0;
                if self.reports.blocking_send(GitReport { tab, git }).is_err() {
                    return;
                }
            }
        }
    }

    /// Reads the watched folder and reports it, starting or retargeting the
    /// watcher as the repository requires. `false` when the service is gone.
    fn refresh(&mut self) -> bool {
        let Some(watched) = &mut self.watched else {
            return true;
        };
        let (git, info) = facts(&self.git, &watched.dir);
        let repo = info.map(|i| i.repo);
        let keep = matches!(
            (&watched.watcher, &repo),
            (Some((old, _)), Some(repo)) if *old == repo.git_dir
        );
        if !keep {
            watched.watcher = None;
            if let Some(repo) = repo {
                self.generation += 1;
                let generation = self.generation;
                let wake = self.wake.clone();
                let spawned = GitWatcher::spawn(&repo, self.options, move |_| {
                    let _ = wake.send(Request::Changed(generation));
                });
                match spawned {
                    Ok(watcher) => watched.watcher = Some((repo.git_dir, watcher)),
                    Err(e) => tracing::debug!("git watcher did not start: {}", e.kind()),
                }
            }
        }
        let tab = watched.tab;
        self.reports.blocking_send(GitReport { tab, git }).is_ok()
    }
}

/// What git says about `dir`, and the raw info when it is a repository.
fn facts(git: &Git, dir: &Path) -> (TabGit, Option<GitInfo>) {
    match git.info(dir) {
        Ok(info) => (TabGit::Repository(to_facts(&info)), Some(info)),
        Err(GitError::NotARepository) => (TabGit::NoRepository, None),
        Err(e) => {
            // The message can hold paths and git's own words; the log gets the kind only.
            tracing::debug!("git facts unavailable: {}", e.kind());
            (
                TabGit::Unavailable {
                    reason: e.to_string(),
                },
                None,
            )
        }
    }
}

/// The protocol form of a repository's state.
pub(crate) fn to_facts(info: &GitInfo) -> GitFacts {
    let status = &info.status;
    let head = match &status.head {
        Head::Branch { name, .. } => GitHead::Branch {
            name: name.clone(),
            unborn: false,
        },
        Head::Unborn { name } => GitHead::Branch {
            name: name.clone(),
            unborn: true,
        },
        Head::Detached { .. } => GitHead::Detached {
            commit: status.head.label().to_owned(),
        },
    };
    GitFacts {
        head,
        upstream: status.upstream.as_ref().map(upstream),
        root: path_text(&info.repo.root),
        linked_worktree: info.repo.linked_worktree,
        main_worktree: info
            .repo
            .main_worktree
            .as_deref()
            .filter(|_| info.repo.linked_worktree)
            .map(path_text),
        files: status.files.iter().take(MAX_GIT_FILES).map(file).collect(),
        changed: u32::try_from(status.files.len()).unwrap_or(u32::MAX),
    }
}

fn upstream(u: &Upstream) -> GitUpstream {
    GitUpstream {
        name: u.name.clone(),
        ahead: u.ahead_behind.map(|ab| ab.ahead),
        behind: u.ahead_behind.map(|ab| ab.behind),
    }
}

fn file(f: &ChangedFile) -> GitFile {
    let status = match &f.status {
        FileStatus::Added => GitFileStatus::Added,
        FileStatus::Deleted => GitFileStatus::Deleted,
        FileStatus::Renamed { from } => GitFileStatus::Renamed { from: from.clone() },
        FileStatus::Copied { from } => GitFileStatus::Copied { from: from.clone() },
        FileStatus::Untracked => GitFileStatus::Untracked,
        FileStatus::Conflicted => GitFileStatus::Conflicted,
        // `Modified`, and any kind a newer yien-git adds.
        _ => GitFileStatus::Modified,
    };
    GitFile {
        path: f.path.clone(),
        status,
    }
}

/// A path as text, with `/` separators like the tab's own folder.
fn path_text(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use yien_git::{AheadBehind, Repo, Status};

    fn info(head: Head, files: Vec<ChangedFile>, linked: bool) -> GitInfo {
        GitInfo {
            repo: Repo {
                root: PathBuf::from("C:\\src\\app-wt"),
                git_dir: PathBuf::from("C:\\src\\app\\.git\\worktrees\\app-wt"),
                common_dir: PathBuf::from("C:\\src\\app\\.git"),
                linked_worktree: linked,
                main_worktree: Some(PathBuf::from("C:\\src\\app")),
            },
            status: Status {
                head,
                upstream: Some(Upstream {
                    name: "origin/feat".into(),
                    ahead_behind: Some(AheadBehind {
                        ahead: 3,
                        behind: 1,
                    }),
                }),
                files,
            },
        }
    }

    /// Whether the thread behind `reports` ends within a bounded wait: it
    /// drops its sender when it returns.
    fn thread_ends(mut reports: tokio_mpsc::Receiver<GitReport>) -> bool {
        let (done, ended) = mpsc::channel();
        thread::spawn(move || {
            while reports.blocking_recv().is_some() {}
            let _ = done.send(());
        });
        ended.recv_timeout(Duration::from_secs(30)).is_ok()
    }

    fn quick() -> GitConfig {
        GitConfig {
            watch: WatchOptions {
                interval: Duration::from_millis(10),
                refresh: Some(Duration::from_millis(50)),
            },
            timeout: Duration::from_secs(20),
        }
    }

    #[test]
    fn dropping_the_handle_ends_an_idle_thread() {
        let (git, reports) = GitThread::start(quick()).unwrap();
        drop(git);
        assert!(thread_ends(reports));
    }

    #[test]
    fn dropping_the_handle_ends_a_thread_that_watches() {
        let plain = std::env::temp_dir();
        let repo = env!("CARGO_MANIFEST_DIR");
        for dir in [plain.to_string_lossy().into_owned(), repo.to_owned()] {
            let (mut git, mut reports) = GitThread::start(quick()).unwrap();
            git.watch(Some((TabId::new(1), &dir)));
            assert!(reports.blocking_recv().is_some(), "watching {dir}");
            drop(git);
            assert!(thread_ends(reports), "watching {dir}");
        }
    }

    #[test]
    fn rf_ctx_04_facts_carry_head_upstream_worktree_and_files() {
        let files = vec![
            ChangedFile {
                path: "a.txt".into(),
                status: FileStatus::Modified,
            },
            ChangedFile {
                path: "b.txt".into(),
                status: FileStatus::Renamed {
                    from: "old.txt".into(),
                },
            },
        ];
        let head = Head::Branch {
            name: "feat".into(),
            oid: "0123456789".into(),
        };
        let facts = to_facts(&info(head, files, true));
        assert_eq!(
            facts.head,
            GitHead::Branch {
                name: "feat".into(),
                unborn: false
            }
        );
        let up = facts.upstream.unwrap();
        assert_eq!((up.ahead, up.behind), (Some(3), Some(1)));
        assert_eq!(facts.root, "C:/src/app-wt");
        assert!(facts.linked_worktree);
        assert_eq!(facts.main_worktree.as_deref(), Some("C:/src/app"));
        assert_eq!(facts.changed, 2);
        assert_eq!(
            facts.files[1].status,
            GitFileStatus::Renamed {
                from: "old.txt".into()
            }
        );
    }

    #[test]
    fn a_detached_head_shows_a_short_commit_and_the_main_worktree_names_nothing() {
        let head = Head::Detached {
            oid: "0123456789abcdef".into(),
        };
        let facts = to_facts(&info(head, vec![], false));
        assert_eq!(
            facts.head,
            GitHead::Detached {
                commit: "0123456".into()
            }
        );
        assert!(!facts.linked_worktree);
        assert_eq!(facts.main_worktree, None);
        assert!(!facts.is_dirty());
    }

    #[test]
    fn a_huge_change_list_is_capped_but_counted() {
        let files = (0..MAX_GIT_FILES + 7)
            .map(|i| ChangedFile {
                path: format!("f{i}"),
                status: FileStatus::Untracked,
            })
            .collect();
        let head = Head::Unborn {
            name: "main".into(),
        };
        let facts = to_facts(&info(head, files, false));
        assert_eq!(facts.files.len(), MAX_GIT_FILES);
        assert_eq!(facts.changed as usize, MAX_GIT_FILES + 7);
        assert!(matches!(facts.head, GitHead::Branch { unborn: true, .. }));
    }
}
