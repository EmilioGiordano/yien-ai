//! The workspace as the saved session (RF-SES-01) and back (RF-SES-02).

use std::collections::{BTreeSet, HashMap, HashSet};

use yien_layout::{Shape, Tree};
use yien_proto::{AgentState, PaneId, ProjectId, TabId};
use yien_session::{
    AgentRecord, LayoutRecord, PaneRecord, ProjectRecord, ResumeAction, Selection, SessionRef,
    SessionSnapshot, TabRecord, resume_plan,
};

use super::{Effect, Pane, Project, Tab, Workspace};
use crate::CoreError;
use crate::launch::{LaunchPlan, SHELL_PROFILE};

/// The saved form of a pane that a restore launched differently.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Kept {
    /// The saved profile, which may not exist in this run.
    pub profile: String,
    /// The saved folder, which may be missing in this run.
    pub cwd: String,
    /// The saved agent session, not resumed in this run.
    pub agent: Option<AgentRecord>,
}

/// What a restore needs to know about the disk, gathered off the core's
/// runtime before it starts.
#[derive(Debug, Clone, Default)]
pub(crate) struct Disk {
    /// The folders named in the snapshot that exist.
    pub existing: HashSet<String>,
    /// The user's home folder, the last fallback for a missing folder.
    pub home: Option<String>,
}

impl Disk {
    /// The first of `candidates` that exists, then home; the first candidate
    /// when none does, so the pane still says what it was asked to open.
    fn pick(&self, candidates: &[&str]) -> String {
        candidates
            .iter()
            .copied()
            .find(|dir| self.existing.contains(*dir))
            .map(str::to_owned)
            .or_else(|| self.home.clone())
            .unwrap_or_else(|| candidates.first().copied().unwrap_or_default().to_owned())
    }
}

/// Every folder a restore of `snapshot` may check: project roots, tab folders
/// and pane folders.
pub(crate) fn folders(snapshot: &SessionSnapshot) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for project in &snapshot.projects {
        out.insert(project.root.clone());
        for tab in &project.tabs {
            out.insert(tab.dir.clone());
            out.extend(tab.panes.iter().map(|p| p.cwd.clone()));
        }
    }
    out
}

impl Workspace {
    /// The saved form of the workspace; `saved_at` is left at 0 for the writer
    /// to stamp.
    pub fn to_session(&self) -> SessionSnapshot {
        let projects = self
            .projects
            .iter()
            .map(|p| ProjectRecord {
                id: p.id,
                name: p.name.clone(),
                root: p.root.clone(),
                setup_script: None,
                collapsed: p.collapsed,
                tabs: p
                    .tabs
                    .iter()
                    .filter_map(|t| self.tab(*t).ok())
                    .map(|t| self.tab_record(t))
                    .collect(),
            })
            .collect();
        let mut snapshot = SessionSnapshot::new(0, projects);
        snapshot.selected = Selection {
            project: self
                .active_tab
                .and_then(|t| self.tab(t).ok())
                .map(|t| t.project),
            tab: self.active_tab,
        };
        snapshot.notifications_muted = self.notifications_muted;
        snapshot
    }

    fn tab_record(&self, tab: &Tab) -> TabRecord {
        let layout = layout_record(&tab.tree.shape(), &|leaf| tab.leaves.get(&leaf).copied())
            .unwrap_or(LayoutRecord::Leaf { pane: tab.focused });
        let panes = tab
            .panes()
            .into_iter()
            .filter_map(|id| self.panes.get(&id).map(|p| pane_record(id, p)))
            .collect();
        TabRecord {
            id: tab.id,
            title: tab.default_name.clone(),
            dir: tab.kept_dir.clone().unwrap_or_else(|| tab.dir.clone()),
            branch: match tab.kept_dir {
                Some(_) => tab.kept_branch.clone(),
                None => tab.branch.clone(),
            },
            worktree: None,
            archived: false,
            muted: tab.muted,
            name: tab.custom_name.clone(),
            layout,
            panes,
            focused: tab.focused,
        }
    }

