//! The core's command loop: applies UI messages to the workspace model,
//! carries out its effects and routes terminal traffic to the pane actors.
//!
//! Pane actors never wait on the service, and the service waits only on the
//! UI and on pane command queues that their actors always drain, so no wait
//! closes a cycle.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::{mpsc, watch};
use yien_agent::SessionReader;
use yien_agent::{HookError, HookEvent};
use yien_platform::agent_sessions::{self, MAX_SESSIONS};
use yien_platform::file_ops;
use yien_proto::file_ops::FileAction;
use yien_proto::file_preview::{FileFrame, FileNotice};
use yien_proto::{
    AgentSession, ClientMsg, Command, CoreMsg, PaneId, PendingQuestion, ProfileId, ProfileInfo,
    ProjectId, SessionTitle, ShellKind, TabGit, TabId, is_shell_profile,
};
use yien_pty::PtyBackend;
use yien_vt::Palette;

use crate::clipboards::Clipboards;
use crate::consoles::Consoles;
use crate::convert;
use crate::file_preview::{FileFrames, FileViews, LoadedDocument, load_file};
use crate::git::{GitReport, GitThread};
use crate::hooks::{self, Hooks};
use crate::launch::{self, LaunchPlan, SHELL_PROFILE};
use crate::opencode::OpenCodeWatch;
use crate::pane::{self, EngineFactory, FrameSlot, PANE_COMMANDS, PaneCmd, PaneInput, PaneSetup};
use crate::persist::{self, Opened, Writer};
use crate::programs::{self, Shells};
use crate::report::Reports;
use crate::screens::{ScreenSource, Screens};
use crate::shell::ShellSetup;
use crate::workspace::{Effect, Workspace};
use crate::worktree::{self, Done};
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
    /// Every session's closer, for the quit path.
    pub consoles: Arc<Consoles>,
    /// What programs asked to put on the clipboard, until the UI takes it.
    pub clipboards: Arc<Clipboards>,
    /// The shell panes the program poller looks under (RF-PAN-12).
    pub shells: Arc<Shells>,
    /// The watcher of OpenCode's service, when the core reads it.
    pub opencode: Option<Arc<OpenCodeWatch>>,
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
    /// What the agent asks the user, when the hook said (RF-ATN-13).
    pub question: Option<PendingQuestion>,
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
    /// The workspace's number for the pane's current launch.
    launch: u64,
    /// Where the screen writer reads the pane's screen; kept for a
    /// restarted actor.
    screen: Arc<ScreenSource>,
}

struct FileLoadDone {
    pane: PaneId,
    request: u64,
    doc: Arc<LoadedDocument>,
}

struct Service {
    ws: Workspace,
    ctx: Ctx,
    panes: HashMap<PaneId, PaneHandle>,
    file_views: FileViews,
    file_frames: Arc<FileFrames>,
    file_loads: mpsc::Sender<FileLoadDone>,
    links: Links,
    /// Saves the workspace; `None` without a session store.
    writer: Option<Arc<Writer>>,
    /// Runs git for the tabs; `None` when git facts are off.
    git: Option<GitThread>,
    /// The git facts last sent for each tab.
    git_sent: HashMap<TabId, TabGit>,
    /// Tabs whose folder git has looked at.
    git_probed: HashSet<TabId>,
    /// Where worktree jobs report back.
    jobs: mpsc::Sender<Done>,
    /// The palette every pane resolves colours with (RF-CFG-02).
    palette: Palette,
    /// Writes the pane screens; `None` without a screen store.
    screens: Option<Arc<Screens>>,
    /// Saved screens of restored panes, until their actors start.
    restored_screens: HashMap<PaneId, Vec<u8>>,
    /// Every shell with and without the folder report (RF-SES-01).
    shells: ShellSetup,
}

/// Worktree jobs waiting for the service; a job waits for room.
const WORKTREE_JOBS: usize = 16;

