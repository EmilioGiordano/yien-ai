//! One actor per pane: the only owner of its terminal engine and agent state
//! machine (ADR-06). A blocking reader thread feeds the engine from the PTY;
//! the actor writes to the PTY, publishes frames and runs the state machine.

use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;
use yien_agent::{
    Effect as AgentEffect, HookEvent, Machine, SessionEndReason, Signal, evaluate_screen,
};
use yien_proto::frame::{Frame, SelectionKind, Side, ViewportPoint};
use yien_proto::{AgentState, ClientMsg, CoreMsg, InputBytes, PaneId, PendingQuestion};
use yien_pty::{PtyControl, PtyError, PtyEvent};
use yien_vt::{
    AlacrittyEngine, Palette, SYNC_UPDATE_TIMEOUT, TitleChange, VtConfig, VtEngine, VtOutput,
};

use crate::convert;
use crate::launch::{self, Fill, HookFill, LaunchPlan};
use crate::resume::{self, ResumeFailure};
use crate::service::{Ctx, HookItem};
use crate::workspace::PaneEvent;

/// How soon a write refused by a full input queue is tried again. Nothing
/// signals when the queue frees up.
const RETRY_AFTER: Duration = Duration::from_millis(5);

/// Output chunks the reader may post ahead of the actor; beyond this it stops
/// reading and the PTY's own backpressure stalls the child (plan §4.2).
const READER_QUEUE: usize = 64;

/// How often an idle reader checks whether its pane is gone. A session's
/// event receiver never disconnects while its control lives.
const READER_POLL: Duration = Duration::from_millis(200);

/// Replies the actor may hold for a child that does not read its input.
/// Beyond this it stops taking the child's output, which stalls the child
/// instead of growing the backlog.
const REPLY_BACKLOG: usize = 64;

/// How long a pane still waiting for its size gives it to arrive once the
/// UI has reported its first size for any pane: the UI sends the sizes of
/// every pane it draws together.
const UI_SIZE_GRACE: Duration = Duration::from_millis(50);

/// Commands a pane actor may have waiting. The actor never waits on the
/// service, so a full queue only holds the service back briefly.
pub(crate) const PANE_COMMANDS: usize = 256;

const READER_LOST: &str = "This pane stopped reading its program after an internal error. \
                           Relaunch it to start the program again.";

/// What the service asks of a pane actor.
#[derive(Debug)]
pub(crate) enum PaneCmd {
    Resize {
        cols: u16,
        rows: u16,
    },
    Scroll(i32),
    ScrollToBottom,
    StartSelection {
        at: ViewportPoint,
        side: Side,
        kind: SelectionKind,
    },
    UpdateSelection {
        at: ViewportPoint,
        side: Side,
    },
    ClearSelection,
    CopySelection,
    Palette(Palette),
    View {
        visible: bool,
        seen: bool,
    },
    /// Start the program again, as the workspace's launch number `launch`.
    Relaunch {
        launch: u64,
    },
    Terminate,
}

/// Bytes for a pane's program, sent by the handle straight to the actor.
#[derive(Debug)]
pub(crate) struct PaneInput {
    pub bytes: Vec<u8>,
    /// False for reports YIEN generates (focus, mouse): they are not the
    /// user acting on the pane.
    pub user: bool,
}

impl PaneInput {
    /// The message this input came from, to hand back when it is refused.
    pub fn into_msg(self, pane: PaneId) -> ClientMsg {
        let bytes = InputBytes::new(self.bytes);
        if self.user {
            ClientMsg::Input { pane, bytes }
        } else {
            ClientMsg::Report { pane, bytes }
        }
    }
}

/// Builds a pane's terminal engine.
pub(crate) type EngineFactory = fn(&VtConfig, u16, u16) -> Box<dyn VtEngine>;

/// The engine every pane runs outside tests.
pub(crate) fn alacritty(config: &VtConfig, cols: u16, rows: u16) -> Box<dyn VtEngine> {
    Box::new(AlacrittyEngine::new(config.clone(), cols, rows))
}

fn new_engine(ctx: &Ctx, palette: Palette, cols: u16, rows: u16) -> Box<dyn VtEngine> {
    let config = VtConfig {
        palette,
        ..ctx.config.vt.clone()
    };
    (ctx.engine)(&config, cols, rows)
}

enum ReaderMsg {
    Output { out: VtOutput, sync: Option<u64> },
    Exited(Option<u32>),
}

type Engine = Arc<Mutex<Box<dyn VtEngine>>>;

fn lock(engine: &Engine) -> MutexGuard<'_, Box<dyn VtEngine>> {
    engine.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The kind of attention state a pane is in, if any: "needs you", error
/// or done. A pane keeps its place in the attention order while its state
/// stays of the same kind.
fn attention_kind(state: AgentState) -> Option<u8> {
    match state {
        AgentState::NeedsYou(_) => Some(0),
        AgentState::Error(_) => Some(1),
        AgentState::Done => Some(2),
        AgentState::Idle | AgentState::Working => None,
    }
}

/// A hook after which an earlier question no longer stands: the user
/// answered, the turn went on or ended, or the session changed.
fn moves_on(event: &HookEvent) -> bool {
    matches!(
        event,
        HookEvent::UserPromptSubmit
            | HookEvent::PostToolUse
            | HookEvent::PostToolUseFailure
            | HookEvent::Stop
            | HookEvent::StopFailure
            | HookEvent::SessionStart { .. }
            | HookEvent::SessionEnd(_)
    )
}

/// The latest frame of a pane the UI has not taken yet: a newer one replaces
/// it (plan §4.2, "last one wins"). The UI is told once per frame it takes.
#[derive(Debug, Default)]
pub(crate) struct FrameSlot(Mutex<SlotState>);

#[derive(Debug, Default)]
struct SlotState {
    frame: Option<Frame>,
    notified: bool,
}

impl FrameSlot {
    fn state(&self) -> MutexGuard<'_, SlotState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Stores `frame`. `notify` tells the UI and says whether the notice got
    /// through. Returns whether a notice is still owed.
    fn put(&self, frame: Frame, notify: impl FnOnce() -> bool) -> bool {
        let mut state = self.state();
        state.frame = Some(frame);
        Self::settle(&mut state, notify)
    }

    /// Sends a notice still owed. Returns whether one is still owed.
    fn retry(&self, notify: impl FnOnce() -> bool) -> bool {
        Self::settle(&mut self.state(), notify)
    }

    /// Runs under the slot's lock, so a take cannot fall between the notice
    /// and its record and leave a frame nobody is told about.
    fn settle(state: &mut SlotState, notify: impl FnOnce() -> bool) -> bool {
        if state.frame.is_some() && !state.notified {
            state.notified = notify();
        }
        state.frame.is_some() && !state.notified
    }

    /// Takes the frame waiting, if any. The next frame is notified again.
    pub fn take(&self) -> Option<Frame> {
        let mut state = self.state();
        state.notified = false;
        state.frame.take()
    }
}

