//! The pane menu and the terminal menu (RF-PAN-20), built on the window's
//! popup menu: which items they offer, and what each one does to the pane
//! it was opened for.

use std::time::Instant;

use gpui_kit::*;
use yien_proto::{Command, Orientation, PaneId};

use super::Shell;
use super::menu::{
    EDGE_MARGIN, MENU_WIDTH, Menu, MenuEntry, MenuEvent, MenuItem, MenuOutcome, OpenMenu,
    closed_by_this_click,
};
use crate::actions::*;
use crate::catalog::{shortcut, spec};
use crate::model::PaneView;

/// What the pane and terminal menus run, on the pane they were opened for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PaneAction {
    Copy,
    Paste,
    SplitRight,
    SplitLeft,
    SplitDown,
    SplitUp,
    ToggleMaximize,
    Relaunch,
    Terminate,
    CopySessionId,
    NewTab,
    CloseTab,
    ClosePane,
}

impl PaneAction {
    /// The catalog command that does the same from the palette (RF-NAV-02).
    pub(super) fn command(self) -> &'static str {
        match self {
            PaneAction::Copy => "terminal.copy",
            PaneAction::Paste => "terminal.paste",
            PaneAction::SplitRight => "pane.split_right",
            PaneAction::SplitLeft => "pane.split_left",
            PaneAction::SplitDown => "pane.split_down",
            PaneAction::SplitUp => "pane.split_up",
            PaneAction::ToggleMaximize => "pane.maximize",
            PaneAction::Relaunch => "pane.relaunch",
            PaneAction::Terminate => "pane.terminate",
            PaneAction::CopySessionId => "pane.copy_session_id",
            PaneAction::NewTab => "tab.new",
            PaneAction::CloseTab => "tab.close",
            PaneAction::ClosePane => "pane.close",
        }
    }

    /// `true` puts the new pane left of or above the pane.
    pub(super) fn split(self) -> Option<(Orientation, bool)> {
        match self {
            PaneAction::SplitRight => Some((Orientation::Horizontal, false)),
            PaneAction::SplitLeft => Some((Orientation::Horizontal, true)),
            PaneAction::SplitDown => Some((Orientation::Vertical, false)),
            PaneAction::SplitUp => Some((Orientation::Vertical, true)),
            PaneAction::Copy
            | PaneAction::Paste
            | PaneAction::ToggleMaximize
            | PaneAction::Relaunch
            | PaneAction::Terminate
            | PaneAction::CopySessionId
            | PaneAction::NewTab
            | PaneAction::CloseTab
            | PaneAction::ClosePane => None,
        }
    }
}

fn item(
    label: &str,
    action: PaneAction,
    enabled: bool,
    keys: &dyn Fn(&str) -> Option<String>,
) -> MenuEntry<PaneAction> {
    MenuEntry::Item(MenuItem {
        label: label.to_owned(),
        keys: keys(action.command()),
        enabled,
        action,
    })
}

/// The pane head's menu (RF-PAN-20). `maximized` says the pane fills its tab
/// now, `alone` that it is its tab's only pane; `keys` answers with a
/// command's current keys.
pub(super) fn pane_menu(
    pane: &PaneView,
    maximized: bool,
    alone: bool,
    keys: &dyn Fn(&str) -> Option<String>,
) -> Menu<PaneAction> {
    use PaneAction::*;
    let maximize = if maximized {
        "Restore layout"
    } else {
        "Maximize"
    };
    Menu::new(vec![
        item("Split right", SplitRight, true, keys),
        item("Split left", SplitLeft, true, keys),
        item("Split down", SplitDown, true, keys),
        item("Split up", SplitUp, true, keys),
        MenuEntry::Separator,
        item(maximize, ToggleMaximize, maximized || !alone, keys),
        item("Relaunch", Relaunch, pane.exited, keys),
        item("Terminate", Terminate, !pane.exited, keys),
        item(
            "Copy session id",
            CopySessionId,
            pane.session_id.is_some(),
            keys,
        ),
        MenuEntry::Separator,
        item("Close pane", ClosePane, true, keys),
    ])
}

