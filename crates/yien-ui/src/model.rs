//! The view model the window draws: projects, their tabs and each tab's panes.
//!
//! Plain data keyed by `yien-proto` ids, kept up to date from the core's
//! events (ADR-03). Events may arrive in any order and may name things the
//! UI has already dropped; updates tolerate both.

use std::collections::{BTreeSet, HashMap, HashSet};

use yien_proto::{
    LayoutNode, NeedsReason, PaneId, PaneInfo, ProfileId, ProfileInfo, ProjectId, ProjectInfo,
    SHELL_PROFILE, TabGit, TabId, TabInfo, same_folder,
};

use crate::layout::{Rect, pane_rects};
use crate::worktree::Question;

/// What the agent in a pane is doing, as the window shows it (RF-AGE-05).
///
/// Anything the detector cannot tell is [`AgentState::Idle`], never
/// [`AgentState::NeedsYou`] (RN-02), so `Idle` is the default.
///
/// ```
/// use yien_ui::AgentState;
///
/// assert_eq!(AgentState::default(), AgentState::Idle);
/// assert!(AgentState::NeedsYou.needs_you());
/// assert_eq!(AgentState::Done.label(), "Ready to review");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AgentState {
    /// Waiting for the user: a permission, a question. The only red state (RN-01).
    NeedsYou,
    /// The program stopped with an error.
    Error,
    /// Finished a turn the user has not looked at yet.
    Done,
    /// Busy with a turn.
    Working,
    /// Nothing to report, or nothing known.
    #[default]
    Idle,
}

impl AgentState {
    /// The words shown next to the glyph.
    pub fn label(self) -> &'static str {
        match self {
            Self::NeedsYou => "Needs you",
            Self::Error => "Error",
            Self::Done => "Ready to review",
            Self::Working => "Working",
            Self::Idle => "Idle",
        }
    }

    /// Whether this is the "needs you" state.
    pub fn needs_you(self) -> bool {
        self == Self::NeedsYou
    }

    /// Lower is more urgent: a tab shows its most urgent pane.
    fn urgency(self) -> u8 {
        match self {
            Self::NeedsYou => 0,
            Self::Error => 1,
            Self::Done => 2,
            Self::Working => 3,
            Self::Idle => 4,
        }
    }
}

impl From<yien_proto::AgentState> for AgentState {
    fn from(state: yien_proto::AgentState) -> Self {
        use yien_proto::AgentState as P;
        match state {
            P::Idle => Self::Idle,
            P::Working => Self::Working,
            P::NeedsYou(_) => Self::NeedsYou,
            P::Done => Self::Done,
            P::Error(_) => Self::Error,
        }
    }
}

fn needs_reason(state: yien_proto::AgentState) -> Option<NeedsReason> {
    match state {
        yien_proto::AgentState::NeedsYou(reason) => Some(reason),
        _ => None,
    }
}

/// One pane, as its title bar shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneView {
    /// The pane's id in the protocol.
    pub id: PaneId,
    /// What runs in it: `claude`, `pwsh`, `cargo test`.
    pub name: String,
    /// The profile it was started with: `Claude Code`, `Shell`.
    pub profile: String,
    /// That profile's id: `claude`, `shell`; empty until the details arrive.
    pub profile_id: ProfileId,
    /// What its agent is doing.
    pub state: AgentState,
    /// Why its agent needs the user, while it does.
    pub needs_reason: Option<NeedsReason>,
    /// Whether its program is running.
    pub running: bool,
    /// The exit code once it stopped, when the OS reported one.
    pub exit_code: Option<u32>,
    /// Its program ran and stopped, or could not start (RF-PAN-19). A pane
    /// still starting is not running but has not exited either.
    pub exited: bool,
    /// While it needs the user: its place in the wait order, lower first
    /// (RF-ATN-05).
    pub needs_you_since: Option<u64>,
    /// Its agent's state cannot be told, so it shows idle (RN-02).
    pub signal_lost: bool,
    /// The agent session it names, once known (RF-SES-07).
    pub session_id: Option<String>,
    /// Its own program is starting or running, so that session runs here.
    pub runs_session: bool,
}

impl PaneView {
    /// A pane the layout names before its details arrive.
    fn placeholder(id: PaneId) -> Self {
        Self {
            id,
            name: "terminal".to_owned(),
            profile: String::new(),
            profile_id: ProfileId::new(""),
            state: AgentState::Idle,
            needs_reason: None,
            running: true,
            exit_code: None,
            exited: false,
            needs_you_since: None,
            signal_lost: false,
            session_id: None,
            runs_session: false,
        }
    }

    fn from_info(info: &PaneInfo) -> Self {
        Self {
            id: info.id,
            name: info.name.clone(),
            profile: info.profile.clone(),
            profile_id: info.profile_id.clone(),
            state: info.state.into(),
            needs_reason: needs_reason(info.state),
            running: info.running,
            exit_code: info.exit_code,
            exited: !info.running && info.exit_code.is_some(),
            needs_you_since: info.needs_you_since,
            signal_lost: info.signal_lost,
            session_id: info.session_id.clone(),
            runs_session: info.runs_session,
        }
    }

    /// Whether closing it stops work the user may want: an agent or other
    /// program that runs, or anything working or waiting for the user. A
    /// shell at its prompt is not busy; a command running inside a shell
    /// cannot be seen from here and does not count either. A pane whose
    /// details have not arrived counts as busy, to ask rather than lose work.
    ///
    /// ```
    /// use yien_ui::{AgentState, PaneView};
    ///
    /// let shell = PaneView {
    ///     id: yien_proto::PaneId::new(1),
    ///     name: "pwsh".into(),
    ///     profile: "Shell".into(),
    ///     profile_id: yien_proto::ProfileId::new("shell"),
    ///     state: AgentState::Idle,
    ///     needs_reason: None,
    ///     running: true,
    ///     exit_code: None,
    ///     exited: false,
    ///     needs_you_since: None,
    ///     signal_lost: false,
    ///     session_id: None,
    ///     runs_session: true,
    /// };
    /// assert!(!shell.is_busy());
    /// let claude = PaneView { profile_id: yien_proto::ProfileId::new("claude"), ..shell };
    /// assert!(claude.is_busy());
    /// ```
    pub fn is_busy(&self) -> bool {
        let agent = self.profile_id.as_str() != SHELL_PROFILE;
        let active = matches!(self.state, AgentState::Working | AgentState::NeedsYou);
        active || (self.running && agent)
    }
}

/// One tab: a working directory, maybe a branch, and its panes.
#[derive(Debug, Clone, PartialEq)]
pub struct TabView {
    /// The tab's id in the protocol.
    pub id: TabId,
    /// The project it belongs to.
    pub project: ProjectId,
    /// The name in the tab bar.
    pub name: String,
    /// The working directory every pane starts in (RF-ESP-04).
    pub dir: String,
    /// The git branch checked out in `dir`, when it is a repository (RF-ESP-04).
    pub branch: Option<String>,
    /// How the panes split the tab.
    pub layout: LayoutNode,
    /// The pane that has the keyboard.
    pub focused: PaneId,
    /// The pane shown alone, if any (RF-PAN-04).
    pub maximized: Option<PaneId>,
    /// Its toasts and taskbar flashes are muted (RF-ATN-11).
    pub muted: bool,
    /// The linked worktree the tab was opened on, which is `dir`; its row
    /// carries a worktree mark (RF-ESP-05).
    pub worktree: Option<String>,
}

