//! Pane screens kept across restarts (RF-SES-10), through the core over
//! `FakePty`, an in-memory session store and in-memory screen files shared
//! between runs, on a clock the tests move.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers outside #[test] fns are not covered by clippy.toml's in-test allowances"
)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::error::TryRecvError;
use yien_core::{
    Clock, Core, CoreConfig, CoreHandle, ScreenFiles, ScreenStore, SessionStore, ShellCommand,
};
use yien_proto::frame::Frame;
use yien_proto::{ClientMsg, Command, CoreMsg, Orientation, PaneId, ProfileId};
use yien_pty::{FakeChild, FakePty};
use yien_session::{MemStore, Store, StoreError};

const TIMEOUT: Duration = Duration::from_secs(5);
const ROOT: &str = env!("CARGO_MANIFEST_DIR");
const MARK: &str = "\u{2500}\u{2500} restored ";

/// The session file both runs share.
#[derive(Clone, Default)]
struct Session(Arc<Mutex<MemStore>>);

impl Store for Session {
    fn read(&self, name: &str) -> Result<Option<Vec<u8>>, StoreError> {
        self.0.lock().unwrap().read(name)
    }

    fn write_atomic(&mut self, name: &str, bytes: &[u8]) -> Result<(), StoreError> {
        self.0.lock().unwrap().write_atomic(name, bytes)
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), StoreError> {
        self.0.lock().unwrap().rename(from, to)
    }
}

/// Screen files both runs share, counting writes; a write can be made to
/// stall like a stuck disk.
#[derive(Clone, Default)]
struct Files {
    files: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    writes: Arc<AtomicUsize>,
    stall: Option<Duration>,
    gate: Option<Arc<Gate>>,
}

/// Holds every write until the test opens it, as a stuck disk would.
#[derive(Default)]
struct Gate {
    state: Mutex<(bool, bool)>,
    changed: Condvar,
}

impl Gate {
    fn reached(&self) -> bool {
        self.state.lock().unwrap().0
    }

    fn open(&self) {
        self.state.lock().unwrap().1 = true;
        self.changed.notify_all();
    }

    fn pass(&self) {
        let mut state = self.state.lock().unwrap();
        state.0 = true;
        // Bounded, so a test that never opens it fails instead of hanging.
        let _ = self
            .changed
            .wait_timeout_while(state, TIMEOUT * 4, |s| !s.1)
            .unwrap();
    }
}

impl Files {
    fn get(&self, name: &str) -> Option<Vec<u8>> {
        self.files.lock().unwrap().get(name).cloned()
    }

    fn put(&self, name: &str, bytes: &[u8]) {
        self.files
            .lock()
            .unwrap()
            .insert(name.to_owned(), bytes.to_vec());
    }

    fn writes(&self) -> usize {
        self.writes.load(Ordering::SeqCst)
    }
}

impl ScreenFiles for Files {
    fn read(&self, name: &str, limit: u64) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self.get(name).filter(|b| b.len() as u64 <= limit))
    }

    fn write(&mut self, name: &str, bytes: &[u8]) -> Result<(), StoreError> {
        if let Some(stall) = self.stall {
            std::thread::sleep(stall);
        }
        if let Some(gate) = &self.gate {
            gate.pass();
        }
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.put(name, bytes);
        Ok(())
    }

    fn remove(&mut self, name: &str) -> Result<(), StoreError> {
        self.files.lock().unwrap().remove(name);
        Ok(())
    }

    fn list(&self) -> Result<Vec<String>, StoreError> {
        Ok(self.files.lock().unwrap().keys().cloned().collect())
    }
}

/// A clock that moves only when the test says so.
#[derive(Debug, Clone, Default)]
struct TestClock(Arc<Mutex<Duration>>);

impl TestClock {
    fn advance(&self, by: Duration) {
        *self.0.lock().unwrap() += by;
    }
}

