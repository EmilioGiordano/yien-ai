//! Transitions of the agent state machine (04 §4.5) and its red-mark rules.

use std::time::Duration;

use yien_agent::{
    AgentState, AgentStatus, BlockedKind, Cause, Effect, ErrorReason, HookEvent, Machine,
    MachineConfig, NEEDS_YOU_CONFIRM, NeedsReason, NotificationKind, OscSignal, PromptMark,
    ScreenVerdict, SessionEndReason, SessionStartSource, Signal, Source,
};
use yien_testkit::FakeClock;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn hook(e: HookEvent) -> Signal {
    Signal::Hook(e)
}

fn osc(o: OscSignal) -> Signal {
    Signal::Osc(o)
}

fn claude() -> MachineConfig {
    MachineConfig::new(&[Source::Hooks, Source::Osc7501, Source::Screen], None)
}

/// A machine plus a fake clock, checking after every step that the red mark
/// follows the state exactly.
struct Pane {
    m: Machine,
    clock: FakeClock,
    red: bool,
    fx: Vec<Effect>,
}

impl Pane {
    fn new(config: MachineConfig) -> Self {
        Self {
            m: Machine::new(config),
            clock: FakeClock::new(),
            red: false,
            fx: Vec::new(),
        }
    }

    fn send(&mut self, signal: Signal) -> &mut Self {
        let (m, fx) = self.m.clone().step(&signal, self.clock.now());
        self.m = m;
        track_red(&mut self.red, &fx);
        assert_eq!(
            self.red,
            self.m.is_red(),
            "red mark diverged after {signal:?}"
        );
        self.fx = fx;
        self
    }

    fn at(&mut self, t: Duration) -> &mut Self {
        let now = self.clock.now();
        if t > now {
            self.clock.advance(t - now);
        }
        self
    }

    fn state(&self) -> AgentState {
        self.m.state()
    }

    /// Claude with hooks alive, mid-turn.
    fn working_claude() -> Self {
        let mut p = Pane::new(claude());
        p.send(Signal::Spawned)
            .send(hook(HookEvent::SessionStart {
                session_id: Some("s1".into()),
                source: SessionStartSource::Startup,
            }))
            .send(hook(HookEvent::UserPromptSubmit))
            .send(hook(HookEvent::PreToolUse));
        assert_eq!(p.state(), AgentState::Working);
        p
    }

    fn needs_you_claude() -> Self {
        let mut p = Pane::working_claude();
        let t = p.clock.now();
        p.send(hook(HookEvent::PermissionRequest))
            .at(t + NEEDS_YOU_CONFIRM)
            .send(Signal::Tick);
        assert_eq!(p.state(), AgentState::NeedsYou(NeedsReason::Permission));
        p
    }
}

fn track_red(red: &mut bool, fx: &[Effect]) {
    for e in fx {
        match e {
            Effect::MarkRed => {
                assert!(!*red, "MarkRed while already red");
                *red = true;
            }
            Effect::ClearRed => {
                assert!(*red, "ClearRed while not red");
                *red = false;
            }
            _ => {}
        }
    }
    let marks = fx.iter().filter(|e| **e == Effect::MarkRed).count();
    let notifies = fx.iter().filter(|e| **e == Effect::Notify).count();
    assert_eq!(
        marks, notifies,
        "Notify must come with MarkRed and only then"
    );
}

#[test]
fn rf_age_05_permission_request_marks_red_after_confirmation() {
    let mut p = Pane::working_claude();
    p.at(ms(1000)).send(hook(HookEvent::PermissionRequest));
    assert_eq!(p.fx, vec![Effect::WakeAt(ms(1300))]);
    p.at(ms(1299)).send(Signal::Tick);
    assert_eq!(p.state(), AgentState::Working);
    p.at(ms(1300)).send(Signal::Tick);
    assert_eq!(
        p.fx,
        vec![
            Effect::Transition {
                from: AgentState::Working,
                to: AgentState::NeedsYou(NeedsReason::Permission),
                cause: Cause::Source(Source::Hooks),
            },
            Effect::MarkRed,
            Effect::Notify,
        ]
    );
}

#[test]
fn rnf_06_red_appears_within_one_second() {
    assert!(NEEDS_YOU_CONFIRM < Duration::from_secs(1));
}

#[test]
fn rf_age_05_needs_you_notifications_mark_red() {
    let cases = [
        (NotificationKind::PermissionPrompt, NeedsReason::Permission),
        (NotificationKind::Elicitation, NeedsReason::Question),
        (NotificationKind::AgentNeedsInput, NeedsReason::Question),
    ];
    for (kind, reason) in cases {
        let mut p = Pane::working_claude();
        p.at(ms(10)).send(hook(HookEvent::Notification(kind)));
        p.at(ms(400)).send(Signal::Tick);
        assert_eq!(p.state(), AgentState::NeedsYou(reason));
    }
}

#[test]
fn rf_age_05_contrary_signal_cancels_pending_red() {
    for contrary in [
        hook(HookEvent::PostToolUse),
        hook(HookEvent::PostToolUseFailure),
        hook(HookEvent::PreToolUse),
        hook(HookEvent::UserPromptSubmit),
    ] {
        let mut p = Pane::working_claude();
        p.at(ms(0)).send(hook(HookEvent::PermissionRequest));
        p.at(ms(100)).send(contrary);
        p.at(ms(300)).send(Signal::Tick);
        p.at(ms(5000)).send(Signal::Tick);
        assert_eq!(p.state(), AgentState::Working);
    }
}

#[test]
fn rf_age_05_second_needs_signal_keeps_the_first_deadline() {
    let mut p = Pane::working_claude();
    p.at(ms(0)).send(hook(HookEvent::PermissionRequest));
    p.at(ms(200)).send(hook(HookEvent::Notification(
        NotificationKind::PermissionPrompt,
    )));
    assert!(p.fx.is_empty());
    p.at(ms(300)).send(Signal::Tick);
    assert!(p.state().is_red());
}

#[test]
fn rf_age_05_post_tool_use_leaves_needs_you_at_once() {
    let mut p = Pane::needs_you_claude();
    p.send(hook(HookEvent::PostToolUse));
    assert_eq!(p.state(), AgentState::Working);
    assert!(p.fx.contains(&Effect::ClearRed));
}

