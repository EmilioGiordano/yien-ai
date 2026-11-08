//! The palettes that resolve terminal colour indices to the RGB colours a
//! frame carries: sumi (dark, the default) and washi (light).

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
    /// The light terminal theme that goes with washi (paper): the prototype's
    /// light `--term-*` tokens in the roles sumi gives the dark ones, with
    /// ANSI colours dark enough to read on its background (normal ones at
    /// 4.5:1, bright ones at 3:1).
    ///
    /// ```
    /// use yien_vt::{Palette, Rgb};
    ///
    /// let washi = Palette::washi();
    /// assert_eq!(washi.background, Rgb::from_hex(0xfbfaf6));
    /// assert_ne!(washi, Palette::default());
    /// ```
    pub const fn washi() -> Self {
        Self {
            ansi: [
                Rgb::from_hex(0xf1efe7), // black: --term-raised
                Rgb::from_hex(0xd11f28), // red: --term-shu
                Rgb::from_hex(0x3e6b24), // green
                Rgb::from_hex(0x8a5a12), // yellow: --term-ochre
                Rgb::from_hex(0x2f4f8f), // blue: --term-ai
                Rgb::from_hex(0x8a3d7c), // magenta
                Rgb::from_hex(0x1f6b66), // cyan
                Rgb::from_hex(0x5c5952), // white: --term-muted
                Rgb::from_hex(0x6f6c64), // bright black: --term-faint
                Rgb::from_hex(0xd9444a), // bright red
                Rgb::from_hex(0x5a8a34), // bright green
                Rgb::from_hex(0xa86f12), // bright yellow
                Rgb::from_hex(0x4a6cb5), // bright blue
                Rgb::from_hex(0xa8559a), // bright magenta
                Rgb::from_hex(0x2f8781), // bright cyan
                Rgb::from_hex(0x23211e), // bright white: --term-text
            ],
            foreground: Rgb::from_hex(0x23211e),
            background: Rgb::from_hex(0xfbfaf6),
            cursor: Rgb::from_hex(0x23211e),
        }
    }

    /// Whether the background is lighter than the text.
    ///
    /// ```
    /// use yien_vt::Palette;
    ///
    /// assert!(Palette::washi().is_light());
    /// assert!(!Palette::default().is_light());
    /// ```
    pub fn is_light(&self) -> bool {
        let luma = |c: Rgb| 299 * u32::from(c.r) + 587 * u32::from(c.g) + 114 * u32::from(c.b);
        luma(self.background) > luma(self.foreground)
    }

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

    #[test]
    fn sumi_is_unchanged() {
        let hex = |p: &Palette| {
            let mut all: Vec<u32> = p.ansi.iter().map(to_hex).collect();
            all.extend([
                to_hex(&p.foreground),
                to_hex(&p.background),
                to_hex(&p.cursor),
            ]);
            all
        };
        assert_eq!(
            hex(&Palette::default()),
            [
                0x1c1a17, 0xe8333c, 0x8fb573, 0xd6a24c, 0x8fa8dc, 0xc08fb5, 0x7fb4b0, 0xa19d93,
                0x7d796f, 0xf0605f, 0xa9c98f, 0xe6bc6e, 0xadc0e8, 0xd4a9cb, 0x9fcbc7, 0xe9e6dd,
                0xe9e6dd, 0x0d0c0b, 0xe9e6dd,
            ]
        );
    }

    #[test]
    fn washi_follows_the_light_prototype_tokens() {
        let p = Palette::washi();
        assert_eq!(p.background, Rgb::from_hex(0xfbfaf6), "--term-bg");
        assert_eq!(p.foreground, Rgb::from_hex(0x23211e), "--term-text");
        assert_eq!(p.ansi[1], Rgb::from_hex(0xd11f28), "--term-shu");
        assert_eq!(p.ansi[3], Rgb::from_hex(0x8a5a12), "--term-ochre");
        assert_eq!(p.ansi[4], Rgb::from_hex(0x2f4f8f), "--term-ai");
        assert_eq!(p.ansi[0], Rgb::from_hex(0xf1efe7), "--term-raised");
        assert_eq!(p.ansi[7], Rgb::from_hex(0x5c5952), "--term-muted");
        assert_eq!(p.ansi[8], Rgb::from_hex(0x6f6c64), "--term-faint");
        assert_eq!(p.ansi[15], p.foreground, "--term-text");
    }

    #[test]
    fn rnf_18_default_text_meets_aa_in_both_palettes() {
        for (name, p) in [("sumi", Palette::default()), ("washi", Palette::washi())] {
            let ratio = contrast(p.foreground, p.background);
            assert!(ratio >= 4.5, "{name}: text is {ratio:.2}:1");
            let ratio = contrast(p.cursor, p.background);
            assert!(ratio >= 3.0, "{name}: cursor is {ratio:.2}:1");
        }
    }

    #[test]
    fn rnf_18_ansi_colours_read_on_their_background() {
        // Black is the raised surface in both, as in the prototype: the
        // Windows console paints black as its background and draws inverse
        // text as black on white, so black is not text.
        for (name, p) in [("sumi", Palette::default()), ("washi", Palette::washi())] {
            for (i, &colour) in p.ansi.iter().enumerate().skip(1) {
                let ratio = contrast(colour, p.background);
                assert!(ratio >= 3.0, "{name}: ANSI {i} is {ratio:.2}:1");
            }
            let ratio = contrast(p.ansi[0], p.ansi[7]);
            assert!(ratio >= 4.5, "{name}: black on white is {ratio:.2}:1");
        }
        let washi = Palette::washi();
        for (i, &colour) in washi.ansi.iter().enumerate().take(8).skip(1) {
            let ratio = contrast(colour, washi.background);
            assert!(ratio >= 4.5, "washi: normal ANSI {i} is {ratio:.2}:1");
        }
    }

    fn to_hex(c: &Rgb) -> u32 {
        u32::from(c.r) << 16 | u32::from(c.g) << 8 | u32::from(c.b)
    }

    fn luminance(c: Rgb) -> f64 {
        let channel = |v: u8| {
            let v = f64::from(v) / 255.0;
            if v <= 0.039_28 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(c.r) + 0.7152 * channel(c.g) + 0.0722 * channel(c.b)
    }

    fn contrast(a: Rgb, b: Rgb) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }
}
