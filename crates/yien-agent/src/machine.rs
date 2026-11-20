//! The pure agent state machine (04 §4.5): `(state, signal, now) -> (state,
//! effects)`. No clock, no I/O; the caller executes the effects.

use std::time::Duration;

use crate::signal::{
    AgentStatus, BlockedKind, HookEvent, NotificationKind, OscSignal, PromptMark, ScreenVerdict,
    SessionStartSource, Signal, Source,
};

/// How long a "needs you" signal must stand without a contrary one before the
/// red mark goes on (04 §4.5, hysteresis). Leaving red is immediate.
pub const NEEDS_YOU_CONFIRM: Duration = Duration::from_millis(300);

/// How long a program must stay quiet before its screen is read for a
/// startup screen, such as folder trust or sign-in, that waits for the user.
pub const STARTUP_SCREEN_SETTLE: Duration = NEEDS_YOU_CONFIRM;

/// The longest a startup screen goes unread while output keeps coming: an
/// animated screen (Codex's sign-in shimmer redraws every 100 ms) never
/// settles.
pub const STARTUP_SCREEN_MAX_WAIT: Duration = Duration::from_secs(1);

/// How long after a turn ended a failure its agent's service reports still
/// belongs to it: OpenCode ends a failed turn with `idle` on the terminal,
/// and the service's event can come before or after that.
pub const SERVICE_ERROR_GRACE: Duration = Duration::from_secs(5);

/// Why an agent needs the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NeedsReason {
    /// A permission prompt.
    Permission,
    /// A question or an input request.
    Question,
    /// The user must sign in again.
    Auth,
    /// OSC 7501 `blocked` with no kind.
    Blocked,
    /// A screen rule confirmed it, or matched a startup screen.
    ScreenRule,
}

/// Why an agent is in error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorReason {
    /// The turn ended on an API error (`StopFailure`).
    TurnFailed,
    /// The process exited in the middle of a turn.
    UnexpectedExit {
        /// Its exit code, when there is one.
        code: Option<i32>,
    },
}

/// The state of the agent in one pane (RF-AGE-05).
///
/// ```
/// use yien_agent::{AgentState, NeedsReason};
///
/// assert!(AgentState::NeedsYou(NeedsReason::Permission).is_red());
/// assert!(!AgentState::Done.is_red());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    /// Inactive, and everything that cannot be determined (RN-02).
    Idle,
    /// A turn is running.
    Working,
    /// The agent waits for the user; the only red state (RN-01).
    NeedsYou(NeedsReason),
    /// A turn completed while the pane was not seen (RF-ATN-08).
    Done,
    /// The turn or the process failed.
    Error(ErrorReason),
}

impl AgentState {
    /// Whether this state carries the red mark (RN-01).
    pub fn is_red(&self) -> bool {
        matches!(self, AgentState::NeedsYou(_))
    }
}

/// What caused a transition, for the transition log (RF-CFG-06).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// A detection source.
    Source(Source),
    /// The process started or exited.
    Process,
    /// The user: the pane was seen, or input was followed by output.
    User,
    /// The agent's own service reported a failed turn.
    Service,
    /// The structured channel was lost.
    SignalLost,
    /// A signal could not be interpreted.
    Ambiguous,
}

/// Something the caller must do after a step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Record the transition (RF-CFG-06) and publish the new state.
    Transition {
        /// The state before.
        from: AgentState,
        /// The state after.
        to: AgentState,
        /// What caused it.
        cause: Cause,
    },
    /// Turn the red mark on. Emitted exactly when entering "needs you".
    MarkRed,
    /// Turn the red mark off. Emitted exactly when leaving "needs you".
    ClearRed,
    /// Raise the system notification for a new "needs you"; the caller
    /// drops it when the window has focus or the pane is muted (RF-ATN-09).
    Notify,
    /// Send [`Signal::Tick`] at or after this instant. One timer armed to
    /// [`Machine::next_wake`] after each step covers every `WakeAt`.
    WakeAt(Duration),
    /// Run the profile's screen rules and answer with [`Signal::Screen`].
    CheckScreen,
    /// The agent's session id is now this one (RF-SES-01, RN-05).
    SessionId(String),
    /// The agent reported the end of its session (input to RN-04).
    SessionEnded(crate::signal::SessionEndReason),
}

/// What the machine needs from the active profile rule.
///
/// ```
/// use yien_agent::{MachineConfig, Source};
///
/// let config = MachineConfig::new(&[Source::Hooks, Source::Screen], None);
/// assert!(config.declares(Source::Hooks));
/// assert!(!MachineConfig::unprofiled().declares(Source::Hooks));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MachineConfig {
    sources: Vec<Source>,
    watchdog: Option<Duration>,
}

impl MachineConfig {
    /// A rule declaring these sources, with an optional watchdog after which
    /// a silent "working" pane is re-checked with the screen rules.
    pub fn new(sources: &[Source], watchdog: Option<Duration>) -> Self {
        Self {
            sources: sources.to_vec(),
            watchdog,
        }
    }

    /// A program without a profile: only its process lifecycle counts
    /// (RF-AGE-08).
    pub fn unprofiled() -> Self {
        Self::default()
    }

    /// Whether the rule declares this source.
    pub fn declares(&self, source: Source) -> bool {
        self.sources.contains(&source)
    }

