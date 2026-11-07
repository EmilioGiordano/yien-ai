//! The view model the window draws: projects, their tabs and each tab's panes.
//!
//! Plain data keyed by `yien-proto` ids, so the core can fill it later without
//! the UI knowing where it came from (ADR-03).

use std::collections::HashSet;

use yien_layout::{Rect, Tree};
use yien_proto::{PaneId, ProjectId, TabId};

/// What the agent in a pane is doing (RF-AGE-05).
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

/// One pane of a tab, as its title bar shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneView {
    /// The pane's id in the protocol.
    pub id: PaneId,
    /// The leaf that holds this pane in its tab's [`TabView::layout`].
    pub leaf: yien_layout::PaneId,
    /// What runs in it: `claude`, `pwsh`, `cargo test`.
    pub name: String,
    /// The profile it was started with: `Claude Code`, `PowerShell`.
    pub profile: String,
    /// What its agent is doing.
    pub state: AgentState,
}

/// One tab: a working directory, maybe a branch, and its panes.
#[derive(Debug, Clone)]
pub struct TabView {
    /// The tab's id in the protocol.
    pub id: TabId,
    /// The name in the tab bar.
    pub name: String,
    /// The working directory every pane starts in (RF-ESP-04).
    pub dir: String,
    /// The git branch checked out in `dir`, when it is a repository (RF-ESP-04).
    pub branch: Option<String>,
    /// How the panes split the tab.
    pub layout: Tree,
    /// One per leaf of `layout`.
    pub panes: Vec<PaneView>,
    /// The pane that has the keyboard.
    pub focused: PaneId,
}

impl TabView {
    /// The tab's state: that of its most urgent pane.
    pub fn state(&self) -> AgentState {
        self.panes
            .iter()
            .map(|p| p.state)
            .min_by_key(|s| s.urgency())
            .unwrap_or_default()
    }

    /// How many of its panes need the user.
    pub fn needs_you_panes(&self) -> usize {
        self.panes.iter().filter(|p| p.state.needs_you()).count()
    }

    /// Each pane with its rectangle as fractions of the pane area, in tree order.
    pub fn pane_rects(&self) -> Vec<(&PaneView, Rect)> {
        let rects = self.layout.layout(Rect::new(0.0, 0.0, 1.0, 1.0));
        self.layout
            .panes()
            .into_iter()
            .filter_map(|leaf| {
                let pane = self.panes.iter().find(|p| p.leaf == leaf)?;
                Some((pane, *rects.get(&leaf)?))
            })
            .collect()
    }
}

/// One project in the tab bar, with its tabs nested under it (RF-ESP-02).
#[derive(Debug, Clone)]
pub struct ProjectView {
    /// The project's id in the protocol.
    pub id: ProjectId,
    /// The name in the tab bar.
    pub name: String,
    /// The folder it was registered from.
    pub root: String,
    /// Its tabs, in tab-bar order.
    pub tabs: Vec<TabView>,
}

impl ProjectView {
    /// How many of its tabs have a pane that needs the user.
    pub fn needs_you_tabs(&self) -> usize {
        self.tabs.iter().filter(|t| t.state().needs_you()).count()
    }
}

/// Why a [`WorkspaceView`] could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ViewError {
    /// Two projects share an id.
    #[error("project {} appears twice", .0.get())]
    DuplicateProject(ProjectId),
    /// Two tabs share an id.
    #[error("tab {} appears twice", .0.get())]
    DuplicateTab(TabId),
    /// Two panes share an id.
    #[error("pane {} appears twice", .0.get())]
    DuplicatePane(PaneId),
    /// A tab's panes are not exactly the leaves of its layout.
    #[error("the panes of tab {} do not match its layout", .0.get())]
    LayoutMismatch(TabId),
    /// A tab's focused pane is not one of its panes.
    #[error("tab {} focuses a pane it does not hold", .0.get())]
    UnknownFocus(TabId),
}

