//! Against the real `git`, on throwaway repositories. Each test skips when git is missing.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use yien_git::{
    AheadBehind, Change, ChangedFile, FileStatus, Git, GitError, GitWatcher, Head, WatchOptions,
};

// Helpers sit in a cfg(test) module so clippy's allow-unwrap-in-tests covers them.
#[cfg(test)]
mod support {
    use super::*;

    /// A temp folder holding repositories and an empty global git config, removed on drop.
    pub struct Sandbox {
        pub dir: PathBuf,
        config: PathBuf,
    }

    impl Sandbox {
        pub fn new() -> Option<Self> {
            let ok = Command::new("git")
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success());
            if !ok {
                eprintln!("git not found, skipping");
                return None;
            }
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "yien-git-it-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let config = dir.join("gitconfig");
            std::fs::write(&config, "").unwrap();
            Some(Self { dir, config })
        }

        /// Runs a setup command, isolated from the user's global and system git config.
        pub fn git(&self, cwd: &Path, args: &[&str]) -> String {
            let out = Command::new("git")
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
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
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

    pub fn git() -> Git {
        Git::new().with_timeout(Duration::from_secs(20))
    }

    pub fn same(a: &Path, b: &Path) -> bool {
        a.canonicalize().unwrap() == b.canonicalize().unwrap()
    }
}

use support::{Sandbox, git, same};

#[test]
fn clean_repo_on_a_branch() {
    let Some(sb) = Sandbox::new() else { return };
    let root = sb.repo("clean");
    let oid = sb.git(&root, &["rev-parse", "HEAD"]);

    let info = git().info(&root).unwrap();
    assert_eq!(
        info.status.head,
        Head::Branch {
            name: "main".into(),
            oid
        }
    );
    assert!(!info.status.is_dirty());
    assert_eq!(info.status.upstream, None);
    assert!(same(&info.repo.root, &root));
    assert!(!info.repo.linked_worktree);
    assert!(same(info.repo.main_worktree.as_deref().unwrap(), &root));
}

#[test]
fn subdirectory_finds_the_root() {
    let Some(sb) = Sandbox::new() else { return };
    let root = sb.repo("sub");
    let deep = root.join("x/y");
    std::fs::create_dir_all(&deep).unwrap();
    let repo = git().discover(&deep).unwrap();
    assert!(same(&repo.root, &root));
}

#[test]
fn rf_ctx_04_lists_changed_files() {
    let Some(sb) = Sandbox::new() else { return };
    let root = sb.repo("dirty");
    std::fs::write(root.join("b.txt"), "b\n").unwrap();
    std::fs::write(root.join("c.txt"), "c\n").unwrap();
    sb.git(&root, &["add", "b.txt", "c.txt"]);
    sb.git(&root, &["commit", "-q", "-m", "more"]);

    std::fs::write(root.join("a.txt"), "two\n").unwrap();
    std::fs::remove_file(root.join("b.txt")).unwrap();
    sb.git(&root, &["mv", "c.txt", "c moved ü.txt"]);
    std::fs::write(root.join("staged new.txt"), "n\n").unwrap();
    sb.git(&root, &["add", "staged new.txt"]);
    std::fs::write(root.join("año 日本.txt"), "u\n").unwrap();

    let status = git().status(&root).unwrap();
    assert!(status.is_dirty());
    let mut files = status.files.clone();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    assert_eq!(
        files,
        vec![
            ChangedFile {
                path: "a.txt".into(),
                status: FileStatus::Modified
            },
            ChangedFile {
                path: "año 日本.txt".into(),
                status: FileStatus::Untracked
            },
            ChangedFile {
                path: "b.txt".into(),
                status: FileStatus::Deleted
            },
            ChangedFile {
                path: "c moved ü.txt".into(),
                status: FileStatus::Renamed {
                    from: "c.txt".into()
                }
            },
            ChangedFile {
                path: "staged new.txt".into(),
                status: FileStatus::Added
            },
        ]
    );
}

#[test]
fn rf_ctx_04_status_never_writes_the_index() {
    let Some(sb) = Sandbox::new() else { return };
    let root = sb.repo("readonly");
    let index = root.join(".git/index");
    // Rewriting a tracked file makes the index's stat data stale, which a plain
    // `git status` would refresh by rewriting the index.
    std::thread::sleep(Duration::from_millis(1100));
    std::fs::write(root.join("a.txt"), "one\n").unwrap();
    let before = std::fs::read(&index).unwrap();
    let before_mtime = std::fs::metadata(&index).unwrap().modified().unwrap();

    let info = git().info(&root).unwrap();
    assert!(!info.status.is_dirty());
    assert_eq!(std::fs::read(&index).unwrap(), before);
    assert_eq!(
        std::fs::metadata(&index).unwrap().modified().unwrap(),
        before_mtime
    );
    assert!(!root.join(".git/index.lock").exists());
}

#[test]
fn unborn_branch() {
    let Some(sb) = Sandbox::new() else { return };
    let root = sb.dir.join("unborn");
    std::fs::create_dir_all(&root).unwrap();
    sb.git(&root, &["init", "-q", "-b", "trunk"]);
    let status = git().status(&root).unwrap();
    assert_eq!(
        status.head,
        Head::Unborn {
            name: "trunk".into()
        }
    );
}

#[test]
fn detached_head() {
    let Some(sb) = Sandbox::new() else { return };
    let root = sb.repo("detached");
    sb.git(&root, &["checkout", "-q", "--detach"]);
    let oid = sb.git(&root, &["rev-parse", "HEAD"]);
    let status = git().status(&root).unwrap();
    assert_eq!(status.head, Head::Detached { oid: oid.clone() });
    assert_eq!(status.head.label(), &oid[..7]);
}

#[test]
fn ahead_and_behind_the_upstream() {
    let Some(sb) = Sandbox::new() else { return };
    let root = sb.repo("upstream");
    sb.git(&root, &["branch", "base"]);
    sb.git(&root, &["branch", "-q", "--set-upstream-to", "base"]);
    std::fs::write(root.join("a.txt"), "local\n").unwrap();
    sb.git(&root, &["commit", "-q", "-am", "local"]);
    sb.git(&root, &["switch", "-q", "base"]);
    std::fs::write(root.join("z.txt"), "z\n").unwrap();
    sb.git(&root, &["add", "z.txt"]);
    sb.git(&root, &["commit", "-q", "-m", "z"]);
    std::fs::write(root.join("y.txt"), "y\n").unwrap();
    sb.git(&root, &["add", "y.txt"]);
    sb.git(&root, &["commit", "-q", "-m", "y"]);
    sb.git(&root, &["switch", "-q", "main"]);

    let upstream = git().status(&root).unwrap().upstream.unwrap();
    assert_eq!(upstream.name, "base");
    assert_eq!(
        upstream.ahead_behind,
        Some(AheadBehind {
            ahead: 1,
            behind: 2
        })
    );
}

#[test]
fn linked_worktree_reports_its_main_worktree() {
    let Some(sb) = Sandbox::new() else { return };
    let root = sb.repo("main-wt");
    let wt = sb.dir.join("linked wt");
    sb.git(
        &root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feat/x",
            wt.to_str().unwrap(),
        ],
    );

