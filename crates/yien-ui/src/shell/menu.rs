//! A small popup menu any part of the window can open: a pure model of
//! items, separators and a cursor, and its drawing at a window position.
//! What the items do belongs to the caller, which gets a [`MenuEvent`] for
//! every key and click and runs the action [`Menu::respond`] hands back.

use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::actions::{MENU_CONTEXT, MenuClose, MenuConfirm, MenuDown, MenuUp};
use crate::theme::Theme;

pub(crate) const MENU_WIDTH: f32 = 240.0;
/// How close to the window's edges a menu may come.
pub(crate) const EDGE_MARGIN: f32 = 8.0;
/// How long after a mouse-down outside closed a menu the click on its own
/// button counts as the same click.
const SAME_CLICK: Duration = Duration::from_millis(500);

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

/// What happened in an open menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuEvent {
    Up,
    Down,
    /// Enter: run the item under the cursor.
    Confirm,
    /// A click on the item at this index.
    Pick(usize),
    /// Escape.
    Close,
    /// A mouse-down outside the menu.
    ClickedOutside,
}

/// What the menu's owner does after an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MenuOutcome<A> {
    /// The menu stays open, maybe with its cursor moved.
    Stay,
    /// Close the menu, then run this.
    Run(A),
    /// Close the menu.
    Close,
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

impl<A: Clone> Menu<A> {
    /// Applies a key or a click. Enter or a click on something that cannot
    /// run leaves the menu open.
    pub(crate) fn respond(&mut self, event: MenuEvent) -> MenuOutcome<A> {
        let run = |action: Option<&A>| match action {
            Some(action) => MenuOutcome::Run(action.clone()),
            None => MenuOutcome::Stay,
        };
        match event {
            MenuEvent::Up => {
                self.move_by(-1);
                MenuOutcome::Stay
            }
            MenuEvent::Down => {
                self.move_by(1);
                MenuOutcome::Stay
            }
            MenuEvent::Confirm => run(self.chosen()),
            MenuEvent::Pick(at) => run(self.pick(at)),
            MenuEvent::Close | MenuEvent::ClickedOutside => MenuOutcome::Close,
        }
    }
}

/// Whether a click on the button of menu `key` is the same click whose
/// mouse-down just closed that menu from outside, so it must not open it
/// again. `closed` is the menu that closed that way last, and when.
pub(crate) fn closed_by_this_click<K: PartialEq>(
    closed: Option<&(K, Instant)>,
    key: &K,
    now: Instant,
) -> bool {
    closed.is_some_and(|(k, at)| k == key && now.saturating_duration_since(*at) < SAME_CLICK)
}

/// A menu on screen: its model, its accessible name, where it opens and the
/// keyboard it holds.
pub(crate) struct OpenMenu<A> {
    model: Menu<A>,
    name: SharedString,
    at: Point<Pixels>,
    focus: FocusHandle,
    _blur: Subscription,
}