/// Everything the window shows, and which tab is selected.
///
/// ```
/// use yien_layout::Tree;
/// use yien_proto::{PaneId, ProjectId, TabId};
/// use yien_ui::{AgentState, PaneView, ProjectView, TabView, WorkspaceView};
///
/// let pane = PaneView {
///     id: PaneId::new(1),
///     leaf: 0,
///     name: "claude".into(),
///     profile: "Claude Code".into(),
///     state: AgentState::NeedsYou,
/// };
/// let tab = TabView {
///     id: TabId::new(1),
///     name: "main".into(),
///     dir: "C:/Code/yien".into(),
///     branch: Some("main".into()),
///     layout: Tree::new(),
///     panes: vec![pane],
///     focused: PaneId::new(1),
/// };
/// let project = ProjectView {
///     id: ProjectId::new(1),
///     name: "yien".into(),
///     root: "C:/Code/yien".into(),
///     tabs: vec![tab],
/// };
/// let view = WorkspaceView::new(vec![project]).unwrap();
/// assert_eq!(view.selected_tab().map(|t| t.id), Some(TabId::new(1)));
/// assert_eq!(view.needs_you_panes(), 1);
/// ```
#[derive(Debug, Clone)]
pub struct WorkspaceView {
    projects: Vec<ProjectView>,
    selected: Option<TabId>,
    folded: HashSet<ProjectId>,
}

impl WorkspaceView {
    /// Checks the ids and layouts, and selects the first tab.
    pub fn new(projects: Vec<ProjectView>) -> Result<Self, ViewError> {
        let mut project_ids = HashSet::new();
        let mut tab_ids = HashSet::new();
        let mut pane_ids = HashSet::new();
        for project in &projects {
            if !project_ids.insert(project.id) {
                return Err(ViewError::DuplicateProject(project.id));
            }
            for tab in &project.tabs {
                if !tab_ids.insert(tab.id) {
                    return Err(ViewError::DuplicateTab(tab.id));
                }
                for pane in &tab.panes {
                    if !pane_ids.insert(pane.id) {
                        return Err(ViewError::DuplicatePane(pane.id));
                    }
                }
                let mut leaves = tab.layout.panes();
                let mut held: Vec<_> = tab.panes.iter().map(|p| p.leaf).collect();
                leaves.sort_unstable();
                held.sort_unstable();
                if leaves != held {
                    return Err(ViewError::LayoutMismatch(tab.id));
                }
                if !tab.panes.iter().any(|p| p.id == tab.focused) {
                    return Err(ViewError::UnknownFocus(tab.id));
                }
            }
        }
        let selected = projects.iter().flat_map(|p| &p.tabs).map(|t| t.id).next();
        Ok(Self {
            projects,
            selected,
            folded: HashSet::new(),
        })
    }

    /// Every project, in tab-bar order.
    pub fn projects(&self) -> &[ProjectView] {
        &self.projects
    }

    /// The selected tab, if there is any tab at all.
    pub fn selected_tab(&self) -> Option<&TabView> {
        let id = self.selected?;
        self.tabs().find(|t| t.id == id)
    }

    /// The project of the selected tab.
    pub fn selected_project(&self) -> Option<&ProjectView> {
        let id = self.selected?;
        self.projects
            .iter()
            .find(|p| p.tabs.iter().any(|t| t.id == id))
    }

    /// Selects a tab and unfolds its project. `false` when no tab has that id.
    pub fn select_tab(&mut self, id: TabId) -> bool {
        let Some(project) = self
            .projects
            .iter()
            .find(|p| p.tabs.iter().any(|t| t.id == id))
            .map(|p| p.id)
        else {
            return false;
        };
        self.selected = Some(id);
        self.folded.remove(&project);
        true
    }

    /// Selects a project's first tab. `false` when the project is unknown or
    /// has no tabs.
    pub fn select_project(&mut self, id: ProjectId) -> bool {
        let first = self
            .projects
            .iter()
            .find(|p| p.id == id)
            .and_then(|p| p.tabs.first())
            .map(|t| t.id);
        first.is_some_and(|tab| self.select_tab(tab))
    }

    /// Moves the selection to the next tab in tab-bar order, wrapping around.
    pub fn select_next_tab(&mut self) -> bool {
        self.step(1)
    }

    /// Moves the selection to the previous tab in tab-bar order, wrapping around.
    pub fn select_previous_tab(&mut self) -> bool {
        self.step(-1)
    }

