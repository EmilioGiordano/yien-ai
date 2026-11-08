//! The YIEN desktop app: starts the core on the system ConPTY, restores the
//! saved session (or opens a project on the start folder) and runs the window.

#![forbid(unsafe_code)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod attention;
mod panic_log;
mod settings;

use std::process::ExitCode;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::{Duration, Instant};

use yien_core::{Core, CoreConfig, CoreHandle, GitConfig, HookConfig, SendError, SessionStore};
use yien_platform::LogGuard;
use yien_platform::single_instance::{self, Claim, Instance};
use yien_platform::state_store::FileStore;
use yien_proto::frame::Frame;
use yien_proto::{ClientMsg, PaneId};
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
    // Before anything can load a DLL: portable-pty loads `conpty.dll` by bare
    // name, which would otherwise pick one up from PATH or the current dir.
    let dll_search = yien_platform::dll_search::restrict_dll_search();
    let log = start_logging();
    panic_log::install();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "yien starting");
    if let Err(error) = dll_search {
        tracing::error!("{error}");
    }
    // Before the core loads or saves anything: a second start on the same
    // data dir only asks the running YIEN for its window (RF-SES-14).
    let window = Arc::new(AtomicIsize::new(0));
    let Some(_instance) = claim_data_dir(&window) else {
        drop(log);
        return ExitCode::SUCCESS;
    };
    let hooks = hook_config();
    let settings_file = hooks.as_ref().map(HookConfig::settings_file);
    let config = CoreConfig {
        hooks,
        session: session_store(),
        start_project: yien_platform::paths::start_dir()
            .map(|dir| dir.to_string_lossy().into_owned()),
        size_wait: SIZE_WAIT,
        ui_wait: UI_WAIT,
        git: Some(GitConfig::default()),
        ..CoreConfig::default()
    };
    let (core, events) = match Core::start(Arc::new(PortablePty::new()), config) {
        Ok(started) => started,
        Err(error) => {
            tracing::error!("could not start the core: {error}");
            eprintln!("yien: {error}");
            return ExitCode::FAILURE;
        }
    };
    // Before the window opens, so its taskbar button carries the toasts' app id.
    let attention = Rc::new(attention::Attention::new(window));
    let (settings, settings_watcher) = settings::start();
    let quitting = core.clone();
    // gpui may exit the process from inside `run` (Windows end session),
    // so the guard is flushed in the quit callback, not after `run`.
    yien_ui::run(
        Box::new(Client(core)),
        events,
        attention,
        settings,
        move || {
            tracing::info!("yien exiting");
            if let Some(watcher) = &settings_watcher {
                watcher.stop();
            }
            if let Err(error) = quitting.save_session(QUIT_SAVE_TIMEOUT) {
                tracing::warn!("the session was not saved on quit: {error}");
            }
            let started = Instant::now();
            match quitting.shutdown(QUIT_SHUTDOWN_TIMEOUT) {
                Ok(closed) => tracing::info!(
                    closed,
                    elapsed = ?started.elapsed(),
                    "panes closed on quit"
                ),
                Err(error) => tracing::warn!(
                    elapsed = ?started.elapsed(),
                    "panes not all closed on quit: {error}"
                ),
            }
            if let Some(file) = &settings_file {
                let _ = std::fs::remove_file(file);
            }
            drop(log);
        },
    );
    ExitCode::SUCCESS
}

/// Claims YIEN's data dir for this process. `None` when another YIEN holds
/// it: that one was asked to show its window, and this one must exit. A
/// failed check is logged and the app starts anyway.
fn claim_data_dir(window: &Arc<AtomicIsize>) -> Option<Option<Instance>> {
    let claimed = yien_platform::paths::data_dir()
        .map_err(|e| e.to_string())
        .and_then(|dir| single_instance::claim(&dir).map_err(|e| e.to_string()));
    match claimed {
        Ok(Claim::First(instance)) => {
            let window = Arc::clone(window);
            Some(Some(instance.serve(move |request| {
                tracing::info!(
                    activate = request.activate,
                    "another start on this data dir asked for the window"
                );
                let hwnd = window.load(Ordering::Acquire);
                if hwnd != 0 {
                    single_instance::show_window(hwnd, request);
                }
            })))
        }
        Ok(Claim::Running { delivered }) => {
            tracing::info!(
                delivered,
                "YIEN already runs on this data dir; this start exits"
            );
            None
        }
        Err(error) => {
            tracing::warn!("the single instance check failed, starting anyway: {error}");
            Some(None)
        }
    }
}

/// How long a new pane waits for its drawn size before its program starts;
/// the UI sends it on the first paint, well within this.
const SIZE_WAIT: Duration = Duration::from_millis(400);

/// How long restored panes wait for the window to show and report sizes.
const UI_WAIT: Duration = Duration::from_secs(2);

/// The longest the quit path waits to save the session.
const QUIT_SAVE_TIMEOUT: Duration = Duration::from_millis(1500);

/// The longest the quit path waits for every pane's program and pseudo
/// terminal to close, after the save.
const QUIT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

/// The saved session under YIEN's data dir; `None` (nothing saved) when the
/// data dir cannot be found.
fn session_store() -> Option<SessionStore> {
    let dir = yien_platform::paths::state_dir()
        .inspect_err(|e| tracing::warn!("no data dir, the session is not saved: {e}"))
        .ok()?;
    Some(SessionStore::new(FileStore::new(dir)))
}

/// `yien-hook` sits next to this executable; the hook settings go under
/// YIEN's data dir (RNF-14). `None` when either cannot be found, which the
/// core treats as hooks being off.
fn hook_config() -> Option<HookConfig> {
    let exe = std::env::current_exe()
        .inspect_err(|e| tracing::warn!("cannot locate yien.exe, hooks are off: {e}"))
        .ok()?;
    let hook_exe = exe
        .parent()?
        .join(format!("yien-hook{}", std::env::consts::EXE_SUFFIX));
    let settings_dir = yien_platform::paths::data_dir()
        .inspect_err(|e| tracing::warn!("no data dir, hooks are off: {e}"))
        .ok()?
        .join("hooks");
    Some(HookConfig {
        hook_exe,
        settings_dir,
    })
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
