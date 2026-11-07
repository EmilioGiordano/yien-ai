//! Window actions and their default keys (RNF-16). Pane actions map onto
//! the command catalog in `yien-proto` (ADR-19).

use gpui_kit::{App, KeyBinding, actions};

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

pub(crate) fn bind_keys(cx: &mut App) {
    let context = Some(CONTEXT);
    let terminal = Some(TERMINAL_CONTEXT);
    // Windows reports Shift+= as "+" and Shift+- as "_", with Shift cleared.
    cx.bind_keys([
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
    ]);
}
