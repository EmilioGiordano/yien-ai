//! Mouse reports for programs that ask for the mouse (RF-PAN-14): which
//! events the program wants, and how each is encoded.
//!
//! ```
//! use yien_input::Mods;
//! use yien_input::mouse::{report, Action, Button, Encoding, MouseEvent, Tracking};
//!
//! let click = MouseEvent { button: Button::Left, action: Action::Press, col: 0, row: 0, mods: Mods::NONE };
//! assert_eq!(report(&click, Tracking::Click, Encoding::Sgr), Some(b"\x1b[<0;1;1M".to_vec()));
//! assert_eq!(report(&click, Tracking::Off, Encoding::Sgr), None);
//! ```

use crate::Mods;

/// Which events the program asked for (modes 1000, 1002, 1003).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tracking {
    /// None: the mouse belongs to the terminal.
    #[default]
    Off,
    /// Presses, releases and the wheel.
    Click,
    /// Also motion while a button is held.
    Drag,
    /// Also motion with no button held.
    Motion,
}

/// How reports are encoded (modes 1005 and 1006).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Encoding {
    /// X10 bytes; coordinates past 222 cannot be reported.
    #[default]
    Default,
    /// Coordinates as UTF-8 characters, up to 2014.
    Utf8,
    /// `ESC [ < b ; x ; y M/m`, with no coordinate limit.
    Sgr,
}

/// The button involved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    /// The primary button.
    Left,
    /// The wheel button.
    Middle,
    /// The secondary button.
    Right,
    /// One wheel notch away from the user.
    WheelUp,
    /// One wheel notch towards the user.
    WheelDown,
    /// No button: plain motion.
    None,
}

/// What happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// A button went down, or the wheel turned.
    Press,
    /// A button came up.
    Release,
    /// The pointer moved to another cell.
    Motion,
}

/// One mouse event, at a zero-based cell of the viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    /// The button.
    pub button: Button,
    /// What it did.
    pub action: Action,
    /// Column, from 0.
    pub col: u16,
    /// Row, from 0.
    pub row: u16,
    /// Modifiers held.
    pub mods: Mods,
}

/// Whether a program in `tracking` wants to hear about `event`.
///
/// ```
/// use yien_input::Mods;
/// use yien_input::mouse::{wants, Action, Button, MouseEvent, Tracking};
///
/// let drag = MouseEvent { button: Button::Left, action: Action::Motion, col: 3, row: 1, mods: Mods::NONE };
/// assert!(!wants(Tracking::Click, &drag));
/// assert!(wants(Tracking::Drag, &drag));
/// ```
pub fn wants(tracking: Tracking, event: &MouseEvent) -> bool {
    match (tracking, event.action) {
        (Tracking::Off, _) => false,
        (_, Action::Press | Action::Release) => event.button != Button::None,
        (Tracking::Click, Action::Motion) => false,
        (Tracking::Drag, Action::Motion) => event.button != Button::None,
        (Tracking::Motion, Action::Motion) => true,
    }
}

/// The bytes reporting `event`, or `None` when the program did not ask for
/// it or the encoding cannot carry its position.
pub fn report(event: &MouseEvent, tracking: Tracking, encoding: Encoding) -> Option<Vec<u8>> {
    if !wants(tracking, event) {
        return None;
    }
    let base: u32 = match event.button {
        Button::Left => 0,
        Button::Middle => 1,
        Button::Right => 2,
        Button::None => 3,
        Button::WheelUp => 64,
        Button::WheelDown => 65,
    };
    let wheel = matches!(event.button, Button::WheelUp | Button::WheelDown);
    if wheel && event.action == Action::Release {
        return None;
    }
    let mut code = base;
    if event.action == Action::Motion {
        code += 32;
    }
    code += 4 * u32::from(event.mods.shift) + 8 * u32::from(event.mods.alt);
    code += 16 * u32::from(event.mods.ctrl);
    let x = u32::from(event.col) + 1;
    let y = u32::from(event.row) + 1;
    match encoding {
        Encoding::Sgr => {
            let end = if event.action == Action::Release {
                'm'
            } else {
                'M'
            };
            Some(format!("\x1b[<{code};{x};{y}{end}").into_bytes())
        }
        Encoding::Default | Encoding::Utf8 => {
            // Legacy encodings cannot say which button came up.
            let code = if event.action == Action::Release {
                code - base + 3
            } else {
                code
            };
            let mut out = vec![0x1b, b'[', b'M'];
            push_legacy(&mut out, 32 + code, Encoding::Default)?;
            push_legacy(&mut out, 32 + x, encoding)?;
            push_legacy(&mut out, 32 + y, encoding)?;
            Some(out)
        }
    }
}

