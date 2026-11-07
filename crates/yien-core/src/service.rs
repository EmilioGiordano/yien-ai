//! The core's command loop: applies UI messages to the workspace model,
//! carries out its effects and routes terminal traffic to the pane actors.
//!
//! Pane actors never wait on the service, and the service waits only on the
//! UI and on pane command queues that their actors always drain, so no wait
//! closes a cycle.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::{mpsc, watch};
use yien_agent::{HookError, HookEvent};
use yien_proto::{ClientMsg, Command, CoreMsg, PaneId, ProfileId, ProfileInfo, TabGit, TabId};
use yien_pty::PtyBackend;

use crate::git::{GitReport, GitThread};
use crate::hooks::{self, Hooks};
use crate::launch::{self, LaunchPlan, SHELL_PROFILE};
use crate::pane::{self, EngineFactory, FrameSlot, PANE_COMMANDS, PaneCmd, PaneInput, PaneSetup};
use crate::persist::{self, Opened, Writer};
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
    /// The last size the UI reported for any pane, `None` until it does.
    pub ui_size: Arc<watch::Sender<Option<(u16, u16)>>>,
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
    /// Saves the workspace; `None` without a session store.
    writer: Option<Arc<Writer>>,
    /// Runs git for the tabs; `None` when git facts are off.
    git: Option<GitThread>,
    /// The git facts last sent for each tab.
    git_sent: HashMap<TabId, TabGit>,
    /// Tabs whose folder git has looked at.
    git_probed: HashSet<TabId>,
}

pub(crate) async fn run(
    ctx: Ctx,
    links: Links,
    mut commands: mpsc::Receiver<ClientMsg>,
    hooks_error: Option<String>,
    writer: Option<Arc<Writer>>,
) {
    let reports = Arc::clone(&ctx.reports);
    let profiles = profile_list(&ctx.config);
    let (opened, open_failed) = match ctx.config.session.clone() {
        Some(store) => match tokio::task::spawn_blocking(move || persist::open(&store)).await {
            Ok(opened) => (opened, false),
            Err(_) => (Opened::default(), true),
        },
        None => (Opened::default(), false),
    };
    let (git, mut git_reports) = match ctx.config.git.and_then(GitThread::start) {
        Some((git, reports)) => (Some(git), Some(reports)),
        None => (None, None),
    };
    let mut service = Service {
        ws: Workspace::new().with_profiles(profiles),
        ctx,
        panes: HashMap::new(),
        links,
        writer,
        git,
        git_sent: HashMap::new(),
        git_probed: HashSet::new(),
    };
    let mut notices = opened.notices;
    let mut restored = Vec::new();
    if let Some(snapshot) = &opened.snapshot {
        let config = Arc::clone(&service.ctx.config);
        let resolve = |profile: Option<&yien_proto::ProfileId>| {
            launch::resolve(profile, &config.profiles, &config.shell)
        };
        let (fx, notes) = service.ws.restore(snapshot, &resolve, &opened.disk);
        restored = fx;
        notices.extend(notes);
    }
    let saved_tabs = opened
        .snapshot
        .as_ref()
        .map(|s| s.projects.iter().map(|p| p.tabs.len()).sum::<usize>());
    if open_failed || saved_tabs.is_some_and(|n| service.ws.tab_count() < n) {
        notices.extend(service.set_aside().await);
    }
    service
        .emit(CoreMsg::WorkspaceSnapshot(service.ws.snapshot()))
        .await;
    if let Some(reason) = hooks_error {
        service
            .emit(CoreMsg::Error {
                pane: None,
                message: format!(
                    "Claude Code state detection is off: {reason}. \
                     Claude panes still run, without the red mark."
                ),
            })
            .await;
    }
    if !notices.is_empty() {
        service
            .emit(CoreMsg::Error {
                pane: None,
                message: notices.join(" "),
            })
            .await;
    }
    service.execute(restored).await;
    if service.ws.tab_count() == 0
        && let Some(root) = service.ctx.config.start_project.clone()
    {
        service
            .on_command(Command::NewProject { root, name: None })
            .await;
    }
    service.publish();
    service.sync_git();
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
                service.publish();
            }
            report = next_git(&mut git_reports) => match report {
                Some(report) => service.on_git(report).await,
                None => git_reports = None,
            },
        }
        service.sync_git();
    }
}

