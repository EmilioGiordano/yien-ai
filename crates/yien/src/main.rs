//! The YIEN desktop app: starts the core on the system ConPTY, opens a
//! project on the current folder and runs the window.

#![forbid(unsafe_code)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod panic_log;

use std::process::ExitCode;
use std::sync::Arc;

use yien_core::{Core, CoreConfig, CoreHandle, SendError};
use yien_platform::LogGuard;
use yien_proto::frame::Frame;
use yien_proto::{ClientMsg, Command, PaneId};
use yien_pty::PortablePty;
use yien_ui::{CoreClient, Refused};

/// The window's way to the core.
struct Client(CoreHandle);

impl CoreClient for Client {
    fn send(&self, msg: ClientMsg) -> Result<(), Refused> {
        self.0.try_send(msg).map_err(|e| match e {
            SendError::Busy(msg) => Refused::Busy(msg),
            SendError::Stopped(_) => Refused::Stopped,
        })
    }

    fn take_frame(&self, pane: PaneId) -> Option<Frame> {
        self.0.take_frame(pane)
    }
}

fn main() -> ExitCode {
    let log = start_logging();
    panic_log::install();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "yien starting");
    let (core, events) = match Core::start(Arc::new(PortablePty::new()), CoreConfig::default()) {
        Ok(started) => started,
        Err(error) => {
            tracing::error!("could not start the core: {error}");
            eprintln!("yien: {error}");
            return ExitCode::FAILURE;
        }
    };
    let root = std::env::current_dir()
        .map(|dir| dir.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_owned());
    let open = ClientMsg::Command(Command::NewProject { root, name: None });
    if let Err(error) = core.try_send(open) {
        tracing::error!("could not open the first project: {error}");
        eprintln!("yien: {error}");
        return ExitCode::FAILURE;
    }
    // gpui may exit the process from inside `run` (Windows end session),
    // so the guard is flushed in the quit callback, not after `run`.
    yien_ui::run(Box::new(Client(core)), events, move || {
        tracing::info!("yien exiting");
        drop(log);
    });
    ExitCode::SUCCESS
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
