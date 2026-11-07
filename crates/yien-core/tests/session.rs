//! Saving and restoring the workspace through the core, over `FakePty` and
//! an in-memory store shared between two runs.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers outside #[test] fns are not covered by clippy.toml's in-test allowances"
)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::error::TryRecvError;
use yien_core::{Core, CoreConfig, CoreHandle, SessionSaveError, SessionStore, ShellCommand};
use yien_proto::{ClientMsg, Command, CoreMsg, Orientation, PaneId, ProfileId, WorkspaceSnapshot};
use yien_pty::{FakeChild, FakePty};
use yien_session::{MemStore, SessionSnapshot, Store, StoreError, load};

const TIMEOUT: Duration = Duration::from_secs(5);
const ROOT: &str = env!("CARGO_MANIFEST_DIR");

/// A store both runs of a test share, as the file on disk would be.
#[derive(Clone, Default)]
struct Shared(Arc<Mutex<MemStore>>);

impl Shared {
    fn saved(&self) -> Option<SessionSnapshot> {
        load(&*self.0.lock().unwrap()).restored.map(|r| r.snapshot)
    }

    fn file(&self, name: &str) -> Option<Vec<u8>> {
        self.0.lock().unwrap().read(name).unwrap()
    }

    fn insert(&self, name: &str, bytes: Vec<u8>) {
        self.0.lock().unwrap().insert(name, bytes);
    }
}

impl Store for Shared {
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

/// A store whose writes take two seconds, as a stalled disk would.
#[derive(Clone, Default)]
struct Slow(Shared);

impl Store for Slow {
    fn read(&self, name: &str) -> Result<Option<Vec<u8>>, StoreError> {
        self.0.read(name)
    }

    fn write_atomic(&mut self, name: &str, bytes: &[u8]) -> Result<(), StoreError> {
        std::thread::sleep(Duration::from_secs(2));
        self.0.write_atomic(name, bytes)
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), StoreError> {
        self.0.rename(from, to)
    }
}

struct Run {
    core: CoreHandle,
    events: Receiver<CoreMsg>,
    backend: FakePty,
}

impl Run {
    /// Starts a core on `store` that opens this crate's folder when nothing
    /// is restored.
    fn start(store: &Shared) -> Self {
        Self::start_on(store.clone())
    }