impl TabView {
    fn from_info(info: &TabInfo) -> Self {
        Self {
            id: info.id,
            project: info.project,
            name: info.name.clone(),
            dir: info.dir.clone(),
            branch: info.branch.clone(),
            layout: info.layout.clone(),
            focused: info.focused,
            maximized: info.maximized,
            muted: info.muted,
            worktree: info.worktree.clone(),
        }
    }

    /// Each visible pane with its rectangle as fractions of the pane area, in
    /// tree order. A maximized pane fills the area alone.
    pub(crate) fn rects(&self) -> Vec<(PaneId, Rect)> {
        let all = pane_rects(&self.layout);
        match self.maximized {
            Some(max) if all.iter().any(|(p, _)| *p == max) => vec![(max, Rect::FULL)],
            _ => all,
        }
    }
}

/// Where adding a folder as a project leads (RF-ESP-01).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderTarget {
    /// The folder is a project already: select this tab of it.
    Tab(TabId),
    /// The folder is a project with no tab left, or a new project; the core
    /// opens a tab with the shell either way.
    NewProject,
}

/// One project in the tab bar, with its tabs nested under it (RF-ESP-02).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectView {
    /// The project's id in the protocol.
    pub id: ProjectId,
    /// The name in the tab bar.
    pub name: String,
    /// The folder it was registered from.
    pub root: String,
    /// Its tabs, in tab-bar order.
    pub tabs: Vec<TabId>,
}

/// Everything the window shows, and which tab is selected.
///
/// ```
/// use yien_proto::{LayoutNode, PaneId, ProjectId, ProjectInfo, TabId, TabInfo, WorkspaceSnapshot};
/// use yien_ui::WorkspaceView;
///
/// let snapshot = WorkspaceSnapshot {
///     projects: vec![ProjectInfo {
///         id: ProjectId::new(1), name: "yien".into(), root: "C:/yien".into(), tabs: vec![TabId::new(2)],
///         collapsed: false,
///     }],
///     tabs: vec![TabInfo {
///         id: TabId::new(2), project: ProjectId::new(1), name: "Tab 1".into(), dir: "C:/yien".into(),
///         branch: None, layout: LayoutNode::Pane(PaneId::new(3)), focused: PaneId::new(3), maximized: None,
///         muted: false, worktree: None,
///     }],
///     panes: vec![],
///     active_tab: Some(TabId::new(2)),
///     profiles: vec![],
///     notifications_muted: false,
/// };
/// let view = WorkspaceView::from_snapshot(&snapshot);
/// assert_eq!(view.selected_tab().map(|t| t.id), Some(TabId::new(2)));
/// assert_eq!(view.pane_count(), 1);
/// ```
#[derive(Debug, Clone, Default)]
pub struct WorkspaceView {
    projects: Vec<ProjectView>,
    tabs: HashMap<TabId, TabView>,
    panes: HashMap<PaneId, PaneView>,
    selected: Option<TabId>,
    folded: HashSet<ProjectId>,
    /// Closed tabs and panes, so a late update cannot bring them back. Ids
    /// are never reused.
    closed_tabs: HashSet<TabId>,
    closed_panes: HashSet<PaneId>,
    /// The profiles a new pane can start with, the shell first.
    profiles: Vec<ProfileInfo>,
    /// Tabs in the order they were last selected, the latest first
    /// (RF-NAV-05).
    recent: Vec<TabId>,
    /// The project of the tab selected last, kept after its tabs close.
    last_project: Option<ProjectId>,
    /// The git facts last reported for each tab (RF-CTX-04).
    git: HashMap<TabId, TabGit>,
    /// Every toast and taskbar flash is muted (RF-ATN-11).
    notifications_muted: bool,
}

impl WorkspaceView {
    /// The workspace as the core's first event describes it.
    pub fn from_snapshot(snapshot: &yien_proto::WorkspaceSnapshot) -> Self {
        let mut view = Self {
            profiles: snapshot.profiles.clone(),
            notifications_muted: snapshot.notifications_muted,
            ..Self::default()
        };
        for project in &snapshot.projects {
            view.project_updated(project);
        }
        for tab in &snapshot.tabs {
            view.tab_updated(tab);
        }
        for pane in &snapshot.panes {
            view.pane_updated(pane);
        }
        view.set_active(snapshot.active_tab);
        if view.selected.is_none() {
            let first = view.tabs_in_order().next().map(|t| t.id);
            view.selected = first;
        }
        view
    }

    /// A project was added or changed. Its tab order is the core's.
    pub fn project_updated(&mut self, info: &ProjectInfo) {
        let project = ProjectView {
            id: info.id,
            name: info.name.clone(),
            root: info.root.clone(),
            tabs: info.tabs.clone(),
        };
        if info.collapsed {
            self.folded.insert(info.id);
        } else {
            self.folded.remove(&info.id);
        }
        match self.projects.iter_mut().find(|p| p.id == info.id) {
            Some(existing) => *existing = project,
            None => self.projects.push(project),
        }
    }

    /// A tab was added or changed. A tab its project does not list yet is
    /// appended to it, so it shows before the project update arrives. An
    /// update for a closed tab is ignored.
    pub fn tab_updated(&mut self, info: &TabInfo) {
        if self.closed_tabs.contains(&info.id) {
            return;
        }
        if let Some(project) = self.projects.iter_mut().find(|p| p.id == info.project)
            && !project.tabs.contains(&info.id)
        {
            project.tabs.push(info.id);
        }
        self.tabs.insert(info.id, TabView::from_info(info));
    }

    /// A tab closed with its panes. Returns the panes that went with it. If
    /// it was selected, a neighbour in the same project is selected instead.
    pub fn tab_closed(&mut self, id: TabId) -> Vec<PaneId> {
        let order: Vec<TabId> = self.tabs_in_order().map(|t| t.id).collect();
        let Some(tab) = self.tabs.remove(&id) else {
            return Vec::new();
        };
        let mut sibling = None;
        if let Some(project) = self.projects.iter_mut().find(|p| p.id == tab.project)
            && let Some(i) = project.tabs.iter().position(|t| *t == id)
        {
            project.tabs.remove(i);
            sibling = project
                .tabs
                .get(i)
                .or_else(|| i.checked_sub(1).and_then(|j| project.tabs.get(j)))
                .copied();
        }
        self.closed_tabs.insert(id);
        self.git.remove(&id);
        self.recent.retain(|t| *t != id);
        let gone = tab.layout.panes();
        for pane in &gone {
            self.panes.remove(pane);
            self.closed_panes.insert(*pane);
        }
        if self.selected == Some(id) {
            let fallback = || {
                let at = order.iter().position(|t| *t == id).unwrap_or(0);
                order
                    .iter()
                    .filter(|t| **t != id)
                    .nth(at.saturating_sub(1))
                    .copied()
            };
            self.selected = sibling.or_else(fallback);
        }
        gone
    }