#[test]
fn rf_age_05_input_followed_by_output_leaves_needs_you() {
    let mut p = Pane::needs_you_claude();
    p.send(Signal::Output);
    assert!(p.state().is_red(), "output alone is not an answer");
    p.send(Signal::Input);
    assert!(p.state().is_red(), "input alone is not an answer");
    p.send(Signal::Output);
    assert_eq!(p.state(), AgentState::Working);
    assert!(p.fx.contains(&Effect::Transition {
        from: AgentState::NeedsYou(NeedsReason::Permission),
        to: AgentState::Working,
        cause: Cause::User,
    }));
}

#[test]
fn rf_age_05_input_and_output_in_the_window_cancel_the_pending_red() {
    let mut p = Pane::working_claude();
    p.at(ms(0)).send(hook(HookEvent::PermissionRequest));
    p.at(ms(100)).send(Signal::Input);
    p.at(ms(150)).send(Signal::Output);
    p.at(ms(300)).send(Signal::Tick);
    p.at(ms(5000)).send(Signal::Tick);
    assert_eq!(p.state(), AgentState::Working);
    assert_eq!(p.m.next_wake(), None);
}

#[test]
fn rf_age_05_input_before_the_permission_request_does_not_cancel_it() {
    let mut p = Pane::working_claude();
    p.at(ms(0)).send(Signal::Input);
    p.send(hook(HookEvent::PermissionRequest));
    p.at(ms(100)).send(Signal::Output);
    p.at(ms(300)).send(Signal::Tick);
    assert!(p.state().is_red());
}

#[test]
fn rf_age_05_post_tool_use_failure_leaves_needs_you_at_once() {
    let mut p = Pane::needs_you_claude();
    p.send(hook(HookEvent::PostToolUseFailure));
    assert_eq!(p.state(), AgentState::Working);
    assert!(p.fx.contains(&Effect::ClearRed));
}

#[test]
fn rn_02_unattributed_permission_request_never_marks_red() {
    let mut p = Pane::working_claude();
    p.at(ms(0))
        .send(hook(HookEvent::UnattributedPermissionRequest));
    assert!(p.fx.is_empty());
    p.at(ms(1000)).send(Signal::Tick);
    assert_eq!(p.state(), AgentState::Working);
}

#[test]
fn next_wake_is_the_earliest_of_pending_red_and_watchdog() {
    let config = MachineConfig::new(&[Source::Hooks], Some(Duration::from_secs(30)));
    let mut p = Pane::new(config);
    assert_eq!(p.m.next_wake(), None);
    p.send(hook(HookEvent::UserPromptSubmit));
    assert_eq!(p.m.next_wake(), Some(ms(30_000)));
    p.at(ms(29_900)).send(hook(HookEvent::PermissionRequest));
    assert_eq!(p.m.next_wake(), Some(ms(30_000)));
    // The watchdog tick comes first and must re-arm the pending deadline.
    p.at(ms(30_000)).send(Signal::Tick);
    assert!(p.fx.contains(&Effect::WakeAt(ms(30_200))));
    assert_eq!(p.m.next_wake(), Some(ms(30_200)));
    p.at(ms(30_200)).send(Signal::Tick);
    assert!(p.state().is_red());
    assert_eq!(p.m.next_wake(), None);
}

#[test]
fn rf_age_05_input_before_the_red_does_not_count_as_an_answer() {
    let mut p = Pane::working_claude();
    p.at(ms(0)).send(hook(HookEvent::PermissionRequest));
    p.send(Signal::Input);
    p.at(ms(300)).send(Signal::Tick);
    p.send(Signal::Output);
    assert!(p.state().is_red());
}

#[test]
fn rf_atn_08_stop_while_not_seen_gives_done() {
    let mut p = Pane::working_claude();
    p.send(hook(HookEvent::Stop));
    assert_eq!(p.state(), AgentState::Done);
}

#[test]
fn rn_07_stop_while_seen_gives_idle() {
    let mut p = Pane::working_claude();
    p.send(Signal::Seen(true)).send(hook(HookEvent::Stop));
    assert_eq!(p.state(), AgentState::Idle);
}

#[test]
fn rf_atn_08_done_holds_until_the_pane_is_seen() {
    let mut p = Pane::working_claude();
    p.send(hook(HookEvent::Stop));
    p.at(ms(600_000))
        .send(Signal::Tick)
        .send(Signal::Seen(false));
    assert_eq!(p.state(), AgentState::Done);
    p.send(Signal::Seen(true));
    assert_eq!(p.state(), AgentState::Idle);
    assert!(p.fx.contains(&Effect::Transition {
        from: AgentState::Done,
        to: AgentState::Idle,
        cause: Cause::User,
    }));
}

#[test]
fn rf_age_07_seen_is_recorded() {
    let mut p = Pane::new(claude());
    assert!(!p.m.seen());
    p.send(Signal::Seen(true));
    assert!(p.m.seen());
    p.send(Signal::Seen(false));
    assert!(!p.m.seen());
}

#[test]
fn rf_atn_04_focus_and_time_do_not_clear_red() {
    let mut p = Pane::needs_you_claude();
    p.send(Signal::Seen(true))
        .send(Signal::Seen(false))
        .send(Signal::Seen(true));
    p.at(ms(3_600_000)).send(Signal::Tick);
    assert!(p.state().is_red());
}

#[test]
fn rf_age_05_stop_or_turn_end_clears_red() {
    let mut p = Pane::needs_you_claude();
    p.send(hook(HookEvent::Stop));
    assert_eq!(p.state(), AgentState::Done);
    assert!(p.fx.contains(&Effect::ClearRed));
}

#[test]
fn rf_age_05_stop_failure_gives_error() {
    for start in [Pane::working_claude(), Pane::needs_you_claude()] {
        let mut p = start;
        p.send(hook(HookEvent::StopFailure));
        assert_eq!(p.state(), AgentState::Error(ErrorReason::TurnFailed));
    }
}

#[test]
fn rf_age_05_error_is_left_by_a_new_prompt_or_session() {
    for leave in [
        hook(HookEvent::UserPromptSubmit),
        hook(HookEvent::SessionStart {
            session_id: None,
            source: SessionStartSource::Clear,
        }),
        Signal::Spawned,
    ] {
        let mut p = Pane::working_claude();
        p.send(hook(HookEvent::StopFailure));
        p.send(leave);
        assert!(matches!(p.state(), AgentState::Working | AgentState::Idle));
    }
}

#[test]
fn rf_age_05_unexpected_exit_mid_turn_gives_error() {
    for start in [Pane::working_claude(), Pane::needs_you_claude()] {
        let mut p = start;
        p.send(Signal::Exited { code: Some(3) });
        assert_eq!(
            p.state(),
            AgentState::Error(ErrorReason::UnexpectedExit { code: Some(3) })
        );
    }
}

