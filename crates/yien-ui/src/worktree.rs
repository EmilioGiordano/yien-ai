//! Worktree tabs in the window (RF-ESP-05, RF-ESP-07): what the "new
//! worktree tab" prompt accepts, and what the question asked before closing
//! a worktree tab says. Pure, so it is tested without a window.

use yien_proto::{Command, TabId, WorktreeCheck, WorktreeWork};

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
    check: Option<WorktreeCheck>,
    /// What the second step shows and confirms: the check as it was when
    /// that step opened.
    shown: Option<WorktreeCheck>,
    pub step: Step,
}

/// What an answer to the question does.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Answer {
    /// Send this and close the question.
    Run(Command),
    /// Ask again, at the second step.
    ConfirmFirst,
    /// Nothing yet: git is still looking.
    Wait,
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
            shown: None,
            step: Step::Choose,
        }
    }

    /// Takes what git found. A check that changes what the second step
    /// showed sends the question back to the first: the user confirmed
    /// something else.
    pub(crate) fn set_check(&mut self, check: WorktreeCheck) {
        if self.step == Step::ConfirmRemove && self.shown.as_ref() != Some(&check) {
            self.step = Step::Choose;
            self.shown = None;
        }
        self.check = Some(check);
    }

    /// Whether git has answered.
    pub(crate) fn checked(&self) -> bool {
        self.check.is_some()
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
            Step::ConfirmRemove => format!("{} {branch}", work_text(self.shown.as_ref())),
        }
    }

    /// What git found, in words.
    pub(crate) fn work_text(&self) -> String {
        work_text(self.check.as_ref())
    }

    /// Whether removing needs the second confirmation: there is work, or it
    /// could not be checked.
    pub(crate) fn needs_confirm(&self) -> bool {
        match &self.check {
            Some(WorktreeCheck::Checked { work, .. }) => work.any(),
            _ => true,
        }
    }

    /// Keep: the tab closes and the worktree stays on disk.
    pub(crate) fn keep(&self) -> Command {
        Command::CloseTab { tab: self.tab }
    }

    /// Remove, at the first step or the second. Nothing happens while git
    /// is still looking.
    pub(crate) fn remove(&mut self) -> Answer {
        let Some(check) = &self.check else {
            return Answer::Wait;
        };
        let seen = match self.step {
            Step::Choose if self.needs_confirm() => {
                self.shown = Some(check.clone());
                self.step = Step::ConfirmRemove;
                return Answer::ConfirmFirst;
            }
            Step::Choose => check,
            Step::ConfirmRemove => match &self.shown {
                Some(shown) => shown,
                None => return Answer::Wait,
            },
        };
        // Only what the user saw counted is confirmed: changes are discarded
        // only then, and the core keeps a worktree that has more by now. A
        // failed check confirms nothing, so git keeps any work.
        let confirmed = match seen {
            WorktreeCheck::Checked { work, .. } => *work,
            WorktreeCheck::Failed { .. } => WorktreeWork::default(),
        };
        Answer::Run(Command::RemoveWorktreeTab {
            tab: self.tab,
            discard_changes: confirmed.changed > 0 || confirmed.ignored > 0,
            confirmed,
        })
    }
}

fn work_text(check: Option<&WorktreeCheck>) -> String {
    match check {
        None => "Checking it for uncommitted changes…".to_owned(),
        Some(WorktreeCheck::Failed { reason }) => {
            format!("It could not be checked ({reason}); if it has uncommitted changes it is kept.")
        }
        Some(WorktreeCheck::Checked {
            work,
            ignored_examples,
            detached,
        }) => {
            if !work.any() {
                return "No uncommitted changes, ignored files or unpushed commits.".to_owned();
            }
            let mut parts = Vec::new();
            if work.changed > 0 {
                parts.push(format!(
                    "{} will be deleted.",
                    count(work.changed, "uncommitted change")
                ));
            }
            if work.ignored > 0 {
                let such_as = if ignored_examples.is_empty() {
                    String::new()
                } else {
                    format!(", such as {},", ignored_examples.join(", "))
                };
                parts.push(format!(
                    "{}{such_as} will be deleted.",
                    count(work.ignored, "ignored file")
                ));
            }
            if work.unpushed > 0 {
                let n = count(work.unpushed, "commit");
                parts.push(match (*detached, work.unpushed) {
                    (true, 1) => format!("{n} is on no branch and will be lost."),
                    (true, _) => format!("{n} are on no branch and will be lost."),
                    (false, 1) => format!("{n} is not pushed; the branch keeps it."),
                    (false, _) => format!("{n} are not pushed; the branch keeps them."),
                });
            }
            parts.join(" ")
        }
    }
}

