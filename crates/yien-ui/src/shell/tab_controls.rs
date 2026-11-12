//! The buttons on the tab bar's rows (RF-ESP-02, RF-ESP-03): × on a tab's
//! row closes it.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use yien_proto::TabId;

use super::Shell;
use crate::actions::*;
use crate::assets;
use crate::theme::Theme;

/// The accessible name of a tab row's ×.
pub(crate) fn close_tab_label(tab: &str) -> String {
    format!("Close tab {tab}")
}

/// Whether a row's × shows without the pointer over the row: on the
/// selected row and on the row under the tab bar's keyboard cursor.
pub(crate) fn close_always_shown(active: bool, under_cursor: bool) -> bool {
    active || under_cursor
}

/// The group a tab row's hover is tracked under, so its × shows with it.
pub(super) fn row_group(tab: TabId) -> SharedString {
    format!("tab-row-{}", tab.get()).into()
}

fn small_button(id: ElementId, icon: &'static str, t: &Theme) -> Stateful<Div> {
    let hover = t.hover;
    div()
        .id(id)
        .role(Role::Button)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(20.0))
        .rounded(px(4.0))
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .child(svg().path(icon).size(px(12.0)).text_color(t.faint))
}

impl Shell {
    /// The × on a tab's row. Always laid out, so showing it never moves the
    /// row a drag is measuring; the press stops here, or the row would
    /// start a drag, select or rename the tab.
    pub(super) fn close_tab_button(
        &self,
        tab: TabId,
        name: &str,
        always: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        small_button(("close-tab", tab.get()).into(), assets::CLOSE, &self.theme)
            .size(px(18.0))
            .aria_label(close_tab_label(name))
            .when(!always, |button| {
                button
                    .opacity(0.0)
                    .group_hover(row_group(tab), |s| s.opacity(1.0))
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.close_tab_now(tab, window, cx);
            }))
    }

    /// Closes the tab under the tab bar's cursor while the bar has the
    /// keyboard, else the selected one.
    pub(super) fn close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        match self.keyboard_tab(window, cx) {
            Some(tab) => self.close_tab_now(tab, window, cx),
            None => self.say("There is no tab to close.".to_owned(), cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{close_always_shown, close_tab_label};

    #[test]
    fn rf_esp_03_the_close_button_names_its_tab() {
        assert_eq!(close_tab_label("Tab 2"), "Close tab Tab 2");
    }

    #[test]
    fn rf_esp_02_a_row_close_button_shows_on_the_selected_or_cursor_row() {
        assert!(close_always_shown(true, false));
        assert!(close_always_shown(false, true));
        assert!(!close_always_shown(false, false), "only on hover");
    }
}
