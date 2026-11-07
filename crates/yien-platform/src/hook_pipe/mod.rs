//! The hook pipe (ADR-09, plan §4 "Pipe de hooks"): receives the frames
//! `yien-hook` sends from each pane's `claude`, checks the pane token and the
//! sizes, decodes the payload and hands `(pane, event)` to a callback.
//!
//! The pipe admits only the current user and local clients (RNF-11). Payloads
//! and tokens are never logged; only the event type, size and timing are.
//! Only Windows has a transport today; elsewhere [`HookPipeServer::start`]
//! returns [`HookPipeError::Unsupported`].
//!
//! ```no_run
//! use yien_platform::hook_pipe::{HookPipeServer, HookRegistry};
//! use yien_proto::PaneId;
//!
//! let registry = HookRegistry::new();
//! let token = registry.issue(PaneId::new(1))?;
//! let server = HookPipeServer::start(registry.clone(), |delivery| {
//!     println!("pane {} sent {:?}", delivery.pane.get(), delivery.event);
//! })?;
//! // Spawn `claude` with YIEN_HOOK_PIPE = server.name() and
//! // YIEN_HOOK_TOKEN = token.as_str().
//! # let _ = (server.name(), token.as_str());
//! # Ok::<(), yien_platform::hook_pipe::HookPipeError>(())
//! ```

use std::collections::HashMap;
use std::fmt;
use std::io::{self, Read};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use yien_agent::{HookError, HookEvent, decode_claude_hook_or_event};
use yien_proto::PaneId;
use yien_proto::hook_frame::{self, FrameError, HEADER_LEN, MAX_EVENT_LEN, MAX_TOKEN_LEN};

pub use yien_proto::hook_frame::{HOOK_PIPE_ENV, HOOK_PIPE_PREFIX, HOOK_TOKEN_ENV};

#[cfg(not(windows))]
mod unsupported;
#[cfg(not(windows))]
use unsupported as imp;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

/// Random bytes in a token; it travels hex encoded, twice as long.
pub const TOKEN_BYTES: usize = 32;

/// How long one connection may take to deliver its whole frame.
pub const READ_TIMEOUT: Duration = Duration::from_secs(2);

/// Connections served at once; more are closed unread.
pub const MAX_CONNECTIONS: usize = 64;

/// Why the hook pipe could not start or a token could not be made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HookPipeError {
    /// The OS random generator failed.
    #[error("the OS random generator failed")]
    Random,
    /// An OS call failed.
    #[error("{call} failed with OS error {code}")]
    Os {
        /// The failing call.
        call: &'static str,
        /// The OS error code.
        code: i32,
    },
    /// This platform has no hook pipe transport yet.
    #[error("the hook pipe is not available on this platform yet")]
    Unsupported,
}

/// A per-pane secret `yien-hook` presents in every frame. `Debug` never
/// shows it.
///
/// ```
/// let registry = yien_platform::hook_pipe::HookRegistry::new();
/// let token = registry.issue(yien_proto::PaneId::new(1))?;
/// assert_eq!(token.as_str().len(), 64);
/// assert!(!format!("{token:?}").contains(token.as_str()));
/// # Ok::<(), yien_platform::hook_pipe::HookPipeError>(())
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct HookToken {
    value: String,
    serial: u64,
}

impl HookToken {
    fn generate(serial: u64) -> Result<Self, HookPipeError> {
        Ok(Self {
            value: random_hex::<TOKEN_BYTES>()?,
            serial,
        })
    }

    /// The value for `YIEN_HOOK_TOKEN`.
    ///
    /// ```
    /// let token = yien_platform::hook_pipe::HookRegistry::new().issue(yien_proto::PaneId::new(1))?;
    /// assert!(token.as_str().bytes().all(|b| b.is_ascii_hexdigit()));
    /// # Ok::<(), yien_platform::hook_pipe::HookPipeError>(())
    /// ```
    pub fn as_str(&self) -> &str {
        &self.value
    }

    /// Which issue of its registry this token is: the first is 1, and each
    /// later one is higher. A delivery carries it as [`HookDelivery::serial`],
    /// so a pane can tell its current launch's hooks from an earlier one's.
    ///
    /// ```
    /// let registry = yien_platform::hook_pipe::HookRegistry::new();
    /// let pane = yien_proto::PaneId::new(1);
    /// assert_eq!(registry.issue(pane)?.serial(), 1);
    /// assert_eq!(registry.issue(pane)?.serial(), 2);
    /// # Ok::<(), yien_platform::hook_pipe::HookPipeError>(())
    /// ```
    pub fn serial(&self) -> u64 {
        self.serial
    }
}