    /// A project left the sidebar. The core closes its tabs first; any still
    /// held go now. Returns the panes that went with them.
    pub fn project_closed(&mut self, id: ProjectId) -> Vec<PaneId> {
        let tabs = self
            .projects
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.tabs.clone())
            .unwrap_or_default();
        let gone = tabs.into_iter().flat_map(|t| self.tab_closed(t)).collect();
        self.projects.retain(|p| p.id != id);
        self.folded.remove(&id);
        if self.last_project == Some(id) {
            self.last_project = None;
        }
        gone
    }

    /// Git facts about a tab's folder arrived. `false` for a tab the view
    /// does not hold.
    pub fn tab_git(&mut self, id: TabId, git: TabGit) -> bool {
        if !self.tabs.contains_key(&id) {
            return false;
        }
        self.git.insert(id, git);
        true
    }

    /// The git facts last reported for a tab, if any.
    pub fn git_of(&self, id: TabId) -> Option<&TabGit> {
        self.git.get(&id)
    }

    /// The second line of a tab row: its branch, the commit when detached,
    /// else its folder (RF-ESP-04).
    pub fn tab_detail(&self, tab: &TabView) -> String {
        if let Some(branch) = &tab.branch {
            return branch.clone();
        }
        match self.git.get(&tab.id) {
            Some(TabGit::Repository(facts)) => facts.head.label().to_owned(),
            _ => tab.dir.clone(),
        }
    }

    /// The core changed the selected tab. An unknown tab is ignored. The
    /// core owns the folds, so this does not unfold the tab's project.
    pub fn set_active(&mut self, tab: Option<TabId>) {
        match tab {
            Some(id) => {
                self.mark_selected(id);
            }
            None => self.selected = None,
        }
    }

    /// A pane was added or its details changed. An update for a closed pane,
    /// or for a pane of a tab the view does not hold, is ignored.
    pub fn pane_updated(&mut self, info: &PaneInfo) {
        if self.closed_panes.contains(&info.id) || !self.tabs.contains_key(&info.tab) {
            return;
        }
        let mut pane = PaneView::from_info(info);
        if !info.running && self.panes.get(&info.id).is_some_and(|p| p.exited) {
            pane.exited = true;
        }
        self.panes.insert(info.id, pane);
    }

    /// A pane closed.
    pub fn pane_closed(&mut self, id: PaneId) {
        self.panes.remove(&id);
        self.closed_panes.insert(id);
    }

    /// A pane's agent state changed. `false` when the pane is unknown. Its
    /// place in the wait order comes with the pane update that follows.
    pub fn pane_state(&mut self, id: PaneId, state: yien_proto::AgentState) -> bool {
        self.panes
            .get_mut(&id)
            .map(|p| {
                p.state = state.into();
                p.needs_reason = needs_reason(state);
                if !p.state.needs_you() {
                    p.needs_you_since = None;
                }
            })
            .is_some()
    }

    /// A pane's program exited. `false` when the pane is unknown.
    pub fn pane_exited(&mut self, id: PaneId, code: Option<u32>) -> bool {
        self.panes
            .get_mut(&id)
            .map(|p| {
                p.running = false;
                p.exit_code = code;
                p.exited = true;
                p.runs_session = false;
            })
            .is_some()
    }

    /// Every project, in tab-bar order.
    pub fn projects(&self) -> &[ProjectView] {
        &self.projects
    }

    /// Toasts and flashes were muted or unmuted for every tab.
    pub fn set_notifications_muted(&mut self, muted: bool) {
        self.notifications_muted = muted;
    }

    /// Whether every toast and flash is muted (RF-ATN-11).
    pub fn notifications_muted(&self) -> bool {
        self.notifications_muted
    }

    /// Whether a pane's toasts and flashes are off: all are muted, or its
    /// tab is (RF-ATN-11). Its red mark and the counter do not change.
    pub fn alerts_muted(&self, pane: PaneId) -> bool {
        self.notifications_muted || self.tab_of(pane).is_some_and(|t| t.muted)
    }

    /// The profiles a new pane can start with, the shell first.
    pub fn profiles(&self) -> &[ProfileInfo] {
        &self.profiles
    }

    /// Every pane that needs the user, the one waiting longest first
    /// (RF-ATN-05). A pane whose place has not arrived yet goes last.
    pub fn waiting_panes(&self) -> Vec<PaneId> {
        let mut waiting: Vec<(Option<u64>, usize, PaneId)> = self
            .all_panes()
            .into_iter()
            .enumerate()
            .filter_map(|(at, id)| {
                let pane = self.panes.get(&id)?;
                pane.state
                    .needs_you()
                    .then_some((pane.needs_you_since, at, id))
            })
            .collect();
        waiting.sort_by_key(|(since, at, _)| (since.is_none(), *since, *at));
        waiting.into_iter().map(|(_, _, id)| id).collect()
    }

    /// Where the jump to the next pane that needs the user goes from
    /// `current` (RF-ATN-05): the pane waiting longest, or, from a pane that
    /// is itself waiting, the next one in the wait order, wrapping around.
    pub fn next_waiting(&self, current: Option<PaneId>) -> Option<PaneId> {
        let waiting = self.waiting_panes();
        let next = current
            .and_then(|c| waiting.iter().position(|p| *p == c))
            .map_or(0, |at| (at + 1) % waiting.len());
        waiting.get(next).copied()
    }

    /// The tabs of a project that the UI knows, in tab-bar order.
    pub fn tabs_of<'a>(&'a self, project: &'a ProjectView) -> impl Iterator<Item = &'a TabView> {
        project.tabs.iter().filter_map(|id| self.tabs.get(id))
    }

    /// The tab rows the tab bar draws, top to bottom: folded projects show
    /// none (RF-NAV-08 moves the keyboard over these).
    pub fn drawn_tabs(&self) -> Vec<TabId> {
        self.projects
            .iter()
            .filter(|p| !self.is_folded(p.id))
            .flat_map(|p| self.tabs_of(p).map(|t| t.id))
            .collect()
    }

    /// A tab by id.
    pub fn tab(&self, id: TabId) -> Option<&TabView> {
        self.tabs.get(&id)
    }

    /// A pane's details; a placeholder while they have not arrived.
    pub fn pane(&self, id: PaneId) -> PaneView {
        self.panes
            .get(&id)
            .cloned()
            .unwrap_or_else(|| PaneView::placeholder(id))
    }

    /// The pane whose own program runs agent session `id`, if any; a pane
    /// that stopped or runs a stand-in shell does not count. Ids compare
    /// without case (RF-SES-07).
    pub fn pane_with_session(&self, id: &str) -> Option<PaneId> {
        self.all_panes().into_iter().find(|pane| {
            self.panes.get(pane).is_some_and(|p| {
                p.runs_session
                    && p.session_id
                        .as_deref()
                        .is_some_and(|s| s.eq_ignore_ascii_case(id))
            })
        })
    }

    /// Every pane of every tab, in tab-bar and tree order.
    pub fn all_panes(&self) -> Vec<PaneId> {
        self.tabs_in_order()
            .flat_map(|t| t.layout.panes())
            .collect()
    }

    /// The question to ask before closing `pane`, when that closes the last
    /// tab on a worktree (RF-ESP-07): the pane is its tab's only one, the tab
    /// is on a worktree, and no other tab works in that folder. `None` closes
    /// at once.
    pub(crate) fn worktree_question(&self, pane: PaneId) -> Option<Question> {
        let tab = self.tab_of(pane)?;
        let worktree = tab.worktree.as_deref()?;
        if tab.layout.panes().len() > 1 {
            return None;
        }
        let shared = self.tabs.values().any(|t| {
            t.id != tab.id
                && (same_folder(&t.dir, worktree)
                    || t.worktree
                        .as_deref()
                        .is_some_and(|w| same_folder(w, worktree)))
        });
        (!shared).then(|| {
            Question::new(
                tab.id,
                tab.name.clone(),
                worktree.to_owned(),
                tab.branch.clone(),
            )
        })
    }

    /// The tab a pane belongs to.
    pub fn tab_of(&self, pane: PaneId) -> Option<&TabView> {
        self.tabs_in_order()
            .find(|t| t.layout.panes().contains(&pane))
    }

    /// The selected tab, if there is any tab at all.
    pub fn selected_tab(&self) -> Option<&TabView> {
        self.tabs.get(&self.selected?)
    }

    /// The project of the selected tab.
    pub fn selected_project(&self) -> Option<&ProjectView> {
        let tab = self.selected_tab()?;
        self.projects.iter().find(|p| p.id == tab.project)
    }

    /// Selects a tab and unfolds its project, as the core will on the
    /// `SelectTab` that follows. `false` when no tab has that id.
    pub fn select_tab(&mut self, id: TabId) -> bool {
        if !self.mark_selected(id) {
            return false;
        }
        if let Some(project) = self.tabs.get(&id).map(|t| t.project) {
            self.folded.remove(&project);
        }
        true
    }

    fn mark_selected(&mut self, id: TabId) -> bool {
        let Some(project) = self.tabs.get(&id).map(|t| t.project) else {
            return false;
        };
        self.selected = Some(id);
        self.recent.retain(|t| *t != id);
        self.recent.insert(0, id);
        self.last_project = Some(project);
        true
    }

    /// Whether there is no project at all: the window then invites the user
    /// to add one.
    pub fn has_no_projects(&self) -> bool {
        self.projects.is_empty()
    }

    /// Where adding `root` as a project leads: a folder that is a project
    /// already, compared as [`same_folder`] does, goes to its most recently
    /// selected tab (RF-ESP-01).
    pub fn folder_target(&self, root: &str) -> FolderTarget {
        let Some(project) = self.projects.iter().find(|p| same_folder(&p.root, root)) else {
            return FolderTarget::NewProject;
        };
        self.tabs_by_recent()
            .iter()
            .find(|t| t.project == project.id)
            .map_or(FolderTarget::NewProject, |t| FolderTarget::Tab(t.id))
    }

    /// How many panes of a project closing it would stop while they work
    /// ([`PaneView::is_busy`]).
    pub fn busy_panes_of(&self, id: ProjectId) -> usize {
        let Some(project) = self.projects.iter().find(|p| p.id == id) else {
            return 0;
        };
        self.tabs_of(project)
            .flat_map(|t| t.layout.panes())
            .filter(|p| self.pane(*p).is_busy())
            .count()
    }

    /// A project by id.
    pub fn project(&self, id: ProjectId) -> Option<&ProjectView> {
        self.projects.iter().find(|p| p.id == id)
    }

    /// The project a new tab opens in: the selected tab's; with no tab
    /// selected, the project selected last, or else the first project.
    pub fn project_for_new_tab(&self) -> Option<ProjectId> {
        if let Some(project) = self.selected_project() {
            return Some(project.id);
        }
        self.last_project
            .filter(|id| self.projects.iter().any(|p| p.id == *id))
            .or_else(|| self.projects.first().map(|p| p.id))
    }

    /// The branch of the project's main working copy, as a tab on it last
    /// saw it: where a new worktree's branch starts by default (RF-ESP-05).
    pub fn main_branch_of(&self, id: ProjectId) -> Option<&str> {
        let project = self.project(id)?;
        self.tabs_of(project)
            .filter(|t| t.worktree.is_none())
            .find_map(|t| t.branch.as_deref())
    }

    /// Every tab, the most recently selected first; tabs never selected
    /// follow in tab-bar order (RF-NAV-05).
    pub fn tabs_by_recent(&self) -> Vec<&TabView> {
        let mut tabs: Vec<&TabView> = self
            .recent
            .iter()
            .filter_map(|id| self.tabs.get(id))
            .collect();
        for tab in self.tabs_in_order() {
            if !self.recent.contains(&tab.id) {
                tabs.push(tab);
            }
        }
        tabs
    }

    /// Tab `n` (from 1) of the selected tab's project, in tab-bar order
    /// (RF-NAV-05).
    pub fn nth_tab_of_selected_project(&self, n: usize) -> Option<TabId> {
        let project = self.selected_project()?;
        self.tabs_of(project).nth(n.checked_sub(1)?).map(|t| t.id)
    }

    /// A project's first tab, if it has one.
    pub fn first_tab_of(&self, id: ProjectId) -> Option<TabId> {
        let project = self.projects.iter().find(|p| p.id == id)?;
        self.tabs_of(project).next().map(|t| t.id)
    }

    /// The tab `by` steps from the selected one in tab-bar order, wrapping
    /// around; the first tab when nothing is selected.
    pub fn tab_after(&self, by: isize) -> Option<TabId> {
        let ids: Vec<TabId> = self.tabs_in_order().map(|t| t.id).collect();
        let len = isize::try_from(ids.len()).ok().filter(|&n| n > 0)?;
        let at = self
            .selected
            .and_then(|s| ids.iter().position(|&id| id == s))
            .and_then(|i| isize::try_from(i).ok());
        let next = match at {
            Some(i) => (i + by).rem_euclid(len),
            None => 0,
        };
        usize::try_from(next).ok().and_then(|i| ids.get(i).copied())
    }

    /// Folds or unfolds a project in the tab bar (RF-ESP-17), ahead of the
    /// core's confirmation. Returns whether it is folded now; `None` when the
    /// project is unknown.
    pub fn toggle_fold(&mut self, id: ProjectId) -> Option<bool> {
        if !self.projects.iter().any(|p| p.id == id) {
            return None;
        }
        if self.folded.remove(&id) {
            Some(false)
        } else {
            self.folded.insert(id);
            Some(true)
        }
    }

    /// Whether a project's tabs are hidden in the tab bar.
    pub fn is_folded(&self, id: ProjectId) -> bool {
        self.folded.contains(&id)
    }

    /// A tab's state: that of its most urgent pane.
    pub fn tab_state(&self, tab: &TabView) -> AgentState {
        tab.layout
            .panes()
            .into_iter()
            .map(|p| self.pane(p).state)
            .min_by_key(|s| s.urgency())
            .unwrap_or_default()
    }

    /// How many of a project's tabs have a pane that needs the user.
    pub fn needs_you_tabs_of(&self, project: &ProjectView) -> usize {
        self.tabs_of(project)
            .filter(|t| self.tab_state(t).needs_you())
            .count()
    }

    /// Panes whose agent needs the user: the global counter (RF-ATN-03).
    pub fn needs_you_panes(&self) -> usize {
        self.count_panes(AgentState::NeedsYou)
    }

    /// Tabs with at least one pane that needs the user.
    pub fn needs_you_tabs(&self) -> usize {
        self.tabs_in_order()
            .filter(|t| self.tab_state(t).needs_you())
            .count()
    }

    /// Panes whose agent is working.
    pub fn working_panes(&self) -> usize {
        self.count_panes(AgentState::Working)
    }

    /// How many tabs there are.
    pub fn tab_count(&self) -> usize {
        self.tabs_in_order().count()
    }

    /// How many panes there are.
    pub fn pane_count(&self) -> usize {
        self.all_panes().len()
    }

    fn count_panes(&self, state: AgentState) -> usize {
        self.all_panes()
            .into_iter()
            .filter(|p| self.pane(*p).state == state)
            .count()
    }

    fn tabs_in_order(&self) -> impl Iterator<Item = &TabView> {
        self.projects.iter().flat_map(|p| self.tabs_of(p))
    }
}

