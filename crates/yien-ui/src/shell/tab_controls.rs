//! The buttons on the tab bar's rows (RF-ESP-02, RF-ESP-03): "+" on a
//! project's head opens a tab in that project, × on a tab's row closes it.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use yien_proto::{Command, ProfileId, ProjectId, TabId};

use super::Shell;
use crate::actions::*;
use crate::assets;
use crate::model::ProjectView;
use crate::theme::Theme;

/// The accessible name of a project head's "+".
pub(crate) fn new_tab_label(project: &str) -> String {
    format!("New tab in {project} ({NEW_TAB_KEYS})")
}

/// The accessible name of a tab row's ×.
pub(crate) fn close_tab_label(tab: &str) -> String {
    format!("Close tab {tab}")
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
    /// Opens a shell tab in `project`; the core selects it.
    pub(super) fn new_tab_in(
        &mut self,
        project: ProjectId,
        profile: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        self.command(
            Command::NewTab {
                project,
                name: None,
                profile: profile.map(ProfileId::new),
            },
            cx,
        );
    }

    /// The "+" on a project's head. It unfolds a folded project, or the new
    /// tab would be selected out of sight.
    pub(super) fn new_tab_button(
        &self,
        project: &ProjectView,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let id = project.id;
        small_button(("new-tab-in", id.get()).into(), assets::PLUS, &self.theme)
            .aria_label(new_tab_label(&project.name))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                if this.workspace.is_folded(id) {
                    this.toggle_fold(id, cx);
                }
                this.new_tab_in(id, None, cx);
            }))
    }

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
    use super::{close_tab_label, new_tab_label};
    use crate::catalog::default_keys;

    #[test]
    fn rf_esp_03_the_new_tab_button_names_its_project_and_keys() {
        let label = new_tab_label("yien");
        assert_eq!(label, "New tab in yien (Ctrl+Shift+T)");
        let keys = default_keys("tab.new").unwrap();
        assert!(label.ends_with(&format!("({keys})")));
    }

    #[test]
    fn rf_esp_03_the_close_button_names_its_tab() {
        assert_eq!(close_tab_label("Tab 2"), "Close tab Tab 2");
    }
}
