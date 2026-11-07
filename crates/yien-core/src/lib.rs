//! The YIEN core: the workspace (projects, tabs, layouts) and one actor per
//! pane that owns its PTY session, terminal engine and agent state machine.
//!
//! The UI talks to it only through `yien-proto` messages (ADR-03): it sends
//! [`ClientMsg`] through a [`CoreHandle`] and receives [`CoreMsg`]. The core
//! runs on its own tokio runtime (ADR-14), so the UI's executor never waits
//! on it.
//!
//! ```no_run
//! use std::sync::Arc;
//! use yien_core::{Core, CoreConfig};
//! use yien_proto::{ClientMsg, Command, CoreMsg};
//! use yien_pty::PortablePty;
//!
//! let (core, mut events) = Core::start(Arc::new(PortablePty::new()), CoreConfig::default())?;
//! core.try_send(ClientMsg::Command(Command::NewProject { root: "C:/Code/yien".into(), name: None }))?;
//! while let Some(event) = events.blocking_recv() {
//!     if let CoreMsg::PaneFrame { pane, frame } = event {
//!         println!("pane {}: {}", pane.get(), frame.text());
//!     }
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![forbid(unsafe_code)]

mod convert;
mod launch;
mod pane;
mod service;
mod shell;
mod workspace;

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use yien_agent::Profile;
use yien_proto::{ClientMsg, CoreMsg, PaneId, ProjectId, TabId};
use yien_pty::PtyBackend;
use yien_vt::VtConfig;

pub use shell::{ShellCommand, default_shell};

/// Commands waiting for the core; beyond this [`CoreHandle::try_send`]
/// reports [`SendError::Busy`] (plan §4.2: the UI shows the core as busy and
/// drops nothing).
pub const COMMAND_QUEUE: usize = 1024;

/// Events waiting for the UI. Frames are skipped while it is full; every
/// other event waits for room.
pub const EVENT_QUEUE: usize = 4096;

/// The longest a pane waits between two frames while its screen changes.
pub const FRAME_INTERVAL: Duration = Duration::from_millis(8);

/// A monotonic clock for agent timers. Injected so the core can run on fake
/// time where tests need it.
///
/// ```
/// use yien_core::{Clock, MonotonicClock};
///
/// let clock = MonotonicClock::new();
/// let a = clock.now();
/// assert!(clock.now() >= a);
/// ```
pub trait Clock: Send + Sync + fmt::Debug {
    /// Time elapsed since an arbitrary fixed origin.
    fn now(&self) -> Duration;
}

/// The real monotonic clock.
#[derive(Debug, Clone, Copy)]
pub struct MonotonicClock {
    origin: Instant,
}

impl MonotonicClock {
    /// A clock whose origin is now.
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for MonotonicClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for MonotonicClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
}

/// How the core is set up.
///
/// ```
/// use yien_core::{CoreConfig, ShellCommand};
///
/// let config = CoreConfig { shell: ShellCommand::new("cmd.exe"), ..CoreConfig::default() };
/// assert_eq!(config.initial_size, (80, 24));
/// ```
#[derive(Debug, Clone)]
pub struct CoreConfig {
    /// What a pane runs when no profile is asked for.
    pub shell: ShellCommand,
    /// The agent profiles panes can be split with (RF-PAN-18).
    pub profiles: Vec<Profile>,
    /// How each pane's terminal engine is set up.
    pub vt: VtConfig,
    /// The clock agent timers run on.
    pub clock: Arc<dyn Clock>,
    /// The longest a pane waits between two frames.
    pub frame_interval: Duration,
    /// Columns and rows a pane starts with, until the UI sends its size.
    pub initial_size: (u16, u16),
    /// Threads of the core's runtime.
    pub worker_threads: usize,
}

impl Default for CoreConfig {
    /// The user's shell, the built-in profiles and the real clock.
    fn default() -> Self {
        Self {
            shell: default_shell(),
            profiles: yien_agent::builtin_profiles().unwrap_or_default(),
            vt: VtConfig::default(),
            clock: Arc::new(MonotonicClock::new()),
            frame_interval: FRAME_INTERVAL,
            initial_size: (80, 24),
            worker_threads: 2,
        }
    }
}