impl<A> OpenMenu<A> {
    /// Shows `model` with its top left at `at` (window coordinates) and moves
    /// the keyboard to it. `on_blur` runs when something else takes the
    /// keyboard; the owner drops the menu then, leaving the keyboard where
    /// it went.
    pub(crate) fn open<V: 'static>(
        model: Menu<A>,
        name: impl Into<SharedString>,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<V>,
        on_blur: impl Fn(&mut V, &mut Context<V>) + 'static,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let blur = cx.on_blur(&focus, window, move |this, _, cx| on_blur(this, cx));
        Self {
            model,
            name: name.into(),
            at,
            focus,
            _blur: blur,
        }
    }

    pub(crate) fn model_mut(&mut self) -> &mut Menu<A> {
        &mut self.model
    }

    /// The menu, drawn over everything at its position and kept inside the
    /// window. Every key and click goes to `on_event`.
    pub(crate) fn render<V: 'static>(
        &self,
        t: &Theme,
        cx: &mut Context<V>,
        on_event: impl Fn(&mut V, MenuEvent, &mut Window, &mut Context<V>) + 'static,
    ) -> AnyElement {
        let on_event = Rc::new(on_event);
        let send = |event: MenuEvent| {
            let on_event = Rc::clone(&on_event);
            move |this: &mut V, window: &mut Window, cx: &mut Context<V>| {
                on_event(this, event, window, cx)
            }
        };
        let cursor = self.model.cursor();
        let hover = t.term_raised;
        let rows: Vec<AnyElement> = self
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
                MenuEntry::Item(item) => {
                    let enabled = item.enabled;
                    let selected = cursor == Some(at);
                    let pick = send(MenuEvent::Pick(at));
                    h_flex()
                        .id(("menu-item", at))
                        .role(Role::MenuItem)
                        .aria_label(item.label.clone())
                        .aria_selected(selected)
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
                                .on_click(cx.listener(move |this, _, w, cx| pick(this, w, cx)))
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
            })
            .collect();
        let up = send(MenuEvent::Up);
        let down = send(MenuEvent::Down);
        let confirm = send(MenuEvent::Confirm);
        let close = send(MenuEvent::Close);
        let outside = send(MenuEvent::ClickedOutside);
        let menu = v_flex()
            .id("menu")
            .role(Role::Menu)
            .aria_label(self.name.clone())
            .track_focus(&self.focus)
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
            .on_action(cx.listener(move |this, _: &MenuUp, w, cx| up(this, w, cx)))
            .on_action(cx.listener(move |this, _: &MenuDown, w, cx| down(this, w, cx)))
            .on_action(cx.listener(move |this, _: &MenuConfirm, w, cx| confirm(this, w, cx)))
            .on_action(cx.listener(move |this, _: &MenuClose, w, cx| close(this, w, cx)))
            .on_mouse_down_out(cx.listener(move |this, _, w, cx| outside(this, w, cx)))
            .children(rows);
        deferred(
            anchored()
                .position(self.at)
                .snap_to_window_with_margin(px(EDGE_MARGIN))
                .child(menu),
        )
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: the gpui glob there would shadow `#[test]`.
    use super::{
        Menu, MenuEntry, MenuEvent, MenuItem, MenuOutcome, SAME_CLICK, closed_by_this_click,
    };
    use std::time::{Duration, Instant};

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
        assert_eq!(empty.respond(MenuEvent::Confirm), MenuOutcome::Stay);
        let mut off = Menu::new(vec![entry("x", false), MenuEntry::Separator]);
        off.move_by(1);
        assert_eq!(off.cursor(), None);
        assert_eq!(off.respond(MenuEvent::Pick(0)), MenuOutcome::Stay);
    }

    #[test]
    fn keys_and_clicks_move_run_or_close() {
        let mut menu = sample();
        assert_eq!(menu.respond(MenuEvent::Down), MenuOutcome::Stay);
        assert_eq!(menu.respond(MenuEvent::Confirm), MenuOutcome::Run("b"));
        assert_eq!(menu.respond(MenuEvent::Up), MenuOutcome::Stay);
        assert_eq!(menu.respond(MenuEvent::Confirm), MenuOutcome::Run("a"));
        assert_eq!(menu.respond(MenuEvent::Pick(3)), MenuOutcome::Run("b"));
        assert_eq!(menu.respond(MenuEvent::Pick(4)), MenuOutcome::Stay);
        assert_eq!(menu.respond(MenuEvent::Close), MenuOutcome::Close);
        assert_eq!(menu.respond(MenuEvent::ClickedOutside), MenuOutcome::Close);
    }

    #[test]
    fn only_the_menu_a_click_just_closed_stays_closed() {
        let now = Instant::now();
        let closed = ((7, "pane"), now);
        assert!(closed_by_this_click(Some(&closed), &(7, "pane"), now));
        // Another menu of the same pane, or another pane's, opens.
        assert!(!closed_by_this_click(Some(&closed), &(7, "terminal"), now));
        assert!(!closed_by_this_click(Some(&closed), &(8, "pane"), now));
        // A later click is a new click.
        let later = now + SAME_CLICK + Duration::from_millis(1);
        assert!(!closed_by_this_click(Some(&closed), &(7, "pane"), later));
        assert!(!closed_by_this_click::<(i32, &str)>(
            None,
            &(7, "pane"),
            now
        ));
    }
}
