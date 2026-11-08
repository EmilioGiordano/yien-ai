//! The side bars (RF-NAV-10): which ones the user hid, the widths they
//! dragged them to, review mode, and how wide each one is drawn in a window
//! of a given width. Decided here without drawing; the shell draws.

use yien_proto::{SideBar, SideBars};

/// The tab bar's width until the user drags it, as the prototype's.
pub(crate) const TAB_BAR_DEFAULT: f32 = 264.0;
/// The context bar's width until the user drags it, as the prototype's.
pub(crate) const CONTEXT_BAR_DEFAULT: f32 = 380.0;
/// The narrowest a drag leaves the tab bar: a tab name and its state still fit.
pub(crate) const TAB_BAR_MIN: f32 = 180.0;
/// The narrowest a drag leaves the context bar: the branch chip and a
/// changed file's name still fit.
pub(crate) const CONTEXT_BAR_MIN: f32 = 280.0;
/// The widest a drag makes a bar, as a share of the window.
pub(crate) const MAX_SHARE: f32 = 0.5;
/// The context bar's share of the window in review mode, as the prototype's.
pub(crate) const REVIEW_SHARE: f32 = 0.62;
/// The pane area keeps at least this width; the bars give way first.
pub(crate) const PANES_MIN: f32 = 320.0;

/// One of the two side bars.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Bar {
    /// The tab bar, on the left.
    Tab,
    /// The context bar, on the right.
    Context,
}

impl Bar {
    fn default_width(self) -> f32 {
        match self {
            Bar::Tab => TAB_BAR_DEFAULT,
            Bar::Context => CONTEXT_BAR_DEFAULT,
        }
    }

    fn min_width(self) -> f32 {
        match self {
            Bar::Tab => TAB_BAR_MIN,
            Bar::Context => CONTEXT_BAR_MIN,
        }
    }

    fn of(self, bars: &SideBars) -> &SideBar {
        match self {
            Bar::Tab => &bars.tab_bar,
            Bar::Context => &bars.context_bar,
        }
    }

    fn of_mut(self, bars: &mut SideBars) -> &mut SideBar {
        match self {
            Bar::Tab => &mut bars.tab_bar,
            Bar::Context => &mut bars.context_bar,
        }
    }
}

/// How wide each bar is drawn; `None` when it is hidden.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Drawn {
    pub(crate) tab_bar: Option<f32>,
    pub(crate) context_bar: Option<f32>,
}

/// Whether `bar` is shown.
pub(crate) fn shown(bars: &SideBars, bar: Bar) -> bool {
    !bar.of(bars).hidden
}

/// Hides `bar` when shown, shows it when hidden.
pub(crate) fn toggle(bars: &mut SideBars, bar: Bar) {
    let side = bar.of_mut(bars);
    side.hidden = !side.hidden;
}

/// Shows `bar`.
pub(crate) fn show(bars: &mut SideBars, bar: Bar) {
    bar.of_mut(bars).hidden = false;
}

/// Turns review mode on, showing the context bar, or back off.
pub(crate) fn toggle_review(bars: &mut SideBars) {
    bars.review = !bars.review;
    if bars.review {
        bars.context_bar.hidden = false;
    }
}

/// Puts `bar` back at its default width; the context bar also leaves
/// review mode.
pub(crate) fn reset(bars: &mut SideBars, bar: Bar) {
    bar.of_mut(bars).width = None;
    if bar == Bar::Context {
        bars.review = false;
    }
}

/// Whether `bar`'s edge can be dragged: the context bar keeps its review
/// width until review mode ends, as in the prototype.
pub(crate) fn draggable(bars: &SideBars, bar: Bar) -> bool {
    shown(bars, bar) && !(bar == Bar::Context && bars.review)
}

