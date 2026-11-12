//! The pane menu and the terminal menu (RF-PAN-20): a small popup drawn at a
//! window position, over a pure model of items, separators and a cursor.

use std::time::{Duration, Instant};

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use yien_proto::{Command, Orientation, PaneId, TabId};

use super::Shell;
use crate::actions::*;
use crate::catalog::{shortcut, spec};
use crate::model::PaneView;

const MENU_WIDTH: f32 = 240.0;
/// How close to the window's edges a menu may come.
const EDGE_MARGIN: f32 = 8.0;

/// One thing a menu can run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MenuItem<A> {
    pub(crate) label: String,
    /// The keys that run the same command, as the user reads them.
    pub(crate) keys: Option<String>,
    pub(crate) enabled: bool,
    pub(crate) action: A,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MenuEntry<A> {
    Item(MenuItem<A>),
    Separator,
}

/// A menu's items and the one the arrows are on. The cursor rests only on
/// enabled items, and there is none when nothing is enabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Menu<A> {
    entries: Vec<MenuEntry<A>>,
    cursor: Option<usize>,
}

impl<A> Menu<A> {
    /// A menu with the cursor on its first enabled item.
    pub(crate) fn new(entries: Vec<MenuEntry<A>>) -> Self {
        let mut menu = Self {
            entries,
            cursor: None,
        };
        menu.cursor = menu.step(None, 1);
        menu
    }

    pub(crate) fn entries(&self) -> &[MenuEntry<A>] {
        &self.entries
    }

    pub(crate) fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    /// Moves the cursor `by` enabled items, down when positive, wrapping
    /// around the ends.
    pub(crate) fn move_by(&mut self, by: isize) {
        for _ in 0..by.unsigned_abs() {
            self.cursor = self.step(self.cursor, by.signum());
        }
    }

    /// The action of the item at `at`, `None` for a separator or an item
    /// that cannot run now.
    pub(crate) fn pick(&self, at: usize) -> Option<&A> {
        match self.entries.get(at) {
            Some(MenuEntry::Item(item)) if item.enabled => Some(&item.action),
            _ => None,
        }
    }

    /// The action under the cursor.
    pub(crate) fn chosen(&self) -> Option<&A> {
        self.pick(self.cursor?)
    }

    fn step(&self, from: Option<usize>, dir: isize) -> Option<usize> {
        let n = self.entries.len();
        let mut at = from;
        for _ in 0..n {
            let next = match (at, dir > 0) {
                (None, true) => 0,
                (None, false) => n - 1,
                (Some(i), true) => (i + 1) % n,
                (Some(i), false) => (i + n - 1) % n,
            };
            if self.pick(next).is_some() {
                return Some(next);
            }
            at = Some(next);
        }
        None
    }
}

/// What the pane and terminal menus run, on the pane they were opened for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PaneAction {
    Copy,
    Paste,
    SplitRight,
    SplitDown,
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
            PaneAction::SplitDown => "pane.split_down",
            PaneAction::ToggleMaximize => "pane.maximize",
            PaneAction::Relaunch => "pane.relaunch",
            PaneAction::Terminate => "pane.terminate",
            PaneAction::CopySessionId => "pane.copy_session_id",
            PaneAction::NewTab => "tab.new",
            PaneAction::CloseTab => "tab.close",
            PaneAction::ClosePane => "pane.close",
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
        item("Split down", SplitDown, true, keys),
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
        item("Split down", SplitDown, true, keys),
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

/// The menu while open, for one pane.
pub(super) struct OpenMenu {
    model: Menu<PaneAction>,
    pane: PaneId,
    kind: MenuKind,
    at: Point<Pixels>,
    focus: FocusHandle,
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
        let Some(tab) = self.workspace.tab_of(pane) else {
            return;
        };
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
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        self.menu = Some(OpenMenu {
            model,
            pane,
            kind,
            at,
            focus,
        });
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
        let just_closed = self
            .menu_closed_at
            .take()
            .is_some_and(|(p, at)| p == pane && at.elapsed() < Duration::from_millis(500));
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

    fn menu_up(&mut self, _: &MenuUp, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(menu) = self.menu.as_mut() {
            menu.model.move_by(-1);
            cx.notify();
        }
    }