/// The name to send when a tab rename ends with `typed` in the field: `None`
/// when it is the name the tab already shows, so keeping it unchanged never
/// turns the automatic name into the tab's own. Empty brings back the
/// automatic name (RF-ESP-16).
pub(crate) fn typed_tab_name(typed: &str, current: Option<&str>) -> Option<String> {
    let typed = typed.trim();
    (Some(typed) != current).then(|| typed.to_owned())
}

/// The panes whose frame to take after a batch of events: every pane that
/// announced one and, when the workspace changed, every pane now drawn, since
/// a pane that just came into view or got its view may hold a frame whose
/// notice came earlier. Only panes with a view are taken: the core sends no
/// new notice until a frame is taken, so taking one with nothing to draw it
/// would lose it.
pub(crate) fn frames_to_take(
    ready: &BTreeSet<PaneId>,
    changed: bool,
    visible: &[PaneId],
    has_view: impl Fn(PaneId) -> bool,
) -> Vec<PaneId> {
    let mut panes = ready.clone();
    if changed {
        panes.extend(visible.iter().copied());
    }
    panes.into_iter().filter(|p| has_view(*p)).collect()
}

#[cfg(test)]
mod tests {
    use yien_proto::{AgentState as P, LayoutChild, NeedsReason, Orientation, WorkspaceSnapshot};

