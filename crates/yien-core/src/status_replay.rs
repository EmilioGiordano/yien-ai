//! Real OSC 7501 captures, as the bundled pseudo console handed them on,
//! replayed through the terminal engine into each profile's state machine.
//! Paths in them name a placeholder user.

use std::time::Duration;

use yien_agent::{
    AgentState, Effect, ErrorReason, HookEvent, Machine, MachineConfig, NeedsReason, ScreenRule,
    ScreenVerdict, SessionEndReason, SessionStartSource, Signal, builtin_profiles, evaluate_screen,
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
/// A slow turn stopped by Ctrl+C, then Ctrl+C twice more to exit.
const CLAUDE_CTRL_C: &[u8] = include_bytes!("../tests/fixtures/osc7501/claude-2.1.325-ctrl-c.bin");
/// The same typed in PowerShell with YIEN's prompt marks.
const CLAUDE_CTRL_C_IN_POWERSHELL: &[u8] =
    include_bytes!("../tests/fixtures/osc7501/claude-2.1.325-ctrl-c-typed-in-powershell.bin");
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
fn rf_age_05_claude_ctrl_c_interrupt_is_idle_and_its_exit_changes_nothing() {
    assert_query_answered_in_order(CLAUDE_CTRL_C, "claude");
    // Ctrl+C mid-turn gives `idle`; two more exit with `clear`.
    assert_eq!(
        states(CLAUDE_CTRL_C, "claude"),
        [AgentState::Working, AgentState::Idle]
    );
}

#[test]
fn rf_age_05_claude_ctrl_c_typed_in_powershell_goes_idle_then_hands_back() {
    assert_query_answered_in_order(CLAUDE_CTRL_C_IN_POWERSHELL, "shell");
    assert_eq!(
        states(CLAUDE_CTRL_C_IN_POWERSHELL, "shell"),
        [
            // The shell runs `claude`, which settles at rest.
            AgentState::Working,
            AgentState::Idle,
            // The turn, and its interrupt.
            AgentState::Working,
            AgentState::Idle,
        ]
    );
    let end = replay(
        CLAUDE_CTRL_C_IN_POWERSHELL,
        config("shell"),
        CLAUDE_CTRL_C_IN_POWERSHELL.len(),
    );
    assert!(!end.machine.agent_reporting());
    assert_eq!(end.machine.state(), AgentState::Idle);
}

/// Replays `capture` into a Claude profile pane with its hooks alive, as
/// they came in the same run: `SessionStart` before the REPL reported,
/// `UserPromptSubmit` with the turn's `working`, no `Stop` for the
/// interrupt, and `SessionEnd` before the process exited.
/// Returns every state, and the state the capture left before the exit.
fn replay_hooked(capture: &[u8], chunk: usize, seen: bool) -> (Vec<AgentState>, AgentState) {
    let turn = position(capture, b"\x1b]7501;state=working").expect("a turn");
    let mut machine = Machine::new(config("claude"));
    let mut states = Vec::new();
    let mut now = Duration::ZERO;
    let mut step = |machine: &mut Machine, signal: Signal| {
        let (next, effects) = machine.clone().step(&signal, now);
        *machine = next;
        states.extend(effects.into_iter().filter_map(|e| match e {
            Effect::Transition { to, .. } => Some(to),
            _ => None,
        }));
        now += Duration::from_secs(1);
    };
    let mut engine = AlacrittyEngine::new(VtConfig::default(), 120, 40);
    step(&mut machine, Signal::Spawned);
    step(&mut machine, Signal::Seen(seen));
    step(
        &mut machine,
        Signal::Hook(HookEvent::SessionStart {
            session_id: Some("4f1c-9a".into()),
            source: SessionStartSource::Startup,
        }),
    );
    for (i, part) in [&capture[..turn], &capture[turn..]].into_iter().enumerate() {
        if i == 1 {
            step(&mut machine, Signal::Hook(HookEvent::UserPromptSubmit));
        }
        for bytes in part.chunks(chunk) {
            for found in &engine.feed(bytes).signals {
                if let Some(osc) = convert::osc_signal(found) {
                    step(&mut machine, Signal::Osc(osc));
                    step(&mut machine, Signal::Tick);
                }
            }
        }
    }
    let before_exit = machine.state();
    let end = HookEvent::SessionEnd(SessionEndReason::PromptInputExit);
    step(&mut machine, Signal::Hook(end));
    step(&mut machine, Signal::Exited { code: Some(0) });
    (states, before_exit)
}

#[test]
fn rf_age_05_claude_ctrl_c_in_a_hooked_pane_goes_idle_then_ends_cleanly() {
    for chunk in [1, 7, 64, CLAUDE_CTRL_C.len()] {
        // The hooks outrank OSC 7501, yet no hook tells the interrupt.
        let (states, before_exit) = replay_hooked(CLAUDE_CTRL_C, chunk, false);
        assert_eq!(before_exit, AgentState::Idle, "{chunk}-byte reads");
        // An unseen pane whose program ended shows it ended.
        assert_eq!(
            states,
            [AgentState::Working, AgentState::Idle, AgentState::Done],
            "{chunk}-byte reads"
        );
        let (states, before_exit) = replay_hooked(CLAUDE_CTRL_C, chunk, true);
        assert_eq!(before_exit, AgentState::Idle, "{chunk}-byte reads, seen");
        assert_eq!(
            states,
            [AgentState::Working, AgentState::Idle],
            "{chunk}-byte reads, seen"
        );
    }
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

/// Codex's folder trust and update prompt as its 0.167.2 sources render them.
const CODEX_TRUST: &str =
    include_str!("../../yien-agent/tests/fixtures/startup-screens/codex-0.167.2-trust.txt");
const CODEX_UPDATE: &str =
    include_str!("../../yien-agent/tests/fixtures/startup-screens/codex-0.167.2-update.txt");

/// Claude Code's folder trust, laid out from the 2.1.325 binary's strings.
const CLAUDE_TRUST: &str = " Accessing workspace:

 C:\\work\\new-project

 Quick safety check: Is this a project you created or one you trust?
 Claude Code'll be able to read, edit, and execute files here.

 Security guide

 \u{276f} No, exit
   Yes, I trust this folder

 Enter to confirm \u{b7} Esc to cancel";

const SCREEN_RED: AgentState = AgentState::NeedsYou(NeedsReason::ScreenRule);

/// A screen drawn from the top, as a full-screen program draws it.
fn drawn(text: &str) -> Vec<u8> {
    format!("\x1b[2J\x1b[H{}", text.replace('\n', "\r\n")).into_bytes()
}

/// The screen rules of the newest rule of a built-in profile.
fn screen_rules(profile: &str) -> Vec<ScreenRule> {
    let profiles = builtin_profiles().unwrap();
    let found = profiles.iter().find(|p| p.id == profile).unwrap();
    found.select_rule(None).rule.screen.clone()
}

fn read_screen(engine: &AlacrittyEngine, rules: &[ScreenRule]) -> ScreenVerdict {
    let frame = engine.frame();
    let rows: Vec<String> = (0..frame.lines.len()).map(|r| frame.line_text(r)).collect();
    let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
    evaluate_screen(rules, &rows)
}

/// A pane's machine, answering its screen reads from the engine.
struct Settled {
    machine: Machine,
    engine: AlacrittyEngine,
    rules: Vec<ScreenRule>,
    now: Duration,
    states: Vec<AgentState>,
}

impl Settled {
    fn step(&mut self, signal: Signal) {
        let mut queue = vec![signal];
        while let Some(signal) = queue.pop() {
            let (next, effects) = self.machine.clone().step(&signal, self.now);
            self.machine = next;
            for effect in effects {
                match effect {
                    Effect::Transition { to, .. } => self.states.push(to),
                    Effect::CheckScreen => {
                        queue.push(Signal::Screen(read_screen(&self.engine, &self.rules)));
                    }
                    _ => {}
                }
            }
        }
    }

    /// A spawned pane running `config`, reading the screen with `rules`.
    fn spawned(config: MachineConfig, rules: Vec<ScreenRule>) -> Self {
        let mut pane = Settled {
            machine: Machine::new(config),
            engine: AlacrittyEngine::new(VtConfig::default(), 120, 40),
            rules,
            now: Duration::ZERO,
            states: Vec::new(),
        };
        pane.step(Signal::Spawned);
        pane
    }

    /// One read of output, let settle.
    fn feed(&mut self, part: &[u8]) {
        let out = self.engine.feed(part);
        for found in &out.signals {
            let Some(osc) = convert::osc_signal(found) else {
                continue;
            };
            for signal in [Signal::Osc(osc), Signal::Tick] {
                self.step(signal);
                self.now += Duration::from_secs(1);
            }
        }
        self.step(Signal::Output);
        self.wait();
    }

    /// Lets the wake the machine asked for come.
    fn wait(&mut self) {
        if let Some(at) = self.machine.next_wake() {
            self.now = self.now.max(at);
            self.step(Signal::Tick);
        }
    }
}

/// Like [`replay`], with the output of each read let settle and the screen
/// read when the machine asks, as a pane does.
fn replay_settled(parts: &[&[u8]], profile: &str) -> Vec<AgentState> {
    let mut pane = Settled::spawned(config(profile), screen_rules(profile));
    for part in parts {
        pane.feed(part);
    }
    pane.states
}

#[test]
fn rf_age_05_codex_folder_trust_needs_you_until_its_first_status() {
    let trust = drawn(CODEX_TRUST);
    assert_eq!(
        replay_settled(&[&trust, CODEX_STARTUP], "codex"),
        [
            SCREEN_RED,
            AgentState::Idle,
            AgentState::Working,
            AgentState::Idle
        ]
    );
}

#[test]
fn rf_age_05_codex_update_then_trust_stays_red_until_answered() {
    let update = drawn(CODEX_UPDATE);
    let trust = drawn(CODEX_TRUST);
    let chat = drawn("\u{203a} Ask Codex to do anything");
    assert_eq!(
        replay_settled(&[&update, &trust, &chat], "codex"),
        [SCREEN_RED, AgentState::Idle]
    );
}

#[test]
fn rf_age_05_claude_folder_trust_needs_you_until_its_repl_reports() {
    let trust = drawn(CLAUDE_TRUST);
    assert_eq!(
        replay_settled(&[&trust, CLAUDE_INTERRUPT], "claude"),
        [
            SCREEN_RED,
            AgentState::Idle,
            AgentState::Working,
            AgentState::Idle
        ]
    );
}

#[test]
fn rn_02_real_sessions_never_show_a_startup_screen() {
    for (capture, profile) in [
        (CLAUDE_PERMISSION, "claude"),
        (CLAUDE_QUESTION, "claude"),
        (CLAUDE_ERROR, "claude"),
        (CLAUDE_INTERRUPT, "claude"),
        (CLAUDE_CTRL_C, "claude"),
        (CODEX_PERMISSION, "codex"),
        (CODEX_ERROR, "codex"),
        (CODEX_STARTUP, "codex"),
    ] {
        // Claude's first rule is its permission prompt, which a turn shows.
        let skip = usize::from(profile == "claude");
        let startup = &screen_rules(profile)[skip..];
        let mut engine = AlacrittyEngine::new(VtConfig::default(), 120, 40);
        for (i, bytes) in capture.chunks(64).enumerate() {
            engine.feed(bytes);
            assert_eq!(
                read_screen(&engine, startup),
                ScreenVerdict::NoMatch,
                "{profile}, read {i}"
            );
        }
        // Reading the screen as output settles changes none of their states.
        let parts: Vec<&[u8]> = capture.chunks(64).collect();
        assert_eq!(
            replay_settled(&parts, profile),
            states(capture, profile),
            "{profile}"
        );
    }
}

#[test]
fn rf_age_05_agents_typed_in_a_shell_report_as_in_their_own_pane() {
    for (capture, profile) in [
        (CODEX_PERMISSION, "codex"),
        (CODEX_STARTUP, "codex"),
        (OPENCODE_PERMISSION, "opencode"),
        (CLAUDE_QUESTION, "claude"),
        (CLAUDE_INTERRUPT, "claude"),
        (CLAUDE_CTRL_C, "claude"),
    ] {
        assert_eq!(
            states(capture, "shell"),
            states(capture, profile),
            "{profile}"
        );
    }
}

/// The patterns a shell pane reads for `program` typed in it.
fn typed_rules(program: &str) -> Vec<ScreenRule> {
    let profiles = builtin_profiles().unwrap();
    let found = profiles.iter().find(|p| p.id == program).unwrap();
    found.typed_startup_screens().to_vec()
}

/// PowerShell's prompt and the `codex` typed at it, with its marks.
const CODEX_TYPED: &[u8] = b"\x1b]133;A\x07PS C:\\work> codex\r\n\x1b]133;C\x07";

#[test]
fn rf_age_05_codex_typed_in_a_shell_needs_you_on_its_trust_screen_once_found() {
    let mut pane = Settled::spawned(config("shell"), typed_rules("codex"));
    pane.feed(CODEX_TYPED);
    pane.feed(&drawn(CODEX_TRUST));
    assert_eq!(pane.states, [AgentState::Working], "not read before found");
    // The poll finds `codex` under the shell a second later.
    pane.now += Duration::from_secs(1);
    pane.step(Signal::TypedAgent(true));
    pane.wait();
    assert_eq!(pane.states, [AgentState::Working, SCREEN_RED]);
    // Trusted: Codex starts and reports.
    pane.feed(CODEX_STARTUP);
    assert_eq!(
        pane.states,
        [
            AgentState::Working,
            SCREEN_RED,
            AgentState::Idle,
            AgentState::Working,
            AgentState::Idle
        ]
    );
    assert!(!pane.machine.reads_typed_agent());
}

#[test]
fn rn_02_agents_typed_in_a_shell_show_no_startup_screen_on_their_way_in() {
    for (capture, program) in [
        (CLAUDE_IN_POWERSHELL, "claude"),
        (CLAUDE_CTRL_C_IN_POWERSHELL, "claude"),
        (CLAUDE_QUESTION, "claude"),
        (CLAUDE_INTERRUPT, "claude"),
        (CODEX_PERMISSION, "codex"),
        (CODEX_STARTUP, "codex"),
    ] {
        // Found at once, so every read until its first record is made.
        let mut pane = Settled::spawned(config("shell"), typed_rules(program));
        pane.step(Signal::TypedAgent(true));
        for part in capture.chunks(64) {
            pane.feed(part);
        }
        assert_eq!(pane.states, states(capture, "shell"), "{program}");
    }
}
