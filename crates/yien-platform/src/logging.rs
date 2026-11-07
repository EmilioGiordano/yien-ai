//! The log file (ADR-16, plan §5.4): daily rolling, last seven days kept,
//! filtered by `YIEN_LOG`, `info` by default. Logs stay on this machine (RNF-13).
//!
//! Never log terminal content, prompt text or full hook payloads: only event
//! kinds, sizes and times.

use std::io;
use std::path::{Path, PathBuf};

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Environment variable holding the log filter, in `tracing` `EnvFilter`
/// syntax (`debug`, `yien_core=trace,info`).
pub const LOG_FILTER_ENV: &str = "YIEN_LOG";

const FILE_PREFIX: &str = "yien";
const FILE_SUFFIX: &str = "log";
const KEEP_FILES: usize = 7;

/// Keeps the background log writer alive. Hold it for the whole run; dropping
/// it flushes what is queued and stops writing.
#[must_use = "dropping the guard stops the log writer"]
#[derive(Debug)]
pub struct LogGuard {
    _worker: WorkerGuard,
}

/// Why logging could not start.
#[derive(Debug, thiserror::Error)]
pub enum LogError {
    /// The log directory could not be created.
    #[error("could not create the log directory {}: {source}", path.display())]
    CreateDir {
        /// The directory.
        path: PathBuf,
        /// The OS error.
        source: io::Error,
    },
    /// The rolling log file could not be opened.
    #[error("could not open a log file in {}: {reason}", path.display())]
    OpenFile {
        /// The directory.
        path: PathBuf,
        /// What the file appender reported.
        reason: String,
    },
    /// A global `tracing` subscriber or `log` logger was already installed.
    #[error("a global logger is already installed")]
    AlreadyInitialized,
}

/// Starts logging to daily files `yien.<date>.log` in `dir`, creating it,
/// with the filter from `YIEN_LOG` (default `info`; a malformed value falls back
/// to `info` for the parts that do not parse).
///
/// Call once per process. Debug builds also print to stderr (plan §5.4).
///
/// ```no_run
/// let _guard = yien_platform::init_logging(std::path::Path::new("D:/profile/logs"))?;
/// tracing::info!(panes = 3, "restored");
/// # Ok::<(), yien_platform::LogError>(())
/// ```
pub fn init_logging(dir: &Path) -> Result<LogGuard, LogError> {
    let spec = std::env::var(LOG_FILTER_ENV).ok();
    init_logging_with(dir, spec.as_deref())
}

/// [`init_logging`] with an explicit filter instead of `YIEN_LOG`; `None` or
/// an empty string means `info`.
///
/// ```no_run
/// let dir = std::path::Path::new("D:/profile/logs");
/// let _guard = yien_platform::init_logging_with(dir, Some("debug"))?;
/// # Ok::<(), yien_platform::LogError>(())
/// ```
pub fn init_logging_with(dir: &Path, filter: Option<&str>) -> Result<LogGuard, LogError> {
    std::fs::create_dir_all(dir).map_err(|source| LogError::CreateDir {
        path: dir.to_path_buf(),
        source,
    })?;
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(FILE_PREFIX)
        .filename_suffix(FILE_SUFFIX)
        .max_log_files(KEEP_FILES)
        .build(dir)
        .map_err(|error| LogError::OpenFile {
            path: dir.to_path_buf(),
            reason: error.to_string(),
        })?;
    let (writer, worker) = tracing_appender::non_blocking(appender);

    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(writer)
        .with_ansi(false);
    let console_layer = cfg!(debug_assertions).then(|| {
        tracing_subscriber::fmt::layer()
            .with_writer(io::stderr)
            .with_ansi(false)
    });

    tracing_subscriber::registry()
        .with(env_filter(filter))
        .with(file_layer)
        .with(console_layer)
        .try_init()
        .map_err(|_| LogError::AlreadyInitialized)?;

    Ok(LogGuard { _worker: worker })
}

fn env_filter(spec: Option<&str>) -> EnvFilter {
    EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .parse_lossy(spec.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn max_level(spec: Option<&str>) -> Option<LevelFilter> {
        env_filter(spec).max_level_hint()
    }

    #[test]
    fn the_default_filter_is_info() {
        assert_eq!(max_level(None), Some(LevelFilter::INFO));
        assert_eq!(max_level(Some("")), Some(LevelFilter::INFO));
    }

    #[test]
    fn the_filter_follows_the_spec() {
        assert_eq!(max_level(Some("debug")), Some(LevelFilter::DEBUG));
        assert_eq!(max_level(Some("warn")), Some(LevelFilter::WARN));
    }

    #[test]
    fn a_malformed_spec_falls_back_to_info() {
        assert_eq!(max_level(Some("=garbage=")), Some(LevelFilter::INFO));
    }

    #[test]
    fn an_unwritable_dir_is_reported() {
        let file = std::env::current_exe().unwrap();
        let err = init_logging_with(&file.join("logs"), None).unwrap_err();
        assert!(matches!(err, LogError::CreateDir { .. }), "{err:?}");
    }
}
