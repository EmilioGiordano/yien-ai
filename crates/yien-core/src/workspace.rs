//! The workspace model: projects, tabs, layouts and pane details. Pure: every
//! change returns the effects the service must carry out (messages for the UI,
//! panes to start or stop), so the invariants are testable without a PTY.

use std::collections::{BTreeMap, HashMap};

use yien_layout::{Rect, Tree};
use yien_proto::{
    AgentState, Command, CoreMsg, PaneId, PaneInfo, ProfileId, ProjectId, ProjectInfo, TabId,
    TabInfo, WorkspaceSnapshot,
};

use crate::CoreError;
use crate::convert;
use crate::launch::LaunchPlan;

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
    },
    /// Stop a pane's actor; its program is terminated.
    Stop { pane: PaneId },
    /// Start the pane's program again.
    Relaunch { pane: PaneId },
    /// Terminate the pane's program, keeping the pane.
    Terminate { pane: PaneId },
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
    State(AgentState),
    Title(Option<String>),
    Cwd(String),
    SessionId(String),
    Started,
    Exited(Option<u32>),
    /// The program could not start or can no longer be shown; the actor
    /// still runs and can relaunch it.
    Failed(String),
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
            PaneEvent::State(_)
                | PaneEvent::Title(_)
                | PaneEvent::Cwd(_)
                | PaneEvent::SessionId(_)
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
}

#[derive(Debug, Clone)]
struct Tab {
    id: TabId,
    project: ProjectId,
    name: String,
    dir: String,
    tree: Tree,
    leaves: HashMap<yien_layout::PaneId, PaneId>,
    focused: PaneId,
    maximized: Option<PaneId>,
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
    custom_name: Option<String>,
    default_name: String,
    profile_name: String,
    title: Option<String>,
    cwd: Option<String>,
    state: AgentState,
    running: bool,
    exit_code: Option<u32>,
    session_id: Option<String>,
    visible: bool,
    seen: bool,
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
}

impl Workspace {
    pub fn new() -> Self {
        Self::default()
    }

