//! win32-input-mode: a ConPTY extension that carries a full Win32 key event
//! instead of VT bytes, so combinations the VT stream cannot express
//! (Ctrl+Backspace, Ctrl+Shift+C) reach the shell intact.
//!
//! Spec: microsoft/terminal `doc/specs/#4999`. Pure formatting: the caller
//! fills a [`KeyEvent`] and writes the bytes to the PTY.
//!
//! ```
//! use yien_input::Mods;
//! use yien_input::win32::{KeyEvent, encode, vk};
//!
//! let ev = KeyEvent::down(vk::BACK, 0, Mods::CTRL);
//! assert_eq!(encode(&ev), b"\x1b[8;0;0;1;8;1_");
//! ```

use crate::Mods;

/// DECSET 9001: asks the terminal to send win32-input-mode sequences.
pub const ENABLE: &[u8] = b"\x1b[?9001h";
/// DECRST 9001: back to plain VT input.
pub const DISABLE: &[u8] = b"\x1b[?9001l";

/// Bits of the Win32 `dwControlKeyState` field.
pub mod control_key_state {
    /// `RIGHT_ALT_PRESSED`.
    pub const RIGHT_ALT: u32 = 0x0001;
    /// `LEFT_ALT_PRESSED`.
    pub const LEFT_ALT: u32 = 0x0002;
    /// `RIGHT_CTRL_PRESSED`.
    pub const RIGHT_CTRL: u32 = 0x0004;
    /// `LEFT_CTRL_PRESSED`.
    pub const LEFT_CTRL: u32 = 0x0008;
    /// `SHIFT_PRESSED`.
    pub const SHIFT: u32 = 0x0010;
    /// `ENHANCED_KEY`: arrows, Insert, Delete and the like on the extended block.
    pub const ENHANCED_KEY: u32 = 0x0100;
}

/// Win32 virtual-key codes for keys that are not plain letters or digits.
///
/// Letters and digits use their uppercase ASCII code (`b'A' as u16`).
pub mod vk {
    /// `VK_BACK`.
    pub const BACK: u16 = 0x08;
    /// `VK_TAB`.
    pub const TAB: u16 = 0x09;
    /// `VK_RETURN`.
    pub const RETURN: u16 = 0x0D;
    /// `VK_ESCAPE`.
    pub const ESCAPE: u16 = 0x1B;
    /// `VK_SPACE`.
    pub const SPACE: u16 = 0x20;
    /// `VK_PRIOR` (Page Up).
    pub const PRIOR: u16 = 0x21;
    /// `VK_NEXT` (Page Down).
    pub const NEXT: u16 = 0x22;
    /// `VK_END`.
    pub const END: u16 = 0x23;
    /// `VK_HOME`.
    pub const HOME: u16 = 0x24;
    /// `VK_LEFT`.
    pub const LEFT: u16 = 0x25;
    /// `VK_UP`.
    pub const UP: u16 = 0x26;
    /// `VK_RIGHT`.
    pub const RIGHT: u16 = 0x27;
    /// `VK_DOWN`.
    pub const DOWN: u16 = 0x28;
    /// `VK_INSERT`.
    pub const INSERT: u16 = 0x2D;
    /// `VK_DELETE`.
    pub const DELETE: u16 = 0x2E;
}

/// One Win32 key event, the fields of `KEY_EVENT_RECORD`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    /// Virtual-key code (`Vk`).
    pub virtual_key: u16,
    /// Hardware scan code (`Sc`); 0 when unknown.
    pub scan_code: u16,
    /// The UTF-16 code unit the key produces (`Uc`); 0 for none.
    pub unicode: u16,
    /// Key down (`Kd` = 1) or key up (`Kd` = 0).
    pub down: bool,
    /// [`control_key_state`] bits (`Cs`).
    pub control_key_state: u32,
    /// Repeat count (`Rc`).
    pub repeat: u16,
}

impl KeyEvent {
    /// A key-down event with no scan code and a repeat count of 1.
    ///
    /// ```
    /// use yien_input::Mods;
    /// use yien_input::win32::{KeyEvent, control_key_state};
    ///
    /// let ev = KeyEvent::down(b'C' as u16, 0, Mods::CTRL);
    /// assert!(ev.down);
    /// assert_eq!(ev.control_key_state, control_key_state::LEFT_CTRL);
    /// ```
    pub fn down(virtual_key: u16, unicode: u16, mods: Mods) -> Self {
        Self {
            virtual_key,
            scan_code: 0,
            unicode,
            down: true,
            control_key_state: mods.to_control_key_state(),
            repeat: 1,
        }
    }

