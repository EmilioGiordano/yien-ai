//! The one message `yien-hook` sends to the hook pipe (ADR-09): a fixed
//! header, then the pane token, the event name from argv and the raw hook
//! payload.
//!
//! ```text
//! offset  size  field
//! 0       2     magic "YH"
//! 2       1     version (HOOK_FRAME_VERSION)
//! 3       1     token length, 1..=MAX_TOKEN_LEN
//! 4       1     event length, 0..=MAX_EVENT_LEN
//! 5       4     payload length, little endian, 0..=MAX_HOOK_PAYLOAD
//! 9       ...   token, event (ASCII letters, digits, `_`), payload
//! ```
//!
//! The header carries every length, so a reader checks the sizes before it
//! allocates, and checks the token before it reads the payload.
//!
//! ```
//! use yien_proto::hook_frame::{decode, encode};
//!
//! let bytes = encode(b"tok", "Stop", b"{}")?;
//! let frame = decode(&bytes)?;
//! assert_eq!((frame.token, frame.event, frame.payload), (&b"tok"[..], "Stop", &b"{}"[..]));
//! # Ok::<(), yien_proto::hook_frame::FrameError>(())
//! ```

use std::fmt;

/// Environment variable that carries the hook pipe's name to `yien-hook`.
pub const HOOK_PIPE_ENV: &str = "YIEN_HOOK_PIPE";

/// Every hook pipe name starts with this, followed by the server's pid, `-`
/// and 16 lowercase hex digits (see [`hook_pipe_name`]).
pub const HOOK_PIPE_PREFIX: &str = r"\\.\pipe\yien-hook-";

/// Hex digits of the random nonce in a hook pipe name.
const PIPE_NONCE_LEN: usize = 16;

/// The hook pipe name for server process `pid` and a random `nonce`.
///
/// ```
/// use yien_proto::hook_frame::{hook_pipe_name, hook_pipe_pid};
///
/// let name = hook_pipe_name(4242, 0xbeef);
/// assert_eq!(name, r"\\.\pipe\yien-hook-4242-000000000000beef");
/// assert_eq!(hook_pipe_pid(&name), Some(4242));
/// ```
pub fn hook_pipe_name(pid: u32, nonce: u64) -> String {
    format!("{HOOK_PIPE_PREFIX}{pid}-{nonce:016x}")
}

/// The server pid in a hook pipe name, or `None` when `name` is not exactly
/// the shape [`hook_pipe_name`] makes. `yien-hook` opens nothing else, so a
/// tampered environment cannot point it at a file: Win32 resolves `..` even
/// inside `\\.\` paths, and this shape has no `.`, `\` or `/` after the prefix.
///
/// ```
/// use yien_proto::hook_frame::hook_pipe_pid;
///
/// assert_eq!(hook_pipe_pid(r"\\.\pipe\yien-hook-1-00000000000000ff"), Some(1));
/// assert_eq!(hook_pipe_pid(r"\\.\pipe\yien-hook-1-00\..\..\C:\x"), None);
/// ```
pub fn hook_pipe_pid(name: &str) -> Option<u32> {
    let rest = name.strip_prefix(HOOK_PIPE_PREFIX)?;
    let (pid, nonce) = rest.split_once('-')?;
    let pid_ok = !pid.is_empty() && pid.len() <= 10 && pid.bytes().all(|b| b.is_ascii_digit());
    let nonce_ok = nonce.len() == PIPE_NONCE_LEN
        && nonce
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !pid_ok || !nonce_ok {
        return None;
    }
    pid.parse().ok()
}

/// Environment variable that carries the per-pane token to `yien-hook`. The
/// same name as `yien_agent::HOOK_TOKEN_ENV`.
pub const HOOK_TOKEN_ENV: &str = "YIEN_HOOK_TOKEN";

/// The first two bytes of every frame.
pub const HOOK_FRAME_MAGIC: [u8; 2] = *b"YH";

/// The frame layout version. Any change to the layout bumps it.
pub const HOOK_FRAME_VERSION: u8 = 1;