pub(crate) struct Stores {
    pub writer: Option<Arc<Writer>>,
    pub screens: Option<Arc<Screens>>,
    pub file_frames: Arc<FileFrames>,
}

pub(crate) async fn run(
    ctx: Ctx,
    links: Links,
    mut commands: mpsc::Receiver<ClientMsg>,
    hooks_error: Option<String>,
    stores: Stores,
    (shells, shell_notice): (ShellSetup, Option<String>),
) {
    let Stores {
        writer,
        screens,
        file_frames,
    } = stores;
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
    if let Some(probe) = ctx.config.programs {
        tokio::spawn(programs::poll(
            probe,
            Arc::clone(&ctx.shells),
            Arc::clone(&reports),
        ));
    }
    let (jobs, mut done) = mpsc::channel(WORKTREE_JOBS);
    let (file_loads, mut file_loaded) = mpsc::channel(WORKTREE_JOBS);
    let ctx_palette = ctx.config.vt.palette;
    let mut service = Service {
        ws: Workspace::new().with_profiles(profiles),
        ctx,
        panes: HashMap::new(),
        file_views: FileViews::default(),
        file_frames,
        file_loads,
        links,
        writer,
        git,
        git_sent: HashMap::new(),
        git_probed: HashSet::new(),
        jobs,
        palette: ctx_palette,
        screens,
        restored_screens: HashMap::new(),
        shells,
    };
    let mut notices = opened.notices;
    notices.extend(shell_notice);
    let mut restored = Vec::new();
    if let Some(snapshot) = &opened.snapshot {
        let config = Arc::clone(&service.ctx.config);
        let resolve =
            |profile: Option<&yien_proto::ProfileId>| launch::resolve_in(profile, &config);
        let (fx, notes) = service.ws.restore(snapshot, &resolve, &opened.disk);
        restored = fx;
        notices.extend(notes);
        service.restored_screens = service.read_screens(&restored).await;
        if let Some(screens) = &service.screens {
            // Panes of the saved session this run did not restore keep their screens.
            let panes = snapshot.projects.iter().flat_map(|p| &p.tabs);
            screens.keep(panes.flat_map(|t| &t.panes).map(|p| p.id.get()));
        }
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
    if opens_start_folder(saved_tabs, service.ws.tab_count())
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
            Some(job) = done.recv() => service.on_worktree(job).await,
            Some(loaded) = file_loaded.recv() => service.on_file_loaded(loaded).await,
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
    /// The saved screens of the panes a restore starts, read off the
    /// runtime's workers.
    async fn read_screens(&self, restored: &[Effect]) -> HashMap<PaneId, Vec<u8>> {
        let Some(screens) = self.screens.clone() else {
            return HashMap::new();
        };
        let panes: Vec<PaneId> = restored
            .iter()
            .filter_map(|fx| match fx {
                Effect::Spawn { pane, .. } => Some(*pane),
                _ => None,
            })
            .collect();
        tokio::task::spawn_blocking(move || {
            panes
                .into_iter()
                .filter_map(|pane| screens.read(pane).map(|dump| (pane, dump)))
                .collect()
        })
        .await
        .unwrap_or_default()
    }

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

    /// Runs a worktree job on a blocking thread; its result comes back
    /// through `jobs`, so the command loop never waits for git.
    fn run_job(&self, job: impl FnOnce() -> Done + Send + 'static) {
        let jobs = self.jobs.clone();
        tokio::task::spawn_blocking(move || {
            let _ = jobs.blocking_send(job());
        });
    }

    /// Applies what a worktree job found.
    async fn on_worktree(&mut self, done: Done) {
        let msg = match done {
            Done::Added {
                project,
                worktree,
                branch,
            } => {
                let config = Arc::clone(&self.ctx.config);
                let resolve = |profile: Option<&ProfileId>| launch::resolve_in(profile, &config);
                match self
                    .ws
                    .open_worktree_tab(project, worktree.clone(), branch.clone(), &resolve)
                {
                    Ok(fx) => {
                        self.execute(fx).await;
                        self.publish();
                        return;
                    }
                    Err(e) => CoreMsg::Error {
                        pane: None,
                        message: format!(
                            "The worktree {worktree} on branch {branch} was created, but no tab \
                             could open on it: {e}. It stays on disk."
                        ),
                    },
                }
            }
            Done::AddFailed { message, .. } => CoreMsg::Error {
                pane: None,
                message,
            },
            Done::Checked {
                tab,
                worktree,
                check,
            } => CoreMsg::WorktreeChecked {
                tab,
                worktree,
                check,
            },
            Done::Removed { worktree, branch } => CoreMsg::Notice {
                message: match branch {
                    Some(branch) => {
                        format!("Removed the worktree {worktree}; branch {branch} is kept.")
                    }
                    None => format!("Removed the worktree {worktree}."),
                },
            },
            Done::RemoveFailed { worktree, message } => CoreMsg::Error {
                pane: None,
                message: format!("The worktree {worktree} was kept: {message}"),
            },
        };
        self.emit(msg).await;
    }

    /// Runs a Files context menu action on a blocking thread and answers
    /// from there, so the command loop never waits on the disk (RF-CTX-01).
    /// Logs name the action only, never a path.
    fn run_file_action(&self, tab: TabId, root: String, action: FileAction) {
        let events = self.ctx.events.clone();
        tokio::task::spawn_blocking(move || {
            let kind = action.kind();
            let msg = match file_ops::run(Path::new(&root), &action) {
                Ok(done) => {
                    tracing::info!(action = kind, "file action done");
                    CoreMsg::FilesChanged {
                        tab,
                        dirs: done.dirs,
                        select: done.select,
                    }
                }
                Err(error) => {
                    tracing::info!(action = kind, "file action refused or failed");
                    CoreMsg::Error {
                        pane: None,
                        message: error.to_string(),
                    }
                }
            };
            let _ = events.blocking_send(msg);
        });
    }

    /// Lists the project's past agent sessions on a blocking thread with the
    /// reader each profile declares, and sends them to the UI from there, so
    /// the command loop never waits on the disk (RF-SES-07).
    fn list_sessions(&self, project: ProjectId, request: u64, root: String) {
        let config = Arc::clone(&self.ctx.config);
        let events = self.ctx.events.clone();
        tokio::task::spawn_blocking(move || {
            let (sessions, failed) = past_sessions(&config, &root);
            tracing::debug!(
                project = project.get(),
                sessions = sessions.len(),
                "listed past agent sessions"
            );
            if let Some(error) = failed {
                tracing::warn!("past sessions could not be read: {error}");
                let _ = events.blocking_send(CoreMsg::Error {
                    pane: None,
                    message: format!("Some past sessions could not be read: {error}"),
                });
            }
            let _ = events.blocking_send(CoreMsg::AgentSessions {
                project,
                request,
                sessions,
            });
        });
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
        let (slot, visible, seen, launch, screen) = self.panes.get(&pane).map_or_else(
            || (Arc::default(), false, false, 0, Arc::default()),
            |h| {
                let screen = Arc::clone(&h.screen);
                (Arc::clone(&h.slot), h.visible, h.seen, h.launch, screen)
            },
        );
        if let Some(screens) = &self.screens {
            screens.track(pane, Arc::clone(&screen));
        }
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
            fallback: resume.as_ref().and_then(|_| {
                let config = &self.ctx.config;
                launch::resolve(None, &config.profiles, &config.shell).ok()
            }),
            resume,
            wait_for_size,
            palette: self.palette,
            launch,
            screen: Arc::clone(&screen),
            restored: self.restored_screens.remove(&pane),
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
                launch,
                screen,
            },
        );
    }

    fn stop_pane(&mut self, pane: PaneId) {
        hooks::revoke(self.ctx.hooks.as_deref(), pane);
        self.panes.remove(&pane);
        self.file_views.forget(pane);
        links(&self.links).remove(&pane);
        self.ctx.clipboards.forget(pane);
        self.file_frames.forget(pane);
        self.restored_screens.remove(&pane);
        if let Some(screens) = &self.screens {
            screens.forget(pane);
        }
    }

    async fn start_file_load(&mut self, pane: PaneId, root: String, path: String) {
        let request = self.file_views.start(pane);
        let loading = FileFrame {
            path: path.clone(),
            language: None,
            top_line: 0,
            total_lines: 0,
            lines: Vec::new(),
            notice: Some(FileNotice::Loading),
        };
        if self.file_frames.put(pane, loading) {
            self.emit(CoreMsg::FileFrameReady { pane }).await;
        }
        let tx = self.file_loads.clone();
        tokio::task::spawn_blocking(move || {
            let doc = Arc::new(load_file(Path::new(&root), &path));
            let _ = tx.blocking_send(FileLoadDone { pane, request, doc });
        });
    }

    async fn on_file_loaded(&mut self, loaded: FileLoadDone) {
        let FileLoadDone { pane, request, doc } = loaded;
        let Some(frame) = self.file_views.loaded(pane, request, doc) else {
            return;
        };
        if self.file_frames.put(pane, frame) {
            self.emit(CoreMsg::FileFrameReady { pane }).await;
        }
    }

    /// Relaunches the pane's program; an actor that crashed is started again
    /// at the pane's last size and view.
    async fn relaunch(&mut self, pane: PaneId, launch: u64) {
        let Some(handle) = self.panes.get_mut(&pane) else {
            return;
        };
        handle.launch = launch;
        let tx = handle.tx.clone();
        if tx.send(PaneCmd::Relaunch { launch }).await.is_ok() {
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
            ClientMsg::SetShell { shell, integration } => {
                let (default, notice) = self.shells.default_shell(shell, integration);
                let installed = self.shells.installed(integration);
                let config = &self.ctx.config;
                if default != config.shell
                    || installed != config.shells
                    || shell != config.shell_kind
                    || integration != config.shell_integration
                {
                    tracing::info!(
                        shell = shell.map(ShellKind::id),
                        integration,
                        "the shell changed for new panes"
                    );
                    let mut config = (**config).clone();
                    config.shell = default;
                    config.shells = installed;
                    config.shell_kind = shell;
                    config.shell_integration = integration;
                    self.ctx.config = Arc::new(config);
                }
                if let Some(message) = notice {
                    // Not an Error with no pane: the UI reads that as a refused command.
                    self.emit(CoreMsg::Notice { message }).await;
                }
            }
            ClientMsg::SetTerminalPalette { palette: named } => {
                let palette = convert::palette(named);
                if palette != self.palette {
                    tracing::debug!(palette = ?named, panes = self.panes.len(), "terminal palette changed");
                    self.palette = palette;
                    let txs: Vec<_> = self.panes.values().map(|h| h.tx.clone()).collect();
                    for tx in txs {
                        let _ = tx.send(PaneCmd::Palette(palette)).await;
                    }
                }
            }
            ClientMsg::SetFileViewport {
                pane,
                top_line,
                rows,
            } => {
                if let Some(frame) = self.file_views.viewport(pane, top_line, rows)
                    && self.file_frames.put(pane, frame)
                {
                    self.emit(CoreMsg::FileFrameReady { pane }).await;
                }
            }
        }
    }

    async fn on_command(&mut self, command: Command) {
        // A folder that is a project already is only selected, even if it
        // is gone from the disk.
        if let Command::NewProject { root, .. } = &command
            && !self.ws.has_project_at(root)
        {
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
        // A folder a link leads out of the tab folder from is refused, as
        // the Files view's writes are (RNF-14).
        if let Command::OpenTerminal { tab, path } = &command
            && let Some(dir) = self.ws.tab_dir(*tab)
        {
            let (root, rel) = (PathBuf::from(dir), path.clone());
            let checked =
                tokio::task::spawn_blocking(move || file_ops::terminal_folder(&root, &rel)).await;
            let refused = match checked {
                Ok(Ok(_)) => None,
                Ok(Err(e)) => Some(e.to_string()),
                Err(_) => Some("The folder could not be checked.".to_owned()),
            };
            if let Some(message) = refused {
                self.emit(CoreMsg::Error {
                    pane: None,
                    message,
                })
                .await;
                return;
            }
        }
        let pane = command_pane(&command);
        let config = Arc::clone(&self.ctx.config);
        let resolve =
            |profile: Option<&yien_proto::ProfileId>| launch::resolve_in(profile, &config);
        match self.ws.apply(command, &resolve) {
            Ok(fx) => {
                self.execute(fx).await;
                self.publish();
            }
            Err(e) => self.error(pane, &e).await,
        }
    }

    async fn execute(&mut self, effects: Vec<Effect>) {
        let mut said = Vec::new();
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
                    let notice = plan.notice.clone();
                    self.start_pane(pane, *plan, cwd, size, resume, true);
                    // A restored tab of panes on a missing shell says so once.
                    if let Some(message) = notice.filter(|m| !said.contains(m)) {
                        said.push(message.clone());
                        self.emit(CoreMsg::Error {
                            pane: Some(pane),
                            message,
                        })
                        .await;
                    }
                }
                Effect::Stop { pane } => self.stop_pane(pane),
                Effect::Relaunch { pane, launch } => self.relaunch(pane, launch).await,
                Effect::Terminate { pane } => self.to_pane(pane, PaneCmd::Terminate).await,
                Effect::AddWorktree(job) => self.run_job(move || worktree::add(job)),
                Effect::ListAgentSessions {
                    project,
                    request,
                    root,
                } => self.list_sessions(project, request, root),
                Effect::CheckWorktree { tab, worktree } => {
                    self.run_job(move || worktree::check(tab, worktree));
                }
                Effect::RemoveWorktree(job) => {
                    let consoles = Arc::clone(&self.ctx.consoles);
                    self.run_job(move || worktree::remove(job, &consoles));
                }
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
                Effect::Program {
                    pane,
                    launch,
                    program,
                } => {
                    self.to_pane(pane, PaneCmd::Program { launch, program })
                        .await
                }
                Effect::LoadFile { pane, root, path } => {
                    self.start_file_load(pane, root, path).await;
                }
                Effect::FileAction { tab, root, action } => {
                    self.run_file_action(tab, root, action);
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

/// Whether the start folder opens as a project: when no session was read, or
/// when the saved one had tabs and none could be restored. A saved session
/// the user emptied stays empty, and the window invites a new project.
fn opens_start_folder(saved_tabs: Option<usize>, restored_tabs: usize) -> bool {
    match saved_tabs {
        None => true,
        Some(saved) => saved > 0 && restored_tabs == 0,
    }
}

/// The past sessions of the project at `root` from every profile that
/// declares a reader, newest first and at most [`MAX_SESSIONS`], and the
/// last failure to read a CLI's folder, if any.
fn past_sessions(config: &CoreConfig, root: &str) -> (Vec<AgentSession>, Option<String>) {
    let mut readers: Vec<(&str, SessionReader)> = Vec::new();
    for profile in &config.profiles {
        if let Some(reader) = profile.launch.sessions
            && !readers.iter().any(|(_, r)| *r == reader)
        {
            readers.push((&profile.id, reader));
        }
    }
    let mut sessions = Vec::new();
    let mut failed = None;
    for (profile, reader) in readers {
        match agent_sessions::list(reader, &config.cli_dirs, root, MAX_SESSIONS) {
            Ok(found) => sessions.extend(found.into_iter().map(|s| AgentSession {
                profile: ProfileId::new(profile),
                id: s.id,
                title: s.summary.title.map(SessionTitle::new),
                modified_ms: s.modified_ms,
            })),
            Err(error) => failed = Some(error.to_string()),
        }
    }
    sessions.sort_by(|a, b| b.modified_ms.cmp(&a.modified_ms).then(a.id.cmp(&b.id)));
    sessions.truncate(MAX_SESSIONS);
    (sessions, failed)
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
    list.extend(config.shells.kinds().map(|kind| ProfileInfo {
        id: kind.profile(),
        name: kind.name().to_owned(),
    }));
    list.extend(
        config
            .profiles
            .iter()
            .filter(|p| !is_shell_profile(&p.id) && p.detect.is_some())
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
        | Command::TerminatePane { pane }
        | Command::SelectFileTab { pane, .. }
        | Command::CloseFileTab { pane, .. }
        | Command::ToggleFileWrap { pane } => Some(*pane),
        Command::SwapPanes { a, .. } => Some(*a),
        Command::SetDivider { before, .. } => Some(*before),
        Command::NewProject { .. }
        | Command::CloseProject { .. }
        | Command::SetProjectCollapsed { .. }
        | Command::RenameTab { .. }
        | Command::MoveTab { .. }
        | Command::SetTabMuted { .. }
        | Command::SetNotificationsMuted { .. }
        | Command::SetSideBars { .. }
        | Command::SetTerminalZoom { .. }
        | Command::NewTab { .. }
        | Command::NewWorktreeTab { .. }
        | Command::CheckWorktree { .. }
        | Command::RemoveWorktreeTab { .. }
        | Command::CloseTab { .. }
        | Command::SelectTab { .. }
        | Command::FocusDirection { .. }
        | Command::ListAgentSessions { .. }
        | Command::ResumeAgentSession { .. }
        | Command::OpenFile { .. }
        | Command::FileAction { .. }
        | Command::OpenTerminal { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use yien_proto::{GitFacts, GitHead};

    use super::*;
    use crate::shell::ShellCommand;

    #[test]
    fn rf_pan_18_the_picker_lists_the_shells_next_to_the_agents() {
        let config = CoreConfig {
            shells: crate::InstalledShells::from_programs([
                (ShellKind::GitBash, "C:/Git/bin/bash.exe".to_owned()),
                (ShellKind::Cmd, "cmd.exe".to_owned()),
            ]),
            ..CoreConfig::default()
        };
        let list: Vec<(String, String)> = profile_list(&config)
            .into_iter()
            .map(|p| (p.id.as_str().to_owned(), p.name))
            .collect();
        let ids: Vec<&str> = list.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "shell",
                "shell:cmd",
                "shell:git-bash",
                "claude",
                "codex",
                "opencode"
            ]
        );
        assert_eq!(list[1].1, "Command Prompt");
        assert_eq!(list[2].1, "Git Bash");
        let none = profile_list(&CoreConfig::default());
        assert!(
            none.iter()
                .all(|p| ShellKind::from_profile(p.id.as_str()).is_none())
        );
    }

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
    fn the_start_folder_opens_only_without_a_session_or_when_none_of_it_came_back() {
        assert!(opens_start_folder(None, 0));
        assert!(opens_start_folder(Some(2), 0));
        assert!(!opens_start_folder(Some(2), 1));
        assert!(
            !opens_start_folder(Some(0), 0),
            "the user closed every project"
        );
    }

    #[test]
    fn a_report_for_a_closed_tab_is_dropped() {
        let (mut ws, _) = workspace();
        let mut sent = HashMap::new();
        assert!(apply_git(&mut ws, &mut sent, report(TabId::new(99), on("main"))).is_none());
        assert!(sent.is_empty());
    }
}
