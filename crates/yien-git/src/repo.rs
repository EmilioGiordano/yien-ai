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
    /// Content filters (`filter.<name>.clean` and `.process`) defined by the
    /// repository's own config are switched off for the call, since a cloned or
    /// planted `.git` must not run programs. Filters from the user's global and
    /// system config, such as git-lfs, still run. This costs one extra `git config` call.
    ///
    /// ```no_run
    /// let status = yien_git::Git::new().status(std::path::Path::new("."))?;
    /// println!("{} dirty={}", status.head.label(), status.is_dirty());
    /// # Ok::<(), yien_git::GitError>(())
    /// ```
    pub fn status(&self, dir: &Path) -> Result<Status, GitError> {
        let overrides = self.repo_filter_overrides(dir)?;
        let out = self.run_with(
            dir,
            &overrides,
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

    /// `-c` settings that switch off every filter the repository's own config
    /// defines, smudge included for the checkout of a new worktree.
    pub(crate) fn repo_filter_overrides(&self, dir: &Path) -> Result<Vec<String>, GitError> {
        let listing = self.run(
            dir,
            &[
                "config",
                "-z",
                "--show-scope",
                "--name-only",
                "--get-regexp",
                r"^filter\..*\.(clean|smudge|process|required)$",
            ],
        );
        match listing {
            Ok(out) => filter_overrides(&out),
            // Exit code 1: no key matched.
            Err(GitError::Failed { code: Some(1), .. }) => Ok(Vec::new()),
            Err(err) => Err(err),
        }
    }

    /// [`Git::discover`] and [`Git::status`] together: three or four git calls.
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
        let out = self.list_worktrees(root).ok()?;
        main_worktree_from_list(&out)
    }
}

/// Config scopes whose settings the user wrote. `--show-scope` labels an
/// included file with the scope of the file that includes it, so a repo file
/// pulled in by `.git/config` still reads as `local`.
const TRUSTED_SCOPES: &[&str] = &["global", "system", "command"];

/// Builds the `-c` overrides from `git config -z --show-scope --name-only`
/// output, which alternates scope and key, each NUL-terminated.
fn filter_overrides(out: &[u8]) -> Result<Vec<String>, GitError> {
    let mut fields = out.split(|&b| b == 0);
    let mut overrides = Vec::new();
    while let Some(scope) = fields.next() {
        if scope.is_empty() {
            continue;
        }
        let key = fields
            .next()
            .filter(|k| !k.is_empty())
            .ok_or(ParseError::ConfigListing)?;
        if TRUSTED_SCOPES.contains(&String::from_utf8_lossy(scope).as_ref()) {
            continue;
        }
        // `-c` splits at the first '=', and a lossy key would name another filter.
        let Ok(key) = std::str::from_utf8(key) else {
            return Err(GitError::UnsafeFilter {
                key: String::from_utf8_lossy(key).into_owned(),
            });
        };
        if key.contains('=') {
            return Err(GitError::UnsafeFilter {
                key: key.to_owned(),
            });
        }
        let value = if key.ends_with(".required") {
            "false"
        } else {
            ""
        };
        overrides.push(format!("{key}={value}"));
    }
    Ok(overrides)
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
    fn repo_filters_are_emptied_and_user_filters_kept() {
        let out = b"local\0filter.evil.clean\0global\0filter.lfs.process\0\
                    worktree\0filter.my evil.process\0local\0filter.evil.required\0\
                    system\0filter.sys.clean\0command\0filter.cmd.clean\0\
                    unknown\0filter.u.clean\0";
        assert_eq!(
            filter_overrides(out).unwrap(),
            vec![
                "filter.evil.clean=",
                "filter.my evil.process=",
                "filter.evil.required=false",
                "filter.u.clean=",
            ]
        );
    }

    #[test]
    fn filter_key_that_cannot_be_overridden_is_refused() {
        let err = filter_overrides(b"local\0filter.a=b.clean\0").unwrap_err();
        assert!(matches!(err, GitError::UnsafeFilter { ref key } if key == "filter.a=b.clean"));
        let err = filter_overrides(b"local\0filter.\xff.clean\0").unwrap_err();
        assert!(matches!(err, GitError::UnsafeFilter { .. }));
        assert!(filter_overrides(b"global\0filter.a=b.clean\0").is_ok());
    }

    #[test]
    fn malformed_config_listing_is_an_error() {
        assert!(filter_overrides(b"local\0").is_err());
        assert!(filter_overrides(b"local\0\0").is_err());
        assert_eq!(filter_overrides(b"").unwrap(), Vec::<String>::new());
    }

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