/// Size of the fixed header.
pub const HEADER_LEN: usize = 9;

/// The longest token a frame carries.
pub const MAX_TOKEN_LEN: usize = 128;

/// The longest event name a frame carries.
pub const MAX_EVENT_LEN: usize = 64;

/// The largest payload a frame carries; the same limit as
/// `yien_agent::MAX_HOOK_PAYLOAD`.
pub const MAX_HOOK_PAYLOAD: usize = 4 * 1024 * 1024;

/// The largest whole frame.
pub const MAX_FRAME_LEN: usize = HEADER_LEN + MAX_TOKEN_LEN + MAX_EVENT_LEN + MAX_HOOK_PAYLOAD;

/// Why a frame was refused. Never carries frame content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// The first two bytes are not [`HOOK_FRAME_MAGIC`].
    BadMagic,
    /// The version is not [`HOOK_FRAME_VERSION`].
    UnsupportedVersion(u8),
    /// The token is empty or longer than [`MAX_TOKEN_LEN`].
    BadTokenLen(usize),
    /// The event name is longer than [`MAX_EVENT_LEN`].
    EventTooLong(usize),
    /// The event name has a byte outside ASCII letters, digits and `_`.
    BadEventName,
    /// The payload is longer than [`MAX_HOOK_PAYLOAD`].
    PayloadTooLarge(usize),
    /// The bytes end before the lengths in the header say.
    Truncated,
    /// There are bytes after the payload.
    TrailingBytes,
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadMagic => f.write_str("not a hook frame"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported hook frame version {v}"),
            Self::BadTokenLen(n) => write!(f, "hook token of {n} bytes is empty or too long"),
            Self::EventTooLong(n) => write!(f, "hook event name of {n} bytes is too long"),
            Self::BadEventName => f.write_str("hook event name has a forbidden byte"),
            Self::PayloadTooLarge(n) => write!(
                f,
                "hook payload of {n} bytes exceeds the {MAX_HOOK_PAYLOAD} byte limit"
            ),
            Self::Truncated => f.write_str("hook frame is truncated"),
            Self::TrailingBytes => f.write_str("hook frame has trailing bytes"),
        }
    }
}

impl std::error::Error for FrameError {}

/// The lengths a valid header announces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    /// Token length, 1..=[`MAX_TOKEN_LEN`].
    pub token_len: usize,
    /// Event name length, 0..=[`MAX_EVENT_LEN`].
    pub event_len: usize,
    /// Payload length, 0..=[`MAX_HOOK_PAYLOAD`].
    pub payload_len: usize,
}

impl FrameHeader {
    /// Length of the token and event name that follow the header.
    ///
    /// ```
    /// let h = yien_proto::hook_frame::FrameHeader { token_len: 3, event_len: 4, payload_len: 9 };
    /// assert_eq!(h.prefix_len(), 7);
    /// ```
    pub fn prefix_len(&self) -> usize {
        self.token_len + self.event_len
    }
}

/// A decoded frame, borrowing the bytes it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HookFrame<'a> {
    /// The pane token.
    pub token: &'a [u8],
    /// The event name `yien-hook` got in argv; may be empty.
    pub event: &'a str,
    /// The raw hook payload.
    pub payload: &'a [u8],
}

/// Checks a header and returns the lengths it announces.
///
/// ```
/// use yien_proto::hook_frame::{decode_header, encode, HEADER_LEN};
///
/// let bytes = encode(b"tok", "Stop", b"{}")?;
/// let header: [u8; HEADER_LEN] = bytes[..HEADER_LEN].try_into().unwrap();
/// assert_eq!(decode_header(&header)?.payload_len, 2);
/// # Ok::<(), yien_proto::hook_frame::FrameError>(())
/// ```
pub fn decode_header(bytes: &[u8; HEADER_LEN]) -> Result<FrameHeader, FrameError> {
    if bytes[..2] != HOOK_FRAME_MAGIC {
        return Err(FrameError::BadMagic);
    }
    if bytes[2] != HOOK_FRAME_VERSION {
        return Err(FrameError::UnsupportedVersion(bytes[2]));
    }
    let token_len = usize::from(bytes[3]);
    if token_len == 0 || token_len > MAX_TOKEN_LEN {
        return Err(FrameError::BadTokenLen(token_len));
    }
    let event_len = usize::from(bytes[4]);
    if event_len > MAX_EVENT_LEN {
        return Err(FrameError::EventTooLong(event_len));
    }
    let payload_len = u32::from_le_bytes([bytes[5], bytes[6], bytes[7], bytes[8]]) as usize;
    if payload_len > MAX_HOOK_PAYLOAD {
        return Err(FrameError::PayloadTooLarge(payload_len));
    }
    Ok(FrameHeader {
        token_len,
        event_len,
        payload_len,
    })
}