    use super::*;

    fn layout(panes: &[u64]) -> LayoutNode {
        match panes {
            [one] => LayoutNode::Pane(PaneId::new(*one)),
            many => LayoutNode::Split {
                orientation: Orientation::Horizontal,
                children: many
                    .iter()
                    .map(|p| LayoutChild {
                        flex: 1.0,
                        node: LayoutNode::Pane(PaneId::new(*p)),
                    })
                    .collect(),
            },
        }
    }

    fn tab(id: u64, project: u64, panes: &[u64]) -> TabInfo {
        TabInfo {
            id: TabId::new(id),
            project: ProjectId::new(project),
            name: format!("tab-{id}"),
            dir: format!("C:/Code/tab-{id}"),
            branch: None,
            layout: layout(panes),
            focused: PaneId::new(panes[0]),
            maximized: None,
            muted: false,
            worktree: None,
        }
    }

    fn project(id: u64, tabs: &[u64]) -> ProjectInfo {
        ProjectInfo {
            id: ProjectId::new(id),
            name: format!("project-{id}"),
            root: format!("C:/Code/project-{id}"),
            tabs: tabs.iter().map(|t| TabId::new(*t)).collect(),
            collapsed: false,
        }
    }

    fn pane(id: u64, tab: u64, state: P) -> PaneInfo {
        PaneInfo {
            id: PaneId::new(id),
            tab: TabId::new(tab),
            name: format!("pane-{id}"),
            profile: "Shell".into(),
            profile_id: yien_proto::ProfileId::new("shell"),
            title: None,
            cwd: None,
            state,
            running: true,
            exit_code: None,
            session_id: None,
            needs_you_since: state.is_red().then_some(id),
            signal_lost: false,
            runs_session: true,
        }
    }

    const NEEDS: P = P::NeedsYou(NeedsReason::Permission);

    /// project 1: tab 10 (panes 100 needs, 101 idle), tab 11 (110 working);
    /// project 2: tab 20 (200 done); project 3: empty.
    fn demo() -> WorkspaceView {
        WorkspaceView::from_snapshot(&WorkspaceSnapshot {
            projects: vec![project(1, &[10, 11]), project(2, &[20]), project(3, &[])],
            tabs: vec![
                tab(10, 1, &[100, 101]),
                tab(11, 1, &[110]),
                tab(20, 2, &[200]),
            ],
            panes: vec![
                pane(100, 10, NEEDS),
                pane(101, 10, P::Idle),
                pane(110, 11, P::Working),
                pane(200, 20, P::Done),
            ],
            active_tab: Some(TabId::new(10)),
            profiles: vec![],
            notifications_muted: false,
        })
    }

    fn tab_names(view: &WorkspaceView) -> Vec<Vec<String>> {
        view.projects()
            .iter()
            .map(|p| view.tabs_of(p).map(|t| t.name.clone()).collect())
            .collect()
    }

    #[test]
    fn rf_esp_07_closing_the_last_pane_of_the_last_tab_on_a_worktree_asks() {
        let mut view = demo();
        let mut wt = tab(30, 1, &[300]);
        wt.worktree = Some("C:/Code/app-wt/feat-x".into());
        wt.dir = "C:/Code/app-wt/feat-x".into();
        wt.branch = Some("feat/x".into());
        view.tab_updated(&wt);
        let shown = view.tab(TabId::new(30)).unwrap();
        assert_eq!(shown.worktree.as_deref(), Some("C:/Code/app-wt/feat-x"));
        assert!(view.tab(TabId::new(10)).unwrap().worktree.is_none());

        let q = view.worktree_question(PaneId::new(300)).unwrap();
        assert_eq!(q.tab, TabId::new(30));
        assert_eq!(q.worktree, "C:/Code/app-wt/feat-x");
        assert_eq!(q.branch.as_deref(), Some("feat/x"));
        assert!(
            view.worktree_question(PaneId::new(110)).is_none(),
            "not a worktree tab"
        );

        // Another pane in the tab: closing one pane closes no tab.
        let mut split = wt.clone();
        split.layout = layout(&[300, 301]);
        view.tab_updated(&split);
        assert!(view.worktree_question(PaneId::new(300)).is_none());
        view.tab_updated(&wt);

        assert_eq!(view.main_branch_of(ProjectId::new(1)), None);
        let mut main = tab(10, 1, &[100, 101]);
        main.branch = Some("main".into());
        view.tab_updated(&main);
        assert_eq!(view.main_branch_of(ProjectId::new(1)), Some("main"));

        // Another tab in the same folder keeps the worktree in use.
        let mut other = tab(31, 1, &[310]);
        // Folders compare without case and with either slash on Windows only.
        other.dir = if cfg!(windows) {
            r"c:\code\app-wt\feat-x\"
        } else {
            "C:/Code/app-wt/feat-x/"
        }
        .into();
        view.tab_updated(&other);
        assert!(view.worktree_question(PaneId::new(300)).is_none());
    }

    #[test]
    fn rf_esp_02_projects_keep_their_tabs_nested_in_order() {
        let view = demo();
        assert_eq!(
            tab_names(&view),
            vec![vec!["tab-10", "tab-11"], vec!["tab-20"], vec![]]
        );
        assert_eq!(view.tab_count(), 3);
        assert_eq!(view.pane_count(), 4);
    }

    #[test]
    fn the_snapshot_selects_the_core_active_tab() {
        let view = demo();
        assert_eq!(view.selected_tab().map(|t| t.id), Some(TabId::new(10)));
        assert_eq!(
            view.selected_project().map(|p| p.id),
            Some(ProjectId::new(1))
        );
    }

    #[test]
    fn rn_02_an_unknown_state_is_idle_not_needs_you() {
        assert_eq!(AgentState::default(), AgentState::Idle);
        assert!(!AgentState::default().needs_you());
        assert_eq!(AgentState::from(P::Idle), AgentState::Idle);
        let view = demo();
        assert_eq!(view.pane(PaneId::new(999)).state, AgentState::Idle);
    }

    #[test]
    fn rf_atn_03_the_counter_counts_agents_not_tabs() {
        let mut view = demo();
        assert!(view.pane_state(PaneId::new(200), NEEDS));
        assert_eq!(view.needs_you_panes(), 2);
        assert_eq!(view.needs_you_tabs(), 2);
        assert_eq!(view.working_panes(), 1);
        let first = view.projects()[0].clone();
        assert_eq!(view.needs_you_tabs_of(&first), 1);
    }