impl fmt::Debug for HookToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HookToken(..)")
    }
}

/// `N` bytes from the OS CSPRNG, lowercase hex.
fn random_hex<const N: usize>() -> Result<String, HookPipeError> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(|_| HookPipeError::Random)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// The live tokens, one per pane. Cloning shares the same registry.
///
/// ```
/// use yien_platform::hook_pipe::HookRegistry;
/// use yien_proto::PaneId;
///
/// let registry = HookRegistry::new();
/// let token = registry.issue(PaneId::new(4))?;
/// assert_eq!(registry.owner(token.as_str().as_bytes()), Some(PaneId::new(4)));
/// registry.revoke(PaneId::new(4));
/// assert_eq!(registry.owner(token.as_str().as_bytes()), None);
/// # Ok::<(), yien_platform::hook_pipe::HookPipeError>(())
/// ```
#[derive(Debug, Clone, Default)]
pub struct HookRegistry {
    tokens: Arc<Mutex<HashMap<PaneId, HookToken>>>,
    issued: Arc<AtomicU64>,
}

impl HookRegistry {
    /// An empty registry.
    ///
    /// ```
    /// let registry = yien_platform::hook_pipe::HookRegistry::new();
    /// assert_eq!(registry.owner(b"anything"), None);
    /// ```
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes a fresh token for `pane`, replacing any earlier one.
    ///
    /// ```
    /// let registry = yien_platform::hook_pipe::HookRegistry::new();
    /// let pane = yien_proto::PaneId::new(1);
    /// let old = registry.issue(pane)?;
    /// let new = registry.issue(pane)?;
    /// assert_eq!(registry.owner(old.as_str().as_bytes()), None);
    /// assert_eq!(registry.owner(new.as_str().as_bytes()), Some(pane));
    /// # Ok::<(), yien_platform::hook_pipe::HookPipeError>(())
    /// ```
    pub fn issue(&self, pane: PaneId) -> Result<HookToken, HookPipeError> {
        let token = HookToken::generate(self.issued.fetch_add(1, Ordering::Relaxed) + 1)?;
        self.lock().insert(pane, token.clone());
        Ok(token)
    }

    /// Forgets the token of `pane`; returns whether it had one.
    ///
    /// ```
    /// let registry = yien_platform::hook_pipe::HookRegistry::new();
    /// assert!(!registry.revoke(yien_proto::PaneId::new(1)));
    /// ```
    pub fn revoke(&self, pane: PaneId) -> bool {
        self.lock().remove(&pane).is_some()
    }

    /// The pane that owns `token`. Every live token is compared in full, so
    /// the time taken does not depend on how much of a token matched.
    ///
    /// ```
    /// let registry = yien_platform::hook_pipe::HookRegistry::new();
    /// assert_eq!(registry.owner(b""), None);
    /// ```
    pub fn owner(&self, token: &[u8]) -> Option<PaneId> {
        self.find(token).map(|(pane, _)| pane)
    }

    /// The pane that owns `token` and the token's serial.
    fn find(&self, token: &[u8]) -> Option<(PaneId, u64)> {
        let tokens = self.lock();
        let mut found = None;
        for (pane, live) in tokens.iter() {
            if constant_time_eq(live.value.as_bytes(), token) {
                found = Some((*pane, live.serial));
            }
        }
        found
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<PaneId, HookToken>> {
        self.tokens.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// One hook event, attributed to the pane whose token came with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookDelivery {
    /// The pane that owns the token.
    pub pane: PaneId,
    /// The token's [`HookToken::serial`].
    pub serial: u64,
    /// The decoded event. An error means the frame was authentic but the
    /// payload could not be read even with the argv fallback; it should reach
    /// the state machine as `Signal::Ambiguous`.
    pub event: Result<HookEvent, HookError>,
    /// Size of the raw payload.
    pub payload_len: usize,
}

/// Why a connection delivered nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// The frame broke the format or a size limit.
    Frame(FrameError),
    /// The token is not live.
    UnknownToken,
    /// The client did not send its whole frame within [`READ_TIMEOUT`].
    TimedOut,
    /// [`MAX_CONNECTIONS`] were already being served.
    Busy,
    /// Reading failed.
    Io(io::ErrorKind),
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frame(e) => write!(f, "bad frame: {e}"),
            Self::UnknownToken => f.write_str("unknown token"),
            Self::TimedOut => f.write_str("timed out"),
            Self::Busy => f.write_str("too many connections"),
            Self::Io(kind) => write!(f, "read failed: {kind}"),
        }
    }
}

