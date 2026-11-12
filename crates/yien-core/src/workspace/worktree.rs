//! Worktree tabs in the model (RF-ESP-05, RF-ESP-07): which git work a
//! command needs and whether a worktree may be removed. The git work itself
//! runs in `crate::worktree`, off the runtime's workers.

use yien_proto::{CoreMsg, PaneId, ProjectId, TabId, WorktreeWork, same_folder};

use super::{Effect, Workspace};
use crate::CoreError;
use crate::launch::LaunchPlan;

/// A worktree to create, then open as a new tab of `project`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AddJob {
    pub project: ProjectId,
    /// The project folder: git finds the repository from it.
    pub root: String,
    pub branch: String,
    pub base: Option<String>,
}

/// A worktree to remove once the panes that ran in it have closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoveJob {
    /// The project folder, where git runs: never inside the worktree.
    pub root: String,
    pub worktree: String,
    /// The tab's branch, which stays; only named in the notice.
    pub branch: Option<String>,
    /// The closed tab's panes, whose programs must end first.
    pub panes: Vec<PaneId>,
    pub discard_changes: bool,
    /// What the user agreed to lose; more than this keeps the worktree.
    pub confirmed: WorktreeWork,
}

impl Workspace {
    /// The job that creates a worktree for a new tab of `project`.
    pub(super) fn add_worktree(
        &self,
        project: ProjectId,
        branch: String,
        base: Option<String>,
    ) -> Result<Effect, CoreError> {
        let root = self
            .projects
            .iter()
            .find(|p| p.id == project)
            .map(|p| p.root.clone())
            .ok_or(CoreError::UnknownProject(project))?;
        Ok(Effect::AddWorktree(AddJob {
            project,
            root,
            branch: branch.trim().to_owned(),
            base: base.map(|b| b.trim().to_owned()).filter(|b| !b.is_empty()),
        }))
    }

    /// Opens the tab on a worktree that now exists, with the shell.
    pub fn open_worktree_tab(
        &mut self,
        project: ProjectId,
        worktree: String,
        branch: String,
        resolve: &dyn Fn(Option<&yien_proto::ProfileId>) -> Result<LaunchPlan, CoreError>,
    ) -> Result<Vec<Effect>, CoreError> {
        let plan = resolve(None)?;
        let mut fx = Vec::new();
        self.new_tab(project, None, plan, Some((worktree, branch)), &mut fx)?;
        self.refresh_views(&mut fx);
        debug_assert_eq!(self.check(), Ok(()));
        Ok(fx)
    }

    /// Whether a tab other than `tab` works in `folder`, as its folder or
    /// its worktree.
    pub fn folder_used_elsewhere(&self, tab: TabId, folder: &str) -> bool {
        self.tabs.iter().any(|t| {
            t.id != tab
                && (same_folder(&t.dir, folder)
                    || t.worktree
                        .as_deref()
                        .is_some_and(|w| same_folder(w, folder)))
        })
    }