/// Splits the bytes after the header into token and event name, exactly
/// [`FrameHeader::prefix_len`] of them.
///
/// ```
/// use yien_proto::hook_frame::{decode_prefix, FrameHeader};
///
/// let h = FrameHeader { token_len: 3, event_len: 4, payload_len: 0 };
/// assert_eq!(decode_prefix(&h, b"tokStop")?, (&b"tok"[..], "Stop"));
/// # Ok::<(), yien_proto::hook_frame::FrameError>(())
/// ```
pub fn decode_prefix<'a>(
    header: &FrameHeader,
    bytes: &'a [u8],
) -> Result<(&'a [u8], &'a str), FrameError> {
    if bytes.len() < header.prefix_len() {
        return Err(FrameError::Truncated);
    }
    if bytes.len() > header.prefix_len() {
        return Err(FrameError::TrailingBytes);
    }
    let (token, event) = bytes.split_at(header.token_len);
    Ok((token, event_str(event)?))
}

/// Decodes one whole frame; the slice must hold exactly one.
///
/// ```
/// use yien_proto::hook_frame::{decode, FrameError};
///
/// assert_eq!(decode(b"XX"), Err(FrameError::Truncated));
/// ```
pub fn decode(bytes: &[u8]) -> Result<HookFrame<'_>, FrameError> {
    let Some((header, rest)) = bytes.split_first_chunk::<HEADER_LEN>() else {
        return Err(FrameError::Truncated);
    };
    let header = decode_header(header)?;
    if rest.len() < header.prefix_len() {
        return Err(FrameError::Truncated);
    }
    let (prefix, payload) = rest.split_at(header.prefix_len());
    let (token, event) = decode_prefix(&header, prefix)?;
    if payload.len() < header.payload_len {
        return Err(FrameError::Truncated);
    }
    if payload.len() > header.payload_len {
        return Err(FrameError::TrailingBytes);
    }
    Ok(HookFrame {
        token,
        event,
        payload,
    })
}

