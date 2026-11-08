//! The core end to end over `FakePty`: commands in, PTY traffic and events
//! out, with real time.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers outside #[test] fns are not covered by clippy.toml's in-test allowances"
)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::error::TryRecvError;
use yien_agent::Profile;
use yien_core::{Core, CoreConfig, CoreHandle, SendError, ShellCommand, ShutdownError};
use yien_proto::frame::Frame;
use yien_proto::{
    AgentState, ClientMsg, Command, CoreMsg, InputBytes, NeedsReason, Orientation, PaneId,
    ProfileId, ProjectId, TabId, TerminalPalette,
};
use yien_pty::{FakeChild, FakeInput, FakePty};
use yien_vt::Palette;

const TIMEOUT: Duration = Duration::from_secs(5);

fn config() -> CoreConfig {
    CoreConfig {
        shell: ShellCommand::new("fake-shell"),
        ..CoreConfig::default()
    }
}

struct Harness {
    core: CoreHandle,
    events: Receiver<CoreMsg>,
    backend: FakePty,
}

impl Harness {
    fn start() -> Self {
        Self::start_with(config())
    }

    fn start_with(config: CoreConfig) -> Self {
        let backend = FakePty::new();
        let (core, events) = Core::start(Arc::new(backend.clone()), config).unwrap();
        Self {
            core,
            events,
            backend,
        }
    }

    fn send(&self, msg: ClientMsg) {
        self.core.try_send(msg).unwrap();
    }

    fn command(&self, command: Command) {
        self.send(ClientMsg::Command(command));
    }

    /// Sends input, waiting while the pane's queue is full.
    fn input(&self, pane: PaneId, bytes: &[u8]) {
        let deadline = Instant::now() + TIMEOUT;
        let mut msg = ClientMsg::Input {
            pane,
            bytes: InputBytes::new(bytes.to_vec()),
        };
        loop {
            match self.core.try_send(msg) {
                Ok(()) => return,
                Err(SendError::Busy(back)) => msg = back,
                Err(e) => panic!("{e}"),
            }
            assert!(Instant::now() < deadline, "the pane stayed busy");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Reads events until `pick` accepts one; drains everything before it.
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

    fn frame_ready(&mut self, pane: PaneId) {
        self.wait_for(|m| match m {
            CoreMsg::PaneFrameReady { pane: p } if *p == pane => Some(()),
            _ => None,
        });
    }

    fn state(&mut self, pane: PaneId) -> AgentState {
        self.wait_for(|m| match m {
            CoreMsg::PaneState { pane: p, state } if *p == pane => Some(*state),
            _ => None,
        })
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

    /// A project on this crate's folder: its tab, first pane and child.
    fn project(&mut self) -> (TabId, PaneId, FakeChild) {
        self.command(Command::NewProject {
            root: env!("CARGO_MANIFEST_DIR").into(),
            name: None,
        });
        let (tab, pane) = self.wait_for(|m| match m {
            CoreMsg::TabUpdated(t) => Some((t.id, t.focused)),
            _ => None,
        });
        (tab, pane, self.child())
    }

    fn split(&mut self, pane: PaneId, profile: Option<&str>) -> (PaneId, FakeChild) {
        self.command(Command::SplitPane {
            pane,
            orientation: Orientation::Horizontal,
            profile: profile.map(ProfileId::new),
        });
        let new = self.wait_for(|m| match m {
            CoreMsg::PaneUpdated(p) if p.id != pane => Some(p.id),
            _ => None,
        });
        (new, self.child())
    }

    /// Takes frames of `pane` as they are announced until one shows `text`.
    fn frame_with(&mut self, pane: PaneId, text: &str) -> Frame {
        self.frame_where(pane, |f| f.text().contains(text))
    }

    /// Takes frames of `pane` as they are announced until `pick` accepts one.
    fn frame_where(&mut self, pane: PaneId, pick: impl Fn(&Frame) -> bool) -> Frame {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(frame) = self.core.take_frame(pane)
                && pick(&frame)
            {
                return frame;
            }
            assert!(Instant::now() < deadline, "no frame matched");
            self.frame_ready(pane);
        }
    }
}

fn next_input(child: &FakeChild) -> FakeInput {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        if let Some(input) = child.recv_input(Duration::from_millis(50)) {
            return input;
        }
    }
    panic!("timed out waiting for input");
}

fn next_bytes(child: &FakeChild) -> Vec<u8> {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        if let FakeInput::Bytes(bytes) = next_input(child) {
            return bytes;
        }
    }
    panic!("timed out waiting for bytes");
}