    /// Closes a worktree tab and, unless another tab works in its worktree,
    /// has the worktree removed after the tab's panes. The branch stays.
    pub(super) fn remove_worktree_tab(
        &mut self,
        tab: TabId,
        discard_changes: bool,
        confirmed: WorktreeWork,
        fx: &mut Vec<Effect>,
    ) -> Result<(), CoreError> {
        let t = self.tab(tab)?;
        let worktree = t.worktree.clone().ok_or(CoreError::NoWorktree(tab))?;
        let root = self
            .projects
            .iter()
            .find(|p| p.id == t.project)
            .map(|p| p.root.clone())
            .ok_or(CoreError::UnknownProject(t.project))?;
        let job = RemoveJob {
            root,
            branch: t.branch.clone(),
            panes: t.panes(),
            discard_changes,
            confirmed,
            worktree,
        };
        let shared = self.folder_used_elsewhere(tab, &job.worktree);
        self.close_tab(tab, fx)?;
        if shared {
            fx.push(Effect::Emit(CoreMsg::Notice {
                message: format!(
                    "{} is still open in another tab, so it was kept.",
                    job.worktree
                ),
            }));
        } else {
            fx.push(Effect::RemoveWorktree(job));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use yien_proto::{Command, Orientation, ProfileId};

    use super::*;
    use crate::launch;
    use crate::shell::ShellCommand;

    fn resolve(profile: Option<&ProfileId>) -> Result<LaunchPlan, CoreError> {
        launch::resolve(profile, &[], &ShellCommand::new("sh"))
    }

    fn project(ws: &mut Workspace) -> ProjectId {
        let new = Command::NewProject {
            root: "C:/Code/app".into(),
            name: None,
        };
        ws.apply(new, &resolve).unwrap();
        ws.snapshot().projects[0].id
    }

    fn worktree_tab(ws: &mut Workspace, project: ProjectId, dir: &str) -> TabId {
        let fx = ws
            .open_worktree_tab(project, dir.into(), "feat/x".into(), &resolve)
            .unwrap();
        assert!(
            fx.iter()
                .any(|e| matches!(e, Effect::Spawn { cwd, .. } if cwd == dir))
        );
        ws.active_dir().unwrap().0
    }

    #[test]
    fn rf_esp_05_a_new_worktree_tab_asks_for_the_worktree_first() {
        let mut ws = Workspace::new();
        let project = project(&mut ws);
        let before = ws.tab_count();
        let command = Command::NewWorktreeTab {
            project,
            branch: " feat/x ".into(),
            base: Some(" ".into()),
        };
        let fx = ws.apply(command, &resolve).unwrap();
        assert!(matches!(
            fx.as_slice(),
            [Effect::AddWorktree(AddJob { root, branch, base: None, .. })]
                if root == "C:/Code/app" && branch == "feat/x"
        ));
        assert_eq!(ws.tab_count(), before, "no tab before the worktree exists");

        let unknown = Command::NewWorktreeTab {
            project: ProjectId::new(99),
            branch: "x".into(),
            base: None,
        };
        assert!(matches!(
            ws.apply(unknown, &resolve),
            Err(CoreError::UnknownProject(_))
        ));
    }

    #[test]
    fn rf_esp_05_the_worktree_tab_works_in_its_worktree() {
        let mut ws = Workspace::new();
        let project = project(&mut ws);
        let tab = worktree_tab(&mut ws, project, "C:/Code/app-wt/feat-x");
        let info = ws
            .snapshot()
            .tabs
            .into_iter()
            .find(|t| t.id == tab)
            .unwrap();
        assert_eq!(info.dir, "C:/Code/app-wt/feat-x");
        assert_eq!(info.worktree.as_deref(), Some("C:/Code/app-wt/feat-x"));
        assert_eq!(info.branch.as_deref(), Some("feat/x"));
        assert_eq!(info.name, "feat/x");

        let pane = info.focused;
        let split = Command::SplitPane {
            pane,
            orientation: Orientation::Horizontal,
            profile: None,
        };
        let fx = ws.apply(split, &resolve).unwrap();
        assert!(
            fx.iter()
                .any(|e| matches!(e, Effect::Spawn { cwd, .. } if cwd == "C:/Code/app-wt/feat-x"))
        );
    }

    #[test]
    fn rf_esp_07_removing_closes_the_tab_then_removes_after_its_panes() {
        let mut ws = Workspace::new();
        let project = project(&mut ws);
        let tab = worktree_tab(&mut ws, project, "C:/Code/app-wt/feat-x");
        let panes = ws.tab(tab).unwrap().panes();
        let command = Command::RemoveWorktreeTab {
            tab,
            discard_changes: false,
            confirmed: WorktreeWork::default(),
        };
        let fx = ws.apply(command, &resolve).unwrap();
        let removal = fx
            .iter()
            .position(|e| matches!(e, Effect::RemoveWorktree(_)))
            .unwrap();
        let last_stop = fx
            .iter()
            .rposition(|e| matches!(e, Effect::Stop { .. }))
            .unwrap();
        assert!(last_stop < removal, "the panes stop before the removal");
        let Effect::RemoveWorktree(job) = &fx[removal] else {
            unreachable!()
        };
        assert_eq!(job.root, "C:/Code/app");
        assert_eq!(job.worktree, "C:/Code/app-wt/feat-x");
        assert_eq!(job.branch.as_deref(), Some("feat/x"));
        assert_eq!(job.panes, panes);
        assert!(!job.discard_changes);
        assert!(!ws.has_tab(tab));
    }

    #[test]
    fn rf_esp_07_removing_a_split_worktree_tab_stops_every_pane_first() {
        let mut ws = Workspace::new();
        let project = project(&mut ws);
        let tab = worktree_tab(&mut ws, project, "C:/Code/app-wt/feat-x");
        let split = Command::SplitPane {
            pane: ws.tab(tab).unwrap().panes()[0],
            orientation: Orientation::Horizontal,
            profile: None,
        };
        ws.apply(split, &resolve).unwrap();
        let panes = ws.tab(tab).unwrap().panes();
        assert_eq!(panes.len(), 2);

        let command = Command::RemoveWorktreeTab {
            tab,
            discard_changes: false,
            confirmed: WorktreeWork::default(),
        };
        let fx = ws.apply(command, &resolve).unwrap();
        let removal = fx
            .iter()
            .position(|e| matches!(e, Effect::RemoveWorktree(_)))
            .unwrap();
        let Effect::RemoveWorktree(job) = &fx[removal] else {
            unreachable!()
        };
        assert_eq!(job.panes, panes);
        for pane in &panes {
            let stop = fx
                .iter()
                .position(|e| matches!(e, Effect::Stop { pane: p } if p == pane))
                .unwrap_or_else(|| panic!("pane {pane:?} is never stopped"));
            assert!(stop < removal, "pane {pane:?} stops before the removal");
        }
        assert!(!ws.has_tab(tab));
    }

    #[test]
    fn rf_esp_07_a_worktree_another_tab_uses_is_kept() {
        let mut ws = Workspace::new();
        let project = project(&mut ws);
        let first = worktree_tab(&mut ws, project, "C:/Code/app-wt/feat-x");
        // Folders compare without case and with either slash on Windows only.
        let same = if cfg!(windows) {
            "c:\\code\\app-wt\\feat-x\\"
        } else {
            "C:/Code/app-wt/feat-x/"
        };
        let second = worktree_tab(&mut ws, project, same);
        assert!(ws.folder_used_elsewhere(first, "C:/Code/app-wt/feat-x"));
        assert!(!ws.folder_used_elsewhere(first, "C:/Code/app-wt/other"));

        let command = Command::RemoveWorktreeTab {
            tab: first,
            discard_changes: true,
            confirmed: WorktreeWork::default(),
        };
        let fx = ws.apply(command, &resolve).unwrap();
        assert!(!fx.iter().any(|e| matches!(e, Effect::RemoveWorktree(_))));
        assert!(
            fx.iter()
                .any(|e| matches!(e, Effect::Emit(CoreMsg::Notice { .. })))
        );
        assert!(!ws.has_tab(first));

        // The last tab on it may remove it.
        let command = Command::RemoveWorktreeTab {
            tab: second,
            discard_changes: false,
            confirmed: WorktreeWork::default(),
        };
        let fx = ws.apply(command, &resolve).unwrap();
        assert!(fx.iter().any(|e| matches!(e, Effect::RemoveWorktree(_))));
    }

    #[test]
    fn rn_08_only_a_worktree_tab_can_be_checked_or_removed() {
        let mut ws = Workspace::new();
        project(&mut ws);
        let plain = ws.active_dir().unwrap().0;
        for command in [
            Command::CheckWorktree { tab: plain },
            Command::RemoveWorktreeTab {
                tab: plain,
                discard_changes: true,
                confirmed: WorktreeWork::default(),
            },
        ] {
            assert!(matches!(
                ws.apply(command, &resolve),
                Err(CoreError::NoWorktree(_))
            ));
        }
        assert!(ws.has_tab(plain), "a failed removal closes nothing");
    }

    #[test]
    fn rn_08_checking_names_the_worktree() {
        let mut ws = Workspace::new();
        let project = project(&mut ws);
        let tab = worktree_tab(&mut ws, project, "C:/Code/app-wt/feat-x");
        let fx = ws.apply(Command::CheckWorktree { tab }, &resolve).unwrap();
        assert!(matches!(
            fx.as_slice(),
            [Effect::CheckWorktree { tab: t, worktree }]
                if *t == tab && worktree == "C:/Code/app-wt/feat-x"
        ));
    }
}