    fn menu_down(&mut self, _: &MenuDown, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(menu) = self.menu.as_mut() {
            menu.model.move_by(1);
            cx.notify();
        }
    }

    fn menu_confirm(&mut self, _: &MenuConfirm, window: &mut Window, cx: &mut Context<Self>) {
        self.run_menu(None, window, cx);
    }

    fn menu_close(&mut self, _: &MenuClose, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menu(window, cx);
    }

    /// Runs item `at`, or the one under the cursor when `None`. The menu
    /// closes first, so the keyboard is back on the pane when it runs.
    fn run_menu(&mut self, at: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = &self.menu else {
            return;
        };
        let action = match at {
            Some(at) => menu.model.pick(at),
            None => menu.model.chosen(),
        };
        let Some(action) = action.copied() else {
            return;
        };
        let pane = menu.pane;
        self.close_menu(window, cx);
        self.run_pane_action(pane, action, window, cx);
    }

    fn run_pane_action(
        &mut self,
        pane: PaneId,
        action: PaneAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
            PaneAction::SplitRight => self.split_pane(pane, Orientation::Horizontal, None, cx),
            PaneAction::SplitDown => self.split_pane(pane, Orientation::Vertical, None, cx),
            PaneAction::ToggleMaximize => self.command(Command::ToggleMaximize { pane }, cx),
            PaneAction::Relaunch => self.command(Command::RelaunchPane { pane }, cx),
            PaneAction::Terminate => self.terminate(pane, cx),
            PaneAction::CopySessionId => self.copy_session_id_of(pane, cx),
            PaneAction::NewTab => self.new_tab_with(None, cx),
            PaneAction::CloseTab => {
                if let Some(tab) = self.workspace.tab_of(pane).map(|t| t.id) {
                    self.close_tab_id(tab, window, cx);
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

    /// Closes a whole tab; the last tab on a worktree asks first whether to
    /// keep the worktree (RF-ESP-07).
    fn close_tab_id(&mut self, tab: TabId, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ask_before_closing_tab(tab, window, cx) {
            self.command(Command::CloseTab { tab }, cx);
        }
    }

    pub(super) fn close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        match self.workspace.selected_tab().map(|t| t.id) {
            Some(tab) => self.close_tab_id(tab, window, cx),
            None => {
                self.say("There is no tab to close.", cx);
            }
        }
    }

    /// The open menu, drawn over everything at its window position and kept
    /// inside the window.
    pub(super) fn render_menu(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let open = self.menu.as_ref()?;
        let t = self.theme;
        let cursor = open.model.cursor();
        let rows: Vec<AnyElement> = open
            .model
            .entries()
            .iter()
            .enumerate()
            .map(|(at, entry)| match entry {
                MenuEntry::Separator => div()
                    .mx(px(6.0))
                    .my(px(4.0))
                    .h(px(1.0))
                    .bg(t.term_line)
                    .into_any_element(),
                MenuEntry::Item(item) => self.render_menu_item(at, item, cursor == Some(at), cx),
            })
            .collect();
        let name = match open.kind {
            MenuKind::Pane => format!("{} actions", self.workspace.pane(open.pane).name),
            MenuKind::Terminal => "Terminal actions".to_owned(),
        };
        let pane = open.pane;
        let menu = v_flex()
            .id("pane-menu")
            .role(Role::Menu)
            .aria_label(name)
            .track_focus(&open.focus)
            .key_context(MENU_CONTEXT)
            .occlude()
            .w(px(MENU_WIDTH))
            .p(px(4.0))
            .rounded(px(7.0))
            .border_1()
            .border_color(t.term_strong)
            .bg(t.term_bg)
            .text_size(t.text(12.5))
            .shadow_lg()
            .on_action(cx.listener(Self::menu_up))
            .on_action(cx.listener(Self::menu_down))
            .on_action(cx.listener(Self::menu_confirm))
            .on_action(cx.listener(Self::menu_close))
            .on_mouse_down_out(cx.listener(move |this, _, window, cx| {
                this.menu_closed_at = Some((pane, Instant::now()));
                this.close_menu(window, cx);
            }))
            .children(rows);
        Some(deferred(
            anchored()
                .position(open.at)
                .snap_to_window_with_margin(px(EDGE_MARGIN))
                .child(menu),
        ))
    }

    fn render_menu_item(
        &self,
        at: usize,
        item: &MenuItem<PaneAction>,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = self.theme;
        let hover = t.term_raised;
        let enabled = item.enabled;
        h_flex()
            .id(("menu-item", at))
            .role(Role::MenuItem)
            .aria_label(item.label.clone())
            .when_some(item.keys.clone(), |d, keys| d.aria_keyshortcuts(keys))
            .when(!enabled, |d| d.aria_description("Unavailable"))
            .items_center()
            .gap(px(12.0))
            .px(px(10.0))
            .py(px(5.0))
            .rounded(px(5.0))
            .when(selected, |d| d.bg(t.term_raised))
            .when(enabled, |d| {
                d.cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.run_menu(Some(at), window, cx);
                    }))
            })
            .child(
                div()
                    .flex_1()
                    .text_color(if enabled { t.term_text } else { t.term_faint })
                    .child(item.label.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(t.term_faint)
                    .child(item.keys.clone().unwrap_or_default()),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: the gpui glob there would shadow `#[test]`.
    use super::{Menu, MenuEntry, MenuItem, PaneAction, pane_menu, terminal_menu};
    use crate::catalog::spec;
    use crate::model::{AgentState, PaneView};
    use yien_proto::{PaneId, ProfileId};

    fn entry(label: &'static str, enabled: bool) -> MenuEntry<&'static str> {
        MenuEntry::Item(MenuItem {
            label: label.to_owned(),
            keys: None,
            enabled,
            action: label,
        })
    }

    fn sample() -> Menu<&'static str> {
        Menu::new(vec![
            entry("off", false),
            entry("a", true),
            MenuEntry::Separator,
            entry("b", true),
            entry("c", false),
            MenuEntry::Separator,
        ])
    }

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
    fn the_cursor_starts_on_the_first_enabled_item() {
        let menu = sample();
        assert_eq!(menu.cursor(), Some(1));
        assert_eq!(menu.chosen(), Some(&"a"));
    }

    #[test]
    fn the_cursor_skips_separators_and_disabled_items_and_wraps() {
        let mut menu = sample();
        menu.move_by(1);
        assert_eq!(menu.chosen(), Some(&"b"));
        menu.move_by(1);
        assert_eq!(menu.chosen(), Some(&"a"), "wraps past the end");
        menu.move_by(-1);
        assert_eq!(menu.chosen(), Some(&"b"), "wraps past the start");
        menu.move_by(-2);
        assert_eq!(menu.chosen(), Some(&"b"));
        menu.move_by(0);
        assert_eq!(menu.chosen(), Some(&"b"));
    }

    #[test]
    fn picking_a_disabled_item_or_a_separator_does_nothing() {
        let menu = sample();
        assert_eq!(menu.pick(0), None);
        assert_eq!(menu.pick(2), None);
        assert_eq!(menu.pick(4), None);
        assert_eq!(menu.pick(99), None);
        assert_eq!(menu.pick(3), Some(&"b"));
    }

    #[test]
    fn an_empty_menu_or_one_with_nothing_enabled_is_safe() {
        let mut empty: Menu<&'static str> = Menu::new(vec![]);
        empty.move_by(1);
        empty.move_by(-3);
        assert_eq!(empty.cursor(), None);
        assert_eq!(empty.chosen(), None);
        let mut off = Menu::new(vec![entry("x", false), MenuEntry::Separator]);
        off.move_by(1);
        assert_eq!(off.cursor(), None);
        assert_eq!(off.chosen(), None);
    }

    #[test]
    fn rf_pan_20_the_pane_menu_offers_relaunch_terminate_and_close() {
        let menu = pane_menu(&pane(), false, false, &no_keys);
        let labels: Vec<String> = items(&menu).into_iter().map(|(l, _)| l).collect();
        assert_eq!(
            labels,
            [
                "Split right",
                "Split down",
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
                "Split down",
                "New tab",
                "Close tab",
                "Close pane",
            ]
        );
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
}
