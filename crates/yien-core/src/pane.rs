//! One actor per pane: the only owner of its terminal engine and agent state
//! machine (ADR-06). A blocking reader thread feeds the engine from the PTY;
//! the actor writes to the PTY, publishes frames and runs the state machine.

use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio::time::Instant;
use yien_agent::{Effect as AgentEffect, Machine, Signal, evaluate_screen};
use yien_proto::frame::{Frame, SelectionKind, Side, ViewportPoint};
use yien_proto::{AgentState, ClientMsg, CoreMsg, InputBytes, PaneId};
use yien_pty::{PtyControl, PtyError, PtyEvent};
use yien_vt::{AlacrittyEngine, SYNC_UPDATE_TIMEOUT, TitleChange, VtConfig, VtEngine, VtOutput};

use crate::convert;
use crate::launch::{self, Fill, HookFill, LaunchPlan};
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
    View {
        visible: bool,
        seen: bool,
    },
    Relaunch,
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

enum ReaderMsg {
    Output { out: VtOutput, sync: Option<u64> },
    Exited(Option<u32>),
}

type Engine = Arc<Mutex<Box<dyn VtEngine>>>;

fn lock(engine: &Engine) -> MutexGuard<'_, Box<dyn VtEngine>> {
    engine.lock().unwrap_or_else(PoisonError::into_inner)
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
    /// The profile reports through hooks but this launch has no channel.
    hooks_missing: bool,
    /// The "signal lost" mark last reported.
    signal_lost: bool,
    /// When the pane entered "needs you", while it is there (RF-ATN-05).
    needs_since: Option<u64>,
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
    let engine = Arc::new(Mutex::new((ctx.engine)(&ctx.config.vt, cols, rows)));
    let mut machine = Machine::new(setup.plan.machine.clone());
    if setup.seen {
        machine = machine.step(&Signal::Seen(true), ctx.config.clock.now()).0;
    }
    let id = setup.id;
    let reports = Arc::clone(&ctx.reports);
    let hooks = setup.hooks;
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
        hooks_missing: false,
        signal_lost: false,
        needs_since: None,
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
    Timer,
}

async fn recv_reader(session: &mut Option<Session>) -> Option<ReaderMsg> {
    match session {
        Some(s) => s.reader.recv().await,
        None => std::future::pending().await,
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
        self.start().await;
        loop {
            let deadline = self.next_deadline();
            // At most one refused chunk is held; the rest waits in the
            // bounded input queue, where the handle sees it full.
            let take_input = self.outbox.input.is_empty();
            let take_output = self.outbox.replies.len() < REPLY_BACKLOG;
            let (slot, pane) = (&self.slot, self.id);
            // Commands first: input queued before a relaunch or terminate is
            // dropped with the old program, never written to it or the next.
            let wake = tokio::select! {
                biased;
                cmd = cmds.recv() => Wake::Cmd(cmd),
                Some(item) = hooks.recv() => Wake::Hook(item),
                Some(bytes) = input.recv(), if take_input => Wake::Input(bytes),
                msg = recv_reader(&mut self.session), if take_output => Wake::Reader(msg),
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
                    if matches!(cmd, PaneCmd::Relaunch | PaneCmd::Terminate) {
                        while input.try_recv().is_ok() {}
                        // Hooks of the old program must not reach the next.
                        while hooks.try_recv().is_ok() {}
                        self.hooks_lost.store(false, Ordering::Release);
                    }
                    self.on_cmd(cmd).await;
                }
                Wake::Input(input) => self.on_input(input),
                Wake::Hook(item) => self.on_hook(item),
                Wake::Reader(Some(msg)) => self.on_reader(msg),
                Wake::Reader(None) => self.on_reader_gone(),
                Wake::Noticed { owed } => self.notice_owed = owed,
                Wake::Timer => {}
            }
            self.on_timers();
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
        self.engine = Arc::new(Mutex::new((self.ctx.engine)(
            &self.ctx.config.vt,
            size.cols,
            size.rows,
        )));
        self.dirty = true;
        let session_id = if self.plan.preassigns_session() {
            launch::new_uuid()
        } else {
            None
        };
        // A relaunch replaces the token, so the old program's hooks are refused.
        let hooks = self.ctx.hooks.as_deref().filter(|_| self.plan.hooks);
        let token = hooks.and_then(|h| h.registry.issue(self.id).ok());
        let fill = Fill {
            pane: self.id,
            session_id: session_id.as_deref(),
            hooks: hooks.zip(token.as_ref()).map(|(h, token)| HookFill {
                settings_file: &h.settings_file,
                pipe: &h.pipe,
                token: token.as_str(),
            }),
        };
        self.hooks_missing = self.plan.hooks && fill.hooks.is_none();
        let spec = self.plan.spec(&fill, &self.cwd, size.cols, size.rows);
        if let Some(id) = session_id {
            self.report(PaneEvent::SessionId(id));
        }
        let backend = Arc::clone(&self.ctx.backend);
        let spawned = tokio::task::spawn_blocking(move || backend.spawn(spec)).await;
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

    fn on_hook(&mut self, item: HookItem) {
        match item {
            Ok(event) => self.step(&Signal::Hook(event)),
            Err(_) => self.step(&Signal::Ambiguous),
        }
        if self.hooks_lost.swap(false, Ordering::AcqRel) {
            self.step(&Signal::SourceLost);
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
            PaneCmd::Relaunch => {
                if let Some(old) = self.session.take() {
                    let _ = old.control.kill();
                }
                self.start().await;
            }
            PaneCmd::Terminate => {
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
                self.step(&Signal::Exited {
                    code: convert::exit_code_to_agent(code),
                });
                self.report(PaneEvent::Exited(code));
            }
        }
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
        self.retry_at = match self.outbox.drain(&session.control) {
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
                        self.needs_since = match (state.is_red(), self.needs_since) {
                            (false, _) => None,
                            (true, Some(since)) => Some(since),
                            (true, None) => {
                                Some(self.ctx.attention.fetch_add(1, Ordering::Relaxed) + 1)
                            }
                        };
                        self.state = state;
                        self.report(PaneEvent::State(state, self.needs_since));
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
                AgentEffect::SessionId(id) => self.report(PaneEvent::SessionId(id)),
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

fn spawn_message(plan: &LaunchPlan, error: &PtyError) -> String {
    match error {
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
