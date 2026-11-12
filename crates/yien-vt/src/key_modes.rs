//! A streaming tracker for the keyboard modes the emulator does not keep:
//! win32-input-mode (DECSET 9001, which ConPTY asks for) and the kitty
//! keyboard protocol flags, with one stack for each screen.
//!
//! Like [`crate::osc_scan`], it keeps its state between calls, so a sequence
//! cut at any byte gives the same result as the whole sequence.
//!
//! ```
//! use yien_vt::key_modes::KeyModeTracker;
//!
//! let mut tracker = KeyModeTracker::new();
//! tracker.scan(b"\x1b[?90");
//! tracker.scan(b"01h\x1b[>1u");
//! assert!(tracker.win32_input());
//! assert_eq!(tracker.kitty_flags(), 1);
//! ```

/// The deepest kitty flag stack kept; a push beyond it drops the oldest entry.
/// The same depth ConPTY's own input side keeps.
pub const KITTY_STACK_MAX: usize = 8;

/// The longest parameter string kept; a longer sequence is ignored whole.
const MAX_PARAMS: usize = 64;

/// The five flags the kitty keyboard protocol defines.
const KITTY_ALL_FLAGS: u8 = 0b1_1111;

const ESC: u8 = 0x1b;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    Csi,
}

/// A reply the program asked for (`CSI ? u`), to be written to the PTY after
/// the bytes before `at` are processed, so it keeps its place among the
/// emulator's own replies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyModeReply {
    /// The index just past the query's last byte in the scanned slice.
    pub at: usize,
    /// The bytes to write.
    pub bytes: Vec<u8>,
}

/// The keyboard mode state of one PTY stream.
///
/// ```
/// use yien_vt::key_modes::KeyModeTracker;
///
/// let mut tracker = KeyModeTracker::new();
/// let replies = tracker.scan(b"\x1b[>5u\x1b[?u");
/// assert_eq!(replies[0].bytes, b"\x1b[?5u");
/// assert_eq!(replies[0].at, 9);
/// tracker.scan(b"\x1bc");
/// assert_eq!(tracker.kitty_flags(), 0);
/// ```
#[derive(Debug, Clone)]
pub struct KeyModeTracker {
    state: State,
    params: Vec<u8>,
    ignore: bool,
    win32_input: bool,
    alt_screen: bool,
    main_stack: Vec<u8>,
    alt_stack: Vec<u8>,
}

impl Default for KeyModeTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyModeTracker {
    /// A tracker with every mode off.
    pub fn new() -> Self {
        Self {
            state: State::Ground,
            params: Vec::new(),
            ignore: false,
            win32_input: false,
            alt_screen: false,
            main_stack: Vec::new(),
            alt_stack: Vec::new(),
        }
    }

    /// Whether the program asked for win32-input-mode (DECSET 9001).
    pub fn win32_input(&self) -> bool {
        self.win32_input
    }

    /// The kitty keyboard protocol flags active on the current screen.
    pub fn kitty_flags(&self) -> u8 {
        self.stack().last().copied().unwrap_or(0)
    }

    /// Scans `bytes`, updates the modes and returns the replies owed, in order.
    pub fn scan(&mut self, bytes: &[u8]) -> Vec<KeyModeReply> {
        let mut replies = Vec::new();
        let mut at = 0;
        while let Some(rest) = bytes.get(at..) {
            if rest.is_empty() {
                break;
            }
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
            let Some(&byte) = rest.first() else {
                break;
            };
            at += 1;
            if let Some(reply) = self.step(byte) {
                replies.push(KeyModeReply { at, bytes: reply });
            }
        }
        replies
    }

    fn step(&mut self, byte: u8) -> Option<Vec<u8>> {
        match self.state {
            State::Ground => {
                if byte == ESC {
                    self.state = State::Escape;
                }
            }
            State::Escape => match byte {
                b'[' => {
                    self.params.clear();
                    self.ignore = false;
                    self.state = State::Csi;
                }
                b'c' => {
                    self.reset();
                    self.state = State::Ground;
                }
                ESC => {}
                _ => self.state = State::Ground,
            },
            State::Csi => match byte {
                ESC => self.state = State::Escape,
                CAN | SUB => self.state = State::Ground,
                0x30..=0x3f => {
                    // A private marker is only valid as the first byte.
                    if matches!(byte, b'<'..=b'?') && !self.params.is_empty() {
                        self.ignore = true;
                    }
                    if self.params.len() < MAX_PARAMS {
                        self.params.push(byte);
                    } else {
                        self.ignore = true;
                    }
                }
                // Intermediates belong to sequences this tracker does not handle.
                0x20..=0x2f => self.ignore = true,
                0x40..=0x7e => {
                    self.state = State::Ground;
                    if !self.ignore {
                        return self.dispatch(byte);
                    }
                }
                // C0 controls inside a sequence are executed and DEL is ignored.
                0x00..=0x1f | 0x7f => {}
                _ => self.state = State::Ground,
            },
        }
        None
    }

