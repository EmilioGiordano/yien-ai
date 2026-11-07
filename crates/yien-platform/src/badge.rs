//! The taskbar badge (RF-ATN-10): how many agents need the user, drawn as a
//! number on a red dot. Pure: the text and the pixels, which [`attention`]
//! hands to the taskbar.
//!
//! [`attention`]: crate::attention
//!
//! ```
//! use yien_platform::badge::{badge_text, render};
//!
//! assert_eq!(badge_text(0), None);
//! assert_eq!(badge_text(3).as_deref(), Some("3"));
//! assert_eq!(badge_text(12).as_deref(), Some("9+"));
//! let pixels = render("3", 16);
//! assert_eq!(pixels.len(), 16 * 16 * 4);
//! ```

/// The badge for `count` agents that need the user: none at 0, the number up
/// to 9, then `9+`.
pub fn badge_text(count: usize) -> Option<String> {
    match count {
        0 => None,
        1..=9 => Some(count.to_string()),
        _ => Some("9+".to_owned()),
    }
}

/// The badge's red, the same shu as the window's "needs you" mark (RN-01).
const RED: [u8; 3] = [0xE0, 0x3E, 0x2E];
const WHITE: [u8; 3] = [0xFF, 0xFF, 0xFF];

const GLYPH_W: usize = 5;
const GLYPH_H: usize = 7;

/// 5x7 glyphs for the characters a badge can hold; one byte per row, the
/// low five bits left to right.
fn glyph(c: char) -> Option<[u8; GLYPH_H]> {
    Some(match c {
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        '+' => [0x00, 0x04, 0x04, 0x1F, 0x04, 0x04, 0x00],
        _ => return None,
    })
}

/// The badge as `size` x `size` BGRA pixels, top row first, alpha not
/// premultiplied: a red dot filling the square with `text` in white at its
/// centre. Characters without a glyph are skipped.
pub fn render(text: &str, size: usize) -> Vec<u8> {
    let mut pixels = vec![0u8; size * size * 4];
    if size == 0 {
        return pixels;
    }
    let radius = size as f32 / 2.0;
    for y in 0..size {
        for x in 0..size {
            let alpha = disc_coverage(x, y, radius);
            if alpha > 0.0 {
                put(&mut pixels, size, x, y, RED, alpha);
            }
        }
    }
    let glyphs: Vec<[u8; GLYPH_H]> = text.chars().filter_map(glyph).collect();
    if glyphs.is_empty() {
        return pixels;
    }
    let scale = (size / 16).max(1);
    let text_w = (glyphs.len() * (GLYPH_W + 1) - 1) * scale;
    let text_h = GLYPH_H * scale;
    let left = size.saturating_sub(text_w) / 2;
    let top = size.saturating_sub(text_h) / 2;
    for (i, rows) in glyphs.iter().enumerate() {
        let gx = left + i * (GLYPH_W + 1) * scale;
        for (row, bits) in rows.iter().enumerate() {
            for col in 0..GLYPH_W {
                if bits & (1 << (GLYPH_W - 1 - col)) == 0 {
                    continue;
                }
                for dy in 0..scale {
                    for dx in 0..scale {
                        let (x, y) = (gx + col * scale + dx, top + row * scale + dy);
                        if x < size && y < size {
                            put(&mut pixels, size, x, y, WHITE, 1.0);
                        }
                    }
                }
            }
        }
    }
    pixels
}

/// How much of pixel `(x, y)` the disc of `radius` centred in the square
/// covers, from 0 to 1, sampled 4x4 for a smooth edge.
fn disc_coverage(x: usize, y: usize, radius: f32) -> f32 {
    const N: usize = 4;
    let mut inside = 0;
    for sy in 0..N {
        for sx in 0..N {
            let px = x as f32 + (sx as f32 + 0.5) / N as f32 - radius;
            let py = y as f32 + (sy as f32 + 0.5) / N as f32 - radius;
            if px * px + py * py <= radius * radius {
                inside += 1;
            }
        }
    }
    inside as f32 / (N * N) as f32
}

fn put(pixels: &mut [u8], size: usize, x: usize, y: usize, rgb: [u8; 3], alpha: f32) {
    let at = (y * size + x) * 4;
    pixels[at] = rgb[2];
    pixels[at + 1] = rgb[1];
    pixels[at + 2] = rgb[0];
    pixels[at + 3] = (alpha * 255.0).round() as u8;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(pixels: &[u8], size: usize, x: usize, y: usize) -> [u8; 4] {
        let at = (y * size + x) * 4;
        [pixels[at], pixels[at + 1], pixels[at + 2], pixels[at + 3]]
    }

    #[test]
    fn rf_atn_10_badge_text_is_none_at_zero_the_number_to_nine_then_nine_plus() {
        assert_eq!(badge_text(0), None);
        for n in 1..=9 {
            assert_eq!(badge_text(n), Some(n.to_string()));
        }
        assert_eq!(badge_text(10).as_deref(), Some("9+"));
        assert_eq!(badge_text(250).as_deref(), Some("9+"));
    }

    #[test]
    fn rf_atn_10_badge_is_a_red_dot_with_white_text() {
        let size = 16;
        let pixels = render("1", size);
        assert_eq!(pixels.len(), size * size * 4);
        assert_eq!(
            pixel(&pixels, size, 0, 0)[3],
            0,
            "the corner is outside the dot"
        );
        let white = [0xFF, 0xFF, 0xFF, 0xFF];
        let red = [RED[2], RED[1], RED[0], 0xFF];
        let lit: Vec<[u8; 4]> = (0..size)
            .flat_map(|y| (0..size).map(move |x| (x, y)))
            .map(|(x, y)| pixel(&pixels, size, x, y))
            .collect();
        assert!(lit.contains(&white));
        assert!(lit.contains(&red));
        assert_eq!(
            pixel(&pixels, size, 1, size / 2),
            red,
            "the left edge is red"
        );
    }

    #[test]
    fn rf_atn_10_nine_plus_fits_inside_the_badge() {
        for size in [16, 20, 24, 32] {
            let pixels = render("9+", size);
            let white_columns: Vec<usize> = (0..size)
                .filter(|&x| (0..size).any(|y| pixel(&pixels, size, x, y)[..3] == WHITE))
                .collect();
            assert!(!white_columns.is_empty());
            assert!(white_columns[0] > 0 && *white_columns.last().unwrap_or(&size) < size - 1);
        }
    }

    #[test]
    fn unknown_characters_and_empty_sizes_draw_no_text() {
        let pixels = render("x", 16);
        assert!(pixels.chunks(4).all(|p| p[..3] != WHITE || p[3] == 0));
        assert!(render("1", 0).is_empty());
    }
}
