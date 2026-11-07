//! The git work behind worktree tabs (RF-ESP-05, RF-ESP-07): creating,
//! checking and removing a worktree. Each job blocks for as long as git
//! takes, so it runs on a blocking thread, never on a runtime worker.

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use yien_git::{Git, WorktreeError, WorktreeState, default_worktree_path};
use yien_proto::{ProjectId, TabId, WorktreeCheck, WorktreeWork};

use crate::consoles::Consoles;

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
        Err((e, path)) => {
            tracing::debug!("worktree not created: {}", e.kind());
            let left = match (&e, &path) {
                // git ran and did not finish cleanly: say what it left.
                (WorktreeError::Git(_), Some(path)) => leftovers(&git, &job, path),
                _ => Vec::new(),
            };
            let message = if left.is_empty() {
                format!("No worktree was created for {}: {e}.", job.branch)
            } else {
                format!(
                    "Creating the worktree for {} failed part-way ({e}). It left {}; nothing \
                     was deleted.",
                    job.branch,
                    left.join(", ")
                )
            };
            Done::AddFailed { message }
        }
    }
}

/// Creates the worktree; a failure comes with the folder it was aimed at,
/// once that is known.
fn create(git: &Git, job: &AddJob) -> Result<PathBuf, (WorktreeError, Option<PathBuf>)> {
    let root = Path::new(&job.root);
    let repo = git.discover(root).map_err(|e| (e.into(), None))?;
    let main = repo.main_worktree.unwrap_or(repo.root);
    let path = default_worktree_path(&main, &job.branch)
        .ok_or_else(|| (WorktreeError::NoRoom(main.clone()), None))?;
    git.add_worktree(root, &job.branch, &path, job.base.as_deref())
        .map_err(|e| (e, Some(path.clone())))?;
    Ok(path)
}

/// What a failed add left behind: the branch, the registration, the folder.
/// The checks before git ran ruled all three out, so each is git's doing.
fn leftovers(git: &Git, job: &AddJob, path: &Path) -> Vec<String> {
    let root = Path::new(&job.root);
    let mut left = Vec::new();
    if git.has_branch(root, &job.branch).unwrap_or(false) {
        left.push(format!("the branch {}", job.branch));
    }
    let registered = git
        .worktrees(root)
        .map(|list| list.iter().any(|wt| same_dir(&wt.path, path)))
        .unwrap_or(false);
    if registered {
        left.push("a worktree registered with git (git worktree list)".to_owned());
    }
    if path.exists() {
        left.push(format!("the folder {}", path_text(path)));
    }
    left
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let text = |p: &Path| path_text(p).trim_end_matches('/').to_lowercase();
    text(a) == text(b)
}

/// What removing the worktree would lose now.
pub(crate) fn check(tab: TabId, worktree: String) -> Done {
    let git = Git::new().with_timeout(CHECK_TIMEOUT);
    let check = match git.worktree_state(Path::new(&worktree)) {
        Ok(state) => WorktreeCheck::Checked {
            work: work_of(&state),
            ignored_examples: state.ignored_examples,
            detached: state.detached,
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

fn work_of(state: &WorktreeState) -> WorktreeWork {
    WorktreeWork {
        changed: state.changed,
        ignored: state.ignored,
        unpushed: state.unpushed,
    }
}

/// Removes the worktree once the closed tab's programs have ended, no other
/// program holds the folder, and it holds no more than the user confirmed;
/// its branch stays.
pub(crate) fn remove(job: RemoveJob, consoles: &Consoles) -> Done {
    let deadline = Instant::now() + CLOSE_WAIT;
    for closer in consoles.closers_after_spawns(&job.panes, CLOSE_WAIT) {
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
    // The programs may have written more since the user confirmed.
    let now = match Git::new().with_timeout(CHECK_TIMEOUT).worktree_state(&path) {
        Ok(state) => work_of(&state),
        Err(e) => return failed(format!("it could not be checked again ({e})")),
    };
    if !now.within(&job.confirmed) {
        return failed(format!(
            "it changed after you confirmed: it now has {} changed, {} ignored and {} \
             unpushed. Close the tab again to see what removing it would lose.",
            now.changed, now.ignored, now.unpushed
        ));
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
        // The child opens its working folder as it starts, after spawn returns;
        // moving the folder before that would only make its start fail.
        std::thread::sleep(Duration::from_secs(1));
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut busy = probe(&inner);
        while busy == Probe::Free && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
            busy = probe(&inner);
        }
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(busy, Probe::Busy);
        assert!(inner.is_dir());
        assert_eq!(wait_until_free(&inner, Duration::from_secs(5)), Probe::Free);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
