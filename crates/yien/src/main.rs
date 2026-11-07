//! The YIEN desktop app: starts the core on the system ConPTY, opens a
//! project on the current folder and runs the window.

#![forbid(unsafe_code)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;
use std::sync::Arc;

use yien_core::{Core, CoreConfig, CoreHandle, SendError};
use yien_proto::{ClientMsg, Command};
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
}

fn main() -> ExitCode {
    let (core, events) = match Core::start(Arc::new(PortablePty::new()), CoreConfig::default()) {
        Ok(started) => started,
        Err(error) => {
            eprintln!("yien: {error}");
            return ExitCode::FAILURE;
        }
    };
    let root = std::env::current_dir()
        .map(|dir| dir.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_owned());
    let open = ClientMsg::Command(Command::NewProject { root, name: None });
    if let Err(error) = core.try_send(open) {
        eprintln!("yien: {error}");
        return ExitCode::FAILURE;
    }
    yien_ui::run(Box::new(Client(core)), events);
    ExitCode::SUCCESS
}
