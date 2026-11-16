//! A small HTTP GET for the update check and download, and a streaming GET
//! for a local service's events: HTTPS only (plain HTTP to `127.0.0.1` when
//! asked), the system proxy and certificate store, timeouts on every step
//! and a cap on the body. On Windows it runs on WinHTTP; elsewhere it is not
//! available yet.

use std::fmt;
use std::io::{self, Write};
use std::ops::ControlFlow;
use std::time::Duration;

#[cfg(windows)]
mod windows;

/// How one request is made. Its `Debug` leaves header values out, as they
/// can carry credentials.
#[derive(Clone, PartialEq, Eq)]
pub struct Options {
    /// Sent as `User-Agent`; nothing else identifies the user or the machine.
    pub user_agent: String,
    /// Extra request headers, as name and value.
    pub headers: Vec<(String, String)>,
    /// The most body bytes taken; a longer body fails the request.
    pub limit: u64,
    /// Name resolution, connecting, sending and each wait for data.
    pub timeout: Duration,
    /// Whether plain `http` to `127.0.0.1` is allowed: for a service on this
    /// machine, and for tests against a local server. Every other URL must
    /// be `https`.
    pub allow_loopback_http: bool,
}

impl fmt::Debug for Options {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.headers.iter().map(|(name, _)| name.as_str()).collect();
        f.debug_struct("Options")
            .field("user_agent", &self.user_agent)
            .field("headers", &names)
            .field("limit", &self.limit)
            .field("timeout", &self.timeout)
            .field("allow_loopback_http", &self.allow_loopback_http)
            .finish()
    }
}

impl Options {
    /// HTTPS only, the given `User-Agent` and body cap, 30 s timeouts.
    ///
    /// ```
    /// let options = yien_platform::http::Options::new("YIEN/0.1.0", 1024);
    /// assert!(!options.allow_loopback_http);
    /// ```
    pub fn new(user_agent: impl Into<String>, limit: u64) -> Self {
        Self {
            user_agent: user_agent.into(),
            headers: Vec::new(),
            limit,
            timeout: Duration::from_secs(30),
            allow_loopback_http: false,
        }
    }

    /// Adds a request header.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

/// Why a request failed.
#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    /// The URL is not one this client accepts.
    #[error("not an accepted URL: {0}")]
    BadUrl(String),
    /// A redirect left HTTPS.
    #[error("the server redirected away from HTTPS")]
    Insecure,
    /// The server answered with a status other than 2xx.
    #[error("the server answered {0}")]
    Status(u32),
    /// The body is longer than the cap.
    #[error("the answer is longer than {0} bytes")]
    TooLarge(u64),
    /// Writing the body failed.
    #[error("writing the answer failed: {0}")]
    Write(#[from] io::Error),
    /// The OS call failed, with its error code.
    #[error("{call} failed with error {code}")]
    Os {
        /// The failing call.
        call: &'static str,
        /// `GetLastError` after it.
        code: u32,
    },
    /// No HTTP client on this OS yet.
    #[error("HTTP is not available on this system")]
    Unsupported,
}

/// A URL split into what a connection needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// `https`, or `http` to the loopback address when allowed.
    pub secure: bool,
    /// The host name, lower case.
    pub host: String,
    /// The port, 443 or 80 by default.
    pub port: u16,
    /// The path and query, starting with `/`.
    pub path: String,
}