    fn step(&mut self, by: isize) -> bool {
        let ids: Vec<TabId> = self.tabs().map(|t| t.id).collect();
        let Some(len) = isize::try_from(ids.len()).ok().filter(|&n| n > 0) else {
            return false;
        };
        let at = self
            .selected
            .and_then(|s| ids.iter().position(|&id| id == s))
            .and_then(|i| isize::try_from(i).ok());
        let next = match at {
            Some(i) => (i + by).rem_euclid(len),
            None => 0,
        };
        usize::try_from(next)
            .ok()
            .and_then(|i| ids.get(i).copied())
            .is_some_and(|id| self.select_tab(id))
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

    /// Panes whose agent needs the user: the global counter (RF-ATN-03).
    pub fn needs_you_panes(&self) -> usize {
        self.tabs().map(TabView::needs_you_panes).sum()
    }

    /// Tabs with at least one pane that needs the user.
    pub fn needs_you_tabs(&self) -> usize {
        self.projects.iter().map(ProjectView::needs_you_tabs).sum()
    }

    /// Panes whose agent is working.
    pub fn working_panes(&self) -> usize {
        self.tabs()
            .flat_map(|t| &t.panes)
            .filter(|p| p.state == AgentState::Working)
            .count()
    }

    /// How many tabs there are.
    pub fn tab_count(&self) -> usize {
        self.tabs().count()
    }

    /// How many panes there are.
    pub fn pane_count(&self) -> usize {
        self.tabs().map(|t| t.panes.len()).sum()
    }

    fn tabs(&self) -> impl Iterator<Item = &TabView> {
        self.projects.iter().flat_map(|p| &p.tabs)
    }
}

#[cfg(test)]
mod tests {
    use yien_layout::Orientation;

    use super::*;

    fn pane(id: u64, leaf: yien_layout::PaneId, state: AgentState) -> PaneView {
        PaneView {
            id: PaneId::new(id),
            leaf,
            name: format!("pane-{id}"),
            profile: "Claude Code".into(),
            state,
        }
    }

    fn tab(id: u64, states: &[AgentState]) -> TabView {
        let mut layout = Tree::new();
        let mut leaves = vec![0];
        for _ in 1..states.len() {
            let last = *leaves.last().unwrap();
            leaves.push(layout.split(last, Orientation::Horizontal).unwrap());
        }
        let panes: Vec<_> = leaves
            .iter()
            .zip(states)
            .enumerate()
            .map(|(i, (&leaf, &state))| pane(id * 100 + i as u64, leaf, state))
            .collect();
        TabView {
            id: TabId::new(id),
            name: format!("tab-{id}"),
            dir: format!("C:/Code/tab-{id}"),
            branch: Some(format!("feat/tab-{id}")),
            layout,
            focused: panes[0].id,
            panes,
        }
    }

    fn project(id: u64, tabs: Vec<TabView>) -> ProjectView {
        ProjectView {
            id: ProjectId::new(id),
            name: format!("project-{id}"),
            root: format!("C:/Code/project-{id}"),
            tabs,
        }
    }

    use super::AgentState::{Done, Error, Idle, NeedsYou, Working};

    fn demo() -> WorkspaceView {
        WorkspaceView::new(vec![
            project(1, vec![tab(1, &[NeedsYou, Idle]), tab(2, &[Working])]),
            project(2, vec![tab(3, &[Done]), tab(4, &[NeedsYou, NeedsYou])]),
            project(3, vec![]),
            project(4, vec![tab(5, &[Error])]),
        ])
        .unwrap()
    }

    #[test]
    fn rf_esp_02_projects_keep_their_tabs_nested_in_order() {
        let view = demo();
        let names: Vec<Vec<&str>> = view
            .projects()
            .iter()
            .map(|p| p.tabs.iter().map(|t| t.name.as_str()).collect())
            .collect();
        assert_eq!(
            names,
            vec![
                vec!["tab-1", "tab-2"],
                vec!["tab-3", "tab-4"],
                vec![],
                vec!["tab-5"]
            ]
        );
        assert_eq!(view.tab_count(), 5);
        assert_eq!(view.pane_count(), 7);
    }

