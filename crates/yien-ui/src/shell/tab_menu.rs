//! The tab menu (RF-ESP-16), built on the window's popup menu: a right click
//! on a tab row, or Shift+F10 on the tab bar's cursor, offers the tab
//! commands for that tab, whichever tab is selected.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::*;
use yien_proto::{ProjectId, TabId};

use super::Shell;
use super::menu::{EDGE_MARGIN, Menu, MenuEntry, MenuEvent, MenuItem, MenuOutcome, OpenMenu};
use super::pane_menu::menu_may_open;
use crate::actions::TabBarMenu;
use crate::catalog::{shortcut, spec};
use crate::model::WorkspaceView;
use crate::region::Region;
use crate::tab_order;

/// What the tab menu runs, on the tab it was opened for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TabAction {
    Rename,
    MoveUp,
    MoveDown,
    Close,
}

impl TabAction {
    /// The catalog command that does the same from the palette (RF-NAV-02).
    pub(super) fn command(self) -> &'static str {
        match self {
            TabAction::Rename => "tab.rename",
            TabAction::MoveUp => "tab.move_up",
            TabAction::MoveDown => "tab.move_down",
            TabAction::Close => "tab.close",
        }
    }
}

/// What a tab menu item does, and to which tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TabEffect {
    Rename(TabId),
    Move {
        tab: TabId,
        project: ProjectId,
        to: usize,
    },
    Close(TabId),
}

/// What `action` does to `tab` as the window shows it now; `None` when the
/// tab is gone, or a move would go past an end of its project.
pub(super) fn tab_effect(view: &WorkspaceView, tab: TabId, action: TabAction) -> Option<TabEffect> {
    match action {
        TabAction::Rename => view.tab(tab).map(|_| TabEffect::Rename(tab)),
        TabAction::Close => view.tab(tab).map(|_| TabEffect::Close(tab)),
        TabAction::MoveUp | TabAction::MoveDown => {
            let (project, from, len) = view.place_of(tab)?;
            let to = tab_order::step(from, len, action == TabAction::MoveDown)?;
            Some(TabEffect::Move { tab, project, to })
        }
    }
}

/// The menu of `tab`, `None` when it is gone. An item that would do nothing
/// now is shown disabled; `keys` answers with a command's current keys.
pub(super) fn tab_menu(
    view: &WorkspaceView,
    tab: TabId,
    keys: &dyn Fn(&str) -> Option<String>,
) -> Option<Menu<TabAction>> {
    view.tab(tab)?;
    let item = |label: &str, action: TabAction| {
        MenuEntry::Item(MenuItem {
            label: label.to_owned(),
            keys: keys(action.command()),
            enabled: tab_effect(view, tab, action).is_some(),
            action,
        })
    };
    Some(Menu::new(vec![
        item("Rename", TabAction::Rename),
        MenuEntry::Separator,
        item("Move up", TabAction::MoveUp),
        item("Move down", TabAction::MoveDown),
        MenuEntry::Separator,
        item("Close tab", TabAction::Close),
    ]))
}

/// The tab menu while open, and where the tab rows were painted.
#[derive(Default)]
pub(super) struct TabMenuState {
    open: Option<OpenTabMenu>,
    rows: Rc<RefCell<HashMap<TabId, Bounds<Pixels>>>>,
}

struct OpenTabMenu {
    tab: TabId,
    /// Opened while the tab bar had the keyboard, which it gets back.
    back_to_bar: bool,
    menu: OpenMenu<TabAction>,
}

impl TabMenuState {
    pub(super) fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Drops the menu without moving the keyboard, for a menu opening now.
    pub(super) fn forget(&mut self) {
        self.open = None;
    }
}

