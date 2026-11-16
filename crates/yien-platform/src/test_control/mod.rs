//! The test control pipe (ADR-21): a local pipe through which a script
//! drives and inspects a YIEN started with `--test-control <name>`. Off
//! unless that flag is given; nothing listens otherwise.
//!
//! Each line the client writes is one JSON [`Request`]; each gets one JSON
//! [`Reply`] line back. The pipe admits only the current user and local
//! clients, and only one client at a time (RNF-11). Requests and replies are
//! never logged; only their `op`, size and timing are.
//! Only Windows has a transport today; elsewhere [`TestControlServer::start`]
//! returns [`TestControlError::Unsupported`].
//!
//! ```no_run
//! use yien_platform::test_control::TestControlServer;
//! use yien_proto::test_control::Reply;
//!
//! let server = TestControlServer::start("run-1f2e", |_request| Reply::done())?;
//! println!("listening on {}", server.path());
//! # Ok::<(), yien_platform::test_control::TestControlError>(())
//! ```

use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use yien_proto::test_control::{MAX_REQUEST_LEN, Reply, Request};

#[cfg(not(windows))]
mod unsupported;
#[cfg(not(windows))]
use unsupported as imp;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

/// Every test control pipe is this followed by the caller's name.
///
/// ```
/// assert!(yien_platform::test_control::PIPE_PREFIX.ends_with("yien-test-control-"));
/// ```
pub const PIPE_PREFIX: &str = r"\\.\pipe\yien-test-control-";

/// The longest name `--test-control` accepts.
///
/// ```
/// assert_eq!(yien_platform::test_control::MAX_NAME_LEN, 64);
/// ```
pub const MAX_NAME_LEN: usize = 64;

/// How long a connected client may stay silent before it is dropped, so a
/// stuck client cannot hold the only instance for ever.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// How long one reply may wait for the client to read it.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// Why the test control pipe did not start.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TestControlError {
    /// The name is empty, too long, or has a character other than ASCII
    /// letters, digits, `-` and `_`.
    #[error("the test control name must be 1 to {MAX_NAME_LEN} ASCII letters, digits, '-' or '_'")]
    BadName,
    /// An OS call failed; `ERROR_ACCESS_DENIED` (5) here usually means the
    /// name is taken.
    #[error("{call} failed with OS error {code}")]
    Os {
        /// The failing call.
        call: &'static str,
        /// The OS error code.
        code: i32,
    },
    /// This platform has no test control transport yet.
    #[error("test control is not available on this platform yet")]
    Unsupported,
}

/// The pipe path for `name`, after checking it. The name is the caller's
/// choice, normally a random nonce the launcher makes; only letters, digits,
/// `-` and `_` are accepted, so it cannot climb out of the pipe namespace.
///
/// ```
/// use yien_platform::test_control::pipe_path;
///
/// assert_eq!(pipe_path("run-42").unwrap(), r"\\.\pipe\yien-test-control-run-42");
/// assert!(pipe_path(r"..\..\C:\x").is_err());
/// ```
pub fn pipe_path(name: &str) -> Result<String, TestControlError> {
    let fits = !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if fits {
        Ok(format!("{PIPE_PREFIX}{name}"))
    } else {
        Err(TestControlError::BadName)
    }
}

/// What answers each request. It runs on the pipe's own thread, one request
/// at a time, and may block until the window answers.
type Handler = Box<dyn Fn(Request) -> Reply + Send>;

/// The test control pipe of one YIEN. Dropping it stops listening and waits
/// for the request in flight.
pub struct TestControlServer {
    path: String,
    _transport: imp::Server,
}

impl TestControlServer {
    /// Creates the pipe for `name` and starts serving. Fails when the name
    /// is not valid or another process holds it. `handler` answers each
    /// parsed request on the pipe's thread.
    ///
    /// ```no_run
    /// let server = yien_platform::test_control::TestControlServer::start("t1", |_| {
    ///     yien_proto::test_control::Reply::error("not ready")
    /// })?;
    /// assert!(server.path().ends_with("t1"));
    /// # Ok::<(), yien_platform::test_control::TestControlError>(())
    /// ```
    pub fn start(
        name: &str,
        handler: impl Fn(Request) -> Reply + Send + 'static,
    ) -> Result<Self, TestControlError> {
        let path = pipe_path(name)?;
        let transport = imp::Server::start(&path, Box::new(handler))?;
        tracing::info!("test control listening");
        Ok(Self {
            path,
            _transport: transport,
        })
    }

    /// The pipe's full path, `\\.\pipe\yien-test-control-<name>`.
    ///
    /// ```no_run
    /// # let server = yien_platform::test_control::TestControlServer::start("t2", |_| {
    /// #     yien_proto::test_control::Reply::done()
    /// # })?;
    /// assert!(server.path().starts_with(yien_platform::test_control::PIPE_PREFIX));
    /// # Ok::<(), yien_platform::test_control::TestControlError>(())
    /// ```
    pub fn path(&self) -> &str {
        &self.path
    }
}

