//! Key presses and pasted text to the bytes a terminal program expects.
//!
//! Pure byte encoding: no window, no OS calls. Imported from foothy.
//!
//! ```
//! use yien_input::{encode, Key, Mods};
//!
//! assert_eq!(encode(&Key::Char('c'), Mods::CTRL), Some(vec![0x03]));
//! assert_eq!(encode(&Key::Up, Mods::NONE), Some(b"\x1b[A".to_vec()));
//! ```

#![forbid(unsafe_code)]

pub mod mouse;
pub mod paste;

/// The modifier keys held during a key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods {
    /// Control.
    pub ctrl: bool,
    /// Alt (Option on macOS).
    pub alt: bool,
    /// Shift.
    pub shift: bool,
}

impl Mods {
    /// No modifier held.
    pub const NONE: Self = Self {
        ctrl: false,
        alt: false,
        shift: false,
    };
    /// Control alone.
    pub const CTRL: Self = Self {
        ctrl: true,
        alt: false,
        shift: false,
    };

    fn csi_param(self) -> u8 {
        1 + u8::from(self.shift) + 2 * u8::from(self.alt) + 4 * u8::from(self.ctrl)
    }
}

/// A key the terminal knows how to encode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    /// A key that produces text, already resolved by the keyboard layout.
    Char(char),
    /// Enter / Return.
    Enter,
    /// Backspace.
    Backspace,
    /// Tab; Shift+Tab is back-tab.
    Tab,
    /// Escape.
    Escape,
    /// Delete (forward delete).
    Delete,
    /// Insert.
    Insert,
    /// Home.
    Home,
    /// End.
    End,
    /// Page Up.
    PageUp,
    /// Page Down.
    PageDown,
    /// Arrow up.
    Up,
    /// Arrow down.
    Down,
    /// Arrow right.
    Right,
    /// Arrow left.
    Left,
    /// Function key F1 to F12; anything else encodes to nothing.
    F(u8),
}

fn csi(mods: Mods, final_byte: u8) -> Vec<u8> {
    if mods == Mods::NONE {
        vec![0x1b, b'[', final_byte]
    } else {
        format!("\x1b[1;{}{}", mods.csi_param(), final_byte as char).into_bytes()
    }
}

fn tilde(mods: Mods, number: u8) -> Vec<u8> {
    if mods == Mods::NONE {
        format!("\x1b[{number}~").into_bytes()
    } else {
        format!("\x1b[{number};{}~", mods.csi_param()).into_bytes()
    }
}