    /// Whether the screen rules may raise "needs you" on their own until
    /// the agent's structured channel (hooks or OSC 7501) first speaks: the
    /// screens an agent shows before it, such as folder trust, report
    /// nothing. A rule without such a channel has no startup to end.
    ///
    /// ```
    /// use yien_agent::{MachineConfig, Source};
    ///
    /// let codex = MachineConfig::new(&[Source::Osc7501, Source::Screen], None);
    /// assert!(codex.reads_startup_screens());
    /// let bell = MachineConfig::new(&[Source::Bel, Source::Screen], None);
    /// assert!(!bell.reads_startup_screens());
    /// ```
    pub fn reads_startup_screens(&self) -> bool {
        self.declares(Source::Screen)
            && (self.declares(Source::Hooks) || self.declares(Source::Osc7501))
    }

    /// Whether an agent CLI typed in this shell can have its startup screens
    /// read, with its own patterns, until it first reports over OSC 7501
    /// ([`Signal::TypedAgent`]). Only a shell rule hears both the prompt
    /// marks and the record that end that window.
    ///
    /// ```
    /// use yien_agent::{MachineConfig, Source};
    ///
    /// let shell = MachineConfig::new(&[Source::Osc133, Source::Osc7501], None);
    /// assert!(shell.reads_typed_startup_screens());
    /// let codex = MachineConfig::new(&[Source::Osc7501, Source::Screen], None);
    /// assert!(!codex.reads_typed_startup_screens());
    /// ```
    pub fn reads_typed_startup_screens(&self) -> bool {
        self.declares(Source::Osc133) && self.declares(Source::Osc7501)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pending {
    deadline: Duration,
    reason: NeedsReason,
    cause: Cause,
}

/// The agent state of one pane and everything the next step depends on.
///
/// ```
/// use std::time::Duration;
/// use yien_agent::{AgentState, Effect, HookEvent, Machine, MachineConfig, Signal, Source};
///
/// let ms = Duration::from_millis;
/// let m = Machine::new(MachineConfig::new(&[Source::Hooks], None));
/// let (m, _) = m.step(&Signal::Hook(HookEvent::UserPromptSubmit), ms(0));
/// let (m, fx) = m.step(&Signal::Hook(HookEvent::PermissionRequest), ms(10));
/// assert_eq!(fx, vec![Effect::WakeAt(ms(310))]);
/// let (m, fx) = m.step(&Signal::Tick, ms(310));
/// assert!(m.state().is_red());
/// assert!(fx.contains(&Effect::MarkRed));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Machine {
    config: MachineConfig,
    state: AgentState,
    seen: bool,
    top_rank: Option<u8>,
    signal_lost: bool,
    pending: Option<Pending>,
    input_pending: bool,
    awaiting_screen: bool,
    last_activity: Duration,
    watchdog_at: Option<Duration>,
    session_ended: bool,
    exited: bool,
    /// The running turn was started by the agent's own OSC 7501 record.
    reported_turn: bool,
    /// An agent reports over OSC 7501 in a rule that also reads OSC 133, so
    /// the shell's marks no longer drive the state.
    agent_reporting: bool,
    /// When the startup screen is read next, once output settles.
    settle_at: Option<Duration>,
    /// The first output since the startup screen was last read.
    unread_since: Option<Duration>,
    /// The red mark is a startup screen's, not a turn's.
    startup_red: bool,
    /// The state a startup screen's red mark returns to once it is gone.
    startup_from: AgentState,
    /// The pending screen read was armed for the interrupt line: needs-you
    /// rules are not read, only the interrupt ends a turn.
    awaiting_interrupt: bool,
    /// The read the pending settle will make is for the interrupt line.
    settle_for_interrupt: bool,
    /// An agent CLI typed in the shell runs and has not reported yet, so its
    /// startup screens are read.
    typed_agent: bool,
    /// The typed agent reached its REPL prompt without sending OSC 7501 yet:
    /// startup `NeedsYou` screens are over, and `in_turn` / interrupt reads
    /// drive turns until OSC 7501 speaks or the shell prompt returns.
    typed_repl_ready: bool,
    /// The shell has marked a command's start (OSC 133 C); cmd never does.
    command_marks: bool,
    /// When the last turn ended in Done or Idle.
    turn_ended_at: Option<Duration>,
}

impl Machine {
    /// An idle pane, not seen, for the given rule.
    pub fn new(config: MachineConfig) -> Self {
        Self {
            config,
            state: AgentState::Idle,
            seen: false,
            top_rank: None,
            signal_lost: false,
            pending: None,
            input_pending: false,
            awaiting_screen: false,
            awaiting_interrupt: false,
            last_activity: Duration::ZERO,
            watchdog_at: None,
            session_ended: false,
            exited: false,
            reported_turn: false,
            agent_reporting: false,
            settle_at: None,
            settle_for_interrupt: false,
            unread_since: None,
            startup_red: false,
            startup_from: AgentState::Idle,
            typed_agent: false,
            typed_repl_ready: false,
            command_marks: false,
            turn_ended_at: None,
        }
    }

    /// The current state.
    pub fn state(&self) -> AgentState {
        self.state
    }

    /// Whether the red mark is on.
    pub fn is_red(&self) -> bool {
        self.state.is_red()
    }

