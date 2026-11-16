//! OpenCode's background service as an outside reader sees it: its
//! registration file (`service.json`) and its event stream (`GET /api/event`,
//! server-sent events). Pure: the caller reads the file and the stream and
//! hands the bytes in.
//!
//! ```
//! use yien_agent::opencode::{ServiceEvent, ServiceFile, SseDecoder, parse_event};
//!
//! let file = br#"{"id":"x","version":"2.2.7","url":"http://127.0.0.1:28894","pid":1,"password":"pw"}"#;
//! let service = ServiceFile::parse(file).unwrap();
//! assert_eq!(service.events_url(), "http://127.0.0.1:28894/api/event");
//!
//! let mut decoder = SseDecoder::new();
//! let mut events = Vec::new();
//! decoder.feed(b"data: {\"type\":\"session.execution.failed\",", &mut events);
//! decoder.feed(b"\"data\":{\"sessionID\":\"ses_1\"}}\n\n", &mut events);
//! let failed = ServiceEvent::Failed { session: "ses_1".into() };
//! assert_eq!(parse_event(&events[0]), Some(failed));
//! ```

use std::fmt;

use serde::Deserialize;

/// The most bytes of `service.json` read; a larger file is refused.
pub const MAX_SERVICE_FILE: usize = 16 * 1024;

/// The longest line of the event stream kept, in bytes. A longer line is
/// dropped with the event it belongs to.
pub const MAX_SSE_LINE: usize = 64 * 1024;

/// The longest event kept (its `data` lines together), in bytes. A longer
/// event is dropped: only short ones, such as a failure, are read.
pub const MAX_SSE_EVENT: usize = 64 * 1024;

/// The longest password taken from `service.json`, in bytes.
const MAX_PASSWORD: usize = 1024;

/// The user name of the service's Basic authentication.
const SERVICE_USER: &str = "opencode";

/// The service's registration file: where it listens and its password.
/// Only plain HTTP to `127.0.0.1` is accepted, so the password never leaves
/// the machine. Its `Debug` leaves the password out.
#[derive(Clone, PartialEq, Eq)]
pub struct ServiceFile {
    /// `http://127.0.0.1:<port>`.
    base: String,
    password: String,
}

/// Why a `service.json` is not used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ServiceFileError {
    /// Larger than [`MAX_SERVICE_FILE`].
    #[error("the service file is larger than {MAX_SERVICE_FILE} bytes")]
    TooLarge,
    /// Not JSON with a `url` and a `password`.
    #[error("the service file is not what OpenCode writes")]
    Malformed,
    /// The URL's host is not `127.0.0.1`, `localhost` or `[::1]`.
    #[error("the service URL is not on this machine")]
    NotLoopback,
    /// A loopback URL other than `http://127.0.0.1:<port>`, which is the
    /// only one connected to.
    #[error("the service URL is not plain HTTP to 127.0.0.1")]
    UnsupportedUrl,
}

#[derive(Deserialize)]
struct RawServiceFile {
    url: String,
    password: String,
}

impl ServiceFile {
    /// Reads `service.json`.
    ///
    /// ```
    /// use yien_agent::opencode::{ServiceFile, ServiceFileError};
    ///
    /// let remote = br#"{"url":"http://10.0.0.2:28894","password":"pw"}"#;
    /// assert_eq!(ServiceFile::parse(remote), Err(ServiceFileError::NotLoopback));
    /// ```
    pub fn parse(bytes: &[u8]) -> Result<Self, ServiceFileError> {
        if bytes.len() > MAX_SERVICE_FILE {
            return Err(ServiceFileError::TooLarge);
        }
        let raw: RawServiceFile =
            serde_json::from_slice(bytes).map_err(|_| ServiceFileError::Malformed)?;
        if raw.password.is_empty() || raw.password.len() > MAX_PASSWORD {
            return Err(ServiceFileError::Malformed);
        }
        Ok(Self {
            base: loopback_base(&raw.url)?,
            password: raw.password,
        })
    }

    /// The URL of the event stream.
    pub fn events_url(&self) -> String {
        format!("{}/api/event", self.base)
    }