/// Counters for one server, read with [`HookPipeServer::stats`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HookPipeStats {
    /// Events handed to the callback.
    pub delivered: u64,
    /// Connections that delivered nothing.
    pub rejected: u64,
    /// The latest rejection.
    pub last_rejection: Option<Rejection>,
}

type Sink = Box<dyn Fn(HookDelivery) + Send + Sync>;

/// What every connection handler shares.
struct Shared {
    registry: HookRegistry,
    sink: Sink,
    delivered: AtomicU64,
    rejected: AtomicU64,
    last_rejection: Mutex<Option<Rejection>>,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl Shared {
    fn handle<R: Read>(&self, conn: &mut R) {
        let started = Instant::now();
        match serve_connection(conn, &self.registry) {
            Ok(delivery) => {
                tracing::debug!(
                    event = event_kind(&delivery.event),
                    bytes = delivery.payload_len,
                    micros = started.elapsed().as_micros() as u64,
                    "hook event delivered"
                );
                self.delivered.fetch_add(1, Ordering::Relaxed);
                (self.sink)(delivery);
            }
            Err(rejection) => self.reject(rejection, started),
        }
    }

    fn reject(&self, rejection: Rejection, started: Instant) {
        tracing::debug!(
            reason = %rejection,
            micros = started.elapsed().as_micros() as u64,
            "hook connection rejected"
        );
        *self
            .last_rejection
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(rejection);
        self.rejected.fetch_add(1, Ordering::Relaxed);
    }
}

/// Reads one frame from `conn` and decodes it. Sizes are checked before
/// anything is allocated, and the token before the payload is read.
#[cfg_attr(not(windows), allow(dead_code))]
fn serve_connection<R: Read>(
    conn: &mut R,
    registry: &HookRegistry,
) -> Result<HookDelivery, Rejection> {
    let mut header = [0u8; HEADER_LEN];
    read_exact(conn, &mut header)?;
    let header = hook_frame::decode_header(&header).map_err(Rejection::Frame)?;

    let mut prefix = [0u8; MAX_TOKEN_LEN + MAX_EVENT_LEN];
    let prefix = &mut prefix[..header.prefix_len()];
    read_exact(conn, prefix)?;
    let (token, event_arg) =
        hook_frame::decode_prefix(&header, prefix).map_err(Rejection::Frame)?;
    let (pane, serial) = registry.find(token).ok_or(Rejection::UnknownToken)?;

    let mut payload = vec![0u8; header.payload_len];
    read_exact(conn, &mut payload)?;

    Ok(HookDelivery {
        pane,
        serial,
        event: decode_claude_hook_or_event(event_arg, &payload),
        payload_len: payload.len(),
    })
}

fn read_exact<R: Read>(conn: &mut R, buf: &mut [u8]) -> Result<(), Rejection> {
    conn.read_exact(buf).map_err(|e| match e.kind() {
        io::ErrorKind::UnexpectedEof => Rejection::Frame(FrameError::Truncated),
        io::ErrorKind::TimedOut => Rejection::TimedOut,
        kind => Rejection::Io(kind),
    })
}

/// The event's type alone, for logs: no session id, no payload.
fn event_kind(event: &Result<HookEvent, HookError>) -> &'static str {
    match event {
        Ok(HookEvent::SessionStart { .. }) => "SessionStart",
        Ok(HookEvent::UserPromptSubmit) => "UserPromptSubmit",
        Ok(HookEvent::PreToolUse) => "PreToolUse",
        Ok(HookEvent::PostToolUse) => "PostToolUse",
        Ok(HookEvent::PostToolUseFailure) => "PostToolUseFailure",
        Ok(HookEvent::PermissionRequest) => "PermissionRequest",
        Ok(HookEvent::UnattributedPermissionRequest) => "UnattributedPermissionRequest",
        Ok(HookEvent::Notification(_)) => "Notification",
        Ok(HookEvent::Stop) => "Stop",
        Ok(HookEvent::StopFailure) => "StopFailure",
        Ok(HookEvent::SessionEnd(_)) => "SessionEnd",
        Err(_) => "undecodable",
    }
}

/// Opens the client end of hook pipe `name`, as `yien-hook` does, retrying
/// while every instance is busy until `deadline`. Returns `None` for a name
/// not made by [`HookPipeServer`] (so a tampered environment cannot point it
/// at a file), when the pipe is missing, or when its server is not the
/// process the name says (someone else took the name after YIEN exited).
///
/// ```
/// use std::time::Instant;
/// use yien_platform::hook_pipe::connect_client;
///
/// assert!(connect_client(r"C:\Windows\win.ini", Instant::now()).is_none());
/// ```
pub fn connect_client(name: &str, deadline: Instant) -> Option<std::fs::File> {
    imp::connect(name, deadline)
}

