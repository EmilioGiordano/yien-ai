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
use yien_core::{Core, CoreConfig, CoreHandle, ShellCommand};
use yien_proto::{
    AgentState, ClientMsg, Command, CoreMsg, InputBytes, Orientation, PaneId, ProfileId, TabId,
};
use yien_pty::{FakeChild, FakeInput, FakePty};

const TIMEOUT: Duration = Duration::from_secs(5);

struct Harness {
    core: CoreHandle,
    events: Receiver<CoreMsg>,
    backend: FakePty,
}

impl Harness {
    fn start() -> Self {
        let backend = FakePty::new();
        let config = CoreConfig {
            shell: ShellCommand::new("fake-shell"),
            ..CoreConfig::default()
        };
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

    fn input(&self, pane: PaneId, bytes: &[u8]) {
        self.send(ClientMsg::Input {
            pane,
            bytes: InputBytes::new(bytes.to_vec()),
        });
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

    fn frame_with(&mut self, pane: PaneId, text: &str) -> String {
        self.wait_for(|m| match m {
            CoreMsg::PaneFrame { pane: p, frame } if *p == pane && frame.text().contains(text) => {
                Some(frame.text())
            }
            _ => None,
        })
    }
}

fn next_bytes(child: &FakeChild) -> Vec<u8> {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        if let Some(FakeInput::Bytes(bytes)) = child.recv_input(Duration::from_millis(50)) {
            return bytes;
        }
    }
    panic!("timed out waiting for input");
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
fn rf_pan_01_close_terminates_and_removes_the_pane() {
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
fn rf_pan_12_output_produces_a_frame_with_the_text() {
    let mut h = Harness::start();
    let (_, pane, child) = h.project();
    child.output(b"hello from the child");
    let text = h.frame_with(pane, "hello from the child");
    assert!(text.starts_with("hello"));
}

#[test]
fn frames_are_coalesced_while_output_streams() {
    let mut h = Harness::start();
    let (_, pane, child) = h.project();
    for i in 0..200 {
        child.output(format!("line {i}\r\n").as_bytes());
    }
    h.frame_with(pane, "line 199");
    let mut frames = 1;
    while let Ok(msg) = h.events.try_recv() {
        if matches!(msg, CoreMsg::PaneFrame { .. }) {
            frames += 1;
        }
    }
    assert!(frames < 200, "{frames} frames for 200 chunks");
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

    let mut inputs = Vec::new();
    let mut replies = Vec::new();
    while inputs.len() < sent.len() || replies.is_empty() {
        let bytes = next_bytes(&child);
        if bytes.starts_with(b"\x1b[") {
            replies.push(bytes);
        } else {
            inputs.push(bytes);
        }
    }
    assert_eq!(inputs, sent);
    assert_eq!(replies.len(), 1);
    assert!(replies[0].ends_with(b"R"));
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
    let state = h.wait_for(|m| match m {
        CoreMsg::PaneState { pane: p, state } if *p == pane => Some(*state),
        _ => None,
    });
    assert_eq!(state, AgentState::Working);
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
    });
    let shown = h.wait_for(|m| match m {
        CoreMsg::TabUpdated(t) if t.id != first_tab => Some(t.focused),
        _ => None,
    });
    let shown_child = h.child();

    hidden_child.output(b"hidden text");
    std::thread::sleep(Duration::from_millis(50));
    shown_child.output(b"shown text");
    h.wait_for(|m| match m {
        CoreMsg::PaneFrame { pane, .. } if *pane == hidden => {
            panic!("a pane in a hidden tab sent a frame")
        }
        CoreMsg::PaneFrame { pane, frame } if *pane == shown && frame.text().contains("shown") => {
            Some(())
        }
        _ => None,
    });

    h.command(Command::SelectTab { tab: first_tab });
    h.frame_with(hidden, "hidden text");
}
