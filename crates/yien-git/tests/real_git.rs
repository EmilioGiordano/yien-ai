//! Against the real `git`, on throwaway repositories. Each test skips when git is missing.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Condvar, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

use yien_git::{
    AheadBehind, Change, ChangedFile, FileStatus, Git, GitError, GitWatcher, Head, WatchOptions,
    WorktreeError, WorktreeState, default_worktree_path,
};

// Helpers sit in a cfg(test) module so clippy's allow-unwrap-in-tests covers them.
#[cfg(test)]
mod support {
    use super::*;

    /// How many sandboxes may run git at once in this test binary.
    const MAX_SANDBOXES: usize = 4;

    static RUNNING: Mutex<usize> = Mutex::new(0);
    static FREED: Condvar = Condvar::new();

    /// A slot among [`MAX_SANDBOXES`]; dropping it frees the slot.
    struct Slot;

    impl Slot {
        fn take() -> Self {
            let mut running = RUNNING.lock().unwrap_or_else(PoisonError::into_inner);
            while *running >= MAX_SANDBOXES {
                running = FREED.wait(running).unwrap_or_else(PoisonError::into_inner);
            }
            *running += 1;
            Self
        }
    }

    impl Drop for Slot {
        fn drop(&mut self) {
            let mut running = RUNNING.lock().unwrap_or_else(PoisonError::into_inner);
            *running = running.saturating_sub(1);
            FREED.notify_one();
        }
    }

    /// A temp folder holding repositories and an empty global git config, removed on drop.
    pub struct Sandbox {
        pub dir: PathBuf,
        config: PathBuf,
        _slot: Slot,
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
            let slot = Slot::take();
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
            Some(Self {
                dir,
                config,
                _slot: slot,
            })
        }

        /// Runs a setup command, isolated from the user's global and system git config.
        pub fn git(&self, cwd: &Path, args: &[&str]) -> String {
            let out = self.command(cwd, args).output().unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        }

        /// Like [`Sandbox::git`], for a command expected to fail; returns whether it succeeded.
        pub fn git_may_fail(&self, cwd: &Path, args: &[&str]) -> bool {
            self.command(cwd, args).output().unwrap().status.success()
        }

        fn command(&self, cwd: &Path, args: &[&str]) -> Command {
            let mut cmd = Command::new("git");
            cmd.current_dir(cwd)
                .args(["-c", "commit.gpgsign=false", "-c", "core.autocrlf=false"])
                .args(args)
                .env("GIT_CONFIG_GLOBAL", &self.config)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_AUTHOR_NAME", "Test")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "Test")
                .env("GIT_COMMITTER_EMAIL", "test@example.com");
            cmd
        }

