//! Hiding, resizing and widening the side bars (RF-NAV-10). The model is
//! `crate::sidebars`; this draws the edges and keeps the core told, so the
//! session saves the bars.

use gpui_kit::*;
use yien_proto::{ClientMsg, Command, SideBars};

use super::Shell;
use crate::actions::*;
use crate::region::Region;
use crate::sidebars::{self, Bar};

/// The width of the strip over a bar's inner edge that drags it.
const EDGE_WIDTH: f32 = 6.0;

/// What a drag of a bar's edge carries.
pub(super) struct BarDrag(Bar);

/// A drag of a bar's edge draws nothing under the pointer.
struct NoGhost;

impl Render for NoGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

impl Shell {
    pub(super) fn bar_shown(&self, bar: Bar) -> bool {
        sidebars::shown(&self.side_bars, bar)
    }

    /// Applies new side bars: a bar that holds the keyboard and hides hands
    /// it to the pane, and the core is told so the session keeps them.
    pub(super) fn set_side_bars(
        &mut self,
        bars: SideBars,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let was = self.side_bars;
        self.side_bars = bars;
        for (bar, region) in [
            (Bar::Tab, Region::TabBar),
            (Bar::Context, Region::ContextBar),
        ] {
            let hid = sidebars::shown(&was, bar) && !self.bar_shown(bar);
            if hid && self.region(window, cx) == region {
                self.give_back_keyboard(window, cx);
            }
        }
        if sidebars::shown(&was, Bar::Context) != self.bar_shown(Bar::Context) {
            self.sync_files(window, cx);
        }
        self.save_side_bars(cx);
        cx.notify();
    }

    pub(super) fn change_side_bars(
        &mut self,
        change: impl FnOnce(&mut SideBars),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut bars = self.side_bars;
        change(&mut bars);
        self.set_side_bars(bars, window, cx);
    }

    /// Tells the core the bars once they differ from what it holds. Never
    /// before the core's snapshot brought the saved ones, which the
    /// defaults drawn until then would overwrite, and never mid-drag: the
    /// drag's end sends the width once.
    pub(super) fn save_side_bars(&mut self, cx: &mut Context<Self>) {
        let Some(saved) = self.side_bars_saved else {
            return;
        };
        if saved == self.side_bars || cx.has_active_drag() {
            return;
        }
        self.side_bars_saved = Some(self.side_bars);
        let bars = self.side_bars;
        self.send(ClientMsg::Command(Command::SetSideBars { bars }), cx);
    }

    pub(super) fn toggle_tab_bar(
        &mut self,
        _: &ToggleTabBar,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_side_bars(|b| sidebars::toggle(b, Bar::Tab), window, cx);
    }

    pub(super) fn toggle_context_bar(
        &mut self,
        _: &ToggleContextBar,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_side_bars(|b| sidebars::toggle(b, Bar::Context), window, cx);
    }

    /// Widens the context bar to review the changes, or back (RF-NAV-10).
    /// Entering review shows the change list, as the prototype does.
    pub(super) fn toggle_review_mode(
        &mut self,
        _: &ToggleReviewMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.side_bars.review {
            self.context_view = super::ContextView::Changes;
        }
        self.change_side_bars(sidebars::toggle_review, window, cx);
        self.sync_files(window, cx);
    }

    /// Follows a drag of a bar's edge; the root gets every move of it, so
    /// the pointer may leave the thin edge.
    pub(super) fn on_bar_drag(
        &mut self,
        event: &DragMoveEvent<BarDrag>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let bar = event.drag(cx).0;
        let x = event.event.position.x.as_f32();
        let width = window.viewport_size().width.as_f32();
        if sidebars::drag_to(&mut self.side_bars, bar, x, width) {
            cx.notify();
        }
    }

    /// The strip over a bar's inner edge: drag it to resize the bar, double
    /// click it for the default width. `None` while the bar cannot be
    /// dragged (review mode).
    pub(super) fn render_bar_edge(
        &self,
        bar: Bar,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        if !sidebars::draggable(&self.side_bars, bar) {
            return None;
        }
        // The prototype's indigo wash; never red (RN-01).
        let wash = Rgba {
            a: 0.25,
            ..self.theme.ai
        };
        let (id, label) = match bar {
            Bar::Tab => ("tab-bar-edge", "Resize tab bar"),
            Bar::Context => ("context-bar-edge", "Resize context bar"),
        };
        let edge = div()
            .id(id)
            .role(Role::Splitter)
            .aria_label(label)
            .occlude()
            .absolute()
            .top_0()
            .bottom_0()
            .w(px(EDGE_WIDTH))
            .cursor_col_resize()
            .hover(move |s| s.bg(wash))
            .on_drag(BarDrag(bar), |_, _, _, cx| cx.new(|_| NoGhost))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                if event.click_count() >= 2 {
                    this.change_side_bars(|b| sidebars::reset(b, bar), window, cx);
                }
            }));
        Some(match bar {
            Bar::Tab => edge.right_0(),
            Bar::Context => edge.left_0(),
        })
    }
}

/// The review button's label, as the prototype's.
pub(super) fn review_label(review: bool) -> &'static str {
    if review { "Exit review" } else { "Review" }
}

#[cfg(test)]
mod tests {
    use super::review_label;

    #[test]
    fn rf_nav_10_the_review_button_says_what_it_does() {
        assert_eq!(review_label(false), "Review");
        assert_eq!(review_label(true), "Exit review");
    }
}
