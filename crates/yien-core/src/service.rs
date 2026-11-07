//! The core's command loop: applies UI messages to the workspace model,
//! carries out its effects and routes terminal traffic to the pane actors.
//!
//! Pane actors never wait on the service, and the service waits only on the
//! UI and on pane command queues that their actors always drain, so no wait
//! closes a cycle.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::mpsc;
use yien_agent::{HookError, HookEvent};
use yien_proto::{ClientMsg, Command, CoreMsg, PaneId, ProfileId, ProfileInfo};
use yien_pty::PtyBackend;

use crate::hooks::{self, Hooks};
use crate::launch::{self, LaunchPlan, SHELL_PROFILE};
use crate::pane::{self, EngineFactory, FrameSlot, PANE_COMMANDS, PaneCmd, PaneInput, PaneSetup};
use crate::report::Reports;
use crate::workspace::{Effect, Workspace};
use crate::{CoreConfig, CoreError, PANE_INPUT_QUEUE};

/// What every pane actor shares.
#[derive(Clone)]
pub(crate) struct Ctx {
    pub backend: Arc<dyn PtyBackend>,
    pub config: Arc<CoreConfig>,
    pub events: mpsc::Sender<CoreMsg>,
    pub reports: Arc<Reports>,
    pub engine: EngineFactory,
    /// The hook pipe, when it runs.
    pub hooks: Option<Arc<Hooks>>,
    /// Numbers panes as they enter "needs you", for the jump order
    /// (RF-ATN-05).
    pub attention: Arc<AtomicU64>,
}

/// Hook events waiting for one pane's actor. Beyond this the pane's channel
/// counts as lost (see [`hooks::route`]).
pub(crate) const PANE_HOOK_QUEUE: usize = 64;

/// A hook for a pane's actor.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct HookItem {
    /// The serial of the token it came with: the launch it belongs to.
    pub serial: u64,
    /// The decoded event, or the error of an authentic frame that did not
    /// decode.
    pub event: Result<HookEvent, HookError>,
}

/// How the handle and the hook pipe reach a pane without going through the
/// service: its input queue, its frame slot and its hook queue.
#[derive(Debug, Clone)]
pub(crate) struct PaneLink {
    pub input: mpsc::Sender<PaneInput>,
    pub frame: Arc<FrameSlot>,
    pub hooks: mpsc::Sender<HookItem>,
    /// Set when a hook was refused by a full queue.
    pub hooks_lost: Arc<AtomicBool>,
}

/// Every running pane's link, shared by the service and the handles.
pub(crate) type Links = Arc<Mutex<HashMap<PaneId, PaneLink>>>;

pub(crate) fn links(links: &Links) -> MutexGuard<'_, HashMap<PaneId, PaneLink>> {
    links.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A running pane actor, and what it was started with so a crashed one can
/// be started again where it left off.
struct PaneHandle {
    tx: mpsc::Sender<PaneCmd>,
    plan: LaunchPlan,
    cwd: String,
    size: (u16, u16),
    slot: Arc<FrameSlot>,
    visible: bool,
    seen: bool,
}

struct Service {
    ws: Workspace,
    ctx: Ctx,
    panes: HashMap<PaneId, PaneHandle>,
    links: Links,
}

pub(crate) async fn run(
    ctx: Ctx,
    links: Links,
    mut commands: mpsc::Receiver<ClientMsg>,
    hooks_error: Option<String>,
) {
    let reports = Arc::clone(&ctx.reports);
    let profiles = profile_list(&ctx.config);
    let mut service = Service {
        ws: Workspace::new().with_profiles(profiles),
        ctx,
        panes: HashMap::new(),
        links,
    };
    service
        .emit(CoreMsg::WorkspaceSnapshot(service.ws.snapshot()))
        .await;
    if let Some(reason) = hooks_error {
        service
            .emit(CoreMsg::Error {
                pane: None,
                message: format!(
                    "Claude Code state detection is off: {reason}. Claude panes still run,                      without the red mark."
                ),
            })
            .await;
    }
    loop {
        tokio::select! {
            msg = commands.recv() => match msg {
                Some(msg) => service.on_client(msg).await,
                None => break,
            },
            batch = reports.next() => {
                for (pane, events) in batch {
                    for event in events {
                        let fx = service.ws.on_pane_event(pane, event);
                        service.execute(fx).await;
                    }
                }
            }
        }
    }
}

