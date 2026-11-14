//! A streaming scanner for the OSC sequences that carry agent and shell
//! signals, independent of the terminal emulator that draws the bytes.
//!
//! It keeps its state between calls, so a sequence cut at any byte by the PTY
//! reader yields the same signals as the whole sequence.
//!
//! ```
//! use yien_vt::osc_scan::{OscScanner, OscSignal};
//!
//! let mut scanner = OscScanner::new();
//! assert!(scanner.scan(b"\x1b]0;buil").is_empty());
//! assert_eq!(scanner.scan(b"d\x07"), vec![OscSignal::Title("build".into())]);
//! ```

/// The longest OSC payload kept, in bytes. Longer sequences are dropped whole.
pub const MAX_PAYLOAD: usize = 4096;

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;

/// A signal found in an OSC sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OscSignal {
    /// OSC 0 or OSC 2: the program set the window title.
    Title(String),
    /// OSC 7, or ConEmu's OSC 9;9: the shell reported its working directory.
    Cwd(String),
    /// OSC 9 or OSC 777;notify: the program asked for a desktop notification.
    Notify {
        /// The title, which only OSC 777 carries.
        title: Option<String>,
        /// The notification text.
        body: String,
    },
    /// OSC 133: a shell integration prompt mark.
    Prompt(PromptMark),
    /// OSC 7501: a program reported its own state. Its `msg` and `title`
    /// fields are user content and are not kept.
    Status(ProgramStatus),
}

/// An OSC 7501 status record: `state=<s>[:kind=<k>][:id=<id>]…`, fields in
/// any order.
///
/// ```
/// use yien_vt::osc_scan::{
///     OscScanner, OscSignal, ProgramState, ProgramStatus, StatusApp, StatusKind,
/// };
///
/// let mut scanner = OscScanner::new();
/// let signals = scanner.scan(b"\x1b]7501;state=blocked:app=codex:kind=permission\x1b\\");
/// let status = ProgramStatus {
///     state: ProgramState::Blocked,
///     kind: Some(StatusKind::Permission),
///     app: StatusApp::Codex,
///     sub_session: false,
/// };
/// assert_eq!(signals, vec![OscSignal::Status(status)]);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgramStatus {
    /// The reported state.
    pub state: ProgramState,
    /// Why it is blocked, when the program said and the value is known.
    pub kind: Option<StatusKind>,
    /// The program that sent it (`app`).
    pub app: StatusApp,
    /// The record carries an `id`: it is about a sub-session or a background
    /// task, not the program as a whole.
    pub sub_session: bool,
}

/// The `state` of an OSC 7501 record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramState {
    /// `idle`.
    Idle,
    /// `working`.
    Working,
    /// `blocked`: it waits for the user.
    Blocked,
    /// `done`: the turn completed.
    Done,
    /// `error`: the turn failed.
    Error,
    /// `clear`: the program gave the terminal back.
    Clear,
}

/// The `kind` of a blocked OSC 7501 record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    /// `permission`.
    Permission,
    /// `question`.
    Question,
    /// `auth`.
    Auth,
}

/// The `app` of an OSC 7501 record: the programs whose records were seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusApp {
    /// `claude-code`.
    ClaudeCode,
    /// `codex`.
    Codex,
    /// `opencode`.
    OpenCode,
    /// Any other value, or none.
    Other,
}

/// A reply the program asked for (`OSC 7501 ; ?`), to be written to the PTY
/// after the bytes before `at` are processed, so it keeps its place among the
/// emulator's own replies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OscReply {
    /// The index just past the query's last byte in the scanned slice.
    pub at: usize,
    /// The bytes to write.
    pub bytes: Vec<u8>,
}

/// The answer to `OSC 7501 ; ?`: support, with no capabilities listed.
/// Claude Code reports its state only once this arrives, and only if it
/// arrives before the answer to the `CSI c` it sends right after the query.
pub const STATUS_QUERY_REPLY: &[u8] = b"\x1b]7501;?\x1b\\";

/// An OSC 7501 payload that asks whether the terminal reads status records.
const STATUS_QUERY: &[u8] = b"7501;?";

