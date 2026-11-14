//! The settings file as the window sees it (RF-CFG-01, RF-CFG-02): what the
//! binary hands over, the last valid settings, the session's theme toggle
//! and the notices a reading gives. The file is read and parsed off the UI
//! thread; nothing here touches the disk.

use tokio::sync::mpsc;
use yien_config::{Config, Loaded, Problem, TabGrouping, ThemeSetting};

use crate::theme::ThemeKind;

/// One reading of the settings file.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigUpdate {
    /// The file parsed: apply it.
    Loaded(Loaded),
    /// The file is invalid: keep the last valid settings.
    Invalid(Problem),
    /// The file could not be read: keep the last valid settings.
    Unreadable(String),
}

/// Opens the settings file for the user, on the platform layer.
///
/// ```
/// use yien_ui::SettingsHost;
///
/// struct Nowhere;
/// impl SettingsHost for Nowhere {
///     fn open_config(&self, _editor: Option<String>, done: Box<dyn FnOnce(Option<String>) + Send>) {
///         done(Some("no editor here".into()));
///     }
/// }
/// ```
pub trait SettingsHost {
    /// Opens the file in `editor` (a command line), or in the OS default
    /// app when `None`. Returns at once; `done` runs later, from any thread,
    /// with what went wrong, if anything.
    fn open_config(&self, editor: Option<String>, done: Box<dyn FnOnce(Option<String>) + Send>);
}

/// What the binary hands the window about settings.
pub struct Settings {
    /// The reading made before the window opened.
    pub initial: ConfigUpdate,
    /// Every later reading, sent when the file changes.
    pub updates: mpsc::UnboundedReceiver<ConfigUpdate>,
    /// Opens the file.
    pub host: Box<dyn SettingsHost>,
    /// The user's profile files that were skipped at start, each naming the
    /// file and, when it can, the line and column (RF-AGE-04).
    pub profile_problems: Vec<String>,
}

/// The status bar notice and the settings' own part of it after a reading
/// that gave `notices`. `profiles`, the start's profile notice, stays
/// beside the settings' text while the bar still shows it; a valid reading
/// takes down only the settings' part.
pub(crate) fn after_reading(
    notice: Option<&str>,
    settings_notice: Option<&str>,
    profiles: Option<&str>,
    notices: &[String],
) -> (Option<String>, Option<String>) {
    let join = |a: Option<&str>, b: Option<&str>| match (a, b) {
        (Some(a), Some(b)) => Some(format!("{a} {b}")),
        (a, b) => a.or(b).map(str::to_owned),
    };
    let profiles_up = profiles.filter(|p| notice.is_some_and(|n| n.contains(p)));
    let settings_up = settings_notice
        .is_some_and(|s| notice == Some(s) || notice == join(Some(s), profiles_up).as_deref());
    if notices.is_empty() {
        let notice = if settings_up {
            profiles_up.map(str::to_owned)
        } else {
            notice.map(str::to_owned)
        };
        (notice, None)
    } else {
        let text = notices.join(" ");
        (join(Some(&text), profiles_up), Some(text))
    }
}

/// The status bar notice for the profile files skipped at start: the first
/// problem and how many more there are.
pub(crate) fn profiles_notice(problems: &[String]) -> Option<String> {
    match problems {
        [] => None,
        [only] => Some(format!("{only}.")),
        [first, rest @ ..] => Some(format!(
            "{first} (and {} more profile problem{}).",
            rest.len(),
            if rest.len() == 1 { "" } else { "s" }
        )),
    }
}

/// What the status bar does after a reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Notice {
    /// Show this.
    Show(String),
    /// Take down an earlier settings notice, if it is still up.
    Clear,
}

/// The last valid settings and the session's theme toggle.
#[derive(Debug, Clone, Default)]
pub(crate) struct SettingsModel {
    config: Config,
    /// The theme the toggle picked, until the file is next applied.
    toggled: Option<ThemeKind>,
    /// Whether the grouping toggle picked tabs by state, until the file is
    /// next applied (RF-ATN-07).
    grouping_toggled: Option<bool>,
}

