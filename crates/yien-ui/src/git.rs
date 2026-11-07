//! What the context bar shows about the selected tab's git state
//! (RF-ESP-04, RF-CTX-04), decided from the core's facts without drawing.

use yien_proto::{GitFacts, GitFileStatus, GitHead, TabGit};

/// The chip text when the folder has no branch to show.
pub(crate) const NO_BRANCH: &str = "no branch";

/// How a changed file is marked, which also picks its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileMark {
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    Untracked,
    Conflicted,
}

impl FileMark {
    /// The letter in the change list.
    pub(crate) fn letter(self) -> &'static str {
        match self {
            FileMark::Modified => "M",
            FileMark::Added => "A",
            FileMark::Deleted => "D",
            FileMark::Renamed => "R",
            FileMark::Copied => "C",
            FileMark::Untracked => "?",
            FileMark::Conflicted => "U",
        }
    }

    fn word(self) -> &'static str {
        match self {
            FileMark::Modified => "Modified",
            FileMark::Added => "Added",
            FileMark::Deleted => "Deleted",
            FileMark::Renamed => "Renamed",
            FileMark::Copied => "Copied",
            FileMark::Untracked => "Untracked",
            FileMark::Conflicted => "Conflicted",
        }
    }
}

/// One row of the change list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileRow {
    pub(crate) mark: FileMark,
    /// The folders before the file name, with the trailing `/`; cut first
    /// when the row is narrow, so the name stays readable.
    pub(crate) folder: String,
    pub(crate) name: String,
    /// What a screen reader says.
    pub(crate) label: String,
}

/// The context bar's git part for one tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContextGit {
    /// The chip: the branch, a short commit when detached, or [`NO_BRANCH`].
    pub(crate) head: String,
    pub(crate) detached: bool,
    pub(crate) dirty: bool,
    /// Whether the folder is a repository, so a change count means something.
    pub(crate) repository: bool,
    /// Commits ahead and behind the upstream, when there is one.
    pub(crate) ahead_behind: Option<String>,
    /// Where the work tree is: the main checkout or a linked worktree, or the
    /// folder when it is not a repository.
    pub(crate) place: String,
    /// A sentence under the header instead of, or before, the change list.
    pub(crate) note: Option<String>,
    pub(crate) files: Vec<FileRow>,
    /// Every changed file, counting those the list leaves out.
    pub(crate) changed: u32,
}

impl ContextGit {
    /// Decides what to show for a tab in `dir` whose facts are `git`, or
    /// `branch` alone while none have arrived.
    pub(crate) fn new(git: Option<&TabGit>, branch: Option<&str>, dir: &str) -> Self {
        let empty = |head: &str, note: Option<String>| Self {
            head: head.to_owned(),
            detached: false,
            dirty: false,
            repository: false,
            ahead_behind: None,
            place: dir.to_owned(),
            note,
            files: Vec::new(),
            changed: 0,
        };
        match git {
            None => empty(branch.unwrap_or(NO_BRANCH), None),
            Some(TabGit::NoRepository) => empty(
                NO_BRANCH,
                Some("This folder is not in a git repository.".to_owned()),
            ),
            Some(TabGit::Unavailable { reason }) => {
                empty(branch.unwrap_or(NO_BRANCH), Some(sentence(reason)))
            }
            Some(TabGit::Repository(facts)) => Self::of(facts),
        }
    }

