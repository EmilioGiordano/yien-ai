//! The settings file for the window (RF-CFG-01): written with commented
//! defaults on first run, read before the window opens, read again on a
//! watcher thread whenever it changes, and opened on request.

use std::io;
use std::path::PathBuf;

use tokio::sync::mpsc;
use yien_config::Loaded;
use yien_platform::config_file;
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

/// The first reading, the watcher sending the later ones, and the opener.
/// Without a data dir the window runs on the defaults.
pub(crate) fn start() -> Settings {
    let (send, updates) = mpsc::unbounded_channel();
    let path = yien_platform::paths::config_file()
        .inspect_err(|e| tracing::warn!("no data dir, the settings file is off: {e}"))
        .ok();
    let Some(file) = path.clone() else {
        return Settings {
            initial: ConfigUpdate::Loaded(Loaded::default()),
            updates,
            host: Box::new(Host { path }),
        };
    };
    match config_file::create_if_absent(&file, &yien_config::default_file(&yien_ui::commands())) {
        Ok(true) => tracing::info!("wrote the default settings file"),
        Ok(false) => {}
        Err(error) => tracing::warn!("could not write the default settings file: {error}"),
    }
    // The stamp before the text: a save in between is seen as a change.
    let stamp = config_file::stamp(&file);
    let initial = reading(config_file::read(&file));
    let watched = config_file::watch(file, stamp, move |text| send.send(reading(text)).is_ok());
    if let Err(error) = watched {
        tracing::warn!("the settings file is not watched, changes need a restart: {error}");
    }
    Settings {
        initial,
        updates,
        host: Box::new(Host { path }),
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
