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
}

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
        let mut rest = bytes;
        while let Some((&byte, tail)) = rest.split_first() {
            if self.state == State::Ground {
                // Fast path: plain output is most of the stream.
                match rest.iter().position(|&b| b == ESC) {
                    Some(at) => {
                        self.state = State::Escape;
                        rest = rest.get(at + 1..).unwrap_or_default();
                    }
                    None => return,
                }
                continue;
            }
            self.step(byte, out);
            rest = tail;
        }
    }

    fn step(&mut self, byte: u8, out: &mut Vec<OscSignal>) {
        match self.state {
            State::Ground => {
                if byte == ESC {
                    self.state = State::Escape;
                }
            }
            State::Escape => self.after_escape(byte),
            State::Osc => match byte {
                BEL => self.finish(out),
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
                    self.finish(out);
                } else {
                    // An ESC that is not ST cancels the sequence and starts a new one.
                    self.payload.clear();
                    self.after_escape(byte);
                }
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

    fn finish(&mut self, out: &mut Vec<OscSignal>) {
        if let Some(signal) = parse(&self.payload) {
            out.push(signal);
        }
        self.abort();
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
        _ => None,
    }
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
}