/// The OSC 133 marks (FinalTerm shell integration).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptMark {
    /// `A`: the prompt starts.
    PromptStart,
    /// `B`: the prompt ends and the user types a command.
    CommandStart,
    /// `C`: the command runs and its output starts.
    CommandExecuted,
    /// `D`: the command finished, with its exit code when the shell sent one.
    CommandFinished {
        /// The exit code, if present and numeric.
        exit_code: Option<i32>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    Osc,
    OscEscape,
    Discard,
    DiscardEscape,
}

/// The scanner state for one PTY stream.
///
/// ```
/// use yien_vt::osc_scan::{OscScanner, OscSignal, PromptMark};
///
/// let mut scanner = OscScanner::new();
/// let signals = scanner.scan(b"\x1b]133;D;1\x1b\\");
/// let finished = PromptMark::CommandFinished { exit_code: Some(1) };
/// assert_eq!(signals, vec![OscSignal::Prompt(finished)]);
/// ```
#[derive(Debug, Clone)]
pub struct OscScanner {
    state: State,
    payload: Vec<u8>,
}

impl Default for OscScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl OscScanner {
    /// A scanner waiting for the start of a sequence.
    pub fn new() -> Self {
        Self {
            state: State::Ground,
            payload: Vec::new(),
        }
    }

    /// Scans `bytes` and returns the signals completed in them.
    pub fn scan(&mut self, bytes: &[u8]) -> Vec<OscSignal> {
        let mut out = Vec::new();
        self.scan_into(bytes, &mut out);
        out
    }

    /// Scans `bytes` and appends the signals completed in them to `out`.
    pub fn scan_into(&mut self, bytes: &[u8], out: &mut Vec<OscSignal>) {
        self.scan_replying(bytes, out);
    }

    /// Like [`OscScanner::scan_into`], and also returns the replies owed to
    /// the queries completed in `bytes`, in order.
    ///
    /// ```
    /// use yien_vt::osc_scan::{OscScanner, STATUS_QUERY_REPLY};
    ///
    /// let mut scanner = OscScanner::new();
    /// let mut signals = Vec::new();
    /// let replies = scanner.scan_replying(b"ab\x1b]7501;?\x07\x1b[c", &mut signals);
    /// assert_eq!(replies[0].at, 11);
    /// assert_eq!(replies[0].bytes, STATUS_QUERY_REPLY);
    /// assert!(signals.is_empty());
    /// ```
    pub fn scan_replying(&mut self, bytes: &[u8], out: &mut Vec<OscSignal>) -> Vec<OscReply> {
        let mut replies = Vec::new();
        let mut at = 0;
        while let Some(rest) = bytes.get(at..) {
            let Some(&byte) = rest.first() else {
                break;
            };
            if self.state == State::Ground {
                // Fast path: plain output is most of the stream.
                match rest.iter().position(|&b| b == ESC) {
                    Some(offset) => {
                        self.state = State::Escape;
                        at += offset + 1;
                    }
                    None => break,
                }
                continue;
            }
            at += 1;
            if self.step(byte, out) {
                replies.push(OscReply {
                    at,
                    bytes: STATUS_QUERY_REPLY.to_vec(),
                });
            }
        }
        replies
    }

    /// Returns whether `byte` completed a status query.
    fn step(&mut self, byte: u8, out: &mut Vec<OscSignal>) -> bool {
        match self.state {
            State::Ground => {
                if byte == ESC {
                    self.state = State::Escape;
                }
            }
            State::Escape => self.after_escape(byte),
            State::Osc => match byte {
                BEL => return self.finish(out),
                ESC => self.state = State::OscEscape,
                CAN | SUB => self.abort(),
                _ if self.payload.len() < MAX_PAYLOAD => self.payload.push(byte),
                _ => {
                    self.payload.clear();
                    self.state = State::Discard;
                }
            },
            State::OscEscape => {
                if byte == b'\\' {
                    return self.finish(out);
                }
                // An ESC that is not ST cancels the sequence and starts a new one.
                self.payload.clear();
                self.after_escape(byte);
            }
            State::Discard => match byte {
                BEL | CAN | SUB => self.state = State::Ground,
                ESC => self.state = State::DiscardEscape,
                _ => {}
            },
            State::DiscardEscape => {
                if byte == b'\\' {
                    self.state = State::Ground;
                } else {
                    self.after_escape(byte);
                }
            }
        }
        false
    }

