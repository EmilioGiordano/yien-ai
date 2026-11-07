//! Faults no real program can cause: a terminal engine that panics in the
//! reader thread or in the pane actor.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc::Receiver;
use yien_proto::frame::{
    Frame, GridPoint, Link, Match, Modes, SelectionKind, Side, Size, ViewportPoint,
};
use yien_proto::{AgentState, ClientMsg, Command, CoreMsg, PaneId};
use yien_pty::{FakeChild, FakePty};
use yien_vt::{AlacrittyEngine, VtConfig, VtEngine, VtError, VtOutput};

use crate::{Core, CoreConfig, CoreHandle, ShellCommand};

const TIMEOUT: Duration = Duration::from_secs(5);

/// Output that makes the engine panic in the reader thread.
const PANIC_IN_READER: &[u8] = b"\x00panic in reader";

/// A scroll that makes the engine panic in the pane actor.
const PANIC_IN_ACTOR: i32 = i32::MIN;

struct Faulty(AlacrittyEngine);

fn faulty(config: &VtConfig, cols: u16, rows: u16) -> Box<dyn VtEngine> {
    Box::new(Faulty(AlacrittyEngine::new(config.clone(), cols, rows)))
}

impl VtEngine for Faulty {
    fn feed(&mut self, bytes: &[u8]) -> VtOutput {
        assert!(bytes != PANIC_IN_READER, "engine bug in the reader");
        self.0.feed(bytes)
    }
    fn sync_pending(&self) -> bool {
        self.0.sync_pending()
    }
    fn sync_generation(&self) -> u64 {
        self.0.sync_generation()
    }
    fn flush_sync(&mut self) -> VtOutput {
        self.0.flush_sync()
    }
    fn resize(&mut self, cols: u16, rows: u16) {
        self.0.resize(cols, rows);
    }
    fn size(&self) -> Size {
        self.0.size()
    }
    fn frame(&self) -> Frame {
        self.0.frame()
    }
    fn modes(&self) -> Modes {
        self.0.modes()
    }
    fn scroll(&mut self, delta: i32) {
        assert!(delta != PANIC_IN_ACTOR, "engine bug in the actor");
        self.0.scroll(delta);
    }
    fn scroll_to_bottom(&mut self) {
        self.0.scroll_to_bottom();
    }
    fn scroll_to(&mut self, at: GridPoint) {
        self.0.scroll_to(at);
    }
    fn start_selection(&mut self, at: ViewportPoint, side: Side, kind: SelectionKind) {
        self.0.start_selection(at, side, kind);
    }
    fn update_selection(&mut self, at: ViewportPoint, side: Side) {
        self.0.update_selection(at, side);
    }
    fn clear_selection(&mut self) {
        self.0.clear_selection();
    }
    fn selection_text(&self) -> Option<String> {
        self.0.selection_text()
    }
    fn search(&self, query: &str) -> Result<Vec<Match>, VtError> {
        self.0.search(query)
    }
    fn link_at(&mut self, at: ViewportPoint) -> Option<Link> {
        self.0.link_at(at)
    }
}

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
        let (core, events) = Core::start_with(Arc::new(backend.clone()), config, faulty).unwrap();
        Self {
            core,
            events,
            backend,
        }
    }

    fn send(&self, msg: ClientMsg) {
        self.core.try_send(msg).unwrap();
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
                Err(_) => {
                    assert!(Instant::now() < deadline, "timed out waiting for an event");
                    std::thread::sleep(Duration::from_millis(1));
                }
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

    fn project(&mut self) -> (PaneId, FakeChild) {
        self.send(ClientMsg::Command(Command::NewProject {
            root: env!("CARGO_MANIFEST_DIR").into(),
            name: None,
        }));
        let pane = self.wait_for(|m| match m {
            CoreMsg::TabUpdated(t) => Some(t.focused),
            _ => None,
        });
        (pane, self.child())
    }

    fn failure(&mut self, pane: PaneId) -> String {
        let message = self.wait_for(|m| match m {
            CoreMsg::Error {
                pane: Some(p),
                message,
            } if *p == pane => Some(message.clone()),
            _ => None,
        });
        self.wait_for(|m| match m {
            CoreMsg::PaneExited { pane: p, .. } if *p == pane => Some(()),
            _ => None,
        });
        let running = self.wait_for(|m| match m {
            CoreMsg::PaneUpdated(p) if p.id == pane => Some(p.running),
            _ => None,
        });
        assert!(!running);
        message
    }

    fn state(&mut self, pane: PaneId) -> AgentState {
        self.wait_for(|m| match m {
            CoreMsg::PaneState { pane: p, state } if *p == pane => Some(*state),
            _ => None,
        })
    }

    fn frame_with(&mut self, pane: PaneId, text: &str) -> Frame {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(frame) = self.core.take_frame(pane)
                && frame.text().contains(text)
            {
                return frame;
            }
            assert!(Instant::now() < deadline, "no frame showed {text:?}");
            self.wait_for(|m| match m {
                CoreMsg::PaneFrameReady { pane: p } if *p == pane => Some(()),
                _ => None,
            });
        }
    }
}

fn wait_until(mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn a_reader_panic_fails_the_pane_and_ends_its_program() {
    let mut h = Harness::start();
    let (pane, child) = h.project();
    child.output(PANIC_IN_READER);
    let message = h.failure(pane);
    assert!(message.contains("Relaunch"), "{message}");
    wait_until(|| child.was_killed());

    h.send(ClientMsg::Command(Command::RelaunchPane { pane }));
    let again = h.child();
    again.output(b"back");
    h.frame_with(pane, "back");
}

#[test]
fn rf_pan_19_a_crashed_pane_relaunches_at_its_size_and_draws_again() {
    let mut h = Harness::start();
    let (pane, child) = h.project();
    h.send(ClientMsg::ResizePane {
        pane,
        cols: 100,
        rows: 30,
    });
    child.output(b"before");
    h.frame_with(pane, "before");
    h.send(ClientMsg::Scroll {
        pane,
        delta: PANIC_IN_ACTOR,
    });
    h.failure(pane);
    wait_until(|| child.was_killed());

    h.send(ClientMsg::Command(Command::RelaunchPane { pane }));
    let again = h.child();
    assert_eq!((again.spec().cols, again.spec().rows), (100, 30));
    again.output(b"after the crash");
    let frame = h.frame_with(pane, "after the crash");
    assert_eq!(
        frame.size,
        Size {
            cols: 100,
            rows: 30
        }
    );
}

#[test]
fn rf_pan_19_a_crashed_pane_given_commands_before_its_relaunch_still_draws_and_is_seen() {
    let mut h = Harness::start();
    let (pane, child) = h.project();
    h.send(ClientMsg::Scroll {
        pane,
        delta: PANIC_IN_ACTOR,
    });
    h.failure(pane);
    wait_until(|| child.was_killed());
    h.send(ClientMsg::WindowFocus { focused: true });
    h.send(ClientMsg::Command(Command::FocusPane { pane }));
    h.send(ClientMsg::Command(Command::RelaunchPane { pane }));
    let again = h.child();
    again.output(b"drawn again");
    h.frame_with(pane, "drawn again");

    again.output(b"\x1b]133;C\x07");
    assert_eq!(h.state(pane), AgentState::Working);
    again.output(b"\x1b]133;D;0\x07");
    assert_eq!(
        h.state(pane),
        AgentState::Idle,
        "a seen pane ends its turn idle"
    );
}
