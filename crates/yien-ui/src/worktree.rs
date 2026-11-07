//! Worktree tabs in the window (RF-ESP-05, RF-ESP-07): what the "new
//! worktree tab" prompt accepts, and what the question asked before closing
//! a worktree tab says. Pure, so it is tested without a window.

use yien_proto::{Command, TabId, WorktreeCheck};

/// Reads the prompt: a branch name, then optionally a base to start from,
/// separated by spaces. Branch names cannot hold spaces, so this is never
/// ambiguous.
pub(crate) fn parse_prompt(text: &str) -> Result<(String, Option<String>), &'static str> {
    let mut words = text.split_whitespace();
    let Some(branch) = words.next() else {
        return Err("Type a name for the new branch.");
    };
    let base = words.next();
    if words.next().is_some() {
        return Err("Type the branch name, then optionally where it starts, and nothing more.");
    }
    Ok((branch.to_owned(), base.map(str::to_owned)))
}

/// The hint under the prompt: where the branch starts when no base is typed.
pub(crate) fn prompt_hint(current: Option<&str>) -> String {
    let from = current.map_or_else(|| "the project's current commit".to_owned(), str::to_owned);
    format!(
        "Starts from {from}. Type a second word to start from another branch, tag or commit. \
         The worktree goes beside the repository, in <repo>-wt."
    )
}

/// Where the question before closing a worktree tab is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// Keep, remove or cancel.
    Choose,
    /// Remove was chosen for a worktree with work in it: confirm again (RN-08).
    ConfirmRemove,
}

/// The question asked before closing the last tab on a worktree (RF-ESP-07).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Question {
    pub tab: TabId,
    pub tab_name: String,
    pub worktree: String,
    pub branch: Option<String>,
    /// What git found; `None` while it is looking.
    pub check: Option<WorktreeCheck>,
    pub step: Step,
}

/// What an answer to the question does.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Answer {
    /// Send this and close the question.
    Run(Command),
    /// Ask again, at the second step.
    ConfirmFirst,
}

impl Question {
    pub(crate) fn new(
        tab: TabId,
        tab_name: String,
        worktree: String,
        branch: Option<String>,
    ) -> Self {
        Self {
            tab,
            tab_name,
            worktree,
            branch,
            check: None,
            step: Step::Choose,
        }
    }

    /// The question's title.
    pub(crate) fn title(&self) -> String {
        match self.step {
            Step::Choose => format!("Close {}?", self.tab_name),
            Step::ConfirmRemove => format!("Remove the worktree of {}?", self.tab_name),
        }
    }

    /// What the question explains: where the worktree is, that the branch
    /// stays, and what git found.
    pub(crate) fn detail(&self) -> String {
        let branch = self.branch.as_deref().map_or_else(
            || "Its branch stays.".to_owned(),
            |b| format!("Its branch {b} stays either way."),
        );
        match self.step {
            Step::Choose => format!(
                "This tab works in the worktree {}. Keep the folder, or remove it? {branch}",
                self.worktree
            ),
            Step::ConfirmRemove => format!("{} {branch}", self.work_text()),
        }
    }

    /// What git found, in words.
    pub(crate) fn work_text(&self) -> String {
        match &self.check {
            None => "Checking it for uncommitted changes…".to_owned(),
            Some(WorktreeCheck::Failed { reason }) => format!(
                "It could not be checked ({reason}); if it has uncommitted changes it is kept."
            ),
            Some(WorktreeCheck::Checked { changed, unpushed }) => match (*changed, *unpushed) {
                (0, 0) => "No uncommitted changes and nothing unpushed.".to_owned(),
                (c, 0) => format!("{} will be deleted.", files(c)),
                (0, u) => commits(u),
                (c, u) => format!("{} will be deleted. {}", files(c), commits(u)),
            },
        }
    }

    /// Whether removing needs the second confirmation: there is work, or it
    /// is not known yet whether there is.
    pub(crate) fn needs_confirm(&self) -> bool {
        match &self.check {
            Some(WorktreeCheck::Checked { changed, unpushed }) => *changed > 0 || *unpushed > 0,
            _ => true,
        }
    }

    /// Keep: the tab closes and the worktree stays on disk.
    pub(crate) fn keep(&self) -> Command {
        Command::CloseTab { tab: self.tab }
    }