    /// Whether a "needs you" waits out its confirmation window: the red
    /// mark is not on yet, and input or work in the window can cancel it.
    ///
    /// ```
    /// use std::time::Duration;
    /// use yien_agent::{HookEvent, Machine, MachineConfig, Signal, Source};
    ///
    /// let ms = Duration::from_millis;
    /// let m = Machine::new(MachineConfig::new(&[Source::Hooks], None));
    /// let (m, _) = m.step(&Signal::Hook(HookEvent::UserPromptSubmit), ms(0));
    /// let (m, _) = m.step(&Signal::Hook(HookEvent::PermissionRequest), ms(10));
    /// assert!(m.needs_you_pending() && !m.is_red());
    /// let (m, _) = m.step(&Signal::Hook(HookEvent::PreToolUse), ms(20));
    /// assert!(!m.needs_you_pending());
    /// ```
    pub fn needs_you_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Whether the pane is seen (RN-07, RF-AGE-07).
    pub fn seen(&self) -> bool {
        self.seen
    }

    /// The "signal lost" diagnostic mark: set by a lost source or an
    /// ambiguous signal, cleared by the next strong signal.
    pub fn signal_lost(&self) -> bool {
        self.signal_lost
    }

    /// The earliest instant at which a [`Signal::Tick`] can change anything:
    /// the pending "needs you" deadline, the watchdog or the read of a
    /// startup screen, whichever comes first. The caller needs a single
    /// timer, re-armed to this after every step; [`Effect::WakeAt`] carries
    /// the same instants.
    ///
    /// ```
    /// use std::time::Duration;
    /// use yien_agent::{HookEvent, Machine, MachineConfig, Signal, Source};
    ///
    /// let ms = Duration::from_millis;
    /// let m = Machine::new(MachineConfig::new(&[Source::Hooks], None));
    /// assert_eq!(m.next_wake(), None);
    /// let (m, _) = m.step(&Signal::Hook(HookEvent::UserPromptSubmit), ms(0));
    /// let (m, _) = m.step(&Signal::Hook(HookEvent::PermissionRequest), ms(10));
    /// assert_eq!(m.next_wake(), Some(ms(310)));
    /// ```
    pub fn next_wake(&self) -> Option<Duration> {
        let working = self.state == AgentState::Working;
        let pending = self.pending.map(|p| p.deadline).filter(|_| working);
        let watchdog = self.watchdog_at.filter(|_| working);
        [pending, watchdog, self.settle_at]
            .into_iter()
            .flatten()
            .min()
    }

    /// Whether the machine waits for an answer to a screen read it armed
    /// for the interrupt line: the caller should feed it only rules that
    /// match an interrupt, since the rest of the turn's prompts are history.
    pub fn screen_read_is_interrupt(&self) -> bool {
        self.awaiting_interrupt
    }

    /// Whether an agent reporting over OSC 7501 has taken the pane over from
    /// the shell's OSC 133 marks, until it sends `clear` or the shell's
    /// prompt comes back.
    ///
    /// ```
    /// use std::time::Duration;
    /// use yien_agent::{AgentStatus, Machine, MachineConfig, OscSignal, Reporter, Signal, Source};
    ///
    /// let status = |s| Signal::Osc(OscSignal::Status7501(s));
    /// let m = Machine::new(MachineConfig::new(&[Source::Osc133, Source::Osc7501], None));
    /// let (m, _) = m.step(&status(AgentStatus::Idle(Reporter::Codex)), Duration::ZERO);
    /// assert!(m.agent_reporting());
    /// let (m, _) = m.step(&status(AgentStatus::Clear), Duration::ZERO);
    /// assert!(!m.agent_reporting());
    /// ```
    pub fn agent_reporting(&self) -> bool {
        self.agent_reporting
    }

    /// Whether the current turn was started by the agent's own OSC 7501
    /// record rather than a shell's OSC 133 command mark.
    ///
    /// ```
    /// use std::time::Duration;
    /// use yien_agent::{AgentStatus, Machine, MachineConfig, OscSignal, PromptMark, Signal, Source};
    ///
    /// let m = Machine::new(MachineConfig::new(&[Source::Osc133, Source::Osc7501], None));
    /// let (m, _) = m.step(&Signal::Osc(OscSignal::Prompt(PromptMark::CommandStart)), Duration::ZERO);
    /// assert!(!m.in_reported_turn());
    /// let (m, _) = m.step(&Signal::Osc(OscSignal::Status7501(AgentStatus::Working)), Duration::ZERO);
    /// assert!(m.in_reported_turn());
    /// ```
    pub fn in_reported_turn(&self) -> bool {
        self.reported_turn
    }

    /// Whether the screen is read for the startup screens of an agent CLI
    /// typed in the shell: the caller answers [`Effect::CheckScreen`] with
    /// that agent's patterns instead of the rule's own.
    ///
    /// ```
    /// use std::time::Duration;
    /// use yien_agent::{Machine, MachineConfig, Signal, Source};
    ///
    /// let m = Machine::new(MachineConfig::new(&[Source::Osc133, Source::Osc7501], None));
    /// let (m, _) = m.step(&Signal::TypedAgent(true), Duration::ZERO);
    /// assert!(m.reads_typed_agent());
    /// let (m, _) = m.step(&Signal::TypedAgent(false), Duration::ZERO);
    /// assert!(!m.reads_typed_agent());
    /// ```
    pub fn reads_typed_agent(&self) -> bool {
        self.typed_agent
    }