    #[test]
    fn rf_esp_04_a_tab_carries_its_directory_and_optional_branch() {
        let mut no_branch = tab(9, &[Idle]);
        no_branch.branch = None;
        let view = WorkspaceView::new(vec![project(1, vec![tab(1, &[Idle]), no_branch])]).unwrap();
        let tabs = &view.projects()[0].tabs;
        assert_eq!(tabs[0].dir, "C:/Code/tab-1");
        assert_eq!(tabs[0].branch.as_deref(), Some("feat/tab-1"));
        assert_eq!(tabs[1].branch, None);
    }

    #[test]
    fn the_first_tab_is_selected_even_after_an_empty_project() {
        let view = WorkspaceView::new(vec![project(1, vec![]), project(2, vec![tab(7, &[Idle])])])
            .unwrap();
        assert_eq!(view.selected_tab().map(|t| t.id), Some(TabId::new(7)));
        assert_eq!(view.selected_project().map(|p| p.id), Some(ProjectId::new(2)));
    }

    #[test]
    fn a_workspace_without_tabs_selects_nothing() {
        let mut view = WorkspaceView::new(vec![project(1, vec![])]).unwrap();
        assert!(view.selected_tab().is_none());
        assert!(view.selected_project().is_none());
        assert!(!view.select_next_tab());
        assert!(!view.select_project(ProjectId::new(1)));
        assert_eq!(view.needs_you_panes(), 0);
    }

    #[test]
    fn a_tab_shows_its_most_urgent_pane() {
        assert_eq!(tab(1, &[Idle, Working, Done]).state(), Done);
        assert_eq!(tab(1, &[Working, Error]).state(), Error);
        assert_eq!(tab(1, &[Done, NeedsYou, Error]).state(), NeedsYou);
        assert_eq!(tab(1, &[Idle]).state(), Idle);
    }

    #[test]
    fn rn_02_an_unknown_state_is_idle_not_needs_you() {
        assert_eq!(AgentState::default(), Idle);
        assert!(!AgentState::default().needs_you());
        let mut empty = tab(1, &[Idle]);
        empty.panes.clear();
        assert_eq!(empty.state(), Idle);
    }

    #[test]
    fn rf_atn_02_a_pane_that_needs_you_marks_its_tab_and_project() {
        let view = demo();
        let first = &view.projects()[0];
        assert!(first.tabs[0].state().needs_you());
        assert!(!first.tabs[1].state().needs_you());
        assert_eq!(first.needs_you_tabs(), 1);
        assert_eq!(view.projects()[1].needs_you_tabs(), 1);
        assert_eq!(view.projects()[3].needs_you_tabs(), 0);
    }

    #[test]
    fn rf_atn_03_the_counter_counts_agents_not_tabs() {
        let view = demo();
        assert_eq!(view.needs_you_panes(), 3);
        assert_eq!(view.needs_you_tabs(), 2);
        assert_eq!(view.working_panes(), 1);
    }

    #[test]
    fn rf_atn_03_nobody_waiting_counts_zero() {
        let view = WorkspaceView::new(vec![project(1, vec![tab(1, &[Working, Done, Error])])])
            .unwrap();
        assert_eq!(view.needs_you_panes(), 0);
        assert_eq!(view.needs_you_tabs(), 0);
    }

    #[test]
    fn selecting_a_tab_in_another_project_moves_the_project() {
        let mut view = demo();
        assert_eq!(view.selected_project().map(|p| p.id), Some(ProjectId::new(1)));
        assert!(view.select_tab(TabId::new(4)));
        assert_eq!(view.selected_tab().map(|t| t.id), Some(TabId::new(4)));
        assert_eq!(view.selected_project().map(|p| p.id), Some(ProjectId::new(2)));
    }

    #[test]
    fn selecting_an_unknown_tab_or_project_changes_nothing() {
        let mut view = demo();
        assert!(!view.select_tab(TabId::new(99)));
        assert!(!view.select_project(ProjectId::new(99)));
        assert!(!view.select_project(ProjectId::new(3)));
        assert_eq!(view.selected_tab().map(|t| t.id), Some(TabId::new(1)));
    }

