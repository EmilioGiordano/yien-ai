//! `--test-control <name>` (ADR-21): the pipe a test script drives the
//! window through, wired to the window's thread. Off unless the flag is
//! given.

use std::ffi::OsString;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use yien_platform::test_control::{TestControlError, TestControlServer};
use yien_proto::test_control::{Reply, Request};
use yien_ui::{TestCall, TestControl};

/// The flag that turns test control on; the pipe's name follows it.
pub const FLAG: &str = "--test-control";

/// How long a request waits for the window. A busy window answers well
/// within it; one that does not is reported as stuck rather than waited on.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(5);

/// How much longer than a call's deadline the pipe waits for the reply, so
/// a call the window took just before its deadline is still answered.
const ANSWER_GRACE: Duration = Duration::from_secs(1);

/// Requests waiting for the window; the pipe serves one client at a time,
/// so one is in flight in practice.
const QUEUE: usize = 4;

/// Why the command line does not name a test control pipe properly.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ArgError {
    /// The flag is last, or followed by another flag.
    #[error("{FLAG} needs a name")]
    MissingName,
    /// The name is not valid Unicode.
    #[error("the {FLAG} name is not valid text")]
    NotText,
    /// The flag appears more than once.
    #[error("{FLAG} is given more than once")]
    Repeated,
}

/// The name given after [`FLAG`] in `args` (without the program name), or
/// `None` when the flag is absent: test control is off by default. Other
/// arguments are left alone.
pub fn requested(args: impl IntoIterator<Item = OsString>) -> Result<Option<String>, ArgError> {
    let mut args = args.into_iter();
    let mut name = None;
    while let Some(arg) = args.next() {
        if arg != FLAG {
            continue;
        }
        if name.is_some() {
            return Err(ArgError::Repeated);
        }
        let value = args.next().ok_or(ArgError::MissingName)?;
        let value = value.into_string().map_err(|_| ArgError::NotText)?;
        if value.starts_with('-') {
            return Err(ArgError::MissingName);
        }
        name = Some(value);
    }
    Ok(name)
}

/// Starts the pipe named `name`; the window answers through the returned
/// [`TestControl`]. Keep the server alive as long as the window.
pub fn start(name: &str) -> Result<(TestControlServer, TestControl), TestControlError> {
    let (calls, receiver) = mpsc::channel(QUEUE);
    let server = serve(name, calls)?;
    Ok((server, TestControl::new(receiver)))
}

/// The pipe named `name`, handing each request to `calls`.
fn serve(name: &str, calls: mpsc::Sender<TestCall>) -> Result<TestControlServer, TestControlError> {
    TestControlServer::start(name, move |request| {
        ask(&calls, request, ANSWER_TIMEOUT, ANSWER_GRACE)
    })
}

