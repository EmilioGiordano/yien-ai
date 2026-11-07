//! Worktree tabs through the core (RF-ESP-05, RF-ESP-07): against the real
//! `git`, on throwaway repositories under the temp folder. Each test skips
//! when git is missing.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers outside #[test] fns are not covered by clippy.toml's in-test allowances"
)]

use std::path::{Path, PathBuf};
use std::process::Command as Process;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::mpsc::Receiver;
use yien_core::{Core, CoreConfig, CoreHandle, ShellCommand};
use yien_proto::{ClientMsg, Command, CoreMsg, ProjectId, TabInfo, WorktreeCheck, WorktreeWork};

const ONE_CHANGE: WorktreeWork = WorktreeWork {
    changed: 1,
    ignored: 0,
    unpushed: 0,
};
use yien_pty::{FakePty, PtyBackend};

const TIMEOUT: Duration = Duration::from_secs(60);

/// A temp folder with an empty global git config, removed on drop.
struct Sandbox {
    dir: PathBuf,
    config: PathBuf,
}

impl Sandbox {
    fn new() -> Option<Self> {
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
            "yien-core-wt-it-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("gitconfig");
        std::fs::write(&config, "").unwrap();
        Some(Self { dir, config })
    }

    fn command(&self, cwd: &Path, args: &[&str]) -> Process {
        let mut cmd = Process::new("git");
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

    fn git(&self, cwd: &Path, args: &[&str]) {
        let out = self.command(cwd, args).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn has_branch(&self, root: &Path, branch: &str) -> bool {
        let full = format!("refs/heads/{branch}");
        self.command(root, &["rev-parse", "--verify", "--quiet", &full])
            .output()
            .unwrap()
            .status
            .success()
    }

    /// A new repository on `main` with one commit, inside the sandbox.
    fn repo(&self, name: &str) -> PathBuf {
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

struct Harness {
    core: CoreHandle,
    events: Receiver<CoreMsg>,
}

impl Harness {
    fn start(backend: Arc<dyn PtyBackend>, shell: ShellCommand) -> Self {
        let config = CoreConfig {
            shell,
            ..CoreConfig::default()
        };
        let (core, events) = Core::start(backend, config).unwrap();
        Self { core, events }
    }

    fn fake() -> Self {
        Self::start(Arc::new(FakePty::new()), ShellCommand::new("fake-shell"))
    }

    fn send(&self, command: Command) {
        self.core.try_send(ClientMsg::Command(command)).unwrap();
    }

    fn wait_for<T>(&mut self, mut pick: impl FnMut(&CoreMsg) -> Option<T>) -> T {
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
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
    }

    /// Opens `root` as a project; returns the project.
    fn open(&mut self, root: &Path) -> ProjectId {
        self.send(Command::NewProject {
            root: root.to_string_lossy().into_owned(),
            name: None,
        });
        self.wait_for(|m| match m {
            CoreMsg::ProjectUpdated(p) => Some(p.id),
            _ => None,
        })
    }

    /// Waits for a tab announced on a worktree.
    fn worktree_tab(&mut self) -> TabInfo {
        self.wait_for(|m| match m {
            CoreMsg::TabUpdated(t) if t.worktree.is_some() => Some(t.clone()),
            CoreMsg::Error { message, .. } => panic!("{message}"),
            _ => None,
        })
    }

    /// Waits for the next notice or error, skipping everything else.
    fn outcome(&mut self) -> Result<String, String> {
        self.wait_for(|m| match m {
            CoreMsg::Notice { message } => Some(Ok(message.clone())),
            CoreMsg::Error { message, .. } => Some(Err(message.clone())),
            _ => None,
        })
    }
}

fn same(a: &Path, b: &Path) -> bool {
    a.canonicalize().unwrap() == b.canonicalize().unwrap()
}

#[test]
fn rf_esp_05_a_worktree_tab_opens_on_a_new_worktree_and_its_removal_keeps_the_branch() {
    let Some(sb) = Sandbox::new() else { return };
    let root = sb.repo("app");
    let mut h = Harness::fake();
    let project = h.open(&root);

    h.send(Command::NewWorktreeTab {
        project,
        branch: "feat/x".into(),
        base: None,
    });
    let tab = h.worktree_tab();
    let expected = sb.dir.join("app-wt").join("feat-x");
    let worktree = PathBuf::from(tab.worktree.clone().unwrap());
    assert!(same(&worktree, &expected));
    assert_eq!(tab.dir, tab.worktree.clone().unwrap());
    assert_eq!(tab.branch.as_deref(), Some("feat/x"));
    assert!(expected.join("a.txt").is_file());

    h.send(Command::CheckWorktree { tab: tab.id });
    let check = h.wait_for(|m| match m {
        CoreMsg::WorktreeChecked { tab: t, check, .. } if *t == tab.id => Some(check.clone()),
        _ => None,
    });
    assert_eq!(
        check,
        WorktreeCheck::Checked {
            work: WorktreeWork::default(),
            ignored_examples: vec![],
            detached: false,
        }
    );

    h.send(Command::RemoveWorktreeTab {
        tab: tab.id,
        discard_changes: false,
        confirmed: WorktreeWork::default(),
    });
    let notice = h.outcome().unwrap();
    assert!(notice.contains("feat/x"), "{notice}");
    assert!(!expected.exists(), "the worktree folder is still there");
    assert!(sb.has_branch(&root, "feat/x"), "the branch was deleted");
}

#[test]
fn rf_esp_05_a_failed_worktree_opens_no_tab() {
    let Some(sb) = Sandbox::new() else { return };
    let root = sb.repo("fails");
    let mut h = Harness::fake();
    let project = h.open(&root);

    for (branch, base) in [("main", None), ("feat/y", Some("nope")), ("bad..x", None)] {
        h.send(Command::NewWorktreeTab {
            project,
            branch: branch.into(),
            base: base.map(str::to_owned),
        });
        let message = h.wait_for(|m| match m {
            CoreMsg::TabUpdated(t) if t.worktree.is_some() => {
                panic!("a tab opened for {branch}")
            }
            CoreMsg::Error { message, .. } => Some(message.clone()),
            _ => None,
        });
        assert!(message.contains(branch), "{message}");
    }
    std::fs::create_dir_all(sb.dir.join("fails-wt").join("feat-z")).unwrap();
    h.send(Command::NewWorktreeTab {
        project,
        branch: "feat/z".into(),
        base: None,
    });
    let message = h.outcome().unwrap_err();
    assert!(message.contains("already exists"), "{message}");
    assert!(!sb.has_branch(&root, "feat/z"));
    assert!(!sb.has_branch(&root, "feat/y"));
}

#[test]
fn rn_08_a_dirty_worktree_is_kept_unless_its_changes_are_discarded() {
    let Some(sb) = Sandbox::new() else { return };
    let root = sb.repo("dirty");
    let mut h = Harness::fake();
    let project = h.open(&root);
    for branch in ["feat/a", "feat/b"] {
        h.send(Command::NewWorktreeTab {
            project,
            branch: branch.into(),
            base: None,
        });
    }
    let a = h.worktree_tab();
    let b = h.worktree_tab();
    let (a, b) = if a.branch.as_deref() == Some("feat/a") {
        (a, b)
    } else {
        (b, a)
    };
    let a_dir = PathBuf::from(a.worktree.clone().unwrap());
    let b_dir = PathBuf::from(b.worktree.clone().unwrap());
    std::fs::write(a_dir.join("new.txt"), "work\n").unwrap();
    std::fs::write(b_dir.join("new.txt"), "work\n").unwrap();

    h.send(Command::CheckWorktree { tab: a.id });
    let check = h.wait_for(|m| match m {
        CoreMsg::WorktreeChecked { check, .. } => Some(check.clone()),
        _ => None,
    });
    assert_eq!(
        check,
        WorktreeCheck::Checked {
            work: ONE_CHANGE,
            ignored_examples: vec![],
            detached: false,
        }
    );

    h.send(Command::RemoveWorktreeTab {
        tab: a.id,
        discard_changes: false,
        confirmed: ONE_CHANGE,
    });
    let message = h.outcome().unwrap_err();
    assert!(message.contains("uncommitted"), "{message}");
    assert!(a_dir.join("new.txt").is_file());

    h.send(Command::RemoveWorktreeTab {
        tab: b.id,
        discard_changes: true,
        confirmed: ONE_CHANGE,
    });
    h.outcome().unwrap();
    assert!(!b_dir.exists());
    assert!(sb.has_branch(&root, "feat/a") && sb.has_branch(&root, "feat/b"));
}

/// With a real shell working in the worktree, the removal waits for it to end.
#[cfg(windows)]
#[test]
fn rf_esp_07_removal_waits_for_the_tab_shell_to_leave_the_worktree() {
    let Some(sb) = Sandbox::new() else { return };
    let root = sb.repo("shell");
    let backend = Arc::new(yien_pty::PortablePty::new());
    let mut h = Harness::start(backend, ShellCommand::new("cmd.exe"));
    let project = h.open(&root);
    h.send(Command::NewWorktreeTab {
        project,
        branch: "feat/s".into(),
        base: None,
    });
    let tab = h.worktree_tab();
    let pane = tab.focused;
    h.wait_for(|m| match m {
        CoreMsg::PaneUpdated(p) if p.id == pane && p.running => Some(()),
        _ => None,
    });
    let worktree = PathBuf::from(tab.worktree.unwrap());

    h.send(Command::RemoveWorktreeTab {
        tab: tab.id,
        discard_changes: false,
        confirmed: WorktreeWork::default(),
    });
    let notice = h.outcome().unwrap();
    assert!(notice.contains("feat/s"), "{notice}");
    assert!(!worktree.exists());
    assert!(sb.has_branch(&root, "feat/s"));
}

#[test]
fn rn_08_work_written_after_the_confirm_keeps_the_worktree() {
    let Some(sb) = Sandbox::new() else { return };
    let root = sb.repo("late");
    let mut h = Harness::fake();
    let project = h.open(&root);
    h.send(Command::NewWorktreeTab {
        project,
        branch: "feat/late".into(),
        base: None,
    });
    let tab = h.worktree_tab();
    let dir = PathBuf::from(tab.worktree.clone().unwrap());
    std::fs::write(dir.join("seen.txt"), "seen\n").unwrap();
    // The user confirmed one change; an agent writes another before the removal.
    std::fs::write(dir.join("late.txt"), "late\n").unwrap();
    h.send(Command::RemoveWorktreeTab {
        tab: tab.id,
        discard_changes: true,
        confirmed: ONE_CHANGE,
    });
    let message = h.outcome().unwrap_err();
    assert!(message.contains("changed after you confirmed"), "{message}");
    assert!(dir.join("late.txt").is_file() && dir.join("seen.txt").is_file());
    assert!(sb.has_branch(&root, "feat/late"));
}