    fn of(facts: &GitFacts) -> Self {
        let (head, detached) = match &facts.head {
            GitHead::Branch { name, .. } => (name.clone(), false),
            GitHead::Detached { commit } => (commit.clone(), true),
        };
        let ahead_behind = facts.upstream.as_ref().map(|u| match (u.ahead, u.behind) {
            (Some(ahead), Some(behind)) => format!("↑{ahead} ↓{behind}"),
            _ => "upstream gone".to_owned(),
        });
        let place = if facts.linked_worktree {
            let name = last_part(&facts.root);
            match &facts.main_worktree {
                Some(main) => format!("worktree {name} · main at {main}"),
                None => format!("worktree {name}"),
            }
        } else {
            "main checkout".to_owned()
        };
        let unborn = matches!(facts.head, GitHead::Branch { unborn: true, .. });
        let note = if facts.is_dirty() {
            None
        } else if unborn {
            Some("No commits yet.".to_owned())
        } else {
            Some("No changes since the last commit.".to_owned())
        };
        let files = facts
            .files
            .iter()
            .map(|f| {
                let mark = match f.status {
                    GitFileStatus::Modified => FileMark::Modified,
                    GitFileStatus::Added => FileMark::Added,
                    GitFileStatus::Deleted => FileMark::Deleted,
                    GitFileStatus::Renamed { .. } => FileMark::Renamed,
                    GitFileStatus::Copied { .. } => FileMark::Copied,
                    GitFileStatus::Untracked => FileMark::Untracked,
                    GitFileStatus::Conflicted => FileMark::Conflicted,
                };
                let label = match &f.status {
                    GitFileStatus::Renamed { from } | GitFileStatus::Copied { from } => {
                        format!("{} {} from {from}", mark.word(), f.path)
                    }
                    _ => format!("{} {}", mark.word(), f.path),
                };
                let (folder, name) = split_path(&f.path);
                FileRow {
                    mark,
                    folder: folder.to_owned(),
                    name: name.to_owned(),
                    label,
                }
            })
            .collect();
        Self {
            head,
            detached,
            dirty: facts.is_dirty(),
            repository: true,
            ahead_behind,
            place,
            note,
            files,
            changed: facts.changed,
        }
    }

    /// Changed files the list leaves out.
    pub(crate) fn hidden(&self) -> u32 {
        let shown = u32::try_from(self.files.len()).unwrap_or(u32::MAX);
        self.changed.saturating_sub(shown)
    }
}

/// A repository path split after its last `/`: the folders, then the name.
fn split_path(path: &str) -> (&str, &str) {
    let at = path.trim_end_matches('/').rfind('/').map_or(0, |i| i + 1);
    path.split_at(at)
}

/// The last part of a `/` or `\` path.
fn last_part(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['/', '\\']);
    trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed)
}