    /// Rebuilds an empty workspace from a saved session, with the same ids,
    /// layouts, ratios, focus and selection. Returns the panes to start, each
    /// resuming its agent session when [`resume_plan`] says so, and notices
    /// for the user: missing folders, unknown profiles, tabs that could not be
    /// rebuilt.
    pub fn restore(
        &mut self,
        snapshot: &SessionSnapshot,
        resolve: &dyn Fn(Option<&yien_proto::ProfileId>) -> Result<LaunchPlan, CoreError>,
        disk: &Disk,
    ) -> (Vec<Effect>, Vec<String>) {
        let mut fx = Vec::new();
        let mut notes = Notes::default();
        for project in &snapshot.projects {
            self.bump(project.id.get());
            let mut tabs = Vec::new();
            for record in &project.tabs {
                self.bump(record.id.get());
                for pane in &record.panes {
                    self.bump(pane.id.get());
                }
                if self.restore_tab(project, record, resolve, disk, &mut fx, &mut notes) {
                    tabs.push(record.id);
                } else {
                    notes
                        .broken
                        .push(record.name.clone().unwrap_or_else(|| record.title.clone()));
                }
            }
            self.projects.push(Project {
                id: project.id,
                name: project.name.clone(),
                root: project.root.clone(),
                tabs,
                collapsed: project.collapsed,
            });
        }
        // Ids grow as tabs open, so this is the order they were opened in.
        self.tabs.sort_by_key(|t| t.id.get());
        let exists = |t: &TabId| self.tabs.iter().any(|tab| tab.id == *t);
        let first_of = |p: ProjectId| {
            self.projects
                .iter()
                .find(|project| project.id == p)
                .and_then(|project| project.tabs.first().copied())
        };
        self.notifications_muted = snapshot.notifications_muted;
        self.active_tab = snapshot
            .selected
            .tab
            .filter(exists)
            .or_else(|| snapshot.selected.project.and_then(first_of))
            .or_else(|| self.tabs.first().map(|t| t.id));
        self.refresh_views(&mut fx);
        debug_assert_eq!(self.check(), Ok(()));
        (fx, notes.into_messages())
    }

    fn bump(&mut self, id: u64) {
        self.next_id = self.next_id.max(id);
    }

