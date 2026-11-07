//! Agent profiles, the signals that describe what an agent is doing, and the
//! pure state machine that decides each pane's agent state and whether it
//! needs the user (RF-AGE-05, RN-01, RN-02).
//!
//! Layer 1: no window, no OS calls, no clock. Time comes in as a value and
//! everything the outside world must do comes out as an [`Effect`].
//!
//! ```
//! use std::time::Duration;
//! use yien_agent::{AgentState, HookEvent, Machine, Signal, builtin_profiles};
//!
//! let profiles = builtin_profiles().unwrap();
//! let claude = profiles.iter().find(|p| p.id == "claude").unwrap();
//! let rule = claude.select_rule(None).rule;
//!
//! let ms = Duration::from_millis;
//! let m = Machine::new(rule.machine_config());
//! let (m, _) = m.step(&Signal::Spawned, ms(0));
//! let (m, _) = m.step(&Signal::Hook(HookEvent::UserPromptSubmit), ms(5));
//! let (m, _) = m.step(&Signal::Hook(HookEvent::Stop), ms(900));
//! assert_eq!(m.state(), AgentState::Done);
//! let (m, _) = m.step(&Signal::Seen(true), ms(1000));
//! assert_eq!(m.state(), AgentState::Idle);
//! ```

#![forbid(unsafe_code)]

mod hooks;
mod machine;
mod profile;
mod signal;
mod version;

pub use hooks::{
    CLAUDE_HOOK_EVENTS, HOOK_TIMEOUT_SECS, HOOK_TOKEN_ENV, HookError, HookTemplate,
    MAX_HOOK_PAYLOAD, decode_claude_hook, decode_claude_hook_or_event, render_claude_settings,
};
pub use machine::{
    AgentState, Cause, Effect, ErrorReason, Machine, MachineConfig, NEEDS_YOU_CONFIRM, NeedsReason,
};
pub use profile::{
    AgentKind, Confirms, Detect, Launch, MAX_PROFILE_BYTES, PROFILE_SCHEMA, Profile, ProfileError,
    Rule, RuleChoice, ScreenRule, SessionIdMode, builtin_profiles, evaluate_screen,
    resolve_profiles,
};
pub use signal::{
    AgentStatus, HookEvent, NotificationKind, OscSignal, PromptMark, ScreenVerdict,
    SessionEndReason, SessionStartSource, Signal, Source,
};
pub use version::{Version, VersionError, VersionReq};
