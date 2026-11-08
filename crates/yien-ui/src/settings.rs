//! The settings file as the window sees it (RF-CFG-01, RF-CFG-02): what the
//! binary hands over, the last valid settings, the session's theme toggle
//! and the notices a reading gives. The file is read and parsed off the UI
//! thread; nothing here touches the disk.

use tokio::sync::mpsc;
use yien_config::{Config, Loaded, Problem, ThemeSetting};

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