impl Clock for TestClock {
    fn now(&self) -> Duration {
        *self.0.lock().unwrap()
    }
}

struct Run {
    core: CoreHandle,
    events: Receiver<CoreMsg>,
    backend: FakePty,
}

impl Run {
    fn start(session: &Session, files: &Files, clock: &TestClock) -> Self {
        let backend = FakePty::new();
        let config = CoreConfig {
            shell: ShellCommand::new("fake-shell"),
            session: Some(SessionStore::new(session.clone())),
            screens: Some(ScreenStore::new(files.clone())),
            start_project: Some(ROOT.to_owned()),
            clock: Arc::new(clock.clone()),
            ..CoreConfig::default()
        };
        let (core, events) = Core::start(Arc::new(backend.clone()), config).unwrap();
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

    fn first_pane(&mut self) -> PaneId {
        self.wait_for(|m| match m {
            CoreMsg::TabUpdated(t) => Some(t.focused),
            _ => None,
        })
    }

    fn children(&self, count: usize) -> Vec<FakeChild> {
        let deadline = Instant::now() + TIMEOUT;
        let mut out = Vec::new();
        while out.len() < count {
            out.extend(self.backend.take_spawned());
            assert!(Instant::now() < deadline, "timed out waiting for spawns");
            std::thread::sleep(Duration::from_millis(1));
        }
        out
    }

    fn split(&mut self, pane: PaneId, profile: Option<&str>) -> PaneId {
        self.command(Command::SplitPane {
            pane,
            orientation: Orientation::Horizontal,
            profile: profile.map(ProfileId::new),
            before: false,
        });
        self.wait_for(|m| match m {
            CoreMsg::PaneUpdated(p) if p.id != pane => Some(p.id),
            _ => None,
        })
    }

    /// Takes frames of `pane` as they come until one shows `text`.
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
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn name(pane: PaneId) -> String {
    format!("{}.vt", pane.get())
}

fn rows(frame: &Frame) -> Vec<String> {
    (0..frame.lines.len()).map(|r| frame.line_text(r)).collect()
}

/// Longer than the writer takes to look at the clock several times.
const QUIET: Duration = Duration::from_millis(1200);

#[test]
fn rf_ses_10_a_restored_pane_shows_its_screen_above_the_program() {
    let (session, files, clock) = (Session::default(), Files::default(), TestClock::default());
    let mut run = Run::start(&session, &files, &clock);
    let pane = run.first_pane();
    let child = run.children(1).pop().unwrap();
    child.output(b"\x1b[31mold text\x1b[0m\r\n$ ");
    run.frame_with(pane, "old text");
    run.core.save_session(TIMEOUT).unwrap();
    let saved = files.get(&name(pane)).expect("the quit wrote the screen");
    assert!(String::from_utf8_lossy(&saved).contains("old text"));
    drop(run);

    let mut again = Run::start(&session, &files, &clock);
    let child = again.children(1).pop().unwrap();
    let frame = again.frame_with(pane, "restored");
    let text = rows(&frame);
    assert_eq!(text[0], "old text");
    assert_eq!(text[1], "$");
    assert!(text[2].starts_with(MARK), "{text:?}");
    assert_eq!(frame.lines[0][0].fg, yien_vt::Palette::default().ansi[1]);
    let cursor = frame.cursor.unwrap().point;
    assert_eq!((cursor.row, cursor.col), (3, 0));
    // Display only: the program gets none of it.
    assert!(child.recv_input(Duration::from_millis(300)).is_none());
    child.output(b"new$ ");
    let frame = again.frame_with(pane, "new$");
    assert_eq!(rows(&frame)[3], "new$");
    assert_eq!(rows(&frame)[0], "old text");
}

#[test]
fn rf_ses_10_a_new_pane_shows_no_saved_screen() {
    let (session, files, clock) = (Session::default(), Files::default(), TestClock::default());
    // A file for an id the new pane gets, from a run whose session is gone.
    files.put("1.vt", b"stale\x1b7\x1b[1G");
    files.put("2.vt", b"stale\x1b7\x1b[1G");
    let mut run = Run::start(&session, &files, &clock);
    let pane = run.first_pane();
    let child = run.children(1).pop().unwrap();
    child.output(b"fresh");
    let frame = run.frame_with(pane, "fresh");
    assert!(!frame.text().contains("stale"));
}

#[test]
fn rf_ses_10_changed_screens_are_written_each_interval_and_unchanged_ones_are_not() {
    let (session, files, clock) = (Session::default(), Files::default(), TestClock::default());
    let mut run = Run::start(&session, &files, &clock);
    let pane = run.first_pane();
    let child = run.children(1).pop().unwrap();
    child.output(b"one");
    run.frame_with(pane, "one");
    std::thread::sleep(QUIET);
    assert_eq!(files.writes(), 0, "nothing before the interval");

    clock.advance(Duration::from_secs(29));
    std::thread::sleep(QUIET);
    assert_eq!(files.writes(), 0, "nothing before the interval");
    clock.advance(Duration::from_secs(1));
    wait_until(|| files.writes() == 1);
    assert!(String::from_utf8_lossy(&files.get(&name(pane)).unwrap()).contains("one"));

    clock.advance(Duration::from_secs(30));
    std::thread::sleep(QUIET);
    assert_eq!(
        files.writes(),
        1,
        "an unchanged screen is not written again"
    );

    child.output(b" two");
    run.frame_with(pane, "two");
    clock.advance(Duration::from_secs(30));
    wait_until(|| files.writes() == 2);
    assert!(String::from_utf8_lossy(&files.get(&name(pane)).unwrap()).contains("one two"));

    // The quit writes nothing that is already on disk.
    run.core.save_session(TIMEOUT).unwrap();
    assert_eq!(files.writes(), 2);
}

#[test]
fn rf_ses_10_screens_of_panes_that_no_longer_exist_are_removed() {
    let (session, files, clock) = (Session::default(), Files::default(), TestClock::default());
    files.put("77.vt", b"old\x1b7\x1b[1G");
    files.put("77.vt.tmp", b"half");
    files.put("notes.txt", b"not ours");
    let mut run = Run::start(&session, &files, &clock);
    let shell = run.first_pane();
    let first = run.children(1).pop().unwrap();
    first.output(b"kept");
    let split = run.split(shell, None);
    let second = run.children(1).pop().unwrap();
    second.output(b"closing");
    run.frame_with(split, "closing");
    clock.advance(Duration::from_secs(30));
    wait_until(|| files.get(&name(split)).is_some() && files.get("77.vt").is_none());
    assert!(files.get("77.vt.tmp").is_none());
    assert!(files.get("notes.txt").is_some());

    run.command(Command::ClosePane { pane: split });
    wait_until(|| files.get(&name(split)).is_none());
    assert!(files.get(&name(shell)).is_some());
}

#[test]
fn rf_ses_10_rf_ses_01_slow_screens_do_not_stretch_the_quit_save() {
    let session = Session::default();
    let files = Files {
        stall: Some(Duration::from_secs(3)),
        ..Files::default()
    };
    let clock = TestClock::default();
    let mut run = Run::start(&session, &files, &clock);
    let pane = run.first_pane();
    let child = run.children(1).pop().unwrap();
    child.output(b"text");
    run.frame_with(pane, "text");
    let bound = Duration::from_millis(600);
    let started = Instant::now();
    run.core.save_session(bound).unwrap();
    let took = started.elapsed();
    // Thread start-up and scheduling under a loaded build, not the stall.
    assert!(took < bound + Duration::from_millis(400), "{took:?}");
    assert!(session.read(yien_session::CURRENT_FILE).unwrap().is_some());
}

#[test]
fn rf_ses_10_rf_ses_11_restored_text_is_not_read_as_the_resume_failing() {
    let (session, files, clock) = (Session::default(), Files::default(), TestClock::default());
    let mut run = Run::start(&session, &files, &clock);
    let shell = run.first_pane();
    run.children(1);
    let claude = run.split(shell, Some("claude"));
    let child = run.children(1).pop().unwrap();
    child.output(b"No conversation found with session ID: 1\r\n> ");
    run.frame_with(claude, "No conversation found");
    // The session to resume reaches the saved workspace first.
    wait_until(|| {
        yien_session::load(&session).restored.is_some_and(|r| {
            r.snapshot.projects[0].tabs[0]
                .panes
                .iter()
                .any(|p| p.agent.as_ref().is_some_and(|a| a.session_id.is_some()))
        })
    });
    run.core.save_session(TIMEOUT).unwrap();
    assert!(files.get(&name(claude)).is_some());
    drop(run);

    let mut again = Run::start(&session, &files, &clock);
    let resumed = again
        .children(2)
        .into_iter()
        .find(|c| c.spec().program == "claude")
        .unwrap();
    again.frame_with(claude, "restored");
    // Past the early-exit grace, inside the message one: only the text on
    // screen could make this exit a failed resume.
    clock.advance(Duration::from_secs(20));
    resumed.exit(1);
    again.wait_for(|m| match m {
        CoreMsg::PaneExited { pane, code } if *pane == claude => {
            assert_eq!(*code, Some(1));
            Some(())
        }
        CoreMsg::Error {
            pane: Some(p),
            message,
        } if *p == claude => panic!("fell back: {message}"),
        _ => None,
    });
    assert!(again.backend.take_spawned().is_empty());
}

#[test]
fn rf_ses_10_a_planted_file_cannot_put_text_below_the_restored_line() {
    let (session, files, clock) = (Session::default(), Files::default(), TestClock::default());
    let mut run = Run::start(&session, &files, &clock);
    let pane = run.first_pane();
    run.children(1).pop().unwrap().output(b"real");
    run.frame_with(pane, "real");
    run.core.save_session(TIMEOUT).unwrap();
    drop(run);
    files.put(
        &name(pane),
        b"x\x1b7\r\n\r\n\r\nDo you want to proceed? (y/n)",
    );

    let mut again = Run::start(&session, &files, &clock);
    let child = again.children(1).pop().unwrap();
    child.output(b"fresh");
    let frame = again.frame_with(pane, "fresh");
    assert!(!frame.text().contains("proceed"), "{:?}", rows(&frame));
    assert!(!frame.text().contains("restored"), "{:?}", rows(&frame));
}

#[test]
fn rf_ses_10_a_pass_under_way_never_writes_over_the_quit_save() {
    let session = Session::default();
    let gate = Arc::new(Gate::default());
    let files = Files {
        gate: Some(Arc::clone(&gate)),
        ..Files::default()
    };
    let clock = TestClock::default();
    let mut run = Run::start(&session, &files, &clock);
    let first = run.first_pane();
    let one = run.children(1).pop().unwrap();
    one.output(b"old");
    let second = run.split(first, None);
    let two = run.children(1).pop().unwrap();
    two.output(b"old");
    run.frame_with(second, "old");
    run.frame_with(first, "old");
    // The 30 s pass encodes the old screens and stalls on its first write.
    clock.advance(Duration::from_secs(30));
    wait_until(|| gate.reached());
    one.output(b" new");
    two.output(b" new");
    run.frame_with(first, "new");
    run.frame_with(second, "new");
    let quitting = run.core.clone();
    let quit = std::thread::spawn(move || quitting.save_session(TIMEOUT));
    std::thread::sleep(Duration::from_millis(300));
    gate.open();
    quit.join().unwrap().unwrap();
    std::thread::sleep(QUIET);
    for pane in [first, second] {
        let saved = String::from_utf8(files.get(&name(pane)).unwrap()).unwrap();
        assert!(saved.contains("old new"), "pane {}: {saved:?}", pane.get());
    }
}
