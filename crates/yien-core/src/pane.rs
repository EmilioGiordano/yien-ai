//! One actor per pane: the only owner of its terminal engine and agent state
//! machine (ADR-06). A blocking reader thread feeds the engine from the PTY;
//! the actor writes to the PTY, coalesces frames and runs the state machine.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::Instant;
use yien_agent::{Effect as AgentEffect, Machine, Signal, evaluate_screen};
use yien_proto::frame::{SelectionKind, Side, ViewportPoint};
use yien_proto::{AgentState, CoreMsg, PaneId};
use yien_pty::{PtyControl, PtyError, PtyEvent};
use yien_vt::{AlacrittyEngine, SYNC_UPDATE_TIMEOUT, TitleChange, VtEngine, VtOutput};

use crate::convert;
use crate::launch::LaunchPlan;
use crate::service::Ctx;
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

/// What the service asks of a pane actor.
#[derive(Debug)]
pub(crate) enum PaneCmd {
    Write {
        bytes: Vec<u8>,
        user: bool,
    },
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

enum ReaderMsg {
    Output { out: VtOutput, sync: Option<u64> },
    Exited(Option<u32>),
}

type Engine = Arc<Mutex<AlacrittyEngine>>;

fn lock(engine: &Engine) -> MutexGuard<'_, AlacrittyEngine> {
    engine.lock().unwrap_or_else(PoisonError::into_inner)
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
}

struct Actor {
    id: PaneId,
    plan: LaunchPlan,
    cwd: String,
    ctx: Ctx,
    report: mpsc::UnboundedSender<(PaneId, PaneEvent)>,
    engine: Engine,
    session: Option<Session>,
    machine: Machine,
    state: AgentState,
    outbox: Outbox,
    retry_at: Option<Instant>,
    dirty: bool,
    visible: bool,
    last_frame: Option<Instant>,
    sync: Option<(u64, Instant)>,
    wake_at: Option<Instant>,
}

/// Starts the actor of `pane`; it ends when `cmds` closes, terminating the
/// program.
pub(crate) fn spawn(
    id: PaneId,
    plan: LaunchPlan,
    cwd: String,
    ctx: Ctx,
    cmds: mpsc::UnboundedReceiver<PaneCmd>,
    report: mpsc::UnboundedSender<(PaneId, PaneEvent)>,
) {
    let (cols, rows) = ctx.config.initial_size;
    let engine = Arc::new(Mutex::new(AlacrittyEngine::new(
        ctx.config.vt.clone(),
        cols,
        rows,
    )));
    let machine = Machine::new(plan.machine.clone());
    let actor = Actor {
        id,
        plan,
        cwd,
        ctx,
        report: report.clone(),
        engine,
        session: None,
        machine,
        state: AgentState::Idle,
        outbox: Outbox::default(),
        retry_at: None,
        dirty: false,
        visible: false,
        last_frame: None,
        sync: None,
        wake_at: None,
    };
    let task = tokio::spawn(actor.run(cmds));
    tokio::spawn(async move {
        if let Err(e) = task.await
            && e.is_panic()
        {
            let _ = report.send((id, PaneEvent::Crashed));
        }
    });
}

enum Wake {
    Cmd(Option<PaneCmd>),
    Reader(Option<ReaderMsg>),
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
    async fn run(mut self, mut cmds: mpsc::UnboundedReceiver<PaneCmd>) {
        self.start().await;
        loop {
            let deadline = self.next_deadline();
            let wake = tokio::select! {
                cmd = cmds.recv() => Wake::Cmd(cmd),
                msg = recv_reader(&mut self.session) => Wake::Reader(msg),
                () = sleep_until(deadline) => Wake::Timer,
            };
            match wake {
                Wake::Cmd(None) => break,
                Wake::Cmd(Some(cmd)) => self.on_cmd(cmd).await,
                Wake::Reader(Some(msg)) => self.on_reader(msg),
                Wake::Reader(None) => self.on_reader_gone(),
                Wake::Timer => {}
            }
            self.on_timers();
        }
    }