    /// The same event released.
    ///
    /// ```
    /// use yien_input::Mods;
    /// use yien_input::win32::{KeyEvent, vk};
    ///
    /// assert!(!KeyEvent::down(vk::RETURN, 13, Mods::NONE).up().down);
    /// ```
    pub fn up(self) -> Self {
        Self {
            down: false,
            ..self
        }
    }
}

impl Mods {
    /// The [`control_key_state`] bits for these modifiers, using the left-hand keys.
    ///
    /// ```
    /// use yien_input::Mods;
    /// use yien_input::win32::control_key_state;
    ///
    /// assert_eq!(Mods::CTRL.to_control_key_state(), control_key_state::LEFT_CTRL);
    /// assert_eq!(Mods::NONE.to_control_key_state(), 0);
    /// ```
    pub fn to_control_key_state(self) -> u32 {
        use control_key_state as c;
        let mut state = 0;
        if self.ctrl {
            state |= c::LEFT_CTRL;
        }
        if self.alt {
            state |= c::LEFT_ALT;
        }
        if self.shift {
            state |= c::SHIFT;
        }
        state
    }
}

/// Encodes one event as `ESC [ Vk ; Sc ; Uc ; Kd ; Cs ; Rc _`.
///
/// ```
/// use yien_input::Mods;
/// use yien_input::win32::{KeyEvent, encode};
///
/// let ev = KeyEvent::down(b'A' as u16, 'a' as u16, Mods::NONE);
/// assert_eq!(encode(&ev), b"\x1b[65;0;97;1;0;1_");
/// ```
pub fn encode(ev: &KeyEvent) -> Vec<u8> {
    format!(
        "\x1b[{};{};{};{};{};{}_",
        ev.virtual_key,
        ev.scan_code,
        ev.unicode,
        u8::from(ev.down),
        ev.control_key_state,
        ev.repeat
    )
    .into_bytes()
}

/// A full press: the key-down sequence followed by the key-up one.
///
/// ```
/// use yien_input::Mods;
/// use yien_input::win32::{KeyEvent, press, vk};
///
/// let ev = KeyEvent::down(vk::RETURN, 13, Mods::NONE);
/// assert_eq!(press(&ev), b"\x1b[13;0;13;1;0;1_\x1b[13;0;13;0;0;1_");
/// ```
pub fn press(ev: &KeyEvent) -> Vec<u8> {
    let mut out = encode(ev);
    out.extend_from_slice(&encode(&ev.up()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_letter() {
        let ev = KeyEvent::down(b'A' as u16, 'a' as u16, Mods::NONE);
        assert_eq!(encode(&ev), b"\x1b[65;0;97;1;0;1_");
    }

    #[test]
    fn ctrl_backspace_carries_the_modifier() {
        let ev = KeyEvent::down(vk::BACK, 0, Mods::CTRL);
        // Cs = 8 = LEFT_CTRL_PRESSED, which the VT stream cannot express at all.
        assert_eq!(encode(&ev), b"\x1b[8;0;0;1;8;1_");
    }

    #[test]
    fn ctrl_backspace_differs_from_ctrl_h() {
        let bs = KeyEvent::down(vk::BACK, 0, Mods::CTRL);
        let h = KeyEvent::down(b'H' as u16, 0, Mods::CTRL);
        assert_ne!(encode(&bs), encode(&h));
    }

    #[test]
    fn ctrl_shift_c_is_distinct_from_ctrl_c() {
        let mods = Mods {
            ctrl: true,
            shift: true,
            alt: false,
        };
        let ctrl_shift_c = KeyEvent::down(b'C' as u16, 0, mods);
        let ctrl_c = KeyEvent::down(b'C' as u16, 0, Mods::CTRL);
        assert_ne!(encode(&ctrl_shift_c), encode(&ctrl_c));
    }

    #[test]
    fn key_up_flips_only_kd() {
        let down = KeyEvent::down(vk::RETURN, 13, Mods::NONE);
        assert_eq!(encode(&down.up()), b"\x1b[13;0;13;0;0;1_");
    }

    #[test]
    fn press_emits_down_then_up() {
        let ev = KeyEvent::down(vk::SPACE, 32, Mods::NONE);
        let mut expected = encode(&ev);
        expected.extend_from_slice(&encode(&ev.up()));
        assert_eq!(press(&ev), expected);
    }

    #[test]
    fn control_key_state_combines() {
        use control_key_state as c;
        let mods = Mods {
            ctrl: true,
            alt: true,
            shift: true,
        };
        assert_eq!(
            mods.to_control_key_state(),
            c::LEFT_CTRL | c::LEFT_ALT | c::SHIFT
        );
    }
}
