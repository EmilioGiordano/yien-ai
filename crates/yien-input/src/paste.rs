//! Paste safety. Pasted newlines are submitted commands: a paste that carries
//! them can run something the user never read.
//!
//! ```
//! use yien_input::paste::{assess, bracket, normalize, Risk};
//!
//! let text = "make\nmake install\n";
//! assert!(matches!(assess(text, false), Risk::WouldExecute { .. }));
//! let bytes = bracket(&normalize(text));
//! assert!(bytes.starts_with(b"\x1b[200~"));
//! ```

/// Bytes above which a paste is worth confirming even without newlines.
pub const LARGE_PASTE: usize = 4096;

/// Whether a paste needs the user's confirmation, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Risk {
    /// Nothing to ask about.
    Safe,
    /// Carries line breaks and the program did not ask for bracketed paste, so
    /// the shell will run each line as it arrives.
    WouldExecute {
        /// Lines the paste would submit.
        lines: usize,
    },
    /// Carries escape or other control characters (anything in C0 except tab,
    /// CR and LF, or in C1), which can drive the terminal or the program
    /// instead of being read as text.
    ControlCharacters {
        /// How many such characters the paste holds.
        count: usize,
    },
    /// Big enough that the user cannot have read all of it.
    Large {
        /// Size of the paste in bytes.
        bytes: usize,
    },
}

/// `bracketed` is whether the running program enabled bracketed paste, which is
/// how it promises to treat the text as data instead of as typed commands.
///
/// ```
/// use yien_input::paste::{assess, Risk};
///
/// assert_eq!(assess("ls", false), Risk::Safe);
/// assert_eq!(assess("a\nb", false), Risk::WouldExecute { lines: 2 });
/// assert_eq!(assess("a\nb", true), Risk::Safe);
/// assert_eq!(assess("a\x1b[201~b", true), Risk::ControlCharacters { count: 1 });
/// ```
pub fn assess(text: &str, bracketed: bool) -> Risk {
    let count = text.chars().filter(|&c| is_unsafe_control(c)).count();
    if count > 0 {
        return Risk::ControlCharacters { count };
    }
    let lines = line_count(text);
    if lines > 1 && !bracketed {
        return Risk::WouldExecute { lines };
    }
    if text.len() > LARGE_PASTE {
        return Risk::Large { bytes: text.len() };
    }
    Risk::Safe
}

/// Lines the text would submit. A single trailing newline still submits the
/// line before it, so it counts.
///
/// ```
/// use yien_input::paste::line_count;
/// assert_eq!(line_count(""), 0);
/// assert_eq!(line_count("a\r\nb\n"), 3);
/// ```
pub fn line_count(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .count()
}

/// Line endings normalised to CR, which is what Enter sends.
///
/// ```
/// assert_eq!(yien_input::paste::normalize("a\r\nb\nc"), "a\rb\rc");
/// ```
pub fn normalize(text: &str) -> String {
    text.replace("\r\n", "\r").replace('\n', "\r")
}

/// Control characters a paste must not carry: C0 except tab, LF and CR, and C1.
fn is_unsafe_control(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{1f}' | '\u{80}'..='\u{9f}') && !matches!(c, '\t' | '\n' | '\r')
}

/// Wraps the text so a program that asked for bracketed paste can tell it apart.
///
/// ESC and the C1 controls (8-bit CSI among them) are dropped from the payload:
/// otherwise a pasted `ESC[201~` ends the bracket early and the rest of the
/// paste runs as typed input.
///
/// ```
/// use yien_input::paste::bracket;
/// assert_eq!(bracket("ls"), b"\x1b[200~ls\x1b[201~");
/// assert_eq!(bracket("a\x1b[201~b"), b"\x1b[200~a[201~b\x1b[201~");
/// ```
pub fn bracket(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 12);
    out.extend_from_slice(b"\x1b[200~");
    let mut buf = [0u8; 4];
    for c in text
        .chars()
        .filter(|&c| c != '\x1b' && !matches!(c, '\u{80}'..='\u{9f}'))
    {
        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    }
    out.extend_from_slice(b"\x1b[201~");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_line_is_safe() {
        assert_eq!(assess("cargo build", false), Risk::Safe);
    }

    #[test]
    fn rf_pan_09_multiline_without_bracketed_would_execute() {
        assert_eq!(
            assess("one\ntwo\nthree", false),
            Risk::WouldExecute { lines: 3 }
        );
    }

    #[test]
    fn rf_pan_09_bracketed_paste_makes_multiline_safe() {
        // The program promised to treat it as data, so nagging would be noise.
        assert_eq!(assess("one\ntwo\nthree", true), Risk::Safe);
    }

    #[test]
    fn rf_pan_09_trailing_newline_still_submits() {
        // "rm -rf /\n" runs on arrival: the newline is the Enter.
        assert!(matches!(
            assess("rm -rf /\n", false),
            Risk::WouldExecute { .. }
        ));
    }

    #[test]
    fn crlf_counts_as_one_break() {
        assert_eq!(line_count("one\r\ntwo"), 2);
        assert_eq!(line_count("one\rtwo"), 2);
        assert_eq!(line_count("one\ntwo"), 2);
    }

    #[test]
    fn empty_text_has_no_lines() {
        assert_eq!(line_count(""), 0);
        assert_eq!(assess("", false), Risk::Safe);
    }

    #[test]
    fn large_single_line_is_flagged() {
        let text = "x".repeat(LARGE_PASTE + 1);
        assert_eq!(
            assess(&text, true),
            Risk::Large {
                bytes: LARGE_PASTE + 1
            }
        );
    }

    #[test]
    fn normalize_maps_every_ending_to_cr() {
        assert_eq!(
            normalize("one\r\ntwo\nthree\rfour"),
            "one\rtwo\rthree\rfour"
        );
    }

    #[test]
    fn bracket_wraps_the_payload() {
        assert_eq!(bracket("hi"), b"\x1b[200~hi\x1b[201~");
    }

    #[test]
    fn rf_pan_09_paste_cannot_close_the_bracket() {
        let payload = "echo hi\x1b[201~rm -rf ~\r\u{9b}201~";
        let bytes = bracket(payload);
        let end = b"\x1b[201~";
        let closes = bytes.windows(end.len()).filter(|w| *w == end).count();
        assert_eq!(closes, 1, "only the closing marker may end the paste");
        assert!(bytes.ends_with(end));
        assert!(!bytes[6..bytes.len() - end.len()].contains(&0x1b));
        assert!(!String::from_utf8(bytes).unwrap().contains('\u{9b}'));
    }

    #[test]
    fn rf_pan_09_assess_flags_embedded_escapes() {
        assert_eq!(
            assess("echo hi\x1b[201~rm -rf ~", true),
            Risk::ControlCharacters { count: 1 }
        );
        assert_eq!(
            assess("a\u{7}b\u{9b}c", false),
            Risk::ControlCharacters { count: 2 }
        );
    }

    #[test]
    fn tab_and_line_breaks_are_not_control_risks() {
        assert_eq!(assess("a\tb", false), Risk::Safe);
        assert_eq!(assess("a\r\nb", true), Risk::Safe);
    }

    #[test]
    fn bracket_keeps_ordinary_text_intact() {
        assert_eq!(bracket("ñ\tok\r"), "\x1b[200~ñ\tok\r\x1b[201~".as_bytes());
    }
}