    /// Whether a typed agent in a shell has already reached its REPL prompt
    /// without reporting over OSC 7501 yet: startup `NeedsYou` checks stop and
    /// only `in_turn` / interrupt reads run.
    pub fn typed_repl_ready(&self) -> bool {
        self.typed_repl_ready
    }

    /// The rule this machine runs with.
    pub fn config(&self) -> &MachineConfig {
        &self.config
    }

    /// Applies one signal at instant `now` (monotonic, from the caller).
    pub fn step(mut self, signal: &Signal, now: Duration) -> (Self, Vec<Effect>) {
        let mut fx = Vec::new();
        self.apply(signal, now, &mut fx);
        (self, fx)
    }

    fn apply(&mut self, signal: &Signal, now: Duration, fx: &mut Vec<Effect>) {
        match signal {
            Signal::Spawned => {
                let fresh = Machine {
                    state: self.state,
                    seen: self.seen,
                    ..Machine::new(self.config.clone())
                };
                *self = fresh;
                self.set_state(AgentState::Idle, Cause::Process, now, fx);
            }
            Signal::Seen(seen) => {
                self.seen = *seen;
                if *seen && matches!(self.state, AgentState::Done | AgentState::Error(_)) {
                    self.set_state(AgentState::Idle, Cause::User, now, fx);
                }
            }
            _ if self.exited => {}
            Signal::Tick => self.on_tick(now, fx),
            Signal::Exited { code } => {
                self.exited = true;
                self.typed_agent = false;
                self.typed_repl_ready = false;
                self.settle_at = None;
                self.unread_since = None;
                match self.state {
                    // Quitting a startup screen ends no turn.
                    AgentState::Working | AgentState::NeedsYou(_)
                        if !self.session_ended && !self.startup_red =>
                    {
                        let to = AgentState::Error(ErrorReason::UnexpectedExit { code: *code });
                        self.set_state(to, Cause::Process, now, fx);
                    }
                    AgentState::Error(_) => {}
                    _ => self.end_turn(Cause::Process, now, fx),
                }
            }
            Signal::SourceLost => self.lose(Cause::SignalLost, now, fx),
            Signal::Ambiguous => self.lose(Cause::Ambiguous, now, fx),
            Signal::Input => {
                if self.state.is_red() || self.pending.is_some() {
                    self.input_pending = true;
                }
            }
            Signal::TypedAgent(running) => self.on_typed_agent(*running, now, fx),
            Signal::ServiceError => self.on_service_error(now, fx),
            Signal::Output => {
                if self.reads_startup_screen() {
                    // An answered startup screen is read again, not taken
                    // for a turn going on.
                    self.input_pending = false;
                    self.arm_screen_read(now, fx, false);
                } else if self.state.is_red() && self.input_pending {
                    self.set_state(AgentState::Working, Cause::User, now, fx);
                } else if self.state == AgentState::Working {
                    self.last_activity = now;
                    if self.pending.is_some() && self.input_pending {
                        self.pending = None;
                        self.input_pending = false;
                    }
                }
                // A turn is read for the interrupt line for as long as it
                // runs, red or not.
                self.arm_interrupt_read(now, fx);
            }
            Signal::Hook(_) | Signal::Osc(_) | Signal::Screen(_) => self.detect(signal, now, fx),
        }
    }

    fn detect(&mut self, signal: &Signal, now: Duration, fx: &mut Vec<Effect>) {
        let Some(source) = signal.source() else {
            return;
        };
        let typed_screen = source == Source::Screen && self.typed_agent;
        let armed_interrupt_read = source == Source::Screen && self.awaiting_screen;
        if !self.config.declares(source) && !typed_screen && !armed_interrupt_read {
            return;
        }
        // An interrupt verdict ends the turn whatever the rank a channel
        // reached: 2.1.326 sends no record for one, so no hook outranks it
        // and no needs-you is raised against it (RN-02 keeps red as it is).
        if let Signal::Screen(ScreenVerdict::Interrupted) = signal {
            self.on_interrupted_screen(now, fx);
            return;
        }
        // A turn-ended verdict ends the turn as Idle, only from Working with
        // no pending NeedsYou: a quiet pause inside a permission prompt is
        // not a turn end (RN-01/RN-02).
        if let Signal::Screen(ScreenVerdict::TurnEnded) = signal {
            self.on_turn_ended(now, fx);
            return;
        }
        if let Signal::Screen(verdict) = signal {
            self.on_screen(*verdict, now, fx);
            return;
        }
        let rank = source.rank();
        if self.top_rank.is_some_and(|top| rank < top) {
            self.outranked_interrupt(signal, now, fx);
            return;
        }
        self.top_rank = Some(rank);
        if self.typed_agent {
            match source {
                // The agent speaks for itself; a startup red is cleared below.
                Source::Osc7501 => {
                    self.typed_agent = false;
                    self.typed_repl_ready = false;
                }
                // The shell's prompt is back: the agent is gone.
                Source::Osc133 => self.end_typed_startup(Cause::Source(source), now, fx),
                _ => {}
            }
        }
        if rank >= Source::Osc7501.rank() {
            self.signal_lost = false;
            // The agent got past its startup screens and speaks for itself.
            self.settle_at = None;
            self.unread_since = None;
            if self.startup_red {
                self.set_state(AgentState::Idle, Cause::Source(source), now, fx);
            }
        }
        if self.state == AgentState::Working {
            self.last_activity = now;
        }
        let cause = Cause::Source(source);
        match signal {
            Signal::Hook(event) => self.on_hook(event, cause, now, fx),
            Signal::Osc(osc) => self.on_osc(*osc, cause, now, fx),
            _ => {}
        }
    }