#[test]
fn rf_age_05_exit_after_session_end_is_not_an_error() {
    let mut p = Pane::working_claude();
    p.send(hook(HookEvent::Stop))
        .send(Signal::Seen(true))
        .send(hook(HookEvent::SessionEnd(
            SessionEndReason::PromptInputExit,
        )))
        .send(Signal::Exited { code: Some(0) });
    assert_eq!(p.state(), AgentState::Idle);
}

#[test]
fn rf_age_08_unprofiled_program_exit_gives_done() {
    let mut p = Pane::new(MachineConfig::unprofiled());
    p.send(Signal::Spawned)
        .send(osc(OscSignal::Status7501(AgentStatus::Working)))
        .send(hook(HookEvent::PermissionRequest))
        .send(Signal::Output);
    assert_eq!(p.state(), AgentState::Idle, "no profile, no detection");
    p.send(Signal::Exited { code: Some(1) });
    assert_eq!(p.state(), AgentState::Done);
    p.send(Signal::Seen(true));
    assert_eq!(p.state(), AgentState::Idle);
}

#[test]
fn rf_age_08_unprofiled_exit_while_seen_gives_idle() {
    let mut p = Pane::new(MachineConfig::unprofiled());
    p.send(Signal::Spawned)
        .send(Signal::Seen(true))
        .send(Signal::Exited { code: None });
    assert_eq!(p.state(), AgentState::Idle);
}

#[test]
fn signals_after_exit_are_ignored_until_respawn() {
    let mut p = Pane::working_claude();
    p.send(hook(HookEvent::Stop))
        .send(Signal::Exited { code: Some(0) });
    p.send(hook(HookEvent::UserPromptSubmit))
        .send(hook(HookEvent::PermissionRequest));
    p.at(ms(10_000)).send(Signal::Tick);
    assert_eq!(p.state(), AgentState::Done);
    p.send(Signal::Spawned)
        .send(hook(HookEvent::UserPromptSubmit));
    assert_eq!(p.state(), AgentState::Working);
}

#[test]
fn rf_age_06_source_lost_goes_idle_with_the_diagnostic_mark() {
    let mut p = Pane::needs_you_claude();
    p.send(Signal::SourceLost);
    assert_eq!(p.state(), AgentState::Idle);
    assert!(p.fx.contains(&Effect::ClearRed));
    assert!(p.fx.contains(&Effect::Transition {
        from: AgentState::NeedsYou(NeedsReason::Permission),
        to: AgentState::Idle,
        cause: Cause::SignalLost,
    }));
    assert!(p.m.signal_lost());
    p.send(hook(HookEvent::UserPromptSubmit));
    assert!(!p.m.signal_lost(), "a strong signal clears the mark");
}

#[test]
fn rn_02_ambiguous_signal_goes_idle_never_red() {
    let mut p = Pane::working_claude();
    p.at(ms(0)).send(hook(HookEvent::PermissionRequest));
    p.send(Signal::Ambiguous);
    p.at(ms(1000)).send(Signal::Tick);
    assert_eq!(p.state(), AgentState::Idle);
    assert!(p.m.signal_lost());
}

#[test]
fn rn_02_needs_signal_outside_a_turn_does_not_mark_red() {
    let mut p = Pane::new(claude());
    p.send(Signal::Spawned)
        .send(hook(HookEvent::PermissionRequest))
        .send(osc(OscSignal::Status7501(AgentStatus::Blocked(None))));
    p.at(ms(1000)).send(Signal::Tick);
    assert_eq!(p.state(), AgentState::Idle);
}

#[test]
fn rn_02_screen_rule_cannot_mark_red_while_hooks_are_alive() {
    let config = MachineConfig::new(
        &[Source::Hooks, Source::Screen],
        Some(Duration::from_secs(30)),
    );
    let mut p = Pane::new(config);
    p.send(Signal::Spawned)
        .send(hook(HookEvent::UserPromptSubmit));
    p.at(ms(30_000)).send(Signal::Tick);
    assert!(!p.fx.contains(&Effect::CheckScreen));
    p.send(Signal::Screen(ScreenVerdict::NeedsYou));
    p.at(ms(60_000)).send(Signal::Tick);
    assert_eq!(p.state(), AgentState::Working);
}

#[test]
fn rn_02_unsolicited_screen_match_never_marks_red() {
    let mut p = Pane::new(MachineConfig::new(&[Source::Osc7501, Source::Screen], None));
    p.send(osc(OscSignal::Status7501(AgentStatus::Working)))
        .send(Signal::Screen(ScreenVerdict::NeedsYou));
    p.at(ms(1000)).send(Signal::Tick);
    assert_eq!(p.state(), AgentState::Working);
}

#[test]
fn rn_02_screen_rule_cannot_mark_red_after_signal_lost() {
    let config = MachineConfig::new(&[Source::Osc9, Source::Screen], None);
    let mut p = Pane::new(config);
    p.send(Signal::SourceLost);
    p.send(osc(OscSignal::Notify9));
    p.send(Signal::Screen(ScreenVerdict::NeedsYou));
    p.at(ms(1000)).send(Signal::Tick);
    assert!(!p.state().is_red());
}

#[test]
fn rn_02_pty_notifications_alone_never_change_the_state() {
    let config = MachineConfig::new(
        &[Source::Osc9, Source::Osc777, Source::Bel, Source::Screen],
        None,
    );
    let mut p = Pane::new(config);
    p.send(Signal::Spawned);
    for signal in [
        osc(OscSignal::Notify9),
        osc(OscSignal::Notify777),
        osc(OscSignal::Bell),
        Signal::Screen(ScreenVerdict::NeedsYou),
    ] {
        p.send(signal);
        assert!(p.fx.is_empty());
    }
    p.at(ms(1000)).send(Signal::Tick);
    assert_eq!(p.state(), AgentState::Idle);
}

