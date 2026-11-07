//! The main window: title bar, tab bar, pane area, context bar and status bar,
//! laid out as in the prototype.

use std::f32::consts::FRAC_PI_2;

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::TitleBar;
use gpui_kit::*;
use yien_proto::{ProjectId, TabId};

use crate::actions::{
    CONTEXT, CONTEXT_BAR_KEYS, NEXT_TAB_KEYS, NextTab, PreviousTab, TAB_BAR_KEYS, THEME_KEYS,
    ToggleContextBar, ToggleTabBar, ToggleTheme,
};
use crate::assets::{self, state_icon};
use crate::fonts::Fonts;
use crate::model::{AgentState, PaneView, ProjectView, TabView, WorkspaceView};
use crate::theme::{Theme, sync_component_theme};

const TAB_BAR_WIDTH: f32 = 264.0;
const CONTEXT_BAR_WIDTH: f32 = 380.0;
const TITLE_BAR_HEIGHT: f32 = 40.0;
const STATUS_BAR_HEIGHT: f32 = 26.0;
/// Half the 6 px gutter between panes: every cell is inset by it.
const HALF_GUTTER: f32 = 3.0;

pub(crate) struct Shell {
    workspace: WorkspaceView,
    theme: Theme,
    fonts: Fonts,
    tab_bar_open: bool,
    context_bar_open: bool,
    focus: FocusHandle,
    title: String,
}

impl Shell {
    pub(crate) fn new(
        workspace: WorkspaceView,
        theme: Theme,
        fonts: Fonts,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        // Bindings are scoped to the root's key context, which only takes part in
        // dispatch while something inside the root holds focus.
        window.focus(&focus, cx);
        Self {
            workspace,
            theme,
            fonts,
            tab_bar_open: true,
            context_bar_open: true,
            focus,
            title: String::new(),
        }
    }

    fn toggle_theme(&mut self, _: &ToggleTheme, _: &mut Window, cx: &mut Context<Self>) {
        self.theme = Theme::of(self.theme.kind.toggled());
        sync_component_theme(&self.theme, cx);
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

    fn next_tab(&mut self, _: &NextTab, _: &mut Window, cx: &mut Context<Self>) {
        if self.workspace.select_next_tab() {
            cx.notify();
        }
    }

    fn previous_tab(&mut self, _: &PreviousTab, _: &mut Window, cx: &mut Context<Self>) {
        if self.workspace.select_previous_tab() {
            cx.notify();
        }
    }

    fn select_tab(&mut self, id: TabId, cx: &mut Context<Self>) {
        if self.workspace.select_tab(id) {
            cx.notify();
        }
    }

    fn toggle_fold(&mut self, id: ProjectId, cx: &mut Context<Self>) {
        if self.workspace.toggle_fold(id) {
            cx.notify();
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
                                .children(project.tabs.iter().map(|tab| {
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
        let waiting = project.needs_you_tabs();
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
        let state = tab.state();
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
            .on_click(cx.listener(move |this, _, _, cx| this.select_tab(id, cx)))
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

    fn render_panes(&self) -> impl IntoElement {
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
        let single = tab.panes.len() == 1;
        let cells = tab.pane_rects().into_iter().map(|(pane, rect)| {
            div()
                .absolute()
                .left(relative(rect.x))
                .top(relative(rect.y))
                .w(relative(rect.w))
                .h(relative(rect.h))
                .p(px(HALF_GUTTER))
                .child(self.render_pane(tab, pane, single))
        });
        area.child(div().relative().size_full().children(cells))
    }

    fn render_pane(&self, tab: &TabView, pane: &PaneView, single: bool) -> impl IntoElement {
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
        let body = div()
            .flex_1()
            .min_h_0()
            .px(px(10.0))
            .py(px(8.0))
            .text_size(px(12.5))
            .text_color(t.term_faint)
            .when_some(self.fonts.mono.clone(), |d, family| d.font_family(family))
            .child("No terminal yet.");
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
            .child(div().ml_auto().child(format!(
                "{TAB_BAR_KEYS} tab bar · {CONTEXT_BAR_KEYS} context bar · \
                 {NEXT_TAB_KEYS} next tab · {THEME_KEYS} theme"
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
                    .child(self.render_panes())
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
