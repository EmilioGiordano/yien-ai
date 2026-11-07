//! The pure agent state machine (04 §4.5): `(state, signal, now) -> (state,
//! effects)`. No clock, no I/O; the caller executes the effects.

use std::time::Duration;

use crate::signal::{
    AgentStatus, HookEvent, NotificationKind, OscSignal, PromptMark, ScreenVerdict,
    SessionStartSource, Signal, Source,
};

/// How long a "needs you" signal must stand without a contrary one before the
/// red mark goes on (04 §4.5, hysteresis). Leaving red is immediate.
pub const NEEDS_YOU_CONFIRM: Duration = Duration::from_millis(300);

/// Why an agent needs the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NeedsReason {
    /// A permission prompt.
    Permission,
    /// A question or an input request.
    Question,
    /// OSC 7501 `blocked`.
    Blocked,
    /// A screen rule confirmed it.
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
    /// Send [`Signal::Tick`] at or after this instant.
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
            last_activity: Duration::ZERO,
            watchdog_at: None,
            session_ended: false,
            exited: false,
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

    /// Whether the pane is seen (RN-07, RF-AGE-07).
    pub fn seen(&self) -> bool {
        self.seen
    }

    /// The "signal lost" diagnostic mark: set by a lost source or an
    /// ambiguous signal, cleared by the next strong signal.
    pub fn signal_lost(&self) -> bool {
        self.signal_lost
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
                if *seen && self.state == AgentState::Done {
                    self.set_state(AgentState::Idle, Cause::User, now, fx);
                }
            }
            _ if self.exited => {}
            Signal::Tick => self.on_tick(now, fx),
            Signal::Exited { code } => {
                self.exited = true;
                match self.state {
                    AgentState::Working | AgentState::NeedsYou(_) if !self.session_ended => {
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
                if self.state.is_red() {
                    self.input_pending = true;
                }
            }
            Signal::Output => {
                if self.state.is_red() && self.input_pending {
                    self.set_state(AgentState::Working, Cause::User, now, fx);
                } else if self.state == AgentState::Working {
                    self.last_activity = now;
                }
            }
            Signal::Hook(_) | Signal::Osc(_) | Signal::Screen(_) => self.detect(signal, now, fx),
        }
    }

    fn detect(&mut self, signal: &Signal, now: Duration, fx: &mut Vec<Effect>) {
        let Some(source) = signal.source() else {
            return;
        };
        if !self.config.declares(source) {
            return;
        }
        if let Signal::Screen(verdict) = signal {
            self.on_screen(*verdict, now, fx);
            return;
        }
        let rank = source.rank();
        if self.top_rank.is_some_and(|top| rank < top) {
            return;
        }
        self.top_rank = Some(rank);
        if rank >= Source::Osc7501.rank() {
            self.signal_lost = false;
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
            HookEvent::PreToolUse | HookEvent::PostToolUse => self.resume_work(cause, now, fx),
            HookEvent::PermissionRequest => self.needs(NeedsReason::Permission, cause, now, fx),
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
        match osc {
            OscSignal::Status7501(AgentStatus::Working) => self.start_turn(cause, now, fx),
            OscSignal::Status7501(AgentStatus::Blocked) => {
                self.needs(NeedsReason::Blocked, cause, now, fx)
            }
            OscSignal::Status7501(AgentStatus::Done) => {
                if self.in_turn() {
                    self.end_turn(cause, now, fx);
                }
            }
            OscSignal::Prompt(PromptMark::CommandStart) => self.start_turn(cause, now, fx),
            OscSignal::Prompt(PromptMark::CommandEnd | PromptMark::PromptStart) => {
                if self.state == AgentState::Working {
                    self.end_turn(cause, now, fx);
                }
            }
            // Done or waiting? A notification cannot tell (RN-02); S7 and S8
            // decide what Codex and OpenCode notifications mean.
            OscSignal::Notify9 | OscSignal::Notify777 | OscSignal::Bell => {}
        }
    }

    fn on_screen(&mut self, verdict: ScreenVerdict, now: Duration, fx: &mut Vec<Effect>) {
        let asked = std::mem::take(&mut self.awaiting_screen);
        if asked
            && verdict == ScreenVerdict::NeedsYou
            && self.screen_allowed()
            && self.state == AgentState::Working
        {
            let cause = Cause::Source(Source::Screen);
            self.needs(NeedsReason::ScreenRule, cause, now, fx);
        }
    }

    fn on_tick(&mut self, now: Duration, fx: &mut Vec<Effect>) {
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
        let (Some(at), Some(period)) = (self.watchdog_at, self.config.watchdog) else {
            return;
        };
        if now < at {
            return;
        }
        if now >= self.last_activity + period {
            if self.screen_allowed() && !self.awaiting_screen {
                self.awaiting_screen = true;
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
        self.input_pending = false;
        self.pending = None;
        self.awaiting_screen = false;
        self.set_state(AgentState::Idle, cause, now, fx);
    }

    /// The only place the state changes, so the red mark always follows it.
    fn set_state(&mut self, to: AgentState, cause: Cause, now: Duration, fx: &mut Vec<Effect>) {
        let from = self.state;
        if from == to {
            return;
        }
        self.state = to;
        fx.push(Effect::Transition { from, to, cause });
        if from.is_red() && !to.is_red() {
            fx.push(Effect::ClearRed);
        }
        if to.is_red() && !from.is_red() {
            self.input_pending = false;
            fx.push(Effect::MarkRed);
            fx.push(Effect::Notify);
        }
        if to == AgentState::Working {
            self.last_activity = now;
            if let Some(period) = self.config.watchdog {
                let at = now + period;
                self.watchdog_at = Some(at);
                fx.push(Effect::WakeAt(at));
            }
        } else {
            self.pending = None;
            self.awaiting_screen = false;
            self.watchdog_at = None;
        }
    }
}
