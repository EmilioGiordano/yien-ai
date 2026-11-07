//! The git work behind worktree tabs (RF-ESP-05, RF-ESP-07): creating,
//! checking and removing a worktree. Each job blocks for as long as git
//! takes, so it runs on a blocking thread, never on a runtime worker.

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use yien_git::{Git, WorktreeError, default_worktree_path};
use yien_proto::{ProjectId, TabId, WorktreeCheck};
use yien_pty::PtyCloser;

use crate::workspace::worktree::{AddJob, RemoveJob};

/// How long creating or removing a worktree may take: a checkout of a large
/// repository is slow.
const WRITE_TIMEOUT: Duration = Duration::from_secs(120);

/// How long checking a worktree for changes may take.
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a removal waits for the closed tab's programs to end.
const CLOSE_WAIT: Duration = Duration::from_secs(10);

/// How long a removal waits for other programs to leave the folder.
const FREE_WAIT: Duration = Duration::from_secs(5);

/// What a job found, for the service to apply.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Done {
    Added {
        project: ProjectId,
        worktree: String,
        branch: String,
    },
    AddFailed {
        branch: String,
        message: String,
    },
    Checked {
        tab: TabId,
        worktree: String,
        check: WorktreeCheck,
    },
    Removed {
        worktree: String,
        branch: Option<String>,
    },
    RemoveFailed {
        worktree: String,
        message: String,
    },
}

/// Creates the worktree beside the repository: `<repo>-wt/<branch slug>`.
pub(crate) fn add(job: AddJob) -> Done {
    let git = Git::new().with_timeout(WRITE_TIMEOUT);
    match create(&git, &job) {
        Ok(path) => Done::Added {
            project: job.project,
            worktree: path_text(&path),
            branch: job.branch,
        },
        Err(e) => {
            tracing::debug!("worktree not created: {}", e.kind());
            Done::AddFailed {
                branch: job.branch,
                message: e.to_string(),
            }
        }
    }
}

fn create(git: &Git, job: &AddJob) -> Result<PathBuf, WorktreeError> {
    let root = Path::new(&job.root);
    let repo = git.discover(root)?;
    let main = repo.main_worktree.unwrap_or(repo.root);
    let path = default_worktree_path(&main, &job.branch)
        .ok_or_else(|| WorktreeError::PathTaken(main.clone()))?;
    git.add_worktree(root, &job.branch, &path, job.base.as_deref())?;
    Ok(path)
}

/// What removing the worktree would lose now.
pub(crate) fn check(tab: TabId, worktree: String) -> Done {
    let git = Git::new().with_timeout(CHECK_TIMEOUT);
    let check = match git.worktree_state(Path::new(&worktree)) {
        Ok(state) => WorktreeCheck::Checked {
            changed: state.changed,
            unpushed: state.unpushed,
        },
        Err(e) => {
            tracing::debug!("worktree not checked: {}", e.kind());
            WorktreeCheck::Failed {
                reason: e.to_string(),
            }
        }
    };
    Done::Checked {
        tab,
        worktree,
        check,
    }
}

/// Removes the worktree once the closed tab's programs have ended and no
/// other program holds the folder; its branch stays.
pub(crate) fn remove(job: RemoveJob, closers: &[PtyCloser]) -> Done {
    let deadline = Instant::now() + CLOSE_WAIT;
    for closer in closers {
        closer.wait(deadline.saturating_duration_since(Instant::now()));
    }
    let path = PathBuf::from(&job.worktree);
    let failed = |message: String| Done::RemoveFailed {
        worktree: job.worktree.clone(),
        message,
    };
    match wait_until_free(&path, FREE_WAIT) {
        Probe::Free => {}
        Probe::Busy => {
            return failed(
                "a program still works in it or has a file of it open. Close that program, \
                 then remove the worktree with git worktree remove."
                    .to_owned(),
            );
        }
        Probe::Stranded(at) => {
            return failed(format!(
                "it could not be put back after a check and is now at {}. Rename it back \
                 before using it.",
                at.display()
            ));
        }
    }
    let git = Git::new().with_timeout(WRITE_TIMEOUT);
    match git.remove_worktree(Path::new(&job.root), &path, job.discard_changes) {
        Ok(()) => Done::Removed {
            worktree: job.worktree,
            branch: job.branch,
        },
        Err(e) => {
            tracing::debug!("worktree not removed: {}", e.kind());
            failed(e.to_string())
        }
    }
}

/// Whether a folder can be removed whole.
#[derive(Debug, PartialEq, Eq)]
enum Probe {
    Free,
    Busy,
    /// The check moved it here and could not move it back.
    Stranded(PathBuf),
}

fn wait_until_free(path: &Path, wait: Duration) -> Probe {
    let deadline = Instant::now() + wait;
    loop {
        match probe(path) {
            Probe::Busy if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(100));
            }
            done => return done,
        }
    }
}

/// On Windows a folder that is a program's working folder, or holds an open
/// file, cannot be renamed; `git worktree remove` would then delete part of
/// it and unregister it anyway. Renaming it away and back tells first.
#[cfg(windows)]
fn probe(path: &Path) -> Probe {
    let Some(name) = path.file_name() else {
        return Probe::Free;
    };
    let mut aside = name.to_os_string();
    aside.push(".yien-closing");
    let aside = path.with_file_name(aside);
    if !path.is_dir() || aside.exists() {
        // Nothing to test, or a stray from an earlier run: git decides.
        return Probe::Free;
    }
    if std::fs::rename(path, &aside).is_err() {
        return Probe::Busy;
    }
    for _ in 0..50 {
        if std::fs::rename(&aside, path).is_ok() {
            return Probe::Free;
        }
        thread::sleep(Duration::from_millis(20));
    }
    Probe::Stranded(aside)
}

/// Elsewhere a busy folder can be removed whole.
#[cfg(not(windows))]
fn probe(_path: &Path) -> Probe {
    Probe::Free
}

/// A path as text, with `/` separators like a tab's own folder.
fn path_text(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("yien-core-wt-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("inner")).unwrap();
        dir
    }

    #[test]
    fn a_free_folder_is_free_and_stays_in_place() {
        let dir = temp("free");
        assert_eq!(
            wait_until_free(&dir.join("inner"), Duration::ZERO),
            Probe::Free
        );
        assert!(dir.join("inner").is_dir());
        assert_eq!(probe(&dir.join("missing")), Probe::Free);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn rf_esp_07_a_folder_a_program_works_in_is_busy() {
        let dir = temp("busy");
        let inner = dir.join("inner");
        let mut child = std::process::Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .current_dir(&inner)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        // The child opens its working folder as it starts, after spawn returns.
        std::thread::sleep(Duration::from_millis(500));
        let busy = wait_until_free(&inner, Duration::from_millis(300));
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(busy, Probe::Busy);
        assert!(inner.is_dir());
        assert_eq!(wait_until_free(&inner, Duration::from_secs(5)), Probe::Free);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
