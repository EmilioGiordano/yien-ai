//! Claude Code hooks through the core: launch arguments and environment,
//! hook frames over the real pipe driving the red mark, tokens per launch,
//! and the fallback when the pipe cannot start.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers outside #[test] fns are not covered by clippy.toml's in-test allowances"
)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::error::TryRecvError;
use yien_core::{Core, CoreConfig, CoreHandle, HookConfig, ShellCommand};
use yien_platform::hook_pipe::{HOOK_PIPE_ENV, HOOK_TOKEN_ENV};
use yien_proto::{
    AgentState, ClientMsg, Command, CoreMsg, Orientation, PaneId, PaneInfo, ProfileId, TabId,
};
use yien_pty::{FakeChild, FakePty};

const TIMEOUT: Duration = Duration::from_secs(5);

/// A settings folder of this test run's own, never a real profile.
fn scratch_dir() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    std::env::temp_dir().join(format!(
        "yien-core-hooks-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ))
}

fn config(hooks: Option<HookConfig>) -> CoreConfig {
    CoreConfig {
        shell: ShellCommand::new("fake-shell"),
        hooks,
        ..CoreConfig::default()
    }
}

/// Any existing file stands in for `yien-hook`: these tests send frames
/// themselves.
fn working_hooks() -> HookConfig {
    HookConfig {
        hook_exe: std::env::current_exe().unwrap(),
        settings_dir: scratch_dir(),
    }
}

struct Harness {
    core: CoreHandle,
    events: Receiver<CoreMsg>,
    backend: FakePty,
}

impl Harness {
    fn start(hooks: Option<HookConfig>) -> Self {
        let backend = FakePty::new();
        let (core, events) = Core::start(Arc::new(backend.clone()), config(hooks)).unwrap();
        Self {
            core,
            events,
            backend,
        }
    }

    fn command(&self, command: Command) {
        self.core.try_send(ClientMsg::Command(command)).unwrap();
    }

    fn wait_for<T>(&mut self, mut pick: impl FnMut(&CoreMsg) -> Option<T>) -> T {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match self.events.try_recv() {
                Ok(msg) => {
                    if let Some(found) = pick(&msg) {
                        return found;
                    }
                }
                Err(TryRecvError::Empty) => {
                    assert!(Instant::now() < deadline, "timed out waiting for an event");
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(TryRecvError::Disconnected) => panic!("the core stopped"),
            }
        }
    }

    fn child(&self) -> FakeChild {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(child) = self.backend.take_spawned().pop() {
                return child;
            }
            assert!(Instant::now() < deadline, "timed out waiting for a spawn");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// A project whose first pane runs `profile`.
    fn project(&mut self, profile: Option<&str>) -> (TabId, PaneId, FakeChild) {
        self.command(Command::NewProject {
            root: env!("CARGO_MANIFEST_DIR").into(),
            name: None,
        });
        let (tab, first) = self.wait_for(|m| match m {
            CoreMsg::TabUpdated(t) => Some((t.id, t.focused)),
            _ => None,
        });
        let first_child = self.child();
        let Some(profile) = profile else {
            return (tab, first, first_child);
        };
        let (pane, child) = self.split(first, profile);
        self.command(Command::ClosePane { pane: first });
        (tab, pane, child)
    }

    fn split(&mut self, pane: PaneId, profile: &str) -> (PaneId, FakeChild) {
        self.command(Command::SplitPane {
            pane,
            orientation: Orientation::Horizontal,
            profile: Some(ProfileId::new(profile)),
            before: false,
        });
        // A split focuses the new pane. Late updates of other panes may come
        // first, so the pane is not taken from them.
        let new = self.wait_for(|m| match m {
            CoreMsg::TabUpdated(t) if t.focused != pane && t.layout.panes().contains(&pane) => {
                Some(t.focused)
            }
            _ => None,
        });
        (new, self.child())
    }

    #[cfg(windows)]
    fn state(&mut self, pane: PaneId) -> AgentState {
        self.wait_for(|m| match m {
            CoreMsg::PaneState { pane: p, state } if *p == pane => Some(*state),
            _ => None,
        })
    }

    fn info(&mut self, pane: PaneId, mut until: impl FnMut(&PaneInfo) -> bool) -> PaneInfo {
        self.wait_for(|m| match m {
            CoreMsg::PaneUpdated(p) if p.id == pane && until(p) => Some(p.clone()),
            _ => None,
        })
    }

    /// Nothing about `pane`'s state arrives for a while.
    #[cfg(windows)]
    fn no_state_change(&mut self, pane: PaneId) {
        let until = Instant::now() + Duration::from_millis(600);
        while Instant::now() < until {
            match self.events.try_recv() {
                Ok(CoreMsg::PaneState { pane: p, state }) if p == pane => {
                    panic!("unexpected state {state:?}")
                }
                Ok(_) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
    }
}

fn env(child: &FakeChild, key: &str) -> Option<String> {
    child
        .spec()
        .env
        .iter()
        .rev()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.to_string_lossy().into_owned())
}

fn args(child: &FakeChild) -> Vec<String> {
    child
        .spec()
        .args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn rn_02_without_the_pipe_claude_launches_with_signal_lost_and_no_settings() {
    let mut h = Harness::start(Some(HookConfig {
        hook_exe: scratch_dir().join("missing-yien-hook.exe"),
        settings_dir: scratch_dir(),
    }));
    let message = h.wait_for(|m| match m {
        CoreMsg::Error {
            pane: None,
            message,
        } => Some(message.clone()),
        _ => None,
    });
    assert!(message.contains("state detection is off"), "{message}");
    assert!(
        message.ends_with(". Claude panes still run, without the red mark."),
        "{message}"
    );
    assert!(!message.contains("  "), "{message}");
    let (_, pane, child) = h.project(Some("claude"));
    assert_eq!(child.spec().program, "claude");
    let args = args(&child);
    assert_eq!(args.len(), 2, "{args:?}");
    assert_eq!(args[0], "--session-id");
    assert_eq!(env(&child, HOOK_TOKEN_ENV).as_deref(), Some(""));
    assert_eq!(env(&child, HOOK_PIPE_ENV).as_deref(), Some(""));
    let info = h.info(pane, |p| p.signal_lost);
    assert_eq!(info.state, AgentState::Idle);
    assert_eq!(info.session_id.as_deref(), Some(args[1].as_str()));
}

#[test]
fn rf_ses_01_the_preassigned_session_id_is_in_the_pane_model() {
    let mut h = Harness::start(None);
    let (_, pane, child) = h.project(Some("claude"));
    let args = args(&child);
    let id = args[1].clone();
    assert_eq!(args[0], "--session-id");
    assert_eq!(id.len(), 36);
    let info = h.info(pane, |p| p.session_id.is_some());
    assert_eq!(info.session_id, Some(id));
    assert_eq!(info.profile, "Claude Code");
}

#[test]
fn rf_age_01_a_shell_pane_gets_no_hooks_and_no_session_id() {
    let mut h = Harness::start(Some(working_hooks()));
    let (_, _, child) = h.project(None);
    assert!(args(&child).is_empty());
    assert_eq!(env(&child, HOOK_TOKEN_ENV).as_deref(), Some(""));
    assert_eq!(env(&child, HOOK_PIPE_ENV).as_deref(), Some(""));
}

#[test]
fn rf_pan_18_the_snapshot_lists_the_profiles_shell_first() {
    let mut h = Harness::start(None);
    let snapshot = h.wait_for(|m| match m {
        CoreMsg::WorkspaceSnapshot(s) => Some(s.clone()),
        _ => None,
    });
    let ids: Vec<&str> = snapshot.profiles.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids.first(), Some(&"shell"));
    assert!(ids.contains(&"claude"));
}

#[test]
fn rf_pan_18_a_new_tab_can_start_with_claude() {
    let mut h = Harness::start(None);
    h.command(Command::NewProject {
        root: env!("CARGO_MANIFEST_DIR").into(),
        name: None,
    });
    let project = h.wait_for(|m| match m {
        CoreMsg::ProjectUpdated(p) => Some(p.id),
        _ => None,
    });
    let shell = h.child();
    assert_eq!(shell.spec().program, "fake-shell");
    h.command(Command::NewTab {
        project,
        name: None,
        profile: Some(ProfileId::new("claude")),
    });
    let child = h.child();
    assert_eq!(child.spec().program, "claude");
}

#[cfg(windows)]
mod pipe {
    use yien_platform::hook_pipe::connect_client;
    use yien_proto::{NeedsReason, hook_frame};

    use super::*;

    const NEEDS: AgentState = AgentState::NeedsYou(NeedsReason::Permission);

    /// Sends one hook frame the way `yien-hook` does.
    fn send_hook(child: &FakeChild, event: &str, payload: &str) {
        use std::io::Write;
        let token = env(child, HOOK_TOKEN_ENV).unwrap();
        let pipe = env(child, HOOK_PIPE_ENV).unwrap();
        let frame = hook_frame::encode(token.as_bytes(), event, payload.as_bytes()).unwrap();
        let mut conn = connect_client(&pipe, Instant::now() + TIMEOUT).unwrap();
        conn.write_all(&frame).unwrap();
    }

    fn hook(child: &FakeChild, event: &str) {
        send_hook(
            child,
            event,
            &format!(r#"{{"hook_event_name":"{event}","session_id":"s1","tool_name":"Bash"}}"#),
        );
    }

    /// Prompt, then a permission request: red after the 300 ms confirmation.
    fn ask_permission(h: &mut Harness, pane: PaneId, child: &FakeChild) {
        hook(child, "UserPromptSubmit");
        assert_eq!(h.state(pane), AgentState::Working);
        hook(child, "PermissionRequest");
        assert_eq!(h.state(pane), NEEDS);
    }

    #[test]
    fn rf_age_01_claude_gets_its_settings_file_token_and_pipe() {
        let hooks = working_hooks();
        let dir = hooks.settings_dir.clone();
        let settings_file = hooks.settings_file();
        let mut h = Harness::start(Some(hooks));
        let (_, pane, child) = h.project(Some("claude"));
        let args = args(&child);
        assert_eq!(args[0], "--session-id");
        assert_eq!(args[2], "--settings");
        let settings = std::fs::read_to_string(&args[3]).unwrap();
        assert!(settings.contains("PermissionRequest"));
        assert!(!settings.contains(&env(&child, HOOK_TOKEN_ENV).unwrap()));
        assert!(PathBuf::from(&args[3]).starts_with(&dir));
        assert_eq!(PathBuf::from(&args[3]), settings_file);
        assert_eq!(env(&child, HOOK_TOKEN_ENV).unwrap().len(), 64);
        assert!(
            env(&child, HOOK_PIPE_ENV)
                .unwrap()
                .starts_with(r"\\.\pipe\yien-hook-")
        );
        let info = h.info(pane, |p| p.session_id.is_some());
        assert!(!info.signal_lost);
        drop(h);
        let deadline = Instant::now() + TIMEOUT;
        while PathBuf::from(&args[3]).exists() {
            assert!(Instant::now() < deadline, "the settings file stayed");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_batch_file_claude_starts_without_settings_its_path_cannot_carry() {
        let mut hooks = working_hooks();
        hooks.settings_dir = hooks.settings_dir.join("John (Work)");
        let mut h = Harness::start(Some(hooks));
        h.backend.batch_programs(true);
        let (_, pane, child) = h.project(Some("claude"));
        let message = h.wait_for(|m| match m {
            CoreMsg::Error {
                pane: Some(p),
                message,
            } if *p == pane => Some(message.clone()),
            _ => None,
        });
        assert!(message.contains("batch file"), "{message}");
        assert!(message.contains("without the red mark"), "{message}");
        let args = args(&child);
        assert_eq!(args.len(), 2, "{args:?}");
        assert_eq!(args[0], "--session-id");
        assert_eq!(env(&child, HOOK_TOKEN_ENV).as_deref(), Some(""));
        let info = h.info(pane, |p| p.signal_lost);
        assert!(info.running);
    }

    #[test]
    fn rf_atn_01_a_permission_request_turns_the_pane_red_and_answering_clears_it() {
        let mut h = Harness::start(Some(working_hooks()));
        let (_, pane, child) = h.project(Some("claude"));
        ask_permission(&mut h, pane, &child);
        let info = h.info(pane, |p| p.needs_you_since.is_some());
        assert_eq!(info.state, NEEDS);
        hook(&child, "PostToolUse");
        assert_eq!(h.state(pane), AgentState::Working);
        let info = h.info(pane, |p| p.needs_you_since.is_none());
        assert_eq!(info.state, AgentState::Working);
    }

    #[test]
    fn rf_atn_04_focus_and_window_focus_do_not_clear_the_red() {
        let mut h = Harness::start(Some(working_hooks()));
        let (_, pane, child) = h.project(Some("claude"));
        ask_permission(&mut h, pane, &child);
        h.core
            .try_send(ClientMsg::WindowFocus { focused: true })
            .unwrap();
        h.command(Command::FocusPane { pane });
        h.core
            .try_send(ClientMsg::WindowFocus { focused: false })
            .unwrap();
        h.no_state_change(pane);
    }

    #[test]
    fn rn_02_an_undecodable_hook_drops_to_idle_with_signal_lost() {
        let mut h = Harness::start(Some(working_hooks()));
        let (_, pane, child) = h.project(Some("claude"));
        ask_permission(&mut h, pane, &child);
        send_hook(&child, "", "not json");
        assert_eq!(h.state(pane), AgentState::Idle);
        assert!(h.info(pane, |p| p.signal_lost).signal_lost);
        hook(&child, "UserPromptSubmit");
        assert_eq!(h.state(pane), AgentState::Working);
        assert!(!h.info(pane, |p| !p.signal_lost).signal_lost);
    }

    #[test]
    fn rnf_11_a_relaunch_issues_a_new_token_and_refuses_the_old_one() {
        let mut h = Harness::start(Some(working_hooks()));
        let (_, pane, old) = h.project(Some("claude"));
        h.command(Command::RelaunchPane { pane });
        let new = h.child();
        assert_ne!(env(&old, HOOK_TOKEN_ENV), env(&new, HOOK_TOKEN_ENV));
        assert_ne!(args(&old)[1], args(&new)[1], "a fresh session id");
        hook(&old, "UserPromptSubmit");
        h.no_state_change(pane);
        hook(&new, "UserPromptSubmit");
        assert_eq!(h.state(pane), AgentState::Working);
    }

    #[test]
    fn rnf_11_a_hook_for_one_pane_never_reaches_another() {
        let mut h = Harness::start(Some(working_hooks()));
        let (_, a, a_child) = h.project(Some("claude"));
        let (b, b_child) = h.split(a, "claude");
        assert_ne!(env(&a_child, HOOK_TOKEN_ENV), env(&b_child, HOOK_TOKEN_ENV));
        ask_permission(&mut h, b, &b_child);
        h.no_state_change(a);
    }

    #[test]
    fn rf_atn_05_panes_are_numbered_in_the_order_they_need_you() {
        let mut h = Harness::start(Some(working_hooks()));
        let (_, a, a_child) = h.project(Some("claude"));
        let (b, b_child) = h.split(a, "claude");
        ask_permission(&mut h, b, &b_child);
        let first = h.info(b, |p| p.needs_you_since.is_some()).needs_you_since;
        ask_permission(&mut h, a, &a_child);
        let second = h.info(a, |p| p.needs_you_since.is_some()).needs_you_since;
        assert!(first < second, "{first:?} {second:?}");
    }

    #[test]
    fn rf_atn_06_selecting_a_tab_focuses_its_pane_that_needs_you() {
        let mut h = Harness::start(Some(working_hooks()));
        let (tab, a, _) = h.project(None);
        let (b, b_child) = h.split(a, "claude");
        ask_permission(&mut h, b, &b_child);
        h.command(Command::FocusPane { pane: a });
        h.wait_for(|m| match m {
            CoreMsg::TabUpdated(t) if t.id == tab && t.focused == a => Some(()),
            _ => None,
        });
        h.command(Command::SelectTab { tab });
        let focused = h.wait_for(|m| match m {
            CoreMsg::TabUpdated(t) if t.id == tab => Some(t.focused),
            _ => None,
        });
        assert_eq!(focused, b);
    }
}