    fn after_escape(&mut self, byte: u8) {
        self.state = match byte {
            b']' => {
                self.payload.clear();
                State::Osc
            }
            ESC => State::Escape,
            _ => State::Ground,
        };
    }

    fn abort(&mut self) {
        self.payload.clear();
        self.state = State::Ground;
    }

    /// Returns whether the sequence was a status query.
    fn finish(&mut self, out: &mut Vec<OscSignal>) -> bool {
        let query = self.payload.starts_with(STATUS_QUERY);
        if !query && let Some(signal) = parse(&self.payload) {
            out.push(signal);
        }
        self.abort();
        query
    }
}

fn parse(payload: &[u8]) -> Option<OscSignal> {
    let text = String::from_utf8_lossy(payload);
    let (command, rest) = match text.split_once(';') {
        Some((command, rest)) => (command, Some(rest)),
        None => (text.as_ref(), None),
    };
    match command {
        "0" | "2" => Some(OscSignal::Title(rest.unwrap_or_default().to_owned())),
        "7" => file_uri_path(rest?).map(OscSignal::Cwd),
        "9" => parse_osc9(rest?),
        "777" => parse_osc777(rest?),
        "133" => parse_prompt_mark(rest?).map(OscSignal::Prompt),
        "7501" => parse_status(rest?).map(OscSignal::Status),
        _ => None,
    }
}

/// `key=value` fields split on `:`; the first `=` splits, as base64 values
/// end in `=` padding. Unknown keys and fields without `=` are skipped.
fn parse_status(rest: &str) -> Option<ProgramStatus> {
    let mut state = None;
    let mut kind = None;
    let mut app = None;
    let mut sub_session = false;
    for field in rest.split(':') {
        let Some((key, value)) = field.split_once('=') else {
            continue;
        };
        match key {
            "state" if state.is_none() => state = Some(program_state(value)?),
            "kind" if kind.is_none() => kind = status_kind(value),
            "app" if app.is_none() => app = Some(status_app(value)),
            "id" => sub_session = true,
            _ => {}
        }
    }
    Some(ProgramStatus {
        state: state?,
        kind,
        app: app.unwrap_or(StatusApp::Other),
        sub_session,
    })
}

fn status_app(value: &str) -> StatusApp {
    match value {
        "claude-code" => StatusApp::ClaudeCode,
        "codex" => StatusApp::Codex,
        "opencode" => StatusApp::OpenCode,
        _ => StatusApp::Other,
    }
}

fn program_state(value: &str) -> Option<ProgramState> {
    Some(match value {
        "idle" => ProgramState::Idle,
        "working" => ProgramState::Working,
        "blocked" => ProgramState::Blocked,
        "done" => ProgramState::Done,
        "error" => ProgramState::Error,
        "clear" => ProgramState::Clear,
        _ => return None,
    })
}

fn status_kind(value: &str) -> Option<StatusKind> {
    Some(match value {
        "permission" => StatusKind::Permission,
        "question" => StatusKind::Question,
        "auth" => StatusKind::Auth,
        _ => return None,
    })
}

/// OSC 9 is a notification for iTerm2, but ConEmu and Windows Terminal use
/// `9;<number>;...` for commands (`9;4` is progress). Treating those as
/// notifications would raise false attention marks (RN-02).
fn parse_osc9(rest: &str) -> Option<OscSignal> {
    let (head, tail) = match rest.split_once(';') {
        Some((head, tail)) => (head, Some(tail)),
        None => (rest, None),
    };
    let is_conemu = !head.is_empty() && head.bytes().all(|b| b.is_ascii_digit());
    if !is_conemu {
        return Some(OscSignal::Notify {
            title: None,
            body: rest.to_owned(),
        });
    }
    if head == "9" {
        let path = tail?.trim_matches('"');
        if !path.is_empty() {
            return Some(OscSignal::Cwd(path.to_owned()));
        }
    }
    None
}