fn count(n: u32, what: &str) -> String {
    if n == 1 {
        format!("1 {what}")
    } else {
        format!("{n} {what}s")
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
        if let Some(check) = check {
            q.set_check(check);
        }
        q
    }

    fn work(changed: u32, ignored: u32, unpushed: u32) -> WorktreeWork {
        WorktreeWork {
            changed,
            ignored,
            unpushed,
        }
    }

    fn checked(changed: u32, ignored: u32, unpushed: u32) -> WorktreeCheck {
        WorktreeCheck::Checked {
            work: work(changed, ignored, unpushed),
            ignored_examples: Vec::new(),
            detached: false,
        }
    }

    fn removes(discard_changes: bool, confirmed: WorktreeWork) -> Answer {
        Answer::Run(Command::RemoveWorktreeTab {
            tab: TabId::new(4),
            discard_changes,
            confirmed,
        })
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
        let q = question(Some(checked(3, 0, 1)));
        assert_eq!(q.keep(), Command::CloseTab { tab: TabId::new(4) });
        assert!(q.detail().contains("C:/Code/app-wt/feat-x"));
        assert!(q.detail().contains("branch feat/x stays"));
        assert_eq!(q.title(), "Close feat/x?");
    }

    #[test]
    fn rf_esp_07_a_clean_worktree_is_removed_at_once() {
        let mut q = question(Some(checked(0, 0, 0)));
        assert!(!q.needs_confirm());
        assert_eq!(q.remove(), removes(false, work(0, 0, 0)));
        assert_eq!(
            q.work_text(),
            "No uncommitted changes, ignored files or unpushed commits."
        );
    }

    #[test]
    fn rn_08_nothing_is_removed_while_the_check_is_pending() {
        let mut q = question(None);
        assert!(!q.checked());
        assert_eq!(q.remove(), Answer::Wait);
        assert_eq!(q.step, Step::Choose);
        assert_eq!(q.work_text(), "Checking it for uncommitted changes…");
    }

    #[test]
    fn rn_08_work_in_the_worktree_needs_a_second_confirm_that_names_it() {
        for (counts, discard) in [
            ((2, 0, 0), true),
            ((0, 4, 0), true),
            ((0, 0, 1), false),
            ((1, 2, 3), true),
        ] {
            let (c, i, u) = counts;
            let mut q = question(Some(checked(c, i, u)));
            assert_eq!(q.remove(), Answer::ConfirmFirst);
            assert_eq!(q.step, Step::ConfirmRemove);
            assert!(q.title().starts_with("Remove"));
            assert_eq!(q.remove(), removes(discard, work(c, i, u)));
        }
        assert_eq!(
            question(Some(checked(2, 0, 3))).work_text(),
            "2 uncommitted changes will be deleted. 3 commits are not pushed; the branch keeps them."
        );
        assert_eq!(
            question(Some(checked(1, 0, 0))).work_text(),
            "1 uncommitted change will be deleted."
        );
        assert_eq!(
            question(Some(checked(0, 0, 1))).work_text(),
            "1 commit is not pushed; the branch keeps it."
        );
    }

    #[test]
    fn rn_08_ignored_files_are_named() {
        let q = question(Some(WorktreeCheck::Checked {
            work: work(0, 3, 0),
            ignored_examples: vec![".env".into(), "target/".into()],
            detached: false,
        }));
        assert!(q.needs_confirm());
        assert_eq!(
            q.work_text(),
            "3 ignored files, such as .env, target/, will be deleted."
        );
    }

    #[test]
    fn rn_08_commits_on_a_detached_head_are_said_to_be_lost() {
        let q = question(Some(WorktreeCheck::Checked {
            work: work(0, 0, 2),
            ignored_examples: Vec::new(),
            detached: true,
        }));
        assert_eq!(
            q.work_text(),
            "2 commits are on no branch and will be lost."
        );
    }

    #[test]
    fn rn_08_a_check_that_changes_during_the_confirm_goes_back_to_the_start() {
        let mut q = question(Some(checked(1, 0, 0)));
        assert_eq!(q.remove(), Answer::ConfirmFirst);
        // The same answer again changes nothing the user saw.
        q.set_check(checked(1, 0, 0));
        assert_eq!(q.step, Step::ConfirmRemove);
        // More work arrived: the confirm no longer applies.
        q.set_check(checked(5, 0, 0));
        assert_eq!(q.step, Step::Choose);
        assert_eq!(q.remove(), Answer::ConfirmFirst);
        assert_eq!(q.remove(), removes(true, work(5, 0, 0)));
    }

    #[test]
    fn rn_08_the_confirm_sends_what_it_showed() {
        let mut q = question(Some(checked(1, 0, 0)));
        assert_eq!(q.remove(), Answer::ConfirmFirst);
        assert!(
            q.detail()
                .starts_with("1 uncommitted change will be deleted.")
        );
        assert_eq!(q.remove(), removes(true, work(1, 0, 0)));
    }

    #[test]
    fn rn_08_a_failed_check_confirms_no_work() {
        let mut q = question(Some(WorktreeCheck::Failed {
            reason: "git was not found".into(),
        }));
        assert!(q.needs_confirm());
        assert_eq!(q.remove(), Answer::ConfirmFirst);
        assert_eq!(q.remove(), removes(false, work(0, 0, 0)));
    }
}