impl std::fmt::Debug for TestControlServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestControlServer")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// Why a connection ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) enum Ended {
    /// The client closed its end.
    Closed,
    /// A line was longer than [`MAX_REQUEST_LEN`]; it was answered with an
    /// error and the connection closed, since the rest of it cannot be told
    /// from the next request.
    TooLong,
    /// Reading or writing failed or timed out.
    Io(io::ErrorKind),
}

/// Serves one client: answers each line in order until it closes, stalls or
/// sends a line too long.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn serve_connection<C: Read + Write>(
    conn: &mut C,
    handler: &dyn Fn(Request) -> Reply,
) -> Ended {
    let mut lines = LineReader::default();
    loop {
        let line = match lines.next(conn) {
            Ok(Some(line)) => line,
            Ok(None) => return Ended::Closed,
            Err(LineError::TooLong) => {
                let reply = encode(&Reply::error(format!(
                    "a request line is longer than {MAX_REQUEST_LEN} bytes"
                )));
                let _ = conn.write_all(&reply);
                return Ended::TooLong;
            }
            Err(LineError::Io(kind)) => return Ended::Io(kind),
        };
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let reply = answer(&line, handler);
        if let Err(e) = conn.write_all(&reply) {
            return Ended::Io(e.kind());
        }
    }
}

/// The reply line, newline included, for one request line.
pub(crate) fn answer(line: &[u8], handler: &dyn Fn(Request) -> Reply) -> Vec<u8> {
    let started = Instant::now();
    let request = match parse(line) {
        Ok(request) => request,
        Err(message) => {
            tracing::debug!(bytes = line.len(), "test control request refused");
            return encode(&Reply::error(message));
        }
    };
    let op = request.op();
    let reply = handler(request);
    let encoded = encode(&reply);
    tracing::debug!(
        op,
        ok = reply.ok,
        bytes = encoded.len(),
        micros = started.elapsed().as_micros() as u64,
        "test control request answered"
    );
    encoded
}

/// Parses and checks one request line.
fn parse(line: &[u8]) -> Result<Request, String> {
    if line.len() > MAX_REQUEST_LEN {
        return Err(format!("a request is longer than {MAX_REQUEST_LEN} bytes"));
    }
    let request: Request =
        serde_json::from_slice(line).map_err(|e| format!("not a valid request: {e}"))?;
    request.check()?;
    Ok(request)
}

