//! Real OSC 7501 captures, as the bundled pseudo console handed them on,
//! replayed through the terminal engine into each profile's state machine.
//! Paths in them name a placeholder user.

use std::time::Duration;

use yien_agent::{
    AgentState, Effect, ErrorReason, Machine, MachineConfig, NeedsReason, Signal, builtin_profiles,
};
use yien_vt::osc_scan::STATUS_QUERY_REPLY;
use yien_vt::{AlacrittyEngine, VtConfig, VtEngine};

use crate::convert;

const CLAUDE_PERMISSION: &[u8] =
    include_bytes!("../tests/fixtures/osc7501/claude-2.1.323-permission.bin");
const CLAUDE_QUESTION: &[u8] =
    include_bytes!("../tests/fixtures/osc7501/claude-2.1.323-question.bin");
const CLAUDE_ERROR: &[u8] = include_bytes!("../tests/fixtures/osc7501/claude-2.1.323-error.bin");
const CLAUDE_INTERRUPT: &[u8] =
    include_bytes!("../tests/fixtures/osc7501/claude-2.1.323-interrupt.bin");
const CLAUDE_IN_POWERSHELL: &[u8] =
    include_bytes!("../tests/fixtures/osc7501/claude-2.1.325-typed-in-powershell.bin");
const CODEX_PERMISSION: &[u8] =
    include_bytes!("../tests/fixtures/osc7501/codex-0.167.2-permission.bin");
const CODEX_ERROR: &[u8] = include_bytes!("../tests/fixtures/osc7501/codex-0.167.2-error.bin");
const CODEX_STARTUP: &[u8] = include_bytes!("../tests/fixtures/osc7501/codex-0.167.2-startup.bin");
const OPENCODE_PERMISSION: &[u8] =
    include_bytes!("../tests/fixtures/osc7501/opencode-2.2.7-permission.bin");

const DA1_QUERY: &[u8] = b"\x1b[c";
const DA1_REPLY: &[u8] = b"\x1b[?6c";

/// The machine of the newest rule of a built-in profile, as a pane runs it.
fn config(profile: &str) -> MachineConfig {
    let profiles = builtin_profiles().unwrap();
    let found = profiles.iter().find(|p| p.id == profile).unwrap();
    found.select_rule(None).rule.machine_config()
}

struct Replay {
    /// Every state the pane went through, in order.
    states: Vec<AgentState>,
    /// The bytes written back to the program.
    replies: Vec<u8>,
    machine: Machine,
}

/// Feeds `capture` in `chunk`-byte reads to an unseen pane. Each signal is
/// followed by a second of quiet: in every capture a user answered a prompt
/// long after the "needs you" confirmation.
fn replay(capture: &[u8], config: MachineConfig, chunk: usize) -> Replay {
    let mut engine = AlacrittyEngine::new(VtConfig::default(), 120, 40);
    let mut machine = Machine::new(config)
        .step(&Signal::Spawned, Duration::ZERO)
        .0;
    let mut now = Duration::ZERO;
    let mut states = Vec::new();
    let mut replies = Vec::new();
    for bytes in capture.chunks(chunk) {
        let out = engine.feed(bytes);
        replies.extend_from_slice(&out.replies);
        for found in &out.signals {
            let Some(osc) = convert::osc_signal(found) else {
                continue;
            };
            for signal in [Signal::Osc(osc), Signal::Tick] {
                let (next, effects) = machine.step(&signal, now);
                machine = next;
                states.extend(effects.into_iter().filter_map(|e| match e {
                    Effect::Transition { to, .. } => Some(to),
                    _ => None,
                }));
                now += Duration::from_secs(1);
            }
        }
    }
    Replay {
        states,
        replies,
        machine,
    }
}

/// The same states for whole, byte-by-byte and odd-sized reads.
fn states(capture: &[u8], profile: &str) -> Vec<AgentState> {
    let whole = replay(capture, config(profile), capture.len()).states;
    for chunk in [1, 7, 64, 4096] {
        assert_eq!(
            replay(capture, config(profile), chunk).states,
            whole,
            "{chunk}-byte reads"
        );
    }
    whole
}