    /// An interrupted turn told by a source the pane's hooks outrank: Claude
    /// Code (2.1.325) fires no hook when Esc or Ctrl+C stops a turn and only
    /// reports it as OSC 7501 `idle`, so that record still ends the turn.
    fn outranked_interrupt(&mut self, signal: &Signal, now: Duration, fx: &mut Vec<Effect>) {
        let Signal::Osc(OscSignal::Status7501(AgentStatus::Idle(reporter))) = signal else {
            return;
        };
        if !reporter.ends_turns_with_idle() && self.in_turn() {
            self.set_state(AgentState::Idle, Cause::Source(Source::Osc7501), now, fx);
        }
    }

    fn on_hook(&mut self, event: &HookEvent, cause: Cause, now: Duration, fx: &mut Vec<Effect>) {
        match event {
            HookEvent::SessionStart { session_id, source } => {
                if let Some(id) = session_id {
                    fx.push(Effect::SessionId(id.clone()));
                }
                self.session_ended = false;
                // Compaction (and a resume landing mid-turn) is part of the
                // turn; going idle here would miss its red and its Stop.
                let keeps_turn = matches!(
                    source,
                    SessionStartSource::Compact | SessionStartSource::Resume
                );
                if !(keeps_turn && self.in_turn()) {
                    self.set_state(AgentState::Idle, cause, now, fx);
                }
            }
            HookEvent::UserPromptSubmit => self.start_turn(cause, now, fx),
            HookEvent::PreToolUse | HookEvent::PostToolUse | HookEvent::PostToolUseFailure => {
                self.resume_work(cause, now, fx)
            }
            HookEvent::PermissionRequest => self.needs(NeedsReason::Permission, cause, now, fx),
            HookEvent::UnattributedPermissionRequest => {}
            HookEvent::Notification(kind) => match kind {
                NotificationKind::PermissionPrompt => {
                    self.needs(NeedsReason::Permission, cause, now, fx)
                }
                NotificationKind::Elicitation | NotificationKind::AgentNeedsInput => {
                    self.needs(NeedsReason::Question, cause, now, fx)
                }
                NotificationKind::Other => {}
            },
            HookEvent::Stop => {
                if self.in_turn() {
                    self.end_turn(cause, now, fx);
                }
            }
            HookEvent::StopFailure => {
                if self.in_turn() {
                    self.set_state(AgentState::Error(ErrorReason::TurnFailed), cause, now, fx);
                }
            }
            HookEvent::SessionEnd(reason) => {
                fx.push(Effect::SessionEnded(*reason));
                self.session_ended = true;
                if self.in_turn() {
                    self.set_state(AgentState::Idle, cause, now, fx);
                }
            }
        }
    }

    fn on_osc(&mut self, osc: OscSignal, cause: Cause, now: Duration, fx: &mut Vec<Effect>) {
        if osc == OscSignal::Prompt(PromptMark::CommandStart) {
            self.command_marks = true;
        }
        match osc {
            OscSignal::Status7501(status) => self.on_status(status, cause, now, fx),
            OscSignal::Prompt(mark) if self.agent_reporting => {
                // The shell only draws its prompt once the agent is gone.
                if matches!(mark, PromptMark::CommandEnd | PromptMark::PromptStart) {
                    self.agent_left(now, fx);
                }
            }
            OscSignal::Prompt(PromptMark::CommandStart) => self.start_turn(cause, now, fx),
            OscSignal::Prompt(PromptMark::CommandEnd | PromptMark::PromptStart) => {
                if self.state == AgentState::Working {
                    self.end_turn(cause, now, fx);
                }
            }
            // Done or waiting? A notification cannot tell (RN-02).
            OscSignal::Notify9 | OscSignal::Notify777 | OscSignal::Bell => {}
        }
    }

    fn on_status(
        &mut self,
        status: AgentStatus,
        cause: Cause,
        now: Duration,
        fx: &mut Vec<Effect>,
    ) {
        if status == AgentStatus::Clear {
            self.agent_reporting = false;
            self.set_state(AgentState::Idle, cause, now, fx);
            return;
        }
        if self.config.declares(Source::Osc133) {
            self.agent_reporting = true;
        }
        match status {
            AgentStatus::Working => {
                self.start_turn(cause, now, fx);
                self.reported_turn = true;
            }
            AgentStatus::Blocked(kind) => {
                // Blocked comes without a working first after a turn ended
                // (Codex async questions) or for a request made from outside.
                if !self.in_turn() {
                    self.set_state(AgentState::Working, cause, now, fx);
                }
                self.reported_turn = true;
                self.needs(needs_reason(kind), cause, now, fx);
            }
            AgentStatus::Done => self.status_turn_end(true, cause, now, fx),
            // Codex turns its done and error into idle on any key: only a
            // running turn may end here, so an unseen Done or an Error stays.
            AgentStatus::Idle(reporter) => {
                self.status_turn_end(reporter.ends_turns_with_idle(), cause, now, fx)
            }
            AgentStatus::Error => {
                if self.in_turn() {
                    self.set_state(AgentState::Error(ErrorReason::TurnFailed), cause, now, fx);
                }
            }
            AgentStatus::Clear => {}
        }
    }

