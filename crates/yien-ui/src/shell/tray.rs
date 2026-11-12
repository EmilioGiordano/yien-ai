//! The attention tray (RF-ATN-13), the jump to the next finished pane
//! (RF-ATN-14) and the tab bar's grouping toggle (RF-ATN-07). What the tray
//! lists is decided in `crate::tray`.

use std::time::{Duration, Instant};

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use yien_proto::{Command, PaneId};

use super::{Shell, TITLE_BAR_HEIGHT, glyph};
use crate::actions::*;
use crate::model::AgentState;
use crate::tray::{TrayRow, keep_cursor, list_index, move_cursor, tray_rows};

const TRAY_WIDTH: f32 = 400.0;
const TRAY_LIST_HEIGHT: f32 = 360.0;

/// The tray, while open.
pub(super) struct Tray {
    focus: FocusHandle,
    /// The row the arrows are on, held by pane so it survives updates.
    cursor: Option<PaneId>,
    /// Where the cursor's row was, for when its pane leaves the list.
    index: usize,
    scroll: ScrollHandle,
}

impl Tray {
    /// A tray whose cursor lands on the first row at the first update.
    pub(super) fn new(focus: FocusHandle) -> Self {
        Self {
            focus,
            cursor: None,
            index: 0,
            scroll: ScrollHandle::new(),
        }
    }

    pub(super) fn focus(&self) -> &FocusHandle {
        &self.focus
    }
}

impl Shell {
    pub(super) fn toggle_attention_tray(
        &mut self,
        _: &ToggleAttentionTray,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tray.is_some() {
            self.close_tray(window, cx);
        } else {
            self.open_tray(window, cx);
        }
    }

    /// Opens the tray with the keyboard on its first row.
    pub(super) fn open_tray(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.split_menu = None;
        self.close_palette(window, cx);
        self.finish_rename(true, window, cx);
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        self.tray = Some(Tray::new(focus));
        self.sync_tray();
        cx.notify();
    }

