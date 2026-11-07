//! Repository discovery and the combined snapshot a tab shows.

use std::path::{Path, PathBuf};

use crate::porcelain::{Status, parse_status};
use crate::{Git, GitError, ParseError};

/// Where a work tree and its git data live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    /// The work tree root (`git rev-parse --show-toplevel`).
    pub root: PathBuf,
    /// This work tree's git directory: `.git`, or `.git/worktrees/<name>` for a linked worktree.
    pub git_dir: PathBuf,
    /// The git directory shared by all worktrees of the repository.
    pub common_dir: PathBuf,
    /// Whether this is a linked worktree (`git worktree add`), not the main one.
    pub linked_worktree: bool,
    /// The main worktree's root; `None` when the repository is bare or git could not list it.
    pub main_worktree: Option<PathBuf>,
}

/// Everything the context bar and the tab row show about one directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitInfo {
    /// Where the repository is.
    pub repo: Repo,
    /// Branch, upstream and changed files.
    pub status: Status,
}

impl Git {
    /// Finds the repository that contains `dir`.
    ///
    /// Needs git 2.31 or newer (`rev-parse --path-format`).
    ///
    /// ```no_run
    /// let repo = yien_git::Git::new().discover(std::path::Path::new("."))?;
    /// if repo.linked_worktree {
    ///     println!("worktree of {:?}", repo.main_worktree);
    /// }
    /// # Ok::<(), yien_git::GitError>(())
    /// ```
    pub fn discover(&self, dir: &Path) -> Result<Repo, GitError> {
        let out = self.run(
            dir,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--show-toplevel",
                "--git-dir",
                "--git-common-dir",
            ],
        )?;
        let text = String::from_utf8_lossy(&out);
        let lines: Vec<&str> = text
            .split('\n')
            .map(|l| l.trim_end_matches('\r'))
            .filter(|l| !l.is_empty())
            .collect();
        let [root, git_dir, common_dir] = lines.as_slice() else {
            return Err(ParseError::RevParseLines {
                got: lines.len(),
                expected: 3,
            }
            .into());
        };
        let (root, git_dir, common_dir) = (
            PathBuf::from(root),
            PathBuf::from(git_dir),
            PathBuf::from(common_dir),
        );
        let linked_worktree = !same_path(&git_dir, &common_dir);
        let main_worktree = if linked_worktree {
            self.main_worktree(&root)
        } else {
            Some(root.clone())
        };
        Ok(Repo {
            root,
            git_dir,
            common_dir,
            linked_worktree,
            main_worktree,
        })
    }

    /// Branch, upstream and changed files of the work tree containing `dir`.
    ///
    /// Submodules count as changed only when their commit differs, not for
    /// edits inside them (`--ignore-submodules=dirty`), which keeps it fast.
    ///
    /// ```no_run
    /// let status = yien_git::Git::new().status(std::path::Path::new("."))?;
    /// println!("{} dirty={}", status.head.label(), status.is_dirty());
    /// # Ok::<(), yien_git::GitError>(())
    /// ```
    pub fn status(&self, dir: &Path) -> Result<Status, GitError> {
        let out = self.run(
            dir,
            &[
                "status",
                "--porcelain=v2",
                "--branch",
                "-z",
                "--ignore-submodules=dirty",
            ],
        )?;
        Ok(parse_status(&out)?)
    }

    /// [`Git::discover`] and [`Git::status`] together: two or three git calls.
    ///
    /// ```no_run
    /// let info = yien_git::Git::new().info(std::path::Path::new("."))?;
    /// println!("{} at {}", info.status.head.label(), info.repo.root.display());
    /// # Ok::<(), yien_git::GitError>(())
    /// ```
    pub fn info(&self, dir: &Path) -> Result<GitInfo, GitError> {
        let repo = self.discover(dir)?;
        let status = self.status(&repo.root)?;
        Ok(GitInfo { repo, status })
    }

    /// The first entry of `git worktree list`, which is always the main worktree.
    fn main_worktree(&self, root: &Path) -> Option<PathBuf> {
        let out = self
            .run(root, &["worktree", "list", "--porcelain", "-z"])
            .ok()?;
        main_worktree_from_list(&out)
    }
}

/// Reads the main worktree from `git worktree list --porcelain -z` output;
/// `None` when it is bare.
fn main_worktree_from_list(out: &[u8]) -> Option<PathBuf> {
    let mut path = None;
    for field in out.split(|&b| b == 0) {
        if field.is_empty() {
            break;
        }
        let field = String::from_utf8_lossy(field);
        if let Some(p) = field.strip_prefix("worktree ") {
            path = Some(PathBuf::from(p));
        } else if field == "bare" {
            return None;
        }
    }
    path
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_worktree_is_the_first_entry() {
        let out = b"worktree C:/src/app\0HEAD abc\0branch refs/heads/main\0\0\
                    worktree C:/src/app-wt\0HEAD def\0branch refs/heads/feat\0\0";
        assert_eq!(
            main_worktree_from_list(out),
            Some(PathBuf::from("C:/src/app"))
        );
    }

    #[test]
    fn bare_main_has_no_worktree() {
        let out = b"worktree /srv/app.git\0bare\0\0worktree /srv/wt\0HEAD def\0\0";
        assert_eq!(main_worktree_from_list(out), None);
    }

    #[test]
    fn garbage_worktree_list_is_none() {
        assert_eq!(main_worktree_from_list(b""), None);
        assert_eq!(main_worktree_from_list(b"\xff\xfe\0"), None);
    }
}
