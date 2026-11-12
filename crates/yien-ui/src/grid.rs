//! The pure half of the terminal view: cell metrics, which text and
//! background runs to paint, where the cursor goes, and how keys, the wheel
//! and pastes become bytes. No GPUI here, so all of it is tested headless.

use yien_input::mouse::{self, Action, Button, MouseEvent};
use yien_input::paste::{self, Risk};
use yien_input::{Key, KeyModes, Mods};
use yien_proto::frame::{
    Cell, CellFlags, Cursor, CursorShape, Frame, Modes, MouseEncoding, MouseTracking, Rgb,
    SelectionKind, Side, ViewportPoint,
};

/// Columns and rows that fit `width` by `height` pixels, at least one of each.
pub(crate) fn grid_size(width: f32, height: f32, cell_w: f32, cell_h: f32) -> (u16, u16) {
    let fit = |space: f32, cell: f32| {
        if !(space.is_finite() && cell.is_finite()) || cell <= 0.0 || space <= 0.0 {
            return 1;
        }
        let n = (space / cell).floor();
        if n >= f32::from(u16::MAX) {
            u16::MAX
        } else {
            (n as u16).max(1)
        }
    };
    (fit(width, cell_w), fit(height, cell_h))
}

/// The cell of a font at `size`: the advance of its "m" (`None` when the
/// font has no metrics: 0.6 of the size) by its ascent plus descent (1.2 of
/// the size when that is not a positive number), each snapped to device
/// pixels at `scale`.
pub(crate) fn cell_size(advance: Option<f32>, height: f32, size: f32, scale: f32) -> (f32, f32) {
    let advance = advance
        .filter(|a| a.is_finite() && *a > 0.0)
        .unwrap_or(size * 0.6);
    let height = if height.is_finite() && height > 0.0 {
        height
    } else {
        size * 1.2
    };
    (snap(advance, scale), snap(height, scale))
}

/// A length rounded to whole device pixels at `scale`, never below one.
pub(crate) fn snap(length: f32, scale: f32) -> f32 {
    if !(scale.is_finite() && scale > 0.0) {
        return length.round().max(1.0);
    }
    (length * scale).round().max(1.0) / scale
}

/// The cell under a point given relative to the grid's top-left corner,
/// clamped to the grid, and which half of it the point is on.
pub(crate) fn cell_at(
    x: f32,
    y: f32,
    cell_w: f32,
    cell_h: f32,
    cols: u16,
    rows: u16,
) -> (ViewportPoint, Side) {
    let index = |v: f32, cell: f32, count: u16| -> (u16, f32) {
        if !(v.is_finite() && cell > 0.0) || v < 0.0 || count == 0 {
            return (0, 0.0);
        }
        let raw = (v / cell).floor();
        let last = count - 1;
        if raw >= f32::from(last) {
            (last, (v - f32::from(last) * cell) / cell)
        } else {
            let i = raw as u16;
            (i, (v - raw * cell) / cell)
        }
    };
    let (col, frac) = index(x, cell_w, cols);
    let (row, _) = index(y, cell_h, rows);
    let side = if frac < 0.5 { Side::Left } else { Side::Right };
    (ViewportPoint::new(row, col), side)
}

/// How a run of text is underlined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Underline {
    None,
    Straight,
    Curly,
}

/// What a run of cells shares, so it can be shaped as one piece of text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RunStyle {
    pub(crate) fg: Rgb,
    pub(crate) bold: bool,
    pub(crate) italic: bool,
    pub(crate) underline: Underline,
    pub(crate) strike: bool,
}

/// Text to shape and paint starting at column `col`, covering `cells` cells.
/// A wide character is always a span of its own covering two cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TextSpan {
    pub(crate) col: u16,
    pub(crate) cells: u16,
    pub(crate) text: String,
    pub(crate) style: RunStyle,
    pub(crate) wide: bool,
}

fn style_of(cell: &Cell) -> RunStyle {
    let f = cell.flags;
    let underline = if f.contains(CellFlags::UNDERCURL) {
        Underline::Curly
    } else if f.contains(CellFlags::UNDERLINE)
        || f.contains(CellFlags::DOUBLE_UNDERLINE)
        || f.contains(CellFlags::DOTTED_UNDERLINE)
        || f.contains(CellFlags::DASHED_UNDERLINE)
    {
        Underline::Straight
    } else {
        Underline::None
    };
    RunStyle {
        fg: cell.fg,
        bold: f.contains(CellFlags::BOLD),
        italic: f.contains(CellFlags::ITALIC),
        underline,
        strike: f.contains(CellFlags::STRIKEOUT),
    }
}

