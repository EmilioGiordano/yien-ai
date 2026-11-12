//! Dragging the dividers between panes, and moving them with the arrows
//! once F6 gave them the keyboard (RF-PAN-02). The geometry is
//! `crate::layout`; this draws the hit strips and tells the core, which
//! keeps the new sizes in the session.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use yien_proto::{Command, LayoutNode, Orientation, TabId};

use super::{HALF_GUTTER, Shell};
use crate::actions::*;
use crate::layout::{self, Divider};
use crate::model::TabView;
use crate::region::{self, DividerKey, Region};

/// The least terminal a drag or an arrow leaves a pane.
const MIN_COLS: f32 = 10.0;
const MIN_ROWS: f32 = 3.0;
/// What a pane draws around its grid: gutters, border and the terminal's
/// padding across; that plus the head down.
const PANE_CHROME_W: f32 = 2.0 * HALF_GUTTER + 2.0 + 20.0;
const PANE_CHROME_H: f32 = 2.0 * HALF_GUTTER + 2.0 + 16.0 + 28.0;
/// The hit strip over the gutter between two panes.
const STRIP: f32 = 2.0 * HALF_GUTTER;

/// What a drag of a divider carries.
pub(super) struct DividerDrag;

/// A drag of a divider draws nothing under the pointer.
struct NoGhost;

impl Render for NoGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// The divider the pointer took, from the mouse-down on, or the share an
/// arrow or a double click sent, until the core's layout shows it.
#[derive(Debug, Clone, Copy)]
pub(super) struct DividerGrab {
    tab: TabId,
    key: DividerKey,
    /// The share and the pointer along the axis at the mouse-down.
    start: f32,
    from: f32,
    /// The share shown; `None` until the pointer moves.
    share: Option<f32>,
    /// The core was told; the share stays shown until the core's layout
    /// for the tab has it.
    sent: bool,
}

/// Two shares the core and the window agree are the same divider position.
const SAME_SHARE: f32 = 1e-4;

/// The pane area's width and height in pixels at the last paint.
pub(super) type PaneArea = Rc<Cell<(f32, f32)>>;

/// The least width and height of a pane holding a `cell`-sized grid of
/// [`MIN_COLS`] by [`MIN_ROWS`].
fn min_leaf(cell: (f32, f32)) -> (f32, f32) {
    (
        MIN_COLS * cell.0 + PANE_CHROME_W,
        MIN_ROWS * cell.1 + PANE_CHROME_H,
    )
}

/// What a screen reader says a divider is.
fn divider_label(before: &str, after: &str) -> String {
    format!("Divider between {before} and {after}")
}

fn along(orientation: Orientation, (w, h): (f32, f32)) -> f32 {
    match orientation {
        Orientation::Horizontal => w,
        Orientation::Vertical => h,
    }
}

impl Shell {
    /// The cell of the selected tab's terminals, or the font's guess before
    /// any of them painted.
    fn cell(&self, tab: &TabView, cx: &App) -> (f32, f32) {
        tab.layout
            .panes()
            .iter()
            .filter_map(|p| self.terminals.get(p))
            .find_map(|t| t.read(cx).cell())
            .unwrap_or((self.terminal_size * 0.6, self.terminal_size * 1.2))
    }

    /// The layout drawn for `tab`: the core's, with the divider being
    /// dragged where the pointer has it.
    pub(super) fn shown_layout(&self, tab: &TabView) -> Option<LayoutNode> {
        let grab = self.divider_grab.filter(|g| g.tab == tab.id)?;
        let (before, after) = grab.key;
        layout::with_share(&tab.layout, before, after, grab.share?)
    }

    /// The dividers drawn for `tab`; none while a pane is maximized.
    fn tab_dividers(&self, tab: &TabView, cx: &App) -> Vec<Divider> {
        if tab.maximized.is_some() {
            return Vec::new();
        }
        let min = min_leaf(self.cell(tab, cx));
        match self.shown_layout(tab) {
            Some(shown) => layout::dividers(&shown, min),
            None => layout::dividers(&tab.layout, min),
        }
    }

