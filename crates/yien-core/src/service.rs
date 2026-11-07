//! The core's command loop: applies UI messages to the workspace model,
//! carries out its effects and routes terminal traffic to the pane actors.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc;
use yien_proto::{ClientMsg, Command, CoreMsg, PaneId};
use yien_pty::PtyBackend;

use crate::launch::{self, LaunchPlan};
use crate::pane::{self, PaneCmd};
use crate::workspace::{Effect, PaneEvent, Workspace};
use crate::{CoreConfig, CoreError};

/// What every pane actor shares.
#[derive(Clone)]
pub(crate) struct Ctx {
    pub backend: Arc<dyn PtyBackend>,
    pub config: Arc<CoreConfig>,
    pub events: mpsc::Sender<CoreMsg>,
}

/// A running pane actor, and what it was started with so a crashed one can
/// be started again.
struct PaneHandle {
    tx: mpsc::UnboundedSender<PaneCmd>,
    plan: LaunchPlan,
    cwd: String,
}

struct Service {
    ws: Workspace,
    ctx: Ctx,
    panes: HashMap<PaneId, PaneHandle>,
    report: mpsc::UnboundedSender<(PaneId, PaneEvent)>,
}

pub(crate) async fn run(ctx: Ctx, mut commands: mpsc::Receiver<ClientMsg>) {
    let (report, mut reports) = mpsc::unbounded_channel();
    let mut service = Service {
        ws: Workspace::new(),
        ctx,
        panes: HashMap::new(),
        report,
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
            Some((pane, event)) = reports.recv() => {
                let fx = service.ws.on_pane_event(pane, event);
                service.execute(fx).await;
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

    fn to_pane(&self, pane: PaneId, cmd: PaneCmd) {
        if let Some(handle) = self.panes.get(&pane) {
            let _ = handle.tx.send(cmd);
        }
    }

    fn start_pane(&mut self, pane: PaneId, plan: LaunchPlan, cwd: String) {
        let (tx, rx) = mpsc::unbounded_channel();
        pane::spawn(
            pane,
            plan.clone(),
            cwd.clone(),
            self.ctx.clone(),
            rx,
            self.report.clone(),
        );
        self.panes.insert(pane, PaneHandle { tx, plan, cwd });
    }

    fn relaunch(&mut self, pane: PaneId) {
        let Some(handle) = self.panes.get(&pane) else {
            return;
        };
        if handle.tx.send(PaneCmd::Relaunch).is_err() {
            let (plan, cwd) = (handle.plan.clone(), handle.cwd.clone());
            self.start_pane(pane, plan, cwd);
        }
    }

    async fn on_client(&mut self, msg: ClientMsg) {
        match msg {
            ClientMsg::Command(command) => self.on_command(command).await,
            ClientMsg::Input { pane, bytes } => self.to_pane(
                pane,
                PaneCmd::Write {
                    bytes: bytes.into_vec(),
                    user: true,
                },
            ),
            ClientMsg::Report { pane, bytes } => self.to_pane(
                pane,
                PaneCmd::Write {
                    bytes: bytes.into_vec(),
                    user: false,
                },
            ),
            ClientMsg::ResizePane { pane, cols, rows } => {
                self.to_pane(pane, PaneCmd::Resize { cols, rows });
            }
            ClientMsg::Scroll { pane, delta } => self.to_pane(pane, PaneCmd::Scroll(delta)),
            ClientMsg::ScrollToBottom { pane } => self.to_pane(pane, PaneCmd::ScrollToBottom),
            ClientMsg::StartSelection {
                pane,
                at,
                side,
                kind,
            } => self.to_pane(pane, PaneCmd::StartSelection { at, side, kind }),
            ClientMsg::UpdateSelection { pane, at, side } => {
                self.to_pane(pane, PaneCmd::UpdateSelection { at, side });
            }
            ClientMsg::ClearSelection { pane } => self.to_pane(pane, PaneCmd::ClearSelection),
            ClientMsg::CopySelection { pane } => self.to_pane(pane, PaneCmd::CopySelection),
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
                Effect::Spawn { pane, plan, cwd } => self.start_pane(pane, *plan, cwd),
                Effect::Stop { pane } => {
                    self.panes.remove(&pane);
                }
                Effect::Relaunch { pane } => self.relaunch(pane),
                Effect::Terminate { pane } => self.to_pane(pane, PaneCmd::Terminate),
                Effect::View {
                    pane,
                    visible,
                    seen,
                } => self.to_pane(pane, PaneCmd::View { visible, seen }),
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