    /// A click on the counter toggles the tray, unless the mouse-down of
    /// that same click just closed it from outside.
    pub(super) fn counter_clicked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let just_closed = self
            .tray_closed_at
            .take()
            .is_some_and(|at| at.elapsed() < Duration::from_millis(500));
        if !just_closed {
            self.toggle_attention_tray(&ToggleAttentionTray, window, cx);
        }
    }

    pub(super) fn close_tray(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tray.take().is_some() {
            self.give_back_keyboard(window, cx);
        }
    }

    fn tray_up(&mut self, _: &TrayUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_tray(-1, cx);
    }

    fn tray_down(&mut self, _: &TrayDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_tray(1, cx);
    }

    fn move_tray(&mut self, by: isize, cx: &mut Context<Self>) {
        let rows = tray_rows(&self.workspace);
        if let Some(tray) = self.tray.as_mut() {
            tray.cursor = move_cursor(&rows, tray.cursor, by);
            tray.index = rows
                .iter()
                .position(|r| Some(r.pane) == tray.cursor)
                .unwrap_or(0);
            if let Some(item) = tray.cursor.and_then(|pane| list_index(&rows, pane)) {
                tray.scroll.scroll_to_item(item);
            }
            cx.notify();
        }
    }

    fn tray_confirm(&mut self, _: &TrayConfirm, window: &mut Window, cx: &mut Context<Self>) {
        let rows = tray_rows(&self.workspace);
        let target = self
            .tray
            .as_ref()
            .and_then(|tray| keep_cursor(&rows, tray.cursor, tray.index));
        match target {
            Some(pane) => self.go_to_tray_pane(pane, window, cx),
            None => self.close_tray(window, cx),
        }
    }

    fn tray_close(&mut self, _: &TrayClose, window: &mut Window, cx: &mut Context<Self>) {
        self.close_tray(window, cx);
    }

    /// Closes the tray and focuses the pane; the core selects its tab.
    fn go_to_tray_pane(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        self.tray = None;
        self.command(Command::FocusPane { pane }, cx);
        self.give_back_keyboard(window, cx);
    }

    /// Focuses the next pane whose agent finished a turn, oldest first
    /// (RF-ATN-14). Seeing it turns it idle (RF-AGE-07).
    pub(super) fn jump_to_done(
        &mut self,
        _: &JumpToDone,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_tray(window, cx);
        match self.workspace.next_done(self.focused_pane()) {
            Some(pane) => self.command(Command::FocusPane { pane }, cx),
            None => {
                self.say("Nothing is waiting for review.", cx);
            }
        }
    }

    /// Lists tabs by state or by project for this session; the settings
    /// file's `ui.tabs.group` wins again when it is next applied (RF-ATN-07).
    pub(super) fn toggle_tab_grouping(
        &mut self,
        _: &ToggleTabGrouping,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let on = self.settings.toggle_grouping();
        self.workspace.set_group_by_state(on);
        self.tab_cursor = None;
        self.say(
            if on {
                "Tabs grouped by state until config.toml is next saved."
            } else {
                "Tabs grouped by project until config.toml is next saved."
            },
            cx,
        );
    }

    /// Drops the tray's cursor onto a listed row after the workspace
    /// changed, so Enter never goes to a pane that left.
    pub(super) fn sync_tray(&mut self) {
        let rows = tray_rows(&self.workspace);
        if let Some(tray) = self.tray.as_mut() {
            tray.cursor = keep_cursor(&rows, tray.cursor, tray.index);
            tray.index = rows
                .iter()
                .position(|r| Some(r.pane) == tray.cursor)
                .unwrap_or(0);
        }
    }

    /// The tray under the counter: every pane that needs the user, failed
    /// or finished, in sections, the one there longest first.
    pub(super) fn render_tray(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let tray = self.tray.as_ref()?;
        let t = self.theme;
        let rows = tray_rows(&self.workspace);
        let cursor = keep_cursor(&rows, tray.cursor, tray.index);
        let mut list: Vec<AnyElement> = Vec::new();
        let mut section = None;
        for row in &rows {
            if section != Some(row.state) {
                section = Some(row.state);
                list.push(
                    div()
                        .px(px(12.0))
                        .pt(px(6.0))
                        .pb(px(2.0))
                        .text_size(t.text(12.0))
                        .text_color(t.faint)
                        .child(row.state.label())
                        .into_any_element(),
                );
            }
            list.push(self.render_tray_row(row, cursor == Some(row.pane), cx));
        }
        let empty = rows.is_empty().then(|| {
            div()
                .p(px(14.0))
                .text_size(t.text(13.0))
                .text_color(t.muted)
                .child("Nobody is waiting. Agents that need you, fail or finish show up here.")
        });
        Some(
            v_flex()
                .id("attention-tray")
                .role(Role::Dialog)
                .aria_label("Attention tray")
                .track_focus(&tray.focus)
                .key_context(TRAY_CONTEXT)
                .occlude()
                .absolute()
                .top(px(TITLE_BAR_HEIGHT + 4.0))
                .right(px(110.0))
                .w(px(TRAY_WIDTH))
                .max_w(relative(0.9))
                .overflow_hidden()
                .rounded(px(10.0))
                .border_1()
                .border_color(t.line)
                .bg(t.raised)
                .shadow_lg()
                .on_action(cx.listener(Self::tray_up))
                .on_action(cx.listener(Self::tray_down))
                .on_action(cx.listener(Self::tray_confirm))
                .on_action(cx.listener(Self::tray_close))
                .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                    this.tray_closed_at = Some(Instant::now());
                    this.close_tray(window, cx);
                }))
                .child(
                    v_flex()
                        .id("attention-tray-list")
                        .role(Role::ListBox)
                        .aria_label("Agents that need you, failed or finished")
                        .track_scroll(&tray.scroll)
                        .overflow_y_scroll()
                        .max_h(px(TRAY_LIST_HEIGHT))
                        .pb(px(6.0))
                        .children(list)
                        .children(empty),
                )
                .child(
                    div()
                        .px(px(12.0))
                        .py(px(6.0))
                        .border_t_1()
                        .border_color(t.line)
                        .text_size(t.text(11.5))
                        .text_color(t.faint)
                        .child(format!(
                            "Next that needs you: {JUMP_KEYS} · Next to review: {JUMP_DONE_KEYS}"
                        )),
                ),
        )
    }

    fn render_tray_row(&self, row: &TrayRow, selected: bool, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let pane = row.pane;
        let hover = t.hover;
        let detail_color = match row.state {
            AgentState::NeedsYou => t.text,
            _ => t.muted,
        };
        h_flex()
            .id(("tray-row", pane.get()))
            .role(Role::ListBoxOption)
            .aria_label(format!(
                "{}, {}, {}: {}",
                row.place,
                row.profile,
                row.state.label(),
                row.detail
            ))
            .aria_selected(selected)
            .items_start()
            .gap(px(8.0))
            .px(px(12.0))
            .py(px(6.0))
            .cursor_pointer()
            .when(selected, |r| r.bg(t.hover))
            .hover(move |s| s.bg(hover))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.go_to_tray_pane(pane, window, cx);
            }))
            .child(div().pt(px(2.0)).child(glyph(row.state, &t)))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.0))
                    .child(
                        div()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(t.text(12.5))
                            .when_some(self.fonts.mono.clone(), |d, family| d.font_family(family))
                            .child(row.place.clone()),
                    )
                    .child(
                        div()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(t.text(12.0))
                            .text_color(detail_color)
                            .child(row.detail.clone()),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(t.text(11.5))
                    .text_color(t.faint)
                    .child(row.profile.clone()),
            )
            .into_any_element()
    }
}
