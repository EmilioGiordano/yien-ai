//! Closing a real ConPTY session leaves no process behind. Its own test
//! binary, so every child of this process belongs to the test.

#![cfg(windows)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers outside #[test] fns are not covered by clippy.toml's in-test allowances"
)]

use std::process::Command;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use yien_pty::{PortablePty, PtyBackend, PtyEvent, SpawnSpec};

const TIMEOUT: Duration = Duration::from_secs(20);
const CURSOR_QUERY: &[u8] = b"\x1b[6n";
const CURSOR_REPLY: &[u8] = b"\x1b[1;1R";

/// `(pid, name)` of every live process whose parent is this test.
fn children() -> Vec<(u32, String)> {
    let script = format!(
        "Get-CimInstance Win32_Process -Filter 'ParentProcessId={}' | \
         ForEach-Object {{ \"$($_.ProcessId) $($_.Name)\" }}",
        std::process::id()
    );
    let out = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let (pid, name) = line.trim().split_once(' ')?;
            Some((pid.parse().ok()?, name.to_ascii_lowercase()))
        })
        .filter(|(_, name)| name != "powershell.exe")
        .collect()
}

#[test]
fn closing_a_session_ends_its_child_and_its_console_host() {
    let session = PortablePty::new()
        .spawn(SpawnSpec::new("cmd.exe").arg("/d"))
        .unwrap();
    let child = session.pid().unwrap();
    let deadline = Instant::now() + TIMEOUT;
    let mut output = Vec::new();
    let mut answered = false;
    while !output.contains(&b'>') {
        let left = deadline.saturating_duration_since(Instant::now());
        match session.events().recv_timeout(left) {
            Ok(PtyEvent::Output(bytes)) => {
                output.extend(bytes);
                if !answered
                    && output
                        .windows(CURSOR_QUERY.len())
                        .any(|w| w == CURSOR_QUERY)
                {
                    session.write(CURSOR_REPLY.to_vec()).unwrap();
                    answered = true;
                }
            }
            Ok(PtyEvent::Exited(status)) => panic!("the shell exited: {status:?}"),
            Err(RecvTimeoutError::Timeout) => panic!("no prompt before the timeout"),
            Err(RecvTimeoutError::Disconnected) => panic!("the session closed"),
        }
    }

    let before = children();
    assert!(
        before.iter().any(|(pid, _)| *pid == child),
        "the shell is not a child: {before:?}"
    );
    let host = before
        .iter()
        .find(|(_, name)| name == "conhost.exe" || name == "openconsole.exe")
        .map(|(pid, _)| *pid)
        .unwrap_or_else(|| panic!("no console host among {before:?}"));

    let (control, events) = session.into_parts();
    std::thread::spawn(move || for _ in events {});
    let start = Instant::now();
    control.close(Duration::from_secs(5)).unwrap();
    let took = start.elapsed();

    // ConPTY may return from the close a moment before its host is gone.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut left = children();
    while left.iter().any(|(pid, _)| *pid == child || *pid == host) {
        assert!(
            Instant::now() < deadline,
            "still running after the close: {left:?}"
        );
        std::thread::sleep(Duration::from_millis(200));
        left = children();
    }
    assert!(took < Duration::from_secs(5), "the close took {took:?}");
}
