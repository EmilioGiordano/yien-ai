//! The window's command catalog (ADR-19, RF-NAV-02): every action the UI can
//! do, with a stable id, the title the palette shows and its default keys.
//! The keymap is built from this table, so the palette lists exactly what the
//! keys can do.

use gpui_kit::{Action, Keymap, Keystroke};
use yien_proto::{Orientation, ProfileId, ProfileInfo};

use crate::actions::*;

/// Where an action's keys apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    /// Anywhere in the window.
    Window,
    /// Only while a terminal pane has the keyboard.
    Terminal,
}

/// One command of the catalog.
pub(crate) struct CommandSpec {
    /// Stable id, `area.verb`; never reused for another meaning.
    pub(crate) id: &'static str,
    /// What the palette shows: `area: what it does`.
    pub(crate) title: &'static str,
    /// The action the command dispatches.
    pub(crate) action: fn() -> Box<dyn Action>,
    /// Default keys in gpui notation, the preferred one last. Windows reports
    /// Shift+= as "+" and Shift+- as "_", with Shift cleared.
    pub(crate) keys: &'static [&'static str],
    pub(crate) scope: Scope,
}

impl CommandSpec {
    /// The part of the title before the colon and the colon, which the
    /// palette dims.
    pub(crate) fn area_len(&self) -> usize {
        self.title.find(": ").map_or(0, |at| at + 1)
    }
}

macro_rules! command {
    ($id:literal, $title:literal, $action:expr, [$($key:literal),*]) => {
        command!($id, $title, $action, [$($key),*], Scope::Window)
    };
    ($id:literal, $title:literal, $action:expr, [$($key:literal),*], $scope:expr) => {
        CommandSpec {
            id: $id,
            title: $title,
            action: || Box::new($action),
            keys: &[$($key),*],
            scope: $scope,
        }
    };
}