fn parse_osc777(rest: &str) -> Option<OscSignal> {
    let mut fields = rest.splitn(3, ';');
    if fields.next()? != "notify" {
        return None;
    }
    let title = fields.next().map(str::to_owned);
    let body = fields.next().unwrap_or_default().to_owned();
    Some(OscSignal::Notify { title, body })
}

fn parse_prompt_mark(rest: &str) -> Option<PromptMark> {
    let mut fields = rest.split(';');
    let mark = match fields.next()? {
        "A" => PromptMark::PromptStart,
        "B" => PromptMark::CommandStart,
        "C" => PromptMark::CommandExecuted,
        "D" => PromptMark::CommandFinished {
            exit_code: fields.next().and_then(|code| code.trim().parse().ok()),
        },
        _ => return None,
    };
    Some(mark)
}

/// `file://host/path` to a path, percent-decoded. `/C:/x` becomes `C:/x` so a
/// Windows path comes out usable. A value that is not a file URI is kept as is.
fn file_uri_path(value: &str) -> Option<String> {
    let path = match strip_prefix_ignore_case(value, "file://") {
        Some(after_scheme) => {
            let at = after_scheme.find('/')?;
            percent_decode(after_scheme.get(at..)?)
        }
        None => value.to_owned(),
    };
    let bytes = path.as_bytes();
    let has_drive = bytes.len() >= 3
        && bytes.first() == Some(&b'/')
        && bytes.get(1).is_some_and(u8::is_ascii_alphabetic)
        && bytes.get(2) == Some(&b':');
    let path = if has_drive {
        path.get(1..).unwrap_or_default().to_owned()
    } else {
        path
    };
    (!path.is_empty()).then_some(path)
}