/// The glyph a cell shows: hidden cells and control characters (external
/// input the shaper must never see, a newline above all) show nothing.
fn glyph_of(cell: &Cell) -> char {
    if cell.flags.contains(CellFlags::HIDDEN) || cell.ch.is_control() {
        ' '
    } else {
        cell.ch
    }
}

/// The runs of a row to paint. Blank cells never break a run; a style change
/// or a wide character does. `cursor` recolours the glyph under a block
/// cursor so it stays readable on top of it.
pub(crate) fn text_spans(line: &[Cell], cursor: Option<(u16, Rgb)>) -> Vec<TextSpan> {
    let mut spans: Vec<TextSpan> = Vec::new();
    // The open run and how many undecorated blanks end it; those are trimmed
    // when it closes, decorated ones (an underlined space) are kept.
    let mut open: Option<(TextSpan, u16)> = None;
    let close = |spans: &mut Vec<TextSpan>, open: Option<(TextSpan, u16)>| {
        if let Some((mut span, trailing)) = open {
            span.text.truncate(span.text.len() - usize::from(trailing));
            span.cells -= trailing;
            if !span.text.is_empty() {
                spans.push(span);
            }
        }
    };
    for (col, cell) in line.iter().enumerate() {
        let Ok(col) = u16::try_from(col) else { break };
        if cell.flags.contains(CellFlags::SPACER) {
            continue;
        }
        let mut style = style_of(cell);
        if let Some((at, color)) = cursor
            && at == col
        {
            style.fg = color;
        }
        let ch = glyph_of(cell);
        let marks = cell.zerowidth.iter().filter(|c| !c.is_control());
        let blank = ch == ' '
            && cell.zerowidth.is_empty()
            && style.underline == Underline::None
            && !style.strike;
        if blank {
            if let Some((span, trailing)) = open.as_mut() {
                span.text.push(' ');
                span.cells += 1;
                *trailing += 1;
            }
            continue;
        }
        if cell.flags.contains(CellFlags::WIDE) {
            close(&mut spans, open.take());
            let mut text = String::new();
            text.push(ch);
            text.extend(marks);
            spans.push(TextSpan {
                col,
                cells: 2,
                text,
                style,
                wide: true,
            });
            continue;
        }
        if open.as_ref().is_some_and(|(s, _)| s.style != style) {
            close(&mut spans, open.take());
        }
        let (span, trailing) = open.get_or_insert_with(|| {
            let span = TextSpan {
                col,
                cells: 0,
                text: String::new(),
                style,
                wide: false,
            };
            (span, 0)
        });
        span.text.push(ch);
        span.text.extend(marks);
        span.cells += 1;
        *trailing = 0;
    }
    close(&mut spans, open);
    spans
}

/// A run of cells sharing one background colour other than the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BgSpan {
    pub(crate) col: u16,
    pub(crate) cells: u16,
    pub(crate) color: Rgb,
}

/// The backgrounds of a row to paint; selected cells take `selection`.
pub(crate) fn background_spans(line: &[Cell], default_bg: Rgb, selection: Rgb) -> Vec<BgSpan> {
    let mut spans: Vec<BgSpan> = Vec::new();
    for (col, cell) in line.iter().enumerate() {
        let Ok(col) = u16::try_from(col) else { break };
        let color = if cell.flags.contains(CellFlags::SELECTED) {
            selection
        } else {
            cell.bg
        };
        if color == default_bg {
            continue;
        }
        match spans.last_mut() {
            Some(last) if last.color == color && last.col + last.cells == col => last.cells += 1,
            _ => spans.push(BgSpan {
                col,
                cells: 1,
                color,
            }),
        }
    }
    spans
}

/// How the cursor is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CursorPaint {
    Block,
    Hollow,
    Beam,
    Underline,
}