        /// A shell command that creates `marker`, for a config value git would run.
        pub fn marker_command(marker: &Path) -> String {
            let path = marker.to_string_lossy().replace('\\', "/");
            format!("echo hit > '{path}'")
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
        // Generous: a loaded machine (a build, other test runs) slows git a lot.
        Git::new().with_timeout(Duration::from_secs(120))
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
fn rnf_14_status_never_writes_the_index() {
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

#[test]
fn rnf_14_repo_filters_never_run() {
    let Some(sb) = Sandbox::new() else { return };
    let root = sb.repo("filters");
    std::fs::write(root.join("b.md"), "b\n").unwrap();
    std::fs::write(root.join("c.inc"), "c\n").unwrap();
    sb.git(&root, &["add", "b.md", "c.inc"]);
    sb.git(&root, &["commit", "-q", "-m", "more"]);

    let clean = sb.dir.join("clean.marker");
    let process = sb.dir.join("process.marker");
    let included = sb.dir.join("included.marker");
    let clean_cmd = format!("{}; cat", Sandbox::marker_command(&clean));
    sb.git(&root, &["config", "filter.evil.clean", &clean_cmd]);
    sb.git(&root, &["config", "filter.evil.required", "true"]);
    sb.git(
        &root,
        &[
            "config",
            "filter.my proc.process",
            &Sandbox::marker_command(&process),
        ],
    );
    // A repo file pulled in through include.path still counts as repo config.
    let include = sb.dir.join("included.cfg");
    std::fs::write(
        &include,
        format!(
            "[filter \"inc\"]\n\tclean = \"{}; cat\"\n",
            Sandbox::marker_command(&included).replace('"', "\\\"")
        ),
    )
    .unwrap();
    sb.git(
        &root,
        &["config", "include.path", include.to_str().unwrap()],
    );
    std::fs::write(
        root.join(".git/info/attributes"),
        "*.txt filter=evil\n*.md filter=\"my proc\"\n*.inc filter=inc\n",
    )
    .unwrap();

    // Stale stat data makes status re-read the files through their filters.
    std::thread::sleep(Duration::from_millis(1100));
    for (name, content) in [("a.txt", "one\n"), ("b.md", "b\n"), ("c.inc", "c\n")] {
        std::fs::write(root.join(name), content).unwrap();
    }

    let status = git().status(&root).unwrap();
    assert!(!status.is_dirty(), "{:?}", status.files);
    for marker in [&clean, &process, &included] {
        assert!(!marker.exists(), "{} ran", marker.display());
    }
}

#[test]
fn rnf_14_repo_fsmonitor_never_runs() {
    let Some(sb) = Sandbox::new() else { return };
    let root = sb.repo("fsmonitor");
    let marker = sb.dir.join("fsmonitor.marker");
    sb.git(
        &root,
        &[
            "config",
            "core.fsmonitor",
            &Sandbox::marker_command(&marker),
        ],
    );
    std::thread::sleep(Duration::from_millis(1100));
    std::fs::write(root.join("a.txt"), "one\n").unwrap();

    git().info(&root).unwrap();
    assert!(!marker.exists(), "the fsmonitor command ran");
}

#[test]
fn rf_ctx_04_merge_conflict() {
    let Some(sb) = Sandbox::new() else { return };
    let root = sb.repo("conflict");
    sb.git(&root, &["switch", "-q", "-c", "other"]);
    std::fs::write(root.join("a.txt"), "theirs\n").unwrap();
    sb.git(&root, &["commit", "-q", "-am", "theirs"]);
    sb.git(&root, &["switch", "-q", "main"]);
    std::fs::write(root.join("a.txt"), "ours\n").unwrap();
    sb.git(&root, &["commit", "-q", "-am", "ours"]);
    assert!(!sb.git_may_fail(&root, &["merge", "-q", "other"]));

    let status = git().status(&root).unwrap();
    assert_eq!(
        status.files,
        vec![ChangedFile {
            path: "a.txt".into(),
            status: FileStatus::Conflicted
        }]
    );
}

#[test]
fn rnf_14_partial_clone_never_fetches() {
    let Some(sb) = Sandbox::new() else { return };
    let root = sb.dir.join("partial");
    std::fs::create_dir_all(&root).unwrap();
    sb.git(&root, &["init", "-q", "-b", "main"]);
    let lines: String = (1..=200).map(|n| format!("line {n}\n")).collect();
    std::fs::write(root.join("a.txt"), &lines).unwrap();
    sb.git(&root, &["add", "a.txt"]);
    sb.git(&root, &["commit", "-q", "-m", "first"]);
    let old = sb.git(&root, &["rev-parse", "HEAD:a.txt"]);
    // A staged rename with an edit makes status read the old blob for rename detection.
    sb.git(&root, &["mv", "a.txt", "moved.txt"]);
    std::fs::write(root.join("moved.txt"), format!("{lines}extra\n")).unwrap();
    sb.git(&root, &["add", "moved.txt"]);
    std::fs::remove_file(root.join(".git/objects").join(&old[..2]).join(&old[2..])).unwrap();

    let marker = sb.dir.join("ssh.marker");
    let ssh = format!("{}; false", Sandbox::marker_command(&marker));
    for (key, value) in [
        ("core.repositoryformatversion", "1"),
        ("extensions.partialClone", "origin"),
        ("remote.origin.url", "ssh://example.invalid/x"),
        ("remote.origin.promisor", "true"),
        ("core.sshCommand", ssh.as_str()),
    ] {
        sb.git(&root, &["config", key, value]);
    }

    let result = git().status(&root);
    assert!(
        !matches!(result, Err(GitError::Timeout(_))),
        "status hung: {result:?}"
    );
    assert!(!marker.exists(), "the repository's ssh command ran");
}

/// Whether `branch` exists in the repository at `root`.
fn has_branch(sb: &Sandbox, root: &Path, branch: &str) -> bool {
    let full = format!("refs/heads/{branch}");
    sb.git_may_fail(root, &["rev-parse", "--verify", "--quiet", &full])
}

#[test]
fn rf_esp_05_add_list_and_remove_keep_the_branch() {
    let Some(sb) = Sandbox::new() else { return };
    let root = sb.repo("app");
    let path = default_worktree_path(&root, "feat/x").unwrap();
    assert!(same(path.parent().unwrap().parent().unwrap(), &sb.dir));

    git().add_worktree(&root, "feat/x", &path, None).unwrap();
    assert!(path.join("a.txt").is_file());
    let list = git().worktrees(&root).unwrap();
    assert_eq!(list.len(), 2);
    assert!(list[0].main && same(&list[0].path, &root));
    assert!(same(&list[1].path, &path));
    assert_eq!(list[1].branch.as_deref(), Some("feat/x"));
    let info = git().info(&path).unwrap();
    assert!(info.repo.linked_worktree);
    assert_eq!(info.status.head.branch(), Some("feat/x"));
    assert_eq!(
        git().worktree_state(&path).unwrap(),
        WorktreeState::default()
    );

    git().remove_worktree(&root, &path, false).unwrap();
    assert!(!path.exists());
    assert_eq!(git().worktrees(&root).unwrap().len(), 1);
    assert!(has_branch(&sb, &root, "feat/x"), "the branch was deleted");
}

#[test]
fn rf_esp_05_a_base_is_where_the_branch_starts() {
    let Some(sb) = Sandbox::new() else { return };
    let root = sb.repo("based");
    let first = sb.git(&root, &["rev-parse", "HEAD"]);
    sb.git(&root, &["tag", "v1"]);
    std::fs::write(root.join("a.txt"), "two\n").unwrap();
    sb.git(&root, &["commit", "-q", "-am", "second"]);

    let path = sb.dir.join("from-tag");
    git()
        .add_worktree(&root, "fix/old", &path, Some("v1"))
        .unwrap();
    assert_eq!(sb.git(&path, &["rev-parse", "HEAD"]), first);
}

#[test]
fn rf_esp_05_add_failures_are_typed_and_leave_nothing() {
    let Some(sb) = Sandbox::new() else { return };
    let root = sb.repo("fails");
    let free = sb.dir.join("free");

    let err = git().add_worktree(&root, "main", &free, None).unwrap_err();
    assert!(
        matches!(err, WorktreeError::BranchExists(ref b) if b == "main"),
        "{err:?}"
    );
    for bad in ["bad..name", "-x", "a b", "@{-1}", "", "x.lock"] {
        let err = git().add_worktree(&root, bad, &free, None).unwrap_err();
        assert!(
            matches!(err, WorktreeError::InvalidBranch(_)),
            "{bad}: {err:?}"
        );
    }
    for base in ["nope", "--orphan", "HEAD~5"] {
        let err = git()
            .add_worktree(&root, "feat/a", &free, Some(base))
            .unwrap_err();
        assert!(
            matches!(err, WorktreeError::UnknownBase(_)),
            "{base}: {err:?}"
        );
    }
    let taken = sb.dir.join("taken");
    std::fs::create_dir_all(&taken).unwrap();
    let err = git()
        .add_worktree(&root, "feat/a", &taken, None)
        .unwrap_err();
    assert!(matches!(err, WorktreeError::PathTaken(_)), "{err:?}");
    let plain = sb.dir.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    // Inside an enclosing repository the add would write to it: skip then.
    if matches!(git().discover(&plain), Err(GitError::NotARepository)) {
        let err = git()
            .add_worktree(&plain, "feat/a", &free, None)
            .unwrap_err();
        assert!(
            matches!(err, WorktreeError::Git(GitError::NotARepository)),
            "{err:?}"
        );
    } else {
        eprintln!("temp dir sits inside a repo, skipping the not-a-repository case");
    }

    assert!(!free.exists());
    assert!(!has_branch(&sb, &root, "feat/a"));
    assert_eq!(git().worktrees(&root).unwrap().len(), 1);
}

#[test]
fn rn_08_dirty_and_unpushed_work_is_detected_and_kept() {
    let Some(sb) = Sandbox::new() else { return };
    let root = sb.repo("work");
    let path = sb.dir.join("work-wt");
    git().add_worktree(&root, "feat/w", &path, None).unwrap();

    std::fs::write(path.join("a.txt"), "edited\n").unwrap();
    std::fs::write(path.join("new.txt"), "new\n").unwrap();
    let state = git().worktree_state(&path).unwrap();
    assert_eq!(state.changed, 2);
    assert_eq!(state.unpushed, 0);
    assert!(state.has_work());

    let err = git().remove_worktree(&root, &path, false).unwrap_err();
    assert!(matches!(err, WorktreeError::Dirty(_)), "{err:?}");
    assert!(
        path.join("new.txt").is_file(),
        "a refused removal deleted files"
    );

    sb.git(&path, &["add", "-A"]);
    sb.git(&path, &["commit", "-q", "-m", "work"]);
    let state = git().worktree_state(&path).unwrap();
    assert_eq!((state.changed, state.unpushed), (0, 1));
    let tip = sb.git(&path, &["rev-parse", "HEAD"]);

    std::fs::write(path.join("scratch.txt"), "lost\n").unwrap();
    git().remove_worktree(&root, &path, true).unwrap();
    assert!(!path.exists());
    assert_eq!(sb.git(&root, &["rev-parse", "refs/heads/feat/w"]), tip);
}

#[test]
fn rn_08_unpushed_counts_against_the_upstream() {
    let Some(sb) = Sandbox::new() else { return };
    let origin = sb.repo("origin");
    let clone = sb.dir.join("clone");
    sb.git(
        &sb.dir,
        &[
            "clone",
            "-q",
            origin.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    let path = sb.dir.join("clone-wt");
    git()
        .add_worktree(&clone, "feat/u", &path, Some("origin/main"))
        .unwrap();
    sb.git(&path, &["branch", "-q", "--set-upstream-to=origin/main"]);
    std::fs::write(path.join("a.txt"), "x\n").unwrap();
    sb.git(&path, &["commit", "-q", "-am", "one"]);
    std::fs::write(path.join("a.txt"), "y\n").unwrap();
    sb.git(&path, &["commit", "-q", "-am", "two"]);
    assert_eq!(git().worktree_state(&path).unwrap().unpushed, 2);
}

#[test]
fn locked_and_main_worktrees_are_not_removed() {
    let Some(sb) = Sandbox::new() else { return };
    let root = sb.repo("locks");
    let path = sb.dir.join("locked-wt");
    git().add_worktree(&root, "feat/l", &path, None).unwrap();
    sb.git(&root, &["worktree", "lock", path.to_str().unwrap()]);

    let err = git().remove_worktree(&root, &path, true).unwrap_err();
    assert!(matches!(err, WorktreeError::Locked(_)), "{err:?}");
    let err = git().remove_worktree(&root, &root, true).unwrap_err();
    assert!(matches!(err, WorktreeError::NotLinked(_)), "{err:?}");
    let err = git()
        .remove_worktree(&root, &sb.dir.join("elsewhere"), true)
        .unwrap_err();
    assert!(matches!(err, WorktreeError::NotLinked(_)), "{err:?}");
    assert!(path.join("a.txt").is_file());
    assert!(root.join("a.txt").is_file());
}

#[test]
fn worktree_add_and_remove_run_no_repo_hook_or_filter() {
    let Some(sb) = Sandbox::new() else { return };
    let root = sb.repo("hooks");
    let hooks = root.join(".git/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let mut markers = Vec::new();
    for hook in ["post-checkout", "reference-transaction"] {
        let marker = sb.dir.join(format!("{hook}.marker"));
        let script = format!("#!/bin/sh\n{}\n", Sandbox::marker_command(&marker));
        std::fs::write(hooks.join(hook), script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::Permissions::from_mode(0o755);
            std::fs::set_permissions(hooks.join(hook), mode).unwrap();
        }
        markers.push(marker);
    }
    let smudge = sb.dir.join("smudge.marker");
    let smudge_cmd = format!("{}; cat", Sandbox::marker_command(&smudge));
    sb.git(&root, &["config", "filter.evil.smudge", &smudge_cmd]);
    std::fs::write(root.join(".gitattributes"), "*.txt filter=evil\n").unwrap();
    sb.git(&root, &["add", ".gitattributes"]);
    sb.git(&root, &["commit", "-q", "-m", "attributes"]);
    markers.push(smudge);
    for marker in &markers {
        let _ = std::fs::remove_file(marker);
    }

    let path = sb.dir.join("hooks-wt");
    git().add_worktree(&root, "feat/h", &path, None).unwrap();
    git().remove_worktree(&root, &path, true).unwrap();
    for marker in &markers {
        assert!(!marker.exists(), "{} ran", marker.display());
    }

    // The same add through plain git runs them, so the test can see a run.
    let control = sb.dir.join("control-wt");
    sb.git(
        &root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feat/c",
            control.to_str().unwrap(),
        ],
    );
    for marker in &markers {
        assert!(
            marker.exists(),
            "{} did not run under plain git",
            marker.display()
        );
    }
}

#[test]
fn rn_08_ignored_files_and_untracked_folders_are_counted() {
    let Some(sb) = Sandbox::new() else { return };
    let root = sb.repo("ignored");
    std::fs::write(root.join(".gitignore"), ".env\nnode_modules/\n").unwrap();
    sb.git(&root, &["add", ".gitignore"]);
    sb.git(&root, &["commit", "-q", "-m", "ignore"]);
    let path = sb.dir.join("ignored-wt");
    git().add_worktree(&root, "feat/i", &path, None).unwrap();
    assert_eq!(
        git().worktree_state(&path).unwrap(),
        WorktreeState::default()
    );

    std::fs::write(path.join(".env"), "SECRET=1\n").unwrap();
    std::fs::create_dir_all(path.join("node_modules/pkg")).unwrap();
    std::fs::write(path.join("node_modules/pkg/index.js"), "x\n").unwrap();
    let state = git().worktree_state(&path).unwrap();
    assert_eq!((state.changed, state.ignored), (0, 2));
    assert!(state.has_work(), "ignored files are lost on removal");
    assert!(state.ignored_examples.contains(&".env".to_owned()));
    assert!(state.ignored_examples.contains(&"node_modules/".to_owned()));
    assert_eq!(state.entries, ["ignored .env", "ignored node_modules/"]);

    std::fs::create_dir_all(path.join("new/deep")).unwrap();
    for name in ["new/a.txt", "new/b.txt", "new/deep/c.txt"] {
        std::fs::write(path.join(name), "n\n").unwrap();
    }
    std::fs::write(path.join("a.txt"), "edited\n").unwrap();
    let state = git().worktree_state(&path).unwrap();
    assert_eq!(state.changed, 4);
    assert_eq!(
        state.entries,
        [
            "changed ? new/a.txt",
            "changed ? new/b.txt",
            "changed ? new/deep/c.txt",
            "changed M a.txt",
            "ignored .env",
            "ignored node_modules/",
        ]
    );
}

#[test]
fn rn_08_commits_on_a_detached_head_are_flagged() {
    let Some(sb) = Sandbox::new() else { return };
    let root = sb.repo("detached");
    let path = sb.dir.join("detached-wt");
    git().add_worktree(&root, "feat/d", &path, None).unwrap();
    sb.git(&path, &["switch", "-q", "--detach"]);
    std::fs::write(path.join("a.txt"), "loose\n").unwrap();
    sb.git(&path, &["commit", "-q", "-am", "loose"]);
    let state = git().worktree_state(&path).unwrap();
    assert!(state.detached);
    assert_eq!(state.unpushed, 1);
    // The stranded commit is part of what a removal must not change unseen.
    let head = sb.git(&path, &["rev-parse", "HEAD"]);
    assert_eq!(state.entries, [format!("head {head}")]);
}