    /// The `Authorization` header value for the service.
    ///
    /// ```
    /// use yien_agent::opencode::ServiceFile;
    ///
    /// let service = ServiceFile::parse(br#"{"url":"http://127.0.0.1:1","password":"pw"}"#).unwrap();
    /// assert_eq!(service.authorization(), "Basic b3BlbmNvZGU6cHc=");
    /// ```
    pub fn authorization(&self) -> String {
        let credentials = format!("{SERVICE_USER}:{}", self.password);
        format!("Basic {}", base64(credentials.as_bytes()))
    }
}

impl fmt::Debug for ServiceFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceFile")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

/// `http://127.0.0.1:<port>` from the service's URL, which may end in `/`.
/// User info, a path, a query and a fragment are refused.
fn loopback_base(url: &str) -> Result<String, ServiceFileError> {
    let (scheme, rest) = url.split_once("://").ok_or(ServiceFileError::Malformed)?;
    let authority = rest.strip_suffix('/').unwrap_or(rest);
    if authority.contains(['/', '?', '#', '@', '\\']) || authority.chars().any(char::is_control) {
        return Err(ServiceFileError::NotLoopback);
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !port.contains(']') => (host, Some(port)),
        _ => (authority, None),
    };
    let host = host.to_ascii_lowercase();
    if !matches!(host.as_str(), "127.0.0.1" | "localhost" | "[::1]") {
        return Err(ServiceFileError::NotLoopback);
    }
    let port = port.and_then(|p| p.parse::<u16>().ok()).filter(|p| *p != 0);
    match (scheme.eq_ignore_ascii_case("http"), host.as_str(), port) {
        (true, "127.0.0.1", Some(port)) => Ok(format!("http://127.0.0.1:{port}")),
        _ => Err(ServiceFileError::UnsupportedUrl),
    }
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk.first().copied().unwrap_or(0),
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                let index = (n >> (18 - 6 * i)) & 0x3f;
                out.push(char::from(ALPHABET[index as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Splits a server-sent event stream into the `data` of each event, however
/// the bytes are cut. Lines end in CR LF, LF or CR; comments (the service's
/// `: heartbeat`) and the `event`, `id` and `retry` fields are skipped.
///
/// ```
/// use yien_agent::opencode::SseDecoder;
///
/// let mut decoder = SseDecoder::new();
/// let mut events = Vec::new();
/// decoder.feed(b": heartbeat\r\n\r\ndata: a\r\ndata: b\r\n\r\n", &mut events);
/// assert_eq!(events, vec!["a\nb".to_owned()]);
/// ```
#[derive(Debug, Default)]
pub struct SseDecoder {
    line: Vec<u8>,
    data: Vec<u8>,
    has_data: bool,
    /// The current line went past [`MAX_SSE_LINE`]; the rest of it is skipped.
    line_dropped: bool,
    /// The current event went past a cap; it is dropped at its end.
    event_dropped: bool,
    /// The last byte was a CR, so a LF right after it ends no other line.
    after_cr: bool,
}

impl SseDecoder {
    /// A decoder at the start of a stream.
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes the next bytes of the stream and appends the `data` of each
    /// event they complete to `out`. Events that are not UTF-8 are dropped.
    pub fn feed(&mut self, bytes: &[u8], out: &mut Vec<String>) {
        for &byte in bytes {
            let after_cr = std::mem::take(&mut self.after_cr);
            match byte {
                b'\n' if after_cr => {}
                b'\n' | b'\r' => {
                    self.after_cr = byte == b'\r';
                    self.end_line(out);
                }
                _ if self.line_dropped => {}
                _ if self.line.len() >= MAX_SSE_LINE => {
                    self.line.clear();
                    self.line_dropped = true;
                    self.event_dropped = true;
                }
                _ => self.line.push(byte),
            }
        }
    }

    fn end_line(&mut self, out: &mut Vec<String>) {
        let line = std::mem::take(&mut self.line);
        if std::mem::take(&mut self.line_dropped) {
            return;
        }
        if line.is_empty() {
            self.dispatch(out);
            return;
        }
        let (field, value) = match line.iter().position(|&b| b == b':') {
            Some(0) => return,
            Some(at) => {
                let value = &line[at + 1..];
                (&line[..at], value.strip_prefix(b" ").unwrap_or(value))
            }
            None => (&line[..], &[][..]),
        };
        if field != b"data" || self.event_dropped {
            return;
        }
        if self.data.len() + value.len() + 1 > MAX_SSE_EVENT {
            self.data.clear();
            self.event_dropped = true;
            return;
        }
        if self.has_data {
            self.data.push(b'\n');
        }
        self.data.extend_from_slice(value);
        self.has_data = true;
    }

    fn dispatch(&mut self, out: &mut Vec<String>) {
        let data = std::mem::take(&mut self.data);
        let complete = std::mem::take(&mut self.has_data) && !self.event_dropped;
        self.event_dropped = false;
        if complete && let Ok(text) = String::from_utf8(data) {
            out.push(text);
        }
    }
}

/// An event of the service that YIEN acts on. Everything else, including
/// what a failure says, is not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceEvent {
    /// `server.connected`: the stream is open.
    Connected,
    /// `session.execution.failed`: a turn of this session failed.
    Failed {
        /// The session's id, in the shape OpenCode also sends over OSC 7501.
        session: String,
    },
}

#[derive(Deserialize)]
struct RawEvent {
    #[serde(rename = "type")]
    kind: String,
    data: Option<RawEventData>,
}

#[derive(Deserialize)]
struct RawEventData {
    #[serde(rename = "sessionID")]
    session_id: Option<String>,
}

/// The event in one `data` payload, when it is one YIEN acts on.
///
/// ```
/// use yien_agent::opencode::{ServiceEvent, parse_event};
///
/// assert_eq!(parse_event(r#"{"type":"server.connected"}"#), Some(ServiceEvent::Connected));
/// assert_eq!(parse_event(r#"{"type":"session.execution.started","data":{"sessionID":"s"}}"#), None);
/// ```
pub fn parse_event(data: &str) -> Option<ServiceEvent> {
    // Most events are text deltas; only these two are decoded.
    if !data.contains("server.connected") && !data.contains("session.execution.failed") {
        return None;
    }
    let raw: RawEvent = serde_json::from_str(data).ok()?;
    match raw.kind.as_str() {
        "server.connected" => Some(ServiceEvent::Connected),
        "session.execution.failed" => {
            let session = raw.data?.session_id?;
            session_shaped(&session).then_some(ServiceEvent::Failed { session })
        }
        _ => None,
    }
}

/// The shape OpenCode gives the session ids it sends over OSC 7501: 1 to 32
/// ASCII letters, digits, `_`, `.`, `+` or `-`.
fn session_shaped(id: &str) -> bool {
    (1..=32).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'+' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(url: &str) -> Result<ServiceFile, ServiceFileError> {
        let file =
            format!(r#"{{"id":"svc","version":"2.2.7","url":"{url}","pid":42,"password":"pw"}}"#);
        ServiceFile::parse(file.as_bytes())
    }

    #[test]
    fn the_service_file_opencode_writes_is_read() {
        let s = service("http://127.0.0.1:28894").unwrap();
        assert_eq!(s.events_url(), "http://127.0.0.1:28894/api/event");
        assert_eq!(s.authorization(), "Basic b3BlbmNvZGU6cHc=");
        let s = service("http://127.0.0.1:28894/").unwrap();
        assert_eq!(s.events_url(), "http://127.0.0.1:28894/api/event");
        let s = service("HTTP://127.0.0.1:1").unwrap();
        assert_eq!(s.events_url(), "http://127.0.0.1:1/api/event");
    }

    #[test]
    fn rnf_11_a_url_off_this_machine_is_refused() {
        for url in [
            "http://10.0.0.2:28894",
            "http://example.com:28894",
            "http://127.0.0.2:28894",
            "http://127.0.0.1.example.com:28894",
            "http://127.0.0.1:28894@example.com",
            "http://user@127.0.0.1:28894",
            "http://example.com:28894#@127.0.0.1:1",
            "http://127.0.0.1:28894/api",
            "http://127.0.0.1:28894?x=1",
            // `\\` in JSON: the URL holds one backslash.
            "http://127.0.0.1\\\\@example.com:1",
            "http://:28894",
        ] {
            assert_eq!(service(url), Err(ServiceFileError::NotLoopback), "{url}");
        }
    }

    #[test]
    fn rnf_11_a_loopback_url_other_than_plain_http_to_127_0_0_1_is_not_used() {
        for url in [
            "https://127.0.0.1:28894",
            "ws://127.0.0.1:28894",
            "http://localhost:28894",
            "http://[::1]:28894",
            "http://127.0.0.1",
            "http://127.0.0.1:0",
            "http://127.0.0.1:99999",
            "http://127.0.0.1:x",
        ] {
            assert_eq!(service(url), Err(ServiceFileError::UnsupportedUrl), "{url}");
        }
    }

    #[test]
    fn a_missing_or_malformed_service_file_is_refused() {
        for file in [
            &b""[..],
            b"not json",
            b"[]",
            b"{}",
            br#"{"url":"http://127.0.0.1:1"}"#,
            br#"{"password":"pw"}"#,
            br#"{"url":"http://127.0.0.1:1","password":""}"#,
            br#"{"url":"http://127.0.0.1:1","password":7}"#,
            br#"{"url":"127.0.0.1:1","password":"pw"}"#,
            br#"{"url":"http://127.0.0.1:1","password":"pw""#,
            b"\xff\xfe",
        ] {
            assert_eq!(
                ServiceFile::parse(file),
                Err(ServiceFileError::Malformed),
                "{file:?}"
            );
        }
        let long = format!(
            r#"{{"url":"http://127.0.0.1:1","password":"{}"}}"#,
            "p".repeat(MAX_PASSWORD + 1)
        );
        assert_eq!(
            ServiceFile::parse(long.as_bytes()),
            Err(ServiceFileError::Malformed)
        );
        let huge = vec![b' '; MAX_SERVICE_FILE + 1];
        assert_eq!(ServiceFile::parse(&huge), Err(ServiceFileError::TooLarge));
    }

    #[test]
    fn the_password_stays_out_of_debug() {
        let file = br#"{"url":"http://127.0.0.1:1","password":"s3cr3t-Pa55"}"#;
        let s = ServiceFile::parse(file).unwrap();
        assert!(!format!("{s:?}").contains("s3cr3t"));
        assert_eq!(s.authorization(), "Basic b3BlbmNvZGU6czNjcjN0LVBhNTU=");
    }

    #[test]
    fn base64_pads_every_length() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"a"), "YQ==");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b"abc"), "YWJj");
        assert_eq!(base64(&[0xff, 0xfe, 0xfd, 0x00]), "//79AA==");
    }

    fn decode(chunks: &[&[u8]]) -> Vec<String> {
        let mut decoder = SseDecoder::new();
        let mut out = Vec::new();
        for chunk in chunks {
            decoder.feed(chunk, &mut out);
        }
        out
    }

    const STREAM: &[u8] = b"data: {\"type\":\"server.connected\"}\n\n\
        : heartbeat\n\n\
        event: message\r\nid: evt_1\r\nretry: 10\r\ndata: one\r\ndata:two\r\n\r\n\
        data: three\r\rdata\n\n\
        data: {\"type\":\"session.execution.failed\",\"data\":{\"sessionID\":\"ses_1\"}}\n\n";

    #[test]
    fn rf_age_05_sse_events_split_at_every_byte_are_the_same() {
        let whole = decode(&[STREAM]);
        assert_eq!(
            whole,
            vec![
                r#"{"type":"server.connected"}"#.to_owned(),
                "one\ntwo".to_owned(),
                "three".to_owned(),
                String::new(),
                r#"{"type":"session.execution.failed","data":{"sessionID":"ses_1"}}"#.to_owned(),
            ]
        );
        for cut in 0..=STREAM.len() {
            let (a, b) = STREAM.split_at(cut);
            assert_eq!(decode(&[a, b]), whole, "cut at {cut}");
        }
        let bytes: Vec<&[u8]> = STREAM.chunks(1).collect();
        assert_eq!(decode(&bytes), whole);
    }

    #[test]
    fn an_event_is_only_sent_at_its_blank_line() {
        assert!(decode(&[b"data: x\n"]).is_empty());
        assert!(decode(&[b"event: only\n\n: comment\n\n\n\n"]).is_empty());
    }

    #[test]
    fn rn_02_an_oversized_line_drops_its_event_and_the_stream_recovers() {
        let mut long = b"data: ".to_vec();
        long.extend(std::iter::repeat_n(b'x', MAX_SSE_LINE));
        long.extend_from_slice(b"\ndata: tail\n\ndata: next\n\n");
        assert_eq!(decode(&[&long]), vec!["next".to_owned()]);
        // A long comment counts the same.
        let mut comment = b":".to_vec();
        comment.extend(std::iter::repeat_n(b'h', MAX_SSE_LINE + 10));
        comment.extend_from_slice(b"\ndata: a\n\ndata: b\n\n");
        assert_eq!(decode(&[&comment]), vec!["b".to_owned()]);
    }

    #[test]
    fn a_line_at_the_cap_is_kept() {
        let value = "y".repeat(MAX_SSE_LINE - "data:".len());
        let stream = format!("data:{value}\n\n");
        assert_eq!(decode(&[stream.as_bytes()]), vec![value]);
    }

    #[test]
    fn rn_02_an_oversized_event_is_dropped_and_the_stream_recovers() {
        let line = format!("data: {}\n", "z".repeat(MAX_SSE_LINE / 2));
        let mut stream = line.repeat(3).into_bytes();
        stream.extend_from_slice(b"\ndata: after\n\n");
        assert_eq!(decode(&[&stream]), vec!["after".to_owned()]);
    }

    #[test]
    fn an_event_that_is_not_utf8_is_dropped() {
        assert_eq!(
            decode(&[b"data: \xff\n\ndata: ok\n\n"]),
            vec!["ok".to_owned()]
        );
    }

    #[test]
    fn rf_age_05_a_failed_turn_names_its_session_and_nothing_else() {
        let failed = r#"{"id":"evt_1","type":"session.execution.failed","created":1,
            "location":{"directory":"C:/work"},
            "data":{"sessionID":"ses_e29191ec0ffe3tC2NzOxg6Pfch",
            "error":{"type":"provider.no-route","message":"secret prompt text"}}}"#;
        let event = parse_event(failed);
        assert_eq!(
            event,
            Some(ServiceEvent::Failed {
                session: "ses_e29191ec0ffe3tC2NzOxg6Pfch".to_owned()
            })
        );
        assert!(!format!("{event:?}").contains("secret"));
    }

    #[test]
    fn rn_02_other_and_malformed_events_are_ignored() {
        for data in [
            r#"{"type":"session.execution.started","data":{"sessionID":"ses_1"}}"#,
            r#"{"type":"session.execution.succeeded","data":{"sessionID":"ses_1"}}"#,
            r#"{"type":"session.execution.interrupted","data":{"sessionID":"ses_1","reason":"user"}}"#,
            r#"{"type":"session.text.delta","data":{"text":"session.execution.failed"}}"#,
            r#"{"type":"session.execution.failed"}"#,
            r#"{"type":"session.execution.failed","data":{}}"#,
            r#"{"type":"session.execution.failed","data":{"sessionID":7}}"#,
            r#"{"type":"session.execution.failed","data":{"sessionID":""}}"#,
            r#"{"type":"session.execution.failed","data":{"sessionID":"ses 1"}}"#,
            r#"{"type":"session.execution.failed","data":{"sessionID":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}"#,
            r#"{"type":"session.execution.failed","data":{"sessionID":"ses_1"}"#,
            "session.execution.failed",
            "",
        ] {
            assert_eq!(parse_event(data), None, "{data}");
        }
    }
}