    /// Ends a running turn: as Done (when unseen) if the agent started it and
    /// `completes`, otherwise back to Idle. A turn the shell's OSC 133 marks
    /// started is the command that launched the agent, not an agent turn.
    fn status_turn_end(
        &mut self,
        completes: bool,
        cause: Cause,
        now: Duration,
        fx: &mut Vec<Effect>,
    ) {
        if !self.in_turn() {
            return;
        }
        if completes && self.reported_turn {
            self.end_turn(cause, now, fx);
        } else {
            self.set_state(AgentState::Idle, cause, now, fx);
        }
    }

    /// The agent that took the pane over from the shell is gone without a
    /// `clear` (killed or crashed): the shell's marks drive the state again,
    /// and a turn it left running is an unexpected exit.
    fn agent_left(&mut self, now: Duration, fx: &mut Vec<Effect>) {
        self.agent_reporting = false;
        if self.in_turn() {
            let to = AgentState::Error(ErrorReason::UnexpectedExit { code: None });
            self.set_state(to, Cause::Process, now, fx);
        }
    }

    fn on_screen(&mut self, verdict: ScreenVerdict, now: Duration, fx: &mut Vec<Effect>) {
        let asked = std::mem::take(&mut self.awaiting_screen);
        let for_interrupt = std::mem::take(&mut self.awaiting_interrupt);
        if asked && for_interrupt {
            // A read armed mid-turn sees the whole history: needs you are a
            // startup channel, and a stale prompt must not light the red
            // mark on an unremarkable turn.
            return;
        }
        if asked && self.reads_startup_screen() {
            // Settled output already held for the confirmation window.
            let cause = Cause::Source(Source::Screen);
            match verdict {
                ScreenVerdict::NeedsYou if !self.startup_red && !self.typed_repl_ready => {
                    self.startup_from = self.state;
                    self.set_state(
                        AgentState::NeedsYou(NeedsReason::ScreenRule),
                        cause,
                        now,
                        fx,
                    );
                    self.startup_red = true;
                }
                ScreenVerdict::NoMatch if self.startup_red => {
                    self.set_state(self.startup_from, cause, now, fx);
                }
                ScreenVerdict::Ready if self.typed_agent => {
                    self.typed_repl_ready = true;
                    if self.startup_red || self.state == AgentState::Working {
                        self.set_state(AgentState::Idle, cause, now, fx);
                    }
                }
                ScreenVerdict::InTurn if self.typed_agent => {
                    self.typed_repl_ready = true;
                    if self.startup_red || matches!(self.state, AgentState::Idle | AgentState::Done)
                    {
                        self.set_state(AgentState::Working, cause, now, fx);
                    }
                }
                _ => {}
            }
            return;
        }
        if asked
            && verdict == ScreenVerdict::NeedsYou
            && self.screen_allowed()
            && self.state == AgentState::Working
        {
            let cause = Cause::Source(Source::Screen);
            self.needs(NeedsReason::ScreenRule, cause, now, fx);
        }
    }

    /// An armed interrupt read saw the profile's `in_turn` marker gone after
    /// seeing it during this turn (Claude 2.1.327 draws no interrupt line and
    /// sends no record on Ctrl+C). Ends the turn as Idle, never Done and never
    /// red (RN-01/RN-02), and never cancels a pending NeedsYou.
    fn on_turn_ended(&mut self, now: Duration, fx: &mut Vec<Effect>) {
        let asked = std::mem::take(&mut self.awaiting_screen);
        let _interrupt_kind = std::mem::take(&mut self.awaiting_interrupt);
        if asked && self.state == AgentState::Working && self.pending.is_none() {
            let cause = Cause::Source(Source::Screen);
            self.set_state(AgentState::Idle, cause, now, fx);
        }
    }

    /// A screen read armed during a turn (or about one) found the line an
    /// agent draws when the user interrupts its turn: Claude Code 2.1.326
    /// stops sending `state=idle` for one, so the line is all the machine
    /// gets. It ends the turn as Idle, whatever channel outranks the screen;
    /// it never raises the red mark.
    fn on_interrupted_screen(&mut self, now: Duration, fx: &mut Vec<Effect>) {
        let asked = std::mem::take(&mut self.awaiting_screen);
        let _interrupt_kind = std::mem::take(&mut self.awaiting_interrupt);
        if asked && self.in_turn() {
            let cause = Cause::Source(Source::Screen);
            self.set_state(AgentState::Idle, cause, now, fx);
        }
    }

    /// Reads the screen once output settles, and at least every
    /// [`STARTUP_SCREEN_MAX_WAIT`] while it keeps coming.
    fn arm_screen_read(&mut self, now: Duration, fx: &mut Vec<Effect>, for_interrupt: bool) {
        let first = *self.unread_since.get_or_insert(now);
        let at = (now + STARTUP_SCREEN_SETTLE).min(first + STARTUP_SCREEN_MAX_WAIT);
        self.settle_at = Some(at);
        self.settle_for_interrupt = for_interrupt;
        fx.push(Effect::WakeAt(at));
    }