    #[test]
    fn rf_atn_02_red_propagates_to_the_tab_and_the_project_and_clears_with_the_pane() {
        let mut view = demo();
        let tab = |v: &WorkspaceView, id| v.tab_state(v.tab(TabId::new(id)).unwrap());
        let project = |v: &WorkspaceView| {
            let first = v.projects()[0].clone();
            v.needs_you_tabs_of(&first)
        };
        assert_eq!(tab(&view, 11), AgentState::Working);
        assert!(view.pane_state(PaneId::new(110), NEEDS));
        assert_eq!(tab(&view, 11), AgentState::NeedsYou);
        assert_eq!(project(&view), 2);
        assert_eq!(view.needs_you_panes(), 2);
        assert!(view.pane_state(PaneId::new(100), P::Working));
        assert!(view.pane_state(PaneId::new(110), P::Working));
        assert_eq!(tab(&view, 10), AgentState::Working);
        assert_eq!(project(&view), 0);
        assert_eq!(view.needs_you_panes(), 0);
    }

    #[test]
    fn rf_atn_15_an_unselected_tab_that_needs_you_shows_the_red_bar() {
        let mut view = demo();
        let t = crate::theme::Theme::of(crate::theme::ThemeKind::default());
        let bar =
            |v: &WorkspaceView, id| t.tab_row_bar(v.tab_state(v.tab(TabId::new(id)).unwrap()));
        assert_eq!(view.selected_tab().map(|t| t.id), Some(TabId::new(10)));
        assert_eq!(bar(&view, 20), None);
        assert_eq!(bar(&view, 11), None);
        assert!(view.pane_state(PaneId::new(200), NEEDS));
        assert_eq!(bar(&view, 20), Some(t.shu));
        assert_eq!(view.selected_tab().map(|t| t.id), Some(TabId::new(10)));
        assert!(view.pane_state(PaneId::new(200), P::Idle));
        assert_eq!(bar(&view, 20), None);
    }

    #[test]
    fn rf_atn_05_the_jump_goes_oldest_first_and_cycles() {
        let mut view = demo();
        // 100 waits since 100; 200 and 110 start waiting later, 110 last.
        let later = |view: &mut WorkspaceView, id: u64, tab: u64, since: u64| {
            let mut info = pane(id, tab, NEEDS);
            info.needs_you_since = Some(since);
            view.pane_updated(&info);
        };
        later(&mut view, 110, 11, 300);
        later(&mut view, 200, 20, 250);
        let order: Vec<u64> = view.waiting_panes().iter().map(|p| p.get()).collect();
        assert_eq!(order, vec![100, 200, 110]);
        assert_eq!(view.next_waiting(None), Some(PaneId::new(100)));
        assert_eq!(
            view.next_waiting(Some(PaneId::new(101))),
            Some(PaneId::new(100))
        );
        assert_eq!(
            view.next_waiting(Some(PaneId::new(100))),
            Some(PaneId::new(200))
        );
        assert_eq!(
            view.next_waiting(Some(PaneId::new(200))),
            Some(PaneId::new(110))
        );
        assert_eq!(
            view.next_waiting(Some(PaneId::new(110))),
            Some(PaneId::new(100))
        );
        assert!(view.pane_state(PaneId::new(100), P::Working));
        assert_eq!(view.pane(PaneId::new(100)).needs_you_since, None);
        assert_eq!(view.next_waiting(None), Some(PaneId::new(200)));
    }

    #[test]
    fn rf_atn_05_a_pane_red_before_its_place_arrives_goes_last() {
        let mut view = demo();
        assert!(view.pane_state(PaneId::new(110), NEEDS));
        assert_eq!(
            view.waiting_panes(),
            vec![PaneId::new(100), PaneId::new(110)]
        );
    }

    #[test]
    fn rf_atn_05_nobody_waiting_means_no_jump() {
        let mut view = demo();
        assert!(view.pane_state(PaneId::new(100), P::Idle));
        assert_eq!(view.next_waiting(None), None);
        assert_eq!(view.next_waiting(Some(PaneId::new(100))), None);
    }

    #[test]
    fn rf_atn_03_the_counter_follows_every_state_change() {
        let mut view = demo();
        assert_eq!(view.needs_you_panes(), 1);
        for (id, state) in [(110, NEEDS), (200, NEEDS), (101, NEEDS)] {
            assert!(view.pane_state(PaneId::new(id), state));
        }
        assert_eq!(view.needs_you_panes(), 4);
        view.pane_closed(PaneId::new(101));
        let mut split = tab(10, 1, &[100]);
        split.focused = PaneId::new(100);
        view.tab_updated(&split);
        assert_eq!(view.needs_you_panes(), 3);
        view.tab_closed(TabId::new(20));
        assert_eq!(view.needs_you_panes(), 2);
    }

    #[test]
    fn rf_pan_18_the_snapshot_brings_the_profiles() {
        let snapshot = WorkspaceSnapshot {
            profiles: vec![
                ProfileInfo {
                    id: yien_proto::ProfileId::new("shell"),
                    name: "Shell".into(),
                },
                ProfileInfo {
                    id: yien_proto::ProfileId::new("claude"),
                    name: "Claude Code".into(),
                },
            ],
            ..WorkspaceSnapshot::default()
        };
        let view = WorkspaceView::from_snapshot(&snapshot);
        let names: Vec<&str> = view.profiles().iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Shell", "Claude Code"]);
    }

    #[test]
    fn rn_02_signal_lost_reaches_the_pane_view() {
        let mut view = demo();
        let mut info = pane(101, 10, P::Idle);
        info.signal_lost = true;
        view.pane_updated(&info);
        let p = view.pane(PaneId::new(101));
        assert!(p.signal_lost && p.state == AgentState::Idle);
    }

    #[test]
    fn a_tab_shows_its_most_urgent_pane() {
        let view = demo();
        let state = |id| view.tab_state(view.tab(TabId::new(id)).unwrap());
        assert_eq!(state(10), AgentState::NeedsYou);
        assert_eq!(state(11), AgentState::Working);
        assert_eq!(state(20), AgentState::Done);
    }

    #[test]
    fn a_tab_update_before_its_project_update_still_shows() {
        let mut view = demo();
        view.tab_updated(&tab(12, 1, &[120]));
        assert_eq!(view.projects()[0].tabs.last(), Some(&TabId::new(12)));
        assert_eq!(view.pane(PaneId::new(120)).name, "terminal");
        view.pane_updated(&pane(120, 12, P::Idle));
        assert_eq!(view.pane(PaneId::new(120)).name, "pane-120");
        view.project_updated(&project(1, &[12, 10, 11]));
        assert_eq!(tab_names(&view)[0], vec!["tab-12", "tab-10", "tab-11"]);
    }

    #[test]
    fn rf_pan_01_a_split_arrives_as_a_tab_update_with_the_new_layout() {
        let mut view = demo();
        let mut split = tab(11, 1, &[110, 111]);
        split.focused = PaneId::new(111);
        view.tab_updated(&split);
        view.pane_updated(&pane(111, 11, P::Idle));
        let t = view.tab(TabId::new(11)).unwrap();
        assert_eq!(t.focused, PaneId::new(111));
        assert_eq!(t.rects().len(), 2);
        assert_eq!(view.pane_count(), 5);
    }

    #[test]
    fn closing_the_selected_tab_selects_a_neighbour_and_drops_its_panes() {
        let mut view = demo();
        let gone = view.tab_closed(TabId::new(10));
        assert_eq!(gone, vec![PaneId::new(100), PaneId::new(101)]);
        assert_eq!(view.selected_tab().map(|t| t.id), Some(TabId::new(11)));
        assert_eq!(view.needs_you_panes(), 0);
        assert_eq!(view.projects()[0].tabs, vec![TabId::new(11)]);
    }

    #[test]
    fn closing_the_last_tab_of_a_project_falls_back_to_another_project() {
        let mut view = demo();
        view.set_active(Some(TabId::new(20)));
        view.tab_closed(TabId::new(20));
        assert_eq!(view.selected_tab().map(|t| t.id), Some(TabId::new(11)));
    }