/// Bytes waiting for the PTY's input queue. Replies to terminal queries go
/// first and are never dropped: ConPTY stalls the child until `ESC [ 6 n` is
/// answered.
#[derive(Debug, Default)]
struct Outbox {
    replies: VecDeque<Vec<u8>>,
    input: VecDeque<Vec<u8>>,
    resize: Option<(u16, u16)>,
}

impl Outbox {
    fn is_empty(&self) -> bool {
        self.replies.is_empty() && self.input.is_empty() && self.resize.is_none()
    }

    fn clear(&mut self) {
        *self = Self::default();
    }

    /// Replies go before a pending resize: a full resize queue must not hold
    /// back the answer the child is blocked on.
    fn drain(&mut self, control: &PtyControl) -> Result<(), Blocked> {
        write_all(control, &mut self.replies)?;
        if let Some((cols, rows)) = self.resize {
            match control.resize(cols, rows) {
                Ok(()) | Err(PtyError::InvalidSize { .. }) => self.resize = None,
                Err(PtyError::ResizeQueueFull) => return Err(Blocked::Full),
                Err(_) => return Err(Blocked::Closed),
            }
        }
        write_all(control, &mut self.input)
    }
}

struct Session {
    control: PtyControl,
    reader: mpsc::Receiver<ReaderMsg>,
    exited: bool,
}

pub(crate) struct PaneSetup {
    pub id: PaneId,
    pub plan: LaunchPlan,
    pub cwd: String,
    pub size: (u16, u16),
    pub slot: Arc<FrameSlot>,
    /// The view the workspace last sent, so a restarted actor draws and
    /// counts as seen without waiting for the next change.
    pub visible: bool,
    pub seen: bool,
    /// Hook events for this pane, from the hook pipe.
    pub hooks: mpsc::Receiver<HookItem>,
    /// Set by the hook pipe when it had to refuse an event for this pane.
    pub hooks_lost: Arc<AtomicBool>,
    /// The agent session the first launch resumes (RF-SES-03).
    pub resume: Option<String>,
    /// What runs instead when that resume fails (RF-SES-11): the shell.
    pub fallback: Option<LaunchPlan>,
    /// Start the program only once the UI's size arrives (see
    /// [`CoreConfig::size_wait`](crate::CoreConfig::size_wait)).
    pub wait_for_size: bool,
    /// The palette the UI asked for last; every engine of this pane uses it.
    pub palette: Palette,
    /// The workspace's number for the launch this actor starts with.
    pub launch: u64,
}

/// A pane that has not started its program yet because it waits for the
/// UI's size: Windows 10's in-box ConPTY draws garbled after a resize that
/// comes right after the spawn.
struct Pending {
    created: Instant,
    ui: watch::Receiver<Option<(u16, u16)>>,
    /// When the UI first reported a size for any pane, once it has.
    ui_seen: Option<Instant>,
}

struct Actor {
    id: PaneId,
    plan: LaunchPlan,
    cwd: String,
    ctx: Ctx,
    slot: Arc<FrameSlot>,
    engine: Engine,
    session: Option<Session>,
    machine: Machine,
    state: AgentState,
    outbox: Outbox,
    retry_at: Option<Instant>,
    dirty: bool,
    visible: bool,
    last_frame: Option<Instant>,
    notice_owed: bool,
    sync: Option<(u64, Instant)>,
    wake_at: Option<Instant>,
    hooks_lost: Arc<AtomicBool>,
    /// The serial of this launch's hook token. A hook with another one was
    /// on its way when the pane relaunched and belongs to the old program.
    hook_serial: Option<u64>,
    /// The profile reports through hooks but this launch has no channel.
    hooks_missing: bool,
    /// The "signal lost" mark last reported.
    signal_lost: bool,
    /// When the pane entered "needs you", error or done, while it is in
    /// that kind of state (RF-ATN-05, RF-ATN-13).
    attention_since: Option<u64>,
    /// What the last hook that asked the user said, until the turn moves on.
    asked: Option<PendingQuestion>,
    /// The question last reported: `asked` while the pane needs the user.
    question: Option<PendingQuestion>,
    /// The session the next launch resumes; only the first launch after a
    /// restore does, so a relaunch starts a new session.
    resume: Option<String>,
    palette: Palette,
    pending: Option<Pending>,
    /// The shell a failed resume falls back to, until it is used.
    fallback: Option<LaunchPlan>,
    /// The agent's plan, put aside while the fallback shell runs; a
    /// relaunch brings it back.
    agent_plan: Option<LaunchPlan>,
    /// When this launch, a resume, started on the agent clock. Cleared when
    /// YIEN itself stops the program, so that exit is never taken for the
    /// resume failing.
    resumed_at: Option<Duration>,
    /// A resume found failed, for the run loop to fall back on.
    /// Also the CLI's last line on screen, for the notice.
    failed_resume: Option<(ResumeFailure, Option<String>)>,
    /// This launch showed it is alive: the user typed into it, or a hook
    /// came with its token (Claude sends `SessionStart` once a resume
    /// works). Its exit is then the session ending, never a failed resume.
    launch_alive: bool,
    /// The workspace's number for the current launch, carried by the session
    /// reports so the workspace can drop those of a launch it replaced.
    launch: u64,
}

/// Starts the actor of a pane; it ends when `cmds` closes, terminating the
/// program. A panic in it is reported as [`PaneEvent::Crashed`].
pub(crate) fn spawn(
    setup: PaneSetup,
    ctx: Ctx,
    cmds: mpsc::Receiver<PaneCmd>,
    input: mpsc::Receiver<PaneInput>,
) {
    let (cols, rows) = setup.size;
    let engine = Arc::new(Mutex::new(new_engine(&ctx, setup.palette, cols, rows)));
    let mut machine = Machine::new(setup.plan.machine.clone());
    if setup.seen {
        machine = machine.step(&Signal::Seen(true), ctx.config.clock.now()).0;
    }
    let id = setup.id;
    let reports = Arc::clone(&ctx.reports);
    let hooks = setup.hooks;
    let waits = !(ctx.config.size_wait.is_zero() && ctx.config.ui_wait.is_zero());
    let pending = (setup.wait_for_size && waits).then(|| {
        let ui = ctx.ui_size.subscribe();
        let created = Instant::now();
        let ui_seen = ui.borrow().is_some().then_some(created);
        Pending {
            created,
            ui,
            ui_seen,
        }
    });
    let actor = Actor {
        id,
        plan: setup.plan,
        cwd: setup.cwd,
        ctx,
        slot: setup.slot,
        engine,
        session: None,
        machine,
        state: AgentState::Idle,
        outbox: Outbox::default(),
        retry_at: None,
        dirty: false,
        visible: setup.visible,
        last_frame: None,
        notice_owed: false,
        sync: None,
        wake_at: None,
        hooks_lost: setup.hooks_lost,
        hook_serial: None,
        hooks_missing: false,
        signal_lost: false,
        attention_since: None,
        asked: None,
        question: None,
        resume: setup.resume,
        palette: setup.palette,
        pending,
        fallback: setup.fallback,
        agent_plan: None,
        resumed_at: None,
        failed_resume: None,
        launch_alive: false,
        launch: setup.launch,
    };
    let task = tokio::spawn(actor.run(cmds, input, hooks));
    tokio::spawn(async move {
        if let Err(e) = task.await
            && e.is_panic()
        {
            let message = panic_message(e.into_panic().as_ref());
            tracing::error!(pane = id.get(), %message, "pane actor panicked");
            reports.push(id, PaneEvent::Crashed);
        }
    });
}