    fn dispatch(&mut self, final_byte: u8) -> Option<Vec<u8>> {
        let (marker, params) = match self.params.split_first() {
            Some((&m, rest)) if matches!(m, b'<'..=b'?') => (Some(m), rest.to_vec()),
            _ => (None, self.params.clone()),
        };
        let values = parse_params(&params);
        let first = values.first().copied().flatten();
        match (marker, final_byte) {
            (Some(b'?'), b'h' | b'l') => {
                let on = final_byte == b'h';
                for value in values.into_iter().flatten() {
                    match value {
                        9001 => self.win32_input = on,
                        47 | 1047 | 1049 => self.set_alt_screen(on),
                        _ => {}
                    }
                }
            }
            (Some(b'?'), b'u') if params.is_empty() => {
                return Some(format!("\x1b[?{}u", self.kitty_flags()).into_bytes());
            }
            (Some(b'>'), b'u') => self.push(kitty_flags(first.unwrap_or(0))),
            (Some(b'<'), b'u') => {
                let count = usize::from(first.unwrap_or(1).max(1));
                let stack = self.stack_mut();
                stack.truncate(stack.len().saturating_sub(count));
            }
            (Some(b'='), b'u') => {
                let flags = kitty_flags(first.unwrap_or(0));
                let mode = values.get(1).copied().flatten().unwrap_or(1);
                self.set(flags, mode);
            }
            _ => {}
        }
        None
    }

    fn push(&mut self, flags: u8) {
        let stack = self.stack_mut();
        if stack.len() >= KITTY_STACK_MAX {
            stack.remove(0);
        }
        stack.push(flags);
    }

    fn set(&mut self, flags: u8, mode: u16) {
        let stack = self.stack_mut();
        if stack.is_empty() {
            stack.push(0);
        }
        if let Some(current) = stack.last_mut() {
            match mode {
                1 => *current = flags,
                2 => *current |= flags,
                3 => *current &= !flags,
                _ => {}
            }
        }
    }

    fn set_alt_screen(&mut self, on: bool) {
        if self.alt_screen && !on {
            self.alt_stack.clear();
        }
        self.alt_screen = on;
    }

    fn reset(&mut self) {
        self.win32_input = false;
        self.alt_screen = false;
        self.main_stack.clear();
        self.alt_stack.clear();
    }

    fn stack(&self) -> &[u8] {
        if self.alt_screen {
            &self.alt_stack
        } else {
            &self.main_stack
        }
    }

    fn stack_mut(&mut self) -> &mut Vec<u8> {
        if self.alt_screen {
            &mut self.alt_stack
        } else {
            &mut self.main_stack
        }
    }
}

fn kitty_flags(value: u16) -> u8 {
    u8::try_from(value & u16::from(KITTY_ALL_FLAGS)).unwrap_or(0)
}