/// What to draw for the cursor, or `None` when it is hidden. An unfocused
/// pane always shows a hollow block.
pub(crate) fn cursor_paint(cursor: &Cursor, focused: bool) -> Option<CursorPaint> {
    if !cursor.visible {
        return None;
    }
    if !focused {
        return Some(CursorPaint::Hollow);
    }
    Some(match cursor.shape {
        CursorShape::Block => CursorPaint::Block,
        CursorShape::HollowBlock => CursorPaint::Hollow,
        CursorShape::Beam => CursorPaint::Beam,
        CursorShape::Underline => CursorPaint::Underline,
    })
}

/// How many cells the cursor covers: two on a wide character.
pub(crate) fn cursor_cells(frame: &Frame, at: ViewportPoint) -> u16 {
    let wide = frame
        .lines
        .get(usize::from(at.row))
        .and_then(|line| line.get(usize::from(at.col)))
        .is_some_and(|cell| cell.flags.contains(CellFlags::WIDE));
    if wide { 2 } else { 1 }
}

/// A named key as GPUI spells it.
pub(crate) fn named_key(key: &str) -> Option<Key> {
    Some(match key {
        "enter" => Key::Enter,
        "backspace" => Key::Backspace,
        "tab" => Key::Tab,
        "escape" => Key::Escape,
        "delete" => Key::Delete,
        "insert" => Key::Insert,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        f if f.len() > 1 && f.starts_with('f') => Key::F(f[1..].parse().ok()?),
        _ => return None,
    })
}

/// A key press as the terminal sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct KeyPress<'a> {
    /// GPUI's key name: `enter`, `a`, `f5`.
    pub(crate) key: &'a str,
    /// The text the layout would type, if any.
    pub(crate) key_char: Option<&'a str>,
    pub(crate) mods: Mods,
    /// The Windows (or Super) key is held.
    pub(crate) platform: bool,
    /// The layout turns the modifiers into a character (AltGr, dead keys).
    pub(crate) prefer_text: bool,
}

/// The bytes to write for a key press, or `None` to let the text reach the
/// pane through the input handler (plain and shifted characters, AltGr,
/// dead keys and the IME), so no character is ever sent twice.
pub(crate) fn key_bytes(press: &KeyPress<'_>, modes: &Modes) -> Option<Vec<u8>> {
    if press.prefer_text || press.platform {
        return None;
    }
    let mods = press.mods;
    let app_cursor = modes.app_cursor;
    if let Some(key) = named_key(press.key) {
        let key_modes = KeyModes {
            app_cursor,
            win32_input: modes.win32_input,
            kitty_flags: modes.kitty_keyboard,
        };
        return yien_input::encode_with(&key, mods, key_modes);
    }
    let base = if press.key == "space" {
        ' '
    } else {
        let mut chars = press.key.chars();
        let c = chars.next()?;
        if chars.next().is_some() {
            return None;
        }
        c
    };
    if !(mods.ctrl || mods.alt) {
        return None;
    }
    let c = if mods.ctrl {
        base
    } else {
        press
            .key_char
            .and_then(|s| {
                let mut it = s.chars();
                let c = it.next()?;
                it.next().is_none().then_some(c)
            })
            .unwrap_or(base)
    };
    yien_input::encode_in(&Key::Char(c), mods, app_cursor)
}

/// What a wheel turn does in a pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WheelAction {
    /// Bytes YIEN generates for the program: mouse reports or arrow keys.
    Report(Vec<u8>),
    /// Scroll the viewport; positive goes back into the history.
    Scroll(i32),
}

pub(crate) fn tracking(modes: &Modes) -> mouse::Tracking {
    match modes.mouse {
        MouseTracking::Off => mouse::Tracking::Off,
        MouseTracking::Click => mouse::Tracking::Click,
        MouseTracking::Drag => mouse::Tracking::Drag,
        MouseTracking::Motion => mouse::Tracking::Motion,
    }
}

pub(crate) fn encoding(modes: &Modes) -> mouse::Encoding {
    match modes.mouse_encoding {
        MouseEncoding::Default => mouse::Encoding::Default,
        MouseEncoding::Utf8 => mouse::Encoding::Utf8,
        MouseEncoding::Sgr => mouse::Encoding::Sgr,
    }
}

/// Who a button press in the grid goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pointer {
    /// The program asked for the mouse: it gets a report.
    Program,
    /// The terminal selects text.
    Terminal,
}

/// A program that asked for the mouse gets it; holding Shift keeps it for
/// the terminal, so text in a full-screen program can still be selected.
pub(crate) fn pointer(modes: &Modes, shift: bool) -> Pointer {
    if modes.mouse != MouseTracking::Off && !shift {
        Pointer::Program
    } else {
        Pointer::Terminal
    }
}