impl SettingsModel {
    pub(crate) fn config(&self) -> &Config {
        &self.config
    }

    /// Applies a reading. A valid file replaces the settings and drops the
    /// toggle, so the file's theme wins again; anything else keeps them.
    pub(crate) fn apply(&mut self, update: ConfigUpdate) -> Notice {
        match update {
            ConfigUpdate::Loaded(loaded) => {
                self.config = loaded.config;
                self.toggled = None;
                self.grouping_toggled = None;
                match loaded.warnings.as_slice() {
                    [] => Notice::Clear,
                    [only] => Notice::Show(format!("{FILE}: {only}.")),
                    [first, rest @ ..] => Notice::Show(format!(
                        "{FILE}: {first} (and {} more warning{}).",
                        rest.len(),
                        if rest.len() == 1 { "" } else { "s" }
                    )),
                }
            }
            ConfigUpdate::Invalid(problem) => {
                Notice::Show(format!("{FILE}: {problem}. The last valid settings stay."))
            }
            ConfigUpdate::Unreadable(why) => Notice::Show(format!(
                "{FILE} could not be read: {why}. The last valid settings stay."
            )),
        }
    }

    /// The palette to draw, with the OS in dark mode or not.
    pub(crate) fn theme(&self, system_dark: bool) -> ThemeKind {
        self.toggled
            .unwrap_or_else(|| file_theme(self.config.theme, system_dark))
    }

    /// The toggle: the other palette for this session only.
    pub(crate) fn toggle_theme(&mut self, system_dark: bool) -> ThemeKind {
        let next = self.theme(system_dark).toggled();
        self.toggled = Some(next);
        next
    }
}

impl SettingsModel {
    /// Whether the tab bar lists tabs by state: the toggle's choice, or the
    /// file's `ui.tabs.group` (RF-ATN-07).
    pub(crate) fn groups_by_state(&self) -> bool {
        self.grouping_toggled
            .unwrap_or(self.config.tab_grouping == TabGrouping::State)
    }

    /// The toggle: the other grouping for this session only. The file is
    /// never written; its value wins again when it is next applied.
    pub(crate) fn toggle_grouping(&mut self) -> bool {
        let next = !self.groups_by_state();
        self.grouping_toggled = Some(next);
        next
    }
}

/// What a program's clipboard write (OSC 52) does: the text for the system
/// clipboard and the notice to show, or `None` when the setting is off or
/// there is nothing to copy (RF-PAN-09). `take` always runs, so the core's
/// slot is emptied whatever the setting says.
pub(crate) fn osc52_copy(
    take: impl FnOnce() -> Option<String>,
    enabled: bool,
    pane_name: &str,
    crlf: bool,
) -> Option<(String, String)> {
    let text = take()?;
    if !enabled || text.is_empty() {
        return None;
    }
    let name = pane_name.trim();
    let name = if name.is_empty() { "pane" } else { name };
    Some((
        crate::grid::clipboard_text(&text, crlf),
        format!("Copied from {name}"),
    ))
}

const FILE: &str = yien_config::FILE_NAME;

