//! The main window: title bar, tab bar, pane area, context bar and status bar,
//! laid out as in the prototype, kept in step with the core's events.

use std::collections::{BTreeSet, HashMap};
use std::f32::consts::FRAC_PI_2;
use std::rc::Rc;

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::TitleBar;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tokio::sync::mpsc;
use yien_proto::{ClientMsg, Command, CoreMsg, Direction, PaneId, ProjectId, TabId};

use crate::actions::{
    CLOSE_PANE_KEYS, CONTEXT, CONTEXT_BAR_KEYS, ClosePane, FOCUS_KEYS, FocusDown, FocusLeft,
    FocusRight, FocusUp, GrowPane, NEXT_TAB_KEYS, NextTab, PreviousTab, SPLIT_DOWN_KEYS,
    SPLIT_RIGHT_KEYS, ShrinkPane, SplitDown, SplitRight, SwapDown, SwapLeft, SwapRight, SwapUp,
    TAB_BAR_KEYS, THEME_KEYS, ToggleContextBar, ToggleMaximize, ToggleTabBar, ToggleTheme,
};
use crate::assets::{self, state_icon};
use crate::bridge::{Link, deliver};
use crate::fonts::Fonts;
use crate::layout::neighbor;
use crate::model::{AgentState, PaneView, ProjectView, TabView, WorkspaceView};
use crate::terminal::TerminalPane;
use crate::theme::{Theme, sync_component_theme};

const TAB_BAR_WIDTH: f32 = 264.0;
const CONTEXT_BAR_WIDTH: f32 = 380.0;
const TITLE_BAR_HEIGHT: f32 = 40.0;
const STATUS_BAR_HEIGHT: f32 = 26.0;
/// Half the 6 px gutter between panes: every cell is inset by it.
const HALF_GUTTER: f32 = 3.0;
/// How much one key press grows or shrinks a pane, as a fraction of its split.
const RESIZE_STEP: f32 = 0.05;
/// Events applied in one go; the rest wait for the next turn of the loop.
const MAX_BATCH: usize = 1024;