/// The hook pipe of one YIEN instance. Dropping it stops accepting and waits
/// for the connections in flight, each bounded by [`READ_TIMEOUT`].
pub struct HookPipeServer {
    name: String,
    shared: Arc<Shared>,
    _transport: imp::Server,
}

impl HookPipeServer {
    /// Creates the pipe under a fresh random name and starts accepting.
    /// `on_event` runs on a connection thread, once per delivered event. It
    /// must not block: dropping the server waits for every connection thread,
    /// so a callback that blocks delays the drop by as long.
    ///
    /// ```no_run
    /// let registry = yien_platform::hook_pipe::HookRegistry::new();
    /// let server = yien_platform::hook_pipe::HookPipeServer::start(registry, |_| {})?;
    /// assert!(server.name().starts_with(yien_platform::hook_pipe::HOOK_PIPE_PREFIX));
    /// # Ok::<(), yien_platform::hook_pipe::HookPipeError>(())
    /// ```
    pub fn start(
        registry: HookRegistry,
        on_event: impl Fn(HookDelivery) + Send + Sync + 'static,
    ) -> Result<Self, HookPipeError> {
        let shared = Arc::new(Shared {
            registry,
            sink: Box::new(on_event),
            delivered: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            last_rejection: Mutex::new(None),
        });
        let nonce = getrandom::u64().map_err(|_| HookPipeError::Random)?;
        let name = hook_frame::hook_pipe_name(std::process::id(), nonce);
        let transport = imp::Server::start(&name, Arc::clone(&shared))?;
        Ok(Self {
            name,
            shared,
            _transport: transport,
        })
    }

    /// The value for `YIEN_HOOK_PIPE`.
    ///
    /// ```no_run
    /// # let server = yien_platform::hook_pipe::HookPipeServer::start(Default::default(), |_| {})?;
    /// let mut claude = std::process::Command::new("claude");
    /// claude.env(yien_platform::hook_pipe::HOOK_PIPE_ENV, server.name());
    /// # Ok::<(), yien_platform::hook_pipe::HookPipeError>(())
    /// ```
    pub fn name(&self) -> &str {
        &self.name
    }

    /// What the server has delivered and rejected so far.
    ///
    /// ```no_run
    /// # let server = yien_platform::hook_pipe::HookPipeServer::start(Default::default(), |_| {})?;
    /// assert_eq!(server.stats().delivered, 0);
    /// # Ok::<(), yien_platform::hook_pipe::HookPipeError>(())
    /// ```
    pub fn stats(&self) -> HookPipeStats {
        HookPipeStats {
            delivered: self.shared.delivered.load(Ordering::Relaxed),
            rejected: self.shared.rejected.load(Ordering::Relaxed),
            last_rejection: *self
                .shared
                .last_rejection
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        }
    }
}