fn strip_prefix_ignore_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    let head = value.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| value.get(prefix.len()..))
        .flatten()
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&byte) = bytes.get(i) {
        if byte == b'%'
            && let (Some(hi), Some(lo)) = (
                bytes.get(i + 1).and_then(|b| hex(*b)),
                bytes.get(i + 2).and_then(|b| hex(*b)),
            )
        {
            out.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        out.push(byte);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(byte: u8) -> Option<u8> {
    char::from(byte)
        .to_digit(16)
        .and_then(|d| u8::try_from(d).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(bytes: &[u8]) -> Vec<OscSignal> {
        OscScanner::new().scan(bytes)
    }

    fn notify(title: Option<&str>, body: &str) -> OscSignal {
        OscSignal::Notify {
            title: title.map(str::to_owned),
            body: body.to_owned(),
        }
    }

    #[test]
    fn title_with_bel_and_st() {
        assert_eq!(
            scan(b"\x1b]0;one\x07\x1b]2;two\x1b\\"),
            vec![
                OscSignal::Title("one".into()),
                OscSignal::Title("two".into())
            ]
        );
    }

    #[test]
    fn title_keeps_utf8_with_c1_lookalike_bytes() {
        // U+2733 is E2 9C B3: 0x9C is C1 ST in 8-bit mode and must not end the title.
        let bytes = "\x1b]0;\u{2733} Claude Code\x07".as_bytes();
        assert_eq!(
            scan(bytes),
            vec![OscSignal::Title("\u{2733} Claude Code".into())]
        );
    }

    #[test]
    fn cwd_from_file_uri() {
        assert_eq!(
            scan(b"\x1b]7;file://host/home/me/my%20dir\x07"),
            vec![OscSignal::Cwd("/home/me/my dir".into())]
        );
        assert_eq!(
            scan(b"\x1b]7;file:///C:/Users/me\x07"),
            vec![OscSignal::Cwd("C:/Users/me".into())]
        );
    }

    #[test]
    fn cwd_from_conemu_osc9_9() {
        assert_eq!(
            scan(b"\x1b]9;9;\"C:\\work\"\x07"),
            vec![OscSignal::Cwd("C:\\work".into())]
        );
    }

    #[test]
    fn osc9_is_a_notification() {
        assert_eq!(
            scan(b"\x1b]9;Claude needs you\x07"),
            vec![notify(None, "Claude needs you")]
        );
    }

    #[test]
    fn osc9_conemu_commands_are_not_notifications() {
        assert!(scan(b"\x1b]9;4;1;50\x07").is_empty());
        assert!(scan(b"\x1b]9;4\x07").is_empty());
    }

    #[test]
    fn osc777_notify() {
        assert_eq!(
            scan(b"\x1b]777;notify;Codex;done; really\x07"),
            vec![notify(Some("Codex"), "done; really")]
        );
        assert!(scan(b"\x1b]777;other;x\x07").is_empty());
    }

    #[test]
    fn prompt_marks() {
        let signals = scan(b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\x1b]133;D;0\x07");
        assert_eq!(
            signals,
            vec![
                OscSignal::Prompt(PromptMark::PromptStart),
                OscSignal::Prompt(PromptMark::CommandStart),
                OscSignal::Prompt(PromptMark::CommandExecuted),
                OscSignal::Prompt(PromptMark::CommandFinished { exit_code: Some(0) }),
            ]
        );
    }

    #[test]
    fn prompt_finished_is_lenient() {
        let finished = |code| OscSignal::Prompt(PromptMark::CommandFinished { exit_code: code });
        assert_eq!(scan(b"\x1b]133;D\x07"), vec![finished(None)]);
        assert_eq!(scan(b"\x1b]133;D;127;aid=4\x07"), vec![finished(Some(127))]);
        assert_eq!(scan(b"\x1b]133;D;aid=4\x07"), vec![finished(None)]);
        assert_eq!(
            scan(b"\x1b]133;A;cl=m\x07"),
            vec![OscSignal::Prompt(PromptMark::PromptStart)]
        );
    }

    #[test]
    fn unknown_osc_is_ignored() {
        assert!(scan(b"\x1b]52;c;aGVsbG8=\x07\x1b]1337;x\x07\x1b]8;;http://a\x07").is_empty());
    }

    #[test]
    fn esc_that_is_not_st_cancels_and_restarts() {
        assert_eq!(
            scan(b"\x1b]0;lost\x1b]0;kept\x07"),
            vec![OscSignal::Title("kept".into())]
        );
        assert!(scan(b"\x1b]0;lost\x1b[1mtext").is_empty());
    }

    #[test]
    fn can_and_sub_abort() {
        assert!(scan(b"\x1b]0;x\x18\x07").is_empty());
        assert!(scan(b"\x1b]0;x\x1a\x07").is_empty());
    }

    #[test]
    fn overlong_sequence_is_dropped_and_scanning_recovers() {
        let mut bytes = b"\x1b]0;".to_vec();
        bytes.extend(std::iter::repeat_n(b'x', MAX_PAYLOAD * 2));
        bytes.extend_from_slice(b"\x07\x1b]0;next\x1b\\");
        assert_eq!(scan(&bytes), vec![OscSignal::Title("next".into())]);
    }

    #[test]
    fn payload_at_the_cap_is_kept() {
        let title = "y".repeat(MAX_PAYLOAD - 2);
        let bytes = format!("\x1b]0;{title}\x07");
        assert_eq!(scan(bytes.as_bytes()), vec![OscSignal::Title(title)]);
    }

    #[test]
    fn plain_text_and_other_escapes_yield_nothing() {
        assert!(scan(b"hello \x1b[31mred\x1b[0m \x1bPdcs\x1b\\").is_empty());
    }

    #[test]
    fn invalid_utf8_does_not_panic() {
        assert_eq!(
            scan(b"\x1b]0;\xff\xfe\x07"),
            vec![OscSignal::Title("\u{fffd}\u{fffd}".into())]
        );
    }

    fn status(
        app: StatusApp,
        state: ProgramState,
        kind: Option<StatusKind>,
        sub_session: bool,
    ) -> OscSignal {
        OscSignal::Status(ProgramStatus {
            state,
            kind,
            app,
            sub_session,
        })
    }

    fn scan_with_replies(
        scanner: &mut OscScanner,
        bytes: &[u8],
    ) -> (Vec<OscSignal>, Vec<OscReply>) {
        let mut signals = Vec::new();
        let replies = scanner.scan_replying(bytes, &mut signals);
        (signals, replies)
    }

    #[test]
    fn rf_age_05_every_7501_state_with_either_terminator() {
        for (word, state) in [
            ("idle", ProgramState::Idle),
            ("working", ProgramState::Working),
            ("blocked", ProgramState::Blocked),
            ("done", ProgramState::Done),
            ("error", ProgramState::Error),
            ("clear", ProgramState::Clear),
        ] {
            for end in ["\x07", "\x1b\\"] {
                let bytes = format!("\x1b]7501;state={word}:app=codex{end}");
                assert_eq!(
                    scan(bytes.as_bytes()),
                    vec![status(StatusApp::Codex, state, None, false)],
                    "{word}"
                );
            }
        }
        // `clear` on exit comes bare, with no other field.
        assert_eq!(
            scan(b"\x1b]7501;state=clear\x1b\\"),
            vec![status(StatusApp::Other, ProgramState::Clear, None, false)]
        );
    }

    #[test]
    fn rf_age_05_7501_kinds_and_unknown_kinds() {
        for (word, kind) in [
            ("permission", StatusKind::Permission),
            ("question", StatusKind::Question),
            ("auth", StatusKind::Auth),
        ] {
            let bytes = format!("\x1b]7501;state=blocked:kind={word}:app=claude-code\x1b\\");
            assert_eq!(
                scan(bytes.as_bytes()),
                vec![status(
                    StatusApp::ClaudeCode,
                    ProgramState::Blocked,
                    Some(kind),
                    false
                )]
            );
        }
        // A kind a later version adds still means blocked.
        assert_eq!(
            scan(b"\x1b]7501;state=blocked:kind=payment\x1b\\"),
            vec![status(StatusApp::Other, ProgramState::Blocked, None, false)]
        );
    }

    #[test]
    fn rf_age_05_7501_fields_in_any_order_with_base64_values() {
        // Real records from Claude Code, Codex and OpenCode.
        let records: [(&[u8], StatusApp); 3] = [
            (
                b"\x1b]7501;state=blocked:app=claude-code:kind=permission:msg=YXBwcm92ZSBCYXNoOiBjdXJs\x1b\\",
                StatusApp::ClaudeCode,
            ),
            (
                b"\x1b]7501;state=blocked:app=codex:kind=permission\x1b\\",
                StatusApp::Codex,
            ),
            (
                b"\x1b]7501;state=blocked:kind=permission:app=opencode:title=eWllbiBwZXJt:msg=UGVybWlzc2lvbiBuZWVkcyBpbnB1dA==\x1b\\",
                StatusApp::OpenCode,
            ),
        ];
        for (bytes, app) in records {
            assert_eq!(
                scan(bytes),
                vec![status(
                    app,
                    ProgramState::Blocked,
                    Some(StatusKind::Permission),
                    false
                )]
            );
        }
        // State last, after padded base64; the first `state` wins.
        assert_eq!(
            scan(b"\x1b]7501;msg=ZG9uZQ==:title=dA==:state=working:state=done\x07"),
            vec![status(StatusApp::Other, ProgramState::Working, None, false)]
        );
    }

    #[test]
    fn rf_age_05_7501_app_is_one_of_the_known_programs_or_other() {
        for (bytes, app) in [
            (
                &b"\x1b]7501;state=idle:app=opencode\x07"[..],
                StatusApp::OpenCode,
            ),
            (
                b"\x1b]7501;state=idle:app=claude-code\x07",
                StatusApp::ClaudeCode,
            ),
            (b"\x1b]7501;state=idle:app=codex\x07", StatusApp::Codex),
            (b"\x1b]7501;state=idle:app=aider\x07", StatusApp::Other),
            (b"\x1b]7501;state=idle:app=\x07", StatusApp::Other),
            (b"\x1b]7501;state=idle:app=OpenCode\x07", StatusApp::Other),
            (b"\x1b]7501;state=idle\x07", StatusApp::Other),
            // The first `app` wins.
            (
                b"\x1b]7501;app=codex:state=idle:app=opencode\x07",
                StatusApp::Codex,
            ),
        ] {
            assert_eq!(
                scan(bytes),
                vec![status(app, ProgramState::Idle, None, false)],
                "{bytes:?}"
            );
        }
    }

    #[test]
    fn rf_age_05_7501_records_with_an_id_are_marked() {
        assert_eq!(
            scan(b"\x1b]7501;state=working:id=ses_e29191ec0ffe3tC2NzOxg6Pfch:app=opencode\x1b\\"),
            vec![status(
                StatusApp::OpenCode,
                ProgramState::Working,
                None,
                true
            )]
        );
        assert_eq!(
            scan(b"\x1b]7501;state=clear:id=ses_1\x1b\\"),
            vec![status(StatusApp::Other, ProgramState::Clear, None, true)]
        );
    }

    #[test]
    fn rn_02_7501_junk_gives_no_signal() {
        for bytes in [
            &b"\x1b]7501\x07"[..],
            b"\x1b]7501;\x07",
            b"\x1b]7501;state=\x07",
            b"\x1b]7501;state=sleeping:app=x\x07",
            b"\x1b]7501;state\x07",
            b"\x1b]7501;app=codex:kind=permission\x07",
            b"\x1b]7501;STATE=working\x07",
            b"\x1b]7501;state=Working\x07",
            b"\x1b]7501;state=working\x1b[c",
            b"\x1b]7501;state=working\x18",
            b"\x1b]75010;state=working\x07",
        ] {
            assert!(scan(bytes).is_empty(), "{bytes:?}");
        }
    }

    #[test]
    fn rn_02_overlong_7501_record_is_dropped_and_scanning_recovers() {
        let mut bytes = b"\x1b]7501;state=blocked:kind=permission:msg=".to_vec();
        bytes.extend(std::iter::repeat_n(b'A', MAX_PAYLOAD));
        bytes.extend_from_slice(b"\x1b\\\x1b]7501;state=working\x1b\\");
        assert_eq!(
            scan(&bytes),
            vec![status(StatusApp::Other, ProgramState::Working, None, false)]
        );
    }

    #[test]
    fn rf_age_05_7501_query_is_answered_where_it_ends() {
        let mut scanner = OscScanner::new();
        let (signals, replies) =
            scan_with_replies(&mut scanner, b"\x1b[>0q\x1b[?u\x1b]7501;?\x07\x1b[c");
        assert!(signals.is_empty());
        assert_eq!(
            replies,
            vec![OscReply {
                at: 18,
                bytes: STATUS_QUERY_REPLY.to_vec()
            }]
        );
        // ST works too, and so does a query with more after the `?`.
        let (_, replies) = scan_with_replies(&mut scanner, b"\x1b]7501;?\x1b\\x\x1b]7501;?x=1\x07");
        let ends: Vec<usize> = replies.iter().map(|r| r.at).collect();
        assert_eq!(ends, vec![10, 23]);
        // Nothing else is a query.
        let (_, replies) = scan_with_replies(
            &mut scanner,
            b"\x1b]7501;state=idle\x07\x1b]7501\x07\x1b]0;?\x07\x1b]7501;?\x18",
        );
        assert!(replies.is_empty());
    }

    #[test]
    fn rf_age_05_7501_cut_at_any_byte_gives_the_same_records_and_replies() {
        let stream: &[u8] = b"ab\x1b]7501;?\x1b\\\x1b[c\x1b]7501;state=idle:app=claude-code\x1b\\\
            \x1b]7501;state=blocked:id=s1:kind=question\x07\x1b]0;t\x07\
            \x1b]7501;state=error:app=codex:msg=QQ==\x1b\\\x1b]7501;?\x07";
        let mut whole = OscScanner::new();
        let (whole_signals, whole_replies) = scan_with_replies(&mut whole, stream);
        assert_eq!(whole_signals.len(), 4);
        assert_eq!(whole_replies.len(), 2);
        let ends: Vec<usize> = whole_replies.iter().map(|r| r.at).collect();
        for cut in 0..=stream.len() {
            let (a, b) = stream.split_at(cut);
            let mut split = OscScanner::new();
            let (mut signals, first) = scan_with_replies(&mut split, a);
            let (more, second) = scan_with_replies(&mut split, b);
            signals.extend(more);
            let mut split_ends: Vec<usize> = first.iter().map(|r| r.at).collect();
            split_ends.extend(second.iter().map(|r| r.at + cut));
            assert_eq!(signals, whole_signals, "cut at {cut}");
            assert_eq!(split_ends, ends, "cut at {cut}");
        }
    }
}
