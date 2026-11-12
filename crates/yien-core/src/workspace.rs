//! The workspace model: projects, tabs, layouts and pane details. Pure: every
//! change returns the effects the service must carry out (messages for the UI,
//! panes to start or stop), so the invariants are testable without a PTY.

use std::collections::{BTreeMap, HashMap};

use yien_layout::{Rect, Tree};
use yien_proto::{
    AgentState, Command, CoreMsg, PaneId, PaneInfo, PendingQuestion, ProfileId, ProfileInfo,
    ProjectId, ProjectInfo, SideBars, TabId, TabInfo, WorkspaceSnapshot, same_folder,
};
use yien_session::is_session_id;

use crate::CoreError;
use crate::convert;
use crate::launch::LaunchPlan;

mod session;
pub(crate) mod worktree;

use session::Kept;
pub(crate) use session::{Disk, folders};

/// Any area works for neighbour lookups; this one keeps the tree's epsilon
/// small relative to the smallest pane.
const AREA: Rect = Rect {
    x: 0.0,
    y: 0.0,
    w: 100.0,
    h: 100.0,
};

/// Something the service must do after a change.
#[derive(Debug)]
pub(crate) enum Effect {
    /// Send this to the UI.
    Emit(CoreMsg),
    /// Start the actor of a new pane.
    Spawn {
        pane: PaneId,
        plan: Box<LaunchPlan>,
        cwd: String,
        /// The agent session the launch resumes (RF-SES-03).
        resume: Option<String>,
    },
    /// Stop a pane's actor; its program is terminated.
    Stop { pane: PaneId },
    /// Start the pane's program again, as launch number `launch`.
    Relaunch { pane: PaneId, launch: u64 },
    /// Terminate the pane's program, keeping the pane.
    Terminate { pane: PaneId },
    /// Create a worktree for a new tab of `project` (RF-ESP-05).
    AddWorktree(worktree::AddJob),
    /// List the past agent sessions of the project at `root` off the
    /// command loop, and answer with [`CoreMsg::AgentSessions`] (RF-SES-07).
    ListAgentSessions {
        project: ProjectId,
        request: u64,
        root: String,
    },
    /// Tell the UI what removing a tab's worktree would lose (RN-08).
    CheckWorktree { tab: TabId, worktree: String },
    /// Remove a worktree once the panes that ran in it have closed.
    RemoveWorktree(worktree::RemoveJob),
    /// Whether the pane is drawn and whether the user sees it (RN-07).
    View {
        pane: PaneId,
        visible: bool,
        seen: bool,
    },
}

/// What a pane actor reports about its pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PaneEvent {
    /// The agent state, and while it is "needs you", error or done when it
    /// got to that kind of state.
    State(AgentState, Option<u64>),
    /// What the agent asks while it needs the user (RF-ATN-13).
    Question(Option<PendingQuestion>),
    /// The "signal lost" mark went on or off.
    SignalLost(bool),
    Title(Option<String>),
    Cwd(String),
    /// The agent session the pane's launch number `launch` runs.
    SessionId {
        id: String,
        launch: u64,
    },
    /// The user ended the agent session of launch number `launch` (`/exit`,
    /// `/logout`); it is not resumed on restore (RN-04).
    EndedByUser {
        launch: u64,
    },
    Started,
    Exited(Option<u32>),
    /// The program could not start or can no longer be shown; the actor
    /// still runs and can relaunch it.
    Failed(String),
    /// Something the user should know about a program that runs.
    Notice(String),
    /// The pane runs a stand-in for its profile's program, with this name
    /// and profile name: the shell a failed resume fell back to
    /// (RF-SES-11). `None` once the profile's own program runs again.
    Standin(Option<(String, String)>),
    /// The actor itself died; a relaunch starts a new one.
    Crashed,
    Selection(Option<String>),
}

impl PaneEvent {
    /// A detail of the pane rather than a step of its program's lifecycle;
    /// only the latest value of each matters.
    pub fn is_detail(&self) -> bool {
        matches!(
            self,
            PaneEvent::State(..)
                | PaneEvent::Question(_)
                | PaneEvent::SignalLost(_)
                | PaneEvent::Title(_)
                | PaneEvent::Cwd(_)
                | PaneEvent::SessionId { .. }
                | PaneEvent::Selection(_)
        )
    }
}

#[derive(Debug, Clone)]
struct Project {
    id: ProjectId,
    name: String,
    root: String,
    tabs: Vec<TabId>,
    /// Its tabs are folded away in the sidebar (RF-ESP-17).
    collapsed: bool,
}

#[derive(Debug, Clone)]
struct Tab {
    id: TabId,
    project: ProjectId,
    /// The name the user gave it (RF-ESP-16).
    custom_name: Option<String>,
    /// The automatic name, shown while it has no name of its own.
    default_name: String,
    dir: String,
    tree: Tree,
    leaves: HashMap<yien_layout::PaneId, PaneId>,
    focused: PaneId,
    maximized: Option<PaneId>,
    /// The saved folder when a restore had to open the tab elsewhere; saved
    /// again in its place, so a folder that is only offline is not lost.
    kept_dir: Option<String>,
    /// The git branch last seen in `dir` (RF-ESP-04).
    branch: Option<String>,
    /// The saved branch that goes with `kept_dir`.
    kept_branch: Option<String>,
    /// Its toasts and taskbar flashes are muted (RF-ATN-11).
    muted: bool,
    /// The linked worktree it was opened on, which is `dir` (RF-ESP-05).
    worktree: Option<String>,
    /// The saved worktree that goes with `kept_dir`.
    kept_worktree: Option<String>,
}

impl Tab {
    fn leaf_of(&self, pane: PaneId) -> Option<yien_layout::PaneId> {
        self.leaves
            .iter()
            .find_map(|(leaf, p)| (*p == pane).then_some(*leaf))
    }

    fn panes(&self) -> Vec<PaneId> {
        self.tree
            .panes()
            .into_iter()
            .filter_map(|leaf| self.leaves.get(&leaf).copied())
            .collect()
    }
}

#[derive(Debug, Clone)]
struct Pane {
    tab: TabId,
    /// The profile's id, as the saved session records it.
    profile: String,
    /// The folder the pane's program started in.
    launch_cwd: String,
    /// The user ended the agent session of the current launch (RN-04).
    /// Only commands and reports of the current launch change it.
    ended_explicitly: bool,
    /// The number of the pane's current launch, raised by each relaunch.
    /// Reports carry it, so one from a launch the user replaced is dropped.
    launch: u64,
    /// What the saved session said, when a restore had to launch the pane
    /// differently (missing folder, unknown profile); saved again instead.
    kept: Option<Kept>,
    custom_name: Option<String>,
    default_name: String,
    profile_name: String,
    title: Option<String>,
    cwd: Option<String>,
    state: AgentState,
    running: bool,
    exit_code: Option<u32>,
    session_id: Option<String>,
    /// While in "needs you", error or done: when it got to that kind of
    /// state (RF-ATN-05, RF-ATN-13).
    attention_since: Option<u64>,
    question: Option<PendingQuestion>,
    signal_lost: bool,
    visible: bool,
    seen: bool,
    /// The name and profile name of a stand-in program, shown instead of
    /// the profile's while it runs; the saved session keeps the profile.
    standin: Option<(String, String)>,
    /// Its program exited, failed to start or lost its actor, and has not
    /// started again.
    stopped: bool,
}

impl Pane {
    /// Whether its own program is starting or running, so its session id
    /// names a session that runs here (RF-SES-07).
    fn runs_session(&self) -> bool {
        !self.stopped && self.standin.is_none()
    }
}

/// The bars with every width that is not a positive number dropped, so the
/// session never saves one (RF-NAV-10).
fn finite_widths(mut bars: SideBars) -> SideBars {
    for bar in [&mut bars.tab_bar, &mut bars.context_bar] {
        bar.width = bar.width.filter(|w| w.is_finite() && *w > 0.0);
    }
    bars
}

/// Every project, tab and pane, and which tab is selected.
#[derive(Debug, Clone, Default)]
pub(crate) struct Workspace {
    next_id: u64,
    projects: Vec<Project>,
    tabs: Vec<Tab>,
    panes: BTreeMap<PaneId, Pane>,
    active_tab: Option<TabId>,
    window_focused: bool,
    profiles: Vec<ProfileInfo>,
    /// Every toast and taskbar flash is muted (RF-ATN-11).
    notifications_muted: bool,
    /// The side bars as the UI last drew them (RF-NAV-10).
    side_bars: SideBars,
}

impl Workspace {
    pub fn new() -> Self {
        Self::default()
    }

    /// How many tabs are open.
    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    /// The profiles the snapshot lists for new panes.
    pub fn with_profiles(mut self, profiles: Vec<ProfileInfo>) -> Self {
        self.profiles = profiles;
        self
    }

    fn next(&mut self) -> Result<u64, CoreError> {
        self.next_id = self.next_id.checked_add(1).ok_or(CoreError::OutOfIds)?;
        Ok(self.next_id)
    }