/// Builds a frame. Fails on the same limits [`decode`] enforces.
///
/// ```
/// use yien_proto::hook_frame::{encode, FrameError};
///
/// assert_eq!(encode(b"", "Stop", b""), Err(FrameError::BadTokenLen(0)));
/// assert_eq!(encode(b"t", "no spaces", b""), Err(FrameError::BadEventName));
/// ```
pub fn encode(token: &[u8], event: &str, payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    if token.is_empty() || token.len() > MAX_TOKEN_LEN {
        return Err(FrameError::BadTokenLen(token.len()));
    }
    if event.len() > MAX_EVENT_LEN {
        return Err(FrameError::EventTooLong(event.len()));
    }
    event_str(event.as_bytes())?;
    if payload.len() > MAX_HOOK_PAYLOAD {
        return Err(FrameError::PayloadTooLarge(payload.len()));
    }
    let mut out = Vec::with_capacity(HEADER_LEN + token.len() + event.len() + payload.len());
    out.extend_from_slice(&HOOK_FRAME_MAGIC);
    out.push(HOOK_FRAME_VERSION);
    // The limits above keep every length inside its field.
    out.push(token.len() as u8);
    out.push(event.len() as u8);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(token);
    out.extend_from_slice(event.as_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// Whether `event` is a name a frame can carry.
///
/// ```
/// assert!(yien_proto::hook_frame::is_valid_event("PreToolUse"));
/// assert!(!yien_proto::hook_frame::is_valid_event("a b"));
/// ```
pub fn is_valid_event(event: &str) -> bool {
    event.len() <= MAX_EVENT_LEN && event_str(event.as_bytes()).is_ok()
}

fn event_str(bytes: &[u8]) -> Result<&str, FrameError> {
    if !bytes
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || *b == b'_')
    {
        return Err(FrameError::BadEventName);
    }
    std::str::from_utf8(bytes).map_err(|_| FrameError::BadEventName)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn frame_matches_the_golden_bytes() {
        let bytes = encode(b"ab12", "Stop", b"{}").unwrap();
        let golden: &[u8] = &[
            b'Y', b'H', // magic
            1,    // version
            4,    // token length
            4,    // event length
            2, 0, 0, 0, // payload length, little endian
            b'a', b'b', b'1', b'2', // token
            b'S', b't', b'o', b'p', // event
            b'{', b'}', // payload
        ];
        assert_eq!(bytes, golden);
        assert_eq!(
            decode(golden),
            Ok(HookFrame {
                token: b"ab12",
                event: "Stop",
                payload: b"{}",
            })
        );
    }

    #[test]
    fn pipe_prefix_is_a_local_pipe_path() {
        assert_eq!(HOOK_PIPE_PREFIX.as_bytes()[..9], *br"\\.\pipe\");
    }

    #[test]
    fn rnf_11_pipe_names_outside_the_exact_shape_are_refused() {
        let good = hook_pipe_name(77, u64::MAX);
        assert_eq!(hook_pipe_pid(&good), Some(77));
        let n = "0123456789abcdef";
        let p = HOOK_PIPE_PREFIX;
        let bad = [
            String::new(),
            p.to_owned(),
            format!(r"{p}1-{n}\..\..\C:\victim.txt"),
            format!(r"{p}..\..\C:\victim.txt"),
            format!(r"{p}1-{n}/../x"),
            format!(r"{p}1-{n}."),
            format!(r"{p}."),
            format!(r"{p}1-0123456789ABCDEF"),
            format!(r"{p}1-{n}0"),
            format!(r"{p}-{n}"),
            format!(r"{p}99999999999-{n}"),
            format!(r"{p}1-2-{n}"),
            format!(r"\\?\pipe\yien-hook-1-{n}"),
            format!(r"C:\yien-hook-1-{n}"),
        ];
        for name in bad {
            assert_eq!(hook_pipe_pid(&name), None, "{name:?}");
        }
    }

    #[test]
    fn empty_event_and_payload_are_allowed() {
        let bytes = encode(b"t", "", b"").unwrap();
        assert_eq!(bytes, [b'Y', b'H', 1, 1, 0, 0, 0, 0, 0, b't']);
        let frame = decode(&bytes).unwrap();
        assert_eq!((frame.event, frame.payload), ("", &b""[..]));
    }

    #[test]
    fn largest_payload_round_trips() {
        let payload = vec![b'x'; MAX_HOOK_PAYLOAD];
        let token = [b'k'; MAX_TOKEN_LEN];
        let event = "E".repeat(MAX_EVENT_LEN);
        let bytes = encode(&token, &event, &payload).unwrap();
        assert_eq!(bytes.len(), MAX_FRAME_LEN);
        assert_eq!(decode(&bytes).unwrap().payload.len(), MAX_HOOK_PAYLOAD);
    }

    #[test]
    fn header_refuses_each_bad_field() {
        let good: [u8; HEADER_LEN] = [b'Y', b'H', 1, 4, 4, 2, 0, 0, 0];
        assert!(decode_header(&good).is_ok());
        let cases: [(usize, u8, FrameError); 5] = [
            (0, b'Z', FrameError::BadMagic),
            (2, 2, FrameError::UnsupportedVersion(2)),
            (3, 0, FrameError::BadTokenLen(0)),
            (3, 129, FrameError::BadTokenLen(129)),
            (4, 65, FrameError::EventTooLong(65)),
        ];
        for (at, value, expected) in cases {
            let mut h = good;
            h[at] = value;
            assert_eq!(decode_header(&h), Err(expected), "byte {at} = {value}");
        }
        let mut big = good;
        big[5..9].copy_from_slice(&((MAX_HOOK_PAYLOAD + 1) as u32).to_le_bytes());
        assert_eq!(
            decode_header(&big),
            Err(FrameError::PayloadTooLarge(MAX_HOOK_PAYLOAD + 1))
        );
        big[5..9].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            decode_header(&big),
            Err(FrameError::PayloadTooLarge(_))
        ));
    }

    #[test]
    fn truncated_and_trailing_bytes_are_refused() {
        let bytes = encode(b"tok", "Stop", b"{}").unwrap();
        for cut in 0..bytes.len() {
            assert_eq!(decode(&bytes[..cut]), Err(FrameError::Truncated), "{cut}");
        }
        let mut long = bytes.clone();
        long.push(0);
        assert_eq!(decode(&long), Err(FrameError::TrailingBytes));
    }

    #[test]
    fn event_names_are_held_to_ascii_words() {
        for bad in ["a b", "Stop\n", "Stöp", "a-b", "../x"] {
            assert_eq!(
                encode(b"t", bad, b""),
                Err(FrameError::BadEventName),
                "{bad:?}"
            );
        }
        let mut bytes = encode(b"t", "Stop", b"").unwrap();
        bytes[HEADER_LEN + 1] = 0xff;
        assert_eq!(decode(&bytes), Err(FrameError::BadEventName));
    }

    #[test]
    fn encode_refuses_oversize_parts() {
        assert_eq!(
            encode(&[b'k'; MAX_TOKEN_LEN + 1], "", b""),
            Err(FrameError::BadTokenLen(MAX_TOKEN_LEN + 1))
        );
        assert_eq!(
            encode(b"t", &"E".repeat(MAX_EVENT_LEN + 1), b""),
            Err(FrameError::EventTooLong(MAX_EVENT_LEN + 1))
        );
        assert_eq!(
            encode(b"t", "", &vec![0; MAX_HOOK_PAYLOAD + 1]),
            Err(FrameError::PayloadTooLarge(MAX_HOOK_PAYLOAD + 1))
        );
    }

    #[test]
    fn errors_do_not_echo_content() {
        let secret = "sk-secret";
        let mut bytes = encode(secret.as_bytes(), "Stop", secret.as_bytes()).unwrap();
        bytes.push(0);
        let err = decode(&bytes).unwrap_err();
        assert!(!err.to_string().contains(secret));
        assert!(!format!("{err:?}").contains(secret));
    }

    proptest! {
        #[test]
        fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
            let _ = decode(&bytes);
        }

        #[test]
        fn arbitrary_bytes_behind_a_valid_magic_never_panic(
            version in 0u8..3,
            token_len in any::<u8>(),
            event_len in any::<u8>(),
            payload_len in 0u32..600,
            body in proptest::collection::vec(any::<u8>(), 0..800),
        ) {
            let mut bytes = vec![b'Y', b'H', version, token_len, event_len];
            bytes.extend_from_slice(&payload_len.to_le_bytes());
            bytes.extend_from_slice(&body);
            if let Ok(frame) = decode(&bytes) {
                prop_assert_eq!(frame.token.len(), usize::from(token_len));
                prop_assert_eq!(frame.payload.len(), payload_len as usize);
            }
        }

        #[test]
        fn valid_frames_round_trip(
            token in proptest::collection::vec(any::<u8>(), 1..=MAX_TOKEN_LEN),
            event in "[A-Za-z0-9_]{0,64}",
            payload in proptest::collection::vec(any::<u8>(), 0..2048),
        ) {
            let bytes = encode(&token, &event, &payload).unwrap();
            let frame = decode(&bytes).unwrap();
            prop_assert_eq!(frame.token, &token[..]);
            prop_assert_eq!(frame.event, event.as_str());
            prop_assert_eq!(frame.payload, &payload[..]);
        }
    }
}