    fn start_on(store: impl Store + Send + 'static) -> Self {
        let backend = FakePty::new();
        let config = CoreConfig {
            shell: ShellCommand::new("fake-shell"),
            session: Some(SessionStore::new(store)),
            start_project: Some(ROOT.to_owned()),
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

    fn snapshot(&mut self) -> WorkspaceSnapshot {
        self.wait_for(|m| match m {
            CoreMsg::WorkspaceSnapshot(s) => Some(s.clone()),
            _ => None,
        })
    }

    fn notice(&mut self) -> String {
        self.wait_for(|m| match m {
            CoreMsg::Error {
                pane: None,
                message,
            } => Some(message.clone()),
            _ => None,
        })
    }

    /// The focused pane of the first tab announced.
    fn first_pane(&mut self) -> PaneId {
        self.wait_for(|m| match m {
            CoreMsg::TabUpdated(t) => Some(t.focused),
            _ => None,
        })
    }

    /// Splits `pane` and returns the new pane, which takes the focus.
    fn split(&mut self, pane: PaneId, profile: Option<&str>) -> PaneId {
        self.command(Command::SplitPane {
            pane,
            orientation: Orientation::Horizontal,
            profile: profile.map(ProfileId::new),
        });
        self.wait_for(|m| match m {
            CoreMsg::TabUpdated(t) if t.focused != pane && t.layout.panes().contains(&pane) => {
                Some(t.focused)
            }
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

    /// The quit save. Whether it wrote or the debounced save had already
    /// written everything, the store holds the workspace afterwards.
    fn save(&self) {
        self.core.save_session(TIMEOUT).unwrap();
    }
}

fn args(child: &FakeChild) -> Vec<String> {
    child
        .spec()
        .args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

fn wait_until(mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Run one: a project with a shell pane and a Claude pane split beside it.
/// Returns the store, the Claude pane and the session id it launched with.
fn first_run() -> (Shared, PaneId, String) {
    let store = Shared::default();
    let mut run = Run::start(&store);
    let shell = run.first_pane();
    run.children(1);
    let claude = run.split(shell, Some("claude"));
    let child = run.children(1).pop().unwrap();
    let launch = args(&child);
    assert_eq!(launch[0], "--session-id");
    let session = launch[1].clone();
    run.save();
    (store, claude, session)
}

#[test]
fn rf_ses_01_the_quit_save_writes_the_workspace_and_nothing_after_it() {
    let (store, claude, session) = first_run();
    let saved = store.saved().unwrap();
    let tab = &saved.projects[0].tabs[0];
    assert_eq!(saved.projects[0].root, ROOT);
    assert_eq!(tab.panes.len(), 2);
    assert_eq!(tab.focused, claude);
    let record = tab.panes.iter().find(|p| p.id == claude).unwrap();
    let agent = record.agent.as_ref().unwrap();
    assert_eq!(agent.session_id.as_ref().unwrap().as_str(), session);
    assert!(!agent.ended_explicitly);
    assert!(saved.saved_at > 0);
}

#[test]
fn rf_ses_01_changes_are_saved_without_quitting() {
    let store = Shared::default();
    let mut run = Run::start(&store);
    let first = run.first_pane();
    wait_until(|| store.saved().is_some());
    run.split(first, None);
    wait_until(|| {
        store
            .saved()
            .is_some_and(|s| s.projects[0].tabs[0].panes.len() == 2)
    });
}

#[test]
fn rf_ses_02_a_restart_restores_layout_focus_and_selection() {
    let store = Shared::default();
    let mut run = Run::start(&store);
    let (project, first_tab, a) = run.wait_for(|m| match m {
        CoreMsg::TabUpdated(t) => Some((t.project, t.id, t.focused)),
        _ => None,
    });
    let b = run.split(a, None);
    run.command(Command::ResizeSplit {
        pane: b,
        delta: 0.2,
    });
    run.command(Command::FocusPane { pane: a });
    run.command(Command::NewTab {
        project,
        name: Some("second".into()),
        profile: None,
    });
    let second = run.wait_for(|m| match m {
        CoreMsg::TabUpdated(t) if t.name == "second" => Some(t.id),
        _ => None,
    });
    run.command(Command::SelectTab { tab: first_tab });
    run.wait_for(|m| match m {
        CoreMsg::ActiveTab { tab } if *tab == Some(first_tab) => Some(()),
        _ => None,
    });
    run.save();
    let saved = store.saved().unwrap();
    drop(run);

    let mut again = Run::start(&store);
    let restored = again.snapshot();
    assert_eq!(restored.active_tab, Some(first_tab));
    assert_eq!(restored.tabs.len(), 2);
    assert!(restored.tabs.iter().any(|t| t.id == second));
    let tab = restored.tabs.iter().find(|t| t.id == first_tab).unwrap();
    assert_eq!(tab.layout.panes(), [a, b]);
    assert_eq!(tab.focused, a);
    let yien_proto::LayoutNode::Split { children, .. } = &tab.layout else {
        panic!("expected a split");
    };
    assert!((children[1].flex - 0.7).abs() < 0.01);
    for child in again.children(3) {
        assert_eq!(
            child.spec().cwd.as_deref(),
            Some(std::path::Path::new(ROOT))
        );
    }
    again.save();
    let mut resaved = store.saved().unwrap();
    resaved.saved_at = saved.saved_at;
    assert_eq!(resaved, saved);
}

#[test]
fn rf_ses_03_a_restored_claude_pane_resumes_its_session() {
    let (store, claude, session) = first_run();
    let mut again = Run::start(&store);
    let restored = again.snapshot();
    let pane = restored.panes.iter().find(|p| p.id == claude).unwrap();
    assert_eq!(pane.session_id.as_deref(), Some(session.as_str()));
    let children = again.children(2);
    let child = children
        .iter()
        .find(|c| c.spec().program == "claude")
        .unwrap();
    assert_eq!(args(child), ["--resume", session.as_str()]);
    let env = |key: &str| {
        child
            .spec()
            .env
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.to_string_lossy().into_owned())
    };
    assert_eq!(env("YIEN_PANE"), Some(claude.get().to_string()));
}

#[test]
fn rf_ses_04_a_terminated_claude_session_launches_fresh() {
    let (store, claude, session) = first_run();
    let again = Run::start(&store);
    again.children(2);
    again.command(Command::TerminatePane { pane: claude });
    wait_until(|| {
        store.saved().is_some_and(|s| {
            s.projects[0].tabs[0]
                .panes
                .iter()
                .any(|p| p.agent.as_ref().is_some_and(|a| a.ended_explicitly))
        })
    });
    drop(again);

    let third = Run::start(&store);
    let children = third.children(2);
    let child = children
        .iter()
        .find(|c| c.spec().program == "claude")
        .unwrap();
    let launch = args(child);
    assert_eq!(launch[0], "--session-id");
    assert_ne!(launch[1], session);
}

#[test]
fn rf_ses_06_a_damaged_file_restores_the_previous_generation() {
    let (store, claude, _) = first_run();
    let good = store.file("workspace.json").unwrap();
    store.insert("workspace.1.json", good);
    store.insert("workspace.json", b"{ not json".to_vec());

    let mut again = Run::start(&store);
    let restored = again.snapshot();
    assert!(restored.panes.iter().any(|p| p.id == claude));
    let notice = again.notice();
    assert!(
        notice.contains("damaged") && notice.contains("workspace.1.json"),
        "{notice}"
    );
    again.save();
    assert_eq!(
        store.file("workspace.damaged.json").as_deref(),
        Some(&b"{ not json"[..])
    );
}

#[test]
fn a_session_from_a_newer_yien_is_kept_and_not_restored() {
    let store = Shared::default();
    let newer = br#"{"schema_version": 99, "projects": []}"#.to_vec();
    store.insert("workspace.json", newer.clone());
    let mut run = Run::start(&store);
    assert!(run.snapshot().projects.is_empty());
    let notice = run.notice();
    assert!(notice.contains("newer YIEN"), "{notice}");
    run.first_pane();
    assert_eq!(store.file("workspace.v99.json").unwrap(), newer);
    run.save();
    assert_eq!(store.saved().unwrap().projects[0].root, ROOT);
}

#[test]
fn without_a_saved_session_the_start_folder_opens() {
    let store = Shared::default();
    let mut run = Run::start(&store);
    assert!(run.snapshot().projects.is_empty());
    let project = run.wait_for(|m| match m {
        CoreMsg::ProjectUpdated(p) => Some(p.root.clone()),
        _ => None,
    });
    assert_eq!(project, ROOT);
}

#[test]
fn rf_ses_01_the_quit_save_waits_no_longer_than_asked() {
    let store = Slow::default();
    let mut run = Run::start_on(store.clone());
    run.first_pane();
    let started = Instant::now();
    let result = run.core.save_session(Duration::from_millis(200));
    assert!(
        matches!(result, Err(SessionSaveError::TimedOut)),
        "{result:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    wait_until(|| store.0.saved().is_some());
}
