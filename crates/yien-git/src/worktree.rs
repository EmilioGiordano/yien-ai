//! Linked worktrees (RF-ESP-05, RF-ESP-07): list them, add one on a new
//! branch, inspect one before it is removed, and remove one. Removing never
//! deletes the branch.

use std::path::{Path, PathBuf};

use crate::{Git, GitError, Head};

/// One entry of `git worktree list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// Its folder.
    pub path: PathBuf,
    /// The checked-out branch, such as `feat/x`; `None` when detached or bare.
    pub branch: Option<String>,
    /// The main worktree's entry (always the first one), not a linked one.
    pub main: bool,
    /// The repository is bare: this entry has no work tree.
    pub bare: bool,
    /// `git worktree lock` protects it from removal.
    pub locked: bool,
    /// Its folder is gone; `git worktree prune` would drop it.
    pub prunable: bool,
}

/// What would be lost, or left unpushed, if a worktree were removed now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WorktreeState {
    /// Changed and untracked files; removing deletes them.
    pub changed: u32,
    /// Commits of its branch not on its upstream, or, without an upstream,
    /// not on any other branch or remote. The branch keeps them.
    pub unpushed: u32,
}

impl WorktreeState {
    /// Whether removing it needs a second confirmation (RN-08).
    ///
    /// ```
    /// let state = yien_git::WorktreeState { changed: 0, unpushed: 2 };
    /// assert!(state.has_work());
    /// assert!(!yien_git::WorktreeState::default().has_work());
    /// ```
    pub fn has_work(&self) -> bool {
        self.changed > 0 || self.unpushed > 0
    }
}

/// Why a worktree could not be added or removed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WorktreeError {
    /// The name is not one git accepts for a branch.
    #[error("\"{0}\" is not a valid branch name; use letters, digits, '-', '_', '.' and '/'")]
    InvalidBranch(String),
    /// A branch with that name exists already.
    #[error("branch {0} already exists; pick another name")]
    BranchExists(String),
    /// The base is not a commit, branch or tag of the repository.
    #[error("there is no branch, tag or commit named \"{0}\" to start from")]
    UnknownBase(String),
    /// Something is already at the folder the worktree would use.
    #[error("{} already exists; remove it or pick another branch name", .0.display())]
    PathTaken(PathBuf),
    /// The worktree has changed or untracked files, so git kept it.
    #[error("{} has uncommitted changes, so it was kept", .0.display())]
    Dirty(PathBuf),
    /// The worktree is locked (`git worktree lock`).
    #[error("{} is locked; unlock it with git worktree unlock first", .0.display())]
    Locked(PathBuf),
    /// The folder is not a linked worktree of the repository (or is its main one).
    #[error("{} is not a linked worktree of this repository", .0.display())]
    NotLinked(PathBuf),
    /// git failed or could not run.
    #[error(transparent)]
    Git(#[from] GitError),
}

impl WorktreeError {
    /// A short name of the error for logs, which must not carry paths or names.
    ///
    /// ```
    /// let err = yien_git::WorktreeError::BranchExists("feat/x".into());
    /// assert_eq!(err.kind(), "branch exists");
    /// ```
    pub fn kind(&self) -> &'static str {
        match self {
            Self::InvalidBranch(_) => "invalid branch name",
            Self::BranchExists(_) => "branch exists",
            Self::UnknownBase(_) => "unknown base",
            Self::PathTaken(_) => "path taken",
            Self::Dirty(_) => "worktree dirty",
            Self::Locked(_) => "worktree locked",
            Self::NotLinked(_) => "not a linked worktree",
            Self::Git(e) => e.kind(),
        }
    }
}

impl GitError {
    /// A short name of the error for logs, which must not carry paths or git's words.
    ///
    /// ```
    /// assert_eq!(yien_git::GitError::NotARepository.kind(), "not a repository");
    /// ```
    pub fn kind(&self) -> &'static str {
        match self {
            Self::GitNotFound => "git not found",
            Self::NotARepository => "not a repository",
            Self::UnsafeRepository { .. } => "repository owned by another user",
            Self::UnsafeFilter { .. } => "repository filter cannot be switched off",
            Self::Timeout(_) => "timeout",
            Self::PipeHeld { .. } => "output pipe held open",
            Self::OutputTooLarge(_) => "output too large",
            Self::Failed { .. } => "git failed",
            Self::Io(_) => "could not run git",
            Self::Parse(_) => "unexpected output",
        }
    }
}