pub(crate) struct Shell {
    workspace: WorkspaceView,
    theme: Theme,
    fonts: Fonts,
    tab_bar_open: bool,
    context_bar_open: bool,
    focus: FocusHandle,
    title: String,
    link: Rc<Link>,
    terminals: HashMap<PaneId, Entity<TerminalPane>>,
    /// The pane the keyboard was last moved to.
    keyboard_on: Option<PaneId>,
    /// The last error from the core, shown in the status bar.
    notice: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl Shell {
    pub(crate) fn new(
        link: Rc<Link>,
        mut events: mpsc::Receiver<CoreMsg>,
        theme: Theme,
        fonts: Fonts,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        // Bindings are scoped to the root's key context, which only takes part in
        // dispatch while something inside the root holds focus.
        window.focus(&focus, cx);
        cx.spawn_in(window, async move |this, cx| {
            while let Some(first) = events.recv().await {
                let mut batch = vec![first];
                while batch.len() < MAX_BATCH {
                    match events.try_recv() {
                        Ok(msg) => batch.push(msg),
                        Err(_) => break,
                    }
                }
                let applied =
                    this.update_in(cx, |shell, window, cx| shell.apply(batch, window, cx));
                if applied.is_err() {
                    return;
                }
            }
            let _ = this.update(cx, |shell, cx| {
                shell.notice = Some("The YIEN core stopped. Restart YIEN.".to_owned());
                cx.notify();
            });
        })
        .detach();
        let focused = window.is_window_active();
        deliver(&link, ClientMsg::WindowFocus { focused }, cx);
        let activation = cx.observe_window_activation(window, |shell, window, cx| {
            let focused = window.is_window_active();
            shell.send(ClientMsg::WindowFocus { focused }, cx);
        });
        Self {
            workspace: WorkspaceView::default(),
            theme,
            fonts,
            tab_bar_open: true,
            context_bar_open: true,
            focus,
            title: String::new(),
            link,
            terminals: HashMap::new(),
            keyboard_on: None,
            notice: None,
            _subscriptions: vec![activation],
        }
    }

    fn send(&self, msg: ClientMsg, cx: &mut Context<Self>) {
        deliver(&self.link, msg, cx);
    }

    fn command(&self, command: Command, cx: &mut Context<Self>) {
        self.send(ClientMsg::Command(command), cx);
    }

    /// Applies a batch of core events, then takes the frame of every pane
    /// that announced one. Events for panes or tabs the UI no longer knows
    /// are dropped.
    fn apply(&mut self, batch: Vec<CoreMsg>, window: &mut Window, cx: &mut Context<Self>) {
        let mut ready: BTreeSet<PaneId> = BTreeSet::new();
        let mut changed = false;
        for msg in batch {
            match msg {
                CoreMsg::PaneFrameReady { pane } => {
                    ready.insert(pane);
                    continue;
                }
                CoreMsg::WorkspaceSnapshot(snapshot) => {
                    self.workspace = WorkspaceView::from_snapshot(&snapshot);
                }
                CoreMsg::ProjectUpdated(info) => self.workspace.project_updated(&info),
                CoreMsg::TabUpdated(info) => self.workspace.tab_updated(&info),
                CoreMsg::TabClosed { tab } => {
                    for pane in self.workspace.tab_closed(tab) {
                        ready.remove(&pane);
                    }
                }
                CoreMsg::ActiveTab { tab } => self.workspace.set_active(tab),
                CoreMsg::PaneUpdated(info) => self.workspace.pane_updated(&info),
                CoreMsg::PaneClosed { pane } => {
                    ready.remove(&pane);
                    self.workspace.pane_closed(pane);
                }
                CoreMsg::PaneState { pane, state } => {
                    self.workspace.pane_state(pane, state);
                }
                CoreMsg::PaneExited { pane, code } => {
                    self.workspace.pane_exited(pane, code);
                }
                CoreMsg::SelectionText { text, .. } => {
                    if let Some(text) = text.filter(|t| !t.is_empty()) {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    }
                    continue;
                }
                CoreMsg::Error { message, .. } => self.notice = Some(message),
            }
            changed = true;
        }
        if changed {
            self.sync_terminals(window, cx);
            ready.extend(self.visible_panes());
        }
        self.take_frames(ready, cx);
        if changed {
            self.sync_focus(window, cx);
            cx.notify();
        }
    }

    /// The panes of the selected tab that are drawn now.
    fn visible_panes(&self) -> Vec<PaneId> {
        self.workspace
            .selected_tab()
            .map(|t| t.rects().into_iter().map(|(p, _)| p).collect())
            .unwrap_or_default()
    }

    /// Takes the latest frame of each pane from the core. The core sends no
    /// new notice for a pane until its frame is taken, so a pane with a view
    /// is always taken; `None` means nothing new.
    fn take_frames(&self, panes: impl IntoIterator<Item = PaneId>, cx: &mut Context<Self>) {
        for pane in panes {
            let Some(terminal) = self.terminals.get(&pane) else {
                continue;
            };
            if let Some(frame) = self.link.take_frame(pane) {
                terminal.update(cx, |t, cx| t.set_frame(frame, cx));
            }
        }
    }

    /// One terminal view per pane the workspace holds; views of closed panes go.
    fn sync_terminals(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let panes = self.workspace.all_panes();
        self.terminals.retain(|id, _| panes.contains(id));
        for pane in panes {
            if self.terminals.contains_key(&pane) {
                continue;
            }
            let link = Rc::clone(&self.link);
            let family = self.fonts.mono.clone();
            let theme = self.theme;
            let view = cx.new(|cx| TerminalPane::new(pane, link, family, theme, window, cx));
            self.terminals.insert(pane, view);
        }
    }

    /// Moves the keyboard to the selected tab's focused pane when that changes.
    fn sync_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.workspace.selected_tab().map(|t| t.focused);
        if target == self.keyboard_on {
            return;
        }
        self.keyboard_on = target;
        match target.and_then(|p| self.terminals.get(&p)) {
            Some(terminal) => {
                let handle = terminal.read(cx).handle().clone();
                window.focus(&handle, cx);
            }
            None => window.focus(&self.focus, cx),
        }
    }