fn wait_until(mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn first_event_is_an_empty_snapshot() {
    let mut h = Harness::start();
    let snapshot = h.wait_for(|m| match m {
        CoreMsg::WorkspaceSnapshot(s) => Some(s.clone()),
        _ => None,
    });
    assert!(snapshot.projects.is_empty() && snapshot.active_tab.is_none());
}

#[test]
fn rf_esp_01_new_project_starts_the_shell_in_its_folder() {
    let mut h = Harness::start();
    let (_, _, child) = h.project();
    assert_eq!(child.spec().program, "fake-shell");
    assert_eq!(
        child.spec().cwd.as_deref(),
        Some(std::path::Path::new(env!("CARGO_MANIFEST_DIR")))
    );
}

#[test]
fn rf_esp_01_a_missing_folder_is_refused() {
    let mut h = Harness::start();
    h.command(Command::NewProject {
        root: "Z:/surely/not/here".into(),
        name: None,
    });
    let message = h.wait_for(|m| match m {
        CoreMsg::Error { message, .. } => Some(message.clone()),
        _ => None,
    });
    assert!(message.contains("not a folder"), "{message}");
    assert!(h.backend.take_spawned().is_empty());
}

#[test]
fn rf_pan_18_split_spawns_the_chosen_profile() {
    let mut h = Harness::start();
    let (_, first, _) = h.project();
    let (_, child) = h.split(first, Some("claude"));
    assert_eq!(child.spec().program, "claude");
    assert!(
        child
            .spec()
            .args
            .iter()
            .all(|a| !a.to_string_lossy().contains('{'))
    );
}

#[test]
fn rf_pan_20_close_terminates_and_removes_the_pane() {
    let mut h = Harness::start();
    let (_, first, _) = h.project();
    let (second, child) = h.split(first, None);
    h.command(Command::ClosePane { pane: second });
    let closed = h.wait_for(|m| match m {
        CoreMsg::PaneClosed { pane } => Some(*pane),
        _ => None,
    });
    assert_eq!(closed, second);
    wait_until(|| child.was_killed());
}

#[test]
fn rf_pan_03_focus_moves_to_the_named_pane() {
    let mut h = Harness::start();
    let (tab, first, _) = h.project();
    let (_second, _) = h.split(first, None);
    h.command(Command::FocusPane { pane: first });
    let focused = h.wait_for(|m| match m {
        CoreMsg::TabUpdated(t) if t.id == tab && t.focused == first => Some(t.focused),
        _ => None,
    });
    assert_eq!(focused, first);
}

#[test]
fn rf_pan_05_swap_keeps_each_program_with_its_pane() {
    let mut h = Harness::start();
    let (tab, a, child_a) = h.project();
    let (b, child_b) = h.split(a, None);
    h.command(Command::SwapPanes { a, b });
    let layout = h.wait_for(|m| match m {
        CoreMsg::TabUpdated(t) if t.id == tab && t.layout.panes() == [b, a] => {
            Some(t.layout.clone())
        }
        _ => None,
    });
    assert_eq!(layout.panes(), [b, a]);
    h.input(a, b"to-a");
    h.input(b, b"to-b");
    assert_eq!(next_bytes(&child_a), b"to-a");
    assert_eq!(next_bytes(&child_b), b"to-b");
    assert!(h.backend.take_spawned().is_empty());
}

#[test]
fn rf_age_02_input_reaches_the_program_unaltered() {
    let mut h = Harness::start();
    let (_, pane, child) = h.project();
    let bytes = b"\x1b[A\r\n\xff\x00\xe2\x82\xac".to_vec();
    h.input(pane, &bytes);
    assert_eq!(next_bytes(&child), bytes);
}

#[test]
fn input_to_a_pane_that_does_not_read_is_busy_and_the_pane_still_closes() {
    let mut h = Harness::start();
    let (_, first, first_child) = h.project();
    let (pane, child) = h.split(first, None);
    let mut accepted = 0;
    let refused = loop {
        let msg = ClientMsg::Input {
            pane,
            bytes: InputBytes::new(b"x".to_vec()),
        };
        match h.core.try_send(msg) {
            Ok(()) => accepted += 1,
            Err(SendError::Busy(msg)) => break msg,
            Err(e) => panic!("{e}"),
        }
        assert!(accepted < 10_000, "input was never refused");
    };
    assert!(matches!(refused, ClientMsg::Input { pane: p, .. } if p == pane));

    h.input(first, b"still served");
    assert_eq!(next_bytes(&first_child), b"still served");
    h.command(Command::ClosePane { pane });
    h.wait_for(|m| match m {
        CoreMsg::PaneClosed { pane: p } if *p == pane => Some(()),
        _ => None,
    });
    wait_until(|| child.was_killed());
}

#[test]
fn rf_pan_06_output_produces_a_frame_with_the_text() {
    let mut h = Harness::start();
    let (_, pane, child) = h.project();
    child.output(b"hello from the child");
    let frame = h.frame_with(pane, "hello from the child");
    assert!(frame.text().starts_with("hello"));
}

#[test]
fn frames_are_coalesced_while_output_streams() {
    let mut h = Harness::start_with(CoreConfig {
        frame_interval: Duration::from_millis(200),
        ..config()
    });
    let (_, pane, child) = h.project();
    for i in 0..200 {
        child.output(format!("line {i}\r\n").as_bytes());
    }
    let mut frames = 0;
    loop {
        h.frame_ready(pane);
        frames += 1;
        let frame = h.core.take_frame(pane);
        if frame.is_some_and(|f| f.text().contains("line 199")) {
            break;
        }
    }
    assert!(frames <= 3, "{frames} frames for 200 chunks");
}

#[test]
fn an_untaken_frame_is_replaced_and_announced_once() {
    let mut h = Harness::start_with(CoreConfig {
        frame_interval: Duration::from_millis(10),
        ..config()
    });
    let (_, pane, child) = h.project();
    h.frame_ready(pane);
    child.output(b"one");
    std::thread::sleep(Duration::from_millis(100));
    child.output(b" two");
    std::thread::sleep(Duration::from_millis(100));
    let mut notices = 0;
    while let Ok(msg) = h.events.try_recv() {
        if matches!(msg, CoreMsg::PaneFrameReady { pane: p } if p == pane) {
            notices += 1;
        }
    }
    assert_eq!(notices, 0, "the first notice stands until a take");
    assert_eq!(h.core.take_frame(pane).unwrap().text(), "one two");
    assert!(h.core.take_frame(pane).is_none());

    child.output(b" three");
    assert!(h.frame_with(pane, "three").text().ends_with("three"));
}

#[test]
fn rf_pan_14_frames_carry_the_terminal_modes() {
    let mut h = Harness::start();
    let (_, pane, child) = h.project();
    child.output(b"\x1b[?2004h\x1b[?1h\x1b[?1000h\x1b[?1006hmodes");
    let modes = h.frame_with(pane, "modes").modes;
    assert!(modes.bracketed_paste && modes.app_cursor);
    assert_eq!(modes.mouse, yien_proto::frame::MouseTracking::Click);
    assert_eq!(modes.mouse_encoding, yien_proto::frame::MouseEncoding::Sgr);
}

#[test]
fn an_open_synchronized_update_shows_after_its_timeout() {
    let mut h = Harness::start();
    let (_, pane, child) = h.project();
    let start = Instant::now();
    child.output(b"\x1b[?2026hheld back");
    h.frame_with(pane, "held back");
    assert!(
        start.elapsed() >= yien_vt::SYNC_UPDATE_TIMEOUT - Duration::from_millis(20),
        "shown after {:?}",
        start.elapsed()
    );
}

#[test]
fn a_device_status_query_gets_its_reply() {
    let mut h = Harness::start();
    let (_, _, child) = h.project();
    child.output(b"\x1b[6n");
    let reply = next_bytes(&child);
    assert!(
        reply.starts_with(b"\x1b[") && reply.ends_with(b"R"),
        "{reply:?}"
    );
}

#[test]
fn a_reply_is_never_dropped_behind_a_full_input_queue() {
    let mut h = Harness::start();
    let (_, pane, child) = h.project();
    let sent: Vec<Vec<u8>> = (0..400).map(|i| format!("<{i}>").into_bytes()).collect();
    for bytes in &sent {
        h.input(pane, bytes);
    }
    std::thread::sleep(Duration::from_millis(50));
    child.output(b"\x1b[6n");
    std::thread::sleep(Duration::from_millis(50));

    let mut inputs = Vec::new();
    let mut replies = Vec::new();
    while inputs.len() < sent.len() || replies.is_empty() {
        let bytes = next_bytes(&child);
        if bytes.starts_with(b"\x1b[") {
            replies.push((inputs.len(), bytes));
        } else {
            inputs.push(bytes);
        }
    }
    assert_eq!(inputs, sent);
    assert_eq!(replies.len(), 1);
    let (after, reply) = &replies[0];
    assert!(reply.ends_with(b"R"));
    assert_eq!(
        *after,
        yien_pty::INPUT_QUEUE_CAPACITY,
        "the reply must go before the input still waiting"
    );
}

#[test]
fn a_reply_goes_before_a_resize_waiting_on_a_full_queue() {
    let mut h = Harness::start();
    let (_, pane, child) = h.project();
    let sent: Vec<Vec<u8>> = (0..300).map(|i| format!("<{i}>").into_bytes()).collect();
    for bytes in &sent {
        h.input(pane, bytes);
    }
    std::thread::sleep(Duration::from_millis(50));
    h.send(ClientMsg::ResizePane {
        pane,
        cols: 100,
        rows: 30,
    });
    std::thread::sleep(Duration::from_millis(50));
    child.output(b"\x1b[6n");
    std::thread::sleep(Duration::from_millis(100));

    let mut order = Vec::new();
    while order.len() < sent.len() + 2 {
        order.push(match next_input(&child) {
            FakeInput::Bytes(b) if b.starts_with(b"\x1b[") => "reply".to_owned(),
            FakeInput::Bytes(b) => String::from_utf8(b).unwrap(),
            FakeInput::Resize { cols, rows } => format!("resize {cols}x{rows}"),
        });
    }
    let at = yien_pty::INPUT_QUEUE_CAPACITY;
    assert_eq!(order[at - 1], format!("<{}>", at - 1));
    assert_eq!(order[at], "reply");
    assert_eq!(order[at + 1], "resize 100x30");
    assert_eq!(order[at + 2], format!("<{at}>"));
}

#[test]
fn resize_reaches_the_pty() {
    let mut h = Harness::start();
    let (_, pane, child) = h.project();
    h.send(ClientMsg::ResizePane {
        pane,
        cols: 120,
        rows: 40,
    });
    let deadline = Instant::now() + TIMEOUT;
    let mut got = None;
    while Instant::now() < deadline && got.is_none() {
        if let Some(FakeInput::Resize { cols, rows }) = child.recv_input(Duration::from_millis(50))
        {
            got = Some((cols, rows));
        }
    }
    assert_eq!(got, Some((120, 40)));
}

#[test]
fn rf_pan_19_exit_is_reported_and_the_pane_relaunches() {
    let mut h = Harness::start();
    let (_, pane, child) = h.project();
    child.exit(3);
    let code = h.wait_for(|m| match m {
        CoreMsg::PaneExited { pane: p, code } if *p == pane => Some(*code),
        _ => None,
    });
    assert_eq!(code, Some(3));
    let info = h.wait_for(|m| match m {
        CoreMsg::PaneUpdated(p) if p.id == pane => Some(p.clone()),
        _ => None,
    });
    assert_eq!((info.running, info.exit_code), (false, Some(3)));

    h.command(Command::RelaunchPane { pane });
    let again = h.child();
    assert_eq!(again.spec().program, "fake-shell");
    let running = h.wait_for(|m| match m {
        CoreMsg::PaneUpdated(p) if p.id == pane && p.running => Some(p.exit_code),
        _ => None,
    });
    assert_eq!(running, None);
}

#[test]
fn rf_pan_19_a_relaunched_program_starts_on_a_fresh_terminal() {
    let mut h = Harness::start();
    let (_, pane, child) = h.project();
    child.output(b"\x1b[?2004h\x1b[?1049hbefore");
    let frame = h.frame_with(pane, "before");
    assert!(frame.modes.bracketed_paste && frame.modes.alt_screen);
    child.exit(0);
    h.wait_for(|m| match m {
        CoreMsg::PaneExited { pane: p, .. } if *p == pane => Some(()),
        _ => None,
    });

    h.command(Command::RelaunchPane { pane });
    let again = h.child();
    again.output(b"after");
    let frame = h.frame_with(pane, "after");
    assert!(!frame.text().contains("before"));
    assert!(!frame.modes.bracketed_paste && !frame.modes.alt_screen);
}

#[test]
fn rf_pan_20_terminate_keeps_the_pane() {
    let mut h = Harness::start();
    let (_, pane, child) = h.project();
    h.command(Command::TerminatePane { pane });
    h.wait_for(|m| match m {
        CoreMsg::PaneExited { pane: p, .. } if *p == pane => Some(()),
        _ => None,
    });
    assert!(child.was_killed());
    h.command(Command::RenamePane {
        pane,
        name: "kept".into(),
    });
    let (name, running) = h.wait_for(|m| match m {
        CoreMsg::PaneUpdated(p) if p.id == pane && p.name == "kept" => {
            Some((p.name.clone(), p.running))
        }
        CoreMsg::Error { .. } => panic!("the pane should still exist"),
        _ => None,
    });
    assert_eq!((name.as_str(), running), ("kept", false));
}

#[test]
fn a_failed_spawn_is_reported_on_the_pane() {
    let mut h = Harness::start();
    let (_, first, _) = h.project();
    h.backend.refuse_spawns(true);
    h.command(Command::SplitPane {
        pane: first,
        orientation: Orientation::Vertical,
        profile: None,
    });
    let (pane, message) = h.wait_for(|m| match m {
        CoreMsg::Error {
            pane: Some(p),
            message,
        } => Some((*p, message.clone())),
        _ => None,
    });
    assert_ne!(pane, first);
    assert!(message.contains("fake-shell"), "{message}");
}

#[test]
fn rf_pan_12_program_title_reaches_the_pane() {
    let mut h = Harness::start();
    let (_, pane, child) = h.project();
    child.output(b"\x1b]0;build watcher\x07");
    let title = h.wait_for(|m| match m {
        CoreMsg::PaneUpdated(p) if p.id == pane && p.title.is_some() => p.title.clone(),
        _ => None,
    });
    assert_eq!(title, "build watcher");
}

#[test]
fn rf_age_05_a_running_shell_command_is_working() {
    let mut h = Harness::start();
    let (_, pane, child) = h.project();
    child.output(b"\x1b]133;C\x07");
    assert_eq!(h.state(pane), AgentState::Working);
}

#[test]
fn rf_age_07_a_turn_done_unseen_goes_idle_once_the_window_is_focused() {
    let mut h = Harness::start();
    let (_, pane, child) = h.project();
    child.output(b"\x1b]133;C\x07");
    assert_eq!(h.state(pane), AgentState::Working);
    child.output(b"\x1b]133;D;0\x07");
    assert_eq!(h.state(pane), AgentState::Done);
    h.send(ClientMsg::WindowFocus { focused: true });
    assert_eq!(h.state(pane), AgentState::Idle);
}

#[test]
fn rf_age_05_the_watchdog_ticks_and_confirms_a_prompt_on_screen() {
    let profile = Profile::from_toml(
        r#"
schema = 1
id = "watched"
name = "Watched"
detect = { command = "fake-agent" }

[[rules]]
sources = ["osc133", "screen"]
watchdog_secs = 1

[[rules.screen]]
confirms = "needs_you"
bottom_rows = 12
pattern = '(?m)^Do you want to proceed\?'
"#,
    )
    .unwrap();
    let mut h = Harness::start_with(CoreConfig {
        profiles: vec![profile],
        ..config()
    });
    let (_, first, _) = h.project();
    let (pane, child) = h.split(first, Some("watched"));
    assert_eq!(child.spec().program, "fake-agent");
    let start = Instant::now();
    child.output(b"\x1b]133;C\x07\x1b[20;1HDo you want to proceed?");
    assert_eq!(h.state(pane), AgentState::Working);
    assert_eq!(h.state(pane), AgentState::NeedsYou(NeedsReason::ScreenRule));
    assert!(
        start.elapsed() >= Duration::from_secs(1),
        "{:?}",
        start.elapsed()
    );
}

#[test]
fn copy_selection_returns_the_selected_text() {
    use yien_proto::frame::{SelectionKind, Side, ViewportPoint};
    let mut h = Harness::start();
    let (_, pane, child) = h.project();
    child.output(b"copy me");
    h.frame_with(pane, "copy me");
    h.send(ClientMsg::StartSelection {
        pane,
        at: ViewportPoint::new(0, 0),
        side: Side::Left,
        kind: SelectionKind::Simple,
    });
    h.send(ClientMsg::UpdateSelection {
        pane,
        at: ViewportPoint::new(0, 3),
        side: Side::Right,
    });
    h.send(ClientMsg::CopySelection { pane });
    let text = h.wait_for(|m| match m {
        CoreMsg::SelectionText { pane: p, text } if *p == pane => Some(text.clone()),
        _ => None,
    });
    assert_eq!(text.as_deref(), Some("copy"));
}

#[test]
fn dropping_the_last_handle_terminates_every_program() {
    let mut h = Harness::start();
    let (_, first, child_a) = h.project();
    let (_, child_b) = h.split(first, None);
    let clone = h.core.clone();
    drop(clone);
    assert!(!child_a.was_killed());
    drop(h.core);
    wait_until(|| child_a.was_killed() && child_b.was_killed());
}

#[test]
fn panes_in_a_hidden_tab_send_frames_once_shown() {
    let mut h = Harness::start();
    h.command(Command::NewProject {
        root: env!("CARGO_MANIFEST_DIR").into(),
        name: None,
    });
    let (project, first_tab) = h.wait_for(|m| match m {
        CoreMsg::ProjectUpdated(p) => Some((p.id, p.tabs[0])),
        _ => None,
    });
    let hidden = h.wait_for(|m| match m {
        CoreMsg::TabUpdated(t) => Some(t.focused),
        _ => None,
    });
    let hidden_child = h.child();
    h.command(Command::NewTab {
        project,
        name: None,
        profile: None,
    });
    let shown = h.wait_for(|m| match m {
        CoreMsg::TabUpdated(t) if t.id != first_tab => Some(t.focused),
        _ => None,
    });
    let shown_child = h.child();
    std::thread::sleep(Duration::from_millis(50));
    let _ = h.core.take_frame(hidden);

    hidden_child.output(b"hidden text");
    std::thread::sleep(Duration::from_millis(50));
    shown_child.output(b"shown text");
    h.frame_with(shown, "shown text");
    assert!(
        h.core
            .take_frame(hidden)
            .is_none_or(|f| !f.text().contains("hidden")),
        "a pane in a hidden tab published a frame"
    );

    h.command(Command::SelectTab { tab: first_tab });
    h.frame_with(hidden, "hidden text");
}

fn set_palette(h: &Harness, palette: TerminalPalette) {
    h.send(ClientMsg::SetTerminalPalette { palette });
}

#[test]
fn rf_cfg_02_a_palette_change_recolours_the_pane_and_keeps_its_scrollback() {
    let (sumi, washi) = (Palette::default(), Palette::washi());
    let mut h = Harness::start();
    let (_, pane, child) = h.project();
    for i in 0..80 {
        child.output(format!("line {i}\r\n").as_bytes());
    }
    child.output(b"\x1b[31mred\x1b[0m");
    let before = h.frame_with(pane, "red");
    assert_eq!(before.background, sumi.background);
    assert!(before.history_len > 0);

    set_palette(&h, TerminalPalette::Washi);
    let after = h.frame_where(pane, |f| f.background == washi.background);
    assert_eq!(after.text(), before.text());
    assert_eq!(after.history_len, before.history_len, "scrollback kept");
    let row = after.lines.iter().position(|l| l[0].ch == 'r').unwrap();
    assert_eq!(after.lines[row][0].fg, washi.ansi[1]);
    assert!(
        !child.was_killed() && !child.was_closed(),
        "the program keeps running"
    );
    assert!(h.backend.take_spawned().is_empty(), "nothing respawned");

    set_palette(&h, TerminalPalette::Sumi);
    let back = h.frame_where(pane, |f| f.background == sumi.background);
    assert_eq!(back, before, "switching back gives the same frame");
}

#[test]
fn rf_cfg_02_new_and_relaunched_panes_use_the_current_palette() {
    let washi = Palette::washi();
    let mut h = Harness::start();
    let (_, pane, child) = h.project();
    set_palette(&h, TerminalPalette::Washi);
    let (split, split_child) = h.split(pane, None);
    split_child.output(b"split");
    let frame = h.frame_with(split, "split");
    assert_eq!(frame.background, washi.background);

    child.exit(0);
    h.wait_for(|m| match m {
        CoreMsg::PaneExited { pane: p, .. } if *p == pane => Some(()),
        _ => None,
    });
    h.command(Command::RelaunchPane { pane });
    h.child().output(b"again");
    let frame = h.frame_with(pane, "again");
    assert_eq!(frame.background, washi.background);
}

#[test]
fn rf_pan_19_input_queued_for_the_old_program_does_not_reach_the_relaunched_one() {
    let mut h = Harness::start();
    let (_, pane, child) = h.project();
    for i in 0..400 {
        h.input(pane, format!("old {i}\r").as_bytes());
    }
    std::thread::sleep(Duration::from_millis(50));
    h.command(Command::RelaunchPane { pane });
    let again = h.child();
    wait_until(|| child.was_killed());
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(again.recv_input(Duration::from_millis(100)), None);
    h.input(pane, b"fresh");
    assert_eq!(next_bytes(&again), b"fresh");
}

#[test]
fn rf_pan_20_input_queued_before_terminate_is_dropped() {
    let mut h = Harness::start();
    let (_, pane, child) = h.project();
    for i in 0..400 {
        h.input(pane, format!("old {i}\r").as_bytes());
    }
    std::thread::sleep(Duration::from_millis(50));
    h.command(Command::TerminatePane { pane });
    h.wait_for(|m| match m {
        CoreMsg::PaneExited { pane: p, .. } if *p == pane => Some(()),
        _ => None,
    });
    let mut received = 0;
    while child.recv_input(Duration::from_millis(50)).is_some() {
        received += 1;
    }
    assert_eq!(received, yien_pty::INPUT_QUEUE_CAPACITY);
}

#[test]
fn a_pane_shown_for_the_first_time_draws_at_once() {
    let mut h = Harness::start();
    let (_, mut pane, mut child) = h.project();
    for _ in 0..5 {
        let start = Instant::now();
        child.output(b"first frame");
        h.frame_with(pane, "first frame");
        assert!(
            start.elapsed() < Duration::from_millis(300),
            "first frame took {:?}",
            start.elapsed()
        );
        (pane, child) = h.split(pane, None);
    }
}

/// A core whose new panes wait for the UI's size; long waits, so only a
/// size or the end of a wait the test chooses starts a program.
fn waiting(size_wait: Duration, ui_wait: Duration) -> Harness {
    Harness::start_with(CoreConfig {
        size_wait,
        ui_wait,
        ..config()
    })
}

impl Harness {
    /// A new project whose pane has not started its program yet.
    fn pending_project(&mut self) -> (ProjectId, TabId, PaneId) {
        self.command(Command::NewProject {
            root: env!("CARGO_MANIFEST_DIR").into(),
            name: None,
        });
        self.wait_for(|m| match m {
            CoreMsg::TabUpdated(t) => Some((t.project, t.id, t.focused)),
            _ => None,
        })
    }

    fn resize(&self, pane: PaneId, cols: u16, rows: u16) {
        self.send(ClientMsg::ResizePane { pane, cols, rows });
    }

    fn assert_nothing_spawned_for(&self, wait: Duration) {
        std::thread::sleep(wait);
        assert!(self.backend.take_spawned().is_empty(), "spawned too early");
    }
}

/// No resize reaches a program that started at its drawn size.
fn assert_no_resize(child: &FakeChild) {
    while let Some(input) = child.recv_input(Duration::from_millis(100)) {
        assert!(
            !matches!(input, FakeInput::Resize { .. }),
            "resized after the spawn: {input:?}"
        );
    }
}

#[test]
fn a_new_pane_starts_at_the_first_size_the_ui_sends() {
    let mut h = waiting(TIMEOUT * 2, TIMEOUT * 2);
    let (_, _, pane) = h.pending_project();
    h.assert_nothing_spawned_for(Duration::from_millis(100));
    h.resize(pane, 95, 31);
    let child = h.child();
    assert_eq!((child.spec().cols, child.spec().rows), (95, 31));
    assert_no_resize(&child);
}

#[test]
fn a_split_pane_starts_at_the_first_size_the_ui_sends() {
    let mut h = waiting(TIMEOUT * 2, TIMEOUT * 2);
    let (_, _, first) = h.pending_project();
    h.resize(first, 120, 40);
    let first_child = h.child();
    h.command(Command::SplitPane {
        pane: first,
        orientation: Orientation::Horizontal,
        profile: None,
    });
    let new = h.wait_for(|m| match m {
        CoreMsg::PaneUpdated(p) if p.id != first => Some(p.id),
        _ => None,
    });
    h.assert_nothing_spawned_for(Duration::from_millis(100));
    h.resize(first, 60, 40);
    h.resize(new, 59, 40);
    let child = h.child();
    assert_eq!((child.spec().cols, child.spec().rows), (59, 40));
    assert_no_resize(&child);
    assert_eq!(
        next_input(&first_child),
        FakeInput::Resize { cols: 60, rows: 40 }
    );
}

#[test]
fn input_sent_before_the_program_starts_reaches_it() {
    let mut h = waiting(TIMEOUT * 2, TIMEOUT * 2);
    let (_, _, pane) = h.pending_project();
    h.input(pane, b"early");
    h.resize(pane, 90, 30);
    let child = h.child();
    assert_eq!(next_bytes(&child), b"early");
}

#[test]
fn without_any_size_a_new_pane_starts_at_the_initial_size_after_the_wait() {
    let mut h = waiting(Duration::from_millis(100), Duration::from_millis(600));
    h.pending_project();
    h.assert_nothing_spawned_for(Duration::from_millis(250));
    let child = h.child();
    assert_eq!((child.spec().cols, child.spec().rows), (80, 24));
}

#[test]
fn a_pane_whose_size_never_comes_starts_at_the_last_size_the_ui_reported() {
    let mut h = waiting(Duration::from_millis(100), TIMEOUT * 2);
    let (project, _, first) = h.pending_project();
    h.resize(first, 111, 33);
    h.child();
    h.command(Command::NewTab {
        project,
        name: None,
        profile: None,
    });
    let child = h.child();
    assert_eq!((child.spec().cols, child.spec().rows), (111, 33));
}

#[test]
fn while_the_ui_has_reported_nothing_a_new_pane_keeps_waiting() {
    let mut h = waiting(Duration::from_millis(50), TIMEOUT * 2);
    let (_, _, pane) = h.pending_project();
    h.assert_nothing_spawned_for(Duration::from_millis(300));
    h.resize(pane, 101, 29);
    let child = h.child();
    assert_eq!((child.spec().cols, child.spec().rows), (101, 29));
}

/// Long enough that the test thread's command lands first on a loaded
/// machine, short enough to watch the deadline pass.
const START_WAIT: Duration = Duration::from_millis(1500);

#[test]
fn a_pane_terminated_before_it_starts_never_starts() {
    let mut h = waiting(START_WAIT, START_WAIT);
    let (_, _, pane) = h.pending_project();
    h.command(Command::TerminatePane { pane });
    h.wait_for(|m| match m {
        CoreMsg::PaneExited {
            pane: p,
            code: None,
        } if *p == pane => Some(()),
        _ => None,
    });
    h.assert_nothing_spawned_for(START_WAIT + Duration::from_millis(500));
    h.command(Command::RelaunchPane { pane });
    h.child();
}

#[test]
fn a_tab_closed_before_its_pane_starts_spawns_nothing() {
    let mut h = waiting(START_WAIT, START_WAIT);
    let (_, tab, _) = h.pending_project();
    h.command(Command::CloseTab { tab });
    h.assert_nothing_spawned_for(START_WAIT + Duration::from_millis(500));
}

#[test]
fn rf_pan_19_a_relaunch_starts_at_once_at_the_last_size() {
    let mut h = waiting(TIMEOUT * 2, TIMEOUT * 2);
    let (_, _, pane) = h.pending_project();
    h.resize(pane, 95, 31);
    let child = h.child();
    h.resize(pane, 70, 20);
    assert_eq!(next_input(&child), FakeInput::Resize { cols: 70, rows: 20 });
    h.command(Command::RelaunchPane { pane });
    let again = h.child();
    assert_eq!((again.spec().cols, again.spec().rows), (70, 20));
}

#[test]
fn a_pane_relaunched_before_it_starts_takes_the_last_size_the_ui_reported() {
    let mut h = waiting(TIMEOUT * 2, TIMEOUT * 2);
    let (project, _, first) = h.pending_project();
    h.resize(first, 111, 33);
    h.child();
    h.command(Command::NewTab {
        project,
        name: None,
        profile: None,
    });
    let pane = h.wait_for(|m| match m {
        CoreMsg::TabUpdated(t) if t.focused != first => Some(t.focused),
        _ => None,
    });
    h.command(Command::RelaunchPane { pane });
    let child = h.child();
    assert_eq!((child.spec().cols, child.spec().rows), (111, 33));
}

#[test]
fn the_shutdown_closes_every_pane() {
    let mut h = Harness::start();
    let (_, first, first_child) = h.project();
    let (_, second_child) = h.split(first, None);
    let started = Instant::now();
    // Far longer than the check below, so returning in time means it never
    // waited for the deadline.
    assert_eq!(h.core.shutdown(TIMEOUT * 4).unwrap(), 2);
    assert!(started.elapsed() < TIMEOUT, "{:?}", started.elapsed());
    for child in [&first_child, &second_child] {
        assert!(child.was_killed() && child.was_closed());
    }
}

#[test]
fn the_shutdown_returns_within_its_bound_when_a_pane_never_closes() {
    let mut h = Harness::start();
    let (_, first, first_child) = h.project();
    h.backend.stall_closes(true);
    let (_, stuck) = h.split(first, None);
    let bound = Duration::from_millis(300);
    let started = Instant::now();
    let result = h.core.shutdown(bound);
    let took = started.elapsed();
    assert!(
        matches!(result, Err(ShutdownError::TimedOut { open: 1, total: 2 })),
        "{result:?}"
    );
    // The stuck pane never closes, so only giving up at the bound returns.
    assert!(took >= bound && took < TIMEOUT, "{took:?}");
    for child in [&first_child, &stuck] {
        assert!(child.was_killed() && child.was_closed());
    }
}

#[test]
fn a_pane_waiting_to_start_never_starts_after_the_shutdown() {
    let wait = Duration::from_secs(2);
    let mut h = waiting(wait, wait);
    h.pending_project();
    assert_eq!(h.core.shutdown(TIMEOUT).unwrap(), 0);
    h.assert_nothing_spawned_for(wait + Duration::from_millis(500));
}

#[test]
fn closing_a_project_terminates_its_panes() {
    let mut h = Harness::start();
    let (project, _, first) = h.pending_project();
    let first_child = h.child();
    let (_, second_child) = h.split(first, None);
    h.command(Command::CloseProject { project });
    let closed = h.wait_for(|m| match m {
        CoreMsg::ProjectClosed { project } => Some(*project),
        _ => None,
    });
    assert_eq!(closed, project);
    wait_until(|| first_child.was_killed() && second_child.was_killed());
}

#[test]
fn a_project_closed_before_its_pane_starts_spawns_nothing() {
    let mut h = waiting(START_WAIT, START_WAIT);
    let (project, _, _) = h.pending_project();
    h.command(Command::CloseProject { project });
    h.assert_nothing_spawned_for(START_WAIT + Duration::from_millis(500));
}

#[test]
fn rf_esp_01_re_adding_a_project_whose_folder_is_gone_selects_it() {
    let gone = std::env::temp_dir().join(format!("yien-gone-{}", std::process::id()));
    std::fs::create_dir_all(&gone).unwrap();
    let root = gone.to_string_lossy().into_owned();
    let mut h = Harness::start();
    h.command(Command::NewProject {
        root: root.clone(),
        name: None,
    });
    let tab = h.wait_for(|m| match m {
        CoreMsg::TabUpdated(t) => Some(t.id),
        _ => None,
    });
    h.command(Command::NewProject {
        root: env!("CARGO_MANIFEST_DIR").into(),
        name: None,
    });
    h.wait_for(|m| match m {
        CoreMsg::ActiveTab { tab: Some(t) } if *t != tab => Some(()),
        _ => None,
    });
    std::fs::remove_dir_all(&gone).unwrap();
    h.command(Command::NewProject { root, name: None });
    let answer = h.wait_for(|m| match m {
        CoreMsg::ActiveTab { tab: Some(t) } if *t == tab => Some(Ok(())),
        CoreMsg::Error { message, .. } => Some(Err(message.clone())),
        _ => None,
    });
    assert_eq!(answer, Ok(()));
}