    #[test]
    fn selecting_a_project_selects_its_first_tab() {
        let mut view = demo();
        assert!(view.select_project(ProjectId::new(4)));
        assert_eq!(view.selected_tab().map(|t| t.id), Some(TabId::new(5)));
    }

    #[test]
    fn rf_nav_09_next_and_previous_walk_every_tab_and_wrap() {
        let mut view = demo();
        let mut seen = Vec::new();
        for _ in 0..5 {
            assert!(view.select_next_tab());
            seen.push(view.selected_tab().unwrap().id.get());
        }
        assert_eq!(seen, vec![2, 3, 4, 5, 1]);
        assert!(view.select_previous_tab());
        assert_eq!(view.selected_tab().map(|t| t.id), Some(TabId::new(5)));
    }

    #[test]
    fn rf_esp_17_folding_hides_tabs_and_selecting_unfolds() {
        let mut view = demo();
        let second = ProjectId::new(2);
        assert!(view.toggle_fold(second));
        assert!(view.is_folded(second));
        assert_eq!(view.projects()[1].needs_you_tabs(), 1);
        assert!(view.select_tab(TabId::new(3)));
        assert!(!view.is_folded(second));
        assert!(view.toggle_fold(second));
        assert!(view.toggle_fold(second));
        assert!(!view.is_folded(second));
        assert!(!view.toggle_fold(ProjectId::new(99)));
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let err = WorkspaceView::new(vec![project(1, vec![]), project(1, vec![])]).unwrap_err();
        assert_eq!(err, ViewError::DuplicateProject(ProjectId::new(1)));

        let err = WorkspaceView::new(vec![project(1, vec![tab(1, &[Idle])]), project(2, vec![tab(1, &[Idle])])])
            .unwrap_err();
        assert_eq!(err, ViewError::DuplicateTab(TabId::new(1)));

        let mut clash = tab(2, &[Idle]);
        clash.panes[0].id = PaneId::new(100);
        clash.focused = PaneId::new(100);
        let err = WorkspaceView::new(vec![project(1, vec![tab(1, &[Idle]), clash])]).unwrap_err();
        assert_eq!(err, ViewError::DuplicatePane(PaneId::new(100)));
    }

    #[test]
    fn panes_must_match_the_layout_leaves() {
        let mut missing = tab(1, &[Idle, Idle]);
        missing.panes.pop();
        let err = WorkspaceView::new(vec![project(1, vec![missing])]).unwrap_err();
        assert_eq!(err, ViewError::LayoutMismatch(TabId::new(1)));

        let mut stray = tab(1, &[Idle]);
        stray.panes[0].leaf = 42;
        let err = WorkspaceView::new(vec![project(1, vec![stray])]).unwrap_err();
        assert_eq!(err, ViewError::LayoutMismatch(TabId::new(1)));
    }

    #[test]
    fn the_focused_pane_must_belong_to_the_tab() {
        let mut lost = tab(1, &[Idle]);
        lost.focused = PaneId::new(7);
        let err = WorkspaceView::new(vec![project(1, vec![lost])]).unwrap_err();
        assert_eq!(err, ViewError::UnknownFocus(TabId::new(1)));
        assert_eq!(err.to_string(), "tab 1 focuses a pane it does not hold");
    }

    #[test]
    fn pane_rects_cover_the_area_in_tree_order() {
        let mut t = tab(1, &[NeedsYou, Idle]);
        let below = t.layout.split(t.panes[1].leaf, Orientation::Vertical).unwrap();
        t.panes.push(pane(199, below, Working));
        let rects = t.pane_rects();
        let ids: Vec<u64> = rects.iter().map(|(p, _)| p.id.get()).collect();
        assert_eq!(ids, vec![100, 101, 199]);
        let area: f32 = rects.iter().map(|(_, r)| r.w * r.h).sum();
        assert!((area - 1.0).abs() < 1e-5);
        assert_eq!(rects[0].1, Rect::new(0.0, 0.0, 0.5, 1.0));
        assert_eq!(rects[2].1, Rect::new(0.5, 0.5, 0.5, 0.5));
    }
}