    #[test]
    fn a_new_tab_after_every_tab_closed_opens_in_the_last_project() {
        let mut view = demo();
        view.select_tab(TabId::new(20));
        for id in [10, 11, 20] {
            view.tab_closed(TabId::new(id));
        }
        assert!(view.selected_tab().is_none());
        assert_eq!(view.project_for_new_tab(), Some(ProjectId::new(2)));
        let fresh = WorkspaceView::from_snapshot(&WorkspaceSnapshot {
            projects: vec![project(3, &[]), project(4, &[])],
            ..WorkspaceSnapshot::default()
        });
        assert_eq!(fresh.project_for_new_tab(), Some(ProjectId::new(3)));
        assert_eq!(WorkspaceView::default().project_for_new_tab(), None);
    }

    #[test]
    fn closing_the_only_tab_selects_nothing() {
        let mut view = WorkspaceView::from_snapshot(&WorkspaceSnapshot {
            projects: vec![project(1, &[10])],
            tabs: vec![tab(10, 1, &[100])],
            panes: vec![],
            active_tab: Some(TabId::new(10)),
            profiles: vec![],
            notifications_muted: false,
        });
        view.tab_closed(TabId::new(10));
        assert!(view.selected_tab().is_none());
        assert_eq!(view.tab_after(1), None);
    }

    #[test]
    fn stale_events_change_nothing() {
        let mut view = demo();
        assert!(view.tab_closed(TabId::new(99)).is_empty());
        assert!(!view.pane_state(PaneId::new(99), NEEDS));
        assert!(!view.pane_exited(PaneId::new(99), Some(1)));
        view.set_active(Some(TabId::new(99)));
        assert_eq!(view.selected_tab().map(|t| t.id), Some(TabId::new(10)));
        assert_eq!(view.needs_you_panes(), 1);

        view.pane_closed(PaneId::new(101));
        view.pane_updated(&pane(101, 10, P::Working));
        assert!(!view.pane_state(PaneId::new(101), NEEDS));

        assert_eq!(view.tab_closed(TabId::new(11)), vec![PaneId::new(110)]);
        view.tab_updated(&tab(11, 1, &[110]));
        view.pane_updated(&pane(110, 11, P::Working));
        assert!(view.tab(TabId::new(11)).is_none());
        assert!(!view.pane_state(PaneId::new(110), NEEDS));
        let first = &view.projects()[0];
        assert_eq!(first.tabs, vec![TabId::new(10)]);
        assert_eq!(view.working_panes(), 0);

        view.pane_updated(&pane(300, 99, P::Idle));
        assert!(!view.pane_state(PaneId::new(300), NEEDS));
    }

    #[test]
    fn rf_pan_19_an_exited_pane_keeps_its_code() {
        let mut view = demo();
        assert!(view.pane_exited(PaneId::new(110), Some(3)));
        let p = view.pane(PaneId::new(110));
        assert!(!p.running);
        assert!(p.exited);
        assert_eq!(p.exit_code, Some(3));
    }

    #[test]
    fn rf_pan_19_a_pane_still_starting_has_not_exited() {
        let mut view = demo();
        let mut starting = pane(110, 11, P::Idle);
        starting.running = false;
        view.pane_updated(&starting);
        assert!(!view.pane(PaneId::new(110)).exited);
    }

    #[test]
    fn rf_pan_19_an_update_after_a_failed_start_keeps_the_exit_and_a_relaunch_clears_it() {
        let mut view = demo();
        assert!(view.pane_exited(PaneId::new(110), None));
        let mut stopped = pane(110, 11, P::Idle);
        stopped.running = false;
        view.pane_updated(&stopped);
        assert!(view.pane(PaneId::new(110)).exited);
        view.pane_updated(&pane(110, 11, P::Idle));
        assert!(!view.pane(PaneId::new(110)).exited);
    }

    #[test]
    fn rf_esp_04_a_tab_row_shows_its_branch_then_a_detached_commit_then_its_folder() {
        use yien_proto::{GitFacts, GitHead};

        let mut view = demo();
        let id = TabId::new(10);
        let mut info = tab(10, 1, &[100, 101]);
        info.branch = Some("feat/x".into());
        view.tab_updated(&info);
        let detail = |view: &WorkspaceView| view.tab_detail(view.tab(id).unwrap());
        assert_eq!(detail(&view), "feat/x");

        info.branch = None;
        view.tab_updated(&info);
        let detached = TabGit::Repository(GitFacts {
            head: GitHead::Detached {
                commit: "0123456".into(),
            },
            upstream: None,
            root: info.dir.clone(),
            linked_worktree: false,
            main_worktree: None,
            files: vec![],
            changed: 0,
        });
        assert!(view.tab_git(id, detached.clone()));
        assert_eq!(detail(&view), "0123456");
        assert_eq!(view.git_of(id), Some(&detached));

        assert!(view.tab_git(id, TabGit::NoRepository));
        assert_eq!(detail(&view), info.dir);
    }

    #[test]
    fn git_facts_for_a_closed_or_unknown_tab_are_dropped() {
        let mut view = demo();
        assert!(view.tab_git(TabId::new(11), TabGit::NoRepository));
        view.tab_closed(TabId::new(11));
        assert!(!view.tab_git(TabId::new(11), TabGit::NoRepository));
        assert_eq!(view.git_of(TabId::new(11)), None);
        assert!(!view.tab_git(TabId::new(99), TabGit::NoRepository));
    }

    #[test]
    fn rf_nav_09_tab_after_walks_every_tab_and_wraps() {
        let mut view = demo();
        let mut seen = Vec::new();
        for _ in 0..3 {
            let next = view.tab_after(1).unwrap();
            view.select_tab(next);
            seen.push(next.get());
        }
        assert_eq!(seen, vec![11, 20, 10]);
        assert_eq!(view.tab_after(-1), Some(TabId::new(20)));
    }

    #[test]
    fn rf_nav_05_tab_n_counts_within_the_selected_project() {
        let mut view = demo();
        assert_eq!(view.nth_tab_of_selected_project(1), Some(TabId::new(10)));
        assert_eq!(view.nth_tab_of_selected_project(2), Some(TabId::new(11)));
        assert_eq!(view.nth_tab_of_selected_project(3), None);
        assert_eq!(view.nth_tab_of_selected_project(0), None);
        view.select_tab(TabId::new(20));
        assert_eq!(view.nth_tab_of_selected_project(1), Some(TabId::new(20)));
        assert_eq!(view.nth_tab_of_selected_project(2), None);
    }

    #[test]
    fn rf_nav_05_tabs_by_recent_put_the_last_selected_first() {
        let ids = |view: &WorkspaceView| -> Vec<u64> {
            view.tabs_by_recent().iter().map(|t| t.id.get()).collect()
        };
        let mut view = demo();
        assert_eq!(ids(&view), vec![10, 11, 20]);
        view.select_tab(TabId::new(20));
        view.set_active(Some(TabId::new(11)));
        assert_eq!(ids(&view), vec![11, 20, 10]);
        view.select_tab(TabId::new(20));
        assert_eq!(ids(&view), vec![20, 11, 10]);
        view.tab_closed(TabId::new(20));
        assert_eq!(ids(&view), vec![11, 10]);
        assert!(!view.select_tab(TabId::new(99)));
        assert_eq!(ids(&view), vec![11, 10]);
    }