/// The next git report; never, when git facts are off.
async fn next_git(reports: &mut Option<mpsc::Receiver<GitReport>>) -> Option<GitReport> {
    match reports {
        Some(reports) => reports.recv().await,
        None => std::future::pending().await,
    }
}

impl Service {
    /// Keeps a saved session this run could not restore whole out of the way
    /// of the first save; when that fails, nothing is saved this run.
    async fn set_aside(&self) -> Option<String> {
        let store = self.ctx.config.session.clone()?;
        let moved = tokio::task::spawn_blocking(move || persist::set_aside(&store)).await;
        match moved {
            Ok(Ok(None)) => None,
            Ok(Ok(Some(name))) => Some(format!(
                "The saved session could not be restored whole; it was kept as {name}."
            )),
            Ok(Err(_)) | Err(_) => {
                if let Some(writer) = &self.writer {
                    writer.stop();
                }
                Some(
                    "The saved session could not be restored or set aside, so this session \
                     will not be saved."
                        .to_owned(),
                )
            }
        }
    }

    /// Points git at the selected tab, and has it look once at every tab it
    /// has not seen, so their rows show a branch too.
    fn sync_git(&mut self) {
        let Some(git) = &mut self.git else {
            return;
        };
        git.watch(self.ws.active_dir());
        let watched = git.watched();
        for (tab, dir) in self.ws.tab_dirs() {
            if self.git_probed.insert(tab) && Some(tab) != watched {
                git.probe(tab, dir);
            }
        }
        let ws = &self.ws;
        self.git_probed.retain(|tab| ws.has_tab(*tab));
        self.git_sent.retain(|tab, _| ws.has_tab(*tab));
    }

    /// Applies what git found in a tab's folder. A failure keeps the branch
    /// last seen, so a slow git does not blank the tab's row.
    async fn on_git(&mut self, report: GitReport) {
        let Some((fx, msg)) = apply_git(&mut self.ws, &mut self.git_sent, report) else {
            return;
        };
        let changed = !fx.is_empty();
        self.execute(fx).await;
        self.emit(msg).await;
        if changed {
            self.publish();
        }
    }

    /// Hands the workspace to the writer; it writes only what changed.
    fn publish(&self) {
        if let Some(writer) = &self.writer {
            writer.update(self.ws.to_session());
        }
    }

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