/// Hands `request` to the window, which must take it within `timeout`, and
/// waits `grace` longer for its reply. Runs on the pipe's thread, never on
/// the window's.
fn ask(
    calls: &mpsc::Sender<TestCall>,
    request: Request,
    timeout: Duration,
    grace: Duration,
) -> Reply {
    let (reply, answer) = std::sync::mpsc::channel();
    let deadline = Instant::now() + timeout;
    if calls
        .try_send(TestCall {
            request,
            reply,
            deadline,
        })
        .is_err()
    {
        return Reply::error("the window is not taking requests");
    }
    answer
        .recv_timeout(timeout + grace)
        .unwrap_or_else(|e| match e {
            RecvTimeoutError::Timeout => Reply::error("the window did not answer in time"),
            RecvTimeoutError::Disconnected => Reply::error("the window is gone"),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn adr_21_test_control_is_off_without_the_flag() {
        assert_eq!(requested(args(&[])), Ok(None));
        assert_eq!(requested(args(&["--other", "x"])), Ok(None));
    }

    #[test]
    fn adr_21_the_flag_takes_the_next_argument_as_its_name() {
        assert_eq!(
            requested(args(&["--test-control", "run-1"])),
            Ok(Some("run-1".to_owned()))
        );
        assert_eq!(
            requested(args(&["x", "--test-control", "a", "y"])),
            Ok(Some("a".to_owned()))
        );
        assert_eq!(
            requested(args(&["--test-control"])),
            Err(ArgError::MissingName)
        );
        assert_eq!(
            requested(args(&["--test-control", "--verbose"])),
            Err(ArgError::MissingName)
        );
        assert_eq!(
            requested(args(&["--test-control", "a", "--test-control", "b"])),
            Err(ArgError::Repeated)
        );
    }

    #[test]
    fn adr_21_a_request_gets_the_window_reply() {
        let (calls, mut receiver) = mpsc::channel::<TestCall>(QUEUE);
        let window = std::thread::spawn(move || {
            let call = receiver.blocking_recv().unwrap();
            assert_eq!(call.request, Request::State { text: true });
            let left = call.deadline.saturating_duration_since(Instant::now());
            assert!(left > Duration::ZERO && left <= ANSWER_TIMEOUT);
            call.reply.send(Reply::key(true)).unwrap();
        });
        let reply = ask(
            &calls,
            Request::State { text: true },
            ANSWER_TIMEOUT,
            ANSWER_GRACE,
        );
        window.join().unwrap();
        assert_eq!(reply, Reply::key(true));
    }

    #[test]
    fn a_window_that_is_gone_or_silent_is_reported_not_waited_on() {
        let (calls, receiver) = mpsc::channel::<TestCall>(QUEUE);
        drop(receiver);
        let reply = ask(
            &calls,
            Request::State { text: false },
            ANSWER_TIMEOUT,
            ANSWER_GRACE,
        );
        assert!(!reply.ok);

        let (calls, mut receiver) = mpsc::channel::<TestCall>(QUEUE);
        let silent = std::thread::spawn(move || {
            let call = receiver.blocking_recv().unwrap();
            std::thread::sleep(Duration::from_millis(200));
            drop(call);
        });
        let reply = ask(
            &calls,
            Request::State { text: false },
            Duration::from_millis(20),
            Duration::from_millis(10),
        );
        assert_eq!(
            reply.error.as_deref(),
            Some("the window did not answer in time")
        );
        silent.join().unwrap();

        let (calls, mut receiver) = mpsc::channel::<TestCall>(QUEUE);
        let dropped = std::thread::spawn(move || drop(receiver.blocking_recv().unwrap()));
        let reply = ask(
            &calls,
            Request::State { text: false },
            ANSWER_TIMEOUT,
            ANSWER_GRACE,
        );
        assert_eq!(reply.error.as_deref(), Some("the window is gone"));
        dropped.join().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn adr_21_args_on_a_command_reach_the_window_untouched() {
        // The window, not the pipe, decides that `tab.new` takes no args: a
        // refusal only works when the args are still on the request.
        use std::io::{BufRead, BufReader, Write};

        let name = format!("arg-{}-{:x}", std::process::id(), std::process::id());
        let (calls, mut receiver) = mpsc::channel::<TestCall>(QUEUE);
        let server = serve(&name, calls).unwrap();
        let path = server.path().to_owned();
        let window = std::thread::spawn(move || {
            let call = receiver.blocking_recv().unwrap();
            let seen = call.request.clone();
            call.reply.send(Reply::done()).unwrap();
            seen
        });
        let mut pipe = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        pipe.write_all(br#"{"op":"command","id":"tab.new","args":{"pane":3}}"#)
            .unwrap();
        pipe.write_all(b"\n").unwrap();
        let mut line = String::new();
        BufReader::new(&mut pipe).read_line(&mut line).unwrap();
        let request = window.join().unwrap();
        assert_eq!(
            request,
            Request::Command {
                id: "tab.new".into(),
                args: yien_proto::test_control::CommandArgs {
                    pane: Some(yien_proto::PaneId::new(3)),
                    ..Default::default()
                },
            }
        );
    }

    #[cfg(windows)]
    #[test]
    fn adr_21_a_line_on_the_pipe_reaches_the_window_and_its_reply_comes_back() {
        use std::io::{BufRead, BufReader, Write};

        let name = format!("bin-{}-{:x}", std::process::id(), std::process::id());
        let (calls, mut receiver) = mpsc::channel::<TestCall>(QUEUE);
        let server = serve(&name, calls).unwrap();
        let path = server.path().to_owned();
        let window = std::thread::spawn(move || {
            let call = receiver.blocking_recv().unwrap();
            call.reply.send(Reply::key(false)).unwrap();
        });
        let mut pipe = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        pipe.write_all(b"{\"op\":\"key\",\"key\":\"enter\"}\n")
            .unwrap();
        let mut line = String::new();
        BufReader::new(&mut pipe).read_line(&mut line).unwrap();
        window.join().unwrap();
        let reply: Reply = serde_json::from_str(&line).unwrap();
        assert_eq!(reply, Reply::key(false));
    }
}
