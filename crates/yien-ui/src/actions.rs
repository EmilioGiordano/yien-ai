//! Window actions and their default keys (RNF-16). Pane actions map onto
//! the command catalog in `yien-proto` (ADR-19).

use gpui_kit::{App, KeyBinding, NoAction, actions};

/// Key context of the window's root, where these bindings apply.
pub(crate) const CONTEXT: &str = "YienShell";

/// Key context of a focused terminal pane.
pub(crate) const TERMINAL_CONTEXT: &str = "Terminal";

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

pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys(bindings());
}

fn bindings() -> Vec<KeyBinding> {
    let context = Some(CONTEXT);
    let terminal = Some(TERMINAL_CONTEXT);
    // Windows reports Shift+= as "+" and Shift+- as "_", with Shift cleared.
    vec![
        // The window root binds Tab and Shift+Tab to move the focus; a
        // focused terminal sends them to the shell (completion) and to
        // Claude Code (Shift+Tab cycles its permission mode).
        KeyBinding::new("tab", NoAction {}, terminal),
        KeyBinding::new("shift-tab", NoAction {}, terminal),
        KeyBinding::new("ctrl-shift-e", ToggleTabBar, context),
        KeyBinding::new("ctrl-shift-d", ToggleContextBar, context),
        KeyBinding::new("ctrl-shift-l", ToggleTheme, context),
        KeyBinding::new("ctrl-tab", NextTab, context),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, context),
        KeyBinding::new("alt-+", SplitRight, context),
        KeyBinding::new("alt-_", SplitDown, context),
        KeyBinding::new("ctrl-shift-w", ClosePane, context),
        KeyBinding::new("alt-left", FocusLeft, context),
        KeyBinding::new("alt-right", FocusRight, context),
        KeyBinding::new("alt-up", FocusUp, context),
        KeyBinding::new("alt-down", FocusDown, context),
        KeyBinding::new("alt-shift-right", GrowPane, context),
        KeyBinding::new("alt-shift-down", GrowPane, context),
        KeyBinding::new("alt-shift-left", ShrinkPane, context),
        KeyBinding::new("alt-shift-up", ShrinkPane, context),
        KeyBinding::new("ctrl-alt-shift-left", SwapLeft, context),
        KeyBinding::new("ctrl-alt-shift-right", SwapRight, context),
        KeyBinding::new("ctrl-alt-shift-up", SwapUp, context),
        KeyBinding::new("ctrl-alt-shift-down", SwapDown, context),
        KeyBinding::new("ctrl-shift-enter", ToggleMaximize, context),
        KeyBinding::new("ctrl-shift-c", CopySelection, terminal),
        KeyBinding::new("ctrl-shift-v", PasteClipboard, terminal),
        KeyBinding::new("ctrl-shift-j", JumpToNeedsYou, context),
        KeyBinding::new("ctrl-shift-t", NewTab, context),
        KeyBinding::new("ctrl-shift-a", NewClaudeTab, context),
        KeyBinding::new("ctrl-shift-s", SplitRightClaude, context),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{KeyContext, Keymap, Keystroke};

    actions!(test_root, [FocusNext]);

    /// The keymap as the window has it: the root's focus binding first.
    fn keymap() -> Keymap {
        let mut all = vec![
            KeyBinding::new("tab", FocusNext, Some("Root")),
            KeyBinding::new("shift-tab", FocusNext, Some("Root")),
        ];
        all.extend(bindings());
        Keymap::new(all)
    }

    fn matches(keys: &str, stack: &[&str]) -> usize {
        let stack: Vec<KeyContext> = stack
            .iter()
            .map(|c| KeyContext::parse(c).unwrap())
            .collect();
        let input = [Keystroke::parse(keys).unwrap()];
        keymap().bindings_for_input(&input, &stack).0.len()
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
