//! Window actions and their default keys (RNF-16). Each one is a command of
//! the catalog in [`crate::catalog`]; pane actions map onto `yien-proto`
//! commands (ADR-19).

use gpui_kit::{
    Action, App, DummyKeyboardMapper, KeyBinding, KeyBindingContextPredicate, NoAction, actions,
};

use crate::catalog::{CATALOG, Scope};

/// Key context of the window's root, where these bindings apply.
pub(crate) const CONTEXT: &str = "YienShell";

/// Key context of a focused terminal pane.
pub(crate) const TERMINAL_CONTEXT: &str = "Terminal";

/// Key context of the command palette and the switcher.
pub(crate) const PALETTE_CONTEXT: &str = "YienPalette";

actions!(
    yien,
    [
        /// Switches between the sumi and washi themes.
        ToggleTheme,
        /// Shows or hides the tab bar.
        ToggleTabBar,
        /// Shows or hides the context bar.
        ToggleContextBar,
        /// Selects the next tab in the tab bar.
        NextTab,
        /// Selects the previous tab in the tab bar.
        PreviousTab,
        /// Splits the focused pane, the new one on the right (RF-PAN-01).
        SplitRight,
        /// Splits the focused pane, the new one below (RF-PAN-01).
        SplitDown,
        /// Closes the focused pane.
        ClosePane,
        /// Moves the focus to the pane on the left (RF-PAN-03).
        FocusLeft,
        /// Moves the focus to the pane on the right (RF-PAN-03).
        FocusRight,
        /// Moves the focus to the pane above (RF-PAN-03).
        FocusUp,
        /// Moves the focus to the pane below (RF-PAN-03).
        FocusDown,
        /// Grows the focused pane within its split (RF-PAN-02).
        GrowPane,
        /// Shrinks the focused pane within its split (RF-PAN-02).
        ShrinkPane,
        /// Swaps the focused pane with the one on the left (RF-PAN-05).
        SwapLeft,
        /// Swaps the focused pane with the one on the right (RF-PAN-05).
        SwapRight,
        /// Swaps the focused pane with the one above (RF-PAN-05).
        SwapUp,
        /// Swaps the focused pane with the one below (RF-PAN-05).
        SwapDown,
        /// Maximizes the focused pane or restores the layout (RF-PAN-04).
        ToggleMaximize,
        /// Copies the terminal selection (RF-PAN-09).
        CopySelection,
        /// Pastes the clipboard into the terminal (RF-PAN-09).
        PasteClipboard,
        /// Focuses the next pane that needs the user, the one waiting
        /// longest first (RF-ATN-05).
        JumpToNeedsYou,
        /// Opens a tab with the shell in the selected tab's project.
        NewTab,
        /// Opens a tab with Claude Code in the selected tab's project
        /// (RF-PAN-18).
        NewClaudeTab,
        /// Splits the focused pane, Claude Code on the right (RF-PAN-18).
        SplitRightClaude,
        /// Starts the focused pane's program again once it has stopped
        /// (RF-PAN-19).
        RelaunchPane,
        /// Folds or unfolds the selected tab's project in the tab bar
        /// (RF-ESP-17).
        ToggleProjectFold,
        /// Opens or closes the command palette (RF-NAV-01).
        ToggleCommandPalette,
        /// Opens or closes the switcher of projects, tabs and panes (RF-NAV-04).
        ToggleSwitcher,
        /// Goes to tab 1 of the selected tab's project (RF-NAV-05).
        GoToTab1,
        /// Goes to tab 2 of the selected tab's project (RF-NAV-05).
        GoToTab2,
        /// Goes to tab 3 of the selected tab's project (RF-NAV-05).
        GoToTab3,
        /// Goes to tab 4 of the selected tab's project (RF-NAV-05).
        GoToTab4,
        /// Goes to tab 5 of the selected tab's project (RF-NAV-05).
        GoToTab5,
        /// Goes to tab 6 of the selected tab's project (RF-NAV-05).
        GoToTab6,
        /// Goes to tab 7 of the selected tab's project (RF-NAV-05).
        GoToTab7,
        /// Goes to tab 8 of the selected tab's project (RF-NAV-05).
        GoToTab8,
        /// Goes to tab 9 of the selected tab's project (RF-NAV-05).
        GoToTab9,
        /// Moves the palette's selection up.
        PaletteUp,
        /// Moves the palette's selection down.
        PaletteDown,
        /// Runs the palette's selected row.
        PaletteConfirm,
        /// Closes the palette without running anything.
        PaletteClose,
    ]
);