impl Target {
    /// Splits `url`. Only `https://host[:port]/path` is accepted, plus
    /// `http://127.0.0.1[:port]/path` when `allow_loopback_http`. User info,
    /// fragments, spaces and control characters are refused.
    ///
    /// ```
    /// use yien_platform::http::Target;
    ///
    /// let t = Target::parse("https://api.github.com/repos/a/b/releases", false).unwrap();
    /// assert_eq!((t.host.as_str(), t.port, t.path.as_str()), ("api.github.com", 443, "/repos/a/b/releases"));
    /// assert!(Target::parse("http://api.github.com/", false).is_err());
    /// ```
    pub fn parse(url: &str, allow_loopback_http: bool) -> Result<Self, HttpError> {
        let bad = || HttpError::BadUrl(url.to_owned());
        if url.chars().any(|c| c.is_control() || c == ' ' || c == '#') {
            return Err(bad());
        }
        let (secure, rest) = if let Some(rest) = url.strip_prefix("https://") {
            (true, rest)
        } else if let Some(rest) = url.strip_prefix("http://") {
            (false, rest)
        } else {
            return Err(bad());
        };
        let (authority, path) = match rest.find(['/', '?']) {
            Some(at) if rest[at..].starts_with('/') => (&rest[..at], rest[at..].to_owned()),
            Some(at) => (&rest[..at], format!("/{}", &rest[at..])),
            None => (rest, "/".to_owned()),
        };
        if authority.contains('@') || authority.is_empty() {
            return Err(bad());
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (host, port.parse::<u16>().map_err(|_| bad())?),
            None => (authority, if secure { 443 } else { 80 }),
        };
        let host = host.to_ascii_lowercase();
        let host_ok = !host.is_empty()
            && host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
        if !host_ok || port == 0 {
            return Err(bad());
        }
        if !secure && !(allow_loopback_http && host == "127.0.0.1") {
            return Err(bad());
        }
        Ok(Self {
            secure,
            host,
            port,
            path,
        })
    }
}

/// Fetches `url` into `sink`; returns the number of body bytes written.
/// Redirects are followed, but never from HTTPS to HTTP.
///
/// ```no_run
/// use yien_platform::http::{Options, get};
///
/// let mut body = Vec::new();
/// get("https://example.com/", &Options::new("YIEN/0.1.0", 1 << 20), &mut body)?;
/// # Ok::<(), yien_platform::http::HttpError>(())
/// ```
pub fn get(url: &str, options: &Options, sink: &mut dyn Write) -> Result<u64, HttpError> {
    let target = Target::parse(url, options.allow_loopback_http)?;
    get_target(&target, options, sink)
}

#[cfg(windows)]
fn get_target(target: &Target, options: &Options, sink: &mut dyn Write) -> Result<u64, HttpError> {
    windows::get(target, options, sink)
}

#[cfg(not(windows))]
fn get_target(_: &Target, _: &Options, _: &mut dyn Write) -> Result<u64, HttpError> {
    Err(HttpError::Unsupported)
}

/// Opens `url` and hands each piece of the body to `on_chunk` as it
/// arrives, for a stream that stays open such as server-sent events. Returns
/// when the body ends or `on_chunk` breaks; a wait for data longer than
/// `options.timeout` fails it. `options.limit` does not apply: the caller
/// keeps what it needs. Redirects are not followed, so headers such as
/// credentials go to `url` only.
///
/// ```no_run
/// use std::ops::ControlFlow;
/// use yien_platform::http::{Options, get_stream};
///
/// let options = Options { allow_loopback_http: true, ..Options::new("YIEN/0.1.0", 0) };
/// get_stream("http://127.0.0.1:28894/api/event", &options, &mut |chunk| {
///     println!("{} bytes", chunk.len());
///     ControlFlow::Continue(())
/// })?;
/// # Ok::<(), yien_platform::http::HttpError>(())
/// ```
pub fn get_stream(
    url: &str,
    options: &Options,
    on_chunk: &mut dyn FnMut(&[u8]) -> ControlFlow<()>,
) -> Result<(), HttpError> {
    let target = Target::parse(url, options.allow_loopback_http)?;
    stream_target(&target, options, on_chunk)
}

#[cfg(windows)]
fn stream_target(
    target: &Target,
    options: &Options,
    on_chunk: &mut dyn FnMut(&[u8]) -> ControlFlow<()>,
) -> Result<(), HttpError> {
    windows::get_stream(target, options, on_chunk)
}

