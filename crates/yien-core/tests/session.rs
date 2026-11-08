//! Saving and restoring the workspace through the core, over `FakePty` and
//! an in-memory store shared between two runs.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers outside #[test] fns are not covered by clippy.toml's in-test allowances"
)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::error::TryRecvError;
use yien_core::{
    Core, CoreConfig, CoreHandle, HookConfig, SessionSaveError, SessionStore, ShellCommand,
};
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

/// A store whose writes stall, as a stalled disk would, until the test
/// opens it.
#[derive(Clone, Default)]
struct Slow(Shared, Arc<Gate>);

#[derive(Default)]
struct Gate {
    state: Mutex<GateState>,
    changed: Condvar,
}

#[derive(Default)]
struct GateState {
    reached: bool,
    open: bool,
}

impl Slow {
    /// Whether a write has reached the disk and is stalled there.
    fn stalled(&self) -> bool {
        self.1.state.lock().unwrap().reached
    }

    fn open(&self) {
        self.1.state.lock().unwrap().open = true;
        self.1.changed.notify_all();
    }
}

impl Store for Slow {
    fn read(&self, name: &str) -> Result<Option<Vec<u8>>, StoreError> {
        self.0.read(name)
    }

    fn write_atomic(&mut self, name: &str, bytes: &[u8]) -> Result<(), StoreError> {
        let mut state = self.1.state.lock().unwrap();
        state.reached = true;
        // Bounded, so a test that never opens the gate fails instead of hanging.
        let _ = self
            .1
            .changed
            .wait_timeout_while(state, TIMEOUT * 4, |s| !s.open)
            .unwrap();
        self.0.write_atomic(name, bytes)
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), StoreError> {
        self.0.rename(from, to)
    }
}

/// A store whose first read stalls (`delay`) or fails like a crashed load
/// (`panic`), for what happens before the restore is done.
#[derive(Clone)]
struct FirstRead {
    inner: Shared,
    delay: Duration,
    panic: bool,
    done: Arc<AtomicBool>,
}

impl FirstRead {
    fn new(inner: &Shared, delay: Duration, panic: bool) -> Self {
        Self {
            inner: inner.clone(),
            delay,
            panic,
            done: Arc::default(),
        }
    }
}

impl Store for FirstRead {
    fn read(&self, name: &str) -> Result<Option<Vec<u8>>, StoreError> {
        if !self.done.swap(true, Ordering::SeqCst) {
            std::thread::sleep(self.delay);
            assert!(!self.panic, "simulated failure while loading");
        }
        self.inner.read(name)
    }

    fn write_atomic(&mut self, name: &str, bytes: &[u8]) -> Result<(), StoreError> {
        self.inner.write_atomic(name, bytes)
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), StoreError> {
        self.inner.rename(from, to)
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
        Self::start_with(store, None)
    }

    fn start_with(store: impl Store + Send + 'static, hooks: Option<HookConfig>) -> Self {
        Self::configured(
            store,
            CoreConfig {
                hooks,
                ..CoreConfig::default()
            },
        )
    }