enum Wake {
    Cmd(Option<PaneCmd>),
    Input(PaneInput),
    Hook(HookItem),
    Reader(Option<ReaderMsg>),
    Noticed { owed: bool },
    UiSize,
    Timer,
}

async fn recv_reader(session: &mut Option<Session>) -> Option<ReaderMsg> {
    match session {
        Some(s) => s.reader.recv().await,
        None => std::future::pending().await,
    }
}

/// Resolves when the UI reports its first size while the pane waits for it.
async fn ui_size_reported(pending: &mut Option<Pending>) -> bool {
    match pending {
        Some(p) if p.ui_seen.is_none() => p.ui.changed().await.is_ok(),
        _ => std::future::pending().await,
    }
}

async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

impl Actor {
    async fn run(
        mut self,
        mut cmds: mpsc::Receiver<PaneCmd>,
        mut input: mpsc::Receiver<PaneInput>,
        mut hooks: mpsc::Receiver<HookItem>,
    ) {
        if self.pending.is_none() {
            self.start().await;
        }
        loop {
            let deadline = self.next_deadline();
            // At most one refused chunk is held; the rest waits in the
            // bounded input queue, where the handle sees it full.
            let take_input = self.outbox.input.is_empty();
            let take_output = self.outbox.replies.len() < REPLY_BACKLOG;
            let (slot, pane) = (&self.slot, self.id);
            // Commands first: input queued before a relaunch or terminate is
            // dropped with the old program, never written to it or the next.
            // Input sent before the first start waits for the program.
            let take_input = take_input && self.pending.is_none();
            let wake = tokio::select! {
                biased;
                cmd = cmds.recv() => Wake::Cmd(cmd),
                Some(item) = hooks.recv() => Wake::Hook(item),
                Some(bytes) = input.recv(), if take_input => Wake::Input(bytes),
                msg = recv_reader(&mut self.session), if take_output => Wake::Reader(msg),
                true = ui_size_reported(&mut self.pending) => Wake::UiSize,
                room = self.ctx.events.reserve(), if self.notice_owed => Wake::Noticed {
                    owed: match room {
                        Ok(permit) => slot.retry(|| {
                            permit.send(CoreMsg::PaneFrameReady { pane });
                            true
                        }),
                        Err(_) => false,
                    },
                },
                () = sleep_until(deadline) => Wake::Timer,
            };
            match wake {
                Wake::Cmd(None) => break,
                Wake::Cmd(Some(cmd)) => {
                    if matches!(cmd, PaneCmd::Relaunch { .. } | PaneCmd::Terminate) {
                        while input.try_recv().is_ok() {}
                        // Hooks of the old program must not reach the next.
                        while hooks.try_recv().is_ok() {}
                        self.hooks_lost.store(false, Ordering::Release);
                    }
                    self.on_cmd(cmd).await;
                }
                Wake::Input(input) => self.on_input(input),
                Wake::Hook(item) => self.on_hook(item, &mut hooks),
                Wake::Reader(Some(msg)) => self.on_reader(msg),
                Wake::Reader(None) => self.on_reader_gone(),
                Wake::Noticed { owed } => self.notice_owed = owed,
                Wake::UiSize => {
                    if let Some(p) = &mut self.pending {
                        p.ui_seen = Some(Instant::now());
                    }
                }
                Wake::Timer => {}
            }
            self.on_timers();
            if let Some((failure, said)) = self.failed_resume.take() {
                self.fall_back(failure, said).await;
            }
            if self.start_due().is_some_and(|at| at <= Instant::now()) {
                self.start_pending().await;
            }
        }
    }

    /// When a pane still waiting for its size starts anyway: soon after its
    /// creation when the UI is up, later while it has not reported anything.
    fn start_due(&self) -> Option<Instant> {
        let p = self.pending.as_ref()?;
        let config = &self.ctx.config;
        let soon = p.created + config.size_wait;
        let late = p.created + config.size_wait.max(config.ui_wait);
        Some(match p.ui_seen {
            Some(at) if at <= p.created => soon,
            Some(at) => (at + UI_SIZE_GRACE).clamp(soon, late),
            None => late,
        })
    }

    /// Starts a pane whose own size never came, at the last size the UI
    /// reported for any pane, or at the one it was created with.
    async fn start_pending(&mut self) {
        let Some(p) = self.pending.take() else {
            return;
        };
        self.resize_to_reported(&p);
        self.start().await;
    }

    /// Takes the last size the UI reported for any pane, if it has.
    fn resize_to_reported(&self, pending: &Pending) {
        let reported = *pending.ui.borrow();
        if let Some((cols, rows)) = reported {
            lock(&self.engine).resize(cols, rows);
        }
    }

    fn report(&self, event: PaneEvent) {
        self.ctx.reports.push(self.id, event);
    }

    fn now(&self) -> Duration {
        self.ctx.config.clock.now()
    }

    /// Starts a session on a fresh engine: modes, screen and pending sync
    /// updates of the last run do not leak into the next, and a reader still
    /// feeding the old engine cannot reach this one.
    async fn start(&mut self) {
        self.session = None;
        self.outbox.clear();
        self.retry_at = None;
        self.sync = None;
        let size = lock(&self.engine).size();
        self.engine = Arc::new(Mutex::new(new_engine(
            &self.ctx,
            self.palette,
            size.cols,
            size.rows,
        )));
        self.dirty = true;
        let resume = self.resume.take().filter(|_| self.plan.can_resume());
        self.resumed_at = resume.is_some().then(|| self.now());
        self.launch_alive = false;
        let session_id = if resume.is_some() {
            resume.clone()
        } else if self.plan.preassigns_session() {
            launch::new_uuid()
        } else {
            None
        };
        // A relaunch replaces the token, so the old program's hooks are refused.
        let hooks = self.ctx.hooks.clone().filter(|_| self.plan.hooks);
        let token = hooks.as_ref().and_then(|h| h.registry.issue(self.id).ok());
        self.hook_serial = token.as_ref().map(|t| t.serial());
        let fill = Fill {
            pane: self.id,
            session_id: session_id.as_deref(),
            resume: resume.as_deref(),
            hooks: hooks
                .as_deref()
                .zip(token.as_ref())
                .map(|(h, token)| HookFill {
                    settings_file: &h.settings_file,
                    pipe: &h.pipe,
                    token: token.as_str(),
                }),
        };
        self.hooks_missing = self.plan.hooks && fill.hooks.is_none();
        if let Some(id) = &session_id {
            self.report(PaneEvent::SessionId {
                id: id.clone(),
                launch: self.launch,
            });
        }
        let mut spawned = self.spawn(&fill, size.cols, size.rows).await;
        if let (Some(h), Ok(Err(PtyError::UnsafeBatchArgument { .. }))) = (&hooks, &spawned) {
            // The settings path has a character a batch file cannot be
            // given safely; the program still starts without it.
            h.registry.revoke(self.id);
            self.hook_serial = None;
            self.hooks_missing = true;
            let fill = Fill {
                hooks: None,
                ..fill
            };
            spawned = self.spawn(&fill, size.cols, size.rows).await;
            if matches!(spawned, Ok(Ok(_))) {
                self.report(PaneEvent::Notice(batch_without_hooks(&self.plan)));
            }
        }
        let session = match spawned {
            Ok(Ok(session)) => session,
            Ok(Err(e)) => {
                self.report(PaneEvent::Failed(spawn_message(&self.plan, &e)));
                self.report_signal_lost();
                return;
            }
            Err(_) => {
                self.report(PaneEvent::Failed(format!(
                    "Could not start {}. Relaunch the pane to try again.",
                    self.plan.program
                )));
                return;
            }
        };
        let (control, events) = session.into_parts();
        let (tx, reader) = mpsc::channel(READER_QUEUE);
        let engine = Arc::clone(&self.engine);
        let feed = move |bytes: &[u8]| {
            let mut engine = lock(&engine);
            let out = engine.feed(bytes);
            let sync = engine.sync_pending().then(|| engine.sync_generation());
            ReaderMsg::Output { out, sync }
        };
        let thread = std::thread::Builder::new()
            .name(format!("yien-pane-{}", self.id.get()))
            .spawn(move || read_loop(events, feed, tx));
        if thread.is_err() {
            self.report(PaneEvent::Failed(
                "Could not start the pane's reader. Close other panes and relaunch this one."
                    .to_owned(),
            ));
            return;
        }
        self.session = Some(Session {
            control,
            reader,
            exited: false,
        });
        self.report(PaneEvent::Started);
        self.step(&Signal::Spawned);
    }

