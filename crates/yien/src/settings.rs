//! The settings file for the window (RF-CFG-01): written with commented
//! defaults on first run, read before the window opens, read again on a
//! watcher thread whenever it changes, and opened on request.

use std::io;
use std::path::PathBuf;

use tokio::sync::mpsc;
use yien_config::{Loaded, ThemeSetting};
use yien_platform::config_file::{self, Watcher};
use yien_proto::TerminalPalette;
use yien_ui::{ConfigUpdate, Settings, SettingsHost};

/// Opens the file through the platform layer.
struct Host {
    path: Option<PathBuf>,
}

impl SettingsHost for Host {
    fn open_config(&self, editor: Option<String>, done: Box<dyn FnOnce(Option<String>) + Send>) {
        let Some(path) = self.path.clone() else {
            return done(Some(
                "YIEN has no data folder for its settings file.".into(),
            ));
        };
        config_file::open(path, editor, move |result| {
            if let Err(error) = &result {
                tracing::warn!("the settings file was not opened: {error}");
            }
            done(result.err().map(|e| e.to_string()));
        });
    }
}

/// The first reading, the watcher sending the later ones, and the opener,
/// with the watcher to stop on quit. Without a data dir the window runs on
/// the defaults. `profile_problems` go to the status bar with the first
/// reading.
pub(crate) fn start(profile_problems: Vec<String>) -> (Settings, Option<Watcher>) {
    let (send, updates) = mpsc::unbounded_channel();
    let path = yien_platform::paths::config_file()
        .inspect_err(|e| tracing::warn!("no data dir, the settings file is off: {e}"))
        .ok();
    let Some(file) = path.clone() else {
        let settings = Settings {
            initial: ConfigUpdate::Loaded(Loaded::default()),
            updates,
            host: Box::new(Host { path }),
            profile_problems,
        };
        return (settings, None);
    };
    match config_file::create_if_absent(&file, &yien_config::default_file(&yien_ui::commands())) {
        Ok(true) => tracing::info!("wrote the default settings file"),
        Ok(false) => {}
        Err(error) => tracing::warn!("could not write the default settings file: {error}"),
    }
    // The stamp before the text: a save in between is seen as a change.
    let stamp = config_file::stamp(&file);
    let initial = reading(config_file::read(&file));
    let watcher = config_file::watch(file, stamp, move |text| send.send(reading(text)).is_ok())
        .inspect_err(|error| {
            tracing::warn!("the settings file is not watched, changes need a restart: {error}");
        })
        .ok();
    let settings = Settings {
        initial,
        updates,
        host: Box::new(Host { path }),
        profile_problems,
    };
    (settings, watcher)
}

/// The palette panes start on, from the first reading: `None` when the
/// window decides, as for `system`, which follows the OS appearance only
/// the window reads.
pub(crate) fn start_palette(initial: &ConfigUpdate) -> Option<TerminalPalette> {
    let theme = match initial {
        ConfigUpdate::Loaded(loaded) => loaded.config.theme,
        ConfigUpdate::Invalid(_) | ConfigUpdate::Unreadable(_) => ThemeSetting::default(),
    };
    match theme {
        ThemeSetting::Sumi => Some(TerminalPalette::Sumi),
        ThemeSetting::Washi => Some(TerminalPalette::Washi),
        ThemeSetting::System => None,
    }
}

/// Whether shells report their folder, from the first reading: a file that
/// does not read keeps the default, on.
pub(crate) fn shell_integration(initial: &ConfigUpdate) -> bool {
    match initial {
        ConfigUpdate::Loaded(loaded) => loaded.config.shell_integration,
        ConfigUpdate::Invalid(_) | ConfigUpdate::Unreadable(_) => {
            yien_config::Config::default().shell_integration
        }
    }
}

/// What one read of the file means for the window. A removed file keeps
/// the last valid settings; it is not written again until the next start.
fn reading(text: io::Result<Option<String>>) -> ConfigUpdate {
    match text {
        Ok(Some(text)) => match yien_config::parse(&text) {
            Ok(loaded) => ConfigUpdate::Loaded(loaded),
            Err(problem) => ConfigUpdate::Invalid(problem),
        },
        Ok(None) => ConfigUpdate::Unreadable("the file was removed".into()),
        Err(error) => ConfigUpdate::Unreadable(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palette_for(text: &str) -> Option<TerminalPalette> {
        start_palette(&reading(Ok(Some(text.to_owned()))))
    }

    #[test]
    fn rf_ses_01_shells_report_their_folder_unless_the_file_says_no() {
        let read = |text: &str| shell_integration(&reading(Ok(Some(text.to_owned()))));
        assert!(read(""));
        assert!(!read("terminal.shell_integration = false"));
        assert!(
            read("terminal.shell_integration = 0"),
            "invalid keeps the default"
        );
        assert!(shell_integration(&reading(Ok(None))));
    }

    #[test]
    fn rf_cfg_02_panes_start_on_the_file_theme() {
        assert_eq!(
            palette_for("theme = \"washi\""),
            Some(TerminalPalette::Washi)
        );
        assert_eq!(palette_for("theme = \"sumi\""), Some(TerminalPalette::Sumi));
        assert_eq!(palette_for(""), Some(TerminalPalette::Sumi), "the default");
        assert_eq!(
            palette_for("theme = 3"),
            Some(TerminalPalette::Sumi),
            "invalid"
        );
        assert_eq!(
            palette_for("theme = \"system\""),
            None,
            "the window decides"
        );
    }
}
