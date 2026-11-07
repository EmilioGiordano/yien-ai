//! The YIEN desktop app. For now it opens the window on sample data.

#![forbid(unsafe_code)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod demo;

use std::process::ExitCode;

use yien_platform::LogGuard;

fn main() -> ExitCode {
    let _log = start_logging();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "yien starting");
    match demo::workspace() {
        Ok(workspace) => {
            yien_ui::run(workspace);
            tracing::info!("yien exiting");
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!("the sample workspace is invalid: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Without a log file there is nowhere else to report the failure, so it goes to stderr.
fn start_logging() -> Option<LogGuard> {
    let started = yien_platform::paths::logs_dir()
        .map_err(|error| error.to_string())
        .and_then(|dir| yien_platform::init_logging(&dir).map_err(|error| error.to_string()));
    match started {
        Ok(guard) => Some(guard),
        Err(error) => {
            eprintln!("yien: logging is off: {error}");
            None
        }
    }
}