    fn focused_pane(&self) -> Option<PaneId> {
        self.workspace.selected_tab().map(|t| t.focused)
    }

    fn toggle_theme(&mut self, _: &ToggleTheme, _: &mut Window, cx: &mut Context<Self>) {
        self.theme = Theme::of(self.theme.kind.toggled());
        sync_component_theme(&self.theme, cx);
        let theme = self.theme;
        for terminal in self.terminals.values() {
            terminal.update(cx, |t, cx| t.set_theme(theme, cx));
        }
        cx.notify();
    }

    fn toggle_tab_bar(&mut self, _: &ToggleTabBar, _: &mut Window, cx: &mut Context<Self>) {
        self.tab_bar_open = !self.tab_bar_open;
        cx.notify();
    }

    fn toggle_context_bar(&mut self, _: &ToggleContextBar, _: &mut Window, cx: &mut Context<Self>) {
        self.context_bar_open = !self.context_bar_open;
        cx.notify();
    }

    fn next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.workspace.tab_after(1) {
            self.select_tab(tab, window, cx);
        }
    }

    fn previous_tab(&mut self, _: &PreviousTab, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.workspace.tab_after(-1) {
            self.select_tab(tab, window, cx);
        }
    }

    /// The core shows a tab's panes only while it is the active one, so the
    /// selection goes through it; the local change just avoids a blank frame.
    fn select_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace.select_tab(id) {
            self.command(Command::SelectTab { tab: id }, cx);
            self.take_frames(self.visible_panes(), cx);
            self.sync_focus(window, cx);
            cx.notify();
        }
    }

    fn toggle_fold(&mut self, id: ProjectId, cx: &mut Context<Self>) {
        if self.workspace.toggle_fold(id) {
            cx.notify();
        }
    }

    fn split(&mut self, orientation: yien_proto::Orientation, cx: &mut Context<Self>) {
        if let Some(pane) = self.focused_pane() {
            self.command(
                Command::SplitPane {
                    pane,
                    orientation,
                    profile: None,
                },
                cx,
            );
        }
    }

    fn split_right(&mut self, _: &SplitRight, _: &mut Window, cx: &mut Context<Self>) {
        self.split(yien_proto::Orientation::Horizontal, cx);
    }

    fn split_down(&mut self, _: &SplitDown, _: &mut Window, cx: &mut Context<Self>) {
        self.split(yien_proto::Orientation::Vertical, cx);
    }

    fn close_pane(&mut self, _: &ClosePane, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(pane) = self.focused_pane() {
            self.command(Command::ClosePane { pane }, cx);
        }
    }

    fn focus_direction(&mut self, direction: Direction, cx: &mut Context<Self>) {
        self.command(Command::FocusDirection { direction }, cx);
    }

    fn focus_left(&mut self, _: &FocusLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.focus_direction(Direction::Left, cx);
    }

    fn focus_right(&mut self, _: &FocusRight, _: &mut Window, cx: &mut Context<Self>) {
        self.focus_direction(Direction::Right, cx);
    }

    fn focus_up(&mut self, _: &FocusUp, _: &mut Window, cx: &mut Context<Self>) {
        self.focus_direction(Direction::Up, cx);
    }

    fn focus_down(&mut self, _: &FocusDown, _: &mut Window, cx: &mut Context<Self>) {
        self.focus_direction(Direction::Down, cx);
    }

    fn resize(&mut self, delta: f32, cx: &mut Context<Self>) {
        if let Some(pane) = self.focused_pane() {
            self.command(Command::ResizeSplit { pane, delta }, cx);
        }
    }

    fn grow_pane(&mut self, _: &GrowPane, _: &mut Window, cx: &mut Context<Self>) {
        self.resize(RESIZE_STEP, cx);
    }

    fn shrink_pane(&mut self, _: &ShrinkPane, _: &mut Window, cx: &mut Context<Self>) {
        self.resize(-RESIZE_STEP, cx);
    }

    fn swap(&mut self, direction: Direction, cx: &mut Context<Self>) {
        let Some(tab) = self.workspace.selected_tab() else {
            return;
        };
        let a = tab.focused;
        if let Some(b) = neighbor(&tab.rects(), a, direction) {
            self.command(Command::SwapPanes { a, b }, cx);
        }
    }

    fn swap_left(&mut self, _: &SwapLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.swap(Direction::Left, cx);
    }

    fn swap_right(&mut self, _: &SwapRight, _: &mut Window, cx: &mut Context<Self>) {
        self.swap(Direction::Right, cx);
    }

    fn swap_up(&mut self, _: &SwapUp, _: &mut Window, cx: &mut Context<Self>) {
        self.swap(Direction::Up, cx);
    }

    fn swap_down(&mut self, _: &SwapDown, _: &mut Window, cx: &mut Context<Self>) {
        self.swap(Direction::Down, cx);
    }

    fn toggle_maximize(&mut self, _: &ToggleMaximize, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(pane) = self.focused_pane() {
            self.command(Command::ToggleMaximize { pane }, cx);
        }
    }

    /// The window title carries the counter so the taskbar shows it, like the
    /// prototype's document title.
    fn sync_title(&mut self, window: &mut Window) {
        let waiting = self.workspace.needs_you_panes();
        let title = if waiting > 0 {
            format!("({waiting}) YIEN")
        } else {
            "YIEN".to_owned()
        };
        if title != self.title {
            window.set_window_title(&title);
            self.title = title;
        }
    }

    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let crumbs = self.workspace.selected_tab().map(|tab| {
            let project = self
                .workspace
                .selected_project()
                .map(|p| p.name.clone())
                .unwrap_or_default();
            h_flex()
                .gap(px(4.0))
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_size(px(13.0))
                .text_color(t.muted)
                .child(project)
                .child("/")
                .child(
                    div()
                        .text_color(t.text)
                        .font_weight(FontWeight::MEDIUM)
                        .child(tab.name.clone()),
                )
        });
        TitleBar::new()
            .h(px(TITLE_BAR_HEIGHT))
            .bg(t.panel)
            .border_color(t.line)
            .child(
                h_flex()
                    .size_full()
                    .items_center()
                    .gap(px(12.0))
                    .pr(px(10.0))
                    .child(
                        h_flex()
                            .flex_none()
                            .items_center()
                            .gap(px(8.0))
                            .font_weight(FontWeight::BOLD)
                            .child(img(assets::MARK).size(px(20.0)))
                            .child("YIEN"),
                    )
                    .child(div().flex_1().min_w_0().children(crumbs))
                    .child(self.render_need_count())
                    .child(
                        icon_button("toggle-theme", assets::THEME, &t)
                            .aria_label(format!("Toggle sumi and washi theme ({THEME_KEYS})"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_theme(&ToggleTheme, window, cx)
                            })),
                    )
                    .child(
                        icon_button("toggle-tab-bar", assets::PANEL_LEFT, &t)
                            .aria_label(format!("Toggle tab bar ({TAB_BAR_KEYS})"))
                            .aria_toggled(toggled(self.tab_bar_open))
                            .when(!self.tab_bar_open, |b| b.opacity(0.55))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_tab_bar(&ToggleTabBar, window, cx)
                            })),
                    )
                    .child(
                        icon_button("toggle-context-bar", assets::PANEL_RIGHT, &t)
                            .aria_label(format!("Toggle context bar ({CONTEXT_BAR_KEYS})"))
                            .aria_toggled(toggled(self.context_bar_open))
                            .when(!self.context_bar_open, |b| b.opacity(0.55))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_context_bar(&ToggleContextBar, window, cx)
                            })),
                    ),
            )
    }

    /// The global "needs you" counter (RF-ATN-03). Red only while someone waits.
    fn render_need_count(&self) -> impl IntoElement {
        let t = self.theme;
        let waiting = self.workspace.needs_you_panes();
        let text = match waiting {
            0 => "Nobody is waiting".to_owned(),
            1 => "1 needs you".to_owned(),
            n => format!("{n} need you"),
        };
        h_flex()
            .id("need-count")
            .role(Role::Status)
            .aria_label(text.clone())
            .flex_none()
            .items_center()
            .gap(px(6.0))
            .px(px(10.0))
            .py(px(3.0))
            .rounded_full()
            .border_1()
            .text_size(px(12.5))
            .font_weight(FontWeight::MEDIUM)
            .map(|pill| {
                if waiting > 0 {
                    pill.border_color(t.shu)
                        .bg(t.shu_wash)
                        .text_color(t.shu)
                        .child(glyph(AgentState::NeedsYou, &t))
                } else {
                    pill.border_color(t.line).text_color(t.faint)
                }
            })
            .child(text)
    }

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let selected = self.workspace.selected_tab().map(|tab| tab.id);
        let groups: Vec<AnyElement> = self
            .workspace
            .projects()
            .iter()
            .map(|project| {
                let folded = self.workspace.is_folded(project.id);
                v_flex()
                    .child(self.render_project_head(project, folded, cx))
                    .when(!folded, |group| {
                        group.child(
                            v_flex()
                                .id(("tabs-of", project.id.get()))
                                .role(Role::TabList)
                                .aria_label(format!("Tabs of {}", project.name))
                                .children(self.workspace.tabs_of(project).map(|tab| {
                                    self.render_tab_row(tab, Some(tab.id) == selected, cx)
                                })),
                        )
                    })
                    .into_any_element()
            })
            .collect();
        v_flex()
            .id("tab-bar")
            .role(Role::Navigation)
            .aria_label("Projects and tabs")
            .w(px(TAB_BAR_WIDTH))
            .flex_none()
            .h_full()
            .border_r_1()
            .border_color(t.line)
            .bg(t.panel)
            .child(
                v_flex()
                    .id("tab-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(6.0))
                    .pb(px(10.0))
                    .children(groups),
            )
    }

    fn render_project_head(
        &self,
        project: &ProjectView,
        folded: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = self.theme;
        let waiting = self.workspace.needs_you_tabs_of(project);
        let id = project.id;
        let label = if waiting > 0 {
            format!("{}, {waiting} need you", project.name)
        } else {
            project.name.clone()
        };
        let chevron = svg()
            .path(assets::CHEVRON)
            .size(px(11.0))
            .flex_none()
            .text_color(t.faint)
            .when(folded, |c| {
                c.with_transformation(Transformation::rotate(radians(-FRAC_PI_2)))
            });
        h_flex()
            .id(("project", id.get()))
            .role(Role::Button)
            .aria_label(label)
            .aria_expanded(!folded)
            .items_center()
            .gap(px(6.0))
            .px(px(6.0))
            .pt(px(10.0))
            .pb(px(4.0))
            .text_size(px(12.5))
            .text_color(t.muted)
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_fold(id, cx)))
            .child(chevron)
            .when(waiting > 0, |head| {
                head.child(glyph(AgentState::NeedsYou, &t))
            })
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(13.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.text)
                    .child(project.name.clone()),
            )
            .when(waiting > 0, |head| {
                head.child(
                    div()
                        .flex_none()
                        .text_size(px(12.0))
                        .text_color(t.shu)
                        .child(format!("{waiting} need you")),
                )
            })
    }

    fn render_tab_row(&self, tab: &TabView, active: bool, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let state = self.workspace.tab_state(tab);
        let id = tab.id;
        let hover = t.hover;
        let second_line = active || matches!(state, AgentState::NeedsYou | AgentState::Error);
        let detail = tab.branch.clone().unwrap_or_else(|| tab.dir.clone());
        div()
            .id(("tab", id.get()))
            .role(Role::Tab)
            .aria_label(format!("{}, {}", tab.name, state.label()))
            .aria_selected(active)
            .relative()
            .flex()
            .flex_col()
            .gap(px(1.0))
            .w_full()
            .px(px(8.0))
            .py(px(5.0))
            .rounded(px(6.0))
            .border_1()
            .cursor_pointer()
            .map(|row| {
                if active {
                    row.bg(t.raised).border_color(t.line)
                } else {
                    row.border_color(gpui_kit::transparent_black())
                        .hover(move |s| s.bg(hover))
                }
            })
            .on_click(cx.listener(move |this, _, window, cx| this.select_tab(id, window, cx)))
            .child(
                h_flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(glyph(state, &t))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .font_weight(FontWeight::MEDIUM)
                            .child(tab.name.clone()),
                    ),
            )
            .when(second_line, |row| {
                row.child(
                    h_flex()
                        .pl(px(22.0))
                        .gap(px(8.0))
                        .min_w_0()
                        .whitespace_nowrap()
                        .text_size(px(12.0))
                        .text_color(t.faint)
                        .child(
                            div()
                                .flex_none()
                                .text_color(t.label_color(state))
                                .child(state.label()),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(detail),
                        ),
                )
            })
            .when(state.needs_you(), |row| {
                row.child(
                    div()
                        .absolute()
                        .left_0()
                        .top(px(6.0))
                        .bottom(px(6.0))
                        .w(px(2.0))
                        .rounded(px(2.0))
                        .bg(t.shu),
                )
            })
            .into_any_element()
    }

    fn render_panes(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let area = div()
            .id("panes")
            .role(Role::Main)
            .aria_label("Panes")
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(t.bg)
            .p(px(HALF_GUTTER));
        let Some(tab) = self.workspace.selected_tab() else {
            return area
                .flex()
                .items_center()
                .justify_center()
                .text_color(t.muted)
                .when_some(self.fonts.display.clone(), |d, family| {
                    d.font_family(family)
                })
                .text_size(px(18.0))
                .child("No tab is open");
        };
        let rects = tab.rects();
        let single = rects.len() == 1;
        let cells: Vec<AnyElement> = rects
            .into_iter()
            .map(|(id, rect)| {
                let pane = self.workspace.pane(id);
                div()
                    .absolute()
                    .left(relative(rect.x))
                    .top(relative(rect.y))
                    .w(relative(rect.w))
                    .h(relative(rect.h))
                    .p(px(HALF_GUTTER))
                    .child(self.render_pane(tab, &pane, single, cx))
                    .into_any_element()
            })
            .collect();
        area.child(div().relative().size_full().children(cells))
    }

    fn render_pane(
        &self,
        tab: &TabView,
        pane: &PaneView,
        single: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = self.theme;
        let focused = pane.id == tab.focused;
        let needs = pane.state.needs_you();
        let state_color = match pane.state {
            AgentState::NeedsYou => t.shu,
            AgentState::Error => t.ochre,
            _ => t.term_muted,
        };
        let head = h_flex()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .min_w_0()
            .px(px(8.0))
            .py(px(5.0))
            .border_b_1()
            .border_color(t.term_line)
            .whitespace_nowrap()
            .text_size(px(12.5))
            .text_color(t.term_muted)
            .when(focused, |h| h.bg(t.term_raised))
            .child(glyph(pane.state, &t))
            .child(
                div()
                    .flex_none()
                    .text_color(t.term_text)
                    .font_weight(FontWeight::MEDIUM)
                    .child(pane.name.clone()),
            )
            .child(div().flex_none().child(pane.profile.clone()))
            .when(single, |h| {
                h.child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_size(px(12.0))
                        .when_some(self.fonts.mono.clone(), |d, family| d.font_family(family))
                        .child(tab.dir.clone()),
                )
            })
            .child(
                div()
                    .ml_auto()
                    .flex_none()
                    .text_color(state_color)
                    .child(pane.state.label()),
            );
        let exited = pane.exited.then(|| {
            let id = pane.id;
            let what = match pane.exit_code {
                Some(code) => format!("{} exited with code {code}.", pane.name),
                None => format!("{} is not running.", pane.name),
            };
            h_flex()
                .flex_none()
                .items_center()
                .gap(px(8.0))
                .px(px(8.0))
                .py(px(4.0))
                .border_b_1()
                .border_color(t.term_line)
                .bg(t.term_raised)
                .text_size(px(12.0))
                .child(div().flex_1().min_w_0().text_color(t.ochre).child(what))
                .child(
                    div()
                        .id(("relaunch", id.get()))
                        .role(Role::Button)
                        .aria_label(format!("Relaunch {}", pane.name))
                        .flex_none()
                        .px(px(8.0))
                        .py(px(2.0))
                        .rounded(px(5.0))
                        .border_1()
                        .border_color(t.term_strong)
                        .text_color(t.term_text)
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.command(Command::RelaunchPane { pane: id }, cx);
                        }))
                        .child("Relaunch"),
                )
        });
        let body = match self.terminals.get(&pane.id) {
            Some(terminal) => terminal.clone().into_any_element(),
            None => div().flex_1().into_any_element(),
        };
        v_flex()
            .id(("pane", pane.id.get()))
            .role(Role::Group)
            .aria_label(format!("{} pane, {}", pane.name, pane.state.label()))
            .size_full()
            .overflow_hidden()
            .rounded(px(8.0))
            .bg(t.term_bg)
            .text_color(t.term_text)
            .map(|p| {
                if needs {
                    p.border_2().border_color(t.shu)
                } else if focused {
                    p.border_1().border_color(t.term_strong)
                } else {
                    p.border_1().border_color(t.term_line)
                }
            })
            .child(head)
            .children(exited)
            .child(body)
    }

    fn render_context_bar(&self) -> impl IntoElement {
        let t = self.theme;
        let tab = self.workspace.selected_tab();
        let branch = tab
            .and_then(|tab| tab.branch.clone())
            .unwrap_or_else(|| "no branch".to_owned());
        let dir = tab.map(|tab| tab.dir.clone()).unwrap_or_default();
        let mono = self.fonts.mono.clone();
        v_flex()
            .id("context-bar")
            .role(Role::Complementary)
            .aria_label("Context bar")
            .w(px(CONTEXT_BAR_WIDTH))
            .flex_none()
            .h_full()
            .border_l_1()
            .border_color(t.line)
            .bg(t.panel)
            .child(
                h_flex()
                    .flex_none()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(10.0))
                    .py(px(8.0))
                    .border_b_1()
                    .border_color(t.line)
                    .child(
                        h_flex()
                            .flex_none()
                            .items_center()
                            .gap(px(6.0))
                            .px(px(8.0))
                            .py(px(4.0))
                            .rounded(px(6.0))
                            .border_1()
                            .border_color(t.line)
                            .bg(t.bg)
                            .text_size(px(12.5))
                            .font_weight(FontWeight::MEDIUM)
                            .when_some(mono.clone(), |d, family| d.font_family(family))
                            .child(
                                svg()
                                    .path(assets::BRANCH)
                                    .size(px(13.0))
                                    .text_color(t.muted),
                            )
                            .child(branch),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(12.0))
                            .text_color(t.faint)
                            .when_some(mono, |d, family| d.font_family(family))
                            .child(dir),
                    ),
            )
            .child(div().flex_1())
    }

    fn render_status_bar(&self) -> impl IntoElement {
        let t = self.theme;
        let ws = &self.workspace;
        h_flex()
            .id("status-bar")
            .role(Role::ContentInfo)
            .flex_none()
            .h(px(STATUS_BAR_HEIGHT))
            .items_center()
            .gap(px(16.0))
            .px(px(10.0))
            .border_t_1()
            .border_color(t.line)
            .bg(t.panel)
            .whitespace_nowrap()
            .overflow_hidden()
            .text_size(px(11.5))
            .text_color(t.faint)
            .when_some(self.fonts.mono.clone(), |d, family| d.font_family(family))
            .child(format!(
                "{} tabs · {} panes",
                ws.tab_count(),
                ws.pane_count()
            ))
            .child(format!("{} working", ws.working_panes()))
            .when_some(self.notice.clone(), |bar, notice| {
                bar.child(
                    div()
                        .id("notice")
                        .role(Role::Status)
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_color(t.ochre)
                        .child(notice),
                )
            })
            .child(div().ml_auto().child(format!(
                "{SPLIT_RIGHT_KEYS} split right · {SPLIT_DOWN_KEYS} split down · \
                 {CLOSE_PANE_KEYS} close pane · {FOCUS_KEYS} focus · {TAB_BAR_KEYS} tab bar · \
                 {CONTEXT_BAR_KEYS} context bar · {NEXT_TAB_KEYS} next tab · {THEME_KEYS} theme"
            )))
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_title(window);
        let t = self.theme;
        v_flex()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::toggle_theme))
            .on_action(cx.listener(Self::toggle_tab_bar))
            .on_action(cx.listener(Self::toggle_context_bar))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::previous_tab))
            .on_action(cx.listener(Self::split_right))
            .on_action(cx.listener(Self::split_down))
            .on_action(cx.listener(Self::close_pane))
            .on_action(cx.listener(Self::focus_left))
            .on_action(cx.listener(Self::focus_right))
            .on_action(cx.listener(Self::focus_up))
            .on_action(cx.listener(Self::focus_down))
            .on_action(cx.listener(Self::grow_pane))
            .on_action(cx.listener(Self::shrink_pane))
            .on_action(cx.listener(Self::swap_left))
            .on_action(cx.listener(Self::swap_right))
            .on_action(cx.listener(Self::swap_up))
            .on_action(cx.listener(Self::swap_down))
            .on_action(cx.listener(Self::toggle_maximize))
            .size_full()
            .bg(t.panel)
            .text_color(t.text)
            .text_size(px(14.0))
            .when_some(self.fonts.ui.clone(), |d, family| d.font_family(family))
            .child(self.render_title_bar(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .when(self.tab_bar_open, |row| row.child(self.render_tab_bar(cx)))
                    .child(self.render_panes(cx))
                    .when(self.context_bar_open, |row| {
                        row.child(self.render_context_bar())
                    }),
            )
            .child(self.render_status_bar())
    }
}

/// A 30 px square button holding a 16 px icon, as the prototype's `.icon-btn`.
fn icon_button(id: &'static str, icon: &'static str, t: &Theme) -> Stateful<Div> {
    let hover = t.hover;
    div()
        .id(id)
        .occlude()
        .role(Role::Button)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(30.0))
        .rounded(px(6.0))
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .child(svg().path(icon).size(px(16.0)).text_color(t.muted))
}

/// The state glyph in its 14 px slot. The shape tells the state, the colour
/// only reinforces it.
fn glyph(state: AgentState, t: &Theme) -> impl IntoElement {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(14.0))
        .child(
            svg()
                .path(state_icon(state))
                .size(px(12.0))
                .text_color(t.state_color(state)),
        )
}

fn toggled(on: bool) -> Toggled {
    if on { Toggled::True } else { Toggled::False }
}