/// Translates a key press into the bytes to write to the PTY.
///
/// Returns `None` when the key produces nothing (a bare modifier, an unmapped key).
///
/// ```
/// use yien_input::{encode, Key, Mods};
///
/// assert_eq!(encode(&Key::Enter, Mods::NONE), Some(b"\r".to_vec()));
/// assert_eq!(encode(&Key::Right, Mods::CTRL), Some(b"\x1b[1;5C".to_vec()));
/// assert_eq!(encode(&Key::F(20), Mods::NONE), None);
/// ```
pub fn encode(key: &Key, mods: Mods) -> Option<Vec<u8>> {
    let bytes = match key {
        Key::Char(c) => {
            let mut out = Vec::new();
            if mods.alt {
                out.push(0x1b);
            }
            if mods.ctrl {
                // The C0 range only covers @ through _ ; anything else keeps its literal byte.
                let upper = c.to_ascii_uppercase();
                match upper {
                    '@'..='_' => out.push(upper as u8 & 0x1f),
                    ' ' => out.push(0),
                    '?' => out.push(0x7f),
                    _ => {
                        let mut buf = [0u8; 4];
                        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                    }
                }
            } else {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
            out
        }

        Key::Enter => vec![b'\r'],
        Key::Tab if mods.shift => vec![0x1b, b'[', b'Z'],
        Key::Tab => vec![b'\t'],
        Key::Escape => vec![0x1b],

        // Ctrl+Backspace cannot be expressed in the legacy protocol. 0x17 (Ctrl+W)
        // was tried and is worse than useless: PSReadLine has no Ctrl+W binding, so
        // it inserts a literal ^W into the line. Falling back to DEL at least
        // deletes one character. Word deletion needs win32-input-mode.
        Key::Backspace if mods.ctrl => vec![0x7f],
        Key::Backspace if mods.alt => vec![0x1b, 0x7f],
        Key::Backspace => vec![0x7f],

        Key::Up => csi(mods, b'A'),
        Key::Down => csi(mods, b'B'),
        Key::Right => csi(mods, b'C'),
        Key::Left => csi(mods, b'D'),
        Key::Home => csi(mods, b'H'),
        Key::End => csi(mods, b'F'),

        Key::Insert => tilde(mods, 2),
        Key::Delete => tilde(mods, 3),
        Key::PageUp => tilde(mods, 5),
        Key::PageDown => tilde(mods, 6),

        Key::F(n) => match n {
            1 => vec![0x1b, b'O', b'P'],
            2 => vec![0x1b, b'O', b'Q'],
            3 => vec![0x1b, b'O', b'R'],
            4 => vec![0x1b, b'O', b'S'],
            5 => tilde(mods, 15),
            6..=10 => tilde(mods, 12 + n),
            11 | 12 => tilde(mods, 11 + n),
            _ => return None,
        },
    };
    Some(bytes)
}

/// Like [`encode`], but honours application cursor keys (DECCKM): with the
/// mode on, unmodified arrows, Home and End send `ESC O x` instead of `ESC [ x`.
///
/// ```
/// use yien_input::{encode_in, Key, Mods};
///
/// assert_eq!(encode_in(&Key::Up, Mods::NONE, true), Some(b"\x1bOA".to_vec()));
/// assert_eq!(encode_in(&Key::Up, Mods::NONE, false), Some(b"\x1b[A".to_vec()));
/// assert_eq!(encode_in(&Key::Up, Mods::CTRL, true), Some(b"\x1b[1;5A".to_vec()));
/// ```
pub fn encode_in(key: &Key, mods: Mods, app_cursor: bool) -> Option<Vec<u8>> {
    if app_cursor && mods == Mods::NONE {
        let ss3 = match key {
            Key::Up => Some(b'A'),
            Key::Down => Some(b'B'),
            Key::Right => Some(b'C'),
            Key::Left => Some(b'D'),
            Key::Home => Some(b'H'),
            Key::End => Some(b'F'),
            _ => None,
        };
        if let Some(final_byte) = ss3 {
            return Some(vec![0x1b, b'O', final_byte]);
        }
    }
    encode(key, mods)
}

/// What a terminal program asked to hear when its pane gains or loses the
/// keyboard (mode 1004).
///
/// ```
/// assert_eq!(yien_input::focus_report(true), b"\x1b[I");
/// assert_eq!(yien_input::focus_report(false), b"\x1b[O");
/// ```
pub fn focus_report(focused: bool) -> &'static [u8] {
    if focused { b"\x1b[I" } else { b"\x1b[O" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_cursor_switches_unmodified_arrows_to_ss3() {
        assert_eq!(encode_in(&Key::Left, Mods::NONE, true).unwrap(), b"\x1bOD");
        assert_eq!(encode_in(&Key::Home, Mods::NONE, true).unwrap(), b"\x1bOH");
        assert_eq!(encode_in(&Key::End, Mods::NONE, true).unwrap(), b"\x1bOF");
        assert_eq!(encode_in(&Key::Enter, Mods::NONE, true).unwrap(), b"\r");
        assert_eq!(
            encode_in(&Key::PageUp, Mods::NONE, true).unwrap(),
            b"\x1b[5~"
        );
    }

    #[test]
    fn modified_arrows_ignore_app_cursor() {
        let shift = Mods {
            shift: true,
            ..Mods::NONE
        };
        assert_eq!(encode_in(&Key::Down, shift, true).unwrap(), b"\x1b[1;2B");
    }

    fn enc(key: Key, mods: Mods) -> Vec<u8> {
        encode(&key, mods).unwrap()
    }

    #[test]
    fn plain_letter() {
        assert_eq!(enc(Key::Char('a'), Mods::NONE), b"a");
    }

    #[test]
    fn non_ascii_letter_is_utf8() {
        assert_eq!(enc(Key::Char('ñ'), Mods::NONE), "ñ".as_bytes());
    }

    #[test]
    fn ctrl_c_is_sigint_byte() {
        assert_eq!(enc(Key::Char('c'), Mods::CTRL), vec![0x03]);
    }

    #[test]
    fn ctrl_letters_map_to_c0() {
        assert_eq!(enc(Key::Char('a'), Mods::CTRL), vec![0x01]);
        assert_eq!(enc(Key::Char('h'), Mods::CTRL), vec![0x08]);
        assert_eq!(enc(Key::Char('['), Mods::CTRL), vec![0x1b]);
    }

    #[test]
    fn backspace_sends_del_not_bs() {
        assert_eq!(enc(Key::Backspace, Mods::NONE), vec![0x7f]);
    }

    #[test]
    fn ctrl_backspace_falls_back_to_del_not_ctrl_w() {
        // 0x17 made PSReadLine print a literal ^W. Never send it again.
        let bytes = enc(Key::Backspace, Mods::CTRL);
        assert_ne!(bytes, vec![0x17]);
        assert_eq!(bytes, vec![0x7f]);
    }

    #[test]
    fn alt_prefixes_escape() {
        let alt = Mods {
            alt: true,
            ..Mods::NONE
        };
        assert_eq!(enc(Key::Char('b'), alt), vec![0x1b, b'b']);
    }

    #[test]
    fn arrows_are_csi() {
        assert_eq!(enc(Key::Up, Mods::NONE), b"\x1b[A");
        assert_eq!(enc(Key::Left, Mods::NONE), b"\x1b[D");
    }

    #[test]
    fn modified_arrows_carry_a_param() {
        assert_eq!(enc(Key::Right, Mods::CTRL), b"\x1b[1;5C");
        let shift = Mods {
            shift: true,
            ..Mods::NONE
        };
        assert_eq!(enc(Key::Up, shift), b"\x1b[1;2A");
    }

    #[test]
    fn delete_is_tilde_form() {
        assert_eq!(enc(Key::Delete, Mods::NONE), b"\x1b[3~");
    }

    #[test]
    fn shift_tab_is_back_tab() {
        let shift = Mods {
            shift: true,
            ..Mods::NONE
        };
        assert_eq!(enc(Key::Tab, shift), b"\x1b[Z");
    }

    #[test]
    fn enter_is_carriage_return() {
        assert_eq!(enc(Key::Enter, Mods::NONE), b"\r");
    }

    #[test]
    fn function_keys() {
        assert_eq!(enc(Key::F(1), Mods::NONE), b"\x1bOP");
        assert_eq!(enc(Key::F(5), Mods::NONE), b"\x1b[15~");
        assert_eq!(enc(Key::F(12), Mods::NONE), b"\x1b[23~");
        assert_eq!(encode(&Key::F(20), Mods::NONE), None);
    }
}
