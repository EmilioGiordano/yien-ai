//! The built `yien-hook` binary against a live hook pipe server.

#![cfg(windows)]
// The helpers are not `#[test]` functions, so clippy's test exemption misses them.
#![allow(clippy::unwrap_used)]

use std::io::Write;
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use yien_agent::{HookEvent, NotificationKind};
use yien_platform::hook_pipe::{
    HOOK_PIPE_ENV, HOOK_PIPE_PREFIX, HOOK_TOKEN_ENV, HookDelivery, HookPipeServer, HookRegistry,
    READ_TIMEOUT, Rejection,
};
use yien_proto::PaneId;
use yien_proto::hook_frame::{FrameError, HEADER_LEN, MAX_HOOK_PAYLOAD};

const HOOK: &str = env!("CARGO_BIN_EXE_yien-hook");
const WAIT: Duration = Duration::from_secs(10);
/// The hook's own deadline is 1 s; the rest is process start on a busy machine.
const HOOK_EXIT_BOUND: Duration = Duration::from_secs(3);

struct Pipe {
    server: HookPipeServer,
    registry: HookRegistry,
    events: mpsc::Receiver<HookDelivery>,
}

fn start() -> Pipe {
    let registry = HookRegistry::new();
    let (tx, events) = mpsc::channel();
    let server = HookPipeServer::start(registry.clone(), move |d| {
        let _ = tx.send(d);
    })
    .unwrap();
    Pipe {
        server,
        registry,
        events,
    }
}

fn run_hook(pipe: Option<&str>, token: Option<&str>, event: &str, payload: &[u8]) -> Output {
    let mut cmd = Command::new(HOOK);
    cmd.arg(event)
        .env_remove(HOOK_PIPE_ENV)
        .env_remove(HOOK_TOKEN_ENV)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(pipe) = pipe {
        cmd.env(HOOK_PIPE_ENV, pipe);
    }
    if let Some(token) = token {
        cmd.env(HOOK_TOKEN_ENV, token);
    }
    let mut child = cmd.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let payload = payload.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&payload);
    });
    let out = child.wait_with_output().unwrap();
    writer.join().unwrap();
    out
}

fn assert_silent_success(out: &Output) {
    assert!(out.status.success(), "{:?}", out.status);
    assert!(out.stdout.is_empty(), "the hook wrote to stdout");
    assert!(out.stderr.is_empty(), "the hook wrote to stderr");
}