#[cfg(not(windows))]
fn stream_target(
    _: &Target,
    _: &Options,
    _: &mut dyn FnMut(&[u8]) -> ControlFlow<()>,
) -> Result<(), HttpError> {
    Err(HttpError::Unsupported)
}

/// Whether a final URL, after redirects, is still acceptable.
#[cfg_attr(not(windows), allow(dead_code))]
fn final_url_ok(url: &str, options: &Options) -> bool {
    Target::parse(url, options.allow_loopback_http).is_ok()
}

/// Copies one chunk into `sink`, counting against the cap.
#[cfg_attr(not(windows), allow(dead_code))]
fn take_chunk(
    chunk: &[u8],
    written: &mut u64,
    limit: u64,
    sink: &mut dyn Write,
) -> Result<(), HttpError> {
    *written = written.saturating_add(chunk.len() as u64);
    if *written > limit {
        return Err(HttpError::TooLarge(limit));
    }
    sink.write_all(chunk)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_urls_are_accepted() {
        let t = Target::parse("https://GitHub.com:8443/a/b?x=1", false).unwrap();
        assert_eq!(
            t,
            Target {
                secure: true,
                host: "github.com".into(),
                port: 8443,
                path: "/a/b?x=1".into()
            }
        );
        assert_eq!(
            Target::parse("https://github.com", false).unwrap().path,
            "/"
        );
        assert_eq!(
            Target::parse("https://github.com?x", false).unwrap().path,
            "/?x"
        );
        for bad in [
            "http://github.com/",
            "ftp://github.com/",
            "file:///C:/Windows/System32/calc.exe",
            "https://user@github.com/",
            "https:///path",
            "https://git hub.com/",
            "https://github.com/a#frag",
            "https://github.com:0/",
            "https://github.com:99999/",
            "https://github.com/\r\nX: y",
            "https://[::1]/",
            "github.com/a",
        ] {
            assert!(Target::parse(bad, false).is_err(), "{bad}");
        }
    }

    #[test]
    fn plain_http_only_to_loopback_and_only_when_allowed() {
        assert!(Target::parse("http://127.0.0.1:8080/x", false).is_err());
        let t = Target::parse("http://127.0.0.1:8080/x", true).unwrap();
        assert!(!t.secure);
        assert!(Target::parse("http://localhost:8080/x", true).is_err());
        assert!(Target::parse("http://example.com/x", true).is_err());
    }

    #[test]
    fn a_redirect_that_leaves_https_is_refused() {
        let options = Options::new("YIEN/test", 10);
        assert!(final_url_ok(
            "https://objects.githubusercontent.com/x",
            &options
        ));
        assert!(!final_url_ok(
            "http://objects.githubusercontent.com/x",
            &options
        ));
    }

    #[test]
    fn the_body_cap_is_enforced() {
        let mut sink = Vec::new();
        let mut written = 0;
        take_chunk(b"12345", &mut written, 8, &mut sink).unwrap();
        assert!(matches!(
            take_chunk(b"6789", &mut written, 8, &mut sink),
            Err(HttpError::TooLarge(8))
        ));
        assert_eq!(sink, b"12345");
    }

    /// A local server answering each connection with the next canned
    /// response; returns its base URL and the requests it saw.
    #[cfg(windows)]
    fn serve(responses: Vec<Vec<u8>>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        use std::io::Read;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let server = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut byte = [0u8; 1];
                while !request.ends_with(b"\r\n\r\n") {
                    if stream.read(&mut byte).unwrap() == 0 {
                        break;
                    }
                    request.push(byte[0]);
                }
                seen.push(String::from_utf8_lossy(&request).into_owned());
                stream.write_all(&response).unwrap();
                // Close only after the client has: dropping the stream first
                // races WinHTTP's read of the answer (error 12030 or 12152).
                let _ = stream.shutdown(std::net::Shutdown::Write);
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                while matches!(stream.read(&mut byte), Ok(n) if n > 0) {}
            }
            seen
        });
        (base, server)
    }

    #[cfg(windows)]
    fn response(status: &str, body: &[u8]) -> Vec<u8> {
        let mut out = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        out.extend_from_slice(body);
        out
    }

    #[cfg(windows)]
    fn local(limit: u64) -> Options {
        Options {
            allow_loopback_http: true,
            timeout: std::time::Duration::from_secs(5),
            ..Options::new("YIEN/9.9.9", limit)
        }
        .header("Accept", "application/vnd.github+json")
    }

    #[cfg(windows)]
    #[test]
    fn a_local_server_answer_is_read_with_only_the_generic_agent() {
        let (base, server) = serve(vec![response("200 OK", b"hello")]);
        let mut body = Vec::new();
        let n = get(
            &format!("{base}/releases?per_page=5"),
            &local(100),
            &mut body,
        )
        .unwrap();
        assert_eq!((n, body.as_slice()), (5, &b"hello"[..]));
        let seen = server.join().unwrap();
        let request = seen[0].to_ascii_lowercase();
        assert!(
            request.starts_with("get /releases?per_page=5 http/1.1\r\n"),
            "{request}"
        );
        assert!(request.contains("user-agent: yien/9.9.9\r\n"), "{request}");
        assert!(
            request.contains("accept: application/vnd.github+json\r\n"),
            "{request}"
        );
        assert!(!request.contains("cookie"), "{request}");
        assert!(!request.contains("authorization"), "{request}");
    }

    #[cfg(windows)]
    #[test]
    fn an_error_status_and_a_long_body_fail() {
        let (base, server) = serve(vec![
            response("404 Not Found", b"nope"),
            response("200 OK", &[7u8; 300]),
        ]);
        let mut body = Vec::new();
        assert!(matches!(
            get(&format!("{base}/a"), &local(100), &mut body),
            Err(HttpError::Status(404))
        ));
        assert!(matches!(
            get(&format!("{base}/b"), &local(100), &mut body),
            Err(HttpError::TooLarge(100))
        ));
        server.join().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn a_redirect_is_followed() {
        let (base, server) = serve(vec![
            // Without `Connection: close`, WinHTTP may send `GET /final` on
            // this connection, which `serve` never reads.
            b"HTTP/1.1 302 Found\r\nLocation: /final\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .to_vec(),
            response("200 OK", b"moved"),
        ]);
        let mut body = Vec::new();
        get(&format!("{base}/start"), &local(100), &mut body).unwrap();
        assert_eq!(body, b"moved");
        let seen = server.join().unwrap();
        assert!(seen[1].starts_with("GET /final "), "{}", seen[1]);
    }

    #[cfg(windows)]
    #[test]
    fn a_silent_server_times_out() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://127.0.0.1:{}/",
            listener.local_addr().unwrap().port()
        );
        let options = Options {
            timeout: std::time::Duration::from_millis(300),
            ..local(10)
        };
        let started = std::time::Instant::now();
        let result = get(&url, &options, &mut Vec::new());
        assert!(matches!(result, Err(HttpError::Os { .. })), "{result:?}");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        drop(listener);
    }

    #[test]
    fn options_debug_leaves_header_values_out() {
        let options = Options::new("YIEN/test", 1).header("Authorization", "Basic c2VjcmV0");
        let text = format!("{options:?}");
        assert!(text.contains("Authorization"), "{text}");
        assert!(!text.contains("c2VjcmV0"), "{text}");
    }

    /// A local server taking one connection: reads the request, then hands
    /// the stream to `answer`. Returns its base URL and the request.
    #[cfg(windows)]
    fn serve_one(
        answer: impl FnOnce(&mut std::net::TcpStream) + Send + 'static,
    ) -> (String, std::thread::JoinHandle<String>) {
        use std::io::Read;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut byte = [0u8; 1];
            while !request.ends_with(b"\r\n\r\n") {
                if stream.read(&mut byte).unwrap() == 0 {
                    break;
                }
                request.push(byte[0]);
            }
            answer(&mut stream);
            // Close only after the client has (see `serve`).
            let _ = stream.shutdown(std::net::Shutdown::Write);
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            while matches!(stream.read(&mut byte), Ok(n) if n > 0) {}
            String::from_utf8_lossy(&request).into_owned()
        });
        (base, server)
    }

    #[cfg(windows)]
    const STREAM_HEAD: &[u8] =
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";

    #[cfg(windows)]
    fn stream_options() -> Options {
        local(0).header("Authorization", "Basic b3BlbmNvZGU6cHc=")
    }

    #[cfg(windows)]
    #[test]
    fn a_stream_arrives_piece_by_piece_before_it_ends() {
        let (got_first, first) = std::sync::mpsc::channel::<()>();
        let (base, server) = serve_one(move |stream| {
            stream.write_all(STREAM_HEAD).unwrap();
            stream.write_all(b"data: one\n\n").unwrap();
            // The second piece goes only once the client has the first.
            first
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            stream.write_all(b"data: two\n\n").unwrap();
        });
        let mut body = Vec::new();
        get_stream(
            &format!("{base}/api/event"),
            &stream_options(),
            &mut |chunk| {
                if body.is_empty() {
                    got_first.send(()).unwrap();
                }
                body.extend_from_slice(chunk);
                ControlFlow::Continue(())
            },
        )
        .unwrap();
        assert_eq!(body, b"data: one\n\ndata: two\n\n");
        let request = server.join().unwrap().to_ascii_lowercase();
        assert!(
            request.starts_with("get /api/event http/1.1\r\n"),
            "{request}"
        );
        assert!(
            request.contains("authorization: basic b3blbmnvzgu6chc=\r\n"),
            "{request}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_chunked_stream_is_decoded() {
        let (base, server) = serve_one(|stream| {
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n\
                      6\r\ndata: \r\n5\r\nabc\n\n\r\n0\r\n\r\n",
                )
                .unwrap();
        });
        let mut body = Vec::new();
        get_stream(&format!("{base}/"), &stream_options(), &mut |chunk| {
            body.extend_from_slice(chunk);
            ControlFlow::Continue(())
        })
        .unwrap();
        assert_eq!(body, b"data: abc\n\n");
        server.join().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn a_stream_stops_when_the_reader_breaks() {
        let (base, server) = serve_one(|stream| {
            stream.write_all(STREAM_HEAD).unwrap();
            stream.write_all(b"data: one\n\n").unwrap();
            // Left open: only the client's break ends the read.
        });
        let mut calls = 0;
        get_stream(&format!("{base}/"), &stream_options(), &mut |_| {
            calls += 1;
            ControlFlow::Break(())
        })
        .unwrap();
        assert_eq!(calls, 1);
        server.join().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn a_refused_stream_fails_and_a_redirect_is_not_followed() {
        for answer in [
            &b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"[..],
            b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ] {
            let (base, server) = serve_one(move |stream| stream.write_all(answer).unwrap());
            let result = get_stream(&format!("{base}/"), &stream_options(), &mut |_| {
                ControlFlow::Continue(())
            });
            assert!(matches!(result, Err(HttpError::Status(401 | 302))), "{result:?}");
            server.join().unwrap();
        }
    }

    #[test]
    fn a_stream_is_refused_off_this_machine_without_https() {
        let options = stream_options_any();
        for url in ["http://example.com/", "http://localhost:1/", "ftp://x/"] {
            let result = get_stream(url, &options, &mut |_| ControlFlow::Continue(()));
            assert!(matches!(result, Err(HttpError::BadUrl(_))), "{url}");
        }
    }

    fn stream_options_any() -> Options {
        Options {
            allow_loopback_http: true,
            ..Options::new("YIEN/test", 0)
        }
    }
}