/// Every command, in the order the palette lists them before a query.
pub(crate) const CATALOG: &[CommandSpec] = &[
    command!(
        "palette.open",
        "workspace: execute a command",
        ToggleCommandPalette,
        ["ctrl-shift-p"]
    ),
    command!(
        "switcher.open",
        "workspace: go to tab, pane or project",
        ToggleSwitcher,
        ["ctrl-shift-o"]
    ),
    command!(
        "project.add",
        "project: add project…",
        AddProject,
        ["ctrl-shift-k"]
    ),
    command!(
        "tab_bar.toggle",
        "workspace: toggle tab bar",
        ToggleTabBar,
        ["ctrl-shift-e"]
    ),
    command!(
        "context_bar.toggle",
        "workspace: toggle context bar",
        ToggleContextBar,
        ["ctrl-shift-d"]
    ),
    command!(
        "region.next",
        "workspace: move focus to next area",
        FocusNextRegion,
        ["f6"]
    ),
    command!(
        "region.previous",
        "workspace: move focus to previous area",
        FocusPreviousRegion,
        ["shift-f6"]
    ),
    command!(
        "agent.next_needs_you",
        "agent: go to next that needs you",
        JumpToNeedsYou,
        ["ctrl-shift-j"]
    ),
    command!(
        "notifications.mute",
        "notifications: mute / unmute notifications",
        ToggleNotificationsMuted,
        []
    ),
    command!(
        "notifications.mute_tab",
        "notifications: mute / unmute notifications for this tab",
        ToggleTabMuted,
        []
    ),
    command!(
        "tab.new",
        "tab: new tab with the shell",
        NewTab,
        ["ctrl-shift-t"]
    ),
    command!(
        "tab.new_claude",
        "tab: new tab with Claude Code",
        NewClaudeTab,
        ["ctrl-shift-a"]
    ),
    command!("tab.rename", "tab: rename tab", RenameTab, []),
    command!("tab.next", "tab: next tab", NextTab, ["ctrl-tab"]),
    command!(
        "tab.previous",
        "tab: previous tab",
        PreviousTab,
        ["ctrl-shift-tab"]
    ),
    command!("tab.go_to_1", "tab: go to tab 1", GoToTab1, ["ctrl-1"]),
    command!("tab.go_to_2", "tab: go to tab 2", GoToTab2, ["ctrl-2"]),
    command!("tab.go_to_3", "tab: go to tab 3", GoToTab3, ["ctrl-3"]),
    command!("tab.go_to_4", "tab: go to tab 4", GoToTab4, ["ctrl-4"]),
    command!("tab.go_to_5", "tab: go to tab 5", GoToTab5, ["ctrl-5"]),
    command!("tab.go_to_6", "tab: go to tab 6", GoToTab6, ["ctrl-6"]),
    command!("tab.go_to_7", "tab: go to tab 7", GoToTab7, ["ctrl-7"]),
    command!("tab.go_to_8", "tab: go to tab 8", GoToTab8, ["ctrl-8"]),
    command!("tab.go_to_9", "tab: go to tab 9", GoToTab9, ["ctrl-9"]),
    command!(
        "pane.split_right",
        "pane: split right",
        SplitRight,
        ["alt-+"]
    ),
    command!("pane.split_down", "pane: split down", SplitDown, ["alt-_"]),
    command!(
        "pane.split_right_claude",
        "pane: split right with Claude Code",
        SplitRightClaude,
        ["ctrl-shift-s"]
    ),
    command!("pane.close", "pane: close", ClosePane, ["ctrl-shift-w"]),
    command!("pane.relaunch", "pane: relaunch", RelaunchPane, []),
    command!(
        "pane.maximize",
        "pane: maximize or restore",
        ToggleMaximize,
        ["ctrl-shift-enter"]
    ),
    command!(
        "pane.focus_left",
        "pane: focus left",
        FocusLeft,
        ["alt-left"]
    ),
    command!(
        "pane.focus_right",
        "pane: focus right",
        FocusRight,
        ["alt-right"]
    ),
    command!("pane.focus_up", "pane: focus up", FocusUp, ["alt-up"]),
    command!(
        "pane.focus_down",
        "pane: focus down",
        FocusDown,
        ["alt-down"]
    ),
    command!(
        "pane.grow",
        "pane: grow",
        GrowPane,
        ["alt-shift-down", "alt-shift-right"]
    ),
    command!(
        "pane.shrink",
        "pane: shrink",
        ShrinkPane,
        ["alt-shift-up", "alt-shift-left"]
    ),
    command!(
        "pane.swap_left",
        "pane: swap left",
        SwapLeft,
        ["ctrl-alt-shift-left"]
    ),
    command!(
        "pane.swap_right",
        "pane: swap right",
        SwapRight,
        ["ctrl-alt-shift-right"]
    ),
    command!(
        "pane.swap_up",
        "pane: swap up",
        SwapUp,
        ["ctrl-alt-shift-up"]
    ),
    command!(
        "pane.swap_down",
        "pane: swap down",
        SwapDown,
        ["ctrl-alt-shift-down"]
    ),
    command!(
        "terminal.copy",
        "terminal: copy selection",
        CopySelection,
        ["ctrl-shift-c"],
        Scope::Terminal
    ),
    command!(
        "terminal.paste",
        "terminal: paste",
        PasteClipboard,
        ["ctrl-shift-v"],
        Scope::Terminal
    ),
    command!(
        "project.fold",
        "project: fold / unfold",
        ToggleProjectFold,
        []
    ),
    command!("project.close", "project: close project", CloseProject, []),
    command!(
        "theme.toggle",
        "theme: toggle sumi / washi",
        ToggleTheme,
        ["ctrl-shift-l"]
    ),
];

/// A split that starts a given profile, as the split menu offers
/// (RF-PAN-18). Profiles come from the core at run time, so these commands
/// are made when the palette opens rather than listed in [`CATALOG`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProfileSplit {
    pub(crate) title: String,
    pub(crate) profile: ProfileId,
    pub(crate) orientation: Orientation,
}

/// Profile splits a keyed catalog command already runs: the shell both ways
/// (`pane.split_right`, `pane.split_down`) and Claude Code on the right
/// (`pane.split_right_claude`).
const KEYED_SPLITS: &[(&str, Orientation)] = &[
    ("shell", Orientation::Horizontal),
    ("shell", Orientation::Vertical),
    ("claude", Orientation::Horizontal),
];