    pub fn snapshot(&self) -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            projects: self.projects.iter().map(project_info).collect(),
            tabs: self.tabs.iter().map(|t| self.tab_info(t)).collect(),
            panes: self.panes.iter().map(|(id, p)| pane_info(*id, p)).collect(),
            active_tab: self.active_tab,
            profiles: self.profiles.clone(),
            notifications_muted: self.notifications_muted,
            side_bars: self.side_bars,
        }
    }

    fn tab_info(&self, tab: &Tab) -> TabInfo {
        let layout = convert::layout(&tab.tree.shape(), &|leaf| tab.leaves.get(&leaf).copied())
            .unwrap_or(yien_proto::LayoutNode::Pane(tab.focused));
        TabInfo {
            id: tab.id,
            project: tab.project,
            name: tab
                .custom_name
                .clone()
                .unwrap_or_else(|| tab.default_name.clone()),
            dir: tab.dir.clone(),
            branch: tab.branch.clone(),
            layout,
            focused: tab.focused,
            maximized: tab.maximized,
            muted: tab.muted,
            worktree: tab.worktree.clone(),
        }
    }

    fn tab(&self, id: TabId) -> Result<&Tab, CoreError> {
        self.tabs
            .iter()
            .find(|t| t.id == id)
            .ok_or(CoreError::UnknownTab(id))
    }

    fn tab_mut(&mut self, id: TabId) -> Result<&mut Tab, CoreError> {
        self.tabs
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or(CoreError::UnknownTab(id))
    }

    fn tab_of(&self, pane: PaneId) -> Result<TabId, CoreError> {
        self.panes
            .get(&pane)
            .map(|p| p.tab)
            .ok_or(CoreError::UnknownPane(pane))
    }

    fn emit_tab(&self, tab: TabId, fx: &mut Vec<Effect>) {
        if let Ok(tab) = self.tab(tab) {
            fx.push(Effect::Emit(CoreMsg::TabUpdated(self.tab_info(tab))));
        }
    }

    fn emit_pane(&self, pane: PaneId, fx: &mut Vec<Effect>) {
        if let Some(p) = self.panes.get(&pane) {
            fx.push(Effect::Emit(CoreMsg::PaneUpdated(pane_info(pane, p))));
        }
    }

    /// The selected tab and its folder.
    pub fn active_dir(&self) -> Option<(TabId, &str)> {
        let tab = self.tab(self.active_tab?).ok()?;
        Some((tab.id, tab.dir.as_str()))
    }

    /// Whether `root` is the folder of a project already ([`same_folder`]).
    pub fn has_project_at(&self, root: &str) -> bool {
        self.projects.iter().any(|p| same_folder(&p.root, root))
    }

    /// Whether the tab is open.
    pub fn has_tab(&self, tab: TabId) -> bool {
        self.tab(tab).is_ok()
    }

    /// Every tab and its folder, in no particular order.
    pub fn tab_dirs(&self) -> impl Iterator<Item = (TabId, &str)> {
        self.tabs.iter().map(|t| (t.id, t.dir.as_str()))
    }

    /// Records the branch git found in a tab's folder (RF-ESP-04); the tab
    /// is announced again only when it changed. A closed tab is ignored.
    pub fn set_branch(&mut self, tab: TabId, branch: Option<String>) -> Vec<Effect> {
        let mut fx = Vec::new();
        if let Ok(t) = self.tab_mut(tab)
            && t.branch != branch
        {
            t.branch = branch;
            self.emit_tab(tab, &mut fx);
        }
        fx
    }

    /// Selects a tab. A tab that becomes selected unfolds its project, so the
    /// jump to a pane that needs the user never lands in a hidden tab.
    fn set_active(&mut self, tab: Option<TabId>, fx: &mut Vec<Effect>) {
        if self.active_tab != tab {
            self.active_tab = tab;
            fx.push(Effect::Emit(CoreMsg::ActiveTab { tab }));
            if let Some(tab) = tab {
                self.unfold_project_of(tab, fx);
            }
        }
    }

    fn unfold_project_of(&mut self, tab: TabId, fx: &mut Vec<Effect>) {
        if let Ok(project) = self.tab(tab).map(|t| t.project) {
            self.set_collapsed(project, false, fx);
        }
    }

    fn set_collapsed(&mut self, project: ProjectId, collapsed: bool, fx: &mut Vec<Effect>) {
        if let Some(p) = self.projects.iter_mut().find(|p| p.id == project)
            && p.collapsed != collapsed
        {
            p.collapsed = collapsed;
            fx.push(Effect::Emit(CoreMsg::ProjectUpdated(project_info(p))));
        }
    }

    /// Applies a command. `resolve` turns a profile into a launch plan; a
    /// command that fails changes nothing.
    pub fn apply(
        &mut self,
        command: Command,
        resolve: &dyn Fn(Option<&ProfileId>) -> Result<LaunchPlan, CoreError>,
    ) -> Result<Vec<Effect>, CoreError> {
        let mut fx = Vec::new();
        match command {
            Command::NewProject { root, name } => {
                let existing = self
                    .projects
                    .iter()
                    .find(|p| same_folder(&p.root, &root))
                    .map(|p| p.id);
                if let Some(project) = existing {
                    self.open_project(project, resolve, &mut fx)?;
                } else {
                    let plan = resolve(None)?;
                    let id = ProjectId::new(self.next()?);
                    let name = name
                        .filter(|n| !n.trim().is_empty())
                        .unwrap_or_else(|| folder_name(&root));
                    self.projects.push(Project {
                        id,
                        name,
                        root,
                        tabs: Vec::new(),
                        collapsed: false,
                    });
                    self.new_tab(id, None, plan, None, &mut fx)?;
                }
            }
            Command::CloseProject { project } => self.close_project(project, &mut fx)?,
            Command::SetProjectCollapsed { project, collapsed } => {
                if !self.projects.iter().any(|p| p.id == project) {
                    return Err(CoreError::UnknownProject(project));
                }
                self.set_collapsed(project, collapsed, &mut fx);
            }
            Command::RenameTab { tab, name } => {
                let t = self.tab_mut(tab)?;
                let name = name.trim();
                t.custom_name =
                    (!name.is_empty() && name != t.default_name).then(|| name.to_owned());
                self.emit_tab(tab, &mut fx);
            }
            Command::MoveTab {
                tab,
                project,
                to_index,
            } => self.move_tab(tab, project, to_index, &mut fx)?,
            Command::SetTabMuted { tab, muted } => {
                let t = self.tab_mut(tab)?;
                if t.muted != muted {
                    t.muted = muted;
                    self.emit_tab(tab, &mut fx);
                }
            }
            Command::SetNotificationsMuted { muted } => {
                if self.notifications_muted != muted {
                    self.notifications_muted = muted;
                    fx.push(Effect::Emit(CoreMsg::NotificationsMuted { muted }));
                }
            }
            Command::SetSideBars { bars } => self.side_bars = finite_widths(bars),
            Command::NewTab {
                project,
                name,
                profile,
            } => {
                if !self.projects.iter().any(|p| p.id == project) {
                    return Err(CoreError::UnknownProject(project));
                }
                let plan = resolve(profile.as_ref())?;
                self.new_tab(project, name, plan, None, &mut fx)?;
            }
            Command::NewWorktreeTab {
                project,
                branch,
                base,
            } => fx.push(self.add_worktree(project, branch, base)?),
            Command::CheckWorktree { tab } => {
                let worktree = self
                    .tab(tab)?
                    .worktree
                    .clone()
                    .ok_or(CoreError::NoWorktree(tab))?;
                fx.push(Effect::CheckWorktree { tab, worktree });
            }
            Command::RemoveWorktreeTab {
                tab,
                discard_changes,
                confirmed,
            } => self.remove_worktree_tab(tab, discard_changes, confirmed, &mut fx)?,
            Command::CloseTab { tab } => self.close_tab(tab, &mut fx)?,
            Command::SelectTab { tab } => self.select(tab, &mut fx)?,
            Command::SplitPane {
                pane,
                orientation,
                profile,
            } => {
                let tab_id = self.tab_of(pane)?;
                let plan = resolve(profile.as_ref())?;
                let new = PaneId::new(self.next()?);
                let tab = self.tab_mut(tab_id)?;
                let leaf = tab.leaf_of(pane).ok_or(CoreError::UnknownPane(pane))?;
                let new_leaf = tab
                    .tree
                    .split(leaf, convert::orientation(orientation))
                    .ok_or(CoreError::UnknownPane(pane))?;
                tab.leaves.insert(new_leaf, new);
                tab.maximized = None;
                let cwd = tab.dir.clone();
                self.add_pane(new, tab_id, plan, cwd, None, &mut fx);
                self.focus(new, &mut fx)?;
                self.emit_pane(new, &mut fx);
            }
            Command::ClosePane { pane } => self.close_pane(pane, &mut fx)?,
            Command::FocusPane { pane } => self.focus(pane, &mut fx)?,
            Command::FocusDirection { direction } => {
                let Some(tab_id) = self.active_tab else {
                    return Ok(fx);
                };
                let tab = self.tab(tab_id)?;
                if tab.maximized.is_some() {
                    return Ok(fx);
                }
                let target = tab
                    .leaf_of(tab.focused)
                    .and_then(|leaf| {
                        tab.tree
                            .neighbour(leaf, convert::direction(direction), AREA)
                    })
                    .and_then(|leaf| tab.leaves.get(&leaf).copied());
                if let Some(target) = target {
                    self.focus(target, &mut fx)?;
                }
            }
            Command::ResizeSplit { pane, delta } => {
                let tab_id = self.tab_of(pane)?;
                let tab = self.tab_mut(tab_id)?;
                let leaf = tab.leaf_of(pane).ok_or(CoreError::UnknownPane(pane))?;
                if tab.tree.resize(leaf, delta) {
                    self.emit_tab(tab_id, &mut fx);
                }
            }
            Command::SwapPanes { a, b } => {
                let tab_id = self.same_tab(a, b)?;
                let tab = self.tab_mut(tab_id)?;
                let (Some(la), Some(lb)) = (tab.leaf_of(a), tab.leaf_of(b)) else {
                    return Err(CoreError::UnknownPane(a));
                };
                tab.leaves.insert(la, b);
                tab.leaves.insert(lb, a);
                self.emit_tab(tab_id, &mut fx);
            }
            Command::MovePane { pane, target, zone } => {
                let tab_id = self.same_tab(pane, target)?;
                let tab = self.tab_mut(tab_id)?;
                let (Some(lp), Some(lt)) = (tab.leaf_of(pane), tab.leaf_of(target)) else {
                    return Err(CoreError::UnknownPane(pane));
                };
                if tab.tree.move_pane(lp, lt, convert::zone(zone)) {
                    self.emit_tab(tab_id, &mut fx);
                }
            }
            Command::ToggleMaximize { pane } => {
                let tab_id = self.tab_of(pane)?;
                let tab = self.tab_mut(tab_id)?;
                tab.maximized = if tab.maximized == Some(pane) {
                    None
                } else {
                    Some(pane)
                };
                tab.focused = pane;
                self.emit_tab(tab_id, &mut fx);
                self.set_active(Some(tab_id), &mut fx);
            }
            Command::RenamePane { pane, name } => {
                let p = self
                    .panes
                    .get_mut(&pane)
                    .ok_or(CoreError::UnknownPane(pane))?;
                p.custom_name = (!name.trim().is_empty()).then_some(name);
                self.emit_pane(pane, &mut fx);
            }
            Command::RelaunchPane { pane } => {
                self.tab_of(pane)?;
                let p = self
                    .panes
                    .get_mut(&pane)
                    .ok_or(CoreError::UnknownPane(pane))?;
                // A new session until the new launch names one, so a quit
                // before that never resumes the one the user ended.
                p.launch += 1;
                p.ended_explicitly = false;
                p.session_id = None;
                let launch = p.launch;
                fx.push(Effect::Relaunch { pane, launch });
                self.emit_pane(pane, &mut fx);
            }
            Command::TerminatePane { pane } => {
                let p = self
                    .panes
                    .get_mut(&pane)
                    .ok_or(CoreError::UnknownPane(pane))?;
                p.ended_explicitly = true;
                fx.push(Effect::Terminate { pane });
            }
            Command::ListAgentSessions { project, request } => {
                let root = self.projects.iter().find(|p| p.id == project);
                fx.push(match root {
                    Some(p) => Effect::ListAgentSessions {
                        project,
                        request,
                        root: p.root.clone(),
                    },
                    // Answered all the same, so the list stops waiting.
                    None => Effect::Emit(CoreMsg::AgentSessions {
                        project,
                        request,
                        sessions: Vec::new(),
                    }),
                });
            }
            Command::ResumeAgentSession {
                project,
                profile,
                session_id,
            } => self.resume_session(project, &profile, session_id, resolve, &mut fx)?,
        }
        self.refresh_views(&mut fx);
        debug_assert_eq!(self.check(), Ok(()));
        Ok(fx)
    }

    /// Selects a tab, focusing its pane that has needed the user longest
    /// (RF-ATN-06), and unfolds its project even when it was selected already.
    fn select(&mut self, tab: TabId, fx: &mut Vec<Effect>) -> Result<(), CoreError> {
        match self.longest_waiting(self.tab(tab)?) {
            Some(pane) => self.focus(pane, fx)?,
            None => self.set_active(Some(tab), fx),
        }
        self.unfold_project_of(tab, fx);
        Ok(())
    }

    /// Moves a tab to `to_index` among its project's tabs, clamped to the
    /// last place. The project is announced only when the order changed.
    fn move_tab(
        &mut self,
        tab: TabId,
        project: ProjectId,
        to_index: u32,
        fx: &mut Vec<Effect>,
    ) -> Result<(), CoreError> {
        let own = self.tab(tab)?.project;
        if own != project {
            return Err(CoreError::OtherProject(tab, project));
        }
        let p = self
            .projects
            .iter_mut()
            .find(|p| p.id == project)
            .ok_or(CoreError::UnknownProject(project))?;
        let from = p
            .tabs
            .iter()
            .position(|t| *t == tab)
            .ok_or(CoreError::UnknownTab(tab))?;
        let last = p.tabs.len() - 1;
        let to = usize::try_from(to_index).map_or(last, |i| i.min(last));
        if to != from {
            let moved = p.tabs.remove(from);
            p.tabs.insert(to, moved);
            fx.push(Effect::Emit(CoreMsg::ProjectUpdated(project_info(p))));
        }
        Ok(())
    }

    /// Goes to a project already registered: its selected tab, else its
    /// first; a project with no tab left gets a new one with the shell.
    fn open_project(
        &mut self,
        project: ProjectId,
        resolve: &dyn Fn(Option<&ProfileId>) -> Result<LaunchPlan, CoreError>,
        fx: &mut Vec<Effect>,
    ) -> Result<(), CoreError> {
        let tabs = self
            .projects
            .iter()
            .find(|p| p.id == project)
            .map(|p| p.tabs.clone())
            .ok_or(CoreError::UnknownProject(project))?;
        let tab = self
            .active_tab
            .filter(|t| tabs.contains(t))
            .or_else(|| tabs.first().copied());
        match tab {
            Some(tab) => self.select(tab, fx),
            None => {
                let plan = resolve(None)?;
                self.new_tab(project, None, plan, None, fx)
            }
        }
    }

    /// The pane of `tab` that has needed the user longest (RF-ATN-06).
    fn longest_waiting(&self, tab: &Tab) -> Option<PaneId> {
        tab.panes()
            .into_iter()
            .filter_map(|id| {
                let p = self.panes.get(&id)?;
                p.state.is_red().then_some((p.attention_since, id))
            })
            .min()
            .map(|(_, id)| id)
    }

    fn same_tab(&self, a: PaneId, b: PaneId) -> Result<TabId, CoreError> {
        let (ta, tb) = (self.tab_of(a)?, self.tab_of(b)?);
        if ta != tb {
            return Err(CoreError::DifferentTabs(a, b));
        }
        Ok(ta)
    }

    /// Opens a tab on the project's folder, or on `worktree` (its folder and
    /// branch), which also names the tab until the user renames it.
    fn new_tab(
        &mut self,
        project: ProjectId,
        name: Option<String>,
        plan: LaunchPlan,
        worktree: Option<(String, String)>,
        fx: &mut Vec<Effect>,
    ) -> Result<(), CoreError> {
        self.open_tab(project, name, plan, worktree, None, fx)
    }

    /// Resumes a past agent session in a new tab on the project's folder,
    /// the folder its CLI keeps the session under (RF-SES-07). A session a
    /// pane already runs is focused instead: two programs resuming one
    /// session would both write it.
    fn resume_session(
        &mut self,
        project: ProjectId,
        profile: &ProfileId,
        session_id: String,
        resolve: &dyn Fn(Option<&ProfileId>) -> Result<LaunchPlan, CoreError>,
        fx: &mut Vec<Effect>,
    ) -> Result<(), CoreError> {
        if !is_session_id(&session_id) {
            return Err(CoreError::InvalidSessionId);
        }
        if !self.projects.iter().any(|p| p.id == project) {
            return Err(CoreError::UnknownProject(project));
        }
        let holds = |p: &Pane| {
            p.session_id
                .as_deref()
                .is_some_and(|s| s.eq_ignore_ascii_case(&session_id))
        };
        let open = self
            .panes
            .iter()
            .find_map(|(id, p)| (holds(p) && p.runs_session()).then_some(*id));
        if let Some(pane) = open {
            return self.focus(pane, fx);
        }
        let plan = resolve(Some(profile))?;
        if !plan.can_resume() {
            return Err(CoreError::CannotResume(plan.profile_name));
        }
        // A stopped pane that named the session gives it up, so a restart
        // does not resume it there as well.
        let stale: Vec<PaneId> = self
            .panes
            .iter()
            .filter_map(|(id, p)| holds(p).then_some(*id))
            .collect();
        for pane in stale {
            if let Some(p) = self.panes.get_mut(&pane) {
                p.session_id = None;
            }
            self.emit_pane(pane, fx);
        }
        self.open_tab(project, None, plan, None, Some(session_id), fx)
    }

    /// [`Workspace::new_tab`], its pane resuming `resume` when set.
    fn open_tab(
        &mut self,
        project: ProjectId,
        name: Option<String>,
        plan: LaunchPlan,
        worktree: Option<(String, String)>,
        resume: Option<String>,
        fx: &mut Vec<Effect>,
    ) -> Result<(), CoreError> {
        let tab_id = TabId::new(self.next()?);
        let pane = PaneId::new(self.next()?);
        let p = self
            .projects
            .iter_mut()
            .find(|p| p.id == project)
            .ok_or(CoreError::UnknownProject(project))?;
        p.tabs.push(tab_id);
        let custom_name = name.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty());
        let (default_name, dir, branch, worktree) = match worktree {
            Some((dir, branch)) => (branch.clone(), dir.clone(), Some(branch), Some(dir)),
            None => (format!("Tab {}", p.tabs.len()), p.root.clone(), None, None),
        };
        let tree = Tree::new();
        let leaves = tree.panes().into_iter().map(|leaf| (leaf, pane)).collect();
        self.tabs.push(Tab {
            id: tab_id,
            project,
            custom_name,
            default_name,
            dir: dir.clone(),
            tree,
            leaves,
            focused: pane,
            maximized: None,
            kept_dir: None,
            branch,
            kept_branch: None,
            muted: false,
            worktree,
            kept_worktree: None,
        });
        if let Some(p) = self.projects.iter().find(|p| p.id == project) {
            fx.push(Effect::Emit(CoreMsg::ProjectUpdated(project_info(p))));
        }
        self.add_pane(pane, tab_id, plan, dir, resume, fx);
        self.emit_tab(tab_id, fx);
        self.emit_pane(pane, fx);
        self.set_active(Some(tab_id), fx);
        Ok(())
    }

    /// Adds a pane and starts its program, resuming the agent session
    /// `resume` when set; the pane names that session until the program
    /// reports one, as a restored pane does.
    fn add_pane(
        &mut self,
        pane: PaneId,
        tab: TabId,
        plan: LaunchPlan,
        cwd: String,
        resume: Option<String>,
        fx: &mut Vec<Effect>,
    ) {
        self.panes.insert(
            pane,
            Pane {
                tab,
                profile: plan.profile.clone(),
                launch_cwd: cwd.clone(),
                ended_explicitly: false,
                launch: 0,
                kept: None,
                custom_name: None,
                default_name: plan.default_name.clone(),
                profile_name: plan.profile_name.clone(),
                title: None,
                cwd: None,
                state: AgentState::Idle,
                running: false,
                exit_code: None,
                session_id: resume.clone(),
                attention_since: None,
                question: None,
                signal_lost: false,
                visible: false,
                seen: false,
                standin: None,
                stopped: false,
            },
        );
        fx.push(Effect::Spawn {
            pane,
            plan: Box::new(plan),
            cwd,
            resume,
        });
    }

    fn focus(&mut self, pane: PaneId, fx: &mut Vec<Effect>) -> Result<(), CoreError> {
        let tab_id = self.tab_of(pane)?;
        let tab = self.tab_mut(tab_id)?;
        let changed = tab.focused != pane || tab.maximized.is_some_and(|m| m != pane);
        tab.focused = pane;
        if tab.maximized.is_some_and(|m| m != pane) {
            tab.maximized = None;
        }
        if changed {
            self.emit_tab(tab_id, fx);
        }
        self.set_active(Some(tab_id), fx);
        Ok(())
    }

    fn close_pane(&mut self, pane: PaneId, fx: &mut Vec<Effect>) -> Result<(), CoreError> {
        let tab_id = self.tab_of(pane)?;
        let tab = self.tab_mut(tab_id)?;
        if tab.leaves.len() <= 1 {
            return self.close_tab(tab_id, fx);
        }
        let order = tab.panes();
        let leaf = tab.leaf_of(pane).ok_or(CoreError::UnknownPane(pane))?;
        if !tab.tree.close(leaf) {
            return Err(CoreError::UnknownPane(pane));
        }
        tab.leaves.remove(&leaf);
        if tab.maximized == Some(pane) {
            tab.maximized = None;
        }
        if tab.focused == pane {
            let at = order.iter().position(|p| *p == pane).unwrap_or(0);
            let remaining: Vec<_> = order.into_iter().filter(|p| *p != pane).collect();
            if let Some(next) = remaining.get(at.saturating_sub(1)).or(remaining.first()) {
                tab.focused = *next;
            }
        }
        self.panes.remove(&pane);
        fx.push(Effect::Stop { pane });
        fx.push(Effect::Emit(CoreMsg::PaneClosed { pane }));
        self.emit_tab(tab_id, fx);
        Ok(())
    }

    fn close_tab(&mut self, tab: TabId, fx: &mut Vec<Effect>) -> Result<(), CoreError> {
        let at = self
            .tabs
            .iter()
            .position(|t| t.id == tab)
            .ok_or(CoreError::UnknownTab(tab))?;
        let removed = self.tabs.remove(at);
        for pane in removed.leaves.values() {
            self.panes.remove(pane);
            fx.push(Effect::Stop { pane: *pane });
        }
        fx.push(Effect::Emit(CoreMsg::TabClosed { tab }));
        let mut sibling = None;
        if let Some(p) = self.projects.iter_mut().find(|p| p.id == removed.project) {
            if let Some(i) = p.tabs.iter().position(|t| *t == tab) {
                p.tabs.remove(i);
                sibling = p
                    .tabs
                    .get(i)
                    .or_else(|| i.checked_sub(1).and_then(|j| p.tabs.get(j)))
                    .copied();
            }
            fx.push(Effect::Emit(CoreMsg::ProjectUpdated(project_info(p))));
        }
        if self.active_tab == Some(tab) {
            let next = sibling.or_else(|| self.tabs.first().map(|t| t.id));
            self.set_active(next, fx);
        }
        Ok(())
    }

    /// Removes a project with its tabs, stopping their panes. A selected tab
    /// among them gives way to a tab of the nearest other project, preferring
    /// one that is not folded.
    fn close_project(&mut self, project: ProjectId, fx: &mut Vec<Effect>) -> Result<(), CoreError> {
        let at = self
            .projects
            .iter()
            .position(|p| p.id == project)
            .ok_or(CoreError::UnknownProject(project))?;
        let removed = self.projects.remove(at);
        for tab in &removed.tabs {
            let Some(i) = self.tabs.iter().position(|t| t.id == *tab) else {
                continue;
            };
            let closed = self.tabs.remove(i);
            for pane in closed.panes() {
                self.panes.remove(&pane);
                fx.push(Effect::Stop { pane });
            }
            fx.push(Effect::Emit(CoreMsg::TabClosed { tab: *tab }));
        }
        fx.push(Effect::Emit(CoreMsg::ProjectClosed { project }));
        if self.active_tab.is_some_and(|t| removed.tabs.contains(&t)) {
            let (before, after) = self.projects.split_at(at);
            let nearest: Vec<&Project> = after.iter().chain(before.iter().rev()).collect();
            let first_tab = |unfolded_only: bool| {
                nearest
                    .iter()
                    .filter(|p| !unfolded_only || !p.collapsed)
                    .find_map(|p| p.tabs.first().copied())
            };
            let next = first_tab(true).or_else(|| first_tab(false));
            self.set_active(next, fx);
        }
        Ok(())
    }

    pub fn set_window_focus(&mut self, focused: bool) -> Vec<Effect> {
        self.window_focused = focused;
        let mut fx = Vec::new();
        self.refresh_views(&mut fx);
        fx
    }

    fn refresh_views(&mut self, fx: &mut Vec<Effect>) {
        let active = self
            .active_tab
            .and_then(|id| self.tabs.iter().find(|t| t.id == id));
        let focus = self.window_focused;
        let views: Vec<(PaneId, bool, bool)> = self
            .panes
            .keys()
            .map(|&pane| {
                let visible = active.is_some_and(|t| {
                    self.panes.get(&pane).is_some_and(|p| p.tab == t.id)
                        && t.maximized.is_none_or(|m| m == pane)
                });
                let seen = visible && focus && active.is_some_and(|t| t.focused == pane);
                (pane, visible, seen)
            })
            .collect();
        for (pane, visible, seen) in views {
            if let Some(p) = self.panes.get_mut(&pane)
                && (p.visible, p.seen) != (visible, seen)
            {
                p.visible = visible;
                p.seen = seen;
                fx.push(Effect::View {
                    pane,
                    visible,
                    seen,
                });
            }
        }
    }

    /// Records what a pane actor reported. Reports for a pane that is gone are
    /// stale and change nothing.
    pub fn on_pane_event(&mut self, pane: PaneId, event: PaneEvent) -> Vec<Effect> {
        let mut fx = Vec::new();
        let Some(p) = self.panes.get_mut(&pane) else {
            return fx;
        };
        match event {
            PaneEvent::State(state, since) => {
                p.state = state;
                let since_changed = p.attention_since != since;
                p.attention_since = since;
                fx.push(Effect::Emit(CoreMsg::PaneState { pane, state }));
                if since_changed {
                    self.emit_pane(pane, &mut fx);
                }
            }
            PaneEvent::Question(question) => {
                if p.question != question {
                    p.question = question;
                    self.emit_pane(pane, &mut fx);
                }
            }
            PaneEvent::SignalLost(lost) => {
                p.signal_lost = lost;
                self.emit_pane(pane, &mut fx);
            }
            PaneEvent::Title(title) => {
                p.title = title;
                self.emit_pane(pane, &mut fx);
            }
            PaneEvent::Cwd(cwd) => {
                if p.cwd.as_deref() != Some(cwd.as_str()) {
                    p.cwd = Some(cwd);
                    self.emit_pane(pane, &mut fx);
                }
            }
            PaneEvent::SessionId { launch, .. } | PaneEvent::EndedByUser { launch }
                if launch != p.launch => {}
            PaneEvent::SessionId { id, .. } => {
                p.session_id = Some(id);
                self.emit_pane(pane, &mut fx);
            }
            PaneEvent::EndedByUser { .. } => p.ended_explicitly = true,
            PaneEvent::Started => {
                p.running = true;
                p.stopped = false;
                p.exit_code = None;
                self.emit_pane(pane, &mut fx);
            }
            PaneEvent::Exited(code) => {
                p.running = false;
                p.stopped = true;
                p.exit_code = code;
                fx.push(Effect::Emit(CoreMsg::PaneExited { pane, code }));
                self.emit_pane(pane, &mut fx);
            }
            PaneEvent::Failed(message) => {
                p.running = false;
                p.stopped = true;
                p.exit_code = None;
                fx.push(Effect::Emit(CoreMsg::Error {
                    pane: Some(pane),
                    message,
                }));
                fx.push(Effect::Emit(CoreMsg::PaneExited { pane, code: None }));
                self.emit_pane(pane, &mut fx);
            }
            PaneEvent::Selection(text) => {
                fx.push(Effect::Emit(CoreMsg::SelectionText { pane, text }));
            }
            PaneEvent::Notice(message) => {
                fx.push(Effect::Emit(CoreMsg::Error {
                    pane: Some(pane),
                    message,
                }));
            }
            PaneEvent::Standin(standin) => {
                p.standin = standin;
                self.emit_pane(pane, &mut fx);
            }
            PaneEvent::Crashed => {
                p.running = false;
                p.stopped = true;
                p.exit_code = None;
                // The restarted actor runs the pane's own program again.
                p.standin = None;
                // The restarted actor starts idle and reports no change, so a
                // red mark would otherwise outlive the agent that raised it.
                p.attention_since = None;
                p.question = None;
                if p.state != AgentState::Idle {
                    p.state = AgentState::Idle;
                    fx.push(Effect::Emit(CoreMsg::PaneState {
                        pane,
                        state: AgentState::Idle,
                    }));
                }
                fx.push(Effect::Emit(CoreMsg::Error {
                    pane: Some(pane),
                    message: "This pane stopped after an internal error. Relaunch it to start \
                              its program again."
                        .to_owned(),
                }));
                fx.push(Effect::Emit(CoreMsg::PaneExited { pane, code: None }));
                self.emit_pane(pane, &mut fx);
            }
        }
        fx
    }

    /// The plan's invariants (§6.2), as a description of the first one broken.
    pub fn check(&self) -> Result<(), String> {
        let mut owned = BTreeMap::new();
        for tab in &self.tabs {
            let mut leaves = tab.tree.panes();
            let mut mapped: Vec<_> = tab.leaves.keys().copied().collect();
            leaves.sort_unstable();
            mapped.sort_unstable();
            if leaves != mapped {
                return Err(format!("tab {:?}: leaves and panes differ", tab.id));
            }
            for pane in tab.leaves.values() {
                if owned.insert(*pane, tab.id).is_some() {
                    return Err(format!("pane {pane:?} appears twice"));
                }
                if self.panes.get(pane).map(|p| p.tab) != Some(tab.id) {
                    return Err(format!("pane {pane:?} of tab {:?} is unknown", tab.id));
                }
            }
            if !tab.leaves.values().any(|p| *p == tab.focused) {
                return Err(format!("tab {:?} focuses a foreign pane", tab.id));
            }
            if tab
                .maximized
                .is_some_and(|m| !tab.leaves.values().any(|p| *p == m))
            {
                return Err(format!("tab {:?} maximizes a foreign pane", tab.id));
            }
            let listed = self
                .projects
                .iter()
                .filter(|p| p.id == tab.project)
                .flat_map(|p| &p.tabs)
                .filter(|t| **t == tab.id)
                .count();
            if listed != 1 {
                return Err(format!("tab {:?} is listed {listed} times", tab.id));
            }
        }
        if let Some(orphan) = self.panes.keys().find(|p| !owned.contains_key(p)) {
            return Err(format!("pane {orphan:?} belongs to no tab"));
        }
        for project in &self.projects {
            if let Some(t) = project.tabs.iter().find(|t| self.tab(**t).is_err()) {
                return Err(format!("project {:?} lists unknown tab {t:?}", project.id));
            }
        }
        match self.active_tab {
            None if !self.tabs.is_empty() => return Err("no tab is selected".into()),
            Some(t) if self.tab(t).is_err() => return Err(format!("unknown tab {t:?} selected")),
            _ => {}
        }
        let seen: Vec<_> = self.panes.iter().filter(|(_, p)| p.seen).collect();
        if seen.len() > 1 {
            return Err("more than one pane is seen".into());
        }
        Ok(())
    }
}

