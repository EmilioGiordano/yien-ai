//! What the state machine listens to: hook events, terminal sequences,
//! screen-rule verdicts, process lifecycle and the user.

use serde::Deserialize;

/// A detection source a profile rule can declare (04 §2.4, ADR-08).
///
/// A signal from a source the active rule does not declare is ignored.
///
/// ```
/// use yien_agent::Source;
///
/// assert!(Source::Hooks.rank() > Source::Osc7501.rank());
/// assert!(Source::Osc7501.rank() > Source::Osc9.rank());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// The agent's structured channel: Claude Code hooks.
    Hooks,
    /// OSC 7501 agent status sequences.
    Osc7501,
    /// OSC 133 shell prompt marks.
    Osc133,
    /// OSC 9 notifications.
    Osc9,
    /// OSC 777 notifications.
    Osc777,
    /// A bare BEL.
    Bel,
    /// Screen rules, which only ever confirm "needs you".
    Screen,
}

impl Source {
    /// Priority of the source; a higher one overrides a lower one (ADR-08).
    ///
    /// ```
    /// use yien_agent::Source;
    ///
    /// assert_eq!(Source::Hooks.rank(), 3);
    /// assert_eq!(Source::Screen.rank(), 0);
    /// ```
    pub fn rank(self) -> u8 {
        match self {
            Source::Hooks => 3,
            Source::Osc7501 | Source::Osc133 => 2,
            Source::Osc9 | Source::Osc777 | Source::Bel => 1,
            Source::Screen => 0,
        }
    }
}

/// How a Claude Code session started (`source` of `SessionStart`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStartSource {
    /// A new session.
    Startup,
    /// `--resume` or `--continue`.
    Resume,
    /// `/clear`.
    Clear,
    /// After compaction.
    Compact,
    /// `--fork-session`.
    Fork,
    /// A value this version does not know.
    Other,
}

/// Why a Claude Code session ended (`reason` of `SessionEnd`).
///
/// Passed through untouched in [`crate::Effect::SessionEnded`]; whether an
/// end counts as explicit for RN-04 is the session layer's call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEndReason {
    /// `/clear`; a new session follows.
    Clear,
    /// The session was replaced by a resumed one.
    Resume,
    /// `/logout`.
    Logout,
    /// The user left the prompt (`/exit`, Ctrl+D).
    PromptInputExit,
    /// `other` or a value this version does not know.
    Other,
}

/// The `notification_type` of a Claude Code `Notification` hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationKind {
    /// `permission_prompt`.
    PermissionPrompt,
    /// `elicitation_dialog` or `elicitation_url_dialog`.
    Elicitation,
    /// `agent_needs_input`.
    AgentNeedsInput,
    /// Anything else; never changes the state.
    Other,
}

/// A decoded Claude Code hook event (the structured channel).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookEvent {
    /// A session started or rotated its id.
    SessionStart {
        /// The new session id, already validated.
        session_id: Option<String>,
        /// How it started.
        source: SessionStartSource,
    },
    /// The user submitted a prompt.
    UserPromptSubmit,
    /// A tool is about to run (fires before any permission prompt).
    PreToolUse,
    /// A tool finished.
    PostToolUse,
    /// Claude is asking for permission.
    PermissionRequest,
    /// Claude sent a notification.
    Notification(NotificationKind),
    /// The turn ended.
    Stop,
    /// The turn ended on an API error.
    StopFailure,
    /// The session ended.
    SessionEnd(SessionEndReason),
}

/// The value of an OSC 7501 agent status sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    /// The agent is working.
    Working,
    /// The agent is waiting for the user.
    Blocked,
    /// The turn is done.
    Done,
}

/// An OSC 133 shell prompt mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptMark {
    /// `A`: the prompt is being drawn.
    PromptStart,
    /// `C`: a command started running.
    CommandStart,
    /// `D`: the command finished.
    CommandEnd,
}

/// A terminal sequence relevant to agent state, as found by the PTY scanner.
///
/// Carries no terminal content: a notification's text is never needed here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OscSignal {
    /// OSC 7501.
    Status7501(AgentStatus),
    /// OSC 133.
    Prompt(PromptMark),
    /// OSC 9 notification.
    Notify9,
    /// OSC 777 notification.
    Notify777,
    /// BEL outside any sequence.
    Bell,
}

/// The answer of the screen rules to a [`crate::Effect::CheckScreen`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenVerdict {
    /// A rule with `confirms = "needs_you"` matched.
    NeedsYou,
    /// No rule matched.
    NoMatch,
}

/// One input to the state machine.
///
/// ```
/// use yien_agent::{HookEvent, Signal, Source};
///
/// assert_eq!(Signal::Hook(HookEvent::Stop).source(), Some(Source::Hooks));
/// assert_eq!(Signal::Tick.source(), None);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    /// A hook event from the structured channel.
    Hook(HookEvent),
    /// A sequence from the PTY.
    Osc(OscSignal),
    /// The result of a screen check the machine asked for.
    Screen(ScreenVerdict),
    /// The pane's process was (re)started.
    Spawned,
    /// The pane's process exited.
    Exited {
        /// Its exit code, when there is one.
        code: Option<i32>,
    },
    /// Whether the pane is seen: focused in a focused window (RN-07).
    Seen(bool),
    /// The user sent input to the pane.
    Input,
    /// The pane produced output (coalesced by the caller).
    Output,
    /// The structured channel cannot deliver (hook pipe down).
    SourceLost,
    /// A detection input that cannot be interpreted, such as a hook payload
    /// that fails to decode.
    Ambiguous,
    /// Time passed; sent at or after a [`crate::Effect::WakeAt`] deadline.
    Tick,
}

impl Signal {
    /// The detection source of the signal, or `None` for lifecycle, user and
    /// timer signals, which are always accepted.
    pub fn source(&self) -> Option<Source> {
        match self {
            Signal::Hook(_) => Some(Source::Hooks),
            Signal::Osc(OscSignal::Status7501(_)) => Some(Source::Osc7501),
            Signal::Osc(OscSignal::Prompt(_)) => Some(Source::Osc133),
            Signal::Osc(OscSignal::Notify9) => Some(Source::Osc9),
            Signal::Osc(OscSignal::Notify777) => Some(Source::Osc777),
            Signal::Osc(OscSignal::Bell) => Some(Source::Bel),
            Signal::Screen(_) => Some(Source::Screen),
            _ => None,
        }
    }
}