impl fmt::Debug for HookPipeServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HookPipeServer")
            .field("name", &self.name)
            .field("stats", &self.stats())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use yien_agent::NotificationKind;

    fn frame(token: &str, event: &str, payload: &[u8]) -> Vec<u8> {
        hook_frame::encode(token.as_bytes(), event, payload).unwrap()
    }

    fn registry_with(pane: u64) -> (HookRegistry, HookToken) {
        let registry = HookRegistry::new();
        let token = registry.issue(PaneId::new(pane)).unwrap();
        (registry, token)
    }

    #[test]
    fn constants_match_the_agent_crate() {
        assert_eq!(HOOK_TOKEN_ENV, yien_agent::HOOK_TOKEN_ENV);
        assert_eq!(hook_frame::MAX_HOOK_PAYLOAD, yien_agent::MAX_HOOK_PAYLOAD);
        assert!(
            READ_TIMEOUT.as_secs() < yien_agent::HOOK_TIMEOUT_SECS,
            "a connection gives up before Claude does"
        );
    }

    #[test]
    fn a_valid_frame_is_decoded_and_attributed() {
        let (registry, token) = registry_with(7);
        let payload =
            br#"{"hook_event_name":"Notification","notification_type":"permission_prompt"}"#;
        let bytes = frame(token.as_str(), "Notification", payload);
        let delivery = serve_connection(&mut Cursor::new(bytes), &registry).unwrap();
        assert_eq!(
            delivery,
            HookDelivery {
                pane: PaneId::new(7),
                serial: token.serial(),
                event: Ok(HookEvent::Notification(NotificationKind::PermissionPrompt)),
                payload_len: payload.len(),
            }
        );
    }

    #[test]
    fn rnf_11_unknown_token_is_rejected_before_the_payload_is_read() {
        let (registry, _) = registry_with(1);
        let other = "f".repeat(2 * TOKEN_BYTES);
        let mut bytes = frame(&other, "Stop", &vec![b' '; 1000]);
        // Drop the payload: a rejection must not wait for it.
        bytes.truncate(HEADER_LEN + other.len() + "Stop".len());
        assert_eq!(
            serve_connection(&mut Cursor::new(bytes), &registry),
            Err(Rejection::UnknownToken)
        );
    }

    #[test]
    fn rnf_11_revoked_token_is_rejected() {
        let (registry, token) = registry_with(1);
        registry.revoke(PaneId::new(1));
        let bytes = frame(token.as_str(), "Stop", b"{}");
        assert_eq!(
            serve_connection(&mut Cursor::new(bytes), &registry),
            Err(Rejection::UnknownToken)
        );
    }

    #[test]
    fn oversize_header_is_rejected_without_reading_on() {
        let (registry, token) = registry_with(1);
        let mut bytes = frame(token.as_str(), "Stop", b"");
        bytes[5..9].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            serve_connection(&mut Cursor::new(bytes), &registry),
            Err(Rejection::Frame(FrameError::PayloadTooLarge(
                u32::MAX as usize
            )))
        );
    }

    #[test]
    fn short_frames_and_timeouts_are_rejections() {
        let (registry, token) = registry_with(1);
        let bytes = frame(token.as_str(), "Stop", b"{}");
        for cut in 0..bytes.len() {
            assert_eq!(
                serve_connection(&mut Cursor::new(&bytes[..cut]), &registry),
                Err(Rejection::Frame(FrameError::Truncated)),
                "{cut}"
            );
        }

        struct Stalled;
        impl Read for Stalled {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::ErrorKind::TimedOut.into())
            }
        }
        assert_eq!(
            serve_connection(&mut Stalled, &registry),
            Err(Rejection::TimedOut)
        );
    }

    #[test]
    fn broken_payload_falls_back_to_the_argv_event() {
        let (registry, token) = registry_with(2);
        let bytes = frame(token.as_str(), "PreToolUse", b"{\"trunc");
        let delivery = serve_connection(&mut Cursor::new(bytes), &registry).unwrap();
        assert_eq!(delivery.event, Ok(HookEvent::PreToolUse));
    }

    #[test]
    fn rn_02_authentic_but_undecodable_frame_is_delivered_as_an_error() {
        let (registry, token) = registry_with(2);
        let bytes = frame(token.as_str(), "", b"not json");
        let delivery = serve_connection(&mut Cursor::new(bytes), &registry).unwrap();
        assert!(matches!(delivery.event, Err(HookError::Malformed { .. })));
    }

    #[test]
    fn tokens_are_random_hex_and_hidden_from_debug() {
        let registry = HookRegistry::new();
        let a = registry.issue(PaneId::new(1)).unwrap();
        let b = registry.issue(PaneId::new(2)).unwrap();
        assert_ne!(a, b);
        for t in [&a, &b] {
            assert_eq!(t.as_str().len(), 2 * TOKEN_BYTES);
            assert!(t.as_str().bytes().all(|c| c.is_ascii_hexdigit()));
        }
        assert!(!format!("{registry:?}").contains(a.as_str()));
        assert_eq!(registry.owner(a.as_str().as_bytes()), Some(PaneId::new(1)));
        assert_eq!(registry.owner(b.as_str().as_bytes()), Some(PaneId::new(2)));
        assert_eq!(registry.owner(&a.as_str().as_bytes()[1..]), None);
    }

    #[test]
    fn shared_counts_deliveries_and_rejections() {
        let (registry, token) = registry_with(3);
        let (tx, rx) = std::sync::mpsc::channel();
        let shared = Shared {
            registry,
            sink: Box::new(move |d| tx.send(d).unwrap()),
            delivered: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            last_rejection: Mutex::new(None),
        };
        shared.handle(&mut Cursor::new(frame(token.as_str(), "Stop", b"")));
        shared.handle(&mut Cursor::new(b"nope".to_vec()));
        assert_eq!(rx.try_recv().unwrap().event, Ok(HookEvent::Stop));
        assert_eq!(shared.delivered.load(Ordering::Relaxed), 1);
        assert_eq!(shared.rejected.load(Ordering::Relaxed), 1);
        assert_eq!(
            *shared.last_rejection.lock().unwrap(),
            Some(Rejection::Frame(FrameError::Truncated))
        );
    }
}