    #[test]
    fn rf_nav_08_the_keyboard_walks_the_tab_rows_the_bar_draws() {
        let mut view = demo();
        let ids =
            |v: &WorkspaceView| -> Vec<u64> { v.drawn_tabs().iter().map(|t| t.get()).collect() };
        assert_eq!(ids(&view), vec![10, 11, 20]);
        view.toggle_fold(ProjectId::new(1));
        assert_eq!(ids(&view), vec![20]);
        let rows = view.drawn_tabs();
        let cursor = crate::region::move_cursor(&rows, Some(TabId::new(10)), None, 1);
        assert_eq!(cursor, Some(TabId::new(20)));
    }

    #[test]
    fn rf_esp_17_folding_hides_tabs_and_selecting_unfolds() {
        let mut view = demo();
        let second = ProjectId::new(2);
        assert_eq!(view.toggle_fold(second), Some(true));
        assert!(view.is_folded(second));
        assert!(view.select_tab(TabId::new(20)));
        assert!(!view.is_folded(second));
        assert_eq!(view.toggle_fold(second), Some(true));
        assert_eq!(view.toggle_fold(second), Some(false));
        assert_eq!(view.toggle_fold(ProjectId::new(99)), None);
        assert_eq!(view.first_tab_of(ProjectId::new(3)), None);
        assert_eq!(view.first_tab_of(second), Some(TabId::new(20)));
    }

    #[test]
    fn rf_esp_17_a_folded_project_still_shows_its_red_mark() {
        let mut view = demo();
        let first = ProjectId::new(1);
        let mut info = project(1, &[10, 11]);
        info.collapsed = true;
        view.project_updated(&info);
        assert!(view.is_folded(first));
        let head = view.project(first).unwrap().clone();
        assert_eq!(view.needs_you_tabs_of(&head), 1);
        assert_eq!(view.needs_you_panes(), 1);

        view.set_active(Some(TabId::new(11)));
        assert!(view.is_folded(first), "the core decides when it unfolds");
        info.collapsed = false;
        view.project_updated(&info);
        assert!(!view.is_folded(first));
    }

    #[test]
    fn rf_esp_01_adding_a_folder_that_is_a_project_selects_its_latest_tab() {
        let mut view = demo();
        let spelled = "C:/Code//project-1/";
        assert_eq!(
            view.folder_target(spelled),
            FolderTarget::Tab(TabId::new(10))
        );
        view.select_tab(TabId::new(11));
        view.select_tab(TabId::new(20));
        assert_eq!(
            view.folder_target(spelled),
            FolderTarget::Tab(TabId::new(11))
        );
        assert_eq!(
            view.folder_target("C:/Code/project-3"),
            FolderTarget::NewProject,
            "a project with no tab gets one from the core"
        );
        assert_eq!(
            view.folder_target("C:/Code/elsewhere"),
            FolderTarget::NewProject
        );
    }

    #[test]
    fn closing_the_last_project_leaves_the_empty_state() {
        let mut view = WorkspaceView::from_snapshot(&WorkspaceSnapshot {
            projects: vec![project(1, &[10])],
            tabs: vec![tab(10, 1, &[100])],
            panes: vec![pane(100, 10, P::Idle)],
            active_tab: Some(TabId::new(10)),
            profiles: vec![],
            notifications_muted: false,
        });
        assert!(!view.has_no_projects());
        assert_eq!(view.tab_closed(TabId::new(10)), vec![PaneId::new(100)]);
        assert!(view.project_closed(ProjectId::new(1)).is_empty());
        view.set_active(None);
        assert!(view.has_no_projects());
        assert!(view.selected_tab().is_none());
        assert_eq!(view.project_for_new_tab(), None);
        assert_eq!(view.pane_count(), 0);
    }

    #[test]
    fn a_closed_project_takes_tabs_the_core_has_not_closed_yet() {
        let mut view = demo();
        let gone = view.project_closed(ProjectId::new(1));
        assert_eq!(gone.len(), 3);
        assert_eq!(view.projects().len(), 2);
        assert_eq!(view.selected_tab().map(|t| t.id), Some(TabId::new(20)));
        view.tab_updated(&tab(10, 1, &[100]));
        assert!(view.tab(TabId::new(10)).is_none());
    }

    #[test]
    fn only_agents_and_work_in_progress_ask_before_a_project_closes() {
        let mut view = demo();
        // 100 needs you and 110 works; 101 is a shell at its prompt.
        assert_eq!(view.busy_panes_of(ProjectId::new(1)), 2);
        assert_eq!(view.busy_panes_of(ProjectId::new(2)), 0);
        assert_eq!(view.busy_panes_of(ProjectId::new(3)), 0);

        let mut claude = pane(200, 20, P::Idle);
        claude.profile_id = yien_proto::ProfileId::new("claude");
        view.pane_updated(&claude);
        assert_eq!(view.busy_panes_of(ProjectId::new(2)), 1);
        claude.running = false;
        claude.exit_code = Some(0);
        view.pane_updated(&claude);
        assert_eq!(view.busy_panes_of(ProjectId::new(2)), 0, "an exited agent");
    }

    #[test]
    fn rf_esp_16_an_unchanged_name_is_not_sent() {
        assert_eq!(typed_tab_name(" Tab 1 ", Some("Tab 1")), None);
        assert_eq!(
            typed_tab_name("review", Some("Tab 1")),
            Some("review".into())
        );
        assert_eq!(typed_tab_name("  ", Some("review")), Some(String::new()));
        assert_eq!(typed_tab_name("x", None), Some("x".into()));
    }

    #[test]
    fn rf_pan_04_a_maximized_pane_fills_the_tab_alone() {
        let mut info = tab(10, 1, &[100, 101]);
        info.maximized = Some(PaneId::new(101));
        let t = TabView::from_info(&info);
        assert_eq!(t.rects(), vec![(PaneId::new(101), Rect::FULL)]);
        info.maximized = Some(PaneId::new(7));
        assert_eq!(TabView::from_info(&info).rects().len(), 2);
    }

    #[test]
    fn the_tab_of_a_pane_is_found_through_layouts() {
        let view = demo();
        assert_eq!(
            view.tab_of(PaneId::new(101)).map(|t| t.id),
            Some(TabId::new(10))
        );
        assert!(view.tab_of(PaneId::new(5)).is_none());
    }

    #[test]
    fn a_frame_announced_before_its_view_exists_is_taken_once_it_is_drawn() {
        let pane = PaneId::new(120);
        let mut views: HashSet<PaneId> = HashSet::new();
        let ready = BTreeSet::from([pane]);
        assert!(frames_to_take(&ready, false, &[], |p| views.contains(&p)).is_empty());

        views.insert(pane);
        let hidden = frames_to_take(&BTreeSet::new(), true, &[PaneId::new(100)], |p| {
            views.contains(&p)
        });
        assert!(hidden.is_empty());

        let shown = frames_to_take(&BTreeSet::new(), true, &[pane], |p| views.contains(&p));
        assert_eq!(shown, vec![pane]);
    }

    #[test]
    fn a_tab_switch_takes_every_pane_it_shows_and_each_once() {
        let views: HashSet<PaneId> = [100, 101, 110].map(PaneId::new).into();
        let ready = BTreeSet::from([PaneId::new(100), PaneId::new(110)]);
        let visible = [PaneId::new(100), PaneId::new(101)];
        assert_eq!(
            frames_to_take(&ready, true, &visible, |p| views.contains(&p)),
            vec![PaneId::new(100), PaneId::new(101), PaneId::new(110)]
        );
        assert_eq!(
            frames_to_take(&BTreeSet::new(), false, &visible, |p| views.contains(&p)),
            Vec::<PaneId>::new()
        );
    }
}