    fn send(&self, event: PaneEvent) {
        let _ = self.report.send((self.id, event));
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
        self.engine = Arc::new(Mutex::new(AlacrittyEngine::new(
            self.ctx.config.vt.clone(),
            size.cols,
            size.rows,
        )));
        self.dirty = true;
        let spec = self.plan.spec(self.id, &self.cwd, size.cols, size.rows);
        let backend = Arc::clone(&self.ctx.backend);
        let spawned = tokio::task::spawn_blocking(move || backend.spawn(spec)).await;
        let session = match spawned {
            Ok(Ok(session)) => session,
            Ok(Err(e)) => {
                self.send(PaneEvent::SpawnFailed(spawn_message(&self.plan, &e)));
                return;
            }
            Err(_) => {
                self.send(PaneEvent::SpawnFailed(format!(
                    "Could not start {}. Relaunch the pane to try again.",
                    self.plan.program
                )));
                return;
            }
        };
        let (control, events) = session.into_parts();
        let (tx, reader) = mpsc::channel(READER_QUEUE);
        let engine = Arc::clone(&self.engine);
        let thread = std::thread::Builder::new()
            .name(format!("yien-pane-{}", self.id.get()))
            .spawn(move || read_loop(events, engine, tx));
        if thread.is_err() {
            self.send(PaneEvent::SpawnFailed(
                "Could not start the pane's reader. Close other panes and relaunch this one."
                    .to_owned(),
            ));
            return;
        }
        self.session = Some(Session { control, reader });
        self.send(PaneEvent::Started);
        self.step(&Signal::Spawned);
    }

    async fn on_cmd(&mut self, cmd: PaneCmd) {
        match cmd {
            PaneCmd::Write { bytes, user } => {
                if user {
                    self.step(&Signal::Input);
                }
                if self.session.is_some() && !bytes.is_empty() {
                    self.outbox.input.push_back(bytes);
                    self.flush();
                }
            }
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
                let _ = self
                    .ctx
                    .events
                    .send(CoreMsg::SelectionText {
                        pane: self.id,
                        text,
                    })
                    .await;
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
            }
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
                self.step(&Signal::Exited {
                    code: convert::exit_code_to_agent(code),
                });
                self.send(PaneEvent::Exited(code));
            }
        }
    }

    fn on_reader_gone(&mut self) {
        if let Some(s) = self.session.take() {
            drop(s);
            self.outbox.clear();
            self.retry_at = None;
        }
    }

    fn absorb(&mut self, out: VtOutput) {
        if !out.replies.is_empty() {
            self.outbox.replies.push_back(out.replies);
            self.flush();
        }
        if let Some(title) = out.title {
            self.send(PaneEvent::Title(match title {
                TitleChange::Set(t) => Some(t),
                TitleChange::Reset => None,
            }));
        }
        for signal in &out.signals {
            if let yien_vt::OscSignal::Cwd(dir) = signal {
                self.send(PaneEvent::Cwd(dir.clone()));
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
                        self.state = state;
                        self.send(PaneEvent::State(state));
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
                AgentEffect::SessionId(id) => self.send(PaneEvent::SessionId(id)),
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
    }

    fn frame_due(&self) -> Option<Instant> {
        if !(self.dirty && self.visible) {
            return None;
        }
        Some(match self.last_frame {
            Some(t) => t + self.ctx.config.frame_interval,
            None => Instant::now(),
        })
    }

    fn next_deadline(&self) -> Option<Instant> {
        [
            self.frame_due(),
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
        if self.frame_due().is_some_and(|at| at <= now) {
            let frame = lock(&self.engine).frame();
            self.last_frame = Some(now);
            match self.ctx.events.try_send(CoreMsg::PaneFrame {
                pane: self.id,
                frame,
            }) {
                Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => self.dirty = false,
                Err(mpsc::error::TrySendError::Full(_)) => {}
            }
        }
    }
}

/// Why the outbox stopped writing.
enum Blocked {
    /// The PTY's queue is full; try again later.
    Full,
    /// The session is gone; nothing will be written.
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
/// of dropping output.
fn read_loop(
    events: std::sync::mpsc::Receiver<PtyEvent>,
    engine: Engine,
    tx: mpsc::Sender<ReaderMsg>,
) {
    use std::sync::mpsc::RecvTimeoutError;
    loop {
        let msg = match events.recv_timeout(READER_POLL) {
            Ok(PtyEvent::Output(bytes)) => {
                let mut engine = lock(&engine);
                let out = engine.feed(&bytes);
                let sync = engine.sync_pending().then(|| engine.sync_generation());
                ReaderMsg::Output { out, sync }
            }
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
