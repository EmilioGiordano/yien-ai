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

use yien_platform::hook_pipe::connect_client;
use yien_proto::hook_frame::{self, HOOK_PIPE_ENV, HOOK_TOKEN_ENV, MAX_HOOK_PAYLOAD};

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
    if hook_frame::hook_pipe_pid(&pipe).is_none() {
        return;
    }
    let event = std::env::args()
        .nth(1)
        .filter(|e| hook_frame::is_valid_event(e))
        .unwrap_or_default();
    let Ok(frame) = hook_frame::encode(token.as_bytes(), &event, &payload) else {
        return;
    };
    if let Some(mut conn) = connect_client(&pipe, deadline) {
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
