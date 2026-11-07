//! Window actions and their default keys (RNF-16).

use gpui_kit::{App, KeyBinding, actions};

/// Key context of the window's root, where these bindings apply.
pub(crate) const CONTEXT: &str = "YienShell";

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
    ]
);

/// The keys, with the label the status bar shows for each.
pub(crate) const TAB_BAR_KEYS: &str = "Ctrl+Shift+E";
pub(crate) const CONTEXT_BAR_KEYS: &str = "Ctrl+Shift+D";
pub(crate) const THEME_KEYS: &str = "Ctrl+Shift+L";
pub(crate) const NEXT_TAB_KEYS: &str = "Ctrl+Tab";

pub(crate) fn bind_keys(cx: &mut App) {
    let context = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("ctrl-shift-e", ToggleTabBar, context),
        KeyBinding::new("ctrl-shift-d", ToggleContextBar, context),
        KeyBinding::new("ctrl-shift-l", ToggleTheme, context),
        KeyBinding::new("ctrl-tab", NextTab, context),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, context),
    ]);
}