fn push_legacy(out: &mut Vec<u8>, value: u32, encoding: Encoding) -> Option<()> {
    match encoding {
        Encoding::Utf8 if value > 127 => {
            let c = char::from_u32(value).filter(|_| value <= 2047)?;
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
        _ => out.push(u8::try_from(value).ok()?),
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(button: Button, action: Action, col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            button,
            action,
            col,
            row,
            mods: Mods::NONE,
        }
    }

    #[test]
    fn rf_pan_14_sgr_press_and_release_name_the_button() {
        let press = ev(Button::Right, Action::Press, 9, 4);
        let release = ev(Button::Right, Action::Release, 9, 4);
        assert_eq!(
            report(&press, Tracking::Click, Encoding::Sgr).unwrap(),
            b"\x1b[<2;10;5M"
        );
        assert_eq!(
            report(&release, Tracking::Click, Encoding::Sgr).unwrap(),
            b"\x1b[<2;10;5m"
        );
    }

    #[test]
    fn rf_pan_14_legacy_release_is_button_three() {
        let release = ev(Button::Left, Action::Release, 0, 0);
        assert_eq!(
            report(&release, Tracking::Click, Encoding::Default).unwrap(),
            vec![0x1b, b'[', b'M', 32 + 3, 33, 33]
        );
    }

    #[test]
    fn rf_pan_14_wheel_is_64_and_65_and_never_released() {
        let up = ev(Button::WheelUp, Action::Press, 1, 1);
        let down = ev(Button::WheelDown, Action::Press, 1, 1);
        assert_eq!(
            report(&up, Tracking::Click, Encoding::Sgr).unwrap(),
            b"\x1b[<64;2;2M"
        );
        assert_eq!(
            report(&down, Tracking::Click, Encoding::Sgr).unwrap(),
            b"\x1b[<65;2;2M"
        );
        let release = ev(Button::WheelUp, Action::Release, 1, 1);
        assert_eq!(report(&release, Tracking::Click, Encoding::Sgr), None);
    }

    #[test]
    fn motion_adds_32_and_follows_the_tracking_mode() {
        let drag = ev(Button::Left, Action::Motion, 2, 0);
        let hover = ev(Button::None, Action::Motion, 2, 0);
        assert_eq!(report(&drag, Tracking::Click, Encoding::Sgr), None);
        assert_eq!(
            report(&drag, Tracking::Drag, Encoding::Sgr).unwrap(),
            b"\x1b[<32;3;1M"
        );
        assert_eq!(report(&hover, Tracking::Drag, Encoding::Sgr), None);
        assert_eq!(
            report(&hover, Tracking::Motion, Encoding::Sgr).unwrap(),
            b"\x1b[<35;3;1M"
        );
    }

    #[test]
    fn modifiers_add_their_bits() {
        let mut click = ev(Button::Left, Action::Press, 0, 0);
        click.mods = Mods {
            ctrl: true,
            alt: true,
            shift: true,
        };
        assert_eq!(
            report(&click, Tracking::Click, Encoding::Sgr).unwrap(),
            b"\x1b[<28;1;1M"
        );
    }

    #[test]
    fn legacy_cannot_report_far_cells_but_utf8_can() {
        let far = ev(Button::Left, Action::Press, 300, 0);
        assert_eq!(report(&far, Tracking::Click, Encoding::Default), None);
        let bytes = report(&far, Tracking::Click, Encoding::Utf8).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(text.chars().nth(4), char::from_u32(32 + 301));
    }

    #[test]
    fn off_reports_nothing() {
        let click = ev(Button::Left, Action::Press, 0, 0);
        assert!(!wants(Tracking::Off, &click));
        assert_eq!(report(&click, Tracking::Off, Encoding::Sgr), None);
    }
}