fn file_theme(setting: ThemeSetting, system_dark: bool) -> ThemeKind {
    match setting {
        ThemeSetting::Sumi => ThemeKind::Sumi,
        ThemeSetting::Washi => ThemeKind::Washi,
        ThemeSetting::System if system_dark => ThemeKind::Sumi,
        ThemeSetting::System => ThemeKind::Washi,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded(text: &str) -> ConfigUpdate {
        ConfigUpdate::Loaded(yien_config::parse(text).unwrap())
    }

    fn invalid(text: &str) -> ConfigUpdate {
        ConfigUpdate::Invalid(yien_config::parse(text).unwrap_err())
    }

    #[test]
    fn rf_pan_09_a_program_copy_follows_the_setting() {
        let said = |text: &'static str| move || Some(text.to_owned());
        let on = osc52_copy(said("a\nb"), true, "claude", true).unwrap();
        assert_eq!(on, ("a\r\nb".to_owned(), "Copied from claude".to_owned()));
        assert_eq!(
            osc52_copy(said("x"), true, "  ", false),
            Some(("x".into(), "Copied from pane".into()))
        );
        assert_eq!(osc52_copy(said("secret"), false, "claude", true), None);
        assert_eq!(osc52_copy(said(""), true, "claude", true), None);
        assert_eq!(osc52_copy(|| None, true, "claude", true), None);
    }

    #[test]
    fn rf_pan_09_a_write_is_taken_even_with_the_setting_off() {
        let mut slot = Some("left in the slot".to_owned());
        let out = osc52_copy(|| slot.take(), false, "claude", true);
        assert_eq!(out, None);
        assert_eq!(slot, None, "the core's slot is emptied");
    }

    #[test]
    fn rf_cfg_01_osc52_copy_applies_live_and_an_invalid_file_keeps_it() {
        let mut m = SettingsModel::default();
        assert!(m.config().osc52_copy);
        m.apply(loaded("terminal.osc52_copy = false"));
        assert!(!m.config().osc52_copy);
        m.apply(invalid("terminal.osc52_copy = 3"));
        assert!(!m.config().osc52_copy);
        m.apply(loaded(""));
        assert!(m.config().osc52_copy);
    }

    #[test]
    fn rf_cfg_01_the_default_tab_folder_applies_live_and_an_invalid_file_keeps_it() {
        let mut m = SettingsModel::default();
        assert_eq!(m.config().default_tab_dir, None);
        m.apply(loaded("ui.tabs.default_dir = 'D:\\Code'"));
        assert_eq!(m.config().default_tab_dir.as_deref(), Some("D:\\Code"));
        m.apply(invalid("ui.tabs.default_dir = 1"));
        assert_eq!(m.config().default_tab_dir.as_deref(), Some("D:\\Code"));
        m.apply(loaded(""));
        assert_eq!(m.config().default_tab_dir, None);
    }

    #[test]
    fn rf_cfg_02_the_file_picks_the_palette() {
        let mut m = SettingsModel::default();
        assert_eq!(m.theme(false), ThemeKind::Sumi, "sumi by default");
        assert_eq!(m.apply(loaded("theme = \"washi\"")), Notice::Clear);
        assert_eq!(m.theme(true), ThemeKind::Washi);
    }

    #[test]
    fn rf_cfg_02_system_follows_the_os_appearance() {
        let mut m = SettingsModel::default();
        m.apply(loaded("theme = \"system\""));
        assert_eq!(m.theme(true), ThemeKind::Sumi);
        assert_eq!(m.theme(false), ThemeKind::Washi);
    }

    #[test]
    fn rf_cfg_02_the_toggle_holds_for_the_session_until_the_file_is_applied() {
        let mut m = SettingsModel::default();
        m.apply(loaded("theme = \"sumi\""));
        assert_eq!(m.toggle_theme(false), ThemeKind::Washi);
        assert_eq!(m.theme(true), ThemeKind::Washi, "the OS does not undo it");
        // An invalid save changes nothing, the toggle included.
        assert!(matches!(m.apply(invalid("theme = 3")), Notice::Show(_)));
        assert_eq!(m.theme(false), ThemeKind::Washi);
        // A valid save wins, whatever it changed.
        m.apply(loaded("theme = \"sumi\"\nui.font.size = 14"));
        assert_eq!(m.theme(false), ThemeKind::Sumi);
    }

    #[test]
    fn rf_age_04_skipped_profile_files_are_one_notice() {
        assert_eq!(profiles_notice(&[]), None);
        let one = "profiles/bad.toml, line 3, column 8: expected a value; this profile is skipped";
        assert_eq!(profiles_notice(&[one.into()]), Some(format!("{one}.")));
        assert_eq!(
            profiles_notice(&[one.into(), "b".into(), "c".into()]),
            Some(format!("{one} (and 2 more profile problems)."))
        );
        assert_eq!(
            profiles_notice(&[one.into(), "b".into()]),
            Some(format!("{one} (and 1 more profile problem)."))
        );
    }

    #[test]
    fn rf_age_04_the_profile_notice_outlives_the_settings_notice() {
        let p = Some("P.");
        let bad = ["C.".to_owned()];
        // At start: the profile notice is up, the settings add theirs.
        let (notice, settings) = after_reading(p, None, p, &bad);
        assert_eq!(notice.as_deref(), Some("C. P."));
        assert_eq!(settings.as_deref(), Some("C."));
        // A valid save takes down only the settings' part.
        let (notice, settings) = after_reading(notice.as_deref(), settings.as_deref(), p, &[]);
        assert_eq!(notice.as_deref(), Some("P."));
        assert_eq!(settings, None);
        // Once another notice replaced it, the profile notice stays down.
        let (notice, _) = after_reading(Some("Nobody is waiting for you."), None, p, &bad);
        assert_eq!(notice.as_deref(), Some("C."));
        let (notice, _) = after_reading(Some("other"), Some("C."), p, &[]);
        assert_eq!(notice.as_deref(), Some("other"));
        // Without profile problems it is the settings notice alone.
        let (notice, _) = after_reading(Some("C."), Some("C."), None, &[]);
        assert_eq!(notice, None);
    }

    #[test]
    fn rf_atn_07_the_grouping_toggle_holds_until_the_file_is_applied() {
        let mut m = SettingsModel::default();
        assert!(!m.groups_by_state(), "by project by default");
        m.apply(loaded("ui.tabs.group = \"state\""));
        assert!(m.groups_by_state());
        assert!(!m.toggle_grouping());
        assert!(!m.groups_by_state());
        m.apply(invalid("ui.tabs.group = 3"));
        assert!(!m.groups_by_state(), "an invalid save keeps the toggle");
        m.apply(loaded("ui.tabs.group = \"state\""));
        assert!(m.groups_by_state(), "a valid save wins");
        assert!(!m.toggle_grouping());
        assert!(m.toggle_grouping());
    }

    #[test]
    fn rf_cfg_02_toggling_from_system_gives_the_other_palette() {
        let mut m = SettingsModel::default();
        m.apply(loaded("theme = \"system\""));
        assert_eq!(m.toggle_theme(true), ThemeKind::Washi);
        assert_eq!(m.toggle_theme(true), ThemeKind::Sumi);
    }

    #[test]
    fn rf_cfg_01_an_invalid_file_keeps_the_last_valid_settings_and_names_the_line() {
        let mut m = SettingsModel::default();
        m.apply(loaded("terminal.font.size = 18"));
        let notice = m.apply(invalid("terminal.font.size = 18\nui.font.size = \"x\"\n"));
        assert_eq!(
            notice,
            Notice::Show(
                "config.toml: line 2, column 16: ui.font.size must be a number, not string. \
                 The last valid settings stay."
                    .into()
            )
        );
        assert_eq!(m.config().terminal_font.size, 18.0);
        let notice = m.apply(ConfigUpdate::Unreadable("access denied".into()));
        assert!(matches!(notice, Notice::Show(s) if s.contains("access denied")));
        assert_eq!(m.config().terminal_font.size, 18.0);
    }

    #[test]
    fn rf_cfg_01_warnings_show_the_first_and_count_the_rest() {
        let mut m = SettingsModel::default();
        assert_eq!(
            m.apply(loaded("a = 1\n")),
            Notice::Show("config.toml: line 1, column 1: unknown setting a, ignored.".into())
        );
        let Notice::Show(text) = m.apply(loaded("a = 1\nb = 2\nc = 3\n")) else {
            panic!("a notice");
        };
        assert!(text.ends_with("(and 2 more warnings)."), "{text}");
    }
}
