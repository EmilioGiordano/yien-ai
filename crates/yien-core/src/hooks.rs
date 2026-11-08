//! Claude Code hooks in the runtime (ADR-09): one hook pipe per app, the
//! `--settings` file every Claude pane points at, and the route from a
//! delivered hook to its pane's actor.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use tokio::sync::mpsc::error::TrySendError;
use yien_agent::render_claude_settings;
use yien_platform::hook_pipe::{HookDelivery, HookPipeError, HookPipeServer, HookRegistry};
use yien_proto::PaneId;

use crate::service::{HookItem, Links, links};

/// Where the hook pipe gets what it needs. The core runs without hooks
/// when [`crate::CoreConfig::hooks`] is `None`.
///
/// ```
/// use std::path::PathBuf;
/// use yien_core::HookConfig;
///
/// let hooks = HookConfig {
///     hook_exe: PathBuf::from("C:/Program Files/YIEN/yien-hook.exe"),
///     settings_dir: PathBuf::from("C:/Users/u/AppData/Local/YIEN/hooks"),
/// };
/// assert!(hooks.hook_exe.ends_with("yien-hook.exe"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookConfig {
    /// The `yien-hook` executable Claude Code runs for each hook.
    pub hook_exe: PathBuf,
    /// A directory of YIEN's own where the settings file is written; never
    /// `~/.claude` (RNF-14).
    pub settings_dir: PathBuf,
}

impl HookConfig {
    /// The settings file this process writes, one per running YIEN. The
    /// core removes it when its last handle drops; the app also removes it
    /// on quit, since the process may exit with the core still running.
    ///
    /// ```
    /// use std::path::PathBuf;
    /// use yien_core::HookConfig;
    ///
    /// let hooks = HookConfig {
    ///     hook_exe: PathBuf::from("yien-hook.exe"),
    ///     settings_dir: PathBuf::from("hooks"),
    /// };
    /// let file = hooks.settings_file();
    /// assert!(file.starts_with("hooks"));
    /// assert!(file.to_string_lossy().contains(&std::process::id().to_string()));
    /// ```
    pub fn settings_file(&self) -> PathBuf {
        self.settings_dir
            .join(format!("claude-hooks-{}.json", std::process::id()))
    }
}

/// Why the hooks could not be set up. Claude panes then launch without
/// `--settings` and show "signal lost".
#[derive(Debug, thiserror::Error)]
pub(crate) enum HooksError {
    #[error("{} was not found next to YIEN", .0.display())]
    MissingHook(PathBuf),
    #[error("the path {} has a character the hook settings refuse", .0.display())]
    UnsafePath(PathBuf),
    #[error("could not write the hook settings to {}: {source}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not open the hook pipe: {0}")]
    Pipe(#[source] HookPipeError),
}

/// The running hook pipe and the settings file. Dropping it closes the pipe
/// and removes the file.
pub(crate) struct Hooks {
    pub registry: HookRegistry,
    pub pipe: String,
    pub settings_file: String,
    settings_path: PathBuf,
    /// `None` only for tests that hand hooks to an actor directly.
    _server: Option<HookPipeServer>,
}

impl std::fmt::Debug for Hooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hooks")
            .field("pipe", &self.pipe)
            .field("settings_file", &self.settings_file)
            .finish_non_exhaustive()
    }
}

impl Drop for Hooks {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.settings_path);
    }
}