    /// The selected tab's dividers by key, in reading order.
    pub(super) fn divider_keys(&self, cx: &App) -> Vec<DividerKey> {
        self.workspace
            .selected_tab()
            .map(|tab| self.tab_dividers(tab, cx))
            .unwrap_or_default()
            .iter()
            .map(|d| (d.before, d.after))
            .collect()
    }

    fn find_divider(&self, key: DividerKey, cx: &App) -> Option<(TabId, Divider)> {
        let tab = self.workspace.selected_tab()?;
        let divider = self
            .tab_dividers(tab, cx)
            .into_iter()
            .find(|d| (d.before, d.after) == key)?;
        Some((tab.id, divider))
    }

    /// The share the core's layout gives the divider, if it has it.
    fn core_share(&self, tab: TabId, key: DividerKey) -> Option<f32> {
        let tab = self.workspace.tab(tab)?;
        layout::divider_share(&tab.layout, key.0, key.1)
    }

    /// Tells the core the divider's new share and shows it until the core's
    /// layout has it. A share the core already holds is not sent: the core
    /// would answer nothing and the preview would outlive it.
    fn send_share(&mut self, tab: TabId, key: DividerKey, share: f32, cx: &mut Context<Self>) {
        // While an earlier send is unanswered the core's share is stale.
        let pending = self.divider_grab.as_ref().is_some_and(|g| g.sent);
        let held = self.core_share(tab, key);
        if !pending && held.is_some_and(|held| (held - share).abs() < SAME_SHARE) {
            self.divider_grab = None;
            cx.notify();
            return;
        }
        self.divider_grab = Some(DividerGrab {
            tab,
            key,
            start: share,
            from: 0.0,
            share: Some(share),
            sent: true,
        });
        let (before, after) = key;
        self.command(
            Command::SetDivider {
                before,
                after,
                share,
            },
            cx,
        );
        cx.notify();
    }

    /// Follows a drag of a divider; the root gets every move of it, so the
    /// pointer may leave the thin strip.
    pub(super) fn on_divider_drag(
        &mut self,
        event: &DragMoveEvent<DividerDrag>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(grab) = self.divider_grab.filter(|g| !g.sent) else {
            return;
        };
        let Some((tab, divider)) = self.find_divider(grab.key, cx) else {
            self.divider_grab = None;
            return;
        };
        if tab != grab.tab {
            self.divider_grab = None;
            return;
        }
        let at = match divider.orientation {
            Orientation::Horizontal => event.event.position.x.as_f32(),
            Orientation::Vertical => event.event.position.y.as_f32(),
        };
        let area = along(divider.orientation, self.pane_area.get());
        let share = layout::dragged_share(&divider, grab.start, at - grab.from, area);
        if share.is_some() && share != grab.share {
            self.divider_grab = Some(DividerGrab { share, ..grab });
            cx.notify();
        }
    }

    /// Sends the share a drag ended on, once; called at each render, as
    /// gpui says nothing when a drag ends.
    pub(super) fn finish_divider_drag(&mut self, cx: &mut Context<Self>) {
        let Some(grab) = self.divider_grab else {
            return;
        };
        let Some(share) = grab.share else {
            return;
        };
        if grab.sent || cx.has_active_drag() {
            return;
        }
        // A pane closed mid-drag: the core would answer with an error.
        if self
            .find_divider(grab.key, cx)
            .is_none_or(|(tab, _)| tab != grab.tab)
        {
            self.divider_grab = None;
            return;
        }
        self.send_share(grab.tab, grab.key, share, cx);
    }

