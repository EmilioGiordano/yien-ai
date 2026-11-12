//! Dragging the dividers between panes (RF-PAN-02). The geometry is
//! `crate::layout`; this draws the hit strips and tells the core, which
//! keeps the new sizes in the session.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use yien_proto::{Command, LayoutNode, Orientation, PaneId, TabId};

use super::{HALF_GUTTER, Shell};
use crate::layout::{self, Divider};
use crate::model::TabView;

/// The least terminal a drag or an arrow leaves a pane.
const MIN_COLS: f32 = 10.0;
const MIN_ROWS: f32 = 3.0;
/// What a pane draws around its grid: gutters, border and the terminal's
/// padding across; that plus the head down.
const PANE_CHROME_W: f32 = 2.0 * HALF_GUTTER + 2.0 + 20.0;
const PANE_CHROME_H: f32 = 2.0 * HALF_GUTTER + 2.0 + 16.0 + 28.0;
/// The hit strip over the gutter between two panes.
const STRIP: f32 = 2.0 * HALF_GUTTER;

/// A divider by the first pane on each of its sides.
pub(super) type DividerKey = (PaneId, PaneId);

/// What a drag of a divider carries.
pub(super) struct DividerDrag;

/// A drag of a divider draws nothing under the pointer.
struct NoGhost;

impl Render for NoGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// The divider the pointer took, from the mouse-down on.
#[derive(Debug, Clone, Copy)]
pub(super) struct DividerGrab {
    tab: TabId,
    key: DividerKey,
    /// The share and the pointer along the axis at the mouse-down.
    start: f32,
    from: f32,
    /// The share shown; `None` until the pointer moves.
    share: Option<f32>,
    /// The drag ended and the core was told; the share stays shown until
    /// the core's layout for the tab comes back.
    sent: bool,
}

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

    fn find_divider(&self, key: DividerKey, cx: &App) -> Option<(TabId, Divider)> {
        let tab = self.workspace.selected_tab()?;
        let divider = self
            .tab_dividers(tab, cx)
            .into_iter()
            .find(|d| (d.before, d.after) == key)?;
        Some((tab.id, divider))
    }

    fn set_divider(&self, key: DividerKey, share: f32, cx: &mut Context<Self>) {
        let (before, after) = key;
        self.command(
            Command::SetDivider {
                before,
                after,
                share,
            },
            cx,
        );
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
        self.divider_grab = Some(DividerGrab { sent: true, ..grab });
        self.set_divider(grab.key, share, cx);
    }

    /// The core's layout for `tab` arrived: a drag already told to the
    /// core stops overriding it.
    pub(super) fn divider_settled(&mut self, tab: TabId) {
        if self.divider_grab.is_some_and(|g| g.sent && g.tab == tab) {
            self.divider_grab = None;
        }
    }

    /// Measures the pane area at each paint, for the drag and the arrows.
    pub(super) fn watch_pane_area(&self) -> impl IntoElement {
        let area = self.pane_area.clone();
        canvas(
            move |bounds, _, _| {
                area.set((bounds.size.width.as_f32(), bounds.size.height.as_f32()));
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
        let dragged = self
            .divider_grab
            .filter(|g| g.tab == tab.id && g.share.is_some() && !g.sent)
            .map(|g| g.key);
        self.tab_dividers(tab, cx)
            .into_iter()
            .map(|d| {
                let key = (d.before, d.after);
                let names = (self.workspace.pane(d.before), self.workspace.pane(d.after));
                let lit = dragged == Some(key);
                let tab_id = tab.id;
                let strip = div()
                    .id(ElementId::Name(
                        format!("divider-{}-{}", d.before.get(), d.after.get()).into(),
                    ))
                    .role(Role::Splitter)
                    .aria_label(divider_label(&names.0.name, &names.1.name))
                    .aria_numeric_value(f64::from((d.share * 100.0).round()))
                    .aria_min_numeric_value(0.0)
                    .aria_max_numeric_value(100.0)
                    .occlude()
                    .absolute()
                    .hover(move |s| s.bg(wash))
                    .when(lit, |s| s.bg(t.focus_ring()));
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
                            this.set_divider(key, share, cx);
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
