//! Git facts reach the UI: against the real `git`, on throwaway repositories.
//! Each test skips when git is missing.

use std::path::{Path, PathBuf};
use std::process::Command as Process;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::mpsc::Receiver;
use yien_core::{Core, CoreConfig, CoreHandle, GitConfig, ShellCommand};
use yien_git::WatchOptions;
use yien_proto::{ClientMsg, Command, CoreMsg, GitFacts, GitFileStatus, GitHead, TabGit, TabId};
use yien_pty::FakePty;

/// git on a busy machine can be slow; the watcher itself polls every 20 ms.
const TIMEOUT: Duration = Duration::from_secs(30);

#[cfg(test)]
mod support {
    use super::*;

    /// A temp folder with an empty global git config, removed on drop.
    pub struct Sandbox {
        pub dir: PathBuf,
        config: PathBuf,
    }

    impl Sandbox {
        pub fn new() -> Option<Self> {
            let ok = Process::new("git")
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success());
            if !ok {
                eprintln!("git not found, skipping");
                return None;
            }
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "yien-core-git-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let config = dir.join("gitconfig");
            std::fs::write(&config, "").unwrap();
            Some(Self { dir, config })
        }

        /// Runs a setup command, isolated from the user's git config.
        pub fn git(&self, cwd: &Path, args: &[&str]) {
            let out = Process::new("git")
                .current_dir(cwd)
                .args(["-c", "commit.gpgsign=false", "-c", "core.autocrlf=false"])
                .args(args)
                .env("GIT_CONFIG_GLOBAL", &self.config)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_AUTHOR_NAME", "Test")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "Test")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }

        /// A new repository on `main` with one commit of `a.txt`.
        pub fn repo(&self, name: &str) -> PathBuf {
            let root = self.dir.join(name);
            std::fs::create_dir_all(&root).unwrap();
            self.git(&root, &["init", "-q", "-b", "main"]);
            std::fs::write(root.join("a.txt"), "one\n").unwrap();
            self.git(&root, &["add", "a.txt"]);
            self.git(&root, &["commit", "-q", "-m", "first"]);
            root
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    pub struct Harness {
        pub core: CoreHandle,
        events: Receiver<CoreMsg>,
        _backend: FakePty,
    }

    impl Harness {
        pub fn start() -> Self {
            let backend = FakePty::new();
            let config = CoreConfig {
                shell: ShellCommand::new("fake-shell"),
                git: Some(GitConfig {
                    watch: WatchOptions {
                        interval: Duration::from_millis(20),
                        refresh: Some(Duration::from_millis(200)),
                    },
                    timeout: Duration::from_secs(20),
                }),
                ..CoreConfig::default()
            };
            let (core, events) = Core::start(Arc::new(backend.clone()), config).unwrap();
            Self {
                core,
                events,
                _backend: backend,
            }
        }

        pub fn send(&self, command: Command) {
            self.core.try_send(ClientMsg::Command(command)).unwrap();
        }

        pub fn wait_for<T>(&mut self, mut pick: impl FnMut(&CoreMsg) -> Option<T>) -> T {
            let deadline = Instant::now() + TIMEOUT;
            loop {
                match self.events.try_recv() {
                    Ok(msg) => {
                        if let Some(found) = pick(&msg) {
                            return found;
                        }
                    }
                    Err(_) => {
                        assert!(Instant::now() < deadline, "timed out waiting for an event");
                        std::thread::sleep(Duration::from_millis(2));
                    }
                }
            }
        }

        /// Opens `root` as a project; returns its tab.
        pub fn open(&mut self, root: &Path) -> TabId {
            self.send(Command::NewProject {
                root: root.to_string_lossy().into_owned(),
                name: None,
            });
            self.wait_for(|m| match m {
                CoreMsg::ActiveTab { tab: Some(tab) } => Some(*tab),
                _ => None,
            })
        }

        /// Waits for git facts about `tab` that `accept` takes.
        pub fn git_until(&mut self, tab: TabId, mut accept: impl FnMut(&TabGit) -> bool) -> TabGit {
            self.wait_for(|m| match m {
                CoreMsg::TabGit { tab: t, git } if *t == tab && accept(git) => Some(git.clone()),
                _ => None,
            })
        }

        /// Waits for `tab` to be announced with `branch`.
        pub fn branch_until(&mut self, tab: TabId, branch: Option<&str>) {
            self.wait_for(|m| match m {
                CoreMsg::TabUpdated(t) if t.id == tab && t.branch.as_deref() == branch => Some(()),
                _ => None,
            });
        }
    }

    pub fn facts(git: &TabGit) -> Option<&GitFacts> {
        match git {
            TabGit::Repository(facts) => Some(facts),
            TabGit::NoRepository | TabGit::Unavailable { .. } => None,
        }
    }

    pub fn on(git: &TabGit, branch: &str) -> bool {
        git.branch() == Some(branch)
    }
}

use support::{Harness, Sandbox, facts, on};

#[test]
fn rf_esp_04_the_selected_tab_follows_its_branch_through_checkout_and_commit() {
    let Some(sb) = Sandbox::new() else { return };
    let root = sb.repo("app");
    std::fs::write(root.join("b.txt"), "new\n").unwrap();
    let mut h = Harness::start();
    let tab = h.open(&root);

    // The tab is announced with its branch just before the facts come.
    h.branch_until(tab, Some("main"));
    let git = h.git_until(tab, |g| on(g, "main"));
    let f = facts(&git).unwrap();
    assert!(f.is_dirty());
    assert_eq!(f.changed, 1);
    assert_eq!(f.files[0].path, "b.txt");
    assert_eq!(f.files[0].status, GitFileStatus::Untracked);
    assert!(!f.linked_worktree);

    sb.git(&root, &["switch", "-q", "-c", "test-branch"]);
    h.branch_until(tab, Some("test-branch"));
    h.git_until(tab, |g| on(g, "test-branch"));

    sb.git(&root, &["add", "b.txt"]);
    sb.git(&root, &["commit", "-q", "-m", "second"]);
    let git = h.git_until(tab, |g| facts(g).is_some_and(|f| !f.is_dirty()));
    assert!(on(&git, "test-branch"));

    sb.git(&root, &["checkout", "-q", "--detach"]);
    h.branch_until(tab, None);
    let git = h.git_until(tab, |g| {
        facts(g).is_some_and(|f| matches!(f.head, GitHead::Detached { .. }))
    });
    assert_eq!(git.branch(), None);
}

#[test]
fn rf_esp_04_a_folder_outside_git_has_no_branch_and_switching_back_watches_again() {
    let Some(sb) = Sandbox::new() else { return };
    let root = sb.repo("app");
    let plain = sb.dir.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let mut h = Harness::start();
    let repo_tab = h.open(&root);
    h.git_until(repo_tab, |g| on(g, "main"));

    let plain_tab = h.open(&plain);
    h.git_until(plain_tab, |g| *g == TabGit::NoRepository);

    // The repository tab is no longer watched; a checkout shows up once it
    // is selected again.
    sb.git(&root, &["switch", "-q", "-c", "later"]);
    h.send(Command::SelectTab { tab: repo_tab });
    h.branch_until(repo_tab, Some("later"));
    h.git_until(repo_tab, |g| on(g, "later"));
}

#[test]
fn rf_ctx_04_a_linked_worktree_names_its_main_worktree() {
    let Some(sb) = Sandbox::new() else { return };
    let root = sb.repo("app");
    let wt = sb.dir.join("app-wt");
    sb.git(
        &root,
        &["worktree", "add", "-q", "-b", "feat", &wt.to_string_lossy()],
    );
    let mut h = Harness::start();
    let tab = h.open(&wt);
    let git = h.git_until(tab, |g| on(g, "feat"));
    let f = facts(&git).unwrap();
    assert!(f.linked_worktree);
    let main = f.main_worktree.as_deref().unwrap();
    assert_eq!(
        Path::new(main).canonicalize().unwrap(),
        root.canonicalize().unwrap()
    );
}