    /// The core's layout for `tab` arrived: a share already sent stops
    /// overriding it once the core has it, or once the divider is gone. An
    /// older share still on its way keeps the newest one shown.
    pub(super) fn divider_settled(&mut self, tab: TabId) {
        let Some(grab) = self.divider_grab.filter(|g| g.sent && g.tab == tab) else {
            return;
        };
        let held = self.core_share(tab, grab.key);
        let reached = match (held, grab.share) {
            (Some(held), Some(sent)) => (held - sent).abs() < SAME_SHARE,
            _ => true,
        };
        if reached {
            self.divider_grab = None;
        }
    }

    /// Keeps the divider cursor on a drawn divider while the dividers have
    /// the keyboard, and hands the keyboard to the pane once none is left.
    /// Runs after core updates, never from render.
    pub(super) fn keep_divider_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.divider_focus.is_focused(window) {
            return;
        }
        let keys = self.divider_keys(cx);
        self.divider_cursor = region::divider_on_entry(&keys, self.divider_cursor, true);
        if self.divider_cursor.is_none() {
            self.give_back_keyboard(window, cx);
        }
    }

    /// F6 or Shift+F6 into the dividers.
    pub(super) fn focus_dividers(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let keys = self.divider_keys(cx);
        self.divider_cursor = region::divider_on_entry(&keys, self.divider_cursor, forward);
        if self.divider_cursor.is_none() {
            self.give_back_keyboard(window, cx);
            return;
        }
        window.focus(&self.divider_focus, cx);
        self.keyboard_in_bar = true;
    }

    fn step_divider(&mut self, axis: Orientation, forward: bool, cx: &mut Context<Self>) {
        self.modality = region::Modality::Keyboard;
        let Some(key) = self.divider_cursor else {
            return;
        };
        // The divider as shown, so a step held down builds on the share sent
        // before rather than on what the core has answered so far.
        let Some((tab, divider)) = self.find_divider(key, cx) else {
            return;
        };
        if divider.orientation != axis {
            return;
        }
        let area = along(axis, self.pane_area.get());
        if let Some(share) = layout::stepped_share(&divider, forward, area) {
            self.send_share(tab, key, share, cx);
        }
    }

    pub(super) fn divider_left(&mut self, _: &DividerLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.step_divider(Orientation::Horizontal, false, cx);
    }

    pub(super) fn divider_right(
        &mut self,
        _: &DividerRight,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.step_divider(Orientation::Horizontal, true, cx);
    }

    pub(super) fn divider_up(&mut self, _: &DividerUp, _: &mut Window, cx: &mut Context<Self>) {
        self.step_divider(Orientation::Vertical, false, cx);
    }

    pub(super) fn divider_down(&mut self, _: &DividerDown, _: &mut Window, cx: &mut Context<Self>) {
        self.step_divider(Orientation::Vertical, true, cx);
    }

    fn cycle_divider(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.modality = region::Modality::Keyboard;
        let keys = self.divider_keys(cx);
        self.divider_cursor = region::next_divider(&keys, self.divider_cursor, forward);
        cx.notify();
    }

    pub(super) fn divider_next(&mut self, _: &DividerNext, _: &mut Window, cx: &mut Context<Self>) {
        self.cycle_divider(true, cx);
    }

    pub(super) fn divider_previous(
        &mut self,
        _: &DividerPrevious,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cycle_divider(false, cx);
    }

    pub(super) fn divider_leave(
        &mut self,
        _: &DividerLeave,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.give_back_keyboard(window, cx);
    }

    /// Measures the pane area at each paint, for the drag and the arrows.
    pub(super) fn watch_pane_area(&self) -> impl IntoElement {
        let area = self.pane_area.clone();
        canvas(
            move |bounds, window, _| {
                let size = (bounds.size.width.as_f32(), bounds.size.height.as_f32());
                // Pane heads lay out from this size, read during render.
                if area.replace(size) != size {
                    window.on_next_frame(|window, _| window.refresh());
                }
            },
            |_, (), _, _| {},
        )
        .absolute()
        .size_full()
    }

    /// The hit strips over the gutters between the selected tab's panes:
    /// drag one to resize the panes on its two sides, double click it to
    /// give them equal sizes.
    pub(super) fn render_dividers(&self, tab: &TabView, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = self.theme;
        let wash = Rgba {
            a: 0.25,
            ..self.theme.ai
        };
        let ring = self.ringed(Region::Dividers);
        // Tracked only while the dividers hold the keyboard: a tracked
        // element takes the focus on mouse-down, and a drag must leave the
        // keyboard in the pane.
        let holding = self.keyboard_region == Region::Dividers;
        let dragged = self
            .divider_grab
            .filter(|g| g.tab == tab.id && g.share.is_some() && !g.sent)
            .map(|g| g.key);
        self.tab_dividers(tab, cx)
            .into_iter()
            .map(|d| {
                let key = (d.before, d.after);
                let names = (self.workspace.pane(d.before), self.workspace.pane(d.after));
                let cursor = holding && self.divider_cursor == Some(key);
                let lit = (ring && cursor) || dragged == Some(key);
                let tab_id = tab.id;
                let strip = div()
                    .id(ElementId::Name(
                        format!("divider-{}-{}", d.before.get(), d.after.get()).into(),
                    ))
                    .role(Role::Splitter)
                    .aria_label(divider_label(&names.0.name, &names.1.name))
                    .aria_numeric_value(f64::from((d.position() * 100.0).round()))
                    .aria_min_numeric_value(0.0)
                    .aria_max_numeric_value(100.0)
                    .aria_numeric_value_step(f64::from(layout::DIVIDER_STEP * 100.0))
                    .occlude()
                    .absolute()
                    .hover(move |s| s.bg(wash))
                    .when(lit, |s| s.bg(t.focus_ring()))
                    .when(cursor, |s| {
                        s.key_context(DIVIDER_CONTEXT)
                            .track_focus(&self.divider_focus)
                    });
                let strip = match d.orientation {
                    Orientation::Horizontal => strip
                        .aria_orientation(accesskit::Orientation::Vertical)
                        .cursor_col_resize()
                        .left(relative(d.at))
                        .ml(px(-STRIP / 2.0))
                        .w(px(STRIP))
                        .top(relative(d.across.0))
                        .h(relative(d.across.1)),
                    Orientation::Vertical => strip
                        .aria_orientation(accesskit::Orientation::Horizontal)
                        .cursor_row_resize()
                        .top(relative(d.at))
                        .mt(px(-STRIP / 2.0))
                        .h(px(STRIP))
                        .left(relative(d.across.0))
                        .w(relative(d.across.1)),
                };
                strip
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, _| {
                            let from = along(
                                d.orientation,
                                (event.position.x.as_f32(), event.position.y.as_f32()),
                            );
                            this.divider_grab = Some(DividerGrab {
                                tab: tab_id,
                                key,
                                start: d.share,
                                from,
                                share: None,
                                sent: false,
                            });
                        }),
                    )
                    .on_drag(DividerDrag, |_, _, _, cx| cx.new(|_| NoGhost))
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        if event.click_count() < 2 {
                            return;
                        }
                        let area = along(d.orientation, this.pane_area.get());
                        if let Some(share) = layout::dragged_share(&d, 0.5, 0.0, area) {
                            this.send_share(tab_id, key, share, cx);
                        }
                    }))
                    .into_any_element()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{PANE_CHROME_H, PANE_CHROME_W, divider_label, min_leaf};

    #[test]
    fn rf_pan_02_a_pane_keeps_at_least_ten_columns_and_three_rows() {
        let (w, h) = min_leaf((8.0, 16.0));
        assert_eq!(w, 80.0 + PANE_CHROME_W);
        assert_eq!(h, 48.0 + PANE_CHROME_H);
    }

    #[test]
    fn rf_pan_02_a_divider_is_named_after_its_two_sides() {
        assert_eq!(
            divider_label("claude", "server"),
            "Divider between claude and server"
        );
    }
}