    /// Spawns the plan filled with `fill`, off the runtime's threads, and
    /// registers the session for the quit path in the same step, so the
    /// shutdown never misses it. Once the app quits nothing is spawned.
    async fn spawn(
        &self,
        fill: &Fill<'_>,
        cols: u16,
        rows: u16,
    ) -> Result<Result<yien_pty::PtySession, PtyError>, tokio::task::JoinError> {
        let Some(spawning) = self.ctx.consoles.begin(self.id) else {
            return Ok(Err(PtyError::Closed));
        };
        let spec = self.plan.spec(fill, &self.cwd, cols, rows);
        let backend = Arc::clone(&self.ctx.backend);
        tokio::task::spawn_blocking(move || {
            let session = backend.spawn(spec);
            if let Ok(session) = &session {
                spawning.add(session.control().closer());
            }
            session
        })
        .await
    }

    /// Once the pipe has refused an event, the queued ones are older than
    /// the gap: stepping them after `SourceLost` could clear it and bring the
    /// red mark back from a stale state, so they are dropped with it.
    fn on_hook(&mut self, item: HookItem, queue: &mut mpsc::Receiver<HookItem>) {
        if self.hooks_lost.swap(false, Ordering::AcqRel) {
            while queue.try_recv().is_ok() {}
            self.step(&Signal::SourceLost);
            return;
        }
        if self.hook_serial != Some(item.serial) {
            tracing::debug!(pane = self.id.get(), "hook from an earlier launch dropped");
            return;
        }
        self.launch_alive = true;
        match item.event {
            Ok(event) => {
                if item.question.is_some() {
                    self.asked = item.question;
                } else if moves_on(&event) {
                    self.asked = None;
                }
                self.step(&Signal::Hook(event));
            }
            Err(_) => self.step(&Signal::Ambiguous),
        }
        self.report_question();
    }

    /// Reports the question when it changes: what the last asking hook
    /// said, only while the pane needs the user (RF-ATN-13).
    fn report_question(&mut self) {
        let question = if self.state.is_red() {
            self.asked.clone()
        } else {
            None
        };
        if question != self.question {
            self.question = question.clone();
            self.report(PaneEvent::Question(question));
        }
    }

    /// Reports the "signal lost" mark when it changes: the launch has no
    /// hook channel, or the machine lost or could not read its source.
    fn report_signal_lost(&mut self) {
        let lost = self.hooks_missing || self.machine.signal_lost();
        if lost != self.signal_lost {
            self.signal_lost = lost;
            self.report(PaneEvent::SignalLost(lost));
        }
    }

    async fn on_cmd(&mut self, cmd: PaneCmd) {
        match cmd {
            PaneCmd::Resize { cols, rows } => {
                let size = {
                    let mut engine = lock(&self.engine);
                    engine.resize(cols, rows);
                    engine.size()
                };
                self.outbox.resize = Some((size.cols, size.rows));
                self.dirty = true;
                self.flush();
                if self.pending.take().is_some() {
                    self.start().await;
                }
            }
            PaneCmd::Scroll(delta) => {
                lock(&self.engine).scroll(delta);
                self.dirty = true;
            }
            PaneCmd::ScrollToBottom => {
                lock(&self.engine).scroll_to_bottom();
                self.dirty = true;
            }
            PaneCmd::StartSelection { at, side, kind } => {
                lock(&self.engine).start_selection(at, side, kind);
                self.dirty = true;
            }
            PaneCmd::UpdateSelection { at, side } => {
                lock(&self.engine).update_selection(at, side);
                self.dirty = true;
            }
            PaneCmd::ClearSelection => {
                lock(&self.engine).clear_selection();
                self.dirty = true;
            }
            PaneCmd::Palette(palette) => {
                self.palette = palette;
                lock(&self.engine).set_palette(palette);
                self.dirty = true;
            }
            PaneCmd::CopySelection => {
                let text = lock(&self.engine).selection_text();
                self.report(PaneEvent::Selection(text));
            }
            PaneCmd::View { visible, seen } => {
                if visible && !self.visible {
                    self.dirty = true;
                    self.last_frame = None;
                }
                self.visible = visible;
                if seen != self.machine.seen() {
                    self.step(&Signal::Seen(seen));
                }
            }
            PaneCmd::Relaunch { launch } => {
                self.launch = launch;
                if let Some(agent) = self.agent_plan.take() {
                    self.plan = agent;
                    self.reset_machine();
                    self.report(PaneEvent::Standin(None));
                }
                // A relaunch starts a new session, even before the first start.
                if let Some(p) = self.pending.take() {
                    self.resume = None;
                    self.resize_to_reported(&p);
                }
                if let Some(old) = self.session.take() {
                    let _ = old.control.kill();
                }
                self.start().await;
            }
            PaneCmd::Terminate => {
                self.resumed_at = None;
                if self.pending.take().is_some() {
                    self.resume = None;
                    self.report(PaneEvent::Exited(None));
                    return;
                }
                if let Some(s) = &self.session {
                    let _ = s.control.kill();
                }
                // A reader held back by a full reply backlog must get through
                // to `Exited`.
                self.outbox.clear();
                self.retry_at = None;
            }
        }
    }

    fn on_input(&mut self, input: PaneInput) {
        if input.user {
            self.launch_alive |= self.session.is_some();
            self.step(&Signal::Input);
        }
        if self.session.is_some() && !input.bytes.is_empty() {
            self.outbox.input.push_back(input.bytes);
            self.flush();
        }
    }