/// The path with forward slashes, as Claude Code's hook settings take it.
fn forward_slashes(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Writes the settings file and opens the pipe. Every delivery goes to its
/// pane's actor without blocking the pipe thread.
pub(crate) fn start(config: &HookConfig, panes: Links) -> Result<Hooks, HooksError> {
    start_with(config, panes, |registry, panes| {
        HookPipeServer::start(registry, move |d| route(&panes, d))
    })
}

/// [`start`] with another way to open the pipe.
fn start_with(
    config: &HookConfig,
    panes: Links,
    open: impl FnOnce(HookRegistry, Links) -> Result<HookPipeServer, HookPipeError>,
) -> Result<Hooks, HooksError> {
    if !config.hook_exe.is_file() {
        return Err(HooksError::MissingHook(config.hook_exe.clone()));
    }
    let json = render_claude_settings(&forward_slashes(&config.hook_exe))
        .map_err(|_| HooksError::UnsafePath(config.hook_exe.clone()))?;
    let settings_path = config.settings_file();
    std::fs::create_dir_all(&config.settings_dir)
        .and_then(|()| std::fs::write(&settings_path, json))
        .map_err(|source| HooksError::Write {
            path: settings_path.clone(),
            source,
        })?;
    let registry = HookRegistry::new();
    let server = match open(registry.clone(), panes) {
        Ok(server) => server,
        Err(e) => {
            let _ = std::fs::remove_file(&settings_path);
            return Err(HooksError::Pipe(e));
        }
    };
    Ok(Hooks {
        registry,
        pipe: server.name().to_owned(),
        settings_file: forward_slashes(&settings_path),
        settings_path,
        _server: Some(server),
    })
}

impl Hooks {
    /// Hooks with a registry and no pipe or settings file.
    #[cfg(test)]
    pub(crate) fn detached() -> Self {
        Self {
            registry: HookRegistry::new(),
            pipe: String::new(),
            settings_file: String::new(),
            settings_path: PathBuf::new(),
            _server: None,
        }
    }
}

/// Hands a delivery to its pane's hook queue. Never waits: a full queue
/// marks the pane's channel as lost, so the actor stops trusting its state
/// instead of keeping a red mark whose clearing event was dropped.
pub(crate) fn route(panes: &Links, delivery: HookDelivery) {
    let panes = links(panes);
    let Some(link) = panes.get(&delivery.pane) else {
        return;
    };
    let item = HookItem {
        serial: delivery.serial,
        event: delivery.event,
        question: delivery.question,
    };
    if let Err(TrySendError::Full(_)) = link.hooks.try_send(item) {
        link.hooks_lost.store(true, Ordering::Release);
    }
}

/// Forgets the pane's token, so a hook from its old program is refused.
pub(crate) fn revoke(hooks: Option<&Hooks>, pane: PaneId) {
    if let Some(hooks) = hooks {
        hooks.registry.revoke(pane);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    use tokio::sync::mpsc;
    use yien_agent::HookEvent;

    use super::*;
    use crate::service::PaneLink;

    fn delivery(pane: u64) -> HookDelivery {
        HookDelivery {
            pane: PaneId::new(pane),
            serial: 3,
            event: Ok(HookEvent::Stop),
            question: None,
            payload_len: 0,
        }
    }

    #[test]
    fn rn_02_a_hook_refused_by_a_full_queue_marks_the_channel_lost() {
        let (input, _input_rx) = mpsc::channel(1);
        let (hooks, mut hooks_rx) = mpsc::channel(1);
        let lost = Arc::new(AtomicBool::new(false));
        let link = PaneLink {
            input,
            frame: Arc::default(),
            hooks,
            hooks_lost: Arc::clone(&lost),
        };
        let panes: Links = Arc::new(Mutex::new(HashMap::from([(PaneId::new(1), link)])));
        route(&panes, delivery(1));
        assert!(!lost.load(Ordering::Acquire));
        route(&panes, delivery(1));
        assert!(lost.load(Ordering::Acquire));
        let item = hooks_rx.try_recv().unwrap();
        assert_eq!((item.serial, item.event), (3, Ok(HookEvent::Stop)));
        route(&panes, delivery(99));
    }

    #[test]
    fn a_missing_hook_executable_is_refused_before_writing_anything() {
        let dir = std::env::temp_dir().join(format!("yien-hooks-missing-{}", std::process::id()));
        let config = HookConfig {
            hook_exe: dir.join("yien-hook.exe"),
            settings_dir: dir.clone(),
        };
        let err = start(&config, Arc::default()).unwrap_err();
        assert!(matches!(err, HooksError::MissingHook(_)), "{err}");
        assert!(!dir.exists());
    }

    #[test]
    fn a_pipe_that_cannot_open_leaves_no_settings_file() {
        let dir = std::env::temp_dir().join(format!("yien-hooks-nopipe-{}", std::process::id()));
        let config = HookConfig {
            hook_exe: std::env::current_exe().unwrap(),
            settings_dir: dir.clone(),
        };
        let refused = HookPipeError::Os {
            call: "CreateNamedPipeW",
            code: 5,
        };
        let file = config.settings_file();
        let err = start_with(&config, Arc::default(), |_, _| {
            assert!(
                file.is_file(),
                "the settings are written before the pipe opens"
            );
            Err(refused.clone())
        })
        .unwrap_err();
        assert!(
            matches!(&err, HooksError::Pipe(e) if *e == refused),
            "{err}"
        );
        assert!(
            err.to_string().starts_with("could not open the hook pipe"),
            "{err}"
        );
        assert!(!file.exists());
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn hook_paths_use_forward_slashes() {
        assert_eq!(
            forward_slashes(Path::new(r"C:\Program Files\YIEN\yien-hook.exe")),
            "C:/Program Files/YIEN/yien-hook.exe"
        );
    }

    #[cfg(windows)]
    #[test]
    fn rnf_11_closing_a_pane_revokes_its_token() {
        use std::time::{Duration, Instant};

        use yien_platform::hook_pipe::HOOK_TOKEN_ENV;
        use yien_proto::{ClientMsg, Command, CoreMsg, Orientation, ProfileId};
        use yien_pty::FakePty;

        use crate::{Core, CoreConfig, ShellCommand};

        let deadline = Instant::now() + Duration::from_secs(5);
        let backend = FakePty::new();
        let config = CoreConfig {
            shell: ShellCommand::new("fake-shell"),
            hooks: Some(HookConfig {
                hook_exe: std::env::current_exe().unwrap(),
                settings_dir: std::env::temp_dir()
                    .join(format!("yien-hooks-revoke-{}", std::process::id())),
            }),
            ..CoreConfig::default()
        };
        let (core, mut events) = Core::start(Arc::new(backend.clone()), config).unwrap();
        let send = |c| core.try_send(ClientMsg::Command(c)).unwrap();
        let focused = |events: &mut mpsc::Receiver<CoreMsg>, not: Option<PaneId>| loop {
            assert!(Instant::now() < deadline, "timed out");
            match events.try_recv() {
                Ok(CoreMsg::TabUpdated(t)) if Some(t.focused) != not => return t.focused,
                Ok(_) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(1)),
            }
        };
        send(Command::NewProject {
            root: env!("CARGO_MANIFEST_DIR").into(),
            name: None,
        });
        let shell = focused(&mut events, None);
        send(Command::SplitPane {
            pane: shell,
            orientation: Orientation::Horizontal,
            profile: Some(ProfileId::new("claude")),
        });
        let claude = focused(&mut events, Some(shell));
        let token = loop {
            assert!(Instant::now() < deadline, "timed out");
            let spawned = backend.take_spawned();
            if let Some(child) = spawned.iter().find(|c| c.spec().program == "claude") {
                let (_, token) = child
                    .spec()
                    .env
                    .iter()
                    .rev()
                    .find(|(k, _)| k == HOOK_TOKEN_ENV)
                    .unwrap();
                break token.to_string_lossy().into_owned();
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        let registry = core.hooks.as_ref().unwrap().registry.clone();
        assert_eq!(registry.owner(token.as_bytes()), Some(claude));
        send(Command::ClosePane { pane: claude });
        while registry.owner(token.as_bytes()).is_some() {
            assert!(Instant::now() < deadline, "the token stayed live");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
