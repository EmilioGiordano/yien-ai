//! Read-only git facts for a tab (layer 2): branch or detached commit,
//! repository root, linked worktree and its main worktree, dirty flag,
//! ahead/behind, and the changed files for the context bar (RF-CTX-04).
//!
//! It runs the `git` CLI and never writes to the repository, the index or git
//! config: no optional locks, no fsmonitor, no fetch, no hooks. [`GitWatcher`]
//! tells the active tab when to ask again.
//!
//! ```no_run
//! let git = yien_git::Git::new();
//! match git.info(std::path::Path::new(".")) {
//!     Ok(info) => println!("{} dirty={}", info.status.head.label(), info.status.is_dirty()),
//!     Err(yien_git::GitError::NotARepository) => println!("no branch"),
//!     Err(err) => println!("{err}"),
//! }
//! ```

#![forbid(unsafe_code)]

mod error;
mod porcelain;
mod runner;

pub use error::{GitError, ParseError};
pub use porcelain::{AheadBehind, ChangedFile, FileStatus, Head, Status, Upstream, parse_status};
pub use runner::{DEFAULT_TIMEOUT, Git, MAX_OUTPUT};