impl Service {
    async fn emit(&self, msg: CoreMsg) {
        let _ = self.ctx.events.send(msg).await;
    }

    async fn error(&self, pane: Option<PaneId>, error: &CoreError) {
        self.emit(CoreMsg::Error {
            pane,
            message: error.to_string(),
        })
        .await;
    }

    async fn to_pane(&self, pane: PaneId, cmd: PaneCmd) {
        if let Some(handle) = self.panes.get(&pane) {
            let tx = handle.tx.clone();
            let _ = tx.send(cmd).await;
        }
    }

    fn start_pane(&mut self, pane: PaneId, plan: LaunchPlan, cwd: String, size: (u16, u16)) {
        let (tx, cmds) = mpsc::channel(PANE_COMMANDS);
        let (input_tx, input) = mpsc::channel(PANE_INPUT_QUEUE);
        let (hooks_tx, hooks) = mpsc::channel(PANE_HOOK_QUEUE);
        let hooks_lost = Arc::new(AtomicBool::new(false));
        let (slot, visible, seen) = self.panes.get(&pane).map_or_else(
            || (Arc::default(), false, false),
            |h| (Arc::clone(&h.slot), h.visible, h.seen),
        );
        let setup = PaneSetup {
            id: pane,
            plan: plan.clone(),
            cwd: cwd.clone(),
            size,
            slot: Arc::clone(&slot),
            visible,
            seen,
            hooks,
            hooks_lost: Arc::clone(&hooks_lost),
        };
        pane::spawn(setup, self.ctx.clone(), cmds, input);
        links(&self.links).insert(
            pane,
            PaneLink {
                input: input_tx,
                frame: Arc::clone(&slot),
                hooks: hooks_tx,
                hooks_lost,
            },
        );
        self.panes.insert(
            pane,
            PaneHandle {
                tx,
                plan,
                cwd,
                size,
                slot,
                visible,
                seen,
            },
        );
    }

    fn stop_pane(&mut self, pane: PaneId) {
        hooks::revoke(self.ctx.hooks.as_deref(), pane);
        self.panes.remove(&pane);
        links(&self.links).remove(&pane);
    }

    /// Relaunches the pane's program; an actor that crashed is started again
    /// at the pane's last size and view.
    async fn relaunch(&mut self, pane: PaneId) {
        let Some(handle) = self.panes.get(&pane) else {
            return;
        };
        let tx = handle.tx.clone();
        if tx.send(PaneCmd::Relaunch).await.is_ok() {
            return;
        }
        if let Some(h) = self.panes.get(&pane) {
            let (plan, cwd, size) = (h.plan.clone(), h.cwd.clone(), h.size);
            self.start_pane(pane, plan, cwd, size);
        }
    }