/// The keys, with the label the status bar shows for each.
pub(crate) const TAB_BAR_KEYS: &str = "Ctrl+Shift+E";
pub(crate) const CONTEXT_BAR_KEYS: &str = "Ctrl+Shift+D";
pub(crate) const THEME_KEYS: &str = "Ctrl+Shift+L";
pub(crate) const NEXT_TAB_KEYS: &str = "Ctrl+Tab";
pub(crate) const SPLIT_RIGHT_KEYS: &str = "Alt+Shift+=";
pub(crate) const SPLIT_DOWN_KEYS: &str = "Alt+Shift+-";
pub(crate) const CLOSE_PANE_KEYS: &str = "Ctrl+Shift+W";
pub(crate) const FOCUS_KEYS: &str = "Alt+Arrows";
pub(crate) const JUMP_KEYS: &str = "Ctrl+Shift+J";
pub(crate) const NEW_TAB_KEYS: &str = "Ctrl+Shift+T";
pub(crate) const NEW_CLAUDE_TAB_KEYS: &str = "Ctrl+Shift+A";
pub(crate) const SPLIT_CLAUDE_KEYS: &str = "Ctrl+Shift+S";
pub(crate) const PALETTE_KEYS: &str = "Ctrl+Shift+P";
pub(crate) const SWITCHER_KEYS: &str = "Ctrl+Shift+O";
pub(crate) const TAB_NUMBER_KEYS: &str = "Ctrl+1..9";

pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys(bindings());
}

pub(crate) fn bindings() -> Vec<KeyBinding> {
    let terminal = Some(TERMINAL_CONTEXT);
    // The palette's keys sit beside the input's own ("Input" context) at the
    // same depth; being bound later, they win.
    let palette = format!("{PALETTE_CONTEXT} > Input");
    let palette = Some(palette.as_str());
    let mut all = vec![
        // The window root binds Tab and Shift+Tab to move the focus; a
        // focused terminal sends them to the shell (completion) and to
        // Claude Code (Shift+Tab cycles its permission mode).
        KeyBinding::new("tab", NoAction {}, terminal),
        KeyBinding::new("shift-tab", NoAction {}, terminal),
        KeyBinding::new("up", PaletteUp, palette),
        KeyBinding::new("down", PaletteDown, palette),
        KeyBinding::new("enter", PaletteConfirm, palette),
        KeyBinding::new("escape", PaletteClose, palette),
    ];
    for spec in CATALOG {
        let context = match spec.scope {
            Scope::Window => CONTEXT,
            Scope::Terminal => TERMINAL_CONTEXT,
        };
        for keys in spec.keys {
            match bind(keys, (spec.action)(), context) {
                Some(binding) => all.push(binding),
                None => tracing::error!("command {} has unreadable keys {keys}", spec.id),
            }
        }
    }
    all
}

fn bind(keys: &str, action: Box<dyn Action>, context: &str) -> Option<KeyBinding> {
    let predicate = KeyBindingContextPredicate::parse(context).ok()?;
    KeyBinding::load(
        keys,
        action,
        Some(predicate.into()),
        false,
        None,
        &DummyKeyboardMapper,
    )
    .ok()
}