    fn on_reader(&mut self, msg: ReaderMsg) {
        match msg {
            ReaderMsg::Output { out, sync } => {
                self.absorb(out);
                match sync {
                    Some(generation) if self.sync.map(|(g, _)| g) != Some(generation) => {
                        self.sync = Some((generation, Instant::now() + SYNC_UPDATE_TIMEOUT));
                    }
                    Some(_) => {}
                    None => self.sync = None,
                }
            }
            ReaderMsg::Exited(code) => {
                if let Some(s) = &mut self.session {
                    s.exited = true;
                }
                if let Some(failure) = self.resume_failure(code) {
                    self.failed_resume = Some(failure);
                    return;
                }
                self.step(&Signal::Exited {
                    code: convert::exit_code_to_agent(code),
                });
                self.report(PaneEvent::Exited(code));
            }
        }
    }

    /// Whether this exit ends a resume that failed. Reads the screen the
    /// program left, all of it fed before the exit was posted.
    fn resume_failure(&mut self, code: Option<u32>) -> Option<(ResumeFailure, Option<String>)> {
        let launched = self.resumed_at.take()?;
        if self.fallback.is_none() || self.launch_alive || self.ctx.consoles.is_shut() {
            return None;
        }
        let elapsed = self.now().saturating_sub(launched);
        let held = {
            let mut engine = lock(&self.engine);
            engine.sync_pending().then(|| engine.flush_sync())
        };
        if let Some(out) = held {
            self.sync = None;
            self.absorb(out);
        }
        let frame = lock(&self.engine).frame();
        let rows: Vec<String> = (0..frame.lines.len()).map(|r| frame.line_text(r)).collect();
        let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
        let failure = resume::resume_failed(code, elapsed, &rows)?;
        Some((failure, resume::last_words(&rows)))
    }

    /// Tells the user why the resume failed and opens the shell in the
    /// pane's folder (RF-SES-11). The workspace keeps the agent and its
    /// session for the saved session, so a restart tries the resume again.
    async fn fall_back(&mut self, failure: ResumeFailure, said: Option<String>) {
        let Some(shell) = self.fallback.take() else {
            return;
        };
        tracing::info!(
            pane = self.id.get(),
            reason = failure.kind(),
            "resume failed, the pane opens a shell"
        );
        self.report(PaneEvent::Notice(
            failure.notice(&self.plan.profile_name, said.as_deref()),
        ));
        if let Some(hooks) = &self.ctx.hooks {
            hooks.registry.revoke(self.id);
        }
        self.hook_serial = None;
        self.agent_plan = Some(std::mem::replace(&mut self.plan, shell));
        self.reset_machine();
        self.report(PaneEvent::Standin(Some((
            self.plan.default_name.clone(),
            self.plan.profile_name.clone(),
        ))));
        self.start().await;
    }

    /// A fresh state machine for the plan now running; a red mark of the
    /// program that ran before goes with it.
    fn reset_machine(&mut self) {
        let mut machine = Machine::new(self.plan.machine.clone());
        if self.machine.seen() {
            machine = machine.step(&Signal::Seen(true), self.now()).0;
        }
        self.machine = machine;
        self.attention_since = None;
        self.asked = None;
        if self.state != AgentState::Idle {
            self.state = AgentState::Idle;
            self.report(PaneEvent::State(AgentState::Idle, None));
        }
        self.report_question();
    }

    /// The reader ended. After `Exited` that is the end of the session; before
    /// it, the reader died and the program can no longer be shown or answered.
    fn on_reader_gone(&mut self) {
        let Some(session) = self.session.take() else {
            return;
        };
        self.outbox.clear();
        self.retry_at = None;
        if !session.exited {
            let _ = session.control.kill();
            self.step(&Signal::Exited { code: None });
            self.report(PaneEvent::Failed(READER_LOST.to_owned()));
        }
    }

    fn absorb(&mut self, out: VtOutput) {
        if !out.replies.is_empty() {
            self.outbox.replies.push_back(out.replies);
            self.flush();
        }
        if let Some(title) = out.title {
            self.report(PaneEvent::Title(match title {
                TitleChange::Set(t) => Some(t),
                TitleChange::Reset => None,
            }));
        }
        for signal in &out.signals {
            if let yien_vt::OscSignal::Cwd(dir) = signal {
                self.report(PaneEvent::Cwd(dir.clone()));
            }
            if let Some(osc) = convert::osc_signal(signal) {
                self.step(&Signal::Osc(osc));
            }
        }
        if out.bell {
            self.step(&Signal::Osc(yien_agent::OscSignal::Bell));
        }
        self.step(&Signal::Output);
        self.dirty = true;
    }

    /// Writes what the outbox holds, replies first, then the resize, then the
    /// input, until the PTY's queue refuses; refused bytes stay first in line
    /// for the retry.
    fn flush(&mut self) {
        let Some(session) = &self.session else {
            self.outbox.clear();
            self.retry_at = None;
            return;
        };
        let resizing = self.outbox.resize;
        let drained = self.outbox.drain(&session.control);
        if let Some((cols, rows)) = resizing
            && self.outbox.resize.is_none()
            && !matches!(drained, Err(Blocked::Closed))
        {
            tracing::debug!(pane = self.id.get(), cols, rows, "pseudo terminal resized");
        }
        self.retry_at = match drained {
            Ok(()) => None,
            Err(Blocked::Full) => Some(Instant::now() + RETRY_AFTER),
            Err(Blocked::Closed) => {
                self.outbox.clear();
                None
            }
        };
    }

    fn step(&mut self, signal: &Signal) {
        let now = self.now();
        let machine = std::mem::replace(&mut self.machine, Machine::new(self.plan.machine.clone()));
        let (machine, effects) = machine.step(signal, now);
        self.machine = machine;
        for effect in effects {
            match effect {
                AgentEffect::Transition { to, .. } => {
                    let state = convert::agent_state(to);
                    if state != self.state {
                        let kind = attention_kind(state);
                        self.attention_since = match self.attention_since {
                            _ if kind.is_none() => None,
                            Some(since) if kind == attention_kind(self.state) => Some(since),
                            _ => Some(self.ctx.attention.fetch_add(1, Ordering::Relaxed) + 1),
                        };
                        if self.state.is_red() && !state.is_red() {
                            self.asked = None;
                        }
                        self.state = state;
                        self.report(PaneEvent::State(state, self.attention_since));
                        self.report_question();
                    }
                }
                AgentEffect::CheckScreen => {
                    let frame = lock(&self.engine).frame();
                    let rows: Vec<String> =
                        (0..frame.lines.len()).map(|r| frame.line_text(r)).collect();
                    let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
                    let verdict = evaluate_screen(&self.plan.screen, &rows);
                    self.step(&Signal::Screen(verdict));
                }
                AgentEffect::SessionId(id) => self.report(PaneEvent::SessionId {
                    id,
                    launch: self.launch,
                }),
                // `/exit` and `/logout` are the user ending the session (RN-04);
                // `other` also comes when YIEN itself stops the program.
                AgentEffect::SessionEnded(
                    SessionEndReason::PromptInputExit | SessionEndReason::Logout,
                ) => self.report(PaneEvent::EndedByUser {
                    launch: self.launch,
                }),
                AgentEffect::MarkRed
                | AgentEffect::ClearRed
                | AgentEffect::Notify
                | AgentEffect::WakeAt(_)
                | AgentEffect::SessionEnded(_) => {}
            }
        }
        self.wake_at = self.machine.next_wake().map(|at| {
            let clock_now = self.now();
            Instant::now() + at.saturating_sub(clock_now)
        });
        self.report_signal_lost();
    }

