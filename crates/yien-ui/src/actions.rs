//! Window actions and their default keys (RNF-16). Each one is a command of
//! the catalog in [`crate::catalog`]; pane actions map onto `yien-proto`
//! commands (ADR-19).

use gpui_kit::{
    Action, App, DummyKeyboardMapper, KeyBinding, KeyBindingContextPredicate, NoAction, actions,
};

use crate::catalog::{CATALOG, Scope};
use crate::keys::Rebinds;

/// Key context of the window's root, where these bindings apply.
pub(crate) const CONTEXT: &str = "YienShell";

/// Key context of a focused terminal pane.
pub(crate) const TERMINAL_CONTEXT: &str = "Terminal";

/// Key context of the command palette and the switcher.
pub(crate) const PALETTE_CONTEXT: &str = "YienPalette";

/// Key context of the field that renames a tab in place.
pub(crate) const RENAME_CONTEXT: &str = "YienRename";

/// Key context of a question the window asks before acting.
pub(crate) const CONFIRM_CONTEXT: &str = "YienConfirm";

/// Key context of the prompt for a new worktree tab's branch.
pub(crate) const WORKTREE_PROMPT_CONTEXT: &str = "YienWorktreePrompt";

/// Key context of the question asked before closing a worktree tab.
pub(crate) const WORKTREE_CLOSE_CONTEXT: &str = "YienWorktreeClose";

/// Key context of the tab bar while it holds the keyboard (RF-NAV-08).
pub(crate) const TAB_BAR_CONTEXT: &str = "YienTabBar";

/// Key context of the context bar while it holds the keyboard (RF-NAV-08).
pub(crate) const CONTEXT_BAR_CONTEXT: &str = "YienContextBar";

/// Key context of the context bar's Files view (RF-CTX-01).
pub(crate) const FILE_TREE_CONTEXT: &str = "YienFileTree";

/// Key context of the attention tray (RF-ATN-13).
pub(crate) const TRAY_CONTEXT: &str = "YienTray";

/// Key context of a newer release's notes.
pub(crate) const WHATS_NEW_CONTEXT: &str = "YienWhatsNew";