#[test]
fn rf_age_05_watchdog_screen_rule_confirms_needs_you_without_hooks() {
    let config = MachineConfig::new(
        &[Source::Osc7501, Source::Screen],
        Some(Duration::from_secs(30)),
    );
    let mut p = Pane::new(config);
    p.send(osc(OscSignal::Status7501(AgentStatus::Working)));
    p.at(ms(30_000)).send(Signal::Tick);
    assert!(p.fx.contains(&Effect::CheckScreen));
    p.send(Signal::Screen(ScreenVerdict::NeedsYou));
    assert_eq!(p.fx, vec![Effect::WakeAt(ms(30_300))]);
    p.at(ms(30_300)).send(Signal::Tick);
    assert_eq!(p.state(), AgentState::NeedsYou(NeedsReason::ScreenRule));
    assert!(p.fx.contains(&Effect::Transition {
        from: AgentState::Working,
        to: AgentState::NeedsYou(NeedsReason::ScreenRule),
        cause: Cause::Source(Source::Screen),
    }));
}

#[test]
fn rf_age_05_watchdog_rechecks_but_never_reds_by_time() {
    let config = MachineConfig::new(
        &[Source::Osc7501, Source::Screen],
        Some(Duration::from_secs(30)),
    );
    let mut p = Pane::new(config);
    p.send(osc(OscSignal::Status7501(AgentStatus::Working)));
    assert_eq!(p.fx.last(), Some(&Effect::WakeAt(ms(30_000))));
    p.at(ms(10_000)).send(Signal::Output);
    p.at(ms(30_000)).send(Signal::Tick);
    assert_eq!(
        p.fx,
        vec![Effect::WakeAt(ms(40_000))],
        "activity pushes the watchdog"
    );
    p.at(ms(40_000)).send(Signal::Tick);
    assert_eq!(p.fx, vec![Effect::CheckScreen, Effect::WakeAt(ms(70_000))]);
    p.send(Signal::Screen(ScreenVerdict::NoMatch));
    for minute in 1..=60 {
        p.at(ms(minute * 60_000)).send(Signal::Tick);
    }
    assert_eq!(p.state(), AgentState::Working);
}

#[test]
fn adr_08_lower_priority_source_is_ignored_once_hooks_are_alive() {
    let mut p = Pane::working_claude();
    p.at(ms(0))
        .send(osc(OscSignal::Status7501(AgentStatus::Blocked(None))));
    p.at(ms(1000)).send(Signal::Tick);
    assert_eq!(p.state(), AgentState::Working);
    p.send(osc(OscSignal::Status7501(AgentStatus::Done)));
    assert_eq!(p.state(), AgentState::Working);
}

#[test]
fn adr_08_osc_7501_drives_the_machine_without_hooks() {
    let mut p = Pane::new(claude());
    p.send(Signal::Spawned)
        .send(osc(OscSignal::Status7501(AgentStatus::Working)));
    p.at(ms(0))
        .send(osc(OscSignal::Status7501(AgentStatus::Blocked(None))));
    p.at(ms(300)).send(Signal::Tick);
    assert_eq!(p.state(), AgentState::NeedsYou(NeedsReason::Blocked));
    p.send(osc(OscSignal::Status7501(AgentStatus::Working)));
    assert_eq!(p.state(), AgentState::Working);
    p.send(osc(OscSignal::Status7501(AgentStatus::Done)));
    assert_eq!(p.state(), AgentState::Done);
}

#[test]
fn undeclared_sources_are_ignored() {
    let mut p = Pane::new(claude());
    p.send(osc(OscSignal::Prompt(PromptMark::CommandStart)))
        .send(osc(OscSignal::Notify9));
    assert_eq!(p.state(), AgentState::Idle);
}

#[test]
fn rn_05_session_start_reports_the_exact_session_id() {
    let mut p = Pane::new(claude());
    p.send(hook(HookEvent::SessionStart {
        session_id: Some("1b9d-77".into()),
        source: SessionStartSource::Resume,
    }));
    assert_eq!(p.fx, vec![Effect::SessionId("1b9d-77".into())]);
}

#[test]
fn rn_04_session_end_reports_its_reason() {
    let mut p = Pane::working_claude();
    p.send(hook(HookEvent::SessionEnd(SessionEndReason::Logout)));
    assert!(p.fx.contains(&Effect::SessionEnded(SessionEndReason::Logout)));
    assert_eq!(p.state(), AgentState::Idle);
}

#[test]
fn session_start_from_needs_you_clears_red() {
    let mut p = Pane::needs_you_claude();
    p.send(hook(HookEvent::SessionStart {
        session_id: Some("s2".into()),
        source: SessionStartSource::Clear,
    }));
    assert_eq!(p.state(), AgentState::Idle);
    assert!(p.fx.contains(&Effect::ClearRed));
}

#[test]
fn rnf_08_compact_mid_turn_keeps_the_turn() {
    for source in [SessionStartSource::Compact, SessionStartSource::Resume] {
        let mut p = Pane::working_claude();
        p.at(ms(0)).send(hook(HookEvent::SessionStart {
            session_id: Some("s2".into()),
            source,
        }));
        assert_eq!(p.state(), AgentState::Working);
        assert_eq!(p.fx, vec![Effect::SessionId("s2".into())]);
        p.send(hook(HookEvent::PermissionRequest));
        p.at(ms(300)).send(Signal::Tick);
        assert_eq!(p.state(), AgentState::NeedsYou(NeedsReason::Permission));
        p.send(hook(HookEvent::SessionStart {
            session_id: Some("s3".into()),
            source,
        }));
        assert!(p.state().is_red(), "compaction keeps the red mark");
        p.send(hook(HookEvent::PostToolUse))
            .send(hook(HookEvent::Stop));
        assert_eq!(p.state(), AgentState::Done);
    }
}

#[test]
fn session_start_startup_or_clear_mid_turn_goes_idle() {
    for source in [SessionStartSource::Startup, SessionStartSource::Clear] {
        let mut p = Pane::working_claude();
        p.send(hook(HookEvent::SessionStart {
            session_id: Some("s2".into()),
            source,
        }));
        assert_eq!(p.state(), AgentState::Idle);
    }
}

#[test]
fn session_start_compact_outside_a_turn_goes_idle() {
    let mut p = Pane::working_claude();
    p.send(hook(HookEvent::Stop))
        .send(hook(HookEvent::SessionStart {
            session_id: Some("s2".into()),
            source: SessionStartSource::Compact,
        }));
    assert_eq!(p.state(), AgentState::Idle);
}

#[test]
fn respawn_resets_and_clears_red() {
    let mut p = Pane::needs_you_claude();
    p.send(Signal::Spawned);
    assert_eq!(p.state(), AgentState::Idle);
    assert!(p.fx.contains(&Effect::ClearRed));
    assert!(!p.m.signal_lost());
}