/// Windows device names that cannot be a folder name, with or without an extension.
const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// A folder name for a branch: `feat/x` becomes `feat-x`. Anything but ASCII
/// letters, digits, `.`, `_` and `-` becomes `-`; a Windows device name
/// gets a suffix. Two branches can share a slug; the caller checks the
/// folder is free.
///
/// ```
/// assert_eq!(yien_git::branch_slug("feat/new ui"), "feat-new-ui");
/// assert_eq!(yien_git::branch_slug("con"), "con-wt");
/// ```
pub fn branch_slug(branch: &str) -> String {
    let slug: String = branch
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('.').to_owned();
    if slug.is_empty() {
        return "worktree".to_owned();
    }
    let stem = slug.split('.').next().unwrap_or_default();
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
        return format!("{slug}-wt");
    }
    slug
}

/// Where a new worktree goes by default: `<parent>/<repo>-wt/<branch slug>`,
/// beside the main worktree `main_root`. `None` when `main_root` has no
/// parent or no name.
///
/// ```
/// use std::path::Path;
/// let path = yien_git::default_worktree_path(Path::new("C:/src/app"), "feat/x").unwrap();
/// assert_eq!(path, Path::new("C:/src/app-wt/feat-x"));
/// ```
pub fn default_worktree_path(main_root: &Path, branch: &str) -> Option<PathBuf> {
    let name = main_root.file_name()?.to_string_lossy();
    let parent = main_root.parent()?;
    Some(parent.join(format!("{name}-wt")).join(branch_slug(branch)))
}

impl Git {
    /// Every worktree of the repository containing `dir`, the main one first.
    ///
    /// ```no_run
    /// for wt in yien_git::Git::new().worktrees(std::path::Path::new("."))? {
    ///     println!("{} {:?}", wt.path.display(), wt.branch);
    /// }
    /// # Ok::<(), yien_git::GitError>(())
    /// ```
    pub fn worktrees(&self, dir: &Path) -> Result<Vec<Worktree>, GitError> {
        let out = self.run(dir, &["worktree", "list", "--porcelain", "-z"])?;
        Ok(parse_worktrees(&out))
    }