actions!(
    yien,
    [
        /// Switches between the sumi and washi themes.
        ToggleTheme,
        /// Opens the settings file in the configured editor (RF-CFG-01).
        OpenConfigFile,
        /// Shows or hides the tab bar.
        ToggleTabBar,
        /// Shows or hides the context bar.
        ToggleContextBar,
        /// Widens the context bar to review changes, or back (RF-NAV-10).
        ToggleReviewMode,
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
        /// Focuses the next pane whose agent finished a turn, the one done
        /// longest first (RF-ATN-14).
        JumpToDone,
        /// Opens or closes the attention tray: every pane that needs the
        /// user, failed or finished (RF-ATN-13).
        ToggleAttentionTray,
        /// Moves the tray's cursor one row up.
        TrayUp,
        /// Moves the tray's cursor one row down.
        TrayDown,
        /// Goes to the pane under the tray's cursor.
        TrayConfirm,
        /// Closes the tray.
        TrayClose,
        /// Lists tabs in sections by agent state, or under their projects
        /// again, for this session (RF-ATN-07).
        ToggleTabGrouping,
        /// Quits YIEN, asking first while agents are working (RF-SES-13).
        Quit,
        /// Lists the selected project's past agent sessions to resume one
        /// (RF-SES-07).
        ResumePastSession,
        /// Turns every toast and taskbar flash off or back on (RF-ATN-11).
        ToggleNotificationsMuted,
        /// Turns the selected tab's toasts and taskbar flashes off or back
        /// on (RF-ATN-11).
        ToggleTabMuted,
        /// Opens a tab with the shell in the selected tab's project.
        NewTab,
        /// Opens a tab with Claude Code in the selected tab's project
        /// (RF-PAN-18).
        NewClaudeTab,
        /// Asks for a branch name and opens a tab on a new worktree of the
        /// selected tab's project (RF-ESP-05).
        NewWorktreeTab,
        /// Creates the worktree tab named in the prompt.
        WorktreePromptConfirm,
        /// Leaves the worktree prompt without creating anything.
        WorktreePromptCancel,
        /// Closes the worktree tab and keeps its worktree (RF-ESP-07).
        WorktreeKeep,
        /// Closes the worktree tab and removes its worktree, asking again
        /// first when it holds work (RN-08).
        WorktreeRemove,
        /// Confirms removing a worktree that holds work (RN-08).
        WorktreeRemoveAnyway,
        /// Leaves the tab and its worktree as they are.
        WorktreeCancel,
        /// Splits the focused pane, Claude Code on the right (RF-PAN-18).
        SplitRightClaude,
        /// Starts the focused pane's program again once it has stopped
        /// (RF-PAN-19).
        RelaunchPane,
        /// Folds or unfolds the selected tab's project in the tab bar
        /// (RF-ESP-17).
        ToggleProjectFold,
        /// Picks a folder and registers it as a project (RF-ESP-01).
        AddProject,
        /// Removes the selected tab's project from the tab bar, asking first
        /// when it would stop work. Nothing on disk is touched.
        CloseProject,
        /// Edits the selected tab's name in place (RF-ESP-16).
        RenameTab,
        /// Keeps the name typed in the tab rename field.
        RenameConfirm,
        /// Leaves the tab rename field without changing the name.
        RenameCancel,
        /// Answers yes to the question the window asks.
        ConfirmAccept,
        /// Answers no to the question the window asks.
        ConfirmCancel,
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
        /// Moves the keyboard to the next region: tab bar, panes, context
        /// bar (RF-NAV-08).
        FocusNextRegion,
        /// Moves the keyboard to the previous region (RF-NAV-08).
        FocusPreviousRegion,
        /// Moves the tab bar's cursor one row up.
        TabBarUp,
        /// Moves the tab bar's cursor one row down.
        TabBarDown,
        /// Selects the tab under the tab bar's cursor and gives the keyboard
        /// to its pane.
        TabBarSelect,
        /// Gives the keyboard back to the pane without selecting anything.
        TabBarLeave,
        /// Gives the keyboard back to the pane from the context bar.
        ContextBarLeave,
        /// Takes down the status bar's notice.
        DismissNotice,
        /// Shows the selected tab's files in the context bar and gives the
        /// keyboard to their filter (RF-CTX-01).
        ShowFiles,
        /// Shows the selected tab's changed files in the context bar
        /// (RF-CTX-04).
        ShowChanges,
        /// Lists the file tree's folders again.
        RefreshFiles,
        /// Moves the file tree's cursor one row up.
        FileTreeUp,
        /// Moves the file tree's cursor one row down.
        FileTreeDown,
        /// Collapses the folder under the cursor, or goes to its parent.
        FileTreeCollapse,
        /// Expands the folder under the cursor, or goes into it.
        FileTreeExpand,
        /// Opens the file under the cursor in the editor (RF-CTX-03), or
        /// expands or collapses the folder.
        FileTreeOpen,
        /// Clears the file filter, or leaves the context bar.
        FileTreeEscape,
        /// Shows the notes of the newer YIEN the update check found.
        ShowWhatsNew,
        /// Downloads the newer YIEN, checks it, quits and installs it.
        InstallUpdate,
        /// Closes the release notes.
        WhatsNewClose,
    ]
);

/// The keys, with the label the palette, menus and aria labels show for each.
pub(crate) const TAB_BAR_KEYS: &str = "Ctrl+Shift+E";
pub(crate) const CONTEXT_BAR_KEYS: &str = "Ctrl+Shift+D";
pub(crate) const REVIEW_KEYS: &str = "Ctrl+Shift+R";
pub(crate) const THEME_KEYS: &str = "Ctrl+Shift+L";
pub(crate) const SPLIT_RIGHT_KEYS: &str = "Alt+Shift+=";
pub(crate) const SPLIT_DOWN_KEYS: &str = "Alt+Shift+-";
pub(crate) const JUMP_KEYS: &str = "Ctrl+Shift+J";
pub(crate) const JUMP_DONE_KEYS: &str = "Ctrl+Shift+U";
pub(crate) const TRAY_KEYS: &str = "Ctrl+Shift+M";
pub(crate) const SPLIT_CLAUDE_KEYS: &str = "Ctrl+Shift+S";
pub(crate) const PALETTE_KEYS: &str = "Ctrl+Shift+P";
pub(crate) const ADD_PROJECT_KEYS: &str = "Ctrl+Shift+K";

pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys(bindings());
}

pub(crate) fn bindings() -> Vec<KeyBinding> {
    bindings_with(&Rebinds::new())
}

/// The window's bindings with some commands' default keys replaced by the
/// settings file's (RF-CFG-03).
pub(crate) fn bindings_with(rebinds: &Rebinds) -> Vec<KeyBinding> {
    let terminal = Some(TERMINAL_CONTEXT);
    // The palette's keys sit beside the input's own ("Input" context) at the
    // same depth; being bound later, they win.
    let palette = format!("{PALETTE_CONTEXT} > Input");
    let palette = Some(palette.as_str());
    let rename = format!("{RENAME_CONTEXT} > Input");
    let rename = Some(rename.as_str());
    let confirm = Some(CONFIRM_CONTEXT);
    let prompt = format!("{WORKTREE_PROMPT_CONTEXT} > Input");
    let prompt = Some(prompt.as_str());
    let close = Some(WORKTREE_CLOSE_CONTEXT);
    let tab_bar = Some(TAB_BAR_CONTEXT);
    let context_bar = Some(CONTEXT_BAR_CONTEXT);
    let file_tree = format!("{FILE_TREE_CONTEXT} > Input");
    let file_tree = Some(file_tree.as_str());
    let tray = Some(TRAY_CONTEXT);
    let whats_new = Some(WHATS_NEW_CONTEXT);
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
        KeyBinding::new("enter", RenameConfirm, rename),
        KeyBinding::new("escape", RenameCancel, rename),
        KeyBinding::new("enter", ConfirmAccept, confirm),
        KeyBinding::new("escape", ConfirmCancel, confirm),
        KeyBinding::new("enter", WorktreePromptConfirm, prompt),
        KeyBinding::new("escape", WorktreePromptCancel, prompt),
        KeyBinding::new("enter", WorktreeKeep, close),
        KeyBinding::new("alt-r", WorktreeRemove, close),
        KeyBinding::new("ctrl-enter", WorktreeRemoveAnyway, close),
        KeyBinding::new("escape", WorktreeCancel, close),
        KeyBinding::new("up", TabBarUp, tab_bar),
        KeyBinding::new("down", TabBarDown, tab_bar),
        KeyBinding::new("enter", TabBarSelect, tab_bar),
        KeyBinding::new("escape", TabBarLeave, tab_bar),
        KeyBinding::new("escape", ContextBarLeave, context_bar),
        KeyBinding::new("escape", DismissNotice, Some("YienShell && !Terminal")),
        KeyBinding::new("up", FileTreeUp, file_tree),
        KeyBinding::new("down", FileTreeDown, file_tree),
        KeyBinding::new("left", FileTreeCollapse, file_tree),
        KeyBinding::new("right", FileTreeExpand, file_tree),
        KeyBinding::new("enter", FileTreeOpen, file_tree),
        KeyBinding::new("escape", FileTreeEscape, file_tree),
        KeyBinding::new("up", TrayUp, tray),
        KeyBinding::new("down", TrayDown, tray),
        KeyBinding::new("enter", TrayConfirm, tray),
        KeyBinding::new("escape", TrayClose, tray),
        KeyBinding::new("escape", WhatsNewClose, whats_new),
    ];
    for spec in CATALOG {
        let context = match spec.scope {
            Scope::Window => CONTEXT,
            Scope::Terminal => TERMINAL_CONTEXT,
        };
        let keys: Vec<&str> = match rebinds.get(spec.id) {
            Some(chords) => chords.iter().map(String::as_str).collect(),
            None => spec.keys.to_vec(),
        };
        for keys in keys {
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
        Box::new(RenameConfirm),
        Box::new(RenameCancel),
        Box::new(ConfirmAccept),
        Box::new(ConfirmCancel),
        Box::new(WorktreePromptConfirm),
        Box::new(WorktreePromptCancel),
        Box::new(WorktreeKeep),
        Box::new(WorktreeRemove),
        Box::new(WorktreeRemoveAnyway),
        Box::new(WorktreeCancel),
        Box::new(TabBarUp),
        Box::new(TabBarDown),
        Box::new(TabBarSelect),
        Box::new(TabBarLeave),
        Box::new(ContextBarLeave),
        Box::new(DismissNotice),
        Box::new(FileTreeUp),
        Box::new(FileTreeDown),
        Box::new(FileTreeCollapse),
        Box::new(FileTreeExpand),
        Box::new(FileTreeOpen),
        Box::new(FileTreeEscape),
        Box::new(TrayUp),
        Box::new(TrayDown),
        Box::new(TrayConfirm),
        Box::new(TrayClose),
        Box::new(WhatsNewClose),
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
        for keys in ["up", "down", "left", "right", "enter", "escape"] {
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
    fn rf_pan_09_copy_and_paste_keys_run_in_a_focused_terminal_only() {
        assert!(first_is("ctrl-shift-c", &TERMINAL_STACK, &CopySelection));
        assert!(first_is("ctrl-shift-v", &TERMINAL_STACK, &PasteClipboard));
        assert_eq!(matches("ctrl-shift-c", &TERMINAL_STACK), 1);
        for keys in ["ctrl-shift-c", "ctrl-shift-v"] {
            assert_eq!(matches(keys, &["Root", CONTEXT]), 0, "{keys}");
            assert_eq!(matches(keys, &PALETTE_STACK), 0, "{keys}");
        }
    }

    #[test]
    fn rf_pan_09_ctrl_c_and_ctrl_v_stay_with_the_program() {
        for keys in ["ctrl-c", "ctrl-v"] {
            assert_eq!(matches(keys, &TERMINAL_STACK), 0, "{keys}");
        }
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
    fn rf_esp_16_enter_and_escape_end_a_tab_rename_not_the_input() {
        let stack = ["Root", CONTEXT, RENAME_CONTEXT, "Input"];
        assert!(first_is("enter", &stack, &RenameConfirm));
        assert!(first_is("escape", &stack, &RenameCancel));
        assert_eq!(matches("enter", &TERMINAL_STACK), 0);
    }

    #[test]
    fn rf_esp_05_the_worktree_prompt_opens_from_a_terminal_and_owns_enter_and_escape() {
        assert!(first_is("ctrl-shift-n", &TERMINAL_STACK, &NewWorktreeTab));
        let stack = ["Root", CONTEXT, WORKTREE_PROMPT_CONTEXT, "Input"];
        assert!(first_is("enter", &stack, &WorktreePromptConfirm));
        assert!(first_is("escape", &stack, &WorktreePromptCancel));
    }

    #[test]
    fn rf_esp_07_the_worktree_question_answers_by_keyboard() {
        let stack = ["Root", CONTEXT, WORKTREE_CLOSE_CONTEXT];
        assert!(first_is("enter", &stack, &WorktreeKeep));
        assert!(first_is("alt-r", &stack, &WorktreeRemove));
        assert!(first_is("ctrl-enter", &stack, &WorktreeRemoveAnyway));
        assert!(first_is("escape", &stack, &WorktreeCancel));
        // A stray letter typed into the question removes nothing.
        assert_eq!(matches("r", &stack), 0);
        for keys in ["alt-r", "ctrl-enter"] {
            assert_eq!(matches(keys, &TERMINAL_STACK), 0, "{keys}");
        }
    }

    #[test]
    fn enter_and_escape_answer_a_question() {
        let stack = ["Root", CONTEXT, CONFIRM_CONTEXT];
        assert!(first_is("enter", &stack, &ConfirmAccept));
        assert!(first_is("escape", &stack, &ConfirmCancel));
    }

    #[test]
    fn rf_nav_08_f6_moves_between_regions_from_anywhere() {
        let tab_bar = ["Root", CONTEXT, TAB_BAR_CONTEXT];
        for stack in [&TERMINAL_STACK[..], &tab_bar[..], &["Root", CONTEXT][..]] {
            assert!(first_is("f6", stack, &FocusNextRegion), "{stack:?}");
            assert!(
                first_is("shift-f6", stack, &FocusPreviousRegion),
                "{stack:?}"
            );
        }
    }

    #[test]
    fn rf_nav_08_arrows_and_enter_drive_the_tab_bar_only_while_it_has_the_keyboard() {
        let tab_bar = ["Root", CONTEXT, TAB_BAR_CONTEXT];
        assert!(first_is("up", &tab_bar, &TabBarUp));
        assert!(first_is("down", &tab_bar, &TabBarDown));
        assert!(first_is("enter", &tab_bar, &TabBarSelect));
        assert!(first_is("escape", &tab_bar, &TabBarLeave));
        for keys in ["up", "down", "enter", "escape"] {
            assert_eq!(matches(keys, &TERMINAL_STACK), 0, "{keys}");
        }
    }

    #[test]
    fn rf_nav_08_escape_leaves_the_context_bar() {
        let context_bar = ["Root", CONTEXT, CONTEXT_BAR_CONTEXT];
        assert!(first_is("escape", &context_bar, &ContextBarLeave));
        assert!(first_is("f6", &context_bar, &FocusNextRegion));
    }

    #[test]
    fn rf_ctx_01_the_file_tree_keys_win_over_its_filter_and_stay_out_of_terminals() {
        let files = [
            "Root",
            CONTEXT,
            CONTEXT_BAR_CONTEXT,
            FILE_TREE_CONTEXT,
            "Input",
        ];
        for (keys, action) in [
            ("up", &FileTreeUp as &dyn Action),
            ("down", &FileTreeDown),
            ("left", &FileTreeCollapse),
            ("right", &FileTreeExpand),
            ("enter", &FileTreeOpen),
            ("escape", &FileTreeEscape),
        ] {
            assert!(first_is(keys, &files, action), "{keys}");
        }
        // Left and right fall through to the caret while the filter has text.
        assert!(dispatched("left", &files).len() >= 2);
        for keys in ["up", "down", "left", "right", "enter", "escape"] {
            assert_eq!(matches(keys, &TERMINAL_STACK), 0, "{keys}");
        }
        assert!(first_is("f6", &files, &FocusNextRegion));
    }

    #[test]
    fn escape_dismisses_the_notice_but_never_in_a_terminal_or_over_an_overlay() {
        assert!(first_is("escape", &["Root", CONTEXT], &DismissNotice));
        assert_eq!(matches("escape", &TERMINAL_STACK), 0);
        assert!(first_is("escape", &PALETTE_STACK, &PaletteClose));
    }

    #[test]
    fn rf_esp_01_add_project_has_a_key_from_a_terminal() {
        assert!(first_is("ctrl-shift-k", &TERMINAL_STACK, &AddProject));
    }

    #[test]
    fn rf_cfg_03_a_rebound_command_answers_its_new_chord_only() {
        let (rebinds, notices) = crate::keys::plan(
            &yien_config::parse(
                "[keys]
\"pane.split_right\" = \"ctrl-alt-r\"
\"pane.close\" = []
",
            )
            .unwrap()
            .config
            .keys,
        );
        assert!(notices.is_empty());
        let keymap = Keymap::new(bindings_with(&rebinds));
        let dispatched = |keys: &str| -> Vec<Box<dyn Action>> {
            let stack: Vec<KeyContext> = TERMINAL_STACK
                .iter()
                .map(|c| KeyContext::parse(c).unwrap())
                .collect();
            keymap
                .bindings_for_input(&[Keystroke::parse(keys).unwrap()], &stack)
                .0
                .iter()
                .map(|b| b.action().boxed_clone())
                .collect()
        };
        assert!(dispatched("ctrl-alt-r")[0].partial_eq(&SplitRight));
        assert!(dispatched("alt-+").is_empty(), "the default chord is free");
        assert!(
            dispatched("ctrl-shift-w").is_empty(),
            "pane.close is unbound"
        );
        assert!(dispatched("ctrl-shift-p")[0].partial_eq(&ToggleCommandPalette));
    }

    #[test]
    fn rf_ses_07_the_resume_command_takes_a_chord_from_the_settings_file() {
        let (rebinds, notices) = crate::keys::plan(
            &yien_config::parse(
                "[keys]
\"agent.resume_session\" = \"ctrl-alt-o\"
",
            )
            .unwrap()
            .config
            .keys,
        );
        assert!(notices.is_empty());
        let keymap = Keymap::new(bindings_with(&rebinds));
        let stack: Vec<KeyContext> = TERMINAL_STACK
            .iter()
            .map(|c| KeyContext::parse(c).unwrap())
            .collect();
        let found = keymap
            .bindings_for_input(&[Keystroke::parse("ctrl-alt-o").unwrap()], &stack)
            .0;
        assert!(found[0].action().partial_eq(&ResumePastSession));
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