    /// A pane that never drew is due at `now`, the caller's own instant: a
    /// fresh `Instant::now()` here would always land after it and never be due.
    fn frame_due(&self, now: Instant) -> Option<Instant> {
        if !(self.dirty && self.visible) {
            return None;
        }
        Some(match self.last_frame {
            Some(t) => t + self.ctx.config.frame_interval,
            None => now,
        })
    }

    fn next_deadline(&self) -> Option<Instant> {
        [
            self.frame_due(Instant::now()),
            self.sync.map(|(_, at)| at),
            self.retry_at,
            self.wake_at,
            self.start_due(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn on_timers(&mut self) {
        let now = Instant::now();
        if self.retry_at.is_some_and(|at| at <= now) {
            self.retry_at = None;
            if !self.outbox.is_empty() {
                self.flush();
            }
        }
        if let Some((generation, at)) = self.sync
            && at <= now
        {
            self.sync = None;
            let out = {
                let mut engine = lock(&self.engine);
                if engine.sync_pending() && engine.sync_generation() == generation {
                    Some(engine.flush_sync())
                } else if engine.sync_pending() {
                    self.sync = Some((engine.sync_generation(), now + SYNC_UPDATE_TIMEOUT));
                    None
                } else {
                    None
                }
            };
            if let Some(out) = out {
                self.absorb(out);
            }
        }
        if self.wake_at.is_some_and(|at| at <= now) {
            self.wake_at = None;
            self.step(&Signal::Tick);
        }
        if self.frame_due(now).is_some_and(|at| at <= now) {
            self.publish(now);
        }
    }

    /// Puts the current screen in the pane's slot and tells the UI, unless it
    /// was already told about a frame it has not taken. With the event queue
    /// full the notice is owed and sent as soon as there is room.
    fn publish(&mut self, now: Instant) {
        let frame = lock(&self.engine).frame();
        self.last_frame = Some(now);
        self.dirty = false;
        let (events, pane) = (&self.ctx.events, self.id);
        self.notice_owed = self.slot.put(frame, || {
            match events.try_send(CoreMsg::PaneFrameReady { pane }) {
                Ok(()) | Err(TrySendError::Closed(_)) => true,
                Err(TrySendError::Full(_)) => false,
            }
        });
    }
}

/// Longest panic message logged, in chars. Messages can quote runtime
/// strings, so they are capped to limit what reaches the log (plan §5.4).
const MAX_PANIC_CHARS: usize = 200;

/// The text of a panic payload, capped; a non-text payload is not logged.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    let text = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str));
    match text {
        Some(text) if text.chars().count() > MAX_PANIC_CHARS => {
            let mut cut: String = text.chars().take(MAX_PANIC_CHARS).collect();
            cut.push_str("...");
            cut
        }
        Some(text) => text.to_owned(),
        None => "<non-string payload>".to_owned(),
    }
}

enum Blocked {
    Full,
    Closed,
}

/// Writes `queue` in order until the PTY refuses; a refused chunk goes back
/// to the front.
fn write_all(control: &PtyControl, queue: &mut VecDeque<Vec<u8>>) -> Result<(), Blocked> {
    while let Some(bytes) = queue.pop_front() {
        match control.write(bytes) {
            Ok(()) => {}
            Err(PtyError::InputQueueFull(returned)) => {
                queue.push_front(returned.into_bytes());
                return Err(Blocked::Full);
            }
            Err(_) => return Err(Blocked::Closed),
        }
    }
    Ok(())
}

/// Why a pane started its batch-file program without the hook settings.
fn batch_without_hooks(plan: &LaunchPlan) -> String {
    format!(
        "{} is installed as a batch file (.cmd or .bat), and the path of YIEN's \
         hook settings has a character such a file cannot be given safely, \
         one of ( ) & ! ^ % \" | < >. It runs without the red mark. Installing \
         it as an .exe, or moving YIEN's data folder to a plain path, brings \
         the red mark back.",
        plan.program
    )
}

fn spawn_message(plan: &LaunchPlan, error: &PtyError) -> String {
    match error {
        PtyError::UnsafeBatchArgument { .. } => format!(
            "Could not start {}: it is a batch file (.cmd or .bat) and one of its \
             arguments has a character cmd.exe would interpret. Remove it from the \
             profile's arguments, or install the program as an .exe.",
            plan.program
        ),
        PtyError::Spawn { .. } => format!(
            "Could not start {}. Check that it is installed and on PATH, then relaunch the pane.",
            plan.program
        ),
        _ => format!(
            "Could not open a terminal for {}. Relaunch the pane to try again.",
            plan.program
        ),
    }
}

/// The pane's reader thread: feeds the engine and posts what it produced.
/// Blocks on the actor when it falls behind, which stalls the child instead
/// of dropping output. A panic in `feed` ends it like a closed PTY: the actor
/// sees the channel close before `Exited` and fails the pane.
fn read_loop(
    events: std::sync::mpsc::Receiver<PtyEvent>,
    mut feed: impl FnMut(&[u8]) -> ReaderMsg,
    tx: mpsc::Sender<ReaderMsg>,
) {
    use std::sync::mpsc::RecvTimeoutError;
    loop {
        let msg = match events.recv_timeout(READER_POLL) {
            Ok(PtyEvent::Output(bytes)) => match catch_unwind(AssertUnwindSafe(|| feed(&bytes))) {
                Ok(msg) => msg,
                Err(_) => return,
            },
            Ok(PtyEvent::Exited(status)) => ReaderMsg::Exited(status.code),
            Err(RecvTimeoutError::Timeout) => {
                if tx.is_closed() {
                    return;
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => return,
        };
        if tx.blocking_send(msg).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::sync_channel;

    use yien_pty::ExitStatus;

    use super::*;

    fn output(text: &str) -> ReaderMsg {
        ReaderMsg::Output {
            out: VtOutput {
                replies: text.as_bytes().to_vec(),
                ..VtOutput::default()
            },
            sync: None,
        }
    }

    fn reader(
        feed: impl FnMut(&[u8]) -> ReaderMsg + Send + 'static,
    ) -> (
        std::sync::mpsc::SyncSender<PtyEvent>,
        mpsc::Receiver<ReaderMsg>,
        std::thread::JoinHandle<()>,
    ) {
        let (events_tx, events) = sync_channel(8);
        let (tx, rx) = mpsc::channel(8);
        let thread = std::thread::spawn(move || read_loop(events, feed, tx));
        (events_tx, rx, thread)
    }

    #[test]
    fn the_reader_posts_output_then_the_exit() {
        let (events, mut rx, thread) = reader(|b| output(std::str::from_utf8(b).unwrap()));
        events.send(PtyEvent::Output(b"hi".to_vec())).unwrap();
        events
            .send(PtyEvent::Exited(ExitStatus::from_code(4)))
            .unwrap();
        assert!(matches!(
            rx.blocking_recv(),
            Some(ReaderMsg::Output { out, .. }) if out.replies == b"hi"
        ));
        assert!(matches!(
            rx.blocking_recv(),
            Some(ReaderMsg::Exited(Some(4)))
        ));
        drop(events);
        assert!(rx.blocking_recv().is_none());
        thread.join().unwrap();
    }

    #[test]
    fn a_disconnected_pty_closes_the_reader_without_an_exit() {
        let (events, mut rx, thread) = reader(|_| output(""));
        drop(events);
        assert!(rx.blocking_recv().is_none());
        thread.join().unwrap();
    }

    #[test]
    fn a_panic_while_feeding_closes_the_reader_without_an_exit() {
        let (events, mut rx, thread) = reader(|_| panic!("engine bug"));
        events.send(PtyEvent::Output(b"boom".to_vec())).unwrap();
        events.send(PtyEvent::Exited(ExitStatus::from_code(0))).ok();
        assert!(rx.blocking_recv().is_none());
        thread.join().unwrap();
    }

    #[test]
    fn panic_messages_are_capped_and_only_text_is_kept() {
        assert_eq!(panic_message(&"boom"), "boom");
        assert_eq!(panic_message(&String::from("boom")), "boom");
        assert_eq!(panic_message(&42_u32), "<non-string payload>");
        let cut = panic_message(&"é".repeat(MAX_PANIC_CHARS + 9));
        assert_eq!(cut.chars().count(), MAX_PANIC_CHARS + 3);
        assert!(cut.ends_with("..."));
    }

    /// A Claude pane actor on a fake PTY. Its hooks have a registry and no
    /// pipe, so the test hands them to the actor and the "signal lost" mark
    /// comes from the state machine alone. The first launch's token has
    /// serial 1, each relaunch's the next.
    struct Solo {
        runtime: tokio::runtime::Runtime,
        reports: Arc<crate::report::Reports>,
        cmds: mpsc::Sender<PaneCmd>,
        hooks: mpsc::Sender<HookItem>,
        hooks_lost: Arc<AtomicBool>,
        _events: mpsc::Receiver<CoreMsg>,
        _input: mpsc::Sender<PaneInput>,
    }

    impl Solo {
        /// Starts the actor with `queued` already waiting in its hook queue.
        fn start(queued: Vec<HookItem>, lost: bool) -> Self {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_time()
                .build()
                .unwrap();
            let config = crate::CoreConfig::default();
            let claude = yien_proto::ProfileId::new("claude");
            let plan = launch::resolve(Some(&claude), &config.profiles, &config.shell).unwrap();
            let (events, events_rx) = mpsc::channel(64);
            let ctx = Ctx {
                backend: Arc::new(yien_pty::FakePty::new()),
                config: Arc::new(config),
                events,
                reports: Arc::default(),
                engine: alacritty,
                hooks: Some(Arc::new(crate::hooks::Hooks::detached())),
                attention: Arc::default(),
                ui_size: Arc::new(watch::Sender::new(None)),
                consoles: Arc::default(),
            };
            let reports = Arc::clone(&ctx.reports);
            let (hooks, hooks_rx) = mpsc::channel(crate::service::PANE_HOOK_QUEUE);
            for item in queued {
                hooks.try_send(item).unwrap();
            }
            let hooks_lost = Arc::new(AtomicBool::new(lost));
            let (cmds, cmds_rx) = mpsc::channel(8);
            let (input, input_rx) = mpsc::channel(8);
            let setup = PaneSetup {
                id: PaneId::new(1),
                plan,
                cwd: ".".to_owned(),
                size: (20, 4),
                slot: Arc::default(),
                visible: false,
                seen: false,
                hooks: hooks_rx,
                hooks_lost: Arc::clone(&hooks_lost),
                resume: None,
                fallback: None,
                wait_for_size: false,
                palette: Palette::default(),
                launch: 0,
            };
            runtime.block_on(async { spawn(setup, ctx, cmds_rx, input_rx) });
            Self {
                runtime,
                reports,
                cmds,
                hooks,
                hooks_lost,
                _events: events_rx,
                _input: input,
            }
        }

        /// Everything the actor reports within `window`.
        fn collect(&self, window: Duration) -> Vec<PaneEvent> {
            let mut seen = Vec::new();
            let end = Instant::now() + window;
            self.runtime.block_on(async {
                while let Ok(batch) = tokio::time::timeout_at(end, self.reports.next()).await {
                    seen.extend(batch.into_values().flatten());
                }
            });
            seen
        }
    }

    fn hook(serial: u64, event: yien_agent::HookEvent) -> HookItem {
        HookItem {
            serial,
            event: Ok(event),
            question: None,
        }
    }

    fn asking(serial: u64, event: yien_agent::HookEvent, question: &str) -> HookItem {
        HookItem {
            question: Some(PendingQuestion::new(question)),
            ..hook(serial, event)
        }
    }

    /// The last question reported, if any was.
    fn last_question(events: &[PaneEvent]) -> Option<Option<String>> {
        events.iter().rev().find_map(|e| match e {
            PaneEvent::Question(q) => Some(q.as_ref().map(|q| q.as_str().to_owned())),
            _ => None,
        })
    }

    fn last_since(events: &[PaneEvent]) -> Option<Option<u64>> {
        events.iter().rev().find_map(|e| match e {
            PaneEvent::State(_, since) => Some(*since),
            _ => None,
        })
    }

    fn last_state(events: &[PaneEvent]) -> Option<AgentState> {
        events.iter().rev().find_map(|e| match e {
            PaneEvent::State(state, _) => Some(*state),
            _ => None,
        })
    }

    fn last_signal_lost(events: &[PaneEvent]) -> Option<bool> {
        events.iter().rev().find_map(|e| match e {
            PaneEvent::SignalLost(lost) => Some(*lost),
            _ => None,
        })
    }

    /// Longer than the "needs you" confirmation, so a red mark would show.
    const SETTLE: Duration = Duration::from_millis(800);

    #[test]
    fn hooks_queued_before_a_refused_one_are_dropped_with_it() {
        use yien_agent::HookEvent::{PermissionRequest, UserPromptSubmit};
        let queued = vec![
            hook(1, UserPromptSubmit),
            hook(1, UserPromptSubmit),
            hook(1, PermissionRequest),
        ];
        let pane = Solo::start(queued, true);
        let events = pane.collect(SETTLE);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, PaneEvent::State(AgentState::NeedsYou(_), _))),
            "{events:?}"
        );
        assert!(
            matches!(last_state(&events), None | Some(AgentState::Idle)),
            "{events:?}"
        );
        assert_eq!(last_signal_lost(&events), Some(true), "{events:?}");
    }

    #[test]
    fn a_hook_after_the_refused_ones_is_read_again() {
        use yien_agent::HookEvent::{PermissionRequest, UserPromptSubmit};
        let pane = Solo::start(vec![hook(1, UserPromptSubmit)], true);
        assert_eq!(last_signal_lost(&pane.collect(SETTLE)), Some(true));
        assert!(!pane.hooks_lost.load(Ordering::Acquire));
        pane.hooks.try_send(hook(1, UserPromptSubmit)).unwrap();
        pane.hooks.try_send(hook(1, PermissionRequest)).unwrap();
        let events = pane.collect(SETTLE);
        assert!(
            matches!(last_state(&events), Some(AgentState::NeedsYou(_))),
            "{events:?}"
        );
        drop(pane.cmds);
    }

    #[test]
    fn rf_atn_13_a_red_shows_its_question_and_a_later_red_without_one_shows_none() {
        use yien_agent::HookEvent::{
            Notification, PermissionRequest, PostToolUse, UserPromptSubmit,
        };
        use yien_agent::NotificationKind::PermissionPrompt;
        let pane = Solo::start(Vec::new(), false);
        pane.hooks.try_send(hook(1, UserPromptSubmit)).unwrap();
        pane.hooks
            .try_send(asking(1, PermissionRequest, "Permission to use Bash"))
            .unwrap();
        let events = pane.collect(SETTLE);
        assert!(
            matches!(last_state(&events), Some(AgentState::NeedsYou(_))),
            "{events:?}"
        );
        assert_eq!(
            last_question(&events),
            Some(Some("Permission to use Bash".to_owned()))
        );
        // The notification that follows the request says it better.
        let note = asking(
            1,
            Notification(PermissionPrompt),
            "Claude needs your permission",
        );
        pane.hooks.try_send(note).unwrap();
        let events = pane.collect(SETTLE);
        assert_eq!(
            last_question(&events),
            Some(Some("Claude needs your permission".to_owned()))
        );
        // Answered: the red and its question go.
        pane.hooks.try_send(hook(1, PostToolUse)).unwrap();
        let events = pane.collect(SETTLE);
        assert_eq!(last_state(&events), Some(AgentState::Working), "{events:?}");
        assert_eq!(last_question(&events), Some(None));
        // A new red whose hooks said nothing shows no old text.
        pane.hooks.try_send(hook(1, PermissionRequest)).unwrap();
        let events = pane.collect(SETTLE);
        assert!(
            matches!(last_state(&events), Some(AgentState::NeedsYou(_))),
            "{events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, PaneEvent::Question(Some(_)))),
            "{events:?}"
        );
        drop(pane.cmds);
    }

    #[test]
    fn rf_atn_14_a_finished_turn_gets_a_place_after_an_earlier_red() {
        use yien_agent::HookEvent::{PermissionRequest, PostToolUse, Stop, UserPromptSubmit};
        let pane = Solo::start(Vec::new(), false);
        pane.hooks.try_send(hook(1, UserPromptSubmit)).unwrap();
        pane.hooks.try_send(hook(1, PermissionRequest)).unwrap();
        let red = last_since(&pane.collect(SETTLE)).flatten().unwrap();
        pane.hooks.try_send(hook(1, PostToolUse)).unwrap();
        pane.hooks.try_send(hook(1, Stop)).unwrap();
        let events = pane.collect(SETTLE);
        assert_eq!(last_state(&events), Some(AgentState::Done), "{events:?}");
        let done = last_since(&events).flatten().unwrap();
        assert!(done > red, "{red} then {done}");
        drop(pane.cmds);
    }

    #[test]
    fn a_hook_from_before_a_relaunch_does_not_reach_the_new_program() {
        use yien_agent::HookEvent::{PermissionRequest, SessionStart, UserPromptSubmit};
        use yien_agent::SessionStartSource;
        let pane = Solo::start(Vec::new(), false);
        pane.collect(Duration::from_millis(100));
        pane.cmds.try_send(PaneCmd::Relaunch { launch: 1 }).unwrap();
        let relaunched = pane.collect(Duration::from_millis(200));
        assert!(relaunched.contains(&PaneEvent::Started), "{relaunched:?}");
        assert!(
            relaunched
                .iter()
                .any(|e| matches!(e, PaneEvent::SessionId { launch: 1, .. })),
            "{relaunched:?}"
        );
        let stale = "11111111-1111-4111-8111-111111111111".to_owned();
        let session = SessionStart {
            session_id: Some(stale.clone()),
            source: SessionStartSource::Resume,
        };
        for event in [session, UserPromptSubmit, PermissionRequest] {
            pane.hooks.try_send(hook(1, event)).unwrap();
        }
        let events = pane.collect(SETTLE);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, PaneEvent::SessionId { id, .. } if *id == stale)),
            "{events:?}"
        );
        assert!(
            matches!(last_state(&events), None | Some(AgentState::Idle)),
            "{events:?}"
        );
        pane.hooks.try_send(hook(2, UserPromptSubmit)).unwrap();
        pane.hooks.try_send(hook(2, PermissionRequest)).unwrap();
        let events = pane.collect(SETTLE);
        assert!(
            matches!(last_state(&events), Some(AgentState::NeedsYou(_))),
            "{events:?}"
        );
    }

    #[test]
    fn rn_04_a_hook_session_names_the_launch_it_came_from() {
        use yien_agent::HookEvent::SessionStart;
        use yien_agent::SessionStartSource;
        let pane = Solo::start(Vec::new(), false);
        pane.collect(Duration::from_millis(100));
        pane.cmds.try_send(PaneCmd::Relaunch { launch: 4 }).unwrap();
        pane.collect(Duration::from_millis(200));
        let id = "22222222-2222-4222-8222-222222222222".to_owned();
        let session = SessionStart {
            session_id: Some(id.clone()),
            source: SessionStartSource::Clear,
        };
        pane.hooks.try_send(hook(2, session)).unwrap();
        let events = pane.collect(SETTLE);
        assert!(
            events.contains(&PaneEvent::SessionId { id, launch: 4 }),
            "{events:?}"
        );
    }

    #[test]
    fn a_notice_is_sent_once_per_taken_frame() {
        let slot = FrameSlot::default();
        let frame = alacritty(&VtConfig::default(), 4, 1).frame();
        let mut sent = 0;
        assert!(!slot.put(frame.clone(), || {
            sent += 1;
            true
        }));
        assert!(!slot.put(frame.clone(), || unreachable!("already told")));
        assert!(slot.take().is_some());
        assert!(slot.take().is_none());
        assert!(!slot.put(frame, || {
            sent += 1;
            true
        }));
        assert_eq!(sent, 2);
    }

    #[test]
    fn a_refused_notice_stays_owed_until_it_gets_through() {
        let slot = FrameSlot::default();
        let frame = alacritty(&VtConfig::default(), 4, 1).frame();
        assert!(slot.put(frame, || false));
        assert!(slot.retry(|| false));
        assert!(!slot.retry(|| true));
        assert!(!slot.retry(|| unreachable!("already told")));
        assert!(slot.take().is_some());
        assert!(!slot.retry(|| unreachable!("nothing to tell")));
    }
}