#[test]
fn rf_age_05_shell_prompt_marks_drive_working_and_done() {
    let mut p = Pane::new(MachineConfig::new(&[Source::Osc133], None));
    p.send(Signal::Spawned)
        .send(osc(OscSignal::Prompt(PromptMark::PromptStart)));
    assert_eq!(p.state(), AgentState::Idle);
    p.send(osc(OscSignal::Prompt(PromptMark::CommandStart)));
    assert_eq!(p.state(), AgentState::Working);
    p.send(osc(OscSignal::Prompt(PromptMark::CommandEnd)));
    assert_eq!(p.state(), AgentState::Done);
    p.send(Signal::Seen(true))
        .send(osc(OscSignal::Prompt(PromptMark::CommandStart)))
        .send(osc(OscSignal::Prompt(PromptMark::PromptStart)));
    assert_eq!(p.state(), AgentState::Idle);
}

// ---- OSC 7501 ----------------------------------------------------------------

fn status(s: AgentStatus) -> Signal {
    osc(OscSignal::Status7501(s))
}

fn blocked(kind: Option<BlockedKind>) -> Signal {
    status(AgentStatus::Blocked(kind))
}

/// Codex from 0.163: OSC 7501 alone, `done` ends its turns.
fn codex() -> MachineConfig {
    MachineConfig::new(&[Source::Osc7501], None)
}

/// OpenCode from 2.2.7: OSC 7501 alone, `idle` ends its turns.
fn opencode() -> MachineConfig {
    MachineConfig::new(&[Source::Osc7501], None).with_idle_ends_turn(true)
}

/// The shell rule: its own marks, and the status of a CLI typed in it.
fn shell() -> MachineConfig {
    MachineConfig::new(&[Source::Osc133, Source::Osc7501], None).with_idle_ends_turn(true)
}

fn mark(m: PromptMark) -> Signal {
    osc(OscSignal::Prompt(m))
}

impl Pane {
    fn spawned(config: MachineConfig) -> Self {
        let mut p = Pane::new(config);
        p.send(Signal::Spawned);
        p
    }

    /// Waits out the "needs you" confirmation.
    fn confirm(&mut self) -> &mut Self {
        let t = self.clock.now() + NEEDS_YOU_CONFIRM;
        self.at(t).send(Signal::Tick)
    }
}

#[test]
fn rf_age_05_osc_7501_turn_with_a_permission_prompt() {
    let mut p = Pane::spawned(codex());
    p.send(status(AgentStatus::Working));
    assert_eq!(p.state(), AgentState::Working);
    p.send(blocked(Some(BlockedKind::Permission)));
    assert_eq!(
        p.state(),
        AgentState::Working,
        "not before the confirmation"
    );
    p.confirm();
    assert_eq!(p.state(), AgentState::NeedsYou(NeedsReason::Permission));
    assert!(p.fx.contains(&Effect::MarkRed));
    p.send(status(AgentStatus::Working));
    assert_eq!(p.state(), AgentState::Working);
    assert!(p.fx.contains(&Effect::ClearRed));
    p.send(status(AgentStatus::Done));
    assert_eq!(p.state(), AgentState::Done);
    assert!(p.fx.contains(&Effect::Transition {
        from: AgentState::Working,
        to: AgentState::Done,
        cause: Cause::Source(Source::Osc7501),
    }));
}

#[test]
fn rf_atn_09_osc_7501_kind_is_the_reason() {
    for (kind, reason) in [
        (Some(BlockedKind::Permission), NeedsReason::Permission),
        (Some(BlockedKind::Question), NeedsReason::Question),
        (Some(BlockedKind::Auth), NeedsReason::Auth),
        (None, NeedsReason::Blocked),
    ] {
        let mut p = Pane::spawned(codex());
        p.send(status(AgentStatus::Working)).send(blocked(kind));
        p.confirm();
        assert_eq!(p.state(), AgentState::NeedsYou(reason));
    }
}

#[test]
fn rf_age_05_osc_7501_working_within_the_window_cancels_the_red() {
    let mut p = Pane::spawned(codex());
    p.send(status(AgentStatus::Working))
        .send(blocked(Some(BlockedKind::Permission)));
    p.at(ms(100)).send(status(AgentStatus::Working));
    p.at(ms(1000)).send(Signal::Tick);
    assert_eq!(p.state(), AgentState::Working);
}

#[test]
fn rf_age_05_a_repeated_blocked_record_keeps_its_deadline() {
    // Claude sends the same state again when only its message changes.
    let mut p = Pane::spawned(codex());
    p.send(status(AgentStatus::Working));
    p.at(ms(0)).send(blocked(Some(BlockedKind::Question)));
    p.at(ms(200)).send(blocked(Some(BlockedKind::Question)));
    p.at(ms(300)).send(Signal::Tick);
    assert_eq!(p.state(), AgentState::NeedsYou(NeedsReason::Question));
}

#[test]
fn rf_age_05_osc_7501_blocked_with_no_turn_running_still_asks() {
    // Codex asks its async questions once the turn has stopped; OpenCode
    // shows a permission asked from outside while it rests.
    for before in [None, Some(AgentStatus::Done)] {
        let mut p = Pane::spawned(codex());
        if let Some(end) = before {
            p.send(status(AgentStatus::Working)).send(status(end));
            assert_eq!(p.state(), AgentState::Done);
        }
        p.send(blocked(Some(BlockedKind::Question)));
        assert_eq!(p.state(), AgentState::Working);
        p.confirm();
        assert_eq!(p.state(), AgentState::NeedsYou(NeedsReason::Question));
    }
}

#[test]
fn rf_atn_08_opencode_idle_after_working_ends_the_turn() {
    let mut p = Pane::spawned(opencode());
    p.send(status(AgentStatus::Idle));
    assert_eq!(p.state(), AgentState::Idle);
    p.send(status(AgentStatus::Working))
        .send(status(AgentStatus::Idle));
    assert_eq!(p.state(), AgentState::Done);
    p.send(Signal::Seen(true));
    assert_eq!(p.state(), AgentState::Idle);
    // Seen while the turn ends: Idle at once.
    p.send(status(AgentStatus::Working))
        .send(status(AgentStatus::Idle));
    assert_eq!(p.state(), AgentState::Idle);
}

#[test]
fn rf_atn_08_opencode_idle_after_blocked_ends_the_turn() {
    let mut p = Pane::spawned(opencode());
    p.send(status(AgentStatus::Working))
        .send(blocked(Some(BlockedKind::Permission)));
    p.confirm();
    assert!(p.state().is_red());
    p.send(status(AgentStatus::Idle));
    assert_eq!(p.state(), AgentState::Done);
    assert!(p.fx.contains(&Effect::ClearRed));
}

