//! The screen and the newest scrollback rows as plain VT text, so a restart
//! can show a pane's history again (RF-SES-10), like Windows Terminal's
//! buffer restore. Colours keep their kind (named, indexed, truecolor), so a
//! palette switch after the restore still recolours them.

use std::io::Write;

use alacritty_terminal::grid::{Dimensions, Grid, GridCell, Row};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::vte::ansi::{Color, NamedColor};

/// How much of a pane's history a dump keeps: the newest rows, up to both
/// limits.
///
/// ```
/// use yien_vt::DumpLimits;
///
/// let limits = DumpLimits { max_bytes: 4096, ..DumpLimits::default() };
/// assert_eq!(limits.max_rows, 2_000);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DumpLimits {
    /// Rows kept, screen included.
    pub max_rows: usize,
    /// Bytes kept; a row that does not fit is dropped with every older one.
    pub max_bytes: usize,
}

impl Default for DumpLimits {
    fn default() -> Self {
        Self {
            max_rows: 2_000,
            max_bytes: 1024 * 1024,
        }
    }
}

/// Moves the cursor back to where the dumped text ends: the dump saves that
/// spot (`ESC 7`) before it moves to the cursor it recorded. Feed this after
/// a dump to write below it.
pub const DUMP_END: &[u8] = b"\x1b8";

const STYLE: Flags = Flags::BOLD
    .union(Flags::DIM)
    .union(Flags::ITALIC)
    .union(Flags::ALL_UNDERLINES)
    .union(Flags::INVERSE)
    .union(Flags::HIDDEN)
    .union(Flags::STRIKEOUT);

/// The attributes one SGR sequence sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pen {
    fg: Color,
    bg: Color,
    underline: Option<Color>,
    flags: Flags,
}

impl Default for Pen {
    fn default() -> Self {
        Self {
            fg: Color::Named(NamedColor::Foreground),
            bg: Color::Named(NamedColor::Background),
            underline: None,
            flags: Flags::empty(),
        }
    }
}

impl Pen {
    fn of(cell: &Cell) -> Self {
        Self {
            fg: cell.fg,
            bg: cell.bg,
            underline: cell.underline_color(),
            flags: cell.flags & STYLE,
        }
    }

    /// Writes the pen from a reset, so a row never depends on the one above.
    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"\x1b[0");
        const FLAGS: [(Flags, &[u8]); 11] = [
            (Flags::BOLD, b";1"),
            (Flags::DIM, b";2"),
            (Flags::ITALIC, b";3"),
            (Flags::UNDERLINE, b";4"),
            (Flags::DOUBLE_UNDERLINE, b";4:2"),
            (Flags::UNDERCURL, b";4:3"),
            (Flags::DOTTED_UNDERLINE, b";4:4"),
            (Flags::DASHED_UNDERLINE, b";4:5"),
            (Flags::INVERSE, b";7"),
            (Flags::HIDDEN, b";8"),
            (Flags::STRIKEOUT, b";9"),
        ];
        for (flag, sgr) in FLAGS {
            if self.flags.contains(flag) {
                out.extend_from_slice(sgr);
            }
        }
        write_color(out, self.fg, 30, 90, 38);
        write_color(out, self.bg, 40, 100, 48);
        match self.underline {
            Some(Color::Spec(rgb)) => {
                let _ = write!(out, ";58;2;{};{};{}", rgb.r, rgb.g, rgb.b);
            }
            Some(Color::Indexed(index)) => {
                let _ = write!(out, ";58;5;{index}");
            }
            Some(Color::Named(named)) => {
                if let Some(index) = base_index(named) {
                    let _ = write!(out, ";58;5;{index}");
                }
            }
            None => {}
        }
        out.push(b'm');
    }
}

/// The ANSI index (0..16) a named colour stands for; `None` for the default
/// colours, which a reset already sets.
fn base_index(named: NamedColor) -> Option<u8> {
    let index = named as usize;
    match index {
        0..16 => u8::try_from(index).ok(),
        // Dim black .. dim white: the cell's DIM flag carries the dimming.
        259..267 => u8::try_from(index - 259).ok(),
        _ => None,
    }
}

fn write_color(out: &mut Vec<u8>, color: Color, base: u8, bright: u8, extended: u8) {
    match color {
        Color::Named(named) => match base_index(named) {
            Some(index @ 0..8) => {
                let _ = write!(out, ";{}", base + index);
            }
            Some(index) => {
                let _ = write!(out, ";{}", bright + index - 8);
            }
            None => {}
        },
        Color::Indexed(index) => {
            let _ = write!(out, ";{extended};5;{index}");
        }
        Color::Spec(rgb) => {
            let _ = write!(out, ";{extended};2;{};{};{}", rgb.r, rgb.g, rgb.b);
        }
    }
}

fn wraps(row: &Row<Cell>, cols: usize) -> bool {
    cols > 0 && row[Column(cols - 1)].flags.contains(Flags::WRAPLINE)
}

/// A cell a dump need not write: it draws as the default background. The
/// wrap mark and a leading spacer only matter while the row wraps.
fn is_blank(cell: &Cell) -> bool {
    let mut plain = cell.clone();
    plain
        .flags
        .remove(Flags::WRAPLINE | Flags::LEADING_WIDE_CHAR_SPACER);
    plain.is_empty()
}

fn row_is_blank(row: &Row<Cell>, cols: usize) -> bool {
    (0..cols).all(|col| is_blank(&row[Column(col)]))
}

