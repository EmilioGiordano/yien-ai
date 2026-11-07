//! The core's command loop: applies UI messages to the workspace model,
//! carries out its effects and routes terminal traffic to the pane actors.
//!
//! Pane actors never wait on the service, and the service waits only on the
//! UI and on pane command queues that their actors always drain, so no wait
//! closes a cycle.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::mpsc;
use yien_proto::{ClientMsg, Command, CoreMsg, PaneId};
use yien_pty::PtyBackend;

use crate::launch::{self, LaunchPlan};
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
}

/// How the handle reaches a pane without going through the service: its
/// input queue and its frame slot.
#[derive(Debug, Clone)]
pub(crate) struct PaneLink {
    pub input: mpsc::Sender<PaneInput>,
    pub frame: Arc<FrameSlot>,
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

pub(crate) async fn run(ctx: Ctx, links: Links, mut commands: mpsc::Receiver<ClientMsg>) {
    let reports = Arc::clone(&ctx.reports);
    let mut service = Service {
        ws: Workspace::new(),
        ctx,
        panes: HashMap::new(),
        links,
    };
    service
        .emit(CoreMsg::WorkspaceSnapshot(service.ws.snapshot()))
        .await;
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

    /// Input normally goes from the handle straight to the pane; this path
    /// serves a message that reached the service anyway.
    async fn write(&self, pane: PaneId, input: PaneInput) {
        let tx = links(&self.links).get(&pane).map(|l| l.input.clone());
        if let Some(tx) = tx {
            let _ = tx.send(input).await;
        }
    }

    fn start_pane(&mut self, pane: PaneId, plan: LaunchPlan, cwd: String, size: (u16, u16)) {
        let (tx, cmds) = mpsc::channel(PANE_COMMANDS);
        let (input_tx, input) = mpsc::channel(PANE_INPUT_QUEUE);
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
        };
        pane::spawn(setup, self.ctx.clone(), cmds, input);
        links(&self.links).insert(
            pane,
            PaneLink {
                input: input_tx,
                frame: Arc::clone(&slot),
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
            ClientMsg::Input { pane, bytes } => {
                let input = PaneInput {
                    bytes: bytes.into_vec(),
                    user: true,
                };
                self.write(pane, input).await;
            }
            ClientMsg::Report { pane, bytes } => {
                let input = PaneInput {
                    bytes: bytes.into_vec(),
                    user: false,
                };
                self.write(pane, input).await;
            }
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
