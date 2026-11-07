//! The palette that resolves terminal colour indices to the RGB colours a
//! frame carries.

pub use yien_proto::frame::Rgb;

/// The colours a frame is resolved with: the 16 ANSI colours, the default
/// foreground and background, and the cursor. Indices 16 to 255 follow the
/// standard xterm cube and grey ramp.
///
/// ```
/// use yien_vt::{Palette, Rgb};
///
/// let palette = Palette::default();
/// assert_eq!(palette.indexed(1), palette.ansi[1]);
/// assert_eq!(palette.indexed(196), Rgb::new(255, 0, 0));
/// assert_eq!(palette.indexed(232), Rgb::new(8, 8, 8));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// Black, red, green, yellow, blue, magenta, cyan, white, then the bright
    /// versions in the same order.
    pub ansi: [Rgb; 16],
    /// The default text colour.
    pub foreground: Rgb,
    /// The default background colour.
    pub background: Rgb,
    /// The cursor colour.
    pub cursor: Rgb,
}

impl Default for Palette {
    /// The dark terminal theme of the prototype (`--term-*` tokens).
    fn default() -> Self {
        Self {
            ansi: [
                Rgb::from_hex(0x1c1a17), // black: --term-raised
                Rgb::from_hex(0xe8333c), // red: --term-shu
                Rgb::from_hex(0x8fb573), // green
                Rgb::from_hex(0xd6a24c), // yellow: --term-ochre
                Rgb::from_hex(0x8fa8dc), // blue: --term-ai
                Rgb::from_hex(0xc08fb5), // magenta
                Rgb::from_hex(0x7fb4b0), // cyan
                Rgb::from_hex(0xa19d93), // white: --term-muted
                Rgb::from_hex(0x7d796f), // bright black: --term-faint
                Rgb::from_hex(0xf0605f), // bright red
                Rgb::from_hex(0xa9c98f), // bright green
                Rgb::from_hex(0xe6bc6e), // bright yellow
                Rgb::from_hex(0xadc0e8), // bright blue
                Rgb::from_hex(0xd4a9cb), // bright magenta
                Rgb::from_hex(0x9fcbc7), // bright cyan
                Rgb::from_hex(0xe9e6dd), // bright white: --term-text
            ],
            foreground: Rgb::from_hex(0xe9e6dd),
            background: Rgb::from_hex(0x0d0c0b),
            cursor: Rgb::from_hex(0xe9e6dd),
        }
    }
}

impl Palette {
    /// The colour of a 256-colour index (SGR 38;5 and 48;5).
    pub fn indexed(&self, index: u8) -> Rgb {
        const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
        let level = |i: u8| LEVELS.get(usize::from(i)).copied().unwrap_or(255);
        match index {
            0..16 => self
                .ansi
                .get(usize::from(index))
                .copied()
                .unwrap_or(self.foreground),
            16..232 => {
                let i = index - 16;
                Rgb::new(level(i / 36), level(i / 6 % 6), level(i % 6))
            }
            232..=255 => {
                let grey = 8 + (index - 232) * 10;
                Rgb::new(grey, grey, grey)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cube_corners() {
        let p = Palette::default();
        assert_eq!(p.indexed(16), Rgb::new(0, 0, 0));
        assert_eq!(p.indexed(21), Rgb::new(0, 0, 255));
        assert_eq!(p.indexed(231), Rgb::new(255, 255, 255));
        assert_eq!(p.indexed(255), Rgb::new(238, 238, 238));
    }

    #[test]
    fn default_follows_prototype_tokens() {
        let p = Palette::default();
        assert_eq!(p.background, Rgb::from_hex(0x0d0c0b));
        assert_eq!(p.foreground, Rgb::from_hex(0xe9e6dd));
        assert_eq!(p.ansi[1], Rgb::from_hex(0xe8333c));
    }
}
