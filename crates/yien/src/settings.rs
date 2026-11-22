//! The settings file for the window (RF-CFG-01): written with commented
//! defaults on first run, read before the window opens, read again on a
//! watcher thread whenever it changes, opened on request, and changed one
//! key at a time by the settings screen (RF-CFG-05).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::mpsc;
use yien_config::{EditError, Loaded, SettingValue, ThemeSetting};
use yien_platform::config_file::{self, Watcher};
use yien_proto::TerminalPalette;
use yien_ui::{ConfigUpdate, Settings, SettingsHost};

use crate::shell_switch::{ShellSwitch, ShellWanted};
use crate::update::Update;

/// One key the settings screen sets, and who hears how it went.
type SetRequest = (
    &'static str,
    SettingValue,
    Box<dyn FnOnce(Option<String>) + Send>,
);

/// Opens and writes the file through the platform layer.
struct Host {
    path: Option<PathBuf>,
    /// The writer thread's queue; `None` without a data dir or a thread.
    writes: Option<std::sync::mpsc::Sender<SetRequest>>,
}

impl Host {
    fn new(path: Option<PathBuf>) -> Self {
        let writes = path.clone().and_then(start_writer);
        Self { path, writes }
    }
}

/// The thread that writes the settings screen's changes one at a time, in
/// the order made, so two quick clicks never interleave their read and
/// write of the file. It ends with the window's host.
fn start_writer(path: PathBuf) -> Option<std::sync::mpsc::Sender<SetRequest>> {
    let (send, requests) = std::sync::mpsc::channel::<SetRequest>();
    let spawned = std::thread::Builder::new()
        .name("yien-config-write".into())
        .spawn(move || {
            for (key, value, done) in requests {
                let outcome = set_in_file(&path, key, &value);
                if let Err(error) = &outcome {
                    tracing::warn!(key, "a setting was not written: {error}");
                }
                done(outcome.err());
            }
        });
    match spawned {
        Ok(_) => Some(send),
        Err(error) => {
            tracing::warn!("the settings screen cannot write, no thread: {error}");
            None
        }
    }
}

/// Sets `key` in the file as it is on disk now. A missing file starts from
/// the first-run text; an invalid one is left as it is.
fn set_in_file(path: &Path, key: &'static str, value: &SettingValue) -> Result<(), String> {
    let (text, existed) = match config_file::read(path) {
        Ok(Some(text)) => (text, true),
        Ok(None) => (yien_config::default_file(&yien_ui::commands()), false),
        Err(error) => return Err(format!("config.toml could not be read: {error}.")),
    };
    let new = yien_config::set_value(&text, key, value).map_err(|error| match error {
        EditError::Invalid(_) => format!("{error}."),
        other => format!("{key} was not changed: {other}."),
    })?;
    if existed && new == text {
        return Ok(());
    }
    config_file::write(path, &new)
        .map_err(|error| format!("config.toml could not be written: {error}."))?;
    tracing::info!(key, "a setting was written to the settings file");
    Ok(())
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

    fn set_setting(
        &self,
        key: &'static str,
        value: SettingValue,
        done: Box<dyn FnOnce(Option<String>) + Send>,
    ) {
        let Some(writes) = &self.writes else {
            return done(Some(
                "YIEN has no data folder for its settings file.".into(),
            ));
        };
        if let Err(std::sync::mpsc::SendError((_, _, done))) = writes.send((key, value, done)) {
            done(Some("The settings file can no longer be written.".into()));
        }
    }
}