fn folder_name(root: &str) -> String {
    root.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|n| !n.is_empty())
        .unwrap_or(root)
        .to_owned()
}

fn project_info(p: &Project) -> ProjectInfo {
    ProjectInfo {
        id: p.id,
        name: p.name.clone(),
        root: p.root.clone(),
        tabs: p.tabs.clone(),
        collapsed: p.collapsed,
    }
}

fn pane_info(id: PaneId, p: &Pane) -> PaneInfo {
    let (default_name, profile_name) = match &p.standin {
        Some((name, profile)) => (name, profile),
        None => (&p.default_name, &p.profile_name),
    };
    PaneInfo {
        id,
        tab: p.tab,
        name: p
            .custom_name
            .clone()
            .unwrap_or_else(|| default_name.clone()),
        profile: profile_name.clone(),
        profile_id: ProfileId::new(p.profile.clone()),
        title: p.title.clone(),
        cwd: p.cwd.clone(),
        state: p.state,
        running: p.running,
        exit_code: p.exit_code,
        session_id: p.session_id.clone(),
        needs_you_since: p.attention_since.filter(|_| p.state.is_red()),
        attention_since: p.attention_since,
        question: p.question.clone().filter(|_| p.state.is_red()),
        signal_lost: p.signal_lost,
        runs_session: p.runs_session(),
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use yien_proto::{Direction, DropZone, LayoutNode, Orientation};

    use super::*;
    use crate::launch;
    use crate::shell::ShellCommand;

    fn resolve(profile: Option<&ProfileId>) -> Result<LaunchPlan, CoreError> {
        launch::resolve(profile, &[], &ShellCommand::new("sh"))
    }

    fn run(ws: &mut Workspace, command: Command) -> Vec<Effect> {
        ws.apply(command, &resolve).unwrap()
    }

    fn spawned(fx: &[Effect]) -> Vec<PaneId> {
        fx.iter()
            .filter_map(|e| match e {
                Effect::Spawn { pane, .. } => Some(*pane),
                _ => None,
            })
            .collect()
    }

    fn stopped(fx: &[Effect]) -> Vec<PaneId> {
        fx.iter()
            .filter_map(|e| match e {
                Effect::Stop { pane } => Some(*pane),
                _ => None,
            })
            .collect()
    }

    fn project(ws: &mut Workspace) -> (TabId, PaneId) {
        let fx = run(
            ws,
            Command::NewProject {
                root: "C:/Code/yien/".into(),
                name: None,
            },
        );
        let pane = spawned(&fx)[0];
        (ws.tab_of(pane).unwrap(), pane)
    }

    #[test]
    fn rf_esp_01_new_project_opens_a_tab_on_its_folder() {
        let mut ws = Workspace::new();
        let fx = run(
            &mut ws,
            Command::NewProject {
                root: "C:/Code/yien/".into(),
                name: None,
            },
        );
        let snap = ws.snapshot();
        assert_eq!(snap.projects[0].name, "yien");
        assert_eq!(snap.tabs[0].dir, "C:/Code/yien/");
        assert_eq!(snap.active_tab, Some(snap.tabs[0].id));
        let spawn = fx.iter().find_map(|e| match e {
            Effect::Spawn { cwd, .. } => Some(cwd.clone()),
            _ => None,
        });
        assert_eq!(spawn.as_deref(), Some("C:/Code/yien/"));
    }

    #[test]
    fn rf_esp_03_tabs_open_and_close_with_their_panes() {
        let mut ws = Workspace::new();
        let (first, _) = project(&mut ws);
        let project_id = ws.snapshot().projects[0].id;
        let fx = run(
            &mut ws,
            Command::NewTab {
                project: project_id,
                name: Some("review".into()),
                profile: None,
            },
        );
        let pane = spawned(&fx)[0];
        let second = ws.tab_of(pane).unwrap();
        assert_eq!(ws.snapshot().projects[0].tabs, [first, second]);
        assert_eq!(ws.snapshot().active_tab, Some(second));

        let fx = run(&mut ws, Command::CloseTab { tab: second });
        assert_eq!(stopped(&fx), [pane]);
        assert_eq!(ws.snapshot().active_tab, Some(first));
        assert!(ws.snapshot().panes.iter().all(|p| p.id != pane));
    }

    #[test]
    fn rf_pan_01_split_adds_a_focused_pane_after_the_target() {
        let mut ws = Workspace::new();
        let (tab, first) = project(&mut ws);
        let fx = run(
            &mut ws,
            Command::SplitPane {
                pane: first,
                orientation: Orientation::Horizontal,
                profile: None,
            },
        );
        let new = spawned(&fx)[0];
        let info = ws.tab_info(ws.tab(tab).unwrap());
        assert_eq!(info.layout.panes(), [first, new]);
        assert_eq!(info.focused, new);
    }

    #[test]
    fn rf_pan_20_closing_the_last_pane_closes_its_tab() {
        let mut ws = Workspace::new();
        let (tab, pane) = project(&mut ws);
        let fx = run(&mut ws, Command::ClosePane { pane });
        assert_eq!(stopped(&fx), [pane]);
        assert!(ws.tab(tab).is_err());
        assert_eq!(ws.snapshot().active_tab, None);
    }

    #[test]
    fn rf_pan_03_focus_moves_by_direction() {
        let mut ws = Workspace::new();
        let (tab, left) = project(&mut ws);
        let fx = run(
            &mut ws,
            Command::SplitPane {
                pane: left,
                orientation: Orientation::Horizontal,
                profile: None,
            },
        );
        let right = spawned(&fx)[0];
        run(
            &mut ws,
            Command::FocusDirection {
                direction: Direction::Left,
            },
        );
        assert_eq!(ws.tab(tab).unwrap().focused, left);
        run(
            &mut ws,
            Command::FocusDirection {
                direction: Direction::Up,
            },
        );
        assert_eq!(ws.tab(tab).unwrap().focused, left);
        run(&mut ws, Command::FocusPane { pane: right });
        assert_eq!(ws.tab(tab).unwrap().focused, right);
    }

    #[test]
    fn rf_pan_04_maximize_hides_the_others_and_restores() {
        let mut ws = Workspace::new();
        ws.set_window_focus(true);
        let (_, a) = project(&mut ws);
        let fx = run(
            &mut ws,
            Command::SplitPane {
                pane: a,
                orientation: Orientation::Vertical,
                profile: None,
            },
        );
        let b = spawned(&fx)[0];
        let fx = run(&mut ws, Command::ToggleMaximize { pane: a });
        assert!(fx.iter().any(|e| matches!(
            e,
            Effect::View { pane, visible: false, .. } if *pane == b
        )));
        assert!(ws.panes[&a].seen);
        run(&mut ws, Command::ToggleMaximize { pane: a });
        assert!(ws.panes[&b].visible);
    }

    #[test]
    fn rf_pan_05_swap_exchanges_places_and_keeps_programs() {
        let mut ws = Workspace::new();
        let (tab, a) = project(&mut ws);
        let fx = run(
            &mut ws,
            Command::SplitPane {
                pane: a,
                orientation: Orientation::Horizontal,
                profile: None,
            },
        );
        let b = spawned(&fx)[0];
        let fx = run(&mut ws, Command::SwapPanes { a, b });
        assert!(spawned(&fx).is_empty() && stopped(&fx).is_empty());
        assert_eq!(ws.tab_info(ws.tab(tab).unwrap()).layout.panes(), [b, a]);
    }

    #[test]
    fn rf_pan_05_move_docks_beside_the_target() {
        let mut ws = Workspace::new();
        let (tab, a) = project(&mut ws);
        let fx = run(
            &mut ws,
            Command::SplitPane {
                pane: a,
                orientation: Orientation::Horizontal,
                profile: None,
            },
        );
        let b = spawned(&fx)[0];
        run(
            &mut ws,
            Command::MovePane {
                pane: b,
                target: a,
                zone: DropZone::Top,
            },
        );
        let layout = ws.tab_info(ws.tab(tab).unwrap()).layout;
        assert!(matches!(
            layout,
            LayoutNode::Split {
                orientation: Orientation::Vertical,
                ..
            }
        ));
        assert_eq!(layout.panes(), [b, a]);
    }

    #[test]
    fn rf_pan_02_resize_changes_the_flex() {
        let mut ws = Workspace::new();
        let (tab, a) = project(&mut ws);
        run(
            &mut ws,
            Command::SplitPane {
                pane: a,
                orientation: Orientation::Horizontal,
                profile: None,
            },
        );
        let fx = run(
            &mut ws,
            Command::ResizeSplit {
                pane: a,
                delta: 0.2,
            },
        );
        assert!(
            fx.iter()
                .any(|e| matches!(e, Effect::Emit(CoreMsg::TabUpdated(_))))
        );
        let LayoutNode::Split { children, .. } = ws.tab_info(ws.tab(tab).unwrap()).layout else {
            panic!("expected a split");
        };
        assert!((children[0].flex - 0.7).abs() < 0.01);
        let fx = run(
            &mut ws,
            Command::ResizeSplit {
                pane: a,
                delta: f32::NAN,
            },
        );
        assert!(fx.is_empty());
    }

    #[test]
    fn rf_pan_20_rename_and_back_to_the_default_name() {
        let mut ws = Workspace::new();
        let (_, a) = project(&mut ws);
        run(
            &mut ws,
            Command::RenamePane {
                pane: a,
                name: "server".into(),
            },
        );
        assert_eq!(ws.snapshot().panes[0].name, "server");
        run(
            &mut ws,
            Command::RenamePane {
                pane: a,
                name: " ".into(),
            },
        );
        assert_eq!(ws.snapshot().panes[0].name, "sh");
    }

    #[test]
    fn failed_commands_change_nothing() {
        let mut ws = Workspace::new();
        let (_, a) = project(&mut ws);
        let before = ws.snapshot();
        let unknown = PaneId::new(999);
        let bad = [
            Command::ClosePane { pane: unknown },
            Command::SplitPane {
                pane: a,
                orientation: Orientation::Vertical,
                profile: Some(ProfileId::new("missing")),
            },
            Command::SwapPanes { a, b: unknown },
            Command::SelectTab {
                tab: TabId::new(999),
            },
        ];
        for command in bad {
            assert!(ws.apply(command, &resolve).is_err());
            assert_eq!(ws.snapshot(), before);
        }
    }

    #[test]
    fn rn_07_only_the_focused_pane_of_the_active_tab_in_a_focused_window_is_seen() {
        let mut ws = Workspace::new();
        let (_, a) = project(&mut ws);
        assert!(!ws.panes[&a].seen);
        let fx = ws.set_window_focus(true);
        assert!(matches!(
            fx[..],
            [Effect::View { pane, visible: true, seen: true }] if pane == a
        ));
        let fx = ws.set_window_focus(false);
        assert!(matches!(fx[..], [Effect::View { seen: false, .. }]));
    }

    #[test]
    fn rf_pan_19_a_crash_keeps_the_view_the_pane_had() {
        let mut ws = Workspace::new();
        ws.set_window_focus(true);
        let (_, a) = project(&mut ws);
        let fx = ws.on_pane_event(a, PaneEvent::Crashed);
        assert!(!fx.iter().any(|e| matches!(e, Effect::View { .. })));
        assert!(fx.iter().any(
            |e| matches!(e, Effect::Emit(CoreMsg::PaneUpdated(p)) if p.id == a && !p.running)
        ));
        assert!(ws.panes[&a].visible && ws.panes[&a].seen);
        let fx = run(&mut ws, Command::RelaunchPane { pane: a });
        assert!(
            matches!(fx[0], Effect::Relaunch { pane, launch: 1 } if pane == a),
            "{fx:?}"
        );
    }

    #[test]
    fn rf_ses_11_a_crash_drops_the_fallback_shell_name() {
        let mut ws = Workspace::new();
        let (_, a) = project(&mut ws);
        let own = ws.panes[&a].profile_name.clone();
        ws.on_pane_event(
            a,
            PaneEvent::Standin(Some(("Shell".to_owned(), "Fallback".to_owned()))),
        );
        assert_eq!(pane_info(a, &ws.panes[&a]).profile, "Fallback");
        ws.on_pane_event(a, PaneEvent::Crashed);
        assert_eq!(pane_info(a, &ws.panes[&a]).profile, own);
    }

    #[test]
    fn rn_02_a_crashed_actor_takes_its_red_mark_with_it() {
        let mut ws = Workspace::new();
        let (_, a) = project(&mut ws);
        let needs = AgentState::NeedsYou(yien_proto::NeedsReason::Permission);
        ws.on_pane_event(a, PaneEvent::State(needs, Some(3)));
        let fx = ws.on_pane_event(a, PaneEvent::Crashed);
        assert!(fx.iter().any(|e| matches!(
            e,
            Effect::Emit(CoreMsg::PaneState { pane, state: AgentState::Idle }) if *pane == a
        )));
        assert!(fx.iter().any(|e| matches!(
            e,
            Effect::Emit(CoreMsg::PaneUpdated(p))
                if p.id == a && p.state == AgentState::Idle && p.needs_you_since.is_none()
        )));
    }

    #[test]
    fn an_exit_updates_the_pane() {
        let mut ws = Workspace::new();
        let (_, a) = project(&mut ws);
        ws.on_pane_event(a, PaneEvent::Started);
        let fx = ws.on_pane_event(a, PaneEvent::Exited(Some(2)));
        assert!(fx.iter().any(|e| matches!(
            e,
            Effect::Emit(CoreMsg::PaneUpdated(p)) if p.id == a && !p.running && p.exit_code == Some(2)
        )));
    }

    #[test]
    fn a_new_tab_is_announced_before_its_pane_and_started_before_both() {
        let mut ws = Workspace::new();
        let fx = run(
            &mut ws,
            Command::NewProject {
                root: "C:/p".into(),
                name: None,
            },
        );
        let spawn = fx.iter().position(|e| matches!(e, Effect::Spawn { .. }));
        let tab = fx
            .iter()
            .position(|e| matches!(e, Effect::Emit(CoreMsg::TabUpdated(_))));
        let pane = fx
            .iter()
            .position(|e| matches!(e, Effect::Emit(CoreMsg::PaneUpdated(_))));
        assert!(spawn.is_some() && spawn < tab && tab < pane, "{fx:?}");
    }

    #[test]
    fn a_split_announces_the_layout_before_the_new_pane() {
        let mut ws = Workspace::new();
        let (_, a) = project(&mut ws);
        let fx = run(
            &mut ws,
            Command::SplitPane {
                pane: a,
                orientation: Orientation::Horizontal,
                profile: None,
            },
        );
        let spawn = fx.iter().position(|e| matches!(e, Effect::Spawn { .. }));
        let tab = fx
            .iter()
            .position(|e| matches!(e, Effect::Emit(CoreMsg::TabUpdated(_))));
        let pane = fx
            .iter()
            .position(|e| matches!(e, Effect::Emit(CoreMsg::PaneUpdated(_))));
        assert!(spawn.is_some() && spawn < tab && tab < pane, "{fx:?}");
    }

    #[test]
    fn rf_atn_06_selecting_a_tab_focuses_the_pane_that_needs_you_longest() {
        let mut ws = Workspace::new();
        let (tab, a) = project(&mut ws);
        let split = |ws: &mut Workspace, pane| {
            let fx = run(
                ws,
                Command::SplitPane {
                    pane,
                    orientation: Orientation::Horizontal,
                    profile: None,
                },
            );
            spawned(&fx)[0]
        };
        let b = split(&mut ws, a);
        let c = split(&mut ws, b);
        let needs = AgentState::NeedsYou(yien_proto::NeedsReason::Permission);
        ws.on_pane_event(c, PaneEvent::State(needs, Some(4)));
        ws.on_pane_event(b, PaneEvent::State(needs, Some(9)));
        run(&mut ws, Command::FocusPane { pane: a });
        run(&mut ws, Command::SelectTab { tab });
        assert_eq!(ws.tab(tab).unwrap().focused, c);
        ws.on_pane_event(c, PaneEvent::State(AgentState::Working, None));
        run(&mut ws, Command::FocusPane { pane: a });
        run(&mut ws, Command::SelectTab { tab });
        assert_eq!(ws.tab(tab).unwrap().focused, b);
    }

    #[test]
    fn rf_atn_05_entering_needs_you_republishes_the_pane_with_its_order() {
        let mut ws = Workspace::new();
        let (_, a) = project(&mut ws);
        let needs = AgentState::NeedsYou(yien_proto::NeedsReason::Question);
        let fx = ws.on_pane_event(a, PaneEvent::State(needs, Some(1)));
        assert!(fx.iter().any(|e| matches!(
            e,
            Effect::Emit(CoreMsg::PaneUpdated(p)) if p.needs_you_since == Some(1) && p.state == needs
        )));
        let fx = ws.on_pane_event(a, PaneEvent::State(AgentState::Working, None));
        assert!(fx.iter().any(|e| matches!(
            e,
            Effect::Emit(CoreMsg::PaneUpdated(p)) if p.needs_you_since.is_none()
        )));
        let fx = ws.on_pane_event(a, PaneEvent::State(AgentState::Idle, None));
        assert!(
            !fx.iter()
                .any(|e| matches!(e, Effect::Emit(CoreMsg::PaneUpdated(_))))
        );
        let fx = ws.on_pane_event(a, PaneEvent::SignalLost(true));
        assert!(fx.iter().any(|e| matches!(
            e,
            Effect::Emit(CoreMsg::PaneUpdated(p)) if p.signal_lost
        )));
    }

    #[test]
    fn rf_atn_13_a_question_republishes_the_pane_and_goes_with_the_red() {
        let mut ws = Workspace::new();
        let (_, a) = project(&mut ws);
        let needs = AgentState::NeedsYou(yien_proto::NeedsReason::Permission);
        ws.on_pane_event(a, PaneEvent::State(needs, Some(2)));
        let asked = Some(PendingQuestion::new("Permission to use Bash"));
        let fx = ws.on_pane_event(a, PaneEvent::Question(asked.clone()));
        assert!(fx.iter().any(|e| matches!(
            e,
            Effect::Emit(CoreMsg::PaneUpdated(p)) if p.question == asked && p.attention_since == Some(2)
        )));
        let fx = ws.on_pane_event(a, PaneEvent::Question(asked.clone()));
        assert!(fx.is_empty(), "the same question again changes nothing");
        // A state report lands before the actor's own clearing question.
        let fx = ws.on_pane_event(a, PaneEvent::State(AgentState::Done, Some(5)));
        assert!(fx.iter().any(|e| matches!(
            e,
            Effect::Emit(CoreMsg::PaneUpdated(p))
                if p.question.is_none() && p.needs_you_since.is_none() && p.attention_since == Some(5)
        )));
        ws.on_pane_event(a, PaneEvent::State(needs, Some(6)));
        ws.on_pane_event(a, PaneEvent::Question(asked));
        let fx = ws.on_pane_event(a, PaneEvent::Crashed);
        assert!(fx.iter().any(|e| matches!(
            e,
            Effect::Emit(CoreMsg::PaneUpdated(p))
                if p.question.is_none() && p.attention_since.is_none()
        )));
    }

    #[test]
    fn exhausted_ids_fail_the_command_instead_of_reusing_one() {
        let mut ws = Workspace::new();
        let (_, a) = project(&mut ws);
        ws.next_id = u64::MAX;
        let before = ws.snapshot();
        let split = Command::SplitPane {
            pane: a,
            orientation: Orientation::Horizontal,
            profile: None,
        };
        assert!(matches!(
            ws.apply(split, &resolve),
            Err(CoreError::OutOfIds)
        ));
        assert_eq!(ws.snapshot(), before);
    }

    #[test]
    fn stale_pane_events_are_ignored() {
        let mut ws = Workspace::new();
        assert!(
            ws.on_pane_event(PaneId::new(5), PaneEvent::Started)
                .is_empty()
        );
    }

    #[test]
    fn rf_esp_04_a_branch_change_republishes_the_tab_once() {
        let mut ws = Workspace::new();
        let (tab, _) = project(&mut ws);
        let fx = ws.set_branch(tab, Some("main".into()));
        assert!(matches!(
            fx.as_slice(),
            [Effect::Emit(CoreMsg::TabUpdated(t))] if t.branch.as_deref() == Some("main")
        ));
        assert!(ws.set_branch(tab, Some("main".into())).is_empty());
        let fx = ws.set_branch(tab, None);
        assert!(matches!(
            fx.as_slice(),
            [Effect::Emit(CoreMsg::TabUpdated(t))] if t.branch.is_none()
        ));
        assert!(ws.set_branch(TabId::new(99), Some("x".into())).is_empty());
        assert_eq!(ws.active_dir(), Some((tab, "C:/Code/yien/")));
    }

    fn emitted(fx: &[Effect]) -> Vec<&CoreMsg> {
        fx.iter()
            .filter_map(|e| match e {
                Effect::Emit(msg) => Some(msg),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn rf_esp_01_adding_a_folder_that_is_a_project_selects_it() {
        let mut ws = Workspace::new();
        let (first, _) = project(&mut ws);
        let other = run(
            &mut ws,
            Command::NewProject {
                root: "C:/Code/other".into(),
                name: None,
            },
        );
        assert_eq!(spawned(&other).len(), 1);
        let fx = run(
            &mut ws,
            Command::NewProject {
                root: "C:/Code//yien".into(),
                name: None,
            },
        );
        assert!(spawned(&fx).is_empty(), "{fx:?}");
        assert_eq!(ws.snapshot().projects.len(), 2);
        assert_eq!(ws.snapshot().active_tab, Some(first));

        run(&mut ws, Command::CloseTab { tab: first });
        let fx = run(
            &mut ws,
            Command::NewProject {
                root: "C:/Code/yien".into(),
                name: None,
            },
        );
        let pane = spawned(&fx)[0];
        let snap = ws.snapshot();
        assert_eq!(snap.projects.len(), 2);
        assert_eq!(snap.projects[0].tabs, [ws.tab_of(pane).unwrap()]);
        assert_eq!(snap.active_tab, Some(ws.tab_of(pane).unwrap()));
    }

    #[test]
    fn closing_a_project_stops_its_panes_and_selects_another() {
        let mut ws = Workspace::new();
        let (tab, a) = project(&mut ws);
        let fx = run(
            &mut ws,
            Command::SplitPane {
                pane: a,
                orientation: Orientation::Horizontal,
                profile: None,
            },
        );
        let b = spawned(&fx)[0];
        let id = ws.snapshot().projects[0].id;
        let fx = run(
            &mut ws,
            Command::NewProject {
                root: "C:/Code/other".into(),
                name: None,
            },
        );
        let other = ws.tab_of(spawned(&fx)[0]).unwrap();
        run(&mut ws, Command::SelectTab { tab });

        let fx = run(&mut ws, Command::CloseProject { project: id });
        let mut gone = stopped(&fx);
        gone.sort_unstable();
        assert_eq!(gone, [a, b]);
        let msgs = emitted(&fx);
        let closed = msgs
            .iter()
            .position(|m| matches!(m, CoreMsg::TabClosed { tab: t } if *t == tab));
        let project_closed = msgs
            .iter()
            .position(|m| matches!(m, CoreMsg::ProjectClosed { project } if *project == id));
        let active = msgs
            .iter()
            .position(|m| matches!(m, CoreMsg::ActiveTab { tab: Some(t) } if *t == other));
        assert!(
            closed.is_some() && closed < project_closed && project_closed < active,
            "{msgs:?}"
        );
        assert!(
            !msgs
                .iter()
                .any(|m| matches!(m, CoreMsg::ProjectUpdated(p) if p.id == id))
        );
        let snap = ws.snapshot();
        assert_eq!(snap.projects.len(), 1);
        assert_eq!(snap.active_tab, Some(other));
        assert_eq!(ws.check(), Ok(()));
    }

    #[test]
    fn closing_the_last_project_leaves_nothing_selected() {
        let mut ws = Workspace::new();
        project(&mut ws);
        let id = ws.snapshot().projects[0].id;
        let fx = run(&mut ws, Command::CloseProject { project: id });
        assert!(
            emitted(&fx)
                .iter()
                .any(|m| matches!(m, CoreMsg::ActiveTab { tab: None }))
        );
        let snap = ws.snapshot();
        assert!(snap.projects.is_empty() && snap.tabs.is_empty() && snap.panes.is_empty());
        assert!(
            ws.apply(Command::CloseProject { project: id }, &resolve)
                .is_err()
        );
    }

    #[test]
    fn rf_esp_16_a_renamed_tab_goes_back_to_its_automatic_name() {
        let mut ws = Workspace::new();
        let (tab, _) = project(&mut ws);
        let name = |ws: &Workspace| ws.snapshot().tabs[0].name.clone();
        assert_eq!(name(&ws), "Tab 1");
        let fx = run(
            &mut ws,
            Command::RenameTab {
                tab,
                name: "  review  ".into(),
            },
        );
        assert!(matches!(
            emitted(&fx)[..],
            [CoreMsg::TabUpdated(t)] if t.name == "review"
        ));
        run(
            &mut ws,
            Command::RenameTab {
                tab,
                name: " ".into(),
            },
        );
        assert_eq!(name(&ws), "Tab 1");
        run(
            &mut ws,
            Command::RenameTab {
                tab,
                name: "Tab 1".into(),
            },
        );
        let saved = ws.to_session();
        assert_eq!(
            saved.projects[0].tabs[0].name, None,
            "the automatic name is not pinned as the tab's own"
        );
        let unknown = Command::RenameTab {
            tab: TabId::new(99),
            name: "x".into(),
        };
        assert!(ws.apply(unknown, &resolve).is_err());
    }

    fn new_tab(ws: &mut Workspace, project: ProjectId) -> TabId {
        let fx = run(
            ws,
            Command::NewTab {
                project,
                name: None,
                profile: None,
            },
        );
        ws.tab_of(spawned(&fx)[0]).unwrap()
    }

    fn move_to(tab: TabId, project: ProjectId, to_index: u32) -> Command {
        Command::MoveTab {
            tab,
            project,
            to_index,
        }
    }

    #[test]
    fn rf_esp_16_a_moved_tab_takes_its_new_place_in_its_project() {
        let mut ws = Workspace::new();
        let (a, _) = project(&mut ws);
        let p = ws.snapshot().projects[0].id;
        let b = new_tab(&mut ws, p);
        let c = new_tab(&mut ws, p);
        let selected = ws.snapshot().active_tab;
        let fx = run(&mut ws, move_to(c, p, 0));
        assert!(matches!(
            emitted(&fx)[..],
            [CoreMsg::ProjectUpdated(info)] if info.tabs == [c, a, b]
        ));
        assert_eq!(ws.snapshot().active_tab, selected, "moving never selects");
        run(&mut ws, move_to(c, p, 2));
        assert_eq!(ws.snapshot().projects[0].tabs, [a, b, c]);
        run(&mut ws, move_to(a, p, 1));
        assert_eq!(ws.snapshot().projects[0].tabs, [b, a, c]);
        // Past the last tab means last.
        run(&mut ws, move_to(b, p, u32::MAX));
        assert_eq!(ws.snapshot().projects[0].tabs, [a, c, b]);
        // Its own place again changes nothing and announces nothing.
        assert!(emitted(&run(&mut ws, move_to(b, p, 9))).is_empty());
        assert!(ws.apply(move_to(TabId::new(99), p, 0), &resolve).is_err());
    }

    #[test]
    fn rf_esp_16_a_tab_never_moves_to_another_project() {
        let mut ws = Workspace::new();
        let (a, _) = project(&mut ws);
        run(
            &mut ws,
            Command::NewProject {
                root: "D:/other".into(),
                name: None,
            },
        );
        let snap = ws.snapshot();
        let (first, second) = (snap.projects[0].id, snap.projects[1].id);
        let before = snap.projects;
        let refused = ws.apply(move_to(a, second, 0), &resolve);
        assert!(matches!(refused, Err(CoreError::OtherProject(t, p)) if t == a && p == second));
        assert_eq!(ws.snapshot().projects, before);
        let unknown = ws.apply(move_to(a, ProjectId::new(99), 0), &resolve);
        assert!(unknown.is_err());
        assert!(ws.apply(move_to(a, first, 0), &resolve).is_ok());
    }

    #[test]
    fn rf_ses_02_a_moved_tab_keeps_its_place_after_a_restart() {
        let mut ws = Workspace::new();
        let (a, _) = project(&mut ws);
        let p = ws.snapshot().projects[0].id;
        let b = new_tab(&mut ws, p);
        let c = new_tab(&mut ws, p);
        run(&mut ws, move_to(c, p, 0));
        run(&mut ws, move_to(a, p, 2));
        let mut store = yien_session::MemStore::new();
        yien_session::save(&mut store, &ws.to_session()).unwrap();
        let loaded = yien_session::load(&store).restored.unwrap().snapshot;
        let disk = Disk {
            existing: folders(&loaded).into_iter().collect(),
            home: None,
        };
        let mut back = Workspace::new();
        let (_, notes) = back.restore(&loaded, &resolve, &disk);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(back.snapshot().projects[0].tabs, [c, b, a]);
    }

    #[test]
    fn rf_esp_17_a_folded_project_unfolds_when_one_of_its_tabs_is_selected() {
        let mut ws = Workspace::new();
        let (tab, a) = project(&mut ws);
        let id = ws.snapshot().projects[0].id;
        let fold = |collapsed| Command::SetProjectCollapsed {
            project: id,
            collapsed,
        };
        let fx = run(&mut ws, fold(true));
        assert!(matches!(
            emitted(&fx)[..],
            [CoreMsg::ProjectUpdated(p)] if p.collapsed
        ));
        assert!(run(&mut ws, fold(true)).is_empty());
        let needs = AgentState::NeedsYou(yien_proto::NeedsReason::Permission);
        ws.on_pane_event(a, PaneEvent::State(needs, Some(1)));
        assert!(ws.snapshot().projects[0].collapsed, "red does not unfold");

        run(&mut ws, Command::SelectTab { tab });
        assert!(!ws.snapshot().projects[0].collapsed);
        run(&mut ws, fold(true));
        run(&mut ws, Command::FocusPane { pane: a });
        assert!(
            ws.snapshot().projects[0].collapsed,
            "clicking a pane of the selected tab keeps its project folded"
        );
        let unknown = Command::SetProjectCollapsed {
            project: ProjectId::new(99),
            collapsed: true,
        };
        assert!(ws.apply(unknown, &resolve).is_err());
    }

    const SESSION: &str = "4f1c2a3b-0000-4000-8000-0123456789ab";

    fn with_agents(profile: Option<&ProfileId>) -> Result<LaunchPlan, CoreError> {
        let profiles = yien_agent::builtin_profiles().unwrap();
        launch::resolve(profile, &profiles, &ShellCommand::new("sh"))
    }

    fn resume(project: ProjectId, profile: &str, id: &str) -> Command {
        Command::ResumeAgentSession {
            project,
            profile: ProfileId::new(profile),
            session_id: id.into(),
        }
    }

    #[test]
    fn rf_ses_07_listing_asks_for_the_projects_folder() {
        let mut ws = Workspace::new();
        project(&mut ws);
        let id = ws.snapshot().projects[0].id;
        let fx = run(
            &mut ws,
            Command::ListAgentSessions {
                project: id,
                request: 3,
            },
        );
        assert!(matches!(
            fx.as_slice(),
            [Effect::ListAgentSessions { project, request: 3, root }]
                if *project == id && root == "C:/Code/yien/"
        ));
        let gone = run(
            &mut ws,
            Command::ListAgentSessions {
                project: ProjectId::new(99),
                request: 4,
            },
        );
        assert!(matches!(
            gone.as_slice(),
            [Effect::Emit(CoreMsg::AgentSessions { request: 4, sessions, .. })]
                if sessions.is_empty()
        ));
    }

    #[test]
    fn rf_ses_07_a_past_session_resumes_in_a_new_tab_on_the_project_folder() {
        let mut ws = Workspace::new();
        let (first, _) = project(&mut ws);
        let id = ws.snapshot().projects[0].id;
        let fx = ws
            .apply(resume(id, "claude", SESSION), &with_agents)
            .unwrap();
        let (pane, plan, cwd, resumed) = fx
            .iter()
            .find_map(|e| match e {
                Effect::Spawn {
                    pane,
                    plan,
                    cwd,
                    resume,
                } => Some((*pane, plan, cwd.clone(), resume.clone())),
                _ => None,
            })
            .unwrap();
        assert_eq!(plan.profile, "claude");
        assert_eq!(cwd, "C:/Code/yien/");
        assert_eq!(resumed.as_deref(), Some(SESSION));
        let snap = ws.snapshot();
        assert_eq!(snap.tabs.len(), 2);
        let tab = ws.tab_of(pane).unwrap();
        assert_ne!(tab, first);
        assert_eq!(snap.active_tab, Some(tab));
        let info = snap.panes.iter().find(|p| p.id == pane).unwrap();
        assert_eq!(info.session_id.as_deref(), Some(SESSION));
        // Saved at once, so a quit before the CLI reports the id keeps it.
        let saved = ws.to_session();
        let record = saved.projects[0]
            .tabs
            .iter()
            .flat_map(|t| &t.panes)
            .find(|p| p.id == pane)
            .unwrap();
        let agent = record.agent.as_ref().unwrap();
        assert_eq!(agent.session_id.as_ref().map(|s| s.as_str()), Some(SESSION));
    }

    #[test]
    fn rf_ses_07_a_session_already_open_is_focused_not_resumed_twice() {
        let mut ws = Workspace::new();
        project(&mut ws);
        let id = ws.snapshot().projects[0].id;
        let fx = ws
            .apply(resume(id, "claude", SESSION), &with_agents)
            .unwrap();
        let pane = spawned(&fx)[0];
        let (other, _) = {
            let fx = run(
                &mut ws,
                Command::NewTab {
                    project: id,
                    name: None,
                    profile: None,
                },
            );
            let p = spawned(&fx)[0];
            (ws.tab_of(p).unwrap(), p)
        };
        assert_eq!(ws.snapshot().active_tab, Some(other));
        let upper = SESSION.to_uppercase();
        let fx = ws
            .apply(resume(id, "claude", &upper), &with_agents)
            .unwrap();
        assert!(spawned(&fx).is_empty());
        let snap = ws.snapshot();
        assert_eq!(snap.tabs.len(), 3);
        assert_eq!(snap.active_tab, Some(ws.tab_of(pane).unwrap()));
    }

    /// A workspace whose Claude pane resumed [`SESSION`] and then got
    /// `event`, and the resume command for it again.
    fn resumed_then(event: PaneEvent) -> (Workspace, PaneId, Command) {
        let mut ws = Workspace::new();
        project(&mut ws);
        let id = ws.snapshot().projects[0].id;
        let fx = ws
            .apply(resume(id, "claude", SESSION), &with_agents)
            .unwrap();
        let pane = spawned(&fx)[0];
        ws.on_pane_event(pane, PaneEvent::Started);
        ws.on_pane_event(pane, event);
        (ws, pane, resume(id, "claude", SESSION))
    }

    fn rf_ses_07_resumes_in_a_new_tab_and_takes_the_session_from(event: PaneEvent) {
        let (mut ws, old, again) = resumed_then(event);
        let info = |ws: &Workspace, pane| {
            ws.snapshot()
                .panes
                .into_iter()
                .find(|p| p.id == pane)
                .unwrap()
        };
        assert!(!info(&ws, old).runs_session);
        let fx = ws.apply(again, &with_agents).unwrap();
        let new = spawned(&fx);
        assert_eq!(new.len(), 1, "a new pane resumes it");
        assert_ne!(ws.tab_of(new[0]).unwrap(), ws.tab_of(old).unwrap());
        assert!(fx.iter().any(|e| matches!(
            e,
            Effect::Spawn { pane, resume: Some(s), .. } if *pane == new[0] && s == SESSION
        )));
        assert_eq!(info(&ws, new[0]).session_id.as_deref(), Some(SESSION));
        assert!(info(&ws, new[0]).runs_session);
        // The stopped pane gives the session up, so a restart resumes it
        // once.
        assert_eq!(info(&ws, old).session_id, None);
        let saved = ws.to_session();
        let holders = saved.projects[0]
            .tabs
            .iter()
            .flat_map(|t| &t.panes)
            .filter(|p| {
                p.agent
                    .as_ref()
                    .and_then(|a| a.session_id.as_ref())
                    .is_some_and(|s| s.as_str() == SESSION)
            })
            .count();
        assert_eq!(holders, 1);
    }

    #[test]
    fn rf_ses_07_a_session_whose_pane_exited_resumes_in_a_new_tab() {
        rf_ses_07_resumes_in_a_new_tab_and_takes_the_session_from(PaneEvent::Exited(Some(0)));
    }

    #[test]
    fn rf_ses_07_a_session_whose_pane_fell_back_to_a_shell_resumes_in_a_new_tab() {
        rf_ses_07_resumes_in_a_new_tab_and_takes_the_session_from(PaneEvent::Standin(Some((
            "pwsh".into(),
            "Shell".into(),
        ))));
    }

    #[test]
    fn rf_ses_07_a_session_whose_pane_runs_or_starts_counts_as_open() {
        let (mut ws, pane, again) = resumed_then(PaneEvent::Cwd("C:/Code/yien".into()));
        let fx = ws.apply(again.clone(), &with_agents).unwrap();
        assert!(spawned(&fx).is_empty());
        // Relaunched after an exit: it runs the session again once started.
        ws.on_pane_event(pane, PaneEvent::Exited(Some(0)));
        ws.on_pane_event(pane, PaneEvent::Started);
        let fx = ws.apply(again, &with_agents).unwrap();
        assert!(spawned(&fx).is_empty());
    }

    #[test]
    fn rf_ses_07_a_resume_that_cannot_work_changes_nothing() {
        let mut ws = Workspace::new();
        project(&mut ws);
        let id = ws.snapshot().projects[0].id;
        let before = ws.snapshot();
        for (command, why) in [
            (resume(id, "claude", "--settings"), "not an id"),
            (resume(id, "claude", ""), "empty"),
            (resume(id, "shell", SESSION), "the shell cannot resume"),
            (resume(id, "aider", SESSION), "unknown profile"),
            (resume(ProjectId::new(99), "claude", SESSION), "no project"),
        ] {
            assert!(ws.apply(command, &with_agents).is_err(), "{why}");
        }
        assert_eq!(ws.snapshot(), before);
        assert!(matches!(
            ws.apply(resume(id, "claude", "x"), &with_agents),
            Err(CoreError::InvalidSessionId)
        ));
        assert!(matches!(
            ws.apply(resume(id, "shell", SESSION), &with_agents),
            Err(CoreError::CannotResume(_))
        ));
    }

    #[test]
    fn rf_atn_11_mutes_are_kept_and_reported_once() {
        let mut ws = Workspace::new();
        let (tab, _) = project(&mut ws);
        let mute = |muted| Command::SetTabMuted { tab, muted };
        let fx = run(&mut ws, mute(true));
        assert!(matches!(emitted(&fx)[..], [CoreMsg::TabUpdated(t)] if t.muted));
        assert!(run(&mut ws, mute(true)).is_empty());
        assert!(ws.snapshot().tabs[0].muted);
        let fx = run(&mut ws, mute(false));
        assert!(matches!(emitted(&fx)[..], [CoreMsg::TabUpdated(t)] if !t.muted));

        let all = |muted| Command::SetNotificationsMuted { muted };
        let fx = run(&mut ws, all(true));
        assert!(matches!(
            emitted(&fx)[..],
            [CoreMsg::NotificationsMuted { muted: true }]
        ));
        assert!(run(&mut ws, all(true)).is_empty());
        assert!(ws.snapshot().notifications_muted);
        let unknown = Command::SetTabMuted {
            tab: TabId::new(99),
            muted: true,
        };
        assert!(ws.apply(unknown, &resolve).is_err());
    }

    #[test]
    fn rf_nav_10_side_bars_are_kept_without_widths_that_are_not_numbers() {
        let mut ws = Workspace::new();
        let bar = |width| yien_proto::SideBar {
            hidden: false,
            width,
        };
        let bars = SideBars {
            tab_bar: bar(Some(f32::NAN)),
            context_bar: bar(Some(-4.0)),
            review: true,
        };
        assert!(run(&mut ws, Command::SetSideBars { bars }).is_empty());
        let kept = ws.snapshot().side_bars;
        assert_eq!((kept.tab_bar.width, kept.context_bar.width), (None, None));
        assert!(kept.review);
        let bars = SideBars {
            tab_bar: bar(Some(300.0)),
            ..bars
        };
        run(&mut ws, Command::SetSideBars { bars });
        assert_eq!(ws.snapshot().side_bars.tab_bar.width, Some(300.0));
    }

    #[test]
    fn folder_names() {
        assert_eq!(folder_name("C:\\Code\\yien\\"), "yien");
        assert_eq!(folder_name("/home/e/proj"), "proj");
        assert_eq!(folder_name("/"), "/");
    }

    #[derive(Debug, Clone)]
    enum Op {
        NewProject(usize),
        CloseProject(usize),
        Collapse(usize, bool),
        RenameTab(usize, usize),
        NewTab(usize),
        CloseTab(usize),
        SelectTab(usize),
        Split(usize, bool),
        Close(usize),
        Focus(usize),
        FocusDir(u8),
        Resize(usize, f32),
        Swap(usize, usize),
        Move(usize, usize, u8),
        Maximize(usize),
        Window(bool),
        Exit(usize),
    }

    fn op() -> impl Strategy<Value = Op> {
        let i = 0..16usize;
        prop_oneof![
            i.clone().prop_map(Op::NewProject),
            i.clone().prop_map(Op::CloseProject),
            (i.clone(), any::<bool>()).prop_map(|(a, b)| Op::Collapse(a, b)),
            (i.clone(), i.clone()).prop_map(|(a, b)| Op::RenameTab(a, b)),
            i.clone().prop_map(Op::NewTab),
            i.clone().prop_map(Op::CloseTab),
            i.clone().prop_map(Op::SelectTab),
            (i.clone(), any::<bool>()).prop_map(|(a, b)| Op::Split(a, b)),
            (i.clone(), any::<bool>()).prop_map(|(a, b)| Op::Split(a, b)),
            i.clone().prop_map(Op::Close),
            i.clone().prop_map(Op::Focus),
            (0..4u8).prop_map(Op::FocusDir),
            (i.clone(), -1.0f32..1.0).prop_map(|(a, d)| Op::Resize(a, d)),
            (i.clone(), i.clone()).prop_map(|(a, b)| Op::Swap(a, b)),
            (i.clone(), i.clone(), 0..4u8).prop_map(|(a, b, z)| Op::Move(a, b, z)),
            i.clone().prop_map(Op::Maximize),
            any::<bool>().prop_map(Op::Window),
            i.prop_map(Op::Exit),
        ]
    }

    fn pick<T: Copy>(items: &[T], i: usize) -> Option<T> {
        (!items.is_empty()).then(|| items[i % items.len()])
    }

    fn command_for(ws: &Workspace, op: &Op) -> Option<Command> {
        let panes: Vec<PaneId> = ws.panes.keys().copied().collect();
        let tabs: Vec<TabId> = ws.tabs.iter().map(|t| t.id).collect();
        let projects: Vec<ProjectId> = ws.projects.iter().map(|p| p.id).collect();
        let dirs = [
            Direction::Left,
            Direction::Right,
            Direction::Up,
            Direction::Down,
        ];
        let zones = [
            DropZone::Left,
            DropZone::Right,
            DropZone::Top,
            DropZone::Bottom,
        ];
        Some(match op {
            Op::NewProject(i) => Command::NewProject {
                root: ["C:/p", "C:/q/", r"C:\p", "D:/r"][i % 4].into(),
                name: None,
            },
            Op::CloseProject(i) => Command::CloseProject {
                project: pick(&projects, *i)?,
            },
            Op::Collapse(i, collapsed) => Command::SetProjectCollapsed {
                project: pick(&projects, *i)?,
                collapsed: *collapsed,
            },
            Op::RenameTab(i, n) => Command::RenameTab {
                tab: pick(&tabs, *i)?,
                name: ["", " ", "review", "Ñandú 2"][n % 4].into(),
            },
            Op::NewTab(i) => Command::NewTab {
                project: pick(&projects, *i)?,
                name: None,
                profile: None,
            },
            Op::CloseTab(i) => Command::CloseTab {
                tab: pick(&tabs, *i)?,
            },
            Op::SelectTab(i) => Command::SelectTab {
                tab: pick(&tabs, *i)?,
            },
            Op::Split(i, h) => Command::SplitPane {
                pane: pick(&panes, *i)?,
                orientation: if *h {
                    Orientation::Horizontal
                } else {
                    Orientation::Vertical
                },
                profile: None,
            },
            Op::Close(i) => Command::ClosePane {
                pane: pick(&panes, *i)?,
            },
            Op::Focus(i) => Command::FocusPane {
                pane: pick(&panes, *i)?,
            },
            Op::FocusDir(d) => Command::FocusDirection {
                direction: dirs[usize::from(*d)],
            },
            Op::Resize(i, delta) => Command::ResizeSplit {
                pane: pick(&panes, *i)?,
                delta: *delta,
            },
            Op::Swap(a, b) => Command::SwapPanes {
                a: pick(&panes, *a)?,
                b: pick(&panes, *b)?,
            },
            Op::Move(a, b, z) => Command::MovePane {
                pane: pick(&panes, *a)?,
                target: pick(&panes, *b)?,
                zone: zones[usize::from(*z)],
            },
            Op::Maximize(i) => Command::ToggleMaximize {
                pane: pick(&panes, *i)?,
            },
            Op::Window(_) | Op::Exit(_) => return None,
        })
    }

    proptest! {
        #[test]
        fn rf_ses_02_any_workspace_survives_a_save_and_restore(
            ops in proptest::collection::vec(op(), 1..60)
        ) {
            let mut ws = Workspace::new();
            for op in &ops {
                if let Some(command) = command_for(&ws, op) {
                    let _ = ws.apply(command, &resolve);
                }
            }
            let saved = ws.to_session();
            let mut store = yien_session::MemStore::new();
            yien_session::save(&mut store, &saved).unwrap();
            let loaded = yien_session::load(&store).restored.unwrap().snapshot;
            let disk = Disk { existing: folders(&loaded).into_iter().collect(), home: None };
            let mut back = Workspace::new();
            let (fx, notes) = back.restore(&loaded, &resolve, &disk);
            prop_assert!(notes.is_empty(), "{:?}", notes);
            prop_assert_eq!(back.check(), Ok(()));
            prop_assert_eq!(back.to_session(), saved);
            let mut expected = ws.snapshot();
            for tab in &mut expected.tabs {
                tab.maximized = None;
            }
            prop_assert_eq!(back.snapshot(), expected);
            let mut started = spawned(&fx);
            started.sort_unstable();
            prop_assert_eq!(started, ws.panes.keys().copied().collect::<Vec<_>>());
        }

        #[test]
        fn workspace_commands_keep_the_invariants(ops in proptest::collection::vec(op(), 1..60)) {
            let mut ws = Workspace::new();
            for op in &ops {
                let before: Vec<PaneId> = ws.panes.keys().copied().collect();
                let fx = match op {
                    Op::Window(f) => ws.set_window_focus(*f),
                    Op::Exit(i) => match pick(&before, *i) {
                        Some(p) => ws.on_pane_event(p, PaneEvent::Exited(Some(0))),
                        None => Vec::new(),
                    },
                    _ => match command_for(&ws, op) {
                        Some(command) => ws.apply(command, &resolve).unwrap_or_default(),
                        None => Vec::new(),
                    },
                };
                prop_assert_eq!(ws.check(), Ok(()));
                let after: Vec<PaneId> = ws.panes.keys().copied().collect();
                for p in spawned(&fx) {
                    prop_assert!(after.contains(&p) && !before.contains(&p));
                }
                for p in stopped(&fx) {
                    prop_assert!(before.contains(&p) && !after.contains(&p));
                }
                let born = after.iter().filter(|p| !before.contains(p)).count();
                let gone = before.iter().filter(|p| !after.contains(p)).count();
                prop_assert_eq!(born, spawned(&fx).len());
                prop_assert_eq!(gone, stopped(&fx).len());
                let snap = ws.snapshot();
                for tab in &snap.tabs {
                    let panes = tab.layout.panes();
                    prop_assert!(panes.contains(&tab.focused));
                    prop_assert!(snap.panes.iter().filter(|p| p.tab == tab.id).count() == panes.len());
                }
            }
        }
    }
}