fn position(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

/// The status query was answered once, and ahead of the device attributes
/// the program asked for after it, however the reads were cut.
fn assert_query_answered_in_order(capture: &[u8], profile: &str) {
    let query = position(capture, b"\x1b]7501;?").expect("the capture asks");
    let asked_before = count(&capture[..query], DA1_QUERY);
    assert!(count(&capture[query..], DA1_QUERY) > 0);
    for chunk in [1, 7, 64, capture.len()] {
        let replies = replay(capture, config(profile), chunk).replies;
        assert_eq!(count(&replies, STATUS_QUERY_REPLY), 1, "{chunk}");
        let answer = position(&replies, STATUS_QUERY_REPLY).unwrap();
        assert_eq!(
            count(&replies[..answer], DA1_REPLY),
            asked_before,
            "{chunk}"
        );
    }
}

#[test]
fn rf_age_05_claude_answered_reports_a_permission_prompt() {
    assert_query_answered_in_order(CLAUDE_PERMISSION, "claude");
    assert_eq!(
        states(CLAUDE_PERMISSION, "claude"),
        [
            AgentState::Working,
            AgentState::NeedsYou(NeedsReason::Permission),
            AgentState::Working,
        ]
    );
}

#[test]
fn rf_age_05_claude_answered_reports_a_question_and_its_done() {
    assert_query_answered_in_order(CLAUDE_QUESTION, "claude");
    assert_eq!(
        states(CLAUDE_QUESTION, "claude"),
        [
            AgentState::Working,
            AgentState::NeedsYou(NeedsReason::Question),
            AgentState::Working,
            AgentState::Done,
        ]
    );
}

#[test]
fn rf_age_05_claude_failed_turn_is_an_error() {
    assert_eq!(
        states(CLAUDE_ERROR, "claude"),
        [
            AgentState::Working,
            AgentState::Error(ErrorReason::TurnFailed)
        ]
    );
}

#[test]
fn rf_age_05_claude_interrupted_turn_is_idle_not_done() {
    assert_eq!(
        states(CLAUDE_INTERRUPT, "claude"),
        [AgentState::Working, AgentState::Idle]
    );
}

#[test]
fn rf_age_05_claude_typed_in_powershell_reports_and_hands_back_on_exit() {
    assert_query_answered_in_order(CLAUDE_IN_POWERSHELL, "shell");
    assert_eq!(
        states(CLAUDE_IN_POWERSHELL, "shell"),
        [
            AgentState::Working,
            AgentState::NeedsYou(NeedsReason::Question),
            AgentState::Working,
            AgentState::Done,
            // `/exit` works for a moment, then clears.
            AgentState::Working,
            AgentState::Idle,
        ]
    );
    let end = replay(
        CLAUDE_IN_POWERSHELL,
        config("shell"),
        CLAUDE_IN_POWERSHELL.len(),
    );
    assert!(!end.machine.agent_reporting());
}

#[test]
fn rf_age_05_codex_reports_a_permission_prompt() {
    assert_eq!(
        states(CODEX_PERMISSION, "codex"),
        [
            AgentState::Working,
            AgentState::NeedsYou(NeedsReason::Permission),
            AgentState::Working,
            AgentState::Done,
        ]
    );
}

#[test]
fn rf_age_05_codex_error_holds_after_its_idle() {
    let replay = replay(CODEX_ERROR, config("codex"), CODEX_ERROR.len());
    assert_eq!(
        replay.states,
        [
            AgentState::Working,
            AgentState::Error(ErrorReason::TurnFailed)
        ]
    );
    // The capture goes on to Codex's `idle` after a key.
    assert_eq!(
        replay.machine.state(),
        AgentState::Error(ErrorReason::TurnFailed)
    );
    assert_eq!(states(CODEX_ERROR, "codex"), replay.states);
}

#[test]
fn rf_atn_08_codex_startup_work_is_not_a_finished_turn() {
    assert_eq!(
        states(CODEX_STARTUP, "codex"),
        [AgentState::Working, AgentState::Idle]
    );
}

#[test]
fn rf_atn_08_opencode_turn_ends_on_idle_and_ignores_session_records() {
    assert_query_answered_in_order(OPENCODE_PERMISSION, "opencode");
    assert_eq!(
        states(OPENCODE_PERMISSION, "opencode"),
        [
            AgentState::Working,
            AgentState::NeedsYou(NeedsReason::Permission),
            AgentState::Working,
            AgentState::Done,
            // Its exit.
            AgentState::Idle,
        ]
    );
}

#[test]
fn rf_age_05_agents_typed_in_a_shell_report_as_in_their_own_pane() {
    for (capture, profile) in [
        (CODEX_PERMISSION, "codex"),
        (CODEX_STARTUP, "codex"),
        (OPENCODE_PERMISSION, "opencode"),
        (CLAUDE_QUESTION, "claude"),
        (CLAUDE_INTERRUPT, "claude"),
    ] {
        assert_eq!(
            states(capture, "shell"),
            states(capture, profile),
            "{profile}"
        );
    }
}