    /// Remove, at the first step or the second.
    pub(crate) fn remove(&self) -> Answer {
        if self.step == Step::Choose && self.needs_confirm() {
            return Answer::ConfirmFirst;
        }
        // Changes are discarded only when git counted them and the user saw
        // the count; otherwise git keeps a worktree that has any.
        let discard_changes = matches!(
            self.check,
            Some(WorktreeCheck::Checked { changed, .. }) if changed > 0
        );
        Answer::Run(Command::RemoveWorktreeTab {
            tab: self.tab,
            discard_changes,
        })
    }
}

fn files(n: u32) -> String {
    if n == 1 {
        "1 uncommitted change".to_owned()
    } else {
        format!("{n} uncommitted changes")
    }
}

fn commits(n: u32) -> String {
    if n == 1 {
        "1 commit is not pushed; the branch keeps it.".to_owned()
    } else {
        format!("{n} commits are not pushed; the branch keeps them.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question(check: Option<WorktreeCheck>) -> Question {
        let mut q = Question::new(
            TabId::new(4),
            "feat/x".into(),
            "C:/Code/app-wt/feat-x".into(),
            Some("feat/x".into()),
        );
        q.check = check;
        q
    }

    fn checked(changed: u32, unpushed: u32) -> Option<WorktreeCheck> {
        Some(WorktreeCheck::Checked { changed, unpushed })
    }

    #[test]
    fn rf_esp_05_the_prompt_takes_a_branch_and_an_optional_base() {
        assert_eq!(parse_prompt("feat/x"), Ok(("feat/x".into(), None)));
        assert_eq!(
            parse_prompt("  feat/x \t main "),
            Ok(("feat/x".into(), Some("main".into())))
        );
        assert!(parse_prompt("   ").is_err());
        assert!(parse_prompt("feat x y").is_err());
        assert!(prompt_hint(Some("main")).contains("Starts from main."));
        assert!(prompt_hint(None).contains("current commit"));
    }

    #[test]
    fn rf_esp_07_keep_closes_the_tab_and_leaves_the_worktree() {
        let q = question(checked(3, 1));
        assert_eq!(q.keep(), Command::CloseTab { tab: TabId::new(4) });
        assert!(q.detail().contains("C:/Code/app-wt/feat-x"));
        assert!(q.detail().contains("branch feat/x stays"));
        assert_eq!(q.title(), "Close feat/x?");
    }

    #[test]
    fn rf_esp_07_a_clean_worktree_is_removed_at_once() {
        let q = question(checked(0, 0));
        assert!(!q.needs_confirm());
        assert_eq!(
            q.remove(),
            Answer::Run(Command::RemoveWorktreeTab {
                tab: TabId::new(4),
                discard_changes: false
            })
        );
        assert_eq!(
            q.work_text(),
            "No uncommitted changes and nothing unpushed."
        );
    }

    #[test]
    fn rn_08_work_in_the_worktree_needs_a_second_confirm_that_names_it() {
        for (check, discard) in [
            (checked(2, 0), true),
            (checked(0, 1), false),
            (checked(1, 3), true),
        ] {
            let mut q = question(check);
            assert_eq!(q.remove(), Answer::ConfirmFirst);
            q.step = Step::ConfirmRemove;
            assert!(q.title().starts_with("Remove"));
            assert_eq!(
                q.remove(),
                Answer::Run(Command::RemoveWorktreeTab {
                    tab: TabId::new(4),
                    discard_changes: discard
                })
            );
        }
        let q = question(checked(2, 3));
        assert_eq!(
            q.work_text(),
            "2 uncommitted changes will be deleted. 3 commits are not pushed; the branch keeps them."
        );
        assert_eq!(
            question(checked(1, 0)).work_text(),
            "1 uncommitted change will be deleted."
        );
        assert_eq!(
            question(checked(0, 1)).work_text(),
            "1 commit is not pushed; the branch keeps it."
        );
    }

    #[test]
    fn rn_08_an_unknown_state_never_discards_changes() {
        for check in [
            None,
            Some(WorktreeCheck::Failed {
                reason: "git was not found".into(),
            }),
        ] {
            let mut q = question(check);
            assert!(q.needs_confirm());
            assert_eq!(q.remove(), Answer::ConfirmFirst);
            q.step = Step::ConfirmRemove;
            assert_eq!(
                q.remove(),
                Answer::Run(Command::RemoveWorktreeTab {
                    tab: TabId::new(4),
                    discard_changes: false
                })
            );
        }
    }
}