    /// Restores one tab, or nothing when its layout cannot be rebuilt.
    fn restore_tab(
        &mut self,
        project: &ProjectRecord,
        record: &TabRecord,
        resolve: &dyn Fn(Option<&yien_proto::ProfileId>) -> Result<LaunchPlan, CoreError>,
        disk: &Disk,
        fx: &mut Vec<Effect>,
        notes: &mut Notes,
    ) -> bool {
        let order = record.layout.panes();
        let Some(tree) = shape(&record.layout, &order).and_then(|s| Tree::from_shape(&s)) else {
            return false;
        };
        let leaves: HashMap<u64, PaneId> = (0u64..).zip(order.iter().copied()).collect();
        if !order.contains(&record.focused) {
            return false;
        }
        let mut panes = Vec::new();
        for id in &order {
            let Some(pane) = record.panes.iter().find(|p| p.id == *id) else {
                return false;
            };
            let profile = yien_proto::ProfileId::new(pane.profile.as_str());
            let plan = match resolve(Some(&profile)) {
                Ok(plan) => plan,
                Err(_) => match resolve(None) {
                    Ok(plan) => {
                        notes.profiles.insert(pane.profile.as_str().to_owned());
                        plan
                    }
                    Err(_) => return false,
                },
            };
            let cwd = disk.pick(&[&pane.cwd, &project.root]);
            let moved = cwd != pane.cwd;
            if moved {
                notes.missing.insert(pane.cwd.clone());
            }
            let resume = match resume_plan(pane) {
                ResumeAction::Resume { kind, session }
                    if plan.can_resume() && plan.profile == kind.as_str() =>
                {
                    // Resuming elsewhere would carry the conversation into
                    // another folder; a new session starts there instead.
                    notes.fresh_agents |= moved;
                    (!moved).then(|| session.as_str().to_owned())
                }
                _ => None,
            };
            let kept = (moved || plan.profile != pane.profile.as_str()).then(|| Kept {
                profile: pane.profile.as_str().to_owned(),
                cwd: pane.cwd.clone(),
                agent: pane.agent.clone(),
            });
            panes.push((pane, plan, cwd, resume, kept));
        }

        let dir = disk.pick(&[&record.dir, &project.root]);
        if dir != record.dir {
            notes.missing.insert(record.dir.clone());
        }
        for (pane, plan, cwd, resume, kept) in panes {
            let agent = pane.agent.as_ref().filter(|_| kept.is_none());
            self.panes.insert(
                pane.id,
                Pane {
                    tab: record.id,
                    profile: plan.profile.clone(),
                    launch_cwd: cwd.clone(),
                    ended_explicitly: agent.is_some_and(|a| a.ended_explicitly),
                    kept,
                    custom_name: pane.name.clone().filter(|n| !n.trim().is_empty()),
                    default_name: plan.default_name.clone(),
                    profile_name: plan.profile_name.clone(),
                    title: None,
                    cwd: None,
                    state: AgentState::Idle,
                    running: false,
                    exit_code: None,
                    // Kept until the resumed program reports one, so a quit
                    // before that still saves the session it resumes.
                    session_id: agent
                        .and_then(|a| a.session_id.as_ref())
                        .map(|s| s.as_str().to_owned()),
                    needs_you_since: None,
                    signal_lost: false,
                    visible: false,
                    seen: false,
                    standin: None,
                },
            );
            fx.push(Effect::Spawn {
                pane: pane.id,
                plan: Box::new(plan),
                cwd,
                resume,
            });
        }
        let kept_dir = (dir != record.dir).then(|| record.dir.clone());
        let (branch, kept_branch) = match kept_dir {
            Some(_) => (None, record.branch.clone()),
            None => (record.branch.clone(), None),
        };
        self.tabs.push(Tab {
            id: record.id,
            project: project.id,
            custom_name: record
                .name
                .as_ref()
                .map(|n| n.trim().to_owned())
                .filter(|n| !n.is_empty()),
            default_name: record.title.clone(),
            dir,
            tree,
            leaves,
            focused: record.focused,
            maximized: None,
            kept_dir,
            branch,
            kept_branch,
            muted: record.muted,
        });
        true
    }
}

/// What went wrong in a restore, gathered so the user gets one notice per kind.
#[derive(Debug, Default)]
struct Notes {
    missing: BTreeSet<String>,
    profiles: BTreeSet<String>,
    broken: Vec<String>,
    /// An agent pane started a new session because its folder is missing.
    fresh_agents: bool,
}

impl Notes {
    fn into_messages(self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.missing.is_empty() {
            out.push(format!(
                "{} no longer exist; their panes opened in the project folder or your home \
                 folder.",
                list(&self.missing)
            ));
        }
        if self.fresh_agents {
            out.push(
                "Agent panes there started a new session; the saved one resumes once its \
                 folder is back."
                    .to_owned(),
            );
        }
        if !self.profiles.is_empty() {
            out.push(format!(
                "The profile(s) {} no longer exist; their panes opened the shell.",
                list(&self.profiles)
            ));
        }
        if !self.broken.is_empty() {
            out.push(format!(
                "The saved layout of {} was damaged and was not restored.",
                list(&self.broken)
            ));
        }
        out
    }
}

fn list<'a>(items: impl IntoIterator<Item = &'a String>) -> String {
    items
        .into_iter()
        .map(|i| format!("\"{i}\""))
        .collect::<Vec<_>>()
        .join(", ")
}