/// One row: its cells, then a line break unless it wraps into `next` or is
/// the last one. A wrapped row keeps every cell, so a terminal of another
/// width reflows it instead of breaking it. A wrap into a blank row breaks
/// the line instead, since nothing would make a terminal wrap there.
fn encode_row(row: &Row<Cell>, next: Option<&Row<Cell>>, continues: bool, cols: usize) -> Vec<u8> {
    let wrapped = wraps(row, cols) && next.is_some_and(|next| !row_is_blank(next, cols));
    let next_wide = next.is_some_and(|next| {
        let first = &next[Column(0)];
        first.flags.contains(Flags::WIDE_CHAR) && first.bg == Pen::default().bg
    });
    let end = if wrapped {
        cols
    } else {
        (0..cols)
            .rev()
            .find(|&col| !is_blank(&row[Column(col)]))
            .map_or(0, |col| col + 1)
    };
    let mut out = Vec::new();
    let mut pen = Pen::default();
    // The cell a wide character just written also fills, whatever it holds.
    let mut covered = false;
    for col in 0..end {
        let cell = &row[Column(col)];
        if std::mem::take(&mut covered) {
            continue;
        }
        // The wide character that pushed a leading spacer here does that
        // again; with anything else next, or one that needs the step back
        // below, a space keeps the wrap.
        let leading = cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER);
        if leading && wrapped && next_wide {
            continue;
        }
        let spacer = leading || cell.flags.contains(Flags::WIDE_CHAR_SPACER);
        let cell_pen = Pen::of(cell);
        if col == 0 && continues && cell_pen.bg != Pen::default().bg {
            // The wrap scrolls in a row filled with the current background:
            // wrap with the default one, then step back onto the first cell.
            out.extend_from_slice(b" \x1b[D");
        }
        if cell_pen != pen {
            cell_pen.write(&mut out);
            pen = cell_pen;
        }
        // A tab would move past the cells that follow it, which come next anyway.
        let c = if spacer || cell.c == '\t' || cell.c.is_control() {
            ' '
        } else {
            cell.c
        };
        covered = c != ' ' && cell.flags.contains(Flags::WIDE_CHAR);
        let mut buf = [0; 4];
        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        for zw in cell.zerowidth().unwrap_or_default() {
            if !zw.is_control() {
                out.extend_from_slice(zw.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    if pen != Pen::default() {
        out.extend_from_slice(b"\x1b[0m");
    }
    if !wrapped && next.is_some() {
        out.extend_from_slice(b"\r\n");
    }
    out
}

/// Encodes the active grid: the newest rows down to the cursor or the last
/// row with content, whichever is lower, then `ESC 7` and the moves back to
/// the cursor. Empty when there is nothing to keep.
pub(crate) fn encode(grid: &Grid<Cell>, limits: DumpLimits) -> Vec<u8> {
    let cols = grid.columns();
    let screen = i32::try_from(grid.screen_lines()).unwrap_or(i32::MAX);
    let top = -i32::try_from(grid.history_size()).unwrap_or(i32::MAX);
    let (cursor_line, cursor_col) = (grid.cursor.point.line.0, grid.cursor.point.column.0);
    let last = (cursor_line + 1..screen)
        .rev()
        .find(|&line| !row_is_blank(&grid[Line(line)], cols))
        .unwrap_or(cursor_line);
    let mut rows = Vec::new();
    let mut bytes = 0;
    let mut line = last;
    while line >= top && rows.len() < limits.max_rows {
        let (this, next) = (
            &grid[Line(line)],
            (line < last).then(|| &grid[Line(line + 1)]),
        );
        let continues =
            line > top && wraps(&grid[Line(line - 1)], cols) && !row_is_blank(this, cols);
        let row = encode_row(this, next, continues, cols);
        if bytes + row.len() > limits.max_bytes {
            break;
        }
        bytes += row.len();
        rows.push(row);
        line -= 1;
    }
    if rows.iter().all(Vec::is_empty) {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(bytes + 16);
    for row in rows.iter().rev() {
        out.extend_from_slice(row);
    }
    out.extend_from_slice(b"\x1b7");
    let up = last - cursor_line;
    if up > 0 {
        let _ = write!(out, "\x1b[{up}A");
    }
    let _ = write!(out, "\x1b[{}G", cursor_col + 1);
    out
}

/// Whether `bytes` holds only what a dump writes: text, line breaks, SGR,
/// cursor up, left and to a column, and `ESC 7` / `ESC 8`. A file that holds
/// anything else (a query, a mode change, a title) is not fed to an engine.
///
/// ```
/// assert!(yien_vt::is_screen_dump(b"\x1b[0;31mred\x1b[0m\r\nok\x1b7\x1b[1A\x1b[3G"));
/// assert!(!yien_vt::is_screen_dump(b"\x1b[6n"));
/// assert!(!yien_vt::is_screen_dump(b"\x1b[?1049h"));
/// ```
pub fn is_screen_dump(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\r' | '\n' => {}
            '\x1b' => match chars.next() {
                Some('7' | '8') => {}
                Some('[') => loop {
                    match chars.next() {
                        Some('0'..='9' | ';' | ':') => {}
                        Some('m' | 'A' | 'D' | 'G') => break,
                        _ => return false,
                    }
                },
                _ => return false,
            },
            c if c.is_control() => return false,
            _ => {}
        }
    }
    true
}
