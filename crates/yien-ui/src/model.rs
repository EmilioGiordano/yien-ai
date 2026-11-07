//! The view model the window draws: projects, their tabs and each tab's panes.
//!
//! Plain data keyed by `yien-proto` ids, kept up to date from the core's
//! events (ADR-03). Events may arrive in any order and may name things the
//! UI has already dropped; updates tolerate both.

use std::collections::{BTreeSet, HashMap, HashSet};

use yien_proto::{
    LayoutNode, PaneId, PaneInfo, ProfileInfo, ProjectId, ProjectInfo, TabId, TabInfo,
};

use crate::layout::{Rect, pane_rects};

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

/// One pane, as its title bar shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneView {
    /// The pane's id in the protocol.
    pub id: PaneId,
    /// What runs in it: `claude`, `pwsh`, `cargo test`.
    pub name: String,
    /// The profile it was started with: `Claude Code`, `Shell`.
    pub profile: String,
    /// What its agent is doing.
    pub state: AgentState,
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
}

impl PaneView {
    /// A pane the layout names before its details arrive.
    fn placeholder(id: PaneId) -> Self {
        Self {
            id,
            name: "terminal".to_owned(),
            profile: String::new(),
            state: AgentState::Idle,
            running: true,
            exit_code: None,
            exited: false,
            needs_you_since: None,
            signal_lost: false,
        }
    }

    fn from_info(info: &PaneInfo) -> Self {
        Self {
            id: info.id,
            name: info.name.clone(),
            profile: info.profile.clone(),
            state: info.state.into(),
            running: info.running,
            exit_code: info.exit_code,
            exited: !info.running && info.exit_code.is_some(),
            needs_you_since: info.needs_you_since,
            signal_lost: info.signal_lost,
        }
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
///     projects: vec![ProjectInfo { id: ProjectId::new(1), name: "yien".into(), root: "C:/yien".into(), tabs: vec![TabId::new(2)] }],
///     tabs: vec![TabInfo {
///         id: TabId::new(2), project: ProjectId::new(1), name: "Tab 1".into(), dir: "C:/yien".into(),
///         branch: None, layout: LayoutNode::Pane(PaneId::new(3)), focused: PaneId::new(3), maximized: None,
///     }],
///     panes: vec![],
///     active_tab: Some(TabId::new(2)),
///     profiles: vec![],
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
}

impl WorkspaceView {
    /// The workspace as the core's first event describes it.
    pub fn from_snapshot(snapshot: &yien_proto::WorkspaceSnapshot) -> Self {
        let mut view = Self {
            profiles: snapshot.profiles.clone(),
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

    /// The core changed the selected tab. An unknown tab is ignored.
    pub fn set_active(&mut self, tab: Option<TabId>) {
        match tab {
            Some(id) => {
                self.select_tab(id);
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
            })
            .is_some()
    }

    /// Every project, in tab-bar order.
    pub fn projects(&self) -> &[ProjectView] {
        &self.projects
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

    /// Every pane of every tab, in tab-bar and tree order.
    pub fn all_panes(&self) -> Vec<PaneId> {
        self.tabs_in_order()
            .flat_map(|t| t.layout.panes())
            .collect()
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

    /// Selects a tab and unfolds its project. `false` when no tab has that id.
    pub fn select_tab(&mut self, id: TabId) -> bool {
        let Some(project) = self.tabs.get(&id).map(|t| t.project) else {
            return false;
        };
        self.selected = Some(id);
        self.folded.remove(&project);
        true
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

    /// Folds or unfolds a project in the tab bar (RF-ESP-17). `false` when the
    /// project is unknown.
    pub fn toggle_fold(&mut self, id: ProjectId) -> bool {
        if !self.projects.iter().any(|p| p.id == id) {
            return false;
        }
        if !self.folded.remove(&id) {
            self.folded.insert(id);
        }
        true
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
        }
    }

    fn project(id: u64, tabs: &[u64]) -> ProjectInfo {
        ProjectInfo {
            id: ProjectId::new(id),
            name: format!("project-{id}"),
            root: format!("C:/Code/project-{id}"),
            tabs: tabs.iter().map(|t| TabId::new(*t)).collect(),
        }
    }

    fn pane(id: u64, tab: u64, state: P) -> PaneInfo {
        PaneInfo {
            id: PaneId::new(id),
            tab: TabId::new(tab),
            name: format!("pane-{id}"),
            profile: "Shell".into(),
            title: None,
            cwd: None,
            state,
            running: true,
            exit_code: None,
            session_id: None,
            needs_you_since: state.is_red().then_some(id),
            signal_lost: false,
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
        })
    }

    fn tab_names(view: &WorkspaceView) -> Vec<Vec<String>> {
        view.projects()
            .iter()
            .map(|p| view.tabs_of(p).map(|t| t.name.clone()).collect())
            .collect()
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
    fn closing_the_only_tab_selects_nothing() {
        let mut view = WorkspaceView::from_snapshot(&WorkspaceSnapshot {
            projects: vec![project(1, &[10])],
            tabs: vec![tab(10, 1, &[100])],
            panes: vec![],
            active_tab: Some(TabId::new(10)),
            profiles: vec![],
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
    fn rf_esp_17_folding_hides_tabs_and_selecting_unfolds() {
        let mut view = demo();
        let second = ProjectId::new(2);
        assert!(view.toggle_fold(second));
        assert!(view.is_folded(second));
        assert!(view.select_tab(TabId::new(20)));
        assert!(!view.is_folded(second));
        assert!(!view.toggle_fold(ProjectId::new(99)));
        assert_eq!(view.first_tab_of(ProjectId::new(3)), None);
        assert_eq!(view.first_tab_of(second), Some(TabId::new(20)));
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