/// Why a core operation failed. The text is meant for the user: it says what
/// happened and what to do.
///
/// ```
/// let err = yien_core::CoreError::UnknownProfile("aider".into());
/// assert!(err.to_string().contains("aider"));
/// ```
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// The core's runtime could not be started.
    #[error("could not start the YIEN core")]
    Runtime(#[source] std::io::Error),
    /// A command named a pane that does not exist.
    #[error("pane {} no longer exists", .0.get())]
    UnknownPane(PaneId),
    /// A command named a tab that does not exist.
    #[error("tab {} no longer exists", .0.get())]
    UnknownTab(TabId),
    /// A command named a project that does not exist.
    #[error("project {} no longer exists", .0.get())]
    UnknownProject(ProjectId),
    /// A split asked for a profile nobody defined.
    #[error("there is no profile named \"{0}\"; pick one from the list of profiles")]
    UnknownProfile(String),
    /// Swapping or moving panes across tabs.
    #[error("panes {} and {} are in different tabs; move panes only within a tab", .0.get(), .1.get())]
    DifferentTabs(PaneId, PaneId),
    /// A project folder that is not a folder.
    #[error("{0} is not a folder; choose an existing folder for the project")]
    NotADirectory(String),
}

/// Why a message did not reach the core. The message is handed back.
#[derive(Debug, thiserror::Error)]
pub enum SendError {
    /// The command queue is full; show the core as busy and retry.
    #[error("the YIEN core is busy")]
    Busy(ClientMsg),
    /// The core has stopped.
    #[error("the YIEN core has stopped")]
    Stopped(ClientMsg),
}

/// Starts the core.
#[derive(Debug)]
pub struct Core;

impl Core {
    /// Starts the core on its own runtime with an empty workspace. The first
    /// event is a [`CoreMsg::WorkspaceSnapshot`]. The core stops when the last
    /// [`CoreHandle`] is dropped, terminating every pane's program.
    pub fn start(
        backend: Arc<dyn PtyBackend>,
        config: CoreConfig,
    ) -> Result<(CoreHandle, mpsc::Receiver<CoreMsg>), CoreError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(config.worker_threads.max(1))
            .thread_name("yien-core")
            .enable_time()
            .build()
            .map_err(CoreError::Runtime)?;
        let (commands, command_rx) = mpsc::channel(COMMAND_QUEUE);
        let (events, event_rx) = mpsc::channel(EVENT_QUEUE);
        let ctx = service::Ctx {
            backend,
            config: Arc::new(config),
            events,
        };
        runtime.spawn(service::run(ctx, command_rx));
        let handle = CoreHandle {
            commands,
            _runtime: Arc::new(RuntimeGuard(Some(runtime))),
        };
        Ok((handle, event_rx))
    }
}

struct RuntimeGuard(Option<tokio::runtime::Runtime>);

impl Drop for RuntimeGuard {
    fn drop(&mut self) {
        if let Some(runtime) = self.0.take() {
            runtime.shutdown_background();
        }
    }
}

/// Sends messages to the core. Cheap to clone; the core stops when the last
/// clone is dropped.
#[derive(Clone)]
pub struct CoreHandle {
    commands: mpsc::Sender<ClientMsg>,
    _runtime: Arc<RuntimeGuard>,
}

impl fmt::Debug for CoreHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CoreHandle").finish_non_exhaustive()
    }
}

impl CoreHandle {
    /// Queues a message without waiting. Safe to call from any thread,
    /// including the UI's.
    pub fn try_send(&self, msg: ClientMsg) -> Result<(), SendError> {
        self.commands.try_send(msg).map_err(|e| match e {
            mpsc::error::TrySendError::Full(msg) => SendError::Busy(msg),
            mpsc::error::TrySendError::Closed(msg) => SendError::Stopped(msg),
        })
    }

    /// Queues a message, waiting for room. Works on any executor.
    pub async fn send(&self, msg: ClientMsg) -> Result<(), SendError> {
        self.commands
            .send(msg)
            .await
            .map_err(|e| SendError::Stopped(e.0))
    }
}