    /// Adds a worktree at `path` on a new branch `branch`, started from
    /// `base` (the current `HEAD` when `None`). `dir` is any folder of the
    /// repository. No hook of the repository runs, nor its own content
    /// filters, nor a git-lfs download; nothing reaches the network.
    ///
    /// Checking out a large repository takes time: give this [`Git`] a
    /// timeout of minutes, not the default. On Windows a timeout kills git
    /// but not a program it started.
    ///
    /// ```no_run
    /// use std::path::Path;
    /// let git = yien_git::Git::new().with_timeout(std::time::Duration::from_secs(120));
    /// let path = yien_git::default_worktree_path(Path::new("C:/src/app"), "feat/x").unwrap();
    /// git.add_worktree(Path::new("C:/src/app"), "feat/x", &path, None)?;
    /// # Ok::<(), yien_git::WorktreeError>(())
    /// ```
    pub fn add_worktree(
        &self,
        dir: &Path,
        branch: &str,
        path: &Path,
        base: Option<&str>,
    ) -> Result<(), WorktreeError> {
        self.check_branch(dir, branch)?;
        let base = match base.map(str::trim).filter(|b| !b.is_empty()) {
            Some(base) => {
                self.check_base(dir, base)?;
                base
            }
            None => "HEAD",
        };
        if path.exists() {
            return Err(WorktreeError::PathTaken(path.to_owned()));
        }
        let path_arg = path.to_string_lossy();
        let overrides = self.repo_filter_overrides(dir)?;
        let args = ["worktree", "add", "-q", "-b", branch, "--", &path_arg, base];
        match self.run_write(dir, &overrides, &args) {
            Ok(_) => Ok(()),
            Err(GitError::Failed { message, .. }) if message.contains("already exists") => {
                Err(if message.contains("branch") {
                    WorktreeError::BranchExists(branch.to_owned())
                } else {
                    WorktreeError::PathTaken(path.to_owned())
                })
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Refuses a name git would not take as a new branch, or one that exists.
    fn check_branch(&self, dir: &Path, branch: &str) -> Result<(), WorktreeError> {
        let invalid = || WorktreeError::InvalidBranch(branch.to_owned());
        if branch.is_empty() || branch.starts_with('-') || branch.contains("@{") {
            return Err(invalid());
        }
        match self.run(dir, &["check-ref-format", "--branch", branch]) {
            Ok(out) if String::from_utf8_lossy(&out).trim() == branch => {}
            Ok(_) | Err(GitError::Failed { .. }) => return Err(invalid()),
            Err(e) => return Err(e.into()),
        }
        let full = format!("refs/heads/{branch}");
        match self.run(dir, &["rev-parse", "--verify", "--quiet", &full]) {
            Ok(_) => Err(WorktreeError::BranchExists(branch.to_owned())),
            Err(GitError::Failed { .. }) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    fn check_base(&self, dir: &Path, base: &str) -> Result<(), WorktreeError> {
        if base.starts_with('-') {
            return Err(WorktreeError::UnknownBase(base.to_owned()));
        }
        let commit = format!("{base}^{{commit}}");
        let args = [
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &commit,
        ];
        match self.run(dir, &args) {
            Ok(_) => Ok(()),
            Err(GitError::Failed { .. }) => Err(WorktreeError::UnknownBase(base.to_owned())),
            Err(e) => Err(e.into()),
        }
    }

    /// What removing the worktree at `path` would lose or leave unpushed.
    ///
    /// ```no_run
    /// let state = yien_git::Git::new().worktree_state(std::path::Path::new("C:/src/app-wt/x"))?;
    /// if state.has_work() {
    ///     println!("{} changed files, {} unpushed commits", state.changed, state.unpushed);
    /// }
    /// # Ok::<(), yien_git::GitError>(())
    /// ```
    pub fn worktree_state(&self, path: &Path) -> Result<WorktreeState, GitError> {
        let status = self.status(path)?;
        let changed = u32::try_from(status.files.len()).unwrap_or(u32::MAX);
        let ahead = status
            .upstream
            .as_ref()
            .and_then(|u| u.ahead_behind)
            .map(|ab| ab.ahead);
        let unpushed = match (&status.head, ahead) {
            (Head::Unborn { .. }, _) => 0,
            (_, Some(ahead)) => ahead,
            (head, None) => {
                // `--branches` patterns leave out the `refs/heads/` prefix.
                let exclude = head.branch().map(|b| format!("--exclude={b}"));
                let mut args = vec!["rev-list", "--count", "HEAD", "--not"];
                args.extend(exclude.as_deref());
                args.extend(["--branches", "--remotes"]);
                let out = self.run(path, &args)?;
                String::from_utf8_lossy(&out).trim().parse().unwrap_or(0)
            }
        };
        Ok(WorktreeState { changed, unpushed })
    }

    /// Removes the linked worktree at `path`, from the repository containing
    /// `dir` (not from inside the worktree). Its branch stays. Without
    /// `discard_changes` a worktree with changed or untracked files is kept
    /// and [`WorktreeError::Dirty`] returned; with it, they are deleted.
    ///
    /// On Windows a program whose working folder is inside the worktree
    /// makes the removal fail half-way; end those programs first.
    ///
    /// ```no_run
    /// use std::path::Path;
    /// let git = yien_git::Git::new().with_timeout(std::time::Duration::from_secs(60));
    /// git.remove_worktree(Path::new("C:/src/app"), Path::new("C:/src/app-wt/feat-x"), false)?;
    /// # Ok::<(), yien_git::WorktreeError>(())
    /// ```
    pub fn remove_worktree(
        &self,
        dir: &Path,
        path: &Path,
        discard_changes: bool,
    ) -> Result<(), WorktreeError> {
        let entry = self
            .worktrees(dir)?
            .into_iter()
            .find(|wt| same_path(&wt.path, path))
            .filter(|wt| !wt.main && !wt.bare)
            .ok_or_else(|| WorktreeError::NotLinked(path.to_owned()))?;
        if entry.locked {
            return Err(WorktreeError::Locked(path.to_owned()));
        }
        let path_arg = entry.path.to_string_lossy();
        let mut args = vec!["worktree", "remove"];
        if discard_changes {
            args.push("--force");
        }
        args.extend(["--", &path_arg]);
        let overrides = self.repo_filter_overrides(dir)?;
        match self.run_write(dir, &overrides, &args) {
            Ok(_) => Ok(()),
            Err(GitError::Failed { message, .. })
                if message.contains("modified or untracked files") =>
            {
                Err(WorktreeError::Dirty(path.to_owned()))
            }
            Err(GitError::Failed { message, .. }) if message.contains("locked") => {
                Err(WorktreeError::Locked(path.to_owned()))
            }
            Err(e) => Err(e.into()),
        }
    }
}

/// Reads `git worktree list --porcelain -z`: records of NUL-terminated
/// fields, each record ended by an empty field. Unknown fields are skipped.
fn parse_worktrees(out: &[u8]) -> Vec<Worktree> {
    let mut list: Vec<Worktree> = Vec::new();
    let mut current: Option<Worktree> = None;
    for field in out.split(|&b| b == 0) {
        if field.is_empty() {
            list.extend(current.take());
            continue;
        }
        let field = String::from_utf8_lossy(field);
        if let Some(path) = field.strip_prefix("worktree ") {
            list.extend(current.take());
            current = Some(Worktree {
                path: PathBuf::from(path),
                branch: None,
                main: list.is_empty(),
                bare: false,
                locked: false,
                prunable: false,
            });
            continue;
        }
        let Some(wt) = current.as_mut() else {
            continue;
        };
        if let Some(branch) = field.strip_prefix("branch ") {
            wt.branch = Some(
                branch
                    .strip_prefix("refs/heads/")
                    .unwrap_or(branch)
                    .to_owned(),
            );
        } else if field == "bare" {
            wt.bare = true;
        } else if field == "locked" || field.starts_with("locked ") {
            wt.locked = true;
        } else if field == "prunable" || field.starts_with("prunable ") {
            wt.prunable = true;
        }
    }
    list.extend(current);
    list
}

/// Whether two paths name the same folder; textual (separators and case
/// folded on Windows) when either cannot be resolved.
pub(crate) fn same_path(a: &Path, b: &Path) -> bool {
    if let (Ok(a), Ok(b)) = (a.canonicalize(), b.canonicalize()) {
        return a == b;
    }
    let norm = |p: &Path| {
        let text = p.to_string_lossy().replace('\\', "/");
        let text = text.trim_end_matches('/').to_owned();
        if cfg!(windows) {
            text.to_lowercase()
        } else {
            text
        }
    };
    norm(a) == norm(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_list_is_parsed() {
        let out = b"worktree C:/src/app\0HEAD abc\0branch refs/heads/main\0\0\
                    worktree C:/src/app-wt/x\0HEAD def\0branch refs/heads/feat/x\0locked\0\0\
                    worktree C:/src/app-wt/d\0HEAD 123\0detached\0prunable gitdir file points to non-existent location\0\0";
        let list = parse_worktrees(out);
        assert_eq!(list.len(), 3);
        assert!(list[0].main && !list[1].main && !list[2].main);
        assert_eq!(list[0].branch.as_deref(), Some("main"));
        assert_eq!(list[1].branch.as_deref(), Some("feat/x"));
        assert!(list[1].locked && !list[1].prunable);
        assert_eq!(list[2].branch, None);
        assert!(list[2].prunable);
        assert_eq!(list[1].path, PathBuf::from("C:/src/app-wt/x"));
    }

    #[test]
    fn a_bare_main_and_garbage_are_survived() {
        let list = parse_worktrees(b"worktree /srv/app.git\0bare\0\0worktree /srv/wt\0HEAD d\0\0");
        assert!(list[0].bare && list[0].main);
        assert!(!list[1].bare && !list[1].main);
        assert!(parse_worktrees(b"").is_empty());
        assert!(parse_worktrees(b"\xff\xfe\0HEAD x\0\0").is_empty());
    }

    #[test]
    fn slugs_are_safe_folder_names() {
        assert_eq!(branch_slug("feat/x"), "feat-x");
        assert_eq!(branch_slug("fix/ñandú"), "fix--and-");
        assert_eq!(branch_slug("..."), "worktree");
        assert_eq!(branch_slug("v1.2."), "v1.2");
        assert_eq!(branch_slug("NUL.txt"), "NUL.txt-wt");
        assert_eq!(branch_slug("com1"), "com1-wt");
        assert_eq!(branch_slug("console"), "console");
    }

    #[test]
    fn the_default_path_sits_beside_the_repository() {
        let path = default_worktree_path(Path::new("/home/me/app"), "feat/x").unwrap();
        assert_eq!(path, Path::new("/home/me/app-wt/feat-x"));
        assert_eq!(default_worktree_path(Path::new("/"), "x"), None);
    }

    #[test]
    fn worktree_errors_log_their_kind_only() {
        let err = WorktreeError::PathTaken(PathBuf::from("C:/secret/place"));
        assert!(!err.kind().contains("secret"));
        let err = WorktreeError::Git(GitError::Failed {
            code: Some(128),
            message: "fatal: C:/secret".into(),
        });
        assert_eq!(err.kind(), "git failed");
    }

    #[test]
    fn same_path_folds_separators() {
        assert!(same_path(
            Path::new("Z:/no/such/dir/"),
            Path::new("Z:\\no\\such\\dir")
        ));
        assert!(!same_path(Path::new("Z:/no/a"), Path::new("Z:/no/b")));
    }
}