/// "Split right with" and "split down with" for every profile, in the
/// profiles' order, leaving out those a keyed command covers.
pub(crate) fn profile_splits(profiles: &[ProfileInfo]) -> Vec<ProfileSplit> {
    let mut splits = Vec::new();
    for (orientation, way) in [
        (Orientation::Horizontal, "right"),
        (Orientation::Vertical, "down"),
    ] {
        for profile in profiles {
            let keyed = KEYED_SPLITS
                .iter()
                .any(|(id, o)| *id == profile.id.as_str() && *o == orientation);
            if keyed {
                continue;
            }
            splits.push(ProfileSplit {
                title: format!("pane: split {way} with {}", profile.name),
                profile: profile.id.clone(),
                orientation,
            });
        }
    }
    splits
}

/// The catalog entry with this id.
#[cfg(test)]
pub(crate) fn spec(id: &str) -> Option<&'static CommandSpec> {
    CATALOG.iter().find(|spec| spec.id == id)
}

/// The keys an action has in `keymap` now, as the user reads them: the
/// binding that takes precedence, `None` when it has none (RF-NAV-03).
pub(crate) fn shortcut(keymap: &Keymap, action: &dyn Action) -> Option<String> {
    let binding = keymap.bindings_for_action(action).last()?;
    let keys: Vec<String> = binding
        .keystrokes()
        .iter()
        .map(|k| keystroke_label(k.inner()))
        .collect();
    Some(keys.join(" "))
}

/// The default keys of a catalog command, as the user reads them.
#[cfg(test)]
pub(crate) fn default_keys(id: &str) -> Option<String> {
    let key = spec(id)?.keys.last()?;
    let keys: Option<Vec<String>> = key
        .split_whitespace()
        .map(|k| Keystroke::parse(k).ok().map(|k| keystroke_label(&k)))
        .collect();
    keys.map(|keys| keys.join(" "))
}

/// `ctrl-shift-p` as `Ctrl+Shift+P`. Windows reports Shift+= as `+` and
/// Shift+- as `_`, which read as the keys pressed.
pub(crate) fn keystroke_label(keystroke: &Keystroke) -> String {
    let m = &keystroke.modifiers;
    let (shifted, key) = match keystroke.key.as_str() {
        "+" => (true, "="),
        "_" => (true, "-"),
        other => (false, other),
    };
    let mut parts: Vec<String> = Vec::new();
    for (on, name) in [
        (m.control, "Ctrl"),
        (m.alt, "Alt"),
        (m.shift || shifted, "Shift"),
        (m.platform, "Win"),
    ] {
        if on {
            parts.push(name.to_owned());
        }
    }
    parts.push(key_name(key));
    parts.join("+")
}