fn pane_record(id: PaneId, p: &Pane) -> PaneRecord {
    if let Some(kept) = &p.kept {
        // A folder the pane reports other than the one it was started in is
        // the user moving on; the fallback folder itself is never saved.
        let cwd = match &p.cwd {
            Some(cwd) if *cwd != p.launch_cwd => cwd.clone(),
            _ => kept.cwd.clone(),
        };
        return PaneRecord {
            id,
            name: p.custom_name.clone(),
            profile: yien_session::ProfileId::new(kept.profile.clone()),
            cwd,
            agent: kept.agent.clone(),
            last_command: None,
        };
    }
    let kind = yien_session::ProfileId::new(p.profile.clone());
    PaneRecord {
        id,
        name: p.custom_name.clone(),
        profile: kind.clone(),
        cwd: p.cwd.clone().unwrap_or_else(|| p.launch_cwd.clone()),
        agent: (p.profile != SHELL_PROFILE).then(|| AgentRecord {
            kind,
            session_id: p.session_id.clone().map(SessionRef::new),
            ended_explicitly: p.ended_explicitly,
        }),
        last_command: None,
    }
}

fn orientation(o: yien_layout::Orientation) -> yien_session::Orientation {
    match o {
        yien_layout::Orientation::Horizontal => yien_session::Orientation::Horizontal,
        yien_layout::Orientation::Vertical => yien_session::Orientation::Vertical,
    }
}

fn orientation_back(o: yien_session::Orientation) -> yien_layout::Orientation {
    match o {
        yien_session::Orientation::Horizontal => yien_layout::Orientation::Horizontal,
        yien_session::Orientation::Vertical => yien_layout::Orientation::Vertical,
    }
}

/// The saved layout of a tree; `None` when a leaf has no pane.
fn layout_record(
    shape: &Shape,
    pane: &impl Fn(yien_layout::PaneId) -> Option<PaneId>,
) -> Option<LayoutRecord> {
    match shape {
        Shape::Pane(leaf) => pane(*leaf).map(|pane| LayoutRecord::Leaf { pane }),
        Shape::Split {
            orientation: o,
            children,
        } => {
            let mut nodes = Vec::with_capacity(children.len());
            let mut ratios = Vec::with_capacity(children.len());
            for (flex, child) in children {
                nodes.push(layout_record(child, pane)?);
                ratios.push(*flex);
            }
            Some(LayoutRecord::Split {
                orientation: orientation(*o),
                children: nodes,
                ratios,
            })
        }
    }
}