    fn start_pane(
        &mut self,
        pane: PaneId,
        plan: LaunchPlan,
        cwd: String,
        size: (u16, u16),
        resume: Option<String>,
        wait_for_size: bool,
    ) {
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
            resume,
            wait_for_size,
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
            self.start_pane(pane, plan, cwd, size, None, false);
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
                self.ctx.ui_size.send_replace(Some((cols, rows)));
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
            Ok(fx) => {
                self.execute(fx).await;
                self.publish();
            }
            Err(e) => self.error(pane, &e).await,
        }
    }

    async fn execute(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Emit(msg) => self.emit(msg).await,
                Effect::Spawn {
                    pane,
                    plan,
                    cwd,
                    resume,
                } => {
                    let size = self.ctx.config.initial_size;
                    self.start_pane(pane, *plan, cwd, size, resume, true);
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

/// What a git report changes: the tab update when its branch changed, then
/// the facts for the UI. `None` for a closed tab or facts already sent. An
/// `Unavailable` report keeps the branch last seen.
fn apply_git(
    ws: &mut Workspace,
    sent: &mut HashMap<TabId, TabGit>,
    report: GitReport,
) -> Option<(Vec<Effect>, CoreMsg)> {
    let GitReport { tab, git } = report;
    if !ws.has_tab(tab) || sent.get(&tab) == Some(&git) {
        return None;
    }
    let fx = match git {
        TabGit::Unavailable { .. } => Vec::new(),
        _ => ws.set_branch(tab, git.branch().map(str::to_owned)),
    };
    sent.insert(tab, git.clone());
    Some((fx, CoreMsg::TabGit { tab, git }))
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

#[cfg(test)]
mod tests {
    use yien_proto::{GitFacts, GitHead};

    use super::*;
    use crate::shell::ShellCommand;

    fn workspace() -> (Workspace, TabId) {
        let mut ws = Workspace::new();
        let resolve =
            |profile: Option<&ProfileId>| launch::resolve(profile, &[], &ShellCommand::new("sh"));
        let new = Command::NewProject {
            root: "C:/Code/yien".into(),
            name: None,
        };
        ws.apply(new, &resolve).unwrap();
        let tab = ws.active_dir().unwrap().0;
        (ws, tab)
    }

    fn on(branch: &str) -> TabGit {
        TabGit::Repository(GitFacts {
            head: GitHead::Branch {
                name: branch.into(),
                unborn: false,
            },
            upstream: None,
            root: "C:/Code/yien".into(),
            linked_worktree: false,
            main_worktree: None,
            files: vec![],
            changed: 0,
        })
    }

    fn report(tab: TabId, git: TabGit) -> GitReport {
        GitReport { tab, git }
    }

    fn branch_of(ws: &Workspace, tab: TabId) -> Option<String> {
        ws.snapshot()
            .tabs
            .into_iter()
            .find(|t| t.id == tab)
            .and_then(|t| t.branch)
    }

    #[test]
    fn rf_esp_04_unchanged_facts_are_not_sent_again() {
        let (mut ws, tab) = workspace();
        let mut sent = HashMap::new();
        let (fx, msg) = apply_git(&mut ws, &mut sent, report(tab, on("main"))).unwrap();
        assert!(matches!(
            fx.as_slice(),
            [Effect::Emit(CoreMsg::TabUpdated(t))] if t.branch.as_deref() == Some("main")
        ));
        assert_eq!(
            msg,
            CoreMsg::TabGit {
                tab,
                git: on("main")
            }
        );
        assert!(apply_git(&mut ws, &mut sent, report(tab, on("main"))).is_none());
        let (fx, _) = apply_git(&mut ws, &mut sent, report(tab, on("feat"))).unwrap();
        assert_eq!(fx.len(), 1);
    }

    #[test]
    fn rf_esp_04_git_trouble_keeps_the_branch_last_seen() {
        let (mut ws, tab) = workspace();
        let mut sent = HashMap::new();
        apply_git(&mut ws, &mut sent, report(tab, on("main"))).unwrap();
        let trouble = TabGit::Unavailable {
            reason: "git did not answer within 2000 ms".into(),
        };
        let (fx, msg) = apply_git(&mut ws, &mut sent, report(tab, trouble.clone())).unwrap();
        assert!(fx.is_empty());
        assert_eq!(msg, CoreMsg::TabGit { tab, git: trouble });
        assert_eq!(branch_of(&ws, tab).as_deref(), Some("main"));

        let (fx, _) = apply_git(&mut ws, &mut sent, report(tab, TabGit::NoRepository)).unwrap();
        assert_eq!(fx.len(), 1);
        assert_eq!(branch_of(&ws, tab), None);
    }

    #[test]
    fn a_report_for_a_closed_tab_is_dropped() {
        let (mut ws, _) = workspace();
        let mut sent = HashMap::new();
        assert!(apply_git(&mut ws, &mut sent, report(TabId::new(99), on("main"))).is_none());
        assert!(sent.is_empty());
    }
}