/// The `;`-separated parameters, each `None` when empty or not a number and
/// capped at `u16::MAX` like the emulator's parser. A `:` sub-parameter is dropped.
fn parse_params(params: &[u8]) -> Vec<Option<u16>> {
    if params.is_empty() {
        return Vec::new();
    }
    params
        .split(|&b| b == b';')
        .map(|param| {
            let main = param.split(|&b| b == b':').next().unwrap_or_default();
            if main.is_empty() || !main.iter().all(u8::is_ascii_digit) {
                return None;
            }
            Some(main.iter().fold(0u16, |n, &d| {
                n.saturating_mul(10).saturating_add(u16::from(d - b'0'))
            }))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracked(bytes: &[u8]) -> KeyModeTracker {
        let mut tracker = KeyModeTracker::new();
        tracker.scan(bytes);
        tracker
    }

    #[test]
    fn rf_age_02_win32_input_mode_is_set_and_reset() {
        assert!(!KeyModeTracker::new().win32_input());
        assert!(tracked(b"\x1b[?9001h").win32_input());
        assert!(!tracked(b"\x1b[?9001h\x1b[?9001l").win32_input());
        // Among other modes in one sequence.
        assert!(tracked(b"\x1b[?1004;9001h").win32_input());
        assert!(!tracked(b"\x1b[?9001h\x1b[?25;9001l").win32_input());
    }

    #[test]
    fn rf_age_02_ris_turns_win32_input_mode_and_kitty_flags_off() {
        let mut t = tracked(b"\x1b[?9001h\x1b[>1u\x1b[?1049h\x1b[>3u");
        assert_eq!(t.kitty_flags(), 3);
        t.scan(b"\x1bc");
        assert!(!t.win32_input());
        assert_eq!(t.kitty_flags(), 0);
        // RIS also returned to the main screen, whose stack is empty.
        t.scan(b"\x1b[>4u\x1b[?1049l");
        assert_eq!(t.kitty_flags(), 4);
    }

    #[test]
    fn similar_sequences_leave_the_modes_alone() {
        // Not private, a different mode, an intermediate, plain text.
        for bytes in [
            &b"\x1b[9001h"[..],
            b"\x1b[?900h",
            b"\x1b[?90011h",
            b"\x1b[?9001$p",
            b"[?9001h",
            b"\x1b]0;\x1b[?9001\x07",
        ] {
            assert!(!tracked(bytes).win32_input(), "{bytes:?}");
        }
        assert_eq!(tracked(b"\x1b[u\x1b[1u\x1b[>1c").kitty_flags(), 0);
    }

    #[test]
    fn a_sequence_cut_at_any_byte_gives_the_same_modes_and_replies() {
        let stream: &[u8] =
            b"x\x1b[?1004;9001h\x1b[>1u\x1b[?u\x1b[?1049h\x1b[>3u\x1b[=4;2u\x1b[?u\x1b[<u";
        let mut whole = KeyModeTracker::new();
        let whole_replies: Vec<Vec<u8>> = whole.scan(stream).into_iter().map(|r| r.bytes).collect();
        assert_eq!(
            whole_replies,
            vec![b"\x1b[?1u".to_vec(), b"\x1b[?7u".to_vec()]
        );
        for cut in 0..=stream.len() {
            let (a, b) = stream.split_at(cut);
            let mut split = KeyModeTracker::new();
            let mut replies: Vec<Vec<u8>> = split.scan(a).into_iter().map(|r| r.bytes).collect();
            replies.extend(split.scan(b).into_iter().map(|r| r.bytes));
            assert_eq!(replies, whole_replies, "cut at {cut}");
            assert_eq!(split.win32_input(), whole.win32_input(), "cut at {cut}");
            assert_eq!(split.kitty_flags(), whole.kitty_flags(), "cut at {cut}");
        }
    }

    #[test]
    fn rf_age_02_kitty_flags_push_pop_and_set() {
        let mut t = tracked(b"\x1b[>1u");
        assert_eq!(t.kitty_flags(), 1);
        t.scan(b"\x1b[>11u");
        assert_eq!(t.kitty_flags(), 11);
        t.scan(b"\x1b[<u");
        assert_eq!(t.kitty_flags(), 1);
        t.scan(b"\x1b[=2;2u");
        assert_eq!(t.kitty_flags(), 3);
        t.scan(b"\x1b[=1;3u");
        assert_eq!(t.kitty_flags(), 2);
        t.scan(b"\x1b[=5u");
        assert_eq!(t.kitty_flags(), 5);
        // Popping more than the stack holds empties it.
        t.scan(b"\x1b[<9u");
        assert_eq!(t.kitty_flags(), 0);
        t.scan(b"\x1b[<u");
        assert_eq!(t.kitty_flags(), 0);
        // Set on an empty stack starts one; unknown flag bits are dropped.
        t.scan(b"\x1b[=255u");
        assert_eq!(t.kitty_flags(), KITTY_ALL_FLAGS);
        // A push with no flags pushes zero.
        t.scan(b"\x1b[>u");
        assert_eq!(t.kitty_flags(), 0);
    }

    #[test]
    fn the_kitty_stack_drops_its_oldest_entry_when_full() {
        let mut t = KeyModeTracker::new();
        for flags in 1..=u8::try_from(KITTY_STACK_MAX).unwrap() + 2 {
            t.scan(format!("\x1b[>{flags}u").as_bytes());
        }
        assert_eq!(t.main_stack.len(), KITTY_STACK_MAX);
        t.scan(format!("\x1b[<{}u", KITTY_STACK_MAX - 1).as_bytes());
        // Entries 1 and 2 were evicted, so 3 is the oldest left.
        assert_eq!(t.kitty_flags(), 3);
        // A huge pop count is capped, not wrapped, and empties the stack.
        t.scan(b"\x1b[>1u\x1b[>2u\x1b[<99999999999u");
        assert_eq!(t.kitty_flags(), 0);
    }

    #[test]
    fn each_screen_has_its_own_kitty_stack() {
        let mut t = tracked(b"\x1b[>1u\x1b[?1049h");
        assert_eq!(t.kitty_flags(), 0);
        t.scan(b"\x1b[>8u");
        assert_eq!(t.kitty_flags(), 8);
        t.scan(b"\x1b[?1049l");
        assert_eq!(t.kitty_flags(), 1);
        // Leaving the alternate screen cleared its stack.
        t.scan(b"\x1b[?1049h");
        assert_eq!(t.kitty_flags(), 0);
    }

    #[test]
    fn rf_age_02_the_flag_query_is_answered_where_it_was_asked() {
        let mut t = KeyModeTracker::new();
        let replies = t.scan(b"ab\x1b[?u\x1b[>1u\x1b[?u");
        assert_eq!(
            replies,
            vec![
                KeyModeReply {
                    at: 6,
                    bytes: b"\x1b[?0u".to_vec()
                },
                KeyModeReply {
                    at: 15,
                    bytes: b"\x1b[?1u".to_vec()
                },
            ]
        );
        // A query with parameters is not the flag query.
        assert!(t.scan(b"\x1b[?1u").is_empty());
    }

    #[test]
    fn an_overlong_sequence_is_ignored_whole() {
        let mut long = b"\x1b[?".to_vec();
        long.extend(std::iter::repeat_n(b'0', 200));
        long.extend_from_slice(b"9001h");
        assert!(!tracked(&long).win32_input());
        // And the tracker recovers for the next one.
        long.extend_from_slice(b"\x1b[?9001h");
        assert!(tracked(&long).win32_input());
    }
}