/// The tree shape of a saved layout, numbering leaves by their place in
/// `order`.
fn shape(layout: &LayoutRecord, order: &[PaneId]) -> Option<Shape> {
    match layout {
        LayoutRecord::Leaf { pane } => {
            let at = order.iter().position(|p| p == pane)?;
            Some(Shape::Pane(u64::try_from(at).ok()?))
        }
        LayoutRecord::Split {
            orientation: o,
            children,
            ratios,
        } => {
            if children.len() != ratios.len() {
                return None;
            }
            let children = ratios
                .iter()
                .zip(children)
                .map(|(r, c)| Some((*r, shape(c, order)?)))
                .collect::<Option<Vec<_>>>()?;
            Some(Shape::Split {
                orientation: orientation_back(*o),
                children,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use yien_proto::{Command, Orientation};

    use super::*;
    use crate::launch;
    use crate::shell::ShellCommand;
    use crate::workspace::PaneEvent;

    const ROOT: &str = "C:/Code/yien";

    fn resolve(profile: Option<&yien_proto::ProfileId>) -> Result<LaunchPlan, CoreError> {
        let profiles = yien_agent::builtin_profiles().unwrap();
        launch::resolve(profile, &profiles, &ShellCommand::new("sh"))
    }

    fn all_exist(snapshot: &SessionSnapshot) -> Disk {
        Disk {
            existing: folders(snapshot).into_iter().collect(),
            home: Some("C:/Users/me".into()),
        }
    }

    /// A project with a shell tab and a tab running `profile`; returns the
    /// second tab's pane.
    fn workspace(profile: &str) -> (Workspace, PaneId) {
        let mut ws = Workspace::new();
        ws.apply(
            Command::NewProject {
                root: ROOT.into(),
                name: None,
            },
            &resolve,
        )
        .unwrap();
        let project = ws.projects[0].id;
        ws.apply(
            Command::NewTab {
                project,
                name: Some("agent".into()),
                profile: Some(yien_proto::ProfileId::new(profile)),
            },
            &resolve,
        )
        .unwrap();
        let pane = ws.tabs[1].focused;
        (ws, pane)
    }

    fn restore(snapshot: &SessionSnapshot, disk: &Disk) -> (Workspace, Vec<Effect>, Vec<String>) {
        let mut ws = Workspace::new();
        let (fx, notes) = ws.restore(snapshot, &resolve, disk);
        (ws, fx, notes)
    }

    fn spawn_of(fx: &[Effect], pane: PaneId) -> (&LaunchPlan, &str, Option<&str>) {
        fx.iter()
            .find_map(|e| match e {
                Effect::Spawn {
                    pane: p,
                    plan,
                    cwd,
                    resume,
                } if *p == pane => Some((&**plan, cwd.as_str(), resume.as_deref())),
                _ => None,
            })
            .unwrap()
    }

    fn first_spawn(fx: &[Effect]) -> PaneId {
        fx.iter()
            .find_map(|e| match e {
                Effect::Spawn { pane, .. } => Some(*pane),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn rf_esp_06_a_new_tab_runs_on_the_main_checkout_without_a_worktree() {
        let mut ws = Workspace::new();
        let new_project = Command::NewProject {
            root: ROOT.into(),
            name: None,
        };
        ws.apply(new_project, &resolve).unwrap();
        let project = ws.projects[0].id;
        let new_tab = Command::NewTab {
            project,
            name: None,
            profile: None,
        };
        let fx = ws.apply(new_tab, &resolve).unwrap();
        let pane = first_spawn(&fx);
        assert_eq!(spawn_of(&fx, pane).1, ROOT);
        let saved = ws.to_session();
        let tab = &saved.projects[0].tabs[1];
        assert_eq!(tab.dir, ROOT);
        assert!(tab.worktree.is_none());
        assert!(tab.panes.iter().all(|p| p.cwd == ROOT));
    }

    #[test]
    fn rf_ses_03_a_claude_pane_resumes_its_exact_session() {
        let (mut ws, pane) = workspace("claude");
        ws.on_pane_event(pane, PaneEvent::SessionId("4f1c-9a".into()));
        let saved = ws.to_session();
        let (back, fx, notes) = restore(&saved, &all_exist(&saved));
        assert!(notes.is_empty(), "{notes:?}");
        let (plan, cwd, resume) = spawn_of(&fx, pane);
        assert_eq!(plan.profile, "claude");
        assert_eq!(cwd, ROOT);
        assert_eq!(resume, Some("4f1c-9a"));
        assert_eq!(back.panes[&pane].session_id.as_deref(), Some("4f1c-9a"));
        assert_eq!(back.to_session(), saved);
    }

    #[test]
    fn rf_ses_04_a_session_the_user_ended_starts_fresh() {
        let (mut ws, pane) = workspace("claude");
        ws.on_pane_event(pane, PaneEvent::SessionId("4f1c".into()));
        ws.on_pane_event(pane, PaneEvent::EndedByUser);
        let saved = ws.to_session();
        let record = &saved.projects[0].tabs[1].panes[0];
        assert!(record.agent.as_ref().unwrap().ended_explicitly);
        let (_, fx, _) = restore(&saved, &all_exist(&saved));
        assert_eq!(spawn_of(&fx, pane).2, None);

        ws.apply(Command::RelaunchPane { pane }, &resolve).unwrap();
        ws.on_pane_event(pane, PaneEvent::SessionId("new".into()));
        let saved = ws.to_session();
        let (_, fx, _) = restore(&saved, &all_exist(&saved));
        assert_eq!(spawn_of(&fx, pane).2, Some("new"));
    }

    #[test]
    fn rn_04_terminating_an_agent_pane_ends_its_session() {
        let (mut ws, pane) = workspace("claude");
        ws.on_pane_event(pane, PaneEvent::SessionId("4f1c".into()));
        ws.apply(Command::TerminatePane { pane }, &resolve).unwrap();
        let saved = ws.to_session();
        let (_, fx, _) = restore(&saved, &all_exist(&saved));
        assert_eq!(spawn_of(&fx, pane).2, None);
    }

    #[test]
    fn rf_ses_05_a_shell_pane_reopens_in_its_last_folder() {
        let (mut ws, _) = workspace("claude");
        let shell = ws.tabs[0].focused;
        ws.on_pane_event(shell, PaneEvent::Cwd("C:/Code/yien/crates".into()));
        let saved = ws.to_session();
        let record = &saved.projects[0].tabs[0].panes[0];
        assert!(record.agent.is_none());
        assert_eq!(record.profile.as_str(), "shell");
        let (_, fx, _) = restore(&saved, &all_exist(&saved));
        let (plan, cwd, resume) = spawn_of(&fx, shell);
        assert_eq!(
            (plan.program.as_str(), cwd, resume),
            ("sh", "C:/Code/yien/crates", None)
        );
    }

    #[test]
    fn rf_ses_12_a_missing_folder_falls_back_to_the_project_root_then_home() {
        let (mut ws, pane) = workspace("claude");
        let shell = ws.tabs[0].focused;
        ws.on_pane_event(shell, PaneEvent::Cwd("C:/gone".into()));
        let saved = ws.to_session();
        let mut disk = all_exist(&saved);
        disk.existing.remove("C:/gone");
        let (_, fx, notes) = restore(&saved, &disk);
        assert_eq!(spawn_of(&fx, shell).1, ROOT);
        assert_eq!(spawn_of(&fx, pane).1, ROOT);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("C:/gone"), "{notes:?}");

        disk.existing.clear();
        let (back, fx, notes) = restore(&saved, &disk);
        assert_eq!(spawn_of(&fx, shell).1, "C:/Users/me");
        assert_eq!(back.tabs[0].dir, "C:/Users/me");
        assert!(notes[0].contains(ROOT), "{notes:?}");

        disk.home = None;
        let (_, fx, _) = restore(&saved, &disk);
        assert_eq!(spawn_of(&fx, shell).1, "C:/gone");
    }

    #[test]
    fn rf_ses_02_ids_focus_selection_names_and_ratios_come_back() {
        let (mut ws, pane) = workspace("claude");
        let split = Command::SplitPane {
            pane,
            orientation: Orientation::Vertical,
            profile: None,
        };
        let below = first_spawn(&ws.apply(split, &resolve).unwrap());
        ws.apply(Command::ResizeSplit { pane, delta: 0.2 }, &resolve)
            .unwrap();
        ws.apply(Command::FocusPane { pane }, &resolve).unwrap();
        let rename = Command::RenamePane {
            pane: below,
            name: "server".into(),
        };
        ws.apply(rename, &resolve).unwrap();
        let first_tab = ws.tabs[0].id;
        ws.apply(Command::SelectTab { tab: first_tab }, &resolve)
            .unwrap();
        let saved = ws.to_session();
        let (mut back, _, _) = restore(&saved, &all_exist(&saved));
        assert_eq!(back.snapshot(), ws.snapshot());
        assert_eq!(back.active_tab, Some(first_tab));

        let split = Command::SplitPane {
            pane: below,
            orientation: Orientation::Horizontal,
            profile: None,
        };
        let new = first_spawn(&back.apply(split, &resolve).unwrap());
        let highest = ws.panes.keys().map(|p| p.get()).max().unwrap();
        assert!(new.get() > highest, "a new pane reused an id");
    }

    #[test]
    fn an_unknown_profile_opens_the_shell_with_a_notice() {
        let (mut ws, pane) = workspace("claude");
        ws.on_pane_event(pane, PaneEvent::SessionId("4f1c".into()));
        let mut saved = ws.to_session();
        let record = &mut saved.projects[0].tabs[1].panes[0];
        record.profile = yien_session::ProfileId::new("aider");
        let (_, fx, notes) = restore(&saved, &all_exist(&saved));
        let (plan, _, resume) = spawn_of(&fx, pane);
        assert_eq!((plan.profile.as_str(), resume), ("shell", None));
        assert!(notes[0].contains("aider"), "{notes:?}");
    }

    #[test]
    fn rf_ses_12_a_fallback_folder_is_never_saved_over_the_original() {
        let (mut ws, pane) = workspace("claude");
        ws.on_pane_event(pane, PaneEvent::SessionId("4f1c".into()));
        let shell = ws.tabs[0].focused;
        ws.on_pane_event(shell, PaneEvent::Cwd("E:/offline".into()));
        let mut saved = ws.to_session();
        saved.projects[0].tabs[1].dir = "E:/offline".into();
        saved.projects[0].tabs[1].panes[0].cwd = "E:/offline".into();
        let mut disk = all_exist(&saved);
        disk.existing.remove("E:/offline");
        let (mut back, fx, notes) = restore(&saved, &disk);
        let (_, cwd, resume) = spawn_of(&fx, pane);
        assert_eq!((cwd, resume), (ROOT, None));
        assert!(notes.iter().any(|n| n.contains("new session")), "{notes:?}");
        back.on_pane_event(pane, PaneEvent::SessionId("fresh".into()));
        back.on_pane_event(shell, PaneEvent::Cwd(ROOT.into()));
        assert_eq!(back.to_session(), saved);

        back.on_pane_event(shell, PaneEvent::Cwd("C:/Code/other".into()));
        let resaved = back.to_session();
        assert_eq!(resaved.projects[0].tabs[0].panes[0].cwd, "C:/Code/other");
    }

    #[test]
    fn an_unknown_profile_and_its_session_are_saved_as_they_were() {
        let (mut ws, pane) = workspace("claude");
        ws.on_pane_event(pane, PaneEvent::SessionId("4f1c".into()));
        let mut saved = ws.to_session();
        let record = &mut saved.projects[0].tabs[1].panes[0];
        record.profile = yien_session::ProfileId::new("aider");
        if let Some(agent) = &mut record.agent {
            agent.kind = yien_session::ProfileId::new("aider");
        }
        let (back, _, _) = restore(&saved, &all_exist(&saved));
        assert_eq!(back.to_session(), saved);
    }

    #[test]
    fn rf_esp_04_the_branch_is_saved_and_shown_again_on_restore() {
        let (mut ws, _) = workspace("shell");
        let tab = ws.tabs[1].id;
        ws.set_branch(tab, Some("feat/x".into()));
        let saved = ws.to_session();
        assert_eq!(saved.projects[0].tabs[1].branch.as_deref(), Some("feat/x"));
        let (back, _, _) = restore(&saved, &all_exist(&saved));
        let info = back.snapshot().tabs.into_iter().find(|t| t.id == tab);
        assert_eq!(info.unwrap().branch.as_deref(), Some("feat/x"));
        assert_eq!(back.to_session(), saved);
    }

    #[test]
    fn rf_ses_12_a_fallback_folder_keeps_the_saved_branch_out_of_sight() {
        let (mut ws, _) = workspace("shell");
        let tab = ws.tabs[1].id;
        ws.set_branch(tab, Some("feat/x".into()));
        let mut saved = ws.to_session();
        saved.projects[0].tabs[1].dir = "E:/offline".into();
        saved.projects[0].tabs[1].panes[0].cwd = "E:/offline".into();
        let mut disk = all_exist(&saved);
        disk.existing.remove("E:/offline");
        let (mut back, _, _) = restore(&saved, &disk);
        let shown = |ws: &Workspace| ws.snapshot().tabs.into_iter().find(|t| t.id == tab);
        assert_eq!(
            shown(&back).unwrap().branch,
            None,
            "not the fallback's branch"
        );
        back.set_branch(tab, Some("main".into()));
        assert_eq!(
            back.to_session().projects[0].tabs[1].branch.as_deref(),
            Some("feat/x")
        );
    }

    #[test]
    fn rf_esp_16_a_tab_name_survives_a_restore_and_can_still_be_cleared() {
        let (mut ws, _) = workspace("shell");
        let tab = ws.tabs[0].id;
        let rename = |name: &str| Command::RenameTab {
            tab,
            name: name.into(),
        };
        ws.apply(rename("review"), &resolve).unwrap();
        let saved = ws.to_session();
        let record = &saved.projects[0].tabs[0];
        assert_eq!(
            (record.title.as_str(), record.name.as_deref()),
            ("Tab 1", Some("review"))
        );
        let (mut back, _, _) = restore(&saved, &all_exist(&saved));
        assert_eq!(back.snapshot().tabs[0].name, "review");
        assert_eq!(back.to_session(), saved);
        back.apply(rename(""), &resolve).unwrap();
        assert_eq!(back.snapshot().tabs[0].name, "Tab 1");
    }

    #[test]
    fn rf_atn_11_mutes_survive_a_restore() {
        let (mut ws, _) = workspace("shell");
        let tab = ws.tabs[0].id;
        ws.apply(Command::SetTabMuted { tab, muted: true }, &resolve)
            .unwrap();
        ws.apply(Command::SetNotificationsMuted { muted: true }, &resolve)
            .unwrap();
        let saved = ws.to_session();
        assert!(saved.notifications_muted && saved.projects[0].tabs[0].muted);
        let (back, _, _) = restore(&saved, &all_exist(&saved));
        let snapshot = back.snapshot();
        assert!(snapshot.notifications_muted && snapshot.tabs[0].muted);
        assert_eq!(back.to_session(), saved);
    }

    #[test]
    fn rf_esp_17_a_folded_project_stays_folded_after_a_restore() {
        let (mut ws, _) = workspace("shell");
        let project = ws.projects[0].id;
        let fold = Command::SetProjectCollapsed {
            project,
            collapsed: true,
        };
        ws.apply(fold, &resolve).unwrap();
        let saved = ws.to_session();
        assert!(saved.projects[0].collapsed);
        let (back, _, _) = restore(&saved, &all_exist(&saved));
        assert!(back.snapshot().projects[0].collapsed);
        assert_eq!(back.to_session(), saved);
    }

    #[test]
    fn a_tab_whose_layout_cannot_be_rebuilt_is_left_out() {
        let (ws, pane) = workspace("claude");
        let mut saved = ws.to_session();
        saved.projects[0].tabs[1].layout = LayoutRecord::Split {
            orientation: yien_session::Orientation::Horizontal,
            children: vec![LayoutRecord::Leaf { pane }],
            ratios: vec![1.0],
        };
        let (back, fx, notes) = restore(&saved, &all_exist(&saved));
        assert_eq!(back.tabs.len(), 1);
        assert_eq!(back.check(), Ok(()));
        let spawned_it = |e: &Effect| matches!(e, Effect::Spawn { pane: p, .. } if *p == pane);
        assert!(!fx.iter().any(spawned_it));
        assert!(notes[0].contains("agent"), "{notes:?}");
    }
}