/// How a left press selects: cells, words from a double click, lines from a
/// triple one, a rectangle with Alt.
pub(crate) fn selection_kind(click_count: usize, alt: bool) -> SelectionKind {
    match click_count {
        _ if alt => SelectionKind::Block,
        0 | 1 => SelectionKind::Simple,
        2 => SelectionKind::Semantic,
        _ => SelectionKind::Lines,
    }
}

/// The selected text as the clipboard gets it: lines end in `\r\n` where
/// `crlf` is set, as Windows programs expect, and in `\n` elsewhere.
pub(crate) fn clipboard_text(text: &str, crlf: bool) -> String {
    let unix = text.replace("\r\n", "\n");
    if crlf {
        unix.replace('\n', "\r\n")
    } else {
        unix
    }
}

/// `lines` positive means away from the user. Shift keeps the wheel for the
/// terminal even when the program asked for the mouse.
pub(crate) fn wheel_action(
    lines: i32,
    modes: &Modes,
    at: ViewportPoint,
    mods: Mods,
) -> Option<WheelAction> {
    if lines == 0 {
        return None;
    }
    let count = lines.unsigned_abs().min(100) as usize;
    if pointer(modes, mods.shift) == Pointer::Program {
        let event = MouseEvent {
            button: if lines > 0 {
                Button::WheelUp
            } else {
                Button::WheelDown
            },
            action: Action::Press,
            col: at.col,
            row: at.row,
            mods,
        };
        let one = mouse::report(&event, tracking(modes), encoding(modes))?;
        return Some(WheelAction::Report(one.repeat(count)));
    }
    if modes.alt_screen && modes.alternate_scroll && !mods.shift {
        let key = if lines > 0 { Key::Up } else { Key::Down };
        let one = yien_input::encode_in(&key, Mods::NONE, modes.app_cursor)?;
        return Some(WheelAction::Report(one.repeat(count)));
    }
    if modes.alt_screen {
        return None;
    }
    Some(WheelAction::Scroll(lines))
}

/// What to do with text the user pastes (RF-PAN-09).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PastePlan {
    /// Nothing to paste.
    Nothing,
    /// Safe: send right away.
    Send(Vec<u8>),
    /// Ask first, saying why; send `bytes` only on a yes.
    Confirm { bytes: Vec<u8>, reason: String },
}

pub(crate) fn paste_plan(text: &str, bracketed: bool) -> PastePlan {
    if text.is_empty() {
        return PastePlan::Nothing;
    }
    let normalized = paste::normalize(text);
    let bytes = if bracketed {
        paste::bracket(&normalized)
    } else {
        normalized.into_bytes()
    };
    let reason = match paste::assess(text, bracketed) {
        Risk::Safe => return PastePlan::Send(bytes),
        Risk::WouldExecute { lines } => {
            format!("This paste has {lines} lines; each one runs as a command.")
        }
        Risk::ControlCharacters { count } => {
            format!("This paste holds {count} control characters that can drive the terminal.")
        }
        Risk::Large { bytes } => format!("This paste is {} KB long.", bytes.div_ceil(1024)),
    };
    PastePlan::Confirm { bytes, reason }
}

/// A key pressed while a paste waits for a yes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptKey {
    Confirm,
    Cancel,
    /// Any other key does nothing: it must not reach the program or the
    /// window's shortcuts while the question is open.
    Swallow,
}

