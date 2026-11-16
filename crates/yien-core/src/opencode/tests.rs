use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use yien_vt::{AlacrittyEngine, OscSignal, VtConfig, VtEngine};

use super::*;

const OPENCODE_PERMISSION: &[u8] =
    include_bytes!("../../tests/fixtures/osc7501/opencode-2.2.7-permission.bin");

/// The session OpenCode named in the capture.
const CAPTURED: &str = "ses_e29191ec0ffe3tC2NzOxg6Pfch";

/// Feeds `capture` in `chunk`-byte reads; returns, after each record, whether
/// the captured session is known, and how many records named a new one.
fn track(capture: &[u8], chunk: usize) -> (Vec<bool>, usize) {
    let mut engine = AlacrittyEngine::new(VtConfig::default(), 120, 40);
    let mut sessions = Sessions::default();
    let mut known = Vec::new();
    let mut learned = 0;
    for bytes in capture.chunks(chunk) {
        for signal in engine.feed(bytes).signals {
            if let OscSignal::Status(status) = signal {
                learned += usize::from(sessions.absorb(&status));
                known.push(sessions.contains(CAPTURED));
            }
        }
    }
    (known, learned)
}

#[test]
fn rf_age_05_a_real_opencode_turn_names_its_session_until_it_exits() {
    let (known, learned) = track(OPENCODE_PERMISSION, OPENCODE_PERMISSION.len());
    // idle, working, working:id, blocked, blocked:id, working, working:id,
    // clear:id, idle, clear.
    assert_eq!(
        known,
        [
            false, false, true, true, true, true, true, true, true, false
        ]
    );
    assert_eq!(learned, 1, "named once, however often it comes back");
    for chunk in [1, 7, 64] {
        assert_eq!(
            track(OPENCODE_PERMISSION, chunk),
            (known.clone(), 1),
            "{chunk}"
        );
    }
}

fn record(app: StatusApp, state: ProgramState, id: Option<&str>) -> ProgramStatus {
    ProgramStatus {
        state,
        kind: None,
        app,
        sub_session: id.is_some(),
        id: id.and_then(StatusId::new),
    }
}

#[test]
fn rn_02_only_opencode_names_sessions() {
    let mut sessions = Sessions::default();
    for app in [StatusApp::ClaudeCode, StatusApp::Codex, StatusApp::Other] {
        assert!(!sessions.absorb(&record(app, ProgramState::Working, Some("ses_1"))));
    }
    assert!(!sessions.contains("ses_1"));
    assert!(!sessions.reported());
    // OpenCode's own `clear:id` carries no `app`: it names nothing either.
    sessions.absorb(&record(
        StatusApp::Other,
        ProgramState::Clear,
        Some("ses_1"),
    ));
    assert!(!sessions.contains("ses_1"));
    assert!(sessions.absorb(&record(
        StatusApp::OpenCode,
        ProgramState::Working,
        Some("ses_1")
    )));
    assert!(sessions.contains("ses_1") && sessions.reported());
    assert!(!sessions.contains("ses_2"));
    // A `clear:id` keeps the session: OpenCode still shows it.
    sessions.absorb(&record(
        StatusApp::Other,
        ProgramState::Clear,
        Some("ses_1"),
    ));
    assert!(sessions.contains("ses_1"));
    // A bare `clear`, from any program, forgets them all.
    sessions.absorb(&record(StatusApp::Other, ProgramState::Clear, None));
    assert!(!sessions.contains("ses_1") && !sessions.reported());
}

#[test]
fn a_pane_remembers_its_latest_sessions_only() {
    let mut sessions = Sessions::default();
    for n in 0..=MAX_SESSIONS_SEEN {
        let id = format!("ses_{n}");
        assert!(sessions.absorb(&record(
            StatusApp::OpenCode,
            ProgramState::Working,
            Some(&id)
        )));
    }
    assert!(!sessions.contains("ses_0"));
    assert!(sessions.contains("ses_1"));
    assert!(sessions.contains(&format!("ses_{MAX_SESSIONS_SEEN}")));
}

/// A folder of its own under the temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("yien-oc-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn file(&self) -> PathBuf {
        self.0.join("service.json")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_service(path: &Path, url: &str, password: &str) {
    let json = format!(
        r#"{{"id":"svc","version":"2.2.7","url":"{url}","pid":1,"password":"{password}"}}"#
    );
    std::fs::write(path, json).unwrap();
}

fn quick(files: Vec<PathBuf>, retry: Duration) -> OpenCodeService {
    OpenCodeService {
        files,
        retry: (retry, retry),
        idle_timeout: Duration::from_secs(5),
    }
}

#[test]
fn rnf_11_a_service_file_off_this_machine_is_not_used() {
    let dir = TempDir::new("remote");
    let watch = OpenCodeWatch::new(quick(vec![dir.file()], Duration::from_secs(1)));
    assert!(watch.service_file().is_none(), "missing");
    write_service(&dir.file(), "http://10.0.0.2:28894", "pw");
    assert!(watch.service_file().is_none(), "remote");
    write_service(&dir.file(), "http://127.0.0.1:28894", "pw");
    assert!(watch.service_file().is_some());
}