/// Sets `bar`'s width from the pointer at `x` dragging its inner edge, in
/// a window `window` wide: the tab bar's edge is its right one, the context
/// bar's its left one. Returns whether the width changed.
pub(crate) fn drag_to(bars: &mut SideBars, bar: Bar, x: f32, window: f32) -> bool {
    if !draggable(bars, bar) || !x.is_finite() || !window.is_finite() {
        return false;
    }
    let wanted = match bar {
        Bar::Tab => x,
        Bar::Context => window - x,
    };
    let width = clamp(bar, wanted, window).round();
    let side = bar.of_mut(bars);
    if side.width == Some(width) {
        return false;
    }
    side.width = Some(width);
    true
}

/// How wide each bar is drawn in a window `window` wide. A saved width is
/// clamped between the bar's minimum and half the window; review mode takes
/// its share of the window. When the bars would leave the panes less than
/// [`PANES_MIN`], each gives up width in proportion to what it has above its
/// minimum, so a bar reaches its minimum only when both do; below that they
/// shrink in proportion. The saved widths are never
/// changed, so a window that grows again gets them back.
pub(crate) fn drawn(bars: &SideBars, window: f32) -> Drawn {
    let window = if window.is_finite() {
        window.max(0.0)
    } else {
        0.0
    };
    let wanted = |bar: Bar| -> f32 {
        if !shown(bars, bar) {
            return 0.0;
        }
        if bar == Bar::Context && bars.review {
            return REVIEW_SHARE * window;
        }
        let saved = bar
            .of(bars)
            .width
            .filter(|w| w.is_finite() && *w > 0.0)
            .unwrap_or(bar.default_width());
        clamp(bar, saved, window)
    };
    let (tab, context) = fit(
        (wanted(Bar::Tab), wanted(Bar::Context)),
        (
            min_if_shown(bars, Bar::Tab),
            min_if_shown(bars, Bar::Context),
        ),
        (window - PANES_MIN).max(0.0),
    );
    Drawn {
        tab_bar: shown(bars, Bar::Tab).then_some(tab),
        context_bar: shown(bars, Bar::Context).then_some(context),
    }
}

fn min_if_shown(bars: &SideBars, bar: Bar) -> f32 {
    if shown(bars, bar) {
        bar.min_width()
    } else {
        0.0
    }
}

/// Shrinks two widths into `room`: first down to their minimums, each in
/// proportion to its slack above its own, then both in proportion.
fn fit((a, b): (f32, f32), (min_a, min_b): (f32, f32), room: f32) -> (f32, f32) {
    let total = a + b;
    if total <= room {
        return (a, b);
    }
    let (slack_a, slack_b) = ((a - min_a).max(0.0), (b - min_b).max(0.0));
    let slack = slack_a + slack_b;
    let excess = total - room;
    if slack > 0.0 && slack >= excess {
        let k = excess / slack;
        return (a - slack_a * k, b - slack_b * k);
    }
    let (a, b) = (a.min(min_a), b.min(min_b));
    let total = a + b;
    if total <= room || total <= 0.0 {
        return (a, b);
    }
    let scale = room / total;
    (a * scale, b * scale)
}