    /// Starts a core on `store` with `config`'s other settings.
    fn configured(store: impl Store + Send + 'static, config: CoreConfig) -> Self {
        let backend = FakePty::new();
        let config = CoreConfig {
            shell: ShellCommand::new("fake-shell"),
            session: Some(SessionStore::new(store)),
            start_project: Some(ROOT.to_owned()),
            ..config
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

    /// The next notice about `pane`. The pane must not report an exit
    /// before it.
    fn pane_notice(&mut self, pane: PaneId) -> String {
        self.wait_for(|m| match m {
            CoreMsg::Error {
                pane: Some(p),
                message,
            } if *p == pane => Some(message.clone()),
            CoreMsg::PaneExited { pane: p, .. } if *p == pane => {
                panic!("the pane reported an exit")
            }
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
    wait_until(|| {
        store.saved().is_some_and(|s| {
            s.projects[0].tabs[0].panes.iter().any(|p| {
                p.agent
                    .as_ref()
                    .and_then(|a| a.session_id.as_ref())
                    .is_some_and(|id| id.as_str() == session)
            })
        })
    });
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
    wait_until(|| {
        store
            .saved()
            .is_some_and(|s| s.selected.tab == Some(first_tab) && s.projects[0].tabs.len() == 2)
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
    wait_until(|| store.saved().is_some_and(|s| s.saved_at != saved.saved_at));
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
    wait_until(|| store.file("workspace.damaged.json").is_some());
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
    wait_until(|| store.saved().is_some());
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
    let run = Run::start_on(store.clone());
    // The writer holds the workspace once its debounced write reaches the disk.
    wait_until(|| store.stalled());
    let asked = Duration::from_millis(200);
    let started = Instant::now();
    let result = run.core.save_session(asked);
    let took = started.elapsed();
    // The disk stays stalled, so only giving up at the deadline returns.
    assert!(
        matches!(result, Err(SessionSaveError::TimedOut)),
        "{result:?}"
    );
    assert!(took >= asked && took < TIMEOUT, "{took:?}");
    store.open();
    wait_until(|| store.0.saved().is_some());
}

#[test]
fn rf_ses_01_nothing_after_the_quit_save_reaches_the_store() {
    let store = Shared::default();
    let mut run = Run::start(&store);
    let first = run.first_pane();
    wait_until(|| store.saved().is_some());
    run.save();
    let saved = store.file("workspace.json").unwrap();
    run.split(first, None);
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(store.file("workspace.json").unwrap(), saved);
    assert!(!run.core.save_session(TIMEOUT).unwrap());
}

#[test]
fn rf_ses_01_the_quit_shutdown_leaves_the_saved_session_as_it_was() {
    let store = Shared::default();
    let mut run = Run::start(&store);
    let first = run.first_pane();
    run.split(first, None);
    run.children(2);
    run.save();
    let saved = store.file("workspace.json").unwrap();
    assert_eq!(run.core.shutdown(TIMEOUT).unwrap(), 2);
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(store.file("workspace.json").unwrap(), saved);
}

#[test]
fn rf_ses_01_nothing_after_the_shutdown_reaches_the_store_even_without_the_quit_save() {
    let store = Shared::default();
    let mut run = Run::start(&store);
    let first = run.first_pane();
    wait_until(|| store.saved().is_some());
    run.core.shutdown(TIMEOUT).unwrap();
    // A write already under way when saving stopped may still land.
    std::thread::sleep(Duration::from_millis(300));
    let saved = store.file("workspace.json").unwrap();
    run.split(first, None);
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(store.file("workspace.json").unwrap(), saved);
}

#[test]
fn rf_ses_01_a_quit_before_the_restore_leaves_the_file_as_it_was() {
    let (store, _, _) = first_run();
    let before = store.file("workspace.json").unwrap();
    let run = Run::start_on(FirstRead::new(&store, Duration::from_millis(500), false));
    assert!(!run.core.save_session(TIMEOUT).unwrap());
    run.children(2);
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(store.file("workspace.json").unwrap(), before);
}

#[test]
fn rf_ses_02_a_session_that_failed_to_load_is_set_aside_before_saving() {
    let (store, _, _) = first_run();
    let before = store.file("workspace.json").unwrap();
    let mut run = Run::start_on(FirstRead::new(&store, Duration::ZERO, true));
    let notice = run.notice();
    assert!(notice.contains("workspace.unrestored.json"), "{notice}");
    run.first_pane();
    wait_until(|| store.saved().is_some());
    assert_eq!(store.file("workspace.unrestored.json").unwrap(), before);
    let fresh = store.saved().unwrap();
    assert_eq!(fresh.projects[0].tabs[0].panes.len(), 1);
}

#[test]
fn rf_ses_03_a_relaunch_after_a_restore_starts_a_new_session() {
    let (store, claude, session) = first_run();
    let run = Run::start(&store);
    let children = run.children(2);
    let resumed = children
        .iter()
        .find(|c| c.spec().program == "claude")
        .unwrap();
    assert_eq!(args(resumed)[0], "--resume");
    run.command(Command::RelaunchPane { pane: claude });
    let relaunched = run.children(1).pop().unwrap();
    let launch = args(&relaunched);
    assert_eq!(launch[0], "--session-id");
    assert_ne!(launch[1], session);
}

#[cfg(windows)]
#[test]
fn rf_ses_03_a_resumed_claude_gets_the_hook_settings_and_a_token() {
    let (store, _, session) = first_run();
    let hooks = HookConfig {
        hook_exe: std::env::current_exe().unwrap(),
        settings_dir: std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("yien-session-hooks-{}", std::process::id())),
    };
    let settings = hooks.settings_file();
    let run = Run::start_with(store, Some(hooks));
    let children = run.children(2);
    let child = children
        .iter()
        .find(|c| c.spec().program == "claude")
        .unwrap();
    let launch = args(child);
    assert_eq!(launch[..3], ["--resume", session.as_str(), "--settings"]);
    assert_eq!(std::path::PathBuf::from(&launch[3]), settings);
    let token = child
        .spec()
        .env
        .iter()
        .rev()
        .find(|(k, _)| k == "YIEN_HOOK_TOKEN")
        .map(|(_, v)| v.to_string_lossy().into_owned())
        .unwrap();
    assert_eq!(token.len(), 64);
}

#[test]
fn rf_ses_02_restored_panes_start_at_the_sizes_the_ui_sends() {
    let (store, claude, _) = first_run();
    let mut again = Run::configured(
        store.clone(),
        CoreConfig {
            size_wait: TIMEOUT * 2,
            ui_wait: TIMEOUT * 2,
            ..CoreConfig::default()
        },
    );
    let restored = again.snapshot();
    let panes = restored.tabs[0].layout.panes();
    let shell = *panes.iter().find(|p| **p != claude).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    assert!(again.backend.take_spawned().is_empty(), "spawned too early");
    for (pane, cols) in [(shell, 61), (claude, 58)] {
        again
            .core
            .try_send(ClientMsg::ResizePane {
                pane,
                cols,
                rows: 27,
            })
            .unwrap();
    }
    for child in again.children(2) {
        let cols = if args(&child).iter().any(|a| a == "--resume") {
            58
        } else {
            61
        };
        assert_eq!((child.spec().cols, child.spec().rows), (cols, 27));
    }
}

#[test]
fn rf_ses_03_a_restored_claude_terminated_before_it_starts_relaunches_fresh() {
    let (store, claude, session) = first_run();
    let mut again = Run::configured(
        store,
        CoreConfig {
            size_wait: TIMEOUT * 2,
            ui_wait: TIMEOUT * 2,
            ..CoreConfig::default()
        },
    );
    again.snapshot();
    again.command(Command::TerminatePane { pane: claude });
    again.wait_for(|m| match m {
        CoreMsg::PaneExited { pane, code: None } if *pane == claude => Some(()),
        _ => None,
    });
    again.command(Command::RelaunchPane { pane: claude });
    let child = again.children(1).pop().unwrap();
    let launch = args(&child);
    assert!(!launch.iter().any(|a| a == "--resume"), "{launch:?}");
    assert!(!launch.contains(&session), "{launch:?}");
}

/// Restores `store` and returns the run and its resumed Claude child.
fn resumed(store: &Shared) -> (Run, FakeChild) {
    let run = Run::start(store);
    let claude = run
        .children(2)
        .into_iter()
        .find(|c| c.spec().program == "claude")
        .unwrap();
    assert_eq!(args(&claude)[0], "--resume");
    (run, claude)
}

fn saved_session_of(store: &Shared, pane: PaneId) -> Option<String> {
    let saved = store.saved()?;
    let record = saved.projects[0].tabs[0]
        .panes
        .iter()
        .find(|p| p.id == pane)?
        .clone();
    assert_eq!(record.profile.as_str(), "claude");
    record
        .agent
        .and_then(|a| a.session_id)
        .map(|s| s.as_str().to_owned())
}

#[test]
fn rf_ses_11_a_resume_that_finds_no_conversation_opens_a_shell_and_keeps_the_session() {
    let (store, claude, session) = first_run();
    let (mut again, child) = resumed(&store);
    child.output(format!("No conversation found with session ID: {session}\r\n").as_bytes());
    child.exit(1);
    let notice = again.pane_notice(claude);
    assert!(notice.contains("could not resume"), "{notice}");
    assert!(notice.contains("not found"), "{notice}");
    let shell = again.children(1).pop().unwrap();
    assert_eq!(shell.spec().program, "fake-shell");
    assert_eq!(
        shell.spec().cwd.as_deref(),
        Some(std::path::Path::new(ROOT))
    );
    // The pane says what runs in it; the saved session keeps the agent.
    again.wait_for(|m| match m {
        CoreMsg::PaneUpdated(p) if p.id == claude && p.profile == "Shell" => Some(()),
        _ => None,
    });
    again.save();
    assert_eq!(saved_session_of(&store, claude), Some(session.clone()));
    drop(again);

    // Nothing was lost: the next start tries the same session again.
    let (again, child) = resumed(&store);
    assert_eq!(args(&child)[1], session);
    drop(again);
}

#[test]
fn rf_ses_11_a_resume_that_exits_with_an_error_at_once_opens_a_shell() {
    let (store, claude, _) = first_run();
    let (mut again, child) = resumed(&store);
    child.exit(1);
    let notice = again.pane_notice(claude);
    assert!(notice.contains("code 1"), "{notice}");
    assert_eq!(again.children(1)[0].spec().program, "fake-shell");
}

#[test]
fn rf_ses_11_the_fallback_notice_keeps_what_the_cli_said() {
    let (store, claude, _) = first_run();
    let (mut again, child) = resumed(&store);
    child.output(b"Error: invalid API key\r\n\r\n");
    child.exit(1);
    let notice = again.pane_notice(claude);
    assert!(
        notice.ends_with("It said: Error: invalid API key"),
        "{notice}"
    );
}

#[test]
fn rf_ses_11_a_relaunch_after_the_fallback_starts_a_new_agent_session() {
    let (store, claude, session) = first_run();
    let (mut again, child) = resumed(&store);
    child.exit(1);
    again.pane_notice(claude);
    let shell = again.children(1).pop().unwrap();
    again.command(Command::RelaunchPane { pane: claude });
    let relaunched = again.children(1).pop().unwrap();
    assert!(shell.was_killed());
    assert_eq!(relaunched.spec().program, "claude");
    again.wait_for(|m| match m {
        CoreMsg::PaneUpdated(p) if p.id == claude && p.profile == "Claude Code" => Some(()),
        _ => None,
    });
    let launch = args(&relaunched);
    assert_eq!(launch[0], "--session-id");
    assert_ne!(launch[1], session);
    wait_until(|| saved_session_of(&store, claude).as_deref() == Some(launch[1].as_str()));
}

#[test]
fn rf_ses_11_a_resumed_pane_the_user_stops_does_not_open_a_shell() {
    let (store, claude, _) = first_run();
    let (mut again, _child) = resumed(&store);
    again.command(Command::TerminatePane { pane: claude });
    again.wait_for(|m| match m {
        CoreMsg::PaneExited { pane, .. } if *pane == claude => Some(()),
        _ => None,
    });
    std::thread::sleep(Duration::from_millis(300));
    assert!(again.backend.take_spawned().is_empty());
}

/// Waits for `pane` to exit and checks no shell opened in its place.
fn exits_without_a_shell(run: &mut Run, pane: PaneId) {
    run.wait_for(|m| match m {
        CoreMsg::PaneExited { pane: p, .. } if *p == pane => Some(()),
        _ => None,
    });
    std::thread::sleep(Duration::from_millis(300));
    assert!(run.backend.take_spawned().is_empty());
}

#[test]
fn rf_ses_11_a_resumed_agent_the_user_typed_into_ends_without_a_shell() {
    let (store, claude, _) = first_run();
    let (mut again, child) = resumed(&store);
    again
        .core
        .try_send(ClientMsg::Input {
            pane: claude,
            bytes: yien_proto::InputBytes::new(b"\x03".to_vec()),
        })
        .unwrap();
    assert!(child.recv_input(TIMEOUT).is_some());
    child.exit(1);
    exits_without_a_shell(&mut again, claude);
}

#[cfg(windows)]
#[test]
fn rf_ses_11_a_resumed_agent_that_sent_a_hook_ends_without_a_shell() {
    use std::io::Write;
    use yien_platform::hook_pipe::{HOOK_PIPE_ENV, HOOK_TOKEN_ENV, connect_client};
    use yien_proto::{AgentState, hook_frame};

    let (store, claude, session) = first_run();
    let hooks = HookConfig {
        hook_exe: std::env::current_exe().unwrap(),
        settings_dir: std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("yien-session-veto-{}", std::process::id())),
    };
    let mut again = Run::start_with(store, Some(hooks));
    let child = again
        .children(2)
        .into_iter()
        .find(|c| c.spec().program == "claude")
        .unwrap();
    let env = |key: &str| {
        child
            .spec()
            .env
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.to_string_lossy().into_owned())
            .unwrap()
    };
    let (token, pipe) = (env(HOOK_TOKEN_ENV), env(HOOK_PIPE_ENV));
    for (event, extra) in [
        ("SessionStart", r#","source":"resume""#),
        ("UserPromptSubmit", ""),
    ] {
        let payload = format!(r#"{{"hook_event_name":"{event}","session_id":"{session}"{extra}}}"#);
        let frame = hook_frame::encode(token.as_bytes(), event, payload.as_bytes()).unwrap();
        let mut conn = connect_client(&pipe, Instant::now() + TIMEOUT).unwrap();
        conn.write_all(&frame).unwrap();
    }
    again.wait_for(|m| match m {
        CoreMsg::PaneState { pane, state } if *pane == claude && *state == AgentState::Working => {
            Some(())
        }
        _ => None,
    });
    child.exit(1);
    exits_without_a_shell(&mut again, claude);
}

#[test]
fn rf_ses_11_quitting_while_a_resume_starts_opens_no_shell() {
    let (store, claude, _) = first_run();
    let (mut again, child) = resumed(&store);
    again.core.shutdown(TIMEOUT).unwrap();
    assert!(child.was_killed());
    std::thread::sleep(Duration::from_millis(300));
    assert!(again.backend.take_spawned().is_empty());
    while let Ok(msg) = again.events.try_recv() {
        if let CoreMsg::Error {
            pane: Some(p),
            message,
        } = msg
        {
            assert_ne!(p, claude, "{message}");
        }
    }
}

#[test]
fn a_session_whose_projects_were_all_closed_starts_empty() {
    let store = Shared::default();
    let mut run = Run::start(&store);
    let project = run.wait_for(|m| match m {
        CoreMsg::ProjectUpdated(p) => Some(p.id),
        _ => None,
    });
    run.children(1);
    run.command(Command::CloseProject { project });
    run.wait_for(|m| matches!(m, CoreMsg::ProjectClosed { .. }).then_some(()));
    wait_until(|| store.saved().is_some_and(|s| s.projects.is_empty()));
    run.save();
    drop(run);

    let mut next = Run::start(&store);
    assert!(next.snapshot().projects.is_empty());
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        next.backend.take_spawned().is_empty(),
        "the start folder was opened"
    );
    loop {
        match next.events.try_recv() {
            Ok(CoreMsg::ProjectUpdated(p)) => panic!("{} was opened", p.root),
            Ok(_) => {}
            Err(_) => break,
        }
    }
}
