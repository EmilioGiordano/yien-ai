//! Git facts about a tab's folder, as the context bar shows them (RF-ESP-04,
//! RF-CTX-04). Plain data: the core fills it from the `git` CLI.

use serde::{Deserialize, Serialize};

/// The most changed files one [`GitFacts`] lists; [`GitFacts::changed`]
/// still counts them all.
///
/// ```
/// assert!(yien_proto::MAX_GIT_FILES >= 100);
/// ```
pub const MAX_GIT_FILES: usize = 500;

/// What git says about a tab's folder.
///
/// ```
/// use yien_proto::TabGit;
///
/// assert_eq!(TabGit::NoRepository.branch(), None);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TabGit {
    /// The folder is not inside a git work tree: "no branch".
    NoRepository,
    /// git could not answer: not installed, the repository belongs to another
    /// user, too slow. The reason says what to do.
    Unavailable {
        /// For the user.
        reason: String,
    },
    /// The folder is in a repository.
    Repository(GitFacts),
}

impl TabGit {
    /// The branch checked out, `None` when detached or not a repository.
    pub fn branch(&self) -> Option<&str> {
        match self {
            TabGit::Repository(facts) => facts.head.branch(),
            TabGit::NoRepository | TabGit::Unavailable { .. } => None,
        }
    }
}

/// What HEAD points at.
///
/// ```
/// use yien_proto::GitHead;
///
/// let head = GitHead::Detached { commit: "0123456".into() };
/// assert_eq!(head.label(), "0123456");
/// assert_eq!(head.branch(), None);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitHead {
    /// On a branch.
    Branch {
        /// Its name, such as `main` or `feat/x`.
        name: String,
        /// Whether it has no commit yet (a fresh `git init`).
        unborn: bool,
    },
    /// Not on a branch.
    Detached {
        /// The commit, abbreviated.
        commit: String,
    },
}

impl GitHead {
    /// The branch name, or the abbreviated commit when detached.
    pub fn label(&self) -> &str {
        match self {
            GitHead::Branch { name, .. } => name,
            GitHead::Detached { commit } => commit,
        }
    }

    /// The branch name, `None` when detached.
    pub fn branch(&self) -> Option<&str> {
        match self {
            GitHead::Branch { name, .. } => Some(name),
            GitHead::Detached { .. } => None,
        }
    }
}

/// The branch's upstream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitUpstream {
    /// Its name, such as `origin/main`.
    pub name: String,
    /// Commits the branch has that the upstream lacks; `None` with `behind`
    /// when the upstream branch is gone.
    pub ahead: Option<u32>,
    /// Commits the upstream has that the branch lacks.
    pub behind: Option<u32>,
}

/// How a file differs from the last commit, staged and unstaged together.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitFileStatus {
    /// Content or type changed.
    Modified,
    /// New and tracked.
    Added,
    /// Removed.
    Deleted,
    /// Moved from another path.
    Renamed {
        /// The path before.
        from: String,
    },
    /// Copied from another path.
    Copied {
        /// The path it was copied from.
        from: String,
    },
    /// Not tracked by git.
    Untracked,
    /// In a merge conflict.
    Conflicted,
}

/// One changed file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitFile {
    /// Path relative to the repository root, with `/` separators.
    pub path: String,
    /// How it changed.
    pub status: GitFileStatus,
}

/// A repository's state as one tab sees it.
///
/// ```
/// use yien_proto::{GitFacts, GitHead};
///
/// let facts = GitFacts {
///     head: GitHead::Branch { name: "main".into(), unborn: false },
///     upstream: None,
///     root: "C:/Code/yien".into(),
///     linked_worktree: false,
///     main_worktree: None,
///     files: vec![],
///     changed: 0,
/// };
/// assert!(!facts.is_dirty());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitFacts {
    /// What HEAD points at.
    pub head: GitHead,
    /// The upstream, if the branch has one.
    pub upstream: Option<GitUpstream>,
    /// The work tree root.
    pub root: String,
    /// Whether `root` is a linked worktree (`git worktree add`), not the main one.
    pub linked_worktree: bool,
    /// The main worktree's root, when `root` is linked and git could list it.
    pub main_worktree: Option<String>,
    /// The changed files in git's order, at most [`MAX_GIT_FILES`].
    pub files: Vec<GitFile>,
    /// How many files changed, counting those beyond `files`.
    pub changed: u32,
}

impl GitFacts {
    /// Whether anything differs from the last commit, untracked files included.
    pub fn is_dirty(&self) -> bool {
        self.changed > 0
    }
}