pub(crate) fn paste_prompt_key(key: &str) -> PromptKey {
    match key {
        "enter" => PromptKey::Confirm,
        "escape" => PromptKey::Cancel,
        _ => PromptKey::Swallow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(ch: char) -> Cell {
        Cell {
            ch,
            zerowidth: Vec::new(),
            fg: Rgb::new(200, 200, 200),
            bg: Rgb::default(),
            flags: CellFlags::EMPTY,
        }
    }

    fn row(text: &str) -> Vec<Cell> {
        text.chars().map(cell).collect()
    }

    #[test]
    fn rf_cfg_02_a_bigger_terminal_font_fits_fewer_columns_and_rows() {
        // A mono face's advance and height grow in step with its size.
        let cell = |size: f32| cell_size(Some(size * 0.55), size * 1.25, size, 1.0);
        let (w13, h13) = cell(13.0);
        let (w18, h18) = cell(18.0);
        assert_eq!((w13, h13), (7.0, 16.0));
        assert_eq!((w18, h18), (10.0, 23.0));
        let (cols13, rows13) = grid_size(700.0, 480.0, w13, h13);
        let (cols18, rows18) = grid_size(700.0, 480.0, w18, h18);
        assert_eq!((cols13, rows13), (100, 30));
        assert_eq!((cols18, rows18), (70, 20));
        // Back at 13 the same grid comes back: nothing drifts.
        assert_eq!(
            grid_size(700.0, 480.0, cell(13.0).0, cell(13.0).1),
            (100, 30)
        );
    }

    #[test]
    fn a_font_without_metrics_gets_a_cell_from_its_size() {
        assert_eq!(cell_size(None, f32::NAN, 10.0, 1.0), (6.0, 12.0));
        assert_eq!(cell_size(Some(0.0), -1.0, 20.0, 2.0), (12.0, 24.0));
        // At 150 % a 7.4 px advance snaps to whole device pixels.
        assert_eq!(cell_size(Some(7.4), 16.0, 13.0, 1.5), (11.0 / 1.5, 16.0));
    }

    #[test]
    fn grid_size_floors_and_never_reaches_zero() {
        assert_eq!(grid_size(805.0, 410.0, 8.0, 20.0), (100, 20));
        assert_eq!(grid_size(3.0, 3.0, 8.0, 20.0), (1, 1));
        assert_eq!(grid_size(f32::NAN, 100.0, 8.0, 0.0), (1, 1));
        assert_eq!(grid_size(1e9, 1e9, 1.0, 1.0), (u16::MAX, u16::MAX));
    }

    #[test]
    fn rnf_19_snap_rounds_to_device_pixels() {
        assert_eq!(snap(7.3, 1.0), 7.0);
        assert_eq!(snap(7.3, 1.5), 11.0 / 1.5);
        assert_eq!(snap(7.3, 2.0), 7.5);
        assert_eq!(snap(0.1, 1.0), 1.0);
    }

    #[test]
    fn cell_at_clamps_and_picks_the_half() {
        let (p, side) = cell_at(17.0, 45.0, 8.0, 20.0, 10, 5);
        assert_eq!(p, ViewportPoint::new(2, 2));
        assert_eq!(side, Side::Left);
        let (p, side) = cell_at(23.0, 0.0, 8.0, 20.0, 10, 5);
        assert_eq!(p, ViewportPoint::new(0, 2));
        assert_eq!(side, Side::Right);
        let (p, _) = cell_at(-5.0, 9999.0, 8.0, 20.0, 10, 5);
        assert_eq!(p, ViewportPoint::new(4, 0));
        let (p, side) = cell_at(9999.0, 0.0, 8.0, 20.0, 10, 5);
        assert_eq!(p.col, 9);
        assert_eq!(side, Side::Right);
    }

    #[test]
    fn equal_styles_batch_into_one_run_and_blanks_do_not_break_it() {
        let spans = text_spans(&row("ls -la   "), None);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "ls -la");
        assert_eq!((spans[0].col, spans[0].cells), (0, 6));
    }

    #[test]
    fn a_style_change_starts_a_new_run_at_its_column() {
        let mut line = row("ab  cd");
        for c in &mut line[4..] {
            c.flags = CellFlags::BOLD;
        }
        let spans = text_spans(&line, None);
        assert_eq!(spans.len(), 2);
        assert_eq!((spans[1].col, spans[1].text.as_str()), (4, "cd"));
        assert!(spans[1].style.bold);
        assert_eq!(spans[0].text, "ab");
    }

    #[test]
    fn rf_pan_10_a_wide_char_is_its_own_two_cell_run_and_skips_its_spacer() {
        let mut line = vec![cell('a'), cell('日'), cell(' '), cell('b')];
        line[1].flags = CellFlags::WIDE;
        line[2].flags = CellFlags::SPACER;
        let spans = text_spans(&line, None);
        let got: Vec<_> = spans
            .iter()
            .map(|s| (s.col, s.cells, s.text.as_str(), s.wide))
            .collect();
        assert_eq!(
            got,
            vec![(0, 1, "a", false), (1, 2, "日", true), (3, 1, "b", false)]
        );
    }

    #[test]
    fn rf_pan_10_combining_marks_stay_with_their_base() {
        let mut line = row("e!");
        line[0].zerowidth = vec!['\u{301}'];
        let spans = text_spans(&line, None);
        assert_eq!(spans[0].text, "e\u{301}!");
        assert_eq!(spans[0].cells, 2);
    }

    #[test]
    fn control_and_hidden_cells_never_reach_the_shaper() {
        let mut line = row("a\nb");
        line[2].flags = CellFlags::HIDDEN;
        let spans = text_spans(&line, None);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "a");
        assert!(!spans.iter().any(|s| s.text.contains('\n')));
    }

    #[test]
    fn an_underlined_blank_is_painted() {
        let mut line = row("a b");
        line[1].flags = CellFlags::UNDERLINE;
        let spans = text_spans(&line, None);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[1].style.underline, Underline::Straight);
    }

    #[test]
    fn the_block_cursor_recolours_its_glyph() {
        let ink = Rgb::new(1, 2, 3);
        let spans = text_spans(&row("abc"), Some((1, ink)));
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[1].style.fg, ink);
        assert_eq!(spans[1].text, "b");
    }

    #[test]
    fn backgrounds_merge_and_skip_the_default() {
        let mut line = row("abcde");
        let red = Rgb::new(200, 0, 0);
        line[1].bg = red;
        line[2].bg = red;
        line[4].flags = CellFlags::SELECTED;
        let sel = Rgb::new(9, 9, 9);
        let spans = background_spans(&line, Rgb::default(), sel);
        assert_eq!(
            spans,
            vec![
                BgSpan {
                    col: 1,
                    cells: 2,
                    color: red
                },
                BgSpan {
                    col: 4,
                    cells: 1,
                    color: sel
                },
            ]
        );
    }

    fn frame_with(line: Vec<Cell>) -> Frame {
        Frame {
            size: yien_proto::frame::Size {
                cols: u16::try_from(line.len()).unwrap(),
                rows: 1,
            },
            lines: vec![line],
            cursor: None,
            display_offset: 0,
            history_len: 0,
            background: Rgb::default(),
            cursor_color: Rgb::default(),
            modes: Modes::default(),
        }
    }

    #[test]
    fn rf_pan_10_the_cursor_covers_both_cells_of_a_wide_char() {
        let mut line = vec![cell('a'), cell('字'), cell(' ')];
        line[1].flags = CellFlags::WIDE;
        line[2].flags = CellFlags::SPACER;
        let frame = frame_with(line);
        assert_eq!(cursor_cells(&frame, ViewportPoint::new(0, 1)), 2);
        assert_eq!(cursor_cells(&frame, ViewportPoint::new(0, 0)), 1);
        assert_eq!(cursor_cells(&frame, ViewportPoint::new(5, 5)), 1);
    }

    #[test]
    fn the_cursor_is_hollow_when_unfocused_and_absent_when_hidden() {
        let mut cursor = Cursor {
            point: ViewportPoint::new(0, 0),
            shape: CursorShape::Beam,
            visible: true,
        };
        assert_eq!(cursor_paint(&cursor, true), Some(CursorPaint::Beam));
        assert_eq!(cursor_paint(&cursor, false), Some(CursorPaint::Hollow));
        cursor.shape = CursorShape::Underline;
        assert_eq!(cursor_paint(&cursor, true), Some(CursorPaint::Underline));
        cursor.visible = false;
        assert_eq!(cursor_paint(&cursor, true), None);
    }

    fn press<'a>(key: &'a str, key_char: Option<&'a str>, mods: Mods) -> KeyPress<'a> {
        KeyPress {
            key,
            key_char,
            mods,
            platform: false,
            prefer_text: false,
        }
    }

    const ALT: Mods = Mods {
        ctrl: false,
        alt: true,
        shift: false,
    };
    const SHIFT: Mods = Mods {
        ctrl: false,
        alt: false,
        shift: true,
    };

    #[test]
    fn named_keys_are_encoded_and_follow_app_cursor() {
        assert_eq!(
            key_bytes(&press("enter", None, Mods::NONE), &modes()).unwrap(),
            b"\r"
        );
        assert_eq!(
            key_bytes(&press("up", None, Mods::NONE), &app_cursor()).unwrap(),
            b"\x1bOA"
        );
        assert_eq!(
            key_bytes(&press("f5", None, Mods::NONE), &modes()).unwrap(),
            b"\x1b[15~"
        );
        assert_eq!(
            key_bytes(&press("tab", None, SHIFT), &modes()).unwrap(),
            b"\x1b[Z"
        );
    }

    #[test]
    fn plain_and_shifted_characters_go_through_the_input_handler() {
        assert_eq!(
            key_bytes(&press("a", Some("a"), Mods::NONE), &modes()),
            None
        );
        assert_eq!(key_bytes(&press("a", Some("A"), SHIFT), &modes()), None);
        assert_eq!(
            key_bytes(&press("space", Some(" "), Mods::NONE), &modes()),
            None
        );
    }

    #[test]
    fn ctrl_and_alt_combinations_are_encoded_here() {
        assert_eq!(
            key_bytes(&press("c", None, Mods::CTRL), &modes()).unwrap(),
            vec![0x03]
        );
        assert_eq!(
            key_bytes(&press("space", None, Mods::CTRL), &modes()).unwrap(),
            vec![0]
        );
        assert_eq!(
            key_bytes(&press("b", Some("b"), ALT), &modes()).unwrap(),
            b"\x1bb"
        );
    }

    #[test]
    fn rf_pan_15_altgr_is_text_not_ctrl_alt() {
        let mut altgr = press(
            "q",
            Some("@"),
            Mods {
                ctrl: true,
                alt: true,
                shift: false,
            },
        );
        altgr.prefer_text = true;
        assert_eq!(key_bytes(&altgr, &modes()), None);
    }

    #[test]
    fn the_windows_key_is_left_alone() {
        let mut p = press("e", None, Mods::NONE);
        p.platform = true;
        assert_eq!(key_bytes(&p, &modes()), None);
    }

    #[test]
    fn rf_age_02_shift_enter_follows_the_keyboard_mode_the_program_set() {
        // GPUI reports Shift+Enter as "enter" with no text: CR is a control.
        let shift_enter = press("enter", None, SHIFT);
        assert_eq!(key_bytes(&shift_enter, &modes()).unwrap(), b"\r");
        let conpty = Modes {
            win32_input: true,
            ..modes()
        };
        assert_eq!(
            key_bytes(&shift_enter, &conpty).unwrap(),
            b"\x1b[13;28;13;1;16;1_\x1b[13;28;13;0;16;1_"
        );
        let kitty = Modes {
            kitty_keyboard: 1,
            ..conpty
        };
        assert_eq!(key_bytes(&shift_enter, &kitty).unwrap(), b"\x1b[13;2u");
        // Plain Enter is unchanged in every mode.
        let enter = press("enter", None, Mods::NONE);
        for m in [modes(), conpty, kitty] {
            assert_eq!(key_bytes(&enter, &m).unwrap(), b"\r");
            assert_eq!(key_bytes(&press("enter", None, ALT), &m).unwrap(), b"\r");
            assert_eq!(
                key_bytes(&press("enter", None, Mods::CTRL), &m).unwrap(),
                b"\r"
            );
        }
    }

    fn modes() -> Modes {
        Modes::default()
    }

    fn app_cursor() -> Modes {
        Modes {
            app_cursor: true,
            ..modes()
        }
    }

    #[test]
    fn rf_pan_07_the_wheel_scrolls_the_history_by_default() {
        let at = ViewportPoint::new(0, 0);
        assert_eq!(
            wheel_action(3, &modes(), at, Mods::NONE),
            Some(WheelAction::Scroll(3))
        );
        assert_eq!(
            wheel_action(-2, &modes(), at, Mods::NONE),
            Some(WheelAction::Scroll(-2))
        );
        assert_eq!(wheel_action(0, &modes(), at, Mods::NONE), None);
    }

    #[test]
    fn rf_pan_14_the_wheel_goes_to_a_program_that_asked_for_the_mouse() {
        let m = Modes {
            mouse: MouseTracking::Click,
            mouse_encoding: MouseEncoding::Sgr,
            ..modes()
        };
        let at = ViewportPoint::new(1, 2);
        assert_eq!(
            wheel_action(2, &m, at, Mods::NONE),
            Some(WheelAction::Report(b"\x1b[<64;3;2M\x1b[<64;3;2M".to_vec()))
        );
        assert_eq!(wheel_action(2, &m, at, SHIFT), Some(WheelAction::Scroll(2)));
    }

    #[test]
    fn rf_pan_09_a_press_selects_unless_the_program_asked_for_the_mouse() {
        assert_eq!(pointer(&modes(), false), Pointer::Terminal);
        assert_eq!(pointer(&modes(), true), Pointer::Terminal);
    }

    #[test]
    fn rf_pan_09_shift_selects_in_a_program_that_asked_for_the_mouse() {
        for mouse in [
            MouseTracking::Click,
            MouseTracking::Drag,
            MouseTracking::Motion,
        ] {
            let m = Modes { mouse, ..modes() };
            assert_eq!(pointer(&m, false), Pointer::Program, "{mouse:?}");
            assert_eq!(pointer(&m, true), Pointer::Terminal, "{mouse:?}");
        }
    }

    #[test]
    fn rf_pan_09_double_click_selects_a_word_and_triple_click_a_line() {
        assert_eq!(selection_kind(0, false), SelectionKind::Simple);
        assert_eq!(selection_kind(1, false), SelectionKind::Simple);
        assert_eq!(selection_kind(2, false), SelectionKind::Semantic);
        assert_eq!(selection_kind(3, false), SelectionKind::Lines);
        assert_eq!(selection_kind(7, false), SelectionKind::Lines);
        for clicks in 0..4 {
            assert_eq!(selection_kind(clicks, true), SelectionKind::Block);
        }
    }

    #[test]
    fn rf_pan_09_copied_lines_end_as_the_system_expects() {
        assert_eq!(clipboard_text("one\ntwo\n", true), "one\r\ntwo\r\n");
        assert_eq!(clipboard_text("one\ntwo\n", false), "one\ntwo\n");
        assert_eq!(clipboard_text("one\r\ntwo", true), "one\r\ntwo");
        assert_eq!(clipboard_text("one\r\ntwo", false), "one\ntwo");
        assert_eq!(clipboard_text("one", true), "one");
        assert_eq!(clipboard_text("", true), "");
    }

    #[test]
    fn the_wheel_sends_arrows_on_the_alternate_screen_when_asked() {
        let m = Modes {
            alt_screen: true,
            alternate_scroll: true,
            app_cursor: true,
            ..modes()
        };
        let at = ViewportPoint::new(0, 0);
        assert_eq!(
            wheel_action(-2, &m, at, Mods::NONE),
            Some(WheelAction::Report(b"\x1bOB\x1bOB".to_vec()))
        );
        let no_alt_scroll = Modes {
            alt_screen: true,
            ..modes()
        };
        assert_eq!(wheel_action(1, &no_alt_scroll, at, Mods::NONE), None);
    }

    #[test]
    fn rf_pan_09_a_single_line_pastes_at_once() {
        assert_eq!(paste_plan("ls", false), PastePlan::Send(b"ls".to_vec()));
        assert_eq!(paste_plan("", false), PastePlan::Nothing);
    }

    #[test]
    fn rf_pan_09_bracketed_multiline_pastes_at_once_as_one_block() {
        assert_eq!(
            paste_plan("a\nb", true),
            PastePlan::Send(b"\x1b[200~a\rb\x1b[201~".to_vec())
        );
    }

    #[test]
    fn rf_pan_09_unbracketed_multiline_asks_first() {
        let PastePlan::Confirm { bytes, reason } = paste_plan("make\nmake install\n", false) else {
            panic!("expected a confirmation");
        };
        assert_eq!(bytes, b"make\rmake install\r");
        assert!(reason.contains("3 lines"));
    }

    #[test]
    fn rf_pan_09_escapes_in_a_paste_ask_first_even_when_bracketed() {
        let plan = paste_plan("echo\x1b[201~rm", true);
        let PastePlan::Confirm { bytes, reason } = plan else {
            panic!("expected a confirmation");
        };
        assert!(reason.contains("control"));
        assert_eq!(bytes.iter().filter(|&&b| b == 0x1b).count(), 2);
    }

    #[test]
    fn rf_pan_09_only_enter_and_escape_answer_the_paste_question() {
        assert_eq!(paste_prompt_key("enter"), PromptKey::Confirm);
        assert_eq!(paste_prompt_key("escape"), PromptKey::Cancel);
        for key in ["a", "space", "tab", "up", "f5", "backspace"] {
            assert_eq!(paste_prompt_key(key), PromptKey::Swallow);
        }
    }
}