#[test]
fn rf_age_05_idle_with_no_turn_running_stays_idle() {
    for config in [codex(), opencode(), shell()] {
        let mut p = Pane::spawned(config);
        p.send(status(AgentStatus::Idle));
        assert!(p.fx.is_empty());
        assert_eq!(p.state(), AgentState::Idle);
    }
}

#[test]
fn rf_age_05_codex_idle_after_working_is_an_interrupt_not_a_done() {
    // Esc, or Codex settling after its startup `working`, unseen.
    let mut p = Pane::spawned(codex());
    p.send(status(AgentStatus::Working))
        .send(status(AgentStatus::Idle));
    assert_eq!(p.state(), AgentState::Idle);
    p.send(status(AgentStatus::Working))
        .send(blocked(None))
        .confirm();
    p.send(status(AgentStatus::Idle));
    assert_eq!(p.state(), AgentState::Idle);
    assert!(p.fx.contains(&Effect::ClearRed));
}

#[test]
fn rf_atn_08_codex_done_then_idle_keeps_done_until_seen() {
    // Codex turns its done into idle on any key or click.
    let mut p = Pane::spawned(codex());
    p.send(status(AgentStatus::Working))
        .send(status(AgentStatus::Done))
        .send(status(AgentStatus::Idle));
    assert_eq!(p.state(), AgentState::Done);
    p.send(Signal::Seen(true));
    assert_eq!(p.state(), AgentState::Idle);
}

#[test]
fn rf_age_05_osc_7501_error_holds_against_idle_and_clears_when_seen() {
    for config in [codex(), opencode()] {
        let mut p = Pane::spawned(config);
        p.send(status(AgentStatus::Working))
            .send(status(AgentStatus::Error));
        assert_eq!(p.state(), AgentState::Error(ErrorReason::TurnFailed));
        p.send(status(AgentStatus::Idle));
        assert_eq!(p.state(), AgentState::Error(ErrorReason::TurnFailed));
        p.send(Signal::Seen(true));
        assert_eq!(p.state(), AgentState::Idle);
        assert!(p.fx.contains(&Effect::Transition {
            from: AgentState::Error(ErrorReason::TurnFailed),
            to: AgentState::Idle,
            cause: Cause::User,
        }));
    }
}

#[test]
fn rf_age_05_osc_7501_error_needs_a_turn() {
    let mut p = Pane::spawned(codex());
    p.send(status(AgentStatus::Error));
    assert_eq!(p.state(), AgentState::Idle);
}

#[test]
fn rf_age_05_every_error_clears_when_the_pane_is_seen() {
    let mut p = Pane::working_claude();
    p.send(hook(HookEvent::StopFailure));
    p.send(Signal::Seen(true));
    assert_eq!(p.state(), AgentState::Idle);
    let mut p = Pane::working_claude();
    p.send(Signal::Exited { code: Some(3) });
    assert!(matches!(p.state(), AgentState::Error(_)));
    p.send(Signal::Seen(true));
    assert_eq!(p.state(), AgentState::Idle);
}

#[test]
fn rf_age_05_an_error_while_seen_shows_until_the_pane_is_seen_again() {
    let mut p = Pane::spawned(codex());
    p.send(Signal::Seen(true))
        .send(status(AgentStatus::Working))
        .send(status(AgentStatus::Error));
    assert_eq!(p.state(), AgentState::Error(ErrorReason::TurnFailed));
    p.send(Signal::Seen(false)).send(Signal::Seen(true));
    assert_eq!(p.state(), AgentState::Idle);
}

#[test]
fn rf_age_05_osc_7501_clear_goes_idle_from_any_state() {
    let mut p = Pane::spawned(codex());
    p.send(status(AgentStatus::Working))
        .send(blocked(None))
        .confirm();
    p.send(status(AgentStatus::Clear));
    assert_eq!(p.state(), AgentState::Idle);
    assert!(p.fx.contains(&Effect::ClearRed));
    for end in [AgentStatus::Done, AgentStatus::Error] {
        let mut p = Pane::spawned(codex());
        p.send(status(AgentStatus::Working)).send(status(end));
        p.send(status(AgentStatus::Clear));
        assert_eq!(p.state(), AgentState::Idle);
    }
}

#[test]
fn rf_age_05_a_killed_agent_without_clear_errors_on_exit() {
    let mut p = Pane::spawned(codex());
    p.send(status(AgentStatus::Working));
    p.send(Signal::Exited { code: Some(1) });
    assert_eq!(
        p.state(),
        AgentState::Error(ErrorReason::UnexpectedExit { code: Some(1) })
    );
}

#[test]
fn adr_08_hooks_outrank_osc_7501_in_a_claude_pane() {
    let mut p = Pane::working_claude();
    for s in [
        AgentStatus::Idle,
        AgentStatus::Clear,
        AgentStatus::Error,
        AgentStatus::Done,
    ] {
        p.send(status(s));
        assert_eq!(p.state(), AgentState::Working, "{s:?}");
    }
    assert!(!p.m.agent_reporting());
}

#[test]
fn rf_age_05_a_shell_without_an_agent_follows_its_marks() {
    let mut p = Pane::spawned(shell());
    p.send(mark(PromptMark::CommandStart));
    assert_eq!(p.state(), AgentState::Working);
    p.send(mark(PromptMark::CommandEnd));
    assert_eq!(p.state(), AgentState::Done);
    assert!(!p.m.agent_reporting());
}

#[test]
fn rf_age_05_an_agent_typed_in_a_shell_takes_over_until_clear() {
    let mut p = Pane::spawned(shell());
    // `claude` typed: the shell marks a running command.
    p.send(mark(PromptMark::CommandStart));
    assert_eq!(p.state(), AgentState::Working);
    // The agent's first record: at rest, not a finished turn.
    p.send(status(AgentStatus::Idle));
    assert_eq!(p.state(), AgentState::Idle);
    assert!(p.m.agent_reporting());
    assert!(p.fx.contains(&Effect::Transition {
        from: AgentState::Working,
        to: AgentState::Idle,
        cause: Cause::Source(Source::Osc7501),
    }));
    // The shell's marks no longer drive the state.
    p.send(mark(PromptMark::CommandStart));
    assert_eq!(p.state(), AgentState::Idle);
    p.send(status(AgentStatus::Working))
        .send(blocked(Some(BlockedKind::Question)))
        .confirm();
    assert_eq!(p.state(), AgentState::NeedsYou(NeedsReason::Question));
    p.send(status(AgentStatus::Working))
        .send(status(AgentStatus::Done));
    assert_eq!(p.state(), AgentState::Done);
    // `/exit`: a last working, then clear hands the pane back.
    p.send(status(AgentStatus::Working))
        .send(status(AgentStatus::Clear))
        .send(status(AgentStatus::Clear));
    assert_eq!(p.state(), AgentState::Idle);
    assert!(!p.m.agent_reporting());
    p.send(mark(PromptMark::CommandEnd))
        .send(mark(PromptMark::PromptStart));
    assert_eq!(p.state(), AgentState::Idle);
    p.send(mark(PromptMark::CommandStart));
    assert_eq!(p.state(), AgentState::Working, "the shell drives again");
}

