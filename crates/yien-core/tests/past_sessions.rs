//! Listing and resuming past agent sessions through the core, over
//! `FakePty` and a fixture CLI home, never the real one.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers outside #[test] fns are not covered by clippy.toml's in-test allowances"
)]

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::error::TryRecvError;
use yien_core::{Core, CoreConfig, CoreHandle, ShellCommand};
use yien_platform::agent_sessions::CliDirs;
use yien_proto::{AgentSession, ClientMsg, Command, CoreMsg, PaneId, ProfileId, ProjectId};
use yien_pty::{FakeChild, FakePty};
use yien_session::claude_project_dir_name;

const TIMEOUT: Duration = Duration::from_secs(5);
const ROOT: &str = env!("CARGO_MANIFEST_DIR");
const A: &str = "aaaaaaaa-0000-4000-8000-000000000001";
const B: &str = "bbbbbbbb-0000-4000-8000-000000000002";
const C: &str = "cccccccc-0000-4000-8000-000000000003";

/// A fake home whose Claude Code folder holds three sessions of [`ROOT`],
/// `B` the newest and `A` the oldest.
fn fake_home(name: &str) -> PathBuf {
    let home = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("yien-core-past-{name}"));
    let _ = fs::remove_dir_all(&home);
    let folder = home
        .join(".claude")
        .join("projects")
        .join(claude_project_dir_name(ROOT));
    fs::create_dir_all(&folder).unwrap();
    for (id, title, hours) in [(A, "oldest", 30), (B, "newest", 1), (C, "middle", 5)] {
        let path = folder.join(format!("{id}.jsonl"));
        let cwd = ROOT.replace('\\', r"\\");
        fs::write(
            &path,
            format!(
                "{{\"type\":\"user\",\"cwd\":\"{cwd}\",\"message\":{{\"content\":\"{title}\"}}}}\n"
            ),
        )
        .unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(hours * 3600))
            .unwrap();
    }
    home
}

struct Run {
    core: CoreHandle,
    events: Receiver<CoreMsg>,
    backend: FakePty,
    project: ProjectId,
    shell: PaneId,
}

impl Run {
    /// A core with the project at [`ROOT`] open on a shell, reading CLI
    /// data from `home`.
    fn start(home: Option<&Path>) -> Self {
        let backend = FakePty::new();
        let config = CoreConfig {
            shell: ShellCommand::new("fake-shell"),
            start_project: Some(ROOT.to_owned()),
            cli_dirs: home.map(CliDirs::under_home).unwrap_or_default(),
            ..CoreConfig::default()
        };
        let (core, events) = Core::start(Arc::new(backend.clone()), config).unwrap();
        let mut run = Self {
            core,
            events,
            backend,
            project: ProjectId::new(0),
            shell: PaneId::new(0),
        };
        run.project = run.wait_for(|m| match m {
            CoreMsg::ProjectUpdated(p) => Some(p.id),
            _ => None,
        });
        run.shell = run.wait_for(|m| match m {
            CoreMsg::TabUpdated(t) => Some(t.focused),
            _ => None,
        });
        assert_eq!(run.children(1)[0].spec().program, "fake-shell");
        run
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

    fn sessions(&mut self) -> Vec<AgentSession> {
        let project = self.project;
        self.command(Command::ListAgentSessions { project });
        self.wait_for(|m| match m {
            CoreMsg::AgentSessions {
                project: p,
                sessions,
            } if *p == project => Some(sessions.clone()),
            _ => None,
        })
    }

    fn resume(&self, id: &str) {
        self.command(Command::ResumeAgentSession {
            project: self.project,
            profile: ProfileId::new("claude"),
            session_id: id.to_owned(),
        });
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
fn rf_ses_07_the_core_lists_a_projects_sessions_newest_first() {
    let home = fake_home("list");
    let mut run = Run::start(Some(&home));
    let sessions = run.sessions();
    let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, [B, C, A]);
    let titles: Vec<&str> = sessions
        .iter()
        .map(|s| s.title.as_ref().unwrap().as_str())
        .collect();
    assert_eq!(titles, ["newest", "middle", "oldest"]);
    assert!(sessions.iter().all(|s| s.profile.as_str() == "claude"));
    assert!(sessions[0].modified_ms > sessions[1].modified_ms);
}

#[test]
fn rf_ses_07_without_cli_folders_nothing_is_listed() {
    let mut run = Run::start(None);
    assert!(run.sessions().is_empty());
}

#[test]
fn rf_ses_07_a_resumed_session_runs_its_cli_with_resume_in_the_project_folder() {
    let home = fake_home("resume");
    let mut run = Run::start(Some(&home));
    run.resume(C);
    let child = run.children(1).pop().unwrap();
    assert_eq!(child.spec().program, "claude");
    assert_eq!(args(&child), ["--resume", C]);
    assert_eq!(child.spec().cwd.as_deref(), Some(Path::new(ROOT)));
    let shell = run.shell;
    let pane = run.wait_for(|m| match m {
        CoreMsg::PaneUpdated(p) if p.id != shell && p.profile_id.as_str() == "claude" => {
            Some(p.clone())
        }
        _ => None,
    });
    assert_eq!(pane.session_id.as_deref(), Some(C));

    // Asking again focuses that pane instead of a second program.
    run.resume(C);
    run.wait_for(|m| match m {
        CoreMsg::ActiveTab { tab: Some(t) } if *t == pane.tab => Some(()),
        _ => None,
    });
    std::thread::sleep(Duration::from_millis(100));
    assert!(run.backend.take_spawned().is_empty());
}

#[test]
fn rf_ses_07_a_resume_that_fails_falls_back_to_a_shell() {
    let home = fake_home("fallback");
    let mut run = Run::start(Some(&home));
    run.resume(A);
    let child = run.children(1).pop().unwrap();
    let shell = run.shell;
    let pane = run.wait_for(|m| match m {
        CoreMsg::PaneUpdated(p) if p.profile_id.as_str() == "claude" && p.id != shell => Some(p.id),
        _ => None,
    });
    child.output(format!("No conversation found with session ID: {A}\r\n").as_bytes());
    child.exit(1);
    let notice = run.wait_for(|m| match m {
        CoreMsg::Error {
            pane: Some(p),
            message,
        } if *p == pane => Some(message.clone()),
        _ => None,
    });
    assert!(notice.contains("could not resume"), "{notice}");
    let shell = run.children(1).pop().unwrap();
    assert_eq!(shell.spec().program, "fake-shell");
    assert_eq!(shell.spec().cwd.as_deref(), Some(Path::new(ROOT)));
}

#[test]
fn rf_ses_07_a_bad_resume_is_refused_with_a_reason() {
    let mut run = Run::start(None);
    run.resume("--dangerously-skip-permissions");
    let message = run.wait_for(|m| match m {
        CoreMsg::Error { message, .. } => Some(message.clone()),
        _ => None,
    });
    assert!(message.contains("not a session id"), "{message}");
    std::thread::sleep(Duration::from_millis(100));
    assert!(run.backend.take_spawned().is_empty());
}