    /// Arms the read of an interrupt during a turn, like a startup read:
    /// once output settles, at least every [`STARTUP_SCREEN_MAX_WAIT`].
    fn arm_interrupt_read(&mut self, now: Duration, fx: &mut Vec<Effect>) {
        if self.reads_startup_screen() {
            // The settled read covers the interrupt line too.
            return;
        }
        if self.reads_interrupt_screen() {
            self.arm_screen_read(now, fx, true);
        }
    }

    /// An agent CLI with startup screens was found running in the shell, or
    /// it no longer runs there. Its screens are read from then until it first
    /// reports; one that reported already is past them.
    fn on_typed_agent(&mut self, running: bool, now: Duration, fx: &mut Vec<Effect>) {
        if !running {
            self.end_typed_startup(Cause::Process, now, fx);
            return;
        }
        let red_of_its_own = self.state.is_red() && !self.startup_red;
        // A poll can land after the CLI exited: in a shell that marks its
        // commands, a prompt mark since the last C means it is gone.
        let at_prompt =
            self.command_marks && self.state != AgentState::Working && !self.startup_red;
        if !self.config.reads_typed_startup_screens()
            || self.agent_reporting
            || self.signal_lost
            || red_of_its_own
            || at_prompt
        {
            return;
        }
        self.typed_agent = true;
        // The screen may have settled long before the program was found.
        self.arm_screen_read(now, fx, false);
    }

    /// A turn the agent's service says failed: the running one, or the one
    /// that ended within [`SERVICE_ERROR_GRACE`]. Only a rule that reads OSC
    /// 7501 hears it, as the pane learns its sessions there.
    fn on_service_error(&mut self, now: Duration, fx: &mut Vec<Effect>) {
        if !self.config.declares(Source::Osc7501) || self.startup_red {
            return;
        }
        let just_ended = matches!(self.state, AgentState::Done | AgentState::Idle)
            && self
                .turn_ended_at
                .is_some_and(|at| now.saturating_sub(at) <= SERVICE_ERROR_GRACE);
        if self.in_turn() || just_ended {
            self.turn_ended_at = None;
            let to = AgentState::Error(ErrorReason::TurnFailed);
            self.set_state(to, Cause::Service, now, fx);
        }
    }

    /// Stops reading a typed agent's startup screens; a red mark one of them
    /// raised goes, back to the state it interrupted.
    fn end_typed_startup(&mut self, cause: Cause, now: Duration, fx: &mut Vec<Effect>) {
        if !std::mem::take(&mut self.typed_agent) {
            return;
        }
        self.typed_repl_ready = false;
        self.settle_at = None;
        self.unread_since = None;
        self.awaiting_screen = false;
        self.awaiting_interrupt = false;
        if self.startup_red {
            self.set_state(self.startup_from, cause, now, fx);
        }
    }

    fn on_tick(&mut self, now: Duration, fx: &mut Vec<Effect>) {
        if let Some(at) = self.settle_at {
            if now < at {
                fx.push(Effect::WakeAt(at));
            } else {
                self.settle_at = None;
                self.unread_since = None;
                if (self.reads_startup_screen() || self.reads_interrupt_screen())
                    && !self.awaiting_screen
                {
                    self.awaiting_screen = true;
                    self.awaiting_interrupt = self.settle_for_interrupt;
                    fx.push(Effect::CheckScreen);
                }
            }
        }
        if let Some(pending) = self.pending
            && now >= pending.deadline
            && self.state == AgentState::Working
        {
            self.set_state(AgentState::NeedsYou(pending.reason), pending.cause, now, fx);
            return;
        }
        if self.state != AgentState::Working {
            return;
        }
        self.watchdog_tick(now, fx);
        if let Some(pending) = self.pending {
            fx.push(Effect::WakeAt(pending.deadline));
        }
    }

    fn watchdog_tick(&mut self, now: Duration, fx: &mut Vec<Effect>) {
        let (Some(at), Some(period)) = (self.watchdog_at, self.config.watchdog) else {
            return;
        };
        if now < at {
            return;
        }
        if now >= self.last_activity + period {
            if self.screen_allowed() && !self.awaiting_screen {
                self.awaiting_screen = true;
                self.awaiting_interrupt = false;
                fx.push(Effect::CheckScreen);
            }
            self.last_activity = now;
        }
        let next = self.last_activity + period;
        self.watchdog_at = Some(next);
        fx.push(Effect::WakeAt(next));
    }

    fn in_turn(&self) -> bool {
        matches!(self.state, AgentState::Working | AgentState::NeedsYou(_))
    }

    /// A screen rule may only confirm when no structured channel is alive
    /// and no source was lost (04 §4.5, "Prioridad de fuentes").
    fn screen_allowed(&self) -> bool {
        self.config.declares(Source::Screen)
            && self.top_rank.is_none_or(|top| top < Source::Hooks.rank())
            && !self.signal_lost
    }

