//! Real PowerShell panes through ConPTY report their folder with the prompt
//! hook: the core follows `cd`, saves the folder and restores the pane there.

#![cfg(windows)]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers outside #[test] fns are not covered by clippy.toml's in-test allowances"
)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::error::TryRecvError;
use yien_core::{Core, CoreConfig, CoreHandle, SendError, SessionStore, ShellCommand};
use yien_proto::{ClientMsg, Command, CoreMsg, InputBytes, PaneId, same_folder};
use yien_pty::PortablePty;
use yien_session::{MemStore, SessionSnapshot, Store, StoreError, load};

/// PowerShell's first start on a cold machine can take many seconds.
const TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Default)]
struct Shared(Arc<Mutex<MemStore>>);

impl Shared {
    fn saved(&self) -> SessionSnapshot {
        load(&*self.0.lock().unwrap()).restored.unwrap().snapshot
    }
}

impl Store for Shared {
    fn read(&self, name: &str) -> Result<Option<Vec<u8>>, StoreError> {
        self.0.lock().unwrap().read(name)
    }

    fn write_atomic(&mut self, name: &str, bytes: &[u8]) -> Result<(), StoreError> {
        self.0.lock().unwrap().write_atomic(name, bytes)
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), StoreError> {
        self.0.lock().unwrap().rename(from, to)
    }
}

struct Harness {
    core: CoreHandle,
    events: Receiver<CoreMsg>,
}

impl Harness {
    fn start(shell: &ShellCommand, store: &Shared) -> Self {
        let config = CoreConfig {
            shell: shell.clone(),
            session: Some(SessionStore::new(store.clone())),
            ..CoreConfig::default()
        };
        let (core, events) = Core::start(Arc::new(PortablePty::new()), config).unwrap();
        Self { core, events }
    }

    fn wait_for<T>(&mut self, mut pick: impl FnMut(&CoreMsg) -> Option<T>) -> T {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match self.events.try_recv() {
                Ok(msg) => {
                    if let Some(found) = pick(&msg) {
                        return found;
                    }
                }
                Err(TryRecvError::Empty) => {
                    assert!(Instant::now() < deadline, "timed out waiting for an event");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(TryRecvError::Disconnected) => panic!("the core stopped"),
            }
        }
    }

    /// The next folder the pane reports other than `from`.
    fn next_cwd(&mut self, pane: PaneId, from: Option<&str>) -> String {
        self.wait_for(|m| match m {
            CoreMsg::PaneUpdated(p) if p.id == pane => p
                .cwd
                .clone()
                .filter(|cwd| from.is_none_or(|from| !same_folder(cwd, from))),
            _ => None,
        })
    }

    fn type_line(&self, pane: PaneId, line: &str) {
        let deadline = Instant::now() + TIMEOUT;
        let mut msg = ClientMsg::Input {
            pane,
            bytes: InputBytes::new(format!("{line}\r").into_bytes()),
        };
        loop {
            match self.core.try_send(msg) {
                Ok(()) => return,
                Err(SendError::Busy(back)) => msg = back,
                Err(e) => panic!("{e}"),
            }
            assert!(Instant::now() < deadline, "the pane stayed busy");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn quit(self) {
        self.core.save_session(Duration::from_secs(5)).unwrap();
        let _ = self.core.shutdown(Duration::from_secs(5));
    }
}

/// A folder whose name needs quoting: a space, an apostrophe and a letter
/// outside ASCII.
fn odd_folder(shell: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{shell} cwd 'q' é"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn on_path(program: &str) -> bool {
    let path = std::env::var_os("PATH");
    yien_which::resolve(program.as_ref(), path.as_deref(), "", Path::is_file).is_ok()
}

/// Opens a project, follows the pane through `cd`, quits and starts again
/// on the saved session.
fn follows_cd_and_restores_there(program: &str) {
    let shell = ShellCommand::new(program).reporting_cwd(None);
    let odd = odd_folder(program.trim_end_matches(".exe"));
    let odd_text = odd.to_str().unwrap().to_owned();
    let store = Shared::default();

    let mut h = Harness::start(&shell, &store);
    h.core
        .try_send(ClientMsg::Command(Command::NewProject {
            root: env!("CARGO_MANIFEST_DIR").into(),
            name: None,
        }))
        .unwrap();
    let pane = h.wait_for(|m| match m {
        CoreMsg::TabUpdated(t) => Some(t.focused),
        _ => None,
    });
    let first = h.next_cwd(pane, None);
    assert!(same_folder(&first, env!("CARGO_MANIFEST_DIR")), "{first}");

    h.type_line(pane, r"cd C:\Windows");
    let windows = h.next_cwd(pane, Some(&first));
    assert_eq!(windows, r"C:\Windows");

    // The registry is no folder: nothing is reported until the next one.
    h.type_line(pane, r"cd HKLM:\SOFTWARE");
    h.type_line(
        pane,
        &format!("cd -LiteralPath '{}'", odd_text.replace('\'', "''")),
    );
    let reported = h.next_cwd(pane, Some(&windows));
    assert!(same_folder(&reported, &odd_text), "{reported}");
    h.quit();

    let saved = store.saved();
    let record = &saved.projects[0].tabs[0].panes[0];
    assert_eq!(record.id, pane);
    assert!(same_folder(&record.cwd, &odd_text), "{}", record.cwd);

    let mut h = Harness::start(&shell, &store);
    let restored = h.next_cwd(pane, None);
    assert!(same_folder(&restored, &odd_text), "{restored}");
    h.quit();
}

#[test]
fn rf_ses_01_windows_powershell_pane_follows_cd_and_reopens_there() {
    follows_cd_and_restores_there("powershell.exe");
}

#[test]
fn rf_ses_01_pwsh_pane_follows_cd_and_reopens_there() {
    if !on_path("pwsh.exe") {
        eprintln!("pwsh.exe is not on PATH; skipped");
        return;
    }
    follows_cd_and_restores_there("pwsh.exe");
}

#[test]
fn rf_ses_01_cmd_pane_follows_cd() {
    let shell = ShellCommand::new("cmd.exe").reporting_cwd(None);
    let store = Shared::default();
    let mut h = Harness::start(&shell, &store);
    h.core
        .try_send(ClientMsg::Command(Command::NewProject {
            root: env!("CARGO_MANIFEST_DIR").into(),
            name: None,
        }))
        .unwrap();
    let pane = h.wait_for(|m| match m {
        CoreMsg::TabUpdated(t) => Some(t.focused),
        _ => None,
    });
    let first = h.next_cwd(pane, None);
    assert!(same_folder(&first, env!("CARGO_MANIFEST_DIR")), "{first}");
    h.type_line(pane, r"cd /d C:\Windows");
    assert_eq!(h.next_cwd(pane, Some(&first)), r"C:\Windows");
    h.quit();
}