fn wait_until(what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn raw_client(pipe: &str) -> std::fs::File {
    std::fs::OpenOptions::new().write(true).open(pipe).unwrap()
}

#[test]
fn hook_event_reaches_the_server_decoded() {
    let pipe = start();
    let token = pipe.registry.issue(PaneId::new(11)).unwrap();
    let payload = br#"{"hook_event_name":"Notification","session_id":"s1","notification_type":"permission_prompt","message":"Claude needs your permission"}"#;
    let out = run_hook(
        Some(pipe.server.name()),
        Some(token.as_str()),
        "Notification",
        payload,
    );
    assert_silent_success(&out);
    let delivery = pipe.events.recv_timeout(WAIT).unwrap();
    assert_eq!(delivery.pane, PaneId::new(11));
    assert_eq!(
        delivery.event,
        Ok(HookEvent::Notification(NotificationKind::PermissionPrompt))
    );
    assert_eq!(delivery.payload_len, payload.len());
}

#[test]
fn rnf_11_wrong_token_is_rejected() {
    let pipe = start();
    pipe.registry.issue(PaneId::new(1)).unwrap();
    let forged = "0".repeat(64);
    let out = run_hook(
        Some(pipe.server.name()),
        Some(&forged),
        "Stop",
        br#"{"hook_event_name":"Stop"}"#,
    );
    assert_silent_success(&out);
    wait_until("the rejection", || pipe.server.stats().rejected == 1);
    assert_eq!(
        pipe.server.stats().last_rejection,
        Some(Rejection::UnknownToken)
    );
    assert!(pipe.events.try_recv().is_err());
}

#[test]
fn oversize_stdin_still_delivers_the_argv_event() {
    let pipe = start();
    let token = pipe.registry.issue(PaneId::new(2)).unwrap();
    let big = vec![b' '; MAX_HOOK_PAYLOAD + 10];
    let out = run_hook(
        Some(pipe.server.name()),
        Some(token.as_str()),
        "PreToolUse",
        &big,
    );
    assert_silent_success(&out);
    let delivery = pipe.events.recv_timeout(WAIT).unwrap();
    assert_eq!(delivery.event, Ok(HookEvent::PreToolUse));
    assert_eq!(delivery.payload_len, 0);
}

#[test]
fn oversize_frame_is_rejected_and_the_server_keeps_serving() {
    let pipe = start();
    let token = pipe.registry.issue(PaneId::new(3)).unwrap();
    let mut header = [b'Y', b'H', 1, 64, 0, 0, 0, 0, 0];
    header[5..HEADER_LEN].copy_from_slice(&((MAX_HOOK_PAYLOAD + 1) as u32).to_le_bytes());
    let mut client = raw_client(pipe.server.name());
    client.write_all(&header).unwrap();
    wait_until("the rejection", || pipe.server.stats().rejected == 1);
    assert_eq!(
        pipe.server.stats().last_rejection,
        Some(Rejection::Frame(FrameError::PayloadTooLarge(
            MAX_HOOK_PAYLOAD + 1
        )))
    );
    drop(client);

    let out = run_hook(Some(pipe.server.name()), Some(token.as_str()), "Stop", b"");
    assert_silent_success(&out);
    assert_eq!(
        pipe.events.recv_timeout(WAIT).unwrap().event,
        Ok(HookEvent::Stop)
    );
}

#[test]
fn rn_02_hook_exits_zero_quickly_without_a_server() {
    let missing = format!("{HOOK_PIPE_PREFIX}missing-{}", std::process::id());
    for (pipe, token) in [
        (Some(missing.as_str()), Some("abc")),
        (None, Some("abc")),
        (Some(missing.as_str()), None),
        (None, None),
    ] {
        let started = Instant::now();
        let out = run_hook(pipe, token, "Stop", br#"{"hook_event_name":"Stop"}"#);
        assert_silent_success(&out);
        assert!(
            started.elapsed() < HOOK_EXIT_BOUND,
            "took {:?}",
            started.elapsed()
        );
    }
}

#[test]
fn pipe_variable_outside_the_pipe_namespace_is_ignored() {
    let dir = std::env::temp_dir().join(format!("yien-hook-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("victim.txt");
    std::fs::write(&file, b"untouched").unwrap();
    let out = run_hook(
        Some(file.to_str().unwrap()),
        Some("abc"),
        "Stop",
        br#"{"hook_event_name":"Stop"}"#,
    );
    assert_silent_success(&out);
    assert_eq!(std::fs::read(&file).unwrap(), b"untouched");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn slow_client_is_cut_off_at_the_read_timeout() {
    let pipe = start();
    let mut client = raw_client(pipe.server.name());
    client.write_all(b"YH").unwrap();
    let started = Instant::now();
    wait_until("the timeout", || pipe.server.stats().rejected == 1);
    assert!(started.elapsed() + Duration::from_millis(100) >= READ_TIMEOUT);
    assert_eq!(
        pipe.server.stats().last_rejection,
        Some(Rejection::TimedOut)
    );
}

#[test]
fn many_concurrent_hooks_all_arrive() {
    const N: u64 = 16;
    let pipe = start();
    let name = pipe.server.name().to_owned();
    let hooks: Vec<_> = (0..N)
        .map(|i| {
            let token = pipe.registry.issue(PaneId::new(i)).unwrap();
            let name = name.clone();
            std::thread::spawn(move || {
                run_hook(
                    Some(&name),
                    Some(token.as_str()),
                    "UserPromptSubmit",
                    br#"{"hook_event_name":"UserPromptSubmit","prompt":"hi"}"#,
                )
            })
        })
        .collect();
    for hook in hooks {
        assert_silent_success(&hook.join().unwrap());
    }
    let mut panes: Vec<u64> = (0..N)
        .map(|_| pipe.events.recv_timeout(WAIT).unwrap())
        .inspect(|d| assert_eq!(d.event, Ok(HookEvent::UserPromptSubmit)))
        .map(|d| d.pane.get())
        .collect();
    panes.sort_unstable();
    assert_eq!(panes, (0..N).collect::<Vec<_>>());
}

#[test]
fn dropping_an_idle_server_returns_promptly() {
    let pipe = start();
    let started = Instant::now();
    drop(pipe.server);
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn two_servers_get_distinct_names() {
    let a = start();
    let b = start();
    assert_ne!(a.server.name(), b.server.name());
    assert!(a.server.name().starts_with(HOOK_PIPE_PREFIX));
}