/// The palette's own navigation and the terminal pass-through: bound, but
/// not commands of their own.
#[cfg(test)]
pub(crate) fn is_internal(action: &dyn Action) -> bool {
    [
        Box::new(NoAction {}) as Box<dyn Action>,
        Box::new(PaletteUp),
        Box::new(PaletteDown),
        Box::new(PaletteConfirm),
        Box::new(PaletteClose),
    ]
    .iter()
    .any(|internal| internal.partial_eq(action))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{KeyContext, Keymap, Keystroke};

    actions!(test_root, [FocusNext, InputKey]);

    /// The keymap as the window has it: gpui-kit's root and text input
    /// bindings first, as `gpui_kit::init` binds them before ours.
    fn keymap() -> Keymap {
        let mut all = vec![
            KeyBinding::new("tab", FocusNext, Some("Root")),
            KeyBinding::new("shift-tab", FocusNext, Some("Root")),
        ];
        for keys in ["up", "down", "enter", "escape"] {
            all.push(KeyBinding::new(keys, InputKey, Some("Input")));
        }
        all.extend(bindings());
        Keymap::new(all)
    }

    /// The actions `keys` would dispatch with this context stack, the one
    /// that runs first at the front.
    fn dispatched(keys: &str, stack: &[&str]) -> Vec<Box<dyn Action>> {
        let stack: Vec<KeyContext> = stack
            .iter()
            .map(|c| KeyContext::parse(c).unwrap())
            .collect();
        let input = [Keystroke::parse(keys).unwrap()];
        keymap()
            .bindings_for_input(&input, &stack)
            .0
            .iter()
            .map(|b| b.action().boxed_clone())
            .collect()
    }

    fn matches(keys: &str, stack: &[&str]) -> usize {
        dispatched(keys, stack).len()
    }

    fn first_is(keys: &str, stack: &[&str], action: &dyn Action) -> bool {
        dispatched(keys, stack)
            .first()
            .is_some_and(|first| first.partial_eq(action))
    }

    const PALETTE_STACK: [&str; 4] = ["Root", CONTEXT, PALETTE_CONTEXT, "Input"];
    const TERMINAL_STACK: [&str; 3] = ["Root", CONTEXT, TERMINAL_CONTEXT];

    #[test]
    fn rf_nav_01_the_palette_keys_win_over_the_text_input() {
        for (keys, action) in [
            ("up", &PaletteUp as &dyn Action),
            ("down", &PaletteDown),
            ("enter", &PaletteConfirm),
            ("escape", &PaletteClose),
        ] {
            assert!(first_is(keys, &PALETTE_STACK, action), "{keys}");
        }
    }

    #[test]
    fn rf_nav_07_the_palette_keys_stay_out_of_the_terminal() {
        for keys in ["up", "down", "enter", "escape", "p", "shift-p", "ctrl-p"] {
            assert_eq!(matches(keys, &TERMINAL_STACK), 0, "{keys}");
        }
    }

    #[test]
    fn rf_nav_01_the_palette_and_the_switcher_open_from_a_terminal() {
        assert!(first_is(
            "ctrl-shift-p",
            &TERMINAL_STACK,
            &ToggleCommandPalette
        ));
        assert!(first_is("ctrl-shift-o", &TERMINAL_STACK, &ToggleSwitcher));
        // Pressed again from the palette's input, they close it.
        assert!(first_is(
            "ctrl-shift-p",
            &PALETTE_STACK,
            &ToggleCommandPalette
        ));
    }

    #[test]
    fn rf_nav_05_ctrl_and_a_digit_go_to_that_tab() {
        let wanted: [&dyn Action; 9] = [
            &GoToTab1, &GoToTab2, &GoToTab3, &GoToTab4, &GoToTab5, &GoToTab6, &GoToTab7, &GoToTab8,
            &GoToTab9,
        ];
        for (n, action) in (1..=9).zip(wanted) {
            assert!(
                first_is(&format!("ctrl-{n}"), &TERMINAL_STACK, action),
                "{n}"
            );
        }
        assert_eq!(matches("ctrl-0", &TERMINAL_STACK), 0);
    }

    #[test]
    fn every_catalog_key_is_bound() {
        let keys: usize = CATALOG.iter().map(|spec| spec.keys.len()).sum();
        let internal = bindings()
            .iter()
            .filter(|b| is_internal(b.action()))
            .count();
        assert_eq!(bindings().len(), keys + internal);
    }

    #[test]
    fn a_focused_terminal_keeps_tab_and_shift_tab() {
        for keys in ["tab", "shift-tab"] {
            assert_eq!(
                matches(keys, &["Root", CONTEXT, TERMINAL_CONTEXT]),
                0,
                "{keys}"
            );
        }
    }

    #[test]
    fn other_widgets_still_move_the_focus_with_tab() {
        for keys in ["tab", "shift-tab"] {
            assert_eq!(matches(keys, &["Root", CONTEXT]), 1, "{keys}");
        }
    }
}