impl Shell {
    /// Lets a right click on a tab row open its menu, and records where the
    /// row is for the menu opened by keyboard.
    pub(super) fn tab_menu_row(
        &self,
        row: Stateful<Div>,
        tab: TabId,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let rows = Rc::clone(&self.tab_menu.rows);
        row.on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                if this.renaming.as_ref().is_some_and(|r| r.tab == tab) {
                    return;
                }
                // Or the tab bar, which tracks focus, takes the keyboard back
                // from the menu as this mouse-down bubbles up.
                window.prevent_default();
                this.open_tab_menu(tab, event.position, window, cx);
            }),
        )
        .child(
            canvas(
                move |bounds, _, _| {
                    rows.borrow_mut().insert(tab, bounds);
                },
                |_, _, _, _| {},
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        )
    }

    /// Opens the menu of `tab` with its top left at `at`, moving the
    /// keyboard to it. Closing gives the keyboard back to the tab bar when
    /// it had it, else to the pane.
    pub(super) fn open_tab_menu(
        &mut self,
        tab: TabId,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !menu_may_open(
            self.confirm.is_some(),
            self.worktree_close.is_some(),
            self.worktree_prompt.is_some(),
        ) {
            return;
        }
        let Some(name) = self.workspace.tab(tab).map(|t| t.name.clone()) else {
            return;
        };
        let back_to_bar = self.region(window, cx) == Region::TabBar;
        // Dropped first, so their blur cannot close the menu opened now.
        self.tab_menu.open = None;
        self.menu = None;
        self.split_menu = None;
        self.close_tray(window, cx);
        self.close_palette(window, cx);
        self.finish_rename(true, window, cx);
        let model = {
            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            let keys = |id: &str| shortcut(&keymap, (spec(id)?.action)().as_ref());
            tab_menu(&self.workspace, tab, &keys)
        };
        let Some(model) = model else {
            return;
        };
        let menu = OpenMenu::open(
            model,
            format!("Tab actions for {name}"),
            at,
            window,
            cx,
            |this: &mut Self, cx| {
                this.tab_menu.open = None;
                cx.notify();
            },
        );
        self.tab_menu.open = Some(OpenTabMenu {
            tab,
            back_to_bar,
            menu,
        });
        cx.notify();
    }

    /// Shift+F10 in the tab bar: the menu of the tab under its cursor, under
    /// that tab's row.
    pub(super) fn tab_bar_menu(
        &mut self,
        _: &TabBarMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.keyboard_tab(window, cx) else {
            return;
        };
        let row = self.tab_menu.rows.borrow().get(&tab).copied();
        let at = row.map_or_else(
            || point(px(EDGE_MARGIN), px(EDGE_MARGIN)),
            |row| point(row.left() + px(8.0), row.bottom() + px(2.0)),
        );
        self.open_tab_menu(tab, at, window, cx);
    }

    fn close_tab_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(open) = self.tab_menu.open.take() else {
            return;
        };
        if open.back_to_bar {
            window.focus(&self.tab_bar_focus, cx);
            self.keyboard_in_bar = true;
            cx.notify();
        } else {
            self.give_back_keyboard(window, cx);
        }
    }

    /// A menu for a tab that closed goes; so do the places of closed tabs'
    /// rows. A tab that is not the selected one keeps its menu.
    pub(super) fn drop_stale_tab_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.tab_menu
            .rows
            .borrow_mut()
            .retain(|tab, _| self.workspace.tab(*tab).is_some());
        let stale = self
            .tab_menu
            .open
            .as_ref()
            .is_some_and(|m| self.workspace.tab(m.tab).is_none());
        if stale {
            self.close_tab_menu(window, cx);
        }
    }

    /// A key or click in the open tab menu. Closing runs first, so the
    /// keyboard is back where it was when an action runs.
    fn on_tab_menu_event(&mut self, event: MenuEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(open) = self.tab_menu.open.as_mut() else {
            return;
        };
        let tab = open.tab;
        match open.menu.model_mut().respond(event) {
            MenuOutcome::Stay => cx.notify(),
            MenuOutcome::Close => self.close_tab_menu(window, cx),
            MenuOutcome::Run(action) => {
                self.close_tab_menu(window, cx);
                self.run_tab_action(tab, action, window, cx);
            }
        }
    }

    pub(super) fn render_tab_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let open = self.tab_menu.open.as_ref()?;
        Some(open.menu.render(&self.theme, cx, Self::on_tab_menu_event))
    }

    fn run_tab_action(
        &mut self,
        tab: TabId,
        action: TabAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match tab_effect(&self.workspace, tab, action) {
            Some(TabEffect::Rename(tab)) => self.start_rename(tab, window, cx),
            Some(TabEffect::Move { tab, project, to }) => self.move_tab_to(tab, project, to, cx),
            Some(TabEffect::Close(tab)) => self.close_tab_now(tab, window, cx),
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: the gpui glob there would shadow `#[test]`.
    use super::{
        Menu, MenuEntry, MenuEvent, MenuOutcome, TabAction, TabEffect, tab_effect, tab_menu,
    };
    use crate::catalog::{default_keys, spec};
    use crate::model::WorkspaceView;
    use yien_proto::{
        LayoutNode, PaneId, ProjectId, ProjectInfo, TabId, TabInfo, WorkspaceSnapshot,
    };

    fn no_keys(_: &str) -> Option<String> {
        None
    }

    /// Project 1 holds `tabs` in that order; `selected` is the active tab.
    fn view(tabs: &[u64], selected: u64) -> WorkspaceView {
        WorkspaceView::from_snapshot(&WorkspaceSnapshot {
            projects: vec![ProjectInfo {
                id: ProjectId::new(1),
                name: "yien".into(),
                root: "C:/Code/yien".into(),
                tabs: tabs.iter().map(|t| TabId::new(*t)).collect(),
                collapsed: false,
            }],
            tabs: tabs
                .iter()
                .map(|t| TabInfo {
                    id: TabId::new(*t),
                    project: ProjectId::new(1),
                    name: format!("tab-{t}"),
                    dir: "C:/Code/yien".into(),
                    branch: None,
                    layout: LayoutNode::Pane(PaneId::new(t * 10)),
                    focused: PaneId::new(t * 10),
                    maximized: None,
                    muted: false,
                    worktree: None,
                })
                .collect(),
            active_tab: Some(TabId::new(selected)),
            ..Default::default()
        })
    }

    fn items(menu: &Menu<TabAction>) -> Vec<(String, bool)> {
        menu.entries()
            .iter()
            .filter_map(|e| match e {
                MenuEntry::Item(i) => Some((i.label.clone(), i.enabled)),
                MenuEntry::Separator => None,
            })
            .collect()
    }

    fn menu_of(tabs: &[u64], tab: u64) -> Menu<TabAction> {
        tab_menu(&view(tabs, tabs[0]), TabId::new(tab), &no_keys).unwrap()
    }

    #[test]
    fn rf_esp_16_the_tab_menu_offers_rename_moves_and_close_last() {
        let menu = menu_of(&[1, 2, 3], 2);
        let all = |on: bool| {
            vec![
                ("Rename".to_owned(), true),
                ("Move up".to_owned(), on),
                ("Move down".to_owned(), on),
                ("Close tab".to_owned(), true),
            ]
        };
        assert_eq!(items(&menu), all(true), "a middle tab");
        assert!(matches!(menu.entries().first(), Some(MenuEntry::Item(_))));
        let n = menu.entries().len();
        assert!(matches!(menu.entries()[n - 2], MenuEntry::Separator));
        assert!(
            matches!(&menu.entries()[n - 1], MenuEntry::Item(i) if i.action == TabAction::Close)
        );
        assert_eq!(items(&menu_of(&[1], 1)), all(false), "a single tab");
    }

    #[test]
    fn rf_esp_16_move_up_and_down_are_off_at_the_ends() {
        let enabled = |menu: &Menu<TabAction>, label: &str| {
            items(menu)
                .into_iter()
                .find(|(l, _)| l == label)
                .unwrap_or_else(|| panic!("no {label}"))
                .1
        };
        let first = menu_of(&[1, 2, 3], 1);
        assert!(!enabled(&first, "Move up"));
        assert!(enabled(&first, "Move down"));
        let last = menu_of(&[1, 2, 3], 3);
        assert!(enabled(&last, "Move up"));
        assert!(!enabled(&last, "Move down"));
        // The cursor never rests on an item that would do nothing.
        let mut single = menu_of(&[1], 1);
        assert_eq!(single.chosen(), Some(&TabAction::Rename));
        single.move_by(1);
        assert_eq!(single.chosen(), Some(&TabAction::Close));
        assert_eq!(single.respond(MenuEvent::Pick(2)), MenuOutcome::Stay);
    }

    #[test]
    fn rf_esp_16_the_menu_acts_on_the_right_clicked_tab_not_the_selected_one() {
        // Tab 2 is selected; the menu was opened on tab 3.
        let view = view(&[1, 2, 3], 2);
        let (clicked, project) = (TabId::new(3), ProjectId::new(1));
        assert_eq!(view.selected_tab().map(|t| t.id), Some(TabId::new(2)));
        let mut menu = tab_menu(&view, clicked, &no_keys).unwrap();
        let mut run = |event: MenuEvent| match menu.respond(event) {
            MenuOutcome::Run(action) => tab_effect(&view, clicked, action),
            other => panic!("{other:?}"),
        };
        assert_eq!(run(MenuEvent::Confirm), Some(TabEffect::Rename(clicked)));
        assert_eq!(
            run(MenuEvent::Pick(2)),
            Some(TabEffect::Move {
                tab: clicked,
                project,
                to: 1
            })
        );
        assert_eq!(run(MenuEvent::Pick(5)), Some(TabEffect::Close(clicked)));
        assert_eq!(tab_effect(&view, clicked, TabAction::MoveDown), None);
        assert_eq!(tab_effect(&view, TabId::new(9), TabAction::Close), None);
        assert!(tab_menu(&view, TabId::new(9), &no_keys).is_none());
    }

    #[test]
    fn rf_nav_02_every_tab_menu_item_is_a_palette_command_and_shows_its_keys() {
        let keys = |id: &str| default_keys(id);
        let menu = tab_menu(&view(&[1, 2], 1), TabId::new(1), &keys).unwrap();
        for entry in menu.entries() {
            if let MenuEntry::Item(item) = entry {
                let id = item.action.command();
                assert!(spec(id).is_some(), "{} has no command {id}", item.label);
                assert_eq!(item.keys, default_keys(id), "{id}");
            }
        }
        let move_up = menu.entries().iter().find_map(|e| match e {
            MenuEntry::Item(i) if i.action == TabAction::MoveUp => i.keys.clone(),
            _ => None,
        });
        assert_eq!(move_up, default_keys("tab.move_up"));
        assert!(move_up.is_some());
    }
}