    let info = git().info(&wt).unwrap();
    assert!(info.repo.linked_worktree);
    assert!(same(&info.repo.root, &wt));
    assert!(same(info.repo.main_worktree.as_deref().unwrap(), &root));
    assert!(same(&info.repo.common_dir, &root.join(".git")));
    assert_eq!(info.status.head.branch(), Some("feat/x"));

    let main = git().discover(&root).unwrap();
    assert!(!main.linked_worktree);
}

#[test]
fn outside_a_repository() {
    let Some(sb) = Sandbox::new() else { return };
    let plain = sb.dir.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    match git().info(&plain) {
        Err(GitError::NotARepository) => {}
        Ok(info) => eprintln!(
            "temp dir sits inside a repo ({}), skipping",
            info.repo.root.display()
        ),
        Err(other) => panic!("{other:?}"),
    }
}

#[test]
fn watcher_reports_a_branch_switch() {
    let Some(sb) = Sandbox::new() else { return };
    let root = sb.repo("watch");
    let repo = git().discover(&root).unwrap();
    let (tx, rx) = mpsc::channel();
    let options = WatchOptions {
        interval: Duration::from_millis(50),
        refresh: None,
    };
    let watcher = GitWatcher::spawn(&repo, options, move |c| {
        let _ = tx.send(c);
    })
    .unwrap();

    std::thread::sleep(Duration::from_millis(200));
    assert!(rx.try_recv().is_err(), "no change, no callback");

    sb.git(&root, &["switch", "-q", "-c", "other"]);
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(Change::Git));
    assert_eq!(git().status(&root).unwrap().head.branch(), Some("other"));

    let start = Instant::now();
    drop(watcher);
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "drop stops the thread"
    );
}

#[test]
fn watcher_refreshes_for_work_tree_edits() {
    let Some(sb) = Sandbox::new() else { return };
    let root = sb.repo("refresh");
    let repo = git().discover(&root).unwrap();
    let (tx, rx) = mpsc::channel();
    let options = WatchOptions {
        interval: Duration::from_millis(20),
        refresh: Some(Duration::from_millis(100)),
    };
    let _watcher = GitWatcher::spawn(&repo, options, move |c| {
        let _ = tx.send(c);
    })
    .unwrap();
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(Change::Refresh));
}