fn key_name(key: &str) -> String {
    match key {
        "escape" => "Esc".to_owned(),
        "pageup" => "PgUp".to_owned(),
        "pagedown" => "PgDn".to_owned(),
        _ => {
            let mut chars = key.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use gpui_kit::KeyBinding;

    use super::*;

    #[test]
    fn rf_nav_02_command_ids_are_unique_and_every_command_has_a_title() {
        let mut ids = HashSet::new();
        for spec in CATALOG {
            assert!(ids.insert(spec.id), "{} twice", spec.id);
            assert!(!spec.id.is_empty());
            assert!(spec.area_len() > 0, "{} has no area", spec.title);
            assert!(spec.title.len() > spec.area_len() + 1, "{}", spec.id);
        }
    }

    #[test]
    fn rf_nav_02_every_bound_action_is_a_command() {
        for binding in crate::actions::bindings() {
            let action = binding.action();
            if crate::actions::is_internal(action) {
                continue;
            }
            assert!(
                CATALOG
                    .iter()
                    .any(|spec| (spec.action)().partial_eq(action)),
                "{} is bound but not in the catalog",
                action.name()
            );
        }
    }

    #[test]
    fn rf_nav_02_every_command_is_a_distinct_action() {
        for (at, spec) in CATALOG.iter().enumerate() {
            for other in &CATALOG[at + 1..] {
                assert!(
                    !(spec.action)().partial_eq((other.action)().as_ref()),
                    "{} and {} run the same action",
                    spec.id,
                    other.id
                );
            }
        }
    }

    #[test]
    fn rf_nav_03_the_shortcut_comes_from_the_keymap() {
        let keymap = Keymap::new(crate::actions::bindings());
        let split = spec("pane.split_right").unwrap();
        assert_eq!(
            shortcut(&keymap, (split.action)().as_ref()).as_deref(),
            Some("Alt+Shift+=")
        );
        let grow = spec("pane.grow").unwrap();
        assert_eq!(
            shortcut(&keymap, (grow.action)().as_ref()).as_deref(),
            Some("Alt+Shift+Right")
        );

        // A later binding takes precedence, and the palette follows it.
        let mut rebound = Keymap::new(crate::actions::bindings());
        rebound.add_bindings([KeyBinding::new("ctrl-alt-shift-x", SplitRight, None)]);
        assert_eq!(
            shortcut(&rebound, &SplitRight).as_deref(),
            Some("Ctrl+Alt+Shift+X")
        );
        assert_eq!(shortcut(&Keymap::new(Vec::new()), &SplitRight), None);
    }

    #[test]
    fn rf_nav_03_every_command_shows_its_default_keys() {
        let keymap = Keymap::new(crate::actions::bindings());
        for spec in CATALOG {
            assert_eq!(
                shortcut(&keymap, (spec.action)().as_ref()),
                default_keys(spec.id),
                "{}",
                spec.id
            );
        }
    }

    #[test]
    fn the_status_bar_labels_match_the_catalog() {
        for (id, label) in [
            ("tab_bar.toggle", TAB_BAR_KEYS),
            ("context_bar.toggle", CONTEXT_BAR_KEYS),
            ("theme.toggle", THEME_KEYS),
            ("tab.next", NEXT_TAB_KEYS),
            ("region.next", REGION_KEYS),
            ("pane.split_right", SPLIT_RIGHT_KEYS),
            ("pane.split_down", SPLIT_DOWN_KEYS),
            ("pane.close", CLOSE_PANE_KEYS),
            ("agent.next_needs_you", JUMP_KEYS),
            ("tab.new", NEW_TAB_KEYS),
            ("tab.new_claude", NEW_CLAUDE_TAB_KEYS),
            ("pane.split_right_claude", SPLIT_CLAUDE_KEYS),
            ("palette.open", PALETTE_KEYS),
            ("switcher.open", SWITCHER_KEYS),
            ("project.add", ADD_PROJECT_KEYS),
        ] {
            assert_eq!(default_keys(id).as_deref(), Some(label), "{id}");
        }
    }

    #[test]
    fn rf_pan_18_every_profile_splits_both_ways() {
        let profiles = [
            ProfileInfo {
                id: ProfileId::new("shell"),
                name: "Shell".into(),
            },
            ProfileInfo {
                id: ProfileId::new("claude"),
                name: "Claude Code".into(),
            },
            ProfileInfo {
                id: ProfileId::new("codex"),
                name: "Codex".into(),
            },
        ];
        let titles: Vec<String> = profile_splits(&profiles)
            .into_iter()
            .map(|s| s.title)
            .collect();
        // The shell both ways and Claude Code on the right are keyed
        // commands already, so they are not repeated.
        assert_eq!(
            titles,
            [
                "pane: split right with Codex",
                "pane: split down with Claude Code",
                "pane: split down with Codex",
            ]
        );
        for id in [
            "pane.split_right",
            "pane.split_down",
            "pane.split_right_claude",
        ] {
            assert!(spec(id).is_some_and(|s| !s.keys.is_empty()), "{id}");
        }
        assert!(profile_splits(&[]).is_empty());
    }

    #[test]
    fn keystrokes_read_as_the_keys_pressed() {
        let label = |k: &str| keystroke_label(&Keystroke::parse(k).unwrap());
        assert_eq!(label("ctrl-shift-p"), "Ctrl+Shift+P");
        assert_eq!(label("alt-_"), "Alt+Shift+-");
        assert_eq!(label("ctrl-alt-shift-left"), "Ctrl+Alt+Shift+Left");
        assert_eq!(label("ctrl-shift-enter"), "Ctrl+Shift+Enter");
        assert_eq!(label("ctrl-1"), "Ctrl+1");
        assert_eq!(label("escape"), "Esc");
    }
}