    async fn on_client(&mut self, msg: ClientMsg) {
        match msg {
            ClientMsg::Command(command) => self.on_command(command).await,
            // The handle sends input straight to the pane's own queue.
            ClientMsg::Input { .. } | ClientMsg::Report { .. } => {}
            ClientMsg::ResizePane { pane, cols, rows } => {
                if let Some(handle) = self.panes.get_mut(&pane) {
                    handle.size = (cols, rows);
                }
                self.to_pane(pane, PaneCmd::Resize { cols, rows }).await;
            }
            ClientMsg::Scroll { pane, delta } => self.to_pane(pane, PaneCmd::Scroll(delta)).await,
            ClientMsg::ScrollToBottom { pane } => {
                self.to_pane(pane, PaneCmd::ScrollToBottom).await;
            }
            ClientMsg::StartSelection {
                pane,
                at,
                side,
                kind,
            } => {
                let cmd = PaneCmd::StartSelection { at, side, kind };
                self.to_pane(pane, cmd).await;
            }
            ClientMsg::UpdateSelection { pane, at, side } => {
                self.to_pane(pane, PaneCmd::UpdateSelection { at, side })
                    .await;
            }
            ClientMsg::ClearSelection { pane } => {
                self.to_pane(pane, PaneCmd::ClearSelection).await;
            }
            ClientMsg::CopySelection { pane } => self.to_pane(pane, PaneCmd::CopySelection).await,
            ClientMsg::WindowFocus { focused } => {
                let fx = self.ws.set_window_focus(focused);
                self.execute(fx).await;
            }
        }
    }

    async fn on_command(&mut self, command: Command) {
        if let Command::NewProject { root, .. } = &command {
            let path = PathBuf::from(root);
            let is_dir = tokio::task::spawn_blocking(move || path.is_dir())
                .await
                .unwrap_or(false);
            if !is_dir {
                self.error(None, &CoreError::NotADirectory(root.clone()))
                    .await;
                return;
            }
        }
        let pane = command_pane(&command);
        let config = Arc::clone(&self.ctx.config);
        let resolve = |profile: Option<&yien_proto::ProfileId>| {
            launch::resolve(profile, &config.profiles, &config.shell)
        };
        match self.ws.apply(command, &resolve) {
            Ok(fx) => self.execute(fx).await,
            Err(e) => self.error(pane, &e).await,
        }
    }

    async fn execute(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Emit(msg) => self.emit(msg).await,
                Effect::Spawn { pane, plan, cwd } => {
                    let size = self.ctx.config.initial_size;
                    self.start_pane(pane, *plan, cwd, size);
                }
                Effect::Stop { pane } => self.stop_pane(pane),
                Effect::Relaunch { pane } => self.relaunch(pane).await,
                Effect::Terminate { pane } => self.to_pane(pane, PaneCmd::Terminate).await,
                Effect::View {
                    pane,
                    visible,
                    seen,
                } => {
                    if let Some(handle) = self.panes.get_mut(&pane) {
                        (handle.visible, handle.seen) = (visible, seen);
                    }
                    self.to_pane(pane, PaneCmd::View { visible, seen }).await;
                }
            }
        }
    }
}

/// The profiles new panes can be started with: the shell first, then each
/// agent profile in the configured order.
fn profile_list(config: &CoreConfig) -> Vec<ProfileInfo> {
    let shell_name = config
        .profiles
        .iter()
        .find(|p| p.id == SHELL_PROFILE)
        .map_or_else(|| "Shell".to_owned(), |p| p.name.clone());
    let mut list = vec![ProfileInfo {
        id: ProfileId::new(SHELL_PROFILE),
        name: shell_name,
    }];
    list.extend(
        config
            .profiles
            .iter()
            .filter(|p| p.id != SHELL_PROFILE && p.detect.is_some())
            .map(|p| ProfileInfo {
                id: ProfileId::new(p.id.clone()),
                name: p.name.clone(),
            }),
    );
    list
}

fn command_pane(command: &Command) -> Option<PaneId> {
    match command {
        Command::SplitPane { pane, .. }
        | Command::ClosePane { pane }
        | Command::FocusPane { pane }
        | Command::ResizeSplit { pane, .. }
        | Command::MovePane { pane, .. }
        | Command::ToggleMaximize { pane }
        | Command::RenamePane { pane, .. }
        | Command::RelaunchPane { pane }
        | Command::TerminatePane { pane } => Some(*pane),
        Command::SwapPanes { a, .. } => Some(*a),
        Command::NewProject { .. }
        | Command::NewTab { .. }
        | Command::CloseTab { .. }
        | Command::SelectTab { .. }
        | Command::FocusDirection { .. } => None,
    }
}