    /// Whether the screen is read during a running turn for the line an
    /// agent draws when the user interrupts it: Claude Code 2.1.326 stops
    /// sending its `state=idle` on an interrupted turn, so the line is the
    /// only sign of it. The read only ends the turn; it never raises the
    /// red mark.
    ///
    /// ```
    /// use std::time::Duration;
    /// use yien_agent::{HookEvent, Machine, MachineConfig, Signal, Source};
    ///
    /// let ms = Duration::from_millis;
    /// let m = Machine::new(MachineConfig::new(&[Source::Hooks, Source::Screen], None));
    /// assert!(!m.reads_interrupt_screen());
    /// let (m, _) = m.step(&Signal::Hook(HookEvent::UserPromptSubmit), ms(0));
    /// assert!(m.reads_interrupt_screen());
    /// let (m, _) = m.step(&Signal::Tick, ms(1_000));
    /// assert!(m.reads_interrupt_screen());
    /// ```
    pub fn reads_interrupt_screen(&self) -> bool {
        self.in_turn()
            && !self.exited
            && (self.config.declares(Source::Screen) || self.typed_agent || self.agent_reporting)
    }

    /// Whether the screen is read for a startup screen: the rule allows it,
    /// the agent has not spoken over its structured channel since it was
    /// spawned, and no turn or end is on show. A typed agent's screens are
    /// read over the shell's running command too.
    fn reads_startup_screen(&self) -> bool {
        if self.typed_agent {
            return !self.exited
                && !self.signal_lost
                && (!self.state.is_red() || self.startup_red)
                && !(self.typed_repl_ready && self.state == AgentState::Working);
        }
        self.config.reads_startup_screens()
            && self.screen_allowed()
            && self.top_rank.is_none_or(|top| top < Source::Osc7501.rank())
            && !self.exited
            && (self.state == AgentState::Idle || self.startup_red)
    }

    fn start_turn(&mut self, cause: Cause, now: Duration, fx: &mut Vec<Effect>) {
        if self.state == AgentState::Working {
            self.pending = None;
        } else {
            self.set_state(AgentState::Working, cause, now, fx);
        }
    }

    fn resume_work(&mut self, cause: Cause, now: Duration, fx: &mut Vec<Effect>) {
        match self.state {
            AgentState::Working => self.pending = None,
            AgentState::NeedsYou(_) => self.set_state(AgentState::Working, cause, now, fx),
            _ => {}
        }
    }

    fn needs(&mut self, reason: NeedsReason, cause: Cause, now: Duration, fx: &mut Vec<Effect>) {
        if self.state != AgentState::Working || self.pending.is_some() {
            return;
        }
        let deadline = now + NEEDS_YOU_CONFIRM;
        self.input_pending = false;
        self.pending = Some(Pending {
            deadline,
            reason,
            cause,
        });
        fx.push(Effect::WakeAt(deadline));
    }

    fn end_turn(&mut self, cause: Cause, now: Duration, fx: &mut Vec<Effect>) {
        let to = if self.seen {
            AgentState::Idle
        } else {
            AgentState::Done
        };
        self.set_state(to, cause, now, fx);
    }

    fn lose(&mut self, cause: Cause, now: Duration, fx: &mut Vec<Effect>) {
        self.signal_lost = true;
        self.typed_agent = false;
        self.typed_repl_ready = false;
        self.input_pending = false;
        self.pending = None;
        self.awaiting_screen = false;
        self.awaiting_interrupt = false;
        self.settle_at = None;
        self.unread_since = None;
        self.set_state(AgentState::Idle, cause, now, fx);
    }

    /// The only place the state changes, so the red mark always follows it.
    fn set_state(&mut self, to: AgentState, cause: Cause, now: Duration, fx: &mut Vec<Effect>) {
        // Like Done, a failure the user is looking at needs no mark.
        let to = match to {
            AgentState::Error(_) if self.seen => AgentState::Idle,
            to => to,
        };
        let from = self.state;
        if from == to {
            return;
        }
        let in_turn = matches!(from, AgentState::Working | AgentState::NeedsYou(_));
        if in_turn && !self.startup_red && matches!(to, AgentState::Done | AgentState::Idle) {
            self.turn_ended_at = Some(now);
        }
        self.state = to;
        fx.push(Effect::Transition { from, to, cause });
        if from.is_red() && !to.is_red() {
            self.startup_red = false;
            fx.push(Effect::ClearRed);
        }
        if to.is_red() && !from.is_red() {
            self.input_pending = false;
            fx.push(Effect::MarkRed);
            fx.push(Effect::Notify);
        }
        if to == AgentState::Working {
            self.settle_at = None;
            self.unread_since = None;
            self.last_activity = now;
            if let Some(period) = self.config.watchdog {
                let at = now + period;
                self.watchdog_at = Some(at);
                fx.push(Effect::WakeAt(at));
            }
        } else {
            self.pending = None;
            self.awaiting_screen = false;
            self.awaiting_interrupt = false;
            self.watchdog_at = None;
        }
        if !matches!(to, AgentState::Working | AgentState::NeedsYou(_)) {
            self.reported_turn = false;
        }
    }
}

fn needs_reason(kind: Option<BlockedKind>) -> NeedsReason {
    match kind {
        Some(BlockedKind::Permission) => NeedsReason::Permission,
        Some(BlockedKind::Question) => NeedsReason::Question,
        Some(BlockedKind::Auth) => NeedsReason::Auth,
        None => NeedsReason::Blocked,
    }
}
