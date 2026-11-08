//! Hiding and showing the side bars (RF-NAV-10). The model is
//! `crate::sidebars`; this keeps the core told, so the session saves the
//! bars.

use gpui_kit::*;
use yien_proto::{ClientMsg, Command, SideBars};

use super::Shell;
use crate::actions::*;
use crate::region::Region;
use crate::sidebars::{self, Bar};

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
    /// defaults drawn until then would overwrite.
    pub(super) fn save_side_bars(&mut self, cx: &mut Context<Self>) {
        let Some(saved) = self.side_bars_saved else {
            return;
        };
        if saved == self.side_bars {
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
}