/// The menu of a right click in the terminal.
pub(super) fn terminal_menu(
    has_selection: bool,
    keys: &dyn Fn(&str) -> Option<String>,
) -> Menu<PaneAction> {
    use PaneAction::*;
    Menu::new(vec![
        item("Copy", Copy, has_selection, keys),
        item("Paste", Paste, true, keys),
        MenuEntry::Separator,
        item("Split right", SplitRight, true, keys),
        item("Split left", SplitLeft, true, keys),
        item("Split down", SplitDown, true, keys),
        item("Split up", SplitUp, true, keys),
        MenuEntry::Separator,
        item("New tab", NewTab, true, keys),
        item("Close tab", CloseTab, true, keys),
        item("Close pane", ClosePane, true, keys),
    ])
}

/// Which menu is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MenuKind {
    Pane,
    Terminal,
}

/// Which of the pane's menus: what closing it from outside remembers.
pub(super) type PaneMenuKey = (PaneId, MenuKind);

/// The pane or terminal menu while open, with the pane it acts on.
pub(super) struct PaneMenu {
    pane: PaneId,
    kind: MenuKind,
    menu: OpenMenu<PaneAction>,
}

/// Whether a menu may open: never over a question the window asks (closing a
/// project or quitting, the worktree question or prompt), which holds the
/// keyboard until answered.
pub(super) fn menu_may_open(confirm: bool, worktree_question: bool, worktree_prompt: bool) -> bool {
    !(confirm || worktree_question || worktree_prompt)
}

impl Shell {
    /// Opens a menu for `pane` with its top left at `at`, moving the
    /// keyboard to it; the pane gets the focus, so the keyboard goes back
    /// there when the menu closes.
    pub(super) fn open_menu(
        &mut self,
        pane: PaneId,
        kind: MenuKind,
        at: Point<Pixels>,
        has_selection: bool,
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
        let Some(tab) = self.workspace.tab_of(pane) else {
            return;
        };
        // Dropped first, so its blur cannot close the menu opened now.
        self.menu = None;
        let maximized = tab.maximized == Some(pane);
        let alone = tab.layout.panes().len() == 1;
        let focused = tab.focused == pane;
        self.split_menu = None;
        self.close_tray(window, cx);
        self.close_palette(window, cx);
        self.finish_rename(true, window, cx);
        if !focused {
            self.command(Command::FocusPane { pane }, cx);
        }
        let model = {
            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            let keys = |id: &str| shortcut(&keymap, (spec(id)?.action)().as_ref());
            match kind {
                MenuKind::Pane => pane_menu(&self.workspace.pane(pane), maximized, alone, &keys),
                MenuKind::Terminal => terminal_menu(has_selection, &keys),
            }
        };
        let name = match kind {
            MenuKind::Pane => format!("{} actions", self.workspace.pane(pane).name),
            MenuKind::Terminal => "Terminal actions".to_owned(),
        };
        // A window key (the palette, F6, the tray) takes the keyboard without
        // closing the menu; left open, it would keep `sync_focus` from moving
        // the keyboard. Whatever took the focus keeps it.
        let menu = OpenMenu::open(model, name, at, window, cx, |this: &mut Self, cx| {
            this.menu = None;
            cx.notify();
        });
        self.menu = Some(PaneMenu { pane, kind, menu });
        cx.notify();
    }

    /// Where the pane menu opens when it is not opened at the pointer: under
    /// the right end of the pane's head.
    fn pane_menu_anchor(&self, pane: PaneId) -> Option<Point<Pixels>> {
        let head = *self.pane_heads.borrow().get(&pane)?;
        Some(point(
            head.right() - px(MENU_WIDTH + 4.0),
            head.bottom() + px(2.0),
        ))
    }

    /// Whether the pane menu of `pane` is open.
    pub(super) fn menu_open_for(&self, pane: PaneId) -> bool {
        self.menu
            .as_ref()
            .is_some_and(|m| m.pane == pane && m.kind == MenuKind::Pane)
    }