/// Waits until the watcher thread is gone.
fn assert_stops(watch: &OpenCodeWatch) {
    let until = Instant::now() + Duration::from_secs(10);
    while watch.state().running {
        assert!(Instant::now() < until, "the watcher did not stop");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn the_watcher_runs_only_while_a_pane_holds_it() {
    let dir = TempDir::new("lifecycle");
    let watch = OpenCodeWatch::new(quick(vec![dir.file()], Duration::from_secs(30)));
    assert!(!watch.state().running);
    let first = watch.join();
    let second = watch.join();
    assert!(watch.state().running);
    drop(first);
    assert!(watch.state().running, "one pane still runs OpenCode");
    drop(second);
    // It waits 30 s between tries, but leaves at once.
    assert_stops(&watch);
    let again = watch.join();
    assert!(watch.state().running);
    drop(again);
    assert_stops(&watch);
}

#[cfg(windows)]
mod stream {
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener, TcpStream};

    use super::*;

    const HEAD: &[u8] =
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
    const CONNECTED: &[u8] = b"data: {\"type\":\"server.connected\",\"data\":{}}\n\n";

    fn failed(session: &str) -> Vec<u8> {
        format!(
            "data: {{\"type\":\"session.execution.failed\",\"data\":{{\"sessionID\":\"{session}\",\
             \"error\":{{\"type\":\"provider.no-route\",\"message\":\"m\"}}}}}}\n\n"
        )
        .into_bytes()
    }

    /// Reads one request's head.
    fn request(stream: &mut TcpStream) -> String {
        let mut request = Vec::new();
        let mut byte = [0u8; 1];
        while !request.ends_with(b"\r\n\r\n") {
            if stream.read(&mut byte).unwrap() == 0 {
                break;
            }
            request.push(byte[0]);
        }
        String::from_utf8_lossy(&request).to_ascii_lowercase()
    }

    /// Closes after the client has: dropping first races WinHTTP's read.
    fn close(mut stream: TcpStream) {
        let _ = stream.shutdown(Shutdown::Write);
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut byte = [0u8; 64];
        while matches!(stream.read(&mut byte), Ok(n) if n > 0) {}
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
    }

    fn next_failure(runtime: &tokio::runtime::Runtime, watching: &mut Watching) -> Arc<str> {
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(10), watching.failed())
                .await
                .expect("a failure arrives")
        })
    }

    #[test]
    fn rf_age_05_a_failure_is_read_and_a_dropped_stream_reopens_with_the_new_password() {
        let dir = TempDir::new("reconnect");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        write_service(&dir.file(), &url, "pw1");
        let file = dir.file();
        let server = std::thread::spawn(move || {
            let (mut first, _) = listener.accept().unwrap();
            let one = request(&mut first);
            first.write_all(HEAD).unwrap();
            first.write_all(CONNECTED).unwrap();
            // The service restarts with a new password, dropping the stream.
            write_service(&file, &url, "pw2");
            close(first);
            let (mut second, _) = listener.accept().unwrap();
            let two = request(&mut second);
            second.write_all(HEAD).unwrap();
            second.write_all(CONNECTED).unwrap();
            second.write_all(&failed("ses_other")).unwrap();
            second.write_all(&failed(CAPTURED)).unwrap();
            second.flush().unwrap();
            (one, two, second)
        });
        let watch = OpenCodeWatch::new(quick(vec![dir.file()], Duration::from_millis(50)));
        let mut watching = watch.join();
        let runtime = runtime();
        assert_eq!(&*next_failure(&runtime, &mut watching), "ses_other");
        assert_eq!(&*next_failure(&runtime, &mut watching), CAPTURED);
        let (one, two, mut second) = server.join().unwrap();
        for (request, encoded) in [
            (&one, "b3blbmnvzgu6chcx"), // opencode:pw1
            (&two, "b3blbmnvzgu6chcy"), // opencode:pw2
        ] {
            assert!(
                request.starts_with("get /api/event http/1.1\r\n"),
                "{request}"
            );
            assert!(
                request.contains(&format!("authorization: basic {encoded}\r\n")),
                "{request}"
            );
            assert!(request.contains("accept: text/event-stream\r\n"));
        }
        // The last pane goes; the next data ends the stream and the thread.
        drop(watching);
        second.write_all(b": heartbeat\n\n").unwrap();
        close(second);
        assert_stops(&watch);
    }

    #[test]
    fn a_pane_learning_a_session_brings_the_watcher_to_a_service_that_just_started() {
        let dir = TempDir::new("poke");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        // No service yet, and a wait far longer than the test.
        let watch = OpenCodeWatch::new(quick(vec![dir.file()], Duration::from_secs(600)));
        let mut watching = watch.join();
        std::thread::sleep(Duration::from_millis(100));
        write_service(&dir.file(), &url, "pw");
        watching.poke();
        let (mut stream, _) = listener.accept().unwrap();
        request(&mut stream);
        stream.write_all(HEAD).unwrap();
        stream.write_all(CONNECTED).unwrap();
        stream.write_all(&failed(CAPTURED)).unwrap();
        assert_eq!(&*next_failure(&runtime(), &mut watching), CAPTURED);
        drop(watching);
        stream.write_all(b": heartbeat\n\n").unwrap();
        close(stream);
        assert_stops(&watch);
    }
}