    fn next(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn snapshot(&self) -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            projects: self.projects.iter().map(project_info).collect(),
            tabs: self.tabs.iter().map(|t| self.tab_info(t)).collect(),
            panes: self.panes.iter().map(|(id, p)| pane_info(*id, p)).collect(),
            active_tab: self.active_tab,
        }
    }

    fn tab_info(&self, tab: &Tab) -> TabInfo {
        let layout = convert::layout(&tab.tree.shape(), &|leaf| tab.leaves.get(&leaf).copied())
            .unwrap_or(yien_proto::LayoutNode::Pane(tab.focused));
        TabInfo {
            id: tab.id,
            project: tab.project,
            name: tab.name.clone(),
            dir: tab.dir.clone(),
            branch: None,
            layout,
            focused: tab.focused,
            maximized: tab.maximized,
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

    fn set_active(&mut self, tab: Option<TabId>, fx: &mut Vec<Effect>) {
        if self.active_tab != tab {
            self.active_tab = tab;
            fx.push(Effect::Emit(CoreMsg::ActiveTab { tab }));
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
                let plan = resolve(None)?;
                let id = ProjectId::new(self.next());
                let name = name
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| folder_name(&root));
                self.projects.push(Project {
                    id,
                    name,
                    root,
                    tabs: Vec::new(),
                });
                self.new_tab(id, None, plan, &mut fx)?;
            }
            Command::NewTab { project, name } => {
                if !self.projects.iter().any(|p| p.id == project) {
                    return Err(CoreError::UnknownProject(project));
                }
                let plan = resolve(None)?;
                self.new_tab(project, name, plan, &mut fx)?;
            }
            Command::CloseTab { tab } => self.close_tab(tab, &mut fx)?,
            Command::SelectTab { tab } => {
                self.tab(tab)?;
                self.set_active(Some(tab), &mut fx);
            }
            Command::SplitPane {
                pane,
                orientation,
                profile,
            } => {
                let tab_id = self.tab_of(pane)?;
                let plan = resolve(profile.as_ref())?;
                let new = PaneId::new(self.next());
                let tab = self.tab_mut(tab_id)?;
                let leaf = tab.leaf_of(pane).ok_or(CoreError::UnknownPane(pane))?;
                let new_leaf = tab
                    .tree
                    .split(leaf, convert::orientation(orientation))
                    .ok_or(CoreError::UnknownPane(pane))?;
                tab.leaves.insert(new_leaf, new);
                tab.maximized = None;
                let cwd = tab.dir.clone();
                self.add_pane(new, tab_id, plan, cwd, &mut fx);
                self.emit_pane(new, &mut fx);
                self.focus(new, &mut fx)?;
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
                fx.push(Effect::Relaunch { pane });
            }
            Command::TerminatePane { pane } => {
                self.tab_of(pane)?;
                fx.push(Effect::Terminate { pane });
            }
        }
        self.refresh_views(&mut fx);
        debug_assert_eq!(self.check(), Ok(()));
        Ok(fx)
    }

    fn same_tab(&self, a: PaneId, b: PaneId) -> Result<TabId, CoreError> {
        let (ta, tb) = (self.tab_of(a)?, self.tab_of(b)?);
        if ta != tb {
            return Err(CoreError::DifferentTabs(a, b));
        }
        Ok(ta)
    }

    fn new_tab(
        &mut self,
        project: ProjectId,
        name: Option<String>,
        plan: LaunchPlan,
        fx: &mut Vec<Effect>,
    ) -> Result<(), CoreError> {
        let tab_id = TabId::new(self.next());
        let pane = PaneId::new(self.next());
        let p = self
            .projects
            .iter_mut()
            .find(|p| p.id == project)
            .ok_or(CoreError::UnknownProject(project))?;
        p.tabs.push(tab_id);
        let name = name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| format!("Tab {}", p.tabs.len()));
        let dir = p.root.clone();
        let tree = Tree::new();
        let leaves = tree.panes().into_iter().map(|leaf| (leaf, pane)).collect();
        self.tabs.push(Tab {
            id: tab_id,
            project,
            name,
            dir: dir.clone(),
            tree,
            leaves,
            focused: pane,
            maximized: None,
        });
        if let Some(p) = self.projects.iter().find(|p| p.id == project) {
            fx.push(Effect::Emit(CoreMsg::ProjectUpdated(project_info(p))));
        }
        self.add_pane(pane, tab_id, plan, dir, fx);
        self.emit_tab(tab_id, fx);
        self.emit_pane(pane, fx);
        self.set_active(Some(tab_id), fx);
        Ok(())
    }

    fn add_pane(
        &mut self,
        pane: PaneId,
        tab: TabId,
        plan: LaunchPlan,
        cwd: String,
        fx: &mut Vec<Effect>,
    ) {
        self.panes.insert(
            pane,
            Pane {
                tab,
                custom_name: None,
                default_name: plan.default_name.clone(),
                profile_name: plan.profile_name.clone(),
                title: None,
                cwd: None,
                state: AgentState::Idle,
                running: false,
                exit_code: None,
                session_id: None,
                visible: false,
                seen: false,
            },
        );
        fx.push(Effect::Spawn {
            pane,
            plan: Box::new(plan),
            cwd,
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
            PaneEvent::State(state) => {
                p.state = state;
                fx.push(Effect::Emit(CoreMsg::PaneState { pane, state }));
            }
            PaneEvent::Title(title) => {
                p.title = title;
                self.emit_pane(pane, &mut fx);
            }
            PaneEvent::Cwd(cwd) => {
                p.cwd = Some(cwd);
                self.emit_pane(pane, &mut fx);
            }
            PaneEvent::SessionId(id) => {
                p.session_id = Some(id);
                self.emit_pane(pane, &mut fx);
            }
            PaneEvent::Started => {
                p.running = true;
                p.exit_code = None;
                self.emit_pane(pane, &mut fx);
            }
            PaneEvent::Exited(code) => {
                p.running = false;
                p.exit_code = code;
                fx.push(Effect::Emit(CoreMsg::PaneExited { pane, code }));
                self.emit_pane(pane, &mut fx);
            }
            PaneEvent::Failed(message) => {
                p.running = false;
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
            PaneEvent::Crashed => {
                p.running = false;
                p.exit_code = None;
                // The new actor a relaunch starts knows nothing: the next
                // refresh sends it the view again.
                p.visible = false;
                p.seen = false;
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
    }
}

fn pane_info(id: PaneId, p: &Pane) -> PaneInfo {
    PaneInfo {
        id,
        tab: p.tab,
        name: p
            .custom_name
            .clone()
            .unwrap_or_else(|| p.default_name.clone()),
        profile: p.profile_name.clone(),
        title: p.title.clone(),
        cwd: p.cwd.clone(),
        state: p.state,
        running: p.running,
        exit_code: p.exit_code,
        session_id: p.session_id.clone(),
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
    fn stale_pane_events_are_ignored() {
        let mut ws = Workspace::new();
        assert!(
            ws.on_pane_event(PaneId::new(5), PaneEvent::Started)
                .is_empty()
        );
    }

    #[test]
    fn folder_names() {
        assert_eq!(folder_name("C:\\Code\\yien\\"), "yien");
        assert_eq!(folder_name("/home/e/proj"), "proj");
        assert_eq!(folder_name("/"), "/");
    }

    #[derive(Debug, Clone)]
    enum Op {
        NewProject,
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
            Just(Op::NewProject),
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
            Op::NewProject => Command::NewProject {
                root: "C:/p".into(),
                name: None,
            },
            Op::NewTab(i) => Command::NewTab {
                project: pick(&projects, *i)?,
                name: None,
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