    /// The "Pane actions" button toggles the pane menu, unless the
    /// mouse-down of that same click just closed it from outside.
    pub(super) fn pane_menu_clicked(
        &mut self,
        pane: PaneId,
        clicked_at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let closed = self.menu_closed_at.take();
        let just_closed =
            closed_by_this_click(closed.as_ref(), &(pane, MenuKind::Pane), Instant::now());
        if self.menu_open_for(pane) {
            self.close_menu(window, cx);
        } else if !just_closed {
            let at = self.pane_menu_anchor(pane).unwrap_or(clicked_at);
            self.open_menu(pane, MenuKind::Pane, at, false, window, cx);
        }
    }

    /// The `pane.actions` command: the focused pane's menu (RNF-16).
    pub(super) fn open_pane_menu(
        &mut self,
        _: &OpenPaneMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pane) = self.focused_pane() else {
            self.say("There is no pane.", cx);
            return;
        };
        let at = self
            .pane_menu_anchor(pane)
            .unwrap_or_else(|| point(px(EDGE_MARGIN), px(EDGE_MARGIN)));
        self.open_menu(pane, MenuKind::Pane, at, false, window, cx);
    }

    pub(super) fn close_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            self.give_back_keyboard(window, cx);
        }
    }

    /// A menu for a pane that left, or whose tab is no longer shown, goes.
    pub(super) fn drop_stale_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let stale = self.menu.as_ref().is_some_and(|m| {
            self.workspace.tab_of(m.pane).map(|t| t.id)
                != self.workspace.selected_tab().map(|t| t.id)
        });
        if stale {
            self.close_menu(window, cx);
        }
    }

    /// A key or click in the open menu. Closing runs first, so the keyboard
    /// is back on the pane when an action runs.
    fn on_menu_event(&mut self, event: MenuEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(open) = self.menu.as_mut() else {
            return;
        };
        let key: PaneMenuKey = (open.pane, open.kind);
        let pane = open.pane;
        match open.menu.model_mut().respond(event) {
            MenuOutcome::Stay => cx.notify(),
            MenuOutcome::Close => {
                if event == MenuEvent::ClickedOutside {
                    self.menu_closed_at = Some((key, Instant::now()));
                }
                self.close_menu(window, cx);
            }
            MenuOutcome::Run(action) => {
                self.close_menu(window, cx);
                self.run_pane_action(pane, action, window, cx);
            }
        }
    }

    /// The open pane or terminal menu.
    pub(super) fn render_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let open = self.menu.as_ref()?;
        Some(open.menu.render(&self.theme, cx, Self::on_menu_event))
    }

    fn run_pane_action(
        &mut self,
        pane: PaneId,
        action: PaneAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((orientation, before)) = action.split() {
            self.split_pane(pane, orientation, before, None, cx);
            return;
        }
        match action {
            PaneAction::Copy => {
                if let Some(terminal) = self.terminals.get(&pane) {
                    terminal.update(cx, |t, cx| t.copy_selection(cx));
                }
            }
            PaneAction::Paste => {
                if let Some(terminal) = self.terminals.get(&pane) {
                    terminal.update(cx, |t, cx| t.paste_clipboard(cx));
                }
            }
            // Run by `split()` above.
            PaneAction::SplitRight
            | PaneAction::SplitLeft
            | PaneAction::SplitDown
            | PaneAction::SplitUp => {}
            PaneAction::ToggleMaximize => self.command(Command::ToggleMaximize { pane }, cx),
            PaneAction::Relaunch => self.command(Command::RelaunchPane { pane }, cx),
            PaneAction::Terminate => self.terminate(pane, cx),
            PaneAction::CopySessionId => self.copy_session_id_of(pane, cx),
            PaneAction::NewTab => self.new_tab_with(None, cx),
            PaneAction::CloseTab => {
                if let Some(tab) = self.workspace.tab_of(pane).map(|t| t.id) {
                    self.close_tab_now(tab, window, cx);
                }
            }
            PaneAction::ClosePane => self.close_pane_id(pane, window, cx),
        }
    }

    /// Ends the pane's program and keeps the pane (RF-PAN-20).
    fn terminate(&mut self, id: PaneId, cx: &mut Context<Self>) {
        let pane = self.workspace.pane(id);
        if pane.exited {
            self.say(format!("{} is not running.", pane.name), cx);
        } else {
            self.command(Command::TerminatePane { pane: id }, cx);
        }
    }

    pub(super) fn terminate_pane(
        &mut self,
        _: &TerminatePane,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(pane) = self.focused_pane() {
            self.terminate(pane, cx);
        }
    }

    fn copy_session_id_of(&mut self, id: PaneId, cx: &mut Context<Self>) {
        let pane = self.workspace.pane(id);
        match pane.session_id {
            Some(session) => {
                cx.write_to_clipboard(ClipboardItem::new_string(session));
                self.say(format!("Copied the session id of {}.", pane.name), cx);
            }
            None => self.say(format!("{} has no agent session.", pane.name), cx),
        }
    }

    pub(super) fn copy_session_id(
        &mut self,
        _: &CopySessionId,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(pane) = self.focused_pane() {
            self.copy_session_id_of(pane, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: the gpui glob there would shadow `#[test]`.
    use super::{Menu, MenuEntry, PaneAction, menu_may_open, pane_menu, terminal_menu};
    use crate::catalog::spec;
    use crate::model::{AgentState, PaneView};
    use yien_proto::{PaneId, ProfileId};

    fn no_keys(_: &str) -> Option<String> {
        None
    }

    fn pane() -> PaneView {
        PaneView {
            id: PaneId::new(1),
            name: "claude".into(),
            profile: "Claude Code".into(),
            cwd: None,
            profile_id: ProfileId::new("claude"),
            state: AgentState::Idle,
            needs_reason: None,
            running: true,
            exit_code: None,
            exited: false,
            needs_you_since: None,
            attention_since: None,
            question: None,
            signal_lost: false,
            session_id: None,
            runs_session: true,
        }
    }

    fn items(menu: &Menu<PaneAction>) -> Vec<(String, bool)> {
        menu.entries()
            .iter()
            .filter_map(|e| match e {
                MenuEntry::Item(i) => Some((i.label.clone(), i.enabled)),
                MenuEntry::Separator => None,
            })
            .collect()
    }

    fn enabled(menu: &Menu<PaneAction>, label: &str) -> bool {
        items(menu)
            .into_iter()
            .find(|(l, _)| l == label)
            .unwrap_or_else(|| panic!("no {label}"))
            .1
    }

    #[test]
    fn rf_pan_20_the_pane_menu_offers_relaunch_terminate_and_close() {
        let menu = pane_menu(&pane(), false, false, &no_keys);
        let labels: Vec<String> = items(&menu).into_iter().map(|(l, _)| l).collect();
        assert_eq!(
            labels,
            [
                "Split right",
                "Split left",
                "Split down",
                "Split up",
                "Maximize",
                "Relaunch",
                "Terminate",
                "Copy session id",
                "Close pane",
            ]
        );
        assert!(enabled(&menu, "Terminate"));
        assert!(enabled(&menu, "Close pane"));
        // A running program is relaunched only after it stopped, as
        // `pane.relaunch` does.
        assert!(!enabled(&menu, "Relaunch"));
        let separators = menu
            .entries()
            .iter()
            .filter(|e| matches!(e, MenuEntry::Separator))
            .count();
        assert_eq!(separators, 2);
    }

    #[test]
    fn rf_pan_20_terminate_is_disabled_for_an_exited_pane() {
        let exited = PaneView {
            running: false,
            exit_code: Some(1),
            exited: true,
            ..pane()
        };
        let menu = pane_menu(&exited, false, false, &no_keys);
        assert!(!enabled(&menu, "Terminate"));
        assert!(enabled(&menu, "Relaunch"));
    }

    #[test]
    fn rf_pan_20_the_session_id_is_copied_only_once_known() {
        assert!(!enabled(
            &pane_menu(&pane(), false, false, &no_keys),
            "Copy session id"
        ));
        let known = PaneView {
            session_id: Some("aaaaaaaa-0000-4000-8000-000000000001".into()),
            ..pane()
        };
        assert!(enabled(
            &pane_menu(&known, false, false, &no_keys),
            "Copy session id"
        ));
    }

    #[test]
    fn rf_pan_04_the_menu_says_restore_layout_while_maximized() {
        let menu = pane_menu(&pane(), true, false, &no_keys);
        assert!(enabled(&menu, "Restore layout"));
        assert!(items(&menu).iter().all(|(l, _)| l != "Maximize"));
        // A pane alone in its tab has nothing to maximize over.
        assert!(!enabled(
            &pane_menu(&pane(), false, true, &no_keys),
            "Maximize"
        ));
    }

    #[test]
    fn rf_pan_09_copy_is_disabled_without_a_selection() {
        assert!(!enabled(&terminal_menu(false, &no_keys), "Copy"));
        assert!(enabled(&terminal_menu(true, &no_keys), "Copy"));
        assert!(enabled(&terminal_menu(false, &no_keys), "Paste"));
    }

    #[test]
    fn rf_pan_01_the_terminal_menu_splits_and_closes() {
        let labels: Vec<String> = items(&terminal_menu(false, &no_keys))
            .into_iter()
            .map(|(l, _)| l)
            .collect();
        assert_eq!(
            labels,
            [
                "Copy",
                "Paste",
                "Split right",
                "Split left",
                "Split down",
                "Split up",
                "New tab",
                "Close tab",
                "Close pane",
            ]
        );
    }

    #[test]
    fn rf_pan_01_each_split_item_puts_the_new_pane_on_its_side() {
        use yien_proto::Orientation::{Horizontal, Vertical};
        let sides = [
            ("Split right", Horizontal, false),
            ("Split left", Horizontal, true),
            ("Split down", Vertical, false),
            ("Split up", Vertical, true),
        ];
        for menu in [
            pane_menu(&pane(), false, false, &no_keys),
            terminal_menu(false, &no_keys),
        ] {
            let splits: Vec<_> = menu
                .entries()
                .iter()
                .filter_map(|e| match e {
                    MenuEntry::Item(i) => i.action.split().map(|(o, before)| {
                        let title = spec(i.action.command()).unwrap().title;
                        assert_eq!(title, format!("pane: {}", i.label.to_lowercase()));
                        (i.label.as_str(), o, before)
                    }),
                    MenuEntry::Separator => None,
                })
                .collect();
            assert_eq!(splits, sides);
        }
        assert_eq!(PaneAction::ClosePane.split(), None);
    }

    #[test]
    fn rf_nav_02_every_menu_item_is_a_palette_command_and_shows_its_keys() {
        let keys = |id: &str| crate::catalog::default_keys(id);
        let menus = [
            pane_menu(&pane(), false, false, &keys),
            terminal_menu(true, &keys),
        ];
        for menu in &menus {
            for entry in menu.entries() {
                if let MenuEntry::Item(item) = entry {
                    let id = item.action.command();
                    assert!(spec(id).is_some(), "{} has no command {id}", item.label);
                    assert_eq!(item.keys, crate::catalog::default_keys(id), "{id}");
                }
            }
        }
        let close = menus[0].entries().iter().find_map(|e| match e {
            MenuEntry::Item(i) if i.action == PaneAction::ClosePane => i.keys.clone(),
            _ => None,
        });
        assert_eq!(close.as_deref(), Some("Ctrl+Shift+W"));
    }

    #[test]
    fn no_menu_opens_over_a_question() {
        assert!(menu_may_open(false, false, false));
        assert!(!menu_may_open(true, false, false), "close or quit question");
        assert!(!menu_may_open(false, true, false), "worktree question");
        assert!(!menu_may_open(false, false, true), "worktree prompt");
    }
}