#[test]
fn rf_age_05_an_agent_killed_in_a_shell_mid_turn_is_an_error() {
    for turn in [
        vec![status(AgentStatus::Working)],
        vec![status(AgentStatus::Working), blocked(None)],
    ] {
        let mut p = Pane::spawned(shell());
        p.send(mark(PromptMark::CommandStart))
            .send(status(AgentStatus::Idle));
        for s in turn {
            p.send(s);
        }
        p.confirm();
        // No clear: the shell's prompt coming back is the only sign.
        p.send(mark(PromptMark::CommandEnd));
        assert_eq!(
            p.state(),
            AgentState::Error(ErrorReason::UnexpectedExit { code: None })
        );
        assert!(!p.m.agent_reporting());
        p.send(mark(PromptMark::PromptStart));
        assert!(matches!(p.state(), AgentState::Error(_)));
        p.send(mark(PromptMark::CommandStart));
        assert_eq!(p.state(), AgentState::Working, "the shell drives again");
    }
}

#[test]
fn rf_age_05_an_agent_killed_in_a_shell_at_rest_leaves_its_state() {
    for (end, expected) in [
        (AgentStatus::Idle, AgentState::Idle),
        (AgentStatus::Done, AgentState::Done),
        (
            AgentStatus::Error,
            AgentState::Error(ErrorReason::TurnFailed),
        ),
    ] {
        let mut p = Pane::spawned(MachineConfig::new(&[Source::Osc133, Source::Osc7501], None));
        p.send(mark(PromptMark::CommandStart))
            .send(status(AgentStatus::Idle))
            .send(status(AgentStatus::Working))
            .send(status(end));
        assert_eq!(p.state(), expected, "{end:?}");
        p.send(mark(PromptMark::PromptStart));
        assert_eq!(p.state(), expected, "{end:?}");
        assert!(!p.m.agent_reporting());
    }
}

#[test]
fn rf_atn_08_opencode_typed_in_a_shell_ends_turns_on_idle() {
    let mut p = Pane::spawned(shell());
    p.send(mark(PromptMark::CommandStart))
        .send(status(AgentStatus::Idle))
        .send(status(AgentStatus::Working))
        .send(status(AgentStatus::Idle));
    assert_eq!(p.state(), AgentState::Done);
}

#[test]
fn rf_age_05_a_respawned_shell_forgets_the_agent() {
    let mut p = Pane::spawned(shell());
    p.send(status(AgentStatus::Idle));
    assert!(p.m.agent_reporting());
    p.send(Signal::Exited { code: Some(0) })
        .send(Signal::Spawned);
    assert!(!p.m.agent_reporting());
    p.send(mark(PromptMark::CommandStart));
    assert_eq!(p.state(), AgentState::Working);
}

#[test]
fn rf_age_05_osc_7501_takes_nothing_over_without_osc_133() {
    let mut p = Pane::spawned(codex());
    p.send(status(AgentStatus::Idle));
    assert!(!p.m.agent_reporting());
}

// ---- Exhaustive and random sequences ----------------------------------------

/// One step of a generated sequence: a signal and how far the clock moves
/// before it.
type Step = (Signal, u64);

fn needs_you_signals() -> Vec<Step> {
    vec![
        (hook(HookEvent::PermissionRequest), 0),
        (
            hook(HookEvent::Notification(NotificationKind::PermissionPrompt)),
            0,
        ),
        (
            hook(HookEvent::Notification(NotificationKind::Elicitation)),
            0,
        ),
        (
            hook(HookEvent::Notification(NotificationKind::AgentNeedsInput)),
            0,
        ),
        (osc(OscSignal::Status7501(AgentStatus::Blocked(None))), 0),
        (
            osc(OscSignal::Status7501(AgentStatus::Blocked(Some(
                BlockedKind::Auth,
            )))),
            0,
        ),
        (Signal::Screen(ScreenVerdict::NeedsYou), 0),
    ]
}

fn other_signals() -> Vec<Step> {
    vec![
        (Signal::Spawned, 0),
        (Signal::Exited { code: Some(1) }, 0),
        (Signal::Seen(true), 0),
        (Signal::Seen(false), 0),
        (Signal::Input, 0),
        (Signal::Output, 0),
        (Signal::SourceLost, 0),
        (Signal::Ambiguous, 0),
        (Signal::Tick, 300),
        (Signal::Tick, 31_000),
        (
            hook(HookEvent::SessionStart {
                session_id: Some("s".into()),
                source: SessionStartSource::Startup,
            }),
            0,
        ),
        (hook(HookEvent::UserPromptSubmit), 0),
        (hook(HookEvent::PreToolUse), 0),
        (hook(HookEvent::PostToolUse), 0),
        (hook(HookEvent::PostToolUseFailure), 0),
        (hook(HookEvent::UnattributedPermissionRequest), 0),
        (hook(HookEvent::Notification(NotificationKind::Other)), 0),
        (hook(HookEvent::Stop), 0),
        (hook(HookEvent::StopFailure), 0),
        (hook(HookEvent::SessionEnd(SessionEndReason::Other)), 0),
        (osc(OscSignal::Status7501(AgentStatus::Working)), 0),
        (osc(OscSignal::Status7501(AgentStatus::Done)), 0),
        (osc(OscSignal::Status7501(AgentStatus::Idle)), 0),
        (osc(OscSignal::Status7501(AgentStatus::Error)), 0),
        (osc(OscSignal::Status7501(AgentStatus::Clear)), 0),
        (osc(OscSignal::Prompt(PromptMark::CommandStart)), 0),
        (osc(OscSignal::Prompt(PromptMark::CommandEnd)), 0),
        (osc(OscSignal::Notify9), 0),
        (osc(OscSignal::Bell), 0),
        (Signal::Screen(ScreenVerdict::NoMatch), 0),
    ]
}

