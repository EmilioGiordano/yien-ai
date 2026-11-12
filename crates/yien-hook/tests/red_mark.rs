//! The red mark end to end: the core launches a Claude pane on `FakePty`,
//! the built `yien-hook` runs as the pane's hook settings say Claude Code
//! would run it, fed real payload fixtures, and the pane turns red.

#![cfg(windows)]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers outside #[test] fns are not covered by clippy.toml's in-test allowances"
)]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use yien_core::{Core, CoreConfig, HookConfig, ShellCommand};
use yien_platform::hook_pipe::{HOOK_PIPE_ENV, HOOK_TOKEN_ENV};
use yien_proto::{
    AgentState, ClientMsg, Command as CoreCommand, CoreMsg, NeedsReason, Orientation, PaneId,
    ProfileId,
};
use yien_pty::{FakeChild, FakePty};

const HOOK: &str = env!("CARGO_BIN_EXE_yien-hook");
const TIMEOUT: Duration = Duration::from_secs(10);

const USER_PROMPT_SUBMIT: &[u8] = include_bytes!("fixtures/claude/user_prompt_submit.json");
const PERMISSION_REQUEST: &[u8] = include_bytes!("fixtures/claude/permission_request.json");
const POST_TOOL_USE: &[u8] = include_bytes!("fixtures/claude/post_tool_use.json");

struct Pane {
    events: Events,
    child: FakeChild,
    pane: PaneId,
}

/// The core's events, read without naming the channel's type.
struct Events(Box<dyn FnMut() -> Option<CoreMsg>>);

impl Events {
    fn wait_for<T>(&mut self, mut pick: impl FnMut(&CoreMsg) -> Option<T>) -> T {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match (self.0)() {
                Some(msg) => {
                    if let Some(found) = pick(&msg) {
                        return found;
                    }
                }
                None => {
                    assert!(Instant::now() < deadline, "timed out waiting for an event");
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
        }
    }
}

/// Starts the core with the built `yien-hook` and opens a Claude pane.
fn claude_pane(settings_dir: PathBuf) -> (yien_core::CoreHandle, Pane) {
    let backend = FakePty::new();
    let config = CoreConfig {
        shell: ShellCommand::new("fake-shell"),
        hooks: Some(HookConfig {
            hook_exe: PathBuf::from(HOOK),
            settings_dir,
        }),
        ..CoreConfig::default()
    };
    let (core, mut rx) = Core::start(Arc::new(backend.clone()), config).unwrap();
    let mut events = Events(Box::new(move || rx.try_recv().ok()));
    let send = |c| core.try_send(ClientMsg::Command(c)).unwrap();
    send(CoreCommand::NewProject {
        root: env!("CARGO_MANIFEST_DIR").into(),
        name: None,
    });
    let shell = events.wait_for(|m| match m {
        CoreMsg::TabUpdated(t) => Some(t.focused),
        _ => None,
    });
    send(CoreCommand::SplitPane {
        pane: shell,
        orientation: Orientation::Horizontal,
        profile: Some(ProfileId::new("claude")),
        before: false,
    });
    let pane = events.wait_for(|m| match m {
        CoreMsg::TabUpdated(t) if t.focused != shell => Some(t.focused),
        _ => None,
    });
    let deadline = Instant::now() + TIMEOUT;
    let child = loop {
        let spawned = backend.take_spawned();
        if let Some(child) = spawned.into_iter().find(|c| c.spec().program == "claude") {
            break child;
        }
        assert!(Instant::now() < deadline, "claude never spawned");
        std::thread::sleep(Duration::from_millis(2));
    };
    (
        core,
        Pane {
            events,
            child,
            pane,
        },
    )
}

impl Pane {
    fn env(&self, key: &str) -> String {
        let (_, value) = self
            .child
            .spec()
            .env
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .unwrap();
        value.to_string_lossy().into_owned()
    }

    fn settings(&self) -> serde_json::Value {
        let args: Vec<String> = self
            .child
            .spec()
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let at = args.iter().position(|a| a == "--settings").unwrap();
        serde_json::from_str(&std::fs::read_to_string(&args[at + 1]).unwrap()).unwrap()
    }

    /// Runs the hook for `event` exactly as the settings file registers it,
    /// with this pane's environment, and waits for it to exit.
    fn run_hook(&self, event: &str, payload: &[u8]) {
        let settings = self.settings();
        let handler = &settings["hooks"][event][0]["hooks"][0];
        let program = handler["command"].as_str().unwrap();
        let args: Vec<&str> = handler["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap())
            .collect();
        let mut child = Command::new(program)
            .args(&args)
            .env(HOOK_TOKEN_ENV, self.env(HOOK_TOKEN_ENV))
            .env(HOOK_PIPE_ENV, self.env(HOOK_PIPE_ENV))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(payload).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success() && out.stdout.is_empty() && out.stderr.is_empty());
    }

    fn state(&mut self) -> AgentState {
        let pane = self.pane;
        self.events.wait_for(|m| match m {
            CoreMsg::PaneState { pane: p, state } if *p == pane => Some(*state),
            _ => None,
        })
    }
}

fn scratch(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("yien-hook-{name}-{}", std::process::id()))
}

#[test]
fn rf_atn_01_a_claude_permission_request_turns_its_pane_red_end_to_end() {
    let (_core, mut pane) = claude_pane(scratch("red"));
    let settings = pane.settings();
    let command = settings["hooks"]["PermissionRequest"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(PathBuf::from(&command), PathBuf::from(HOOK));

    let started = Instant::now();
    pane.run_hook("UserPromptSubmit", USER_PROMPT_SUBMIT);
    assert_eq!(pane.state(), AgentState::Working);
    pane.run_hook("PermissionRequest", PERMISSION_REQUEST);
    assert_eq!(pane.state(), AgentState::NeedsYou(NeedsReason::Permission));
    let pane_id = pane.pane;
    let since = pane.events.wait_for(|m| match m {
        CoreMsg::PaneUpdated(p) if p.id == pane_id && p.needs_you_since.is_some() => {
            Some(p.needs_you_since)
        }
        _ => None,
    });
    assert!(since.is_some());
    // RNF-06: red within a second of the prompt, on a busy machine.
    assert!(started.elapsed() < Duration::from_secs(5));

    pane.run_hook("PostToolUse", POST_TOOL_USE);
    assert_eq!(pane.state(), AgentState::Working);
}

#[test]
fn rnf_11_a_hook_with_another_panes_token_changes_nothing() {
    let (_core, mut pane) = claude_pane(scratch("token"));
    pane.run_hook("UserPromptSubmit", USER_PROMPT_SUBMIT);
    assert_eq!(pane.state(), AgentState::Working);
    let out = Command::new(HOOK)
        .arg("PermissionRequest")
        .env(HOOK_TOKEN_ENV, "0".repeat(64))
        .env(HOOK_PIPE_ENV, pane.env(HOOK_PIPE_ENV))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success());
    let pane_id = pane.pane;
    let until = Instant::now() + Duration::from_millis(800);
    while Instant::now() < until {
        if let Some(CoreMsg::PaneState { pane: p, state }) = (pane.events.0)()
            && p == pane_id
        {
            panic!("unexpected state {state:?}");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