/// The first reading, the watcher sending the later ones, and the opener,
/// with the watcher to stop on quit, and the switch that hands every later
/// `terminal.shell` and `terminal.shell_integration` to the core. Without a
/// data dir the window runs on the defaults. `profile_problems` go to the
/// status bar with the first reading. Every valid reading also reaches
/// `update`, for `update.check`.
pub(crate) fn start(
    profile_problems: Vec<String>,
    update: Arc<Update>,
) -> (Settings, Option<Watcher>, Arc<ShellSwitch>) {
    let (send, updates) = mpsc::unbounded_channel();
    let path = yien_platform::paths::config_file()
        .inspect_err(|e| tracing::warn!("no data dir, the settings file is off: {e}"))
        .ok();
    let Some(file) = path.clone() else {
        let initial = ConfigUpdate::Loaded(Loaded::default());
        update.reading(&initial);
        let settings = Settings {
            initial,
            updates,
            host: Box::new(Host::new(path)),
            profile_problems,
        };
        let switch = Arc::new(ShellSwitch::new(shell_wanted(&settings.initial)));
        return (settings, None, switch);
    };
    match config_file::create_if_absent(&file, &yien_config::default_file(&yien_ui::commands())) {
        Ok(true) => tracing::info!("wrote the default settings file"),
        Ok(false) => {}
        Err(error) => tracing::warn!("could not write the default settings file: {error}"),
    }
    // The stamp before the text: a save in between is seen as a change.
    let stamp = config_file::stamp(&file);
    let initial = reading(config_file::read(&file));
    update.reading(&initial);
    let switch = Arc::new(ShellSwitch::new(shell_wanted(&initial)));
    let to_core = Arc::clone(&switch);
    let watcher = config_file::watch(file, stamp, move |text| {
        let reading = reading(text);
        to_core.reading(&reading);
        update.reading(&reading);
        send.send(reading).is_ok()
    })
    .inspect_err(|error| {
        tracing::warn!("the settings file is not watched, changes need a restart: {error}");
    })
    .ok();
    let settings = Settings {
        initial,
        updates,
        host: Box::new(Host::new(path)),
        profile_problems,
    };
    (settings, watcher, switch)
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

/// The shell panes start and whether shells report their folder, from the
/// first reading: a file that does not read keeps the defaults, YIEN's own
/// pick with the report on. Later readings go through [`ShellSwitch`].
pub(crate) fn shell_wanted(initial: &ConfigUpdate) -> ShellWanted {
    match initial {
        ConfigUpdate::Loaded(loaded) => ShellWanted::of(&loaded.config),
        ConfigUpdate::Invalid(_) | ConfigUpdate::Unreadable(_) => {
            ShellWanted::of(&yien_config::Config::default())
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
    use yien_proto::ShellKind;

    use super::*;

    fn palette_for(text: &str) -> Option<TerminalPalette> {
        start_palette(&reading(Ok(Some(text.to_owned()))))
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("yien-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn rf_cfg_05_a_change_keeps_the_rest_of_the_file() {
        let dir = scratch("keep");
        let file = dir.join("config.toml");
        let before = "# Bigger, please.\nui.font.size = 13\n\n[keys]\n\"pane.close\" = []\n";
        std::fs::write(&file, before).unwrap();
        set_in_file(&file, "ui.font.size", &SettingValue::Number(17.0)).unwrap();
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            before.replace("= 13", "= 17")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rf_cfg_05_an_invalid_file_is_never_written_over() {
        let dir = scratch("invalid");
        let file = dir.join("config.toml");
        let bad = "theme = \"washi\"\nui.font.size = = 3\n";
        std::fs::write(&file, bad).unwrap();
        let error = set_in_file(&file, "theme", &SettingValue::Text("sumi".into())).unwrap_err();
        assert!(
            error.starts_with("config.toml is invalid at line 2"),
            "{error}"
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), bad);
        std::fs::write(&file, "").unwrap();
        let refused = set_in_file(&file, "theme", &SettingValue::Text("blue".into()));
        assert!(refused.unwrap_err().starts_with("theme was not changed"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rf_cfg_05_a_removed_file_comes_back_with_its_comments() {
        let dir = scratch("removed");
        let file = dir.join("config.toml");
        set_in_file(&file, "update.check", &SettingValue::Bool(false)).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.starts_with("# YIEN settings."), "{text}");
        assert!(text.contains("\nupdate.check = false\n"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rf_cfg_05_writes_reach_the_file_in_order() {
        let dir = scratch("order");
        let file = dir.join("config.toml");
        std::fs::write(&file, "").unwrap();
        let host = Host::new(Some(file.clone()));
        let (tx, rx) = std::sync::mpsc::channel();
        for size in [14.0, 15.0, 16.0] {
            let tx = tx.clone();
            host.set_setting(
                "ui.font.size",
                SettingValue::Number(size),
                Box::new(move |error| {
                    let _ = tx.send(error);
                }),
            );
        }
        for _ in 0..3 {
            let outcome = rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap();
            assert_eq!(outcome, None);
        }
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "ui.font.size = 16\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rf_cfg_05_without_a_data_folder_nothing_is_written() {
        let host = Host::new(None);
        let (tx, rx) = std::sync::mpsc::channel();
        host.set_setting(
            "theme",
            SettingValue::Text("washi".into()),
            Box::new(move |error| {
                let _ = tx.send(error);
            }),
        );
        assert!(rx.recv().unwrap().is_some());
    }

    #[test]
    fn rf_ses_01_shells_report_their_folder_unless_the_file_says_no() {
        let read = |text: &str| shell_wanted(&reading(Ok(Some(text.to_owned())))).integration;
        assert!(read(""));
        assert!(!read("terminal.shell_integration = false"));
        assert!(
            read("terminal.shell_integration = 0"),
            "invalid keeps the default"
        );
        assert!(shell_wanted(&reading(Ok(None))).integration);
    }

    #[test]
    fn rf_cfg_01_panes_start_on_the_file_shell() {
        let read = |text: &str| shell_wanted(&reading(Ok(Some(text.to_owned())))).shell;
        assert_eq!(read(""), None, "YIEN's own pick");
        assert_eq!(read("terminal.shell = \"cmd\""), Some(ShellKind::Cmd));
        assert_eq!(
            read("terminal.shell = \"git-bash\"\ntheme = 3"),
            None,
            "invalid keeps the default"
        );
        assert_eq!(shell_wanted(&reading(Ok(None))).shell, None);
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