/// Starts with a capital and ends with a full stop.
fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    let mut out: String = chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default();
    if !out.ends_with('.') {
        out.push('.');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use yien_proto::{GitFile, GitUpstream};

    fn facts(head: GitHead, files: Vec<GitFile>) -> GitFacts {
        let changed = u32::try_from(files.len()).unwrap();
        GitFacts {
            head,
            upstream: None,
            root: "C:/Code/yien".into(),
            linked_worktree: false,
            main_worktree: None,
            files,
            changed,
        }
    }

    fn branch(name: &str) -> GitHead {
        GitHead::Branch {
            name: name.into(),
            unborn: false,
        }
    }

    fn file(path: &str, status: GitFileStatus) -> GitFile {
        GitFile {
            path: path.into(),
            status,
        }
    }

    #[test]
    fn rf_esp_04_no_repository_shows_no_branch_and_the_folder() {
        let view = ContextGit::new(Some(&TabGit::NoRepository), None, "C:/notes");
        assert_eq!(view.head, NO_BRANCH);
        assert_eq!(view.place, "C:/notes");
        assert!(!view.dirty);
        assert!(view.files.is_empty());
        assert!(view.note.is_some());
        assert!(!view.repository, "no change count outside a repository");
    }

    #[test]
    fn rf_ctx_04_a_row_keeps_the_file_name_apart_from_its_folders() {
        assert_eq!(
            split_path("src/components/Hero.astro"),
            ("src/components/", "Hero.astro")
        );
        assert_eq!(split_path("README.md"), ("", "README.md"));
        assert_eq!(split_path("vendor/sub/"), ("vendor/", "sub/"));
        let git = TabGit::Repository(facts(
            branch("main"),
            vec![file("crates/yien-ui/src/shell.rs", GitFileStatus::Modified)],
        ));
        let view = ContextGit::new(Some(&git), None, "C:/x");
        assert!(view.repository);
        assert_eq!(view.files[0].folder, "crates/yien-ui/src/");
        assert_eq!(view.files[0].name, "shell.rs");
    }

    #[test]
    fn rf_esp_04_before_any_facts_the_known_branch_shows() {
        let view = ContextGit::new(None, Some("main"), "C:/Code/yien");
        assert_eq!(view.head, "main");
        assert_eq!(view.note, None);
        assert_eq!(ContextGit::new(None, None, "C:/x").head, NO_BRANCH);
    }

    #[test]
    fn rf_esp_04_a_detached_head_shows_its_commit() {
        let git = TabGit::Repository(facts(
            GitHead::Detached {
                commit: "0123456".into(),
            },
            vec![],
        ));
        let view = ContextGit::new(Some(&git), None, "C:/Code/yien");
        assert_eq!(view.head, "0123456");
        assert!(view.detached);
        assert_eq!(view.place, "main checkout");
        assert_eq!(
            view.note.as_deref(),
            Some("No changes since the last commit.")
        );
    }

    #[test]
    fn rf_ctx_04_dirty_files_are_listed_with_their_marks() {
        let git = TabGit::Repository(facts(
            branch("main"),
            vec![
                file("src/a.rs", GitFileStatus::Modified),
                file(
                    "src/b.rs",
                    GitFileStatus::Renamed {
                        from: "src/old.rs".into(),
                    },
                ),
                file("notes.txt", GitFileStatus::Untracked),
                file("lib.rs", GitFileStatus::Conflicted),
            ],
        ));
        let view = ContextGit::new(Some(&git), Some("main"), "C:/Code/yien");
        assert!(view.dirty);
        assert_eq!(view.note, None);
        let letters: Vec<_> = view.files.iter().map(|f| f.mark.letter()).collect();
        assert_eq!(letters, ["M", "R", "?", "U"]);
        assert_eq!(view.files[1].label, "Renamed src/b.rs from src/old.rs");
        assert_eq!(view.changed, 4);
        assert_eq!(view.hidden(), 0);
    }

    #[test]
    fn rf_ctx_04_a_capped_list_says_how_many_it_left_out() {
        let mut f = facts(branch("main"), vec![file("a", GitFileStatus::Added)]);
        f.changed = 501;
        let view = ContextGit::new(Some(&TabGit::Repository(f)), None, "C:/x");
        assert_eq!(view.hidden(), 500);
    }

    #[test]
    fn ahead_and_behind_show_with_an_upstream_only() {
        let mut f = facts(branch("feat"), vec![]);
        let view = ContextGit::new(Some(&TabGit::Repository(f.clone())), None, "C:/x");
        assert_eq!(view.ahead_behind, None);
        f.upstream = Some(GitUpstream {
            name: "origin/feat".into(),
            ahead: Some(3),
            behind: Some(0),
        });
        let view = ContextGit::new(Some(&TabGit::Repository(f.clone())), None, "C:/x");
        assert_eq!(view.ahead_behind.as_deref(), Some("↑3 ↓0"));
        f.upstream = Some(GitUpstream {
            name: "origin/feat".into(),
            ahead: None,
            behind: None,
        });
        let view = ContextGit::new(Some(&TabGit::Repository(f)), None, "C:/x");
        assert_eq!(view.ahead_behind.as_deref(), Some("upstream gone"));
    }

    #[test]
    fn a_linked_worktree_names_itself_and_its_main_worktree() {
        let mut f = facts(branch("feat"), vec![]);
        f.root = "C:/Code/yien-wt/git-ui".into();
        f.linked_worktree = true;
        f.main_worktree = Some("C:/Code/yien".into());
        let view = ContextGit::new(Some(&TabGit::Repository(f.clone())), None, "C:/x");
        assert_eq!(view.place, "worktree git-ui · main at C:/Code/yien");
        f.main_worktree = None;
        let view = ContextGit::new(Some(&TabGit::Repository(f)), None, "C:/x");
        assert_eq!(view.place, "worktree git-ui");
    }

    #[test]
    fn git_trouble_keeps_the_known_branch_and_explains() {
        let git = TabGit::Unavailable {
            reason: "git was not found; install git or add it to PATH".into(),
        };
        let view = ContextGit::new(Some(&git), Some("main"), "C:/x");
        assert_eq!(view.head, "main");
        assert_eq!(
            view.note.as_deref(),
            Some("Git was not found; install git or add it to PATH.")
        );
    }

    #[test]
    fn an_unborn_branch_says_there_are_no_commits() {
        let git = TabGit::Repository(facts(
            GitHead::Branch {
                name: "main".into(),
                unborn: true,
            },
            vec![],
        ));
        let view = ContextGit::new(Some(&git), None, "C:/x");
        assert_eq!(view.head, "main");
        assert_eq!(view.note.as_deref(), Some("No commits yet."));
    }
}
