//! `yien-hook`: the command Claude Code runs for each hook YIEN registers
//! (ADR-09). It reads the hook payload from stdin and sends it to the hook
//! pipe named in `YIEN_HOOK_PIPE` as one frame, with the pane token from
//! `YIEN_HOOK_TOKEN` and the event name from its first argument.
//!
//! It always exits 0 within [`DEADLINE`] and never writes to stdout or
//! stderr: Claude reads hook stdout, and a slow or failing hook would stall
//! or fail the agent (plan §4.5, RN-02). No async runtime, so it starts fast.

#![forbid(unsafe_code)]

use std::io::{self, Read, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use yien_proto::hook_frame::{
    self, HOOK_PIPE_ENV, HOOK_PIPE_PREFIX, HOOK_TOKEN_ENV, MAX_HOOK_PAYLOAD,
};

/// Reading stdin, connecting and writing all end by then, done or not.
const DEADLINE: Duration = Duration::from_millis(1000);

fn main() {
    // A panic message on stderr is all a panic could add; there is no one to read it.
    std::panic::set_hook(Box::new(|_| {}));
    let deadline = Instant::now() + DEADLINE;
    let (done_tx, done_rx) = mpsc::channel();
    let worker = std::thread::Builder::new().spawn(move || {
        forward(deadline);
        let _ = done_tx.send(());
    });
    if worker.is_ok() {
        let _ = done_rx.recv_timeout(DEADLINE);
    }
    // Ends the process even while the worker is blocked on a stalled pipe.
    std::process::exit(0);
}

fn forward(deadline: Instant) {
    let payload = read_payload();
    let (Some(token), Some(pipe)) = (env(HOOK_TOKEN_ENV), env(HOOK_PIPE_ENV)) else {
        return;
    };
    if !pipe.starts_with(HOOK_PIPE_PREFIX) {
        return;
    }
    let event = std::env::args()
        .nth(1)
        .filter(|e| hook_frame::is_valid_event(e))
        .unwrap_or_default();
    let Ok(frame) = hook_frame::encode(token.as_bytes(), &event, &payload) else {
        return;
    };
    if let Some(mut conn) = connect(&pipe, deadline) {
        let _ = conn.write_all(&frame);
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// The whole of stdin, or nothing when it exceeds [`MAX_HOOK_PAYLOAD`] or
/// fails: the pipe then falls back to the argv event, so a large `Write`
/// still counts.
fn read_payload() -> Vec<u8> {
    let mut stdin = io::stdin().lock();
    let mut payload = Vec::new();
    let read = (&mut stdin)
        .take(MAX_HOOK_PAYLOAD as u64 + 1)
        .read_to_end(&mut payload);
    if read.is_err() {
        payload.clear();
    } else if payload.len() > MAX_HOOK_PAYLOAD {
        payload.clear();
        // Drained so Claude's write to stdin completes instead of failing.
        let _ = io::copy(&mut stdin, &mut io::sink());
    }
    payload
}

#[cfg(windows)]
fn connect(pipe: &str, deadline: Instant) -> Option<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;

    const ERROR_PIPE_BUSY: i32 = 231;
    // With SECURITY_SQOS_PRESENT, which std adds: the server cannot
    // impersonate this process.
    const SECURITY_ANONYMOUS: u32 = 0;
    const BUSY_RETRY: Duration = Duration::from_millis(5);

    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .security_qos_flags(SECURITY_ANONYMOUS)
            .open(pipe)
        {
            Ok(file) => return Some(file),
            Err(e)
                if e.raw_os_error() == Some(ERROR_PIPE_BUSY)
                    && Instant::now() + BUSY_RETRY < deadline =>
            {
                std::thread::sleep(BUSY_RETRY);
            }
            Err(_) => return None,
        }
    }
}

#[cfg(not(windows))]
fn connect(_pipe: &str, _deadline: Instant) -> Option<std::fs::File> {
    None
}