fn configs() -> Vec<MachineConfig> {
    let every = [
        Source::Hooks,
        Source::Osc7501,
        Source::Osc133,
        Source::Osc9,
        Source::Osc777,
        Source::Bel,
        Source::Screen,
    ];
    vec![
        claude(),
        MachineConfig::new(&every, Some(Duration::from_secs(30))),
        MachineConfig::new(
            &[Source::Osc7501, Source::Osc9, Source::Screen],
            Some(Duration::from_secs(30)),
        ),
        MachineConfig::new(&[Source::Osc133], None),
        shell(),
        opencode(),
        MachineConfig::unprofiled(),
    ]
}

/// Runs a sequence, checking the red invariant at every step; returns whether
/// red was ever marked.
fn run(config: &MachineConfig, steps: &[&Step]) -> bool {
    let mut m = Machine::new(config.clone());
    let mut now = Duration::ZERO;
    let mut red = false;
    let mut ever_red = false;
    for (signal, dt) in steps {
        now += ms(*dt);
        let (next, fx) = m.step(signal, now);
        m = next;
        track_red(&mut red, &fx);
        assert_eq!(
            red,
            m.is_red(),
            "red diverged after {signal:?} in {steps:?}"
        );
        ever_red |= fx.contains(&Effect::MarkRed);
    }
    ever_red
}

fn for_each_sequence(alphabet: &[Step], max_len: usize, f: &mut dyn FnMut(&[&Step])) {
    fn go<'a>(
        alphabet: &'a [Step],
        left: usize,
        seq: &mut Vec<&'a Step>,
        f: &mut dyn FnMut(&[&Step]),
    ) {
        f(seq);
        if left == 0 {
            return;
        }
        for step in alphabet {
            seq.push(step);
            go(alphabet, left - 1, seq, f);
            seq.pop();
        }
    }
    go(alphabet, max_len, &mut Vec::new(), f);
}

struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn walk<'a>(&mut self, alphabet: &'a [Step], len: usize) -> Vec<&'a Step> {
        (0..len)
            .map(|_| &alphabet[(self.next() % alphabet.len() as u64) as usize])
            .collect()
    }
}

#[test]
fn rn_01_red_mark_follows_the_state_in_every_short_sequence() {
    let mut alphabet = other_signals();
    alphabet.extend(needs_you_signals());
    let config = MachineConfig::new(
        &[Source::Hooks, Source::Osc7501, Source::Osc9, Source::Screen],
        Some(Duration::from_secs(30)),
    );
    let mut count = 0usize;
    for_each_sequence(&alphabet, 3, &mut |seq| {
        run(&config, seq);
        count += 1;
    });
    assert!(count > 30_000);
}

#[test]
fn rn_01_red_mark_follows_the_state_in_long_random_walks() {
    let mut alphabet = other_signals();
    alphabet.extend(needs_you_signals());
    let mut rng = XorShift(0x0123_4567_89ab_cdef);
    let mut ever_red = false;
    for config in configs() {
        for _ in 0..400 {
            let walk = rng.walk(&alphabet, 120);
            ever_red |= run(&config, &walk);
        }
    }
    assert!(
        ever_red,
        "the walks must reach the red state to test anything"
    );
}

#[test]
fn rn_01_no_needs_you_signal_never_marks_red() {
    let alphabet = other_signals();
    for config in configs() {
        for_each_sequence(&alphabet, 4, &mut |seq| {
            assert!(
                !run(&config, seq),
                "red without a needs-you signal: {seq:?}"
            );
        });
    }
    let mut rng = XorShift(0xdead_beef_cafe_f00d);
    for config in configs() {
        for _ in 0..500 {
            let walk = rng.walk(&alphabet, 150);
            assert!(
                !run(&config, &walk),
                "red without a needs-you signal: {walk:?}"
            );
        }
    }
}

#[test]
fn rn_02_ambiguous_and_weak_signals_never_mark_red_while_hooks_are_alive() {
    // Hooks alive: PTY hints, screen verdicts, losses and undecodable
    // signals are all weaker than the structured channel.
    let weak: Vec<Step> = vec![
        (osc(OscSignal::Status7501(AgentStatus::Blocked(None))), 0),
        (osc(OscSignal::Notify9), 0),
        (osc(OscSignal::Notify777), 0),
        (osc(OscSignal::Bell), 0),
        (Signal::Screen(ScreenVerdict::NeedsYou), 0),
        (hook(HookEvent::Notification(NotificationKind::Other)), 0),
        (Signal::Ambiguous, 0),
        (Signal::SourceLost, 0),
        (Signal::Input, 0),
        (Signal::Output, 0),
        (Signal::Tick, 300),
        (Signal::Tick, 31_000),
        (hook(HookEvent::UserPromptSubmit), 0),
        (hook(HookEvent::PreToolUse), 0),
    ];
    let every = [
        Source::Hooks,
        Source::Osc7501,
        Source::Osc9,
        Source::Osc777,
        Source::Bel,
        Source::Screen,
    ];
    let config = MachineConfig::new(&every, Some(Duration::from_secs(30)));
    let start: Step = (hook(HookEvent::UserPromptSubmit), 0);
    for_each_sequence(&weak, 4, &mut |seq| {
        let mut full = vec![&start];
        full.extend_from_slice(seq);
        assert!(!run(&config, &full), "weak signals marked red: {seq:?}");
    });
}

#[test]
fn rn_02_after_signal_lost_only_a_strong_signal_can_mark_red() {
    let weak: Vec<Step> = vec![
        (osc(OscSignal::Notify9), 0),
        (osc(OscSignal::Bell), 0),
        (Signal::Screen(ScreenVerdict::NeedsYou), 0),
        (Signal::Screen(ScreenVerdict::NoMatch), 0),
        (Signal::Input, 0),
        (Signal::Output, 0),
        (Signal::Seen(true), 0),
        (Signal::Tick, 300),
        (Signal::Tick, 31_000),
    ];
    let config = MachineConfig::new(
        &[Source::Osc9, Source::Bel, Source::Screen],
        Some(Duration::from_secs(30)),
    );
    let lost: Step = (Signal::SourceLost, 0);
    for_each_sequence(&weak, 5, &mut |seq| {
        let mut full = vec![&lost];
        full.extend_from_slice(seq);
        assert!(!run(&config, &full), "red after signal lost: {seq:?}");
    });
}