fn clamp(bar: Bar, width: f32, window: f32) -> f32 {
    let min = bar.min_width();
    width.clamp(min, (MAX_SHARE * window).max(min))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::region::{Region, next_region};

    const WINDOW: f32 = 1440.0;

    fn sized(tab: Option<f32>, context: Option<f32>) -> SideBars {
        SideBars {
            tab_bar: SideBar {
                hidden: false,
                width: tab,
            },
            context_bar: SideBar {
                hidden: false,
                width: context,
            },
            review: false,
        }
    }

    #[test]
    fn rf_nav_10_the_bars_open_at_the_prototype_widths() {
        let drawn = drawn(&SideBars::default(), WINDOW);
        assert_eq!(drawn.tab_bar, Some(TAB_BAR_DEFAULT));
        assert_eq!(drawn.context_bar, Some(CONTEXT_BAR_DEFAULT));
    }

    #[test]
    fn rf_nav_10_a_hidden_bar_takes_no_room_and_shows_again() {
        let mut bars = SideBars::default();
        toggle(&mut bars, Bar::Context);
        assert!(!shown(&bars, Bar::Context));
        let d = drawn(&bars, WINDOW);
        assert_eq!((d.tab_bar, d.context_bar), (Some(TAB_BAR_DEFAULT), None));
        toggle(&mut bars, Bar::Tab);
        assert_eq!(drawn(&bars, WINDOW).tab_bar, None);
        // A hidden bar cannot be dragged.
        assert!(!drag_to(&mut bars, Bar::Tab, 400.0, WINDOW));
        toggle(&mut bars, Bar::Tab);
        show(&mut bars, Bar::Context);
        assert_eq!(drawn(&bars, WINDOW).context_bar, Some(CONTEXT_BAR_DEFAULT));
    }

    #[test]
    fn rf_nav_10_dragging_the_inner_edge_sets_the_width() {
        let mut bars = SideBars::default();
        assert!(drag_to(&mut bars, Bar::Tab, 300.4, WINDOW));
        assert_eq!(bars.tab_bar.width, Some(300.0));
        // The context bar's inner edge is its left one.
        assert!(drag_to(&mut bars, Bar::Context, WINDOW - 450.0, WINDOW));
        assert_eq!(bars.context_bar.width, Some(450.0));
        // The same width again is no change, so nothing is saved.
        assert!(!drag_to(&mut bars, Bar::Context, WINDOW - 450.0, WINDOW));
        let d = drawn(&bars, WINDOW);
        assert_eq!((d.tab_bar, d.context_bar), (Some(300.0), Some(450.0)));
    }

    #[test]
    fn rf_nav_10_a_drag_stops_at_the_minimum_and_at_half_the_window() {
        let mut bars = SideBars::default();
        drag_to(&mut bars, Bar::Tab, 20.0, WINDOW);
        assert_eq!(bars.tab_bar.width, Some(TAB_BAR_MIN));
        drag_to(&mut bars, Bar::Tab, 1300.0, WINDOW);
        assert_eq!(bars.tab_bar.width, Some(WINDOW * MAX_SHARE));
        drag_to(&mut bars, Bar::Context, WINDOW - 10.0, WINDOW);
        assert_eq!(bars.context_bar.width, Some(CONTEXT_BAR_MIN));
        drag_to(&mut bars, Bar::Context, -500.0, WINDOW);
        assert_eq!(bars.context_bar.width, Some(WINDOW * MAX_SHARE));
        assert!(!drag_to(&mut bars, Bar::Tab, f32::NAN, WINDOW));
    }

    #[test]
    fn rf_nav_10_a_saved_width_out_of_range_is_clamped_when_drawn() {
        let d = drawn(&sized(Some(10.0), Some(5000.0)), WINDOW);
        assert_eq!(d.tab_bar, Some(TAB_BAR_MIN));
        assert_eq!(d.context_bar, Some(WINDOW * MAX_SHARE));
        for bad in [f32::NAN, f32::INFINITY, -3.0, 0.0] {
            let d = drawn(&sized(Some(bad), Some(bad)), WINDOW);
            assert_eq!(d.tab_bar, Some(TAB_BAR_DEFAULT), "{bad}");
            assert_eq!(d.context_bar, Some(CONTEXT_BAR_DEFAULT), "{bad}");
        }
    }

    #[test]
    fn rf_nav_10_a_double_click_resets_to_the_default_width() {
        let mut bars = sized(Some(400.0), Some(600.0));
        bars.review = true;
        reset(&mut bars, Bar::Tab);
        assert_eq!(bars.tab_bar.width, None);
        assert!(bars.review, "the tab bar leaves review mode alone");
        reset(&mut bars, Bar::Context);
        assert_eq!(bars.context_bar.width, None);
        assert!(!bars.review);
        assert_eq!(drawn(&bars, WINDOW), drawn(&SideBars::default(), WINDOW));
    }

    #[test]
    fn rf_nav_10_review_mode_widens_the_context_bar_and_toggles_back() {
        let mut bars = sized(None, Some(420.0));
        bars.context_bar.hidden = true;
        toggle_review(&mut bars);
        assert!(bars.review && shown(&bars, Bar::Context));
        let wide = drawn(&bars, 1920.0).context_bar.unwrap();
        assert_eq!(wide, 1920.0 * REVIEW_SHARE);
        // The review width is not dragged; the saved width waits for it.
        assert!(!drag_to(&mut bars, Bar::Context, 900.0, 1920.0));
        toggle_review(&mut bars);
        assert!(!bars.review);
        assert_eq!(drawn(&bars, 1920.0).context_bar, Some(420.0));
    }

    #[test]
    fn rf_nav_10_a_narrow_window_shrinks_both_bars_toward_their_minimums() {
        let bars = sized(Some(400.0), Some(600.0));
        let window = 960.0;
        let d = drawn(&bars, window);
        let (tab, context) = (d.tab_bar.unwrap(), d.context_bar.unwrap());
        assert!((window - tab - context - PANES_MIN).abs() < 0.01);
        // 400 and 480 (half the window) give up width in proportion to what
        // they have above their minimums, so neither falls below it.
        let given = (400.0 - tab, 480.0 - context);
        let slack = (400.0 - TAB_BAR_MIN, 480.0 - CONTEXT_BAR_MIN);
        assert!((given.0 / given.1 - slack.0 / slack.1).abs() < 1e-3);
        assert!(tab > TAB_BAR_MIN && context > CONTEXT_BAR_MIN);
        // A small tab bar is left alone while the wide context bar gives way.
        let d = drawn(&sized(Some(TAB_BAR_MIN), Some(600.0)), window);
        assert_eq!(d.tab_bar, Some(TAB_BAR_MIN));
        assert!((d.context_bar.unwrap() - (window - PANES_MIN - TAB_BAR_MIN)).abs() < 0.01);
        // The saved widths come back once the window grows.
        assert_eq!(drawn(&bars, 1920.0).tab_bar, Some(400.0));
        assert_eq!(drawn(&bars, 1920.0).context_bar, Some(600.0));
    }

    #[test]
    fn rf_nav_10_review_mode_yields_to_the_panes_in_the_smallest_window() {
        let mut bars = SideBars::default();
        toggle_review(&mut bars);
        let window = 960.0;
        let d = drawn(&bars, window);
        let used = d.tab_bar.unwrap() + d.context_bar.unwrap();
        assert!((window - used - PANES_MIN).abs() < 0.01, "{d:?}");
        assert!(d.context_bar.unwrap() > d.tab_bar.unwrap());
        // With the tab bar hidden the context bar alone yields.
        toggle(&mut bars, Bar::Tab);
        let d = drawn(&bars, window);
        assert_eq!(d.tab_bar, None);
        assert_eq!(d.context_bar, Some(window * REVIEW_SHARE));
        // A window narrower than the pane minimum draws no bar width.
        let d = drawn(&bars, 200.0);
        assert_eq!(d.context_bar, Some(0.0));
    }

    #[test]
    fn rf_nav_10_f6_skips_a_hidden_bar() {
        let mut bars = SideBars::default();
        toggle(&mut bars, Bar::Context);
        let next = |from| {
            next_region(
                from,
                true,
                shown(&bars, Bar::Tab),
                shown(&bars, Bar::Context),
            )
        };
        assert_eq!(next(Region::Panes), Region::TabBar);
        assert_eq!(next(Region::TabBar), Region::Panes);
        toggle(&mut bars, Bar::Tab);
        assert_eq!(
            next_region(
                Region::Panes,
                true,
                shown(&bars, Bar::Tab),
                shown(&bars, Bar::Context)
            ),
            Region::Panes
        );
    }
}