/// `reply` as one JSON line.
fn encode(reply: &Reply) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(reply)
        .unwrap_or_else(|_| br#"{"ok":false,"error":"the reply could not be encoded"}"#.to_vec());
    bytes.push(b'\n');
    bytes
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineError {
    TooLong,
    Io(io::ErrorKind),
}

/// Splits what a client writes into lines, holding at most one line and one
/// read's worth of bytes. Bytes after a newline wait for the next call, so a
/// client may send several requests at once.
#[derive(Debug, Default)]
struct LineReader {
    buf: Vec<u8>,
}

impl LineReader {
    const CHUNK: usize = 4096;

    /// The next line without its `\n` or `\r\n`; `None` when the client
    /// closed. A last line without a newline still counts.
    fn next<R: Read>(&mut self, conn: &mut R) -> Result<Option<Vec<u8>>, LineError> {
        let mut scanned = 0;
        loop {
            if let Some(at) = self.buf[scanned..].iter().position(|&b| b == b'\n') {
                let end = scanned + at;
                if end > MAX_REQUEST_LEN + 1 {
                    return Err(LineError::TooLong);
                }
                let mut line: Vec<u8> = self.buf.drain(..=end).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(Some(line));
            }
            scanned = self.buf.len();
            if scanned > MAX_REQUEST_LEN + 1 {
                return Err(LineError::TooLong);
            }
            let mut chunk = [0u8; Self::CHUNK];
            let n = match conn.read(&mut chunk) {
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(LineError::Io(e.kind())),
            };
            if n == 0 {
                if self.buf.is_empty() {
                    return Ok(None);
                }
                if self.buf.len() > MAX_REQUEST_LEN {
                    return Err(LineError::TooLong);
                }
                return Ok(Some(std::mem::take(&mut self.buf)));
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use yien_proto::PaneId;
    use yien_proto::test_control::{MAX_TEXT_LEN, TypedText};

    /// A client: what it sends, and what the server wrote back.
    struct Client {
        input: io::Cursor<Vec<u8>>,
        output: Vec<u8>,
    }

    impl Client {
        fn new(input: impl Into<Vec<u8>>) -> Self {
            Self {
                input: io::Cursor::new(input.into()),
                output: Vec::new(),
            }
        }

        fn replies(&self) -> Vec<Reply> {
            String::from_utf8(self.output.clone())
                .unwrap()
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect()
        }
    }

    impl Read for Client {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.input.read(buf)
        }
    }

    impl Write for Client {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.output.write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn serve(input: impl Into<Vec<u8>>) -> (Ended, Vec<Reply>, Vec<Request>) {
        let seen = RefCell::new(Vec::new());
        let mut client = Client::new(input);
        let ended = serve_connection(&mut client, &|request| {
            seen.borrow_mut().push(request);
            Reply::done()
        });
        (ended, client.replies(), seen.into_inner())
    }

    #[test]
    fn adr_21_each_line_gets_one_reply_in_order() {
        let (ended, replies, seen) = serve(
            "{\"op\":\"state\"}\r\n\n  \n{\"op\":\"key\",\"pane\":4,\"key\":\"ctrl-c\"}\n{\"op\":\"state\",\"text\":true}",
        );
        assert_eq!(ended, Ended::Closed);
        assert_eq!(replies, vec![Reply::done(); 3]);
        assert_eq!(
            seen,
            vec![
                Request::State { text: false },
                Request::Key {
                    pane: Some(PaneId::new(4)),
                    key: "ctrl-c".into()
                },
                Request::State { text: true },
            ]
        );
    }

    #[test]
    fn adr_21_hostile_lines_are_answered_with_errors_not_panics() {
        let mut input = Vec::new();
        for line in [
            &b"not json"[..],
            b"\xff\xfe\x00",
            b"{\"op\":\"text\",\"text\":\"a\\u0003\"}",
            b"{\"op\":\"resize\",\"width\":1e40,\"height\":1}",
            b"{\"op\":\"state\",\"extra\":1}",
            b"{\"op\":\"command\",\"id\":\"\"}",
            &[b'['; 10_000],
            b"{\"op\":\"state\"}",
        ] {
            input.extend_from_slice(line);
            input.push(b'\n');
        }
        let (ended, replies, seen) = serve(input);
        assert_eq!(ended, Ended::Closed);
        assert_eq!(replies.len(), 8);
        assert!(replies[..7].iter().all(|r| !r.ok && r.error.is_some()));
        assert_eq!(replies[7], Reply::done());
        assert_eq!(seen, vec![Request::State { text: false }]);
    }

    #[test]
    fn adr_21_a_line_over_the_cap_is_refused_and_ends_the_connection() {
        let mut input = vec![b'a'; MAX_REQUEST_LEN + 10];
        input.extend_from_slice(b"\n{\"op\":\"state\"}\n");
        let (ended, replies, seen) = serve(input);
        assert_eq!(ended, Ended::TooLong);
        assert_eq!(replies.len(), 1);
        assert!(!replies[0].ok);
        assert!(seen.is_empty());

        let endless = vec![b' '; 3 * MAX_REQUEST_LEN];
        let (ended, _, seen) = serve(endless);
        assert_eq!(ended, Ended::TooLong);
        assert!(seen.is_empty());
    }

    #[test]
    fn a_line_at_the_cap_is_read() {
        let text = "a".repeat(MAX_TEXT_LEN);
        let line = serde_json::to_vec(&Request::Text {
            pane: None,
            text: TypedText::new(text),
        })
        .unwrap();
        let mut input = line.clone();
        input.extend(std::iter::repeat_n(b' ', MAX_REQUEST_LEN - line.len()));
        input.push(b'\n');
        let (ended, replies, seen) = serve(input);
        assert_eq!((ended, replies.len(), seen.len()), (Ended::Closed, 1, 1));
    }

    #[test]
    fn a_failing_write_ends_the_connection() {
        struct Deaf(io::Cursor<Vec<u8>>);
        impl Read for Deaf {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                self.0.read(buf)
            }
        }
        impl Write for Deaf {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut conn = Deaf(io::Cursor::new(
            b"{\"op\":\"state\"}\n{\"op\":\"state\"}\n".to_vec(),
        ));
        let calls = RefCell::new(0);
        let ended = serve_connection(&mut conn, &|_| {
            *calls.borrow_mut() += 1;
            Reply::done()
        });
        assert_eq!(ended, Ended::Io(io::ErrorKind::BrokenPipe));
        assert_eq!(*calls.borrow(), 1);
    }

    #[test]
    fn a_stalled_read_ends_the_connection() {
        struct Stalled;
        impl Read for Stalled {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::ErrorKind::TimedOut.into())
            }
        }
        impl Write for Stalled {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        assert_eq!(
            serve_connection(&mut Stalled, &|_| Reply::done()),
            Ended::Io(io::ErrorKind::TimedOut)
        );
    }

    #[test]
    fn rnf_11_names_are_limited_to_a_safe_alphabet() {
        assert!(pipe_path("a").is_ok());
        assert!(pipe_path(&"x".repeat(MAX_NAME_LEN)).is_ok());
        for bad in [
            "",
            "a/b",
            r"a\b",
            "..",
            "a b",
            "a:b",
            "é",
            "a\0",
            &"x".repeat(MAX_NAME_LEN + 1),
        ] {
            assert_eq!(pipe_path(bad), Err(TestControlError::BadName), "{bad:?}");
        }
    }
}
