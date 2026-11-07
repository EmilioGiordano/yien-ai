//! Plain data the UI renders from: the visible rows, the cursor, positions,
//! selections, matches, links and terminal modes. No emulator types leak out.

use std::ops::BitOr;

use crate::palette::Rgb;

/// A terminal size in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Size {
    /// Columns.
    pub cols: u16,
    /// Visible rows.
    pub rows: u16,
}

/// A cell on the screen as the user sees it: row 0 is the top of the
/// viewport, wherever the viewport is scrolled to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ViewportPoint {
    /// Row from the top of the viewport.
    pub row: u16,
    /// Column from the left.
    pub col: u16,
}

impl ViewportPoint {
    /// A point from its row and column.
    pub const fn new(row: u16, col: u16) -> Self {
        Self { row, col }
    }

    /// The buffer position under this point for a viewport scrolled back by
    /// `display_offset` lines.
    ///
    /// ```
    /// use yien_vt::{GridPoint, ViewportPoint};
    ///
    /// assert_eq!(ViewportPoint::new(0, 3).to_grid(5), GridPoint::new(-5, 3));
    /// ```
    pub fn to_grid(self, display_offset: usize) -> GridPoint {
        let offset = i32::try_from(display_offset).unwrap_or(i32::MAX);
        GridPoint::new(i32::from(self.row).saturating_sub(offset), self.col)
    }
}

/// A cell in the buffer, independent of scrolling: line 0 is the top row of
/// the live screen, negative lines are scrollback (`-1` is the newest history
/// line).
///
/// Lines shift as output scrolls the screen, so a stored `GridPoint` is valid
/// only until the next [`crate::VtEngine::feed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GridPoint {
    /// Line, 0 at the top of the live screen.
    pub line: i32,
    /// Column from the left.
    pub col: u16,
}

impl GridPoint {
    /// A point from its line and column.
    pub const fn new(line: i32, col: u16) -> Self {
        Self { line, col }
    }

    /// Where this point shows in a viewport of `rows` rows scrolled back by
    /// `display_offset`, or `None` when it is out of view.
    ///
    /// ```
    /// use yien_vt::{GridPoint, ViewportPoint};
    ///
    /// assert_eq!(GridPoint::new(-2, 1).to_viewport(2, 10), Some(ViewportPoint::new(0, 1)));
    /// assert_eq!(GridPoint::new(-3, 1).to_viewport(2, 10), None);
    /// ```
    pub fn to_viewport(self, display_offset: usize, rows: u16) -> Option<ViewportPoint> {
        let offset = i64::try_from(display_offset).ok()?;
        let row = i64::from(self.line) + offset;
        let row = u16::try_from(row).ok()?;
        (row < rows).then_some(ViewportPoint::new(row, self.col))
    }
}

/// Which half of a cell a mouse position falls on; selections use it to
/// decide whether the cell is included.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    /// The left half.
    Left,
    /// The right half.
    Right,
}

/// How a selection grows from its anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SelectionKind {
    /// Character by character, wrapping across lines.
    Simple,
    /// A rectangle.
    Block,
    /// Whole words (double click).
    Semantic,
    /// Whole lines (triple click).
    Lines,
}

/// A search match, from `start` to `end` inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Match {
    /// First cell of the match.
    pub start: GridPoint,
    /// Last cell of the match.
    pub end: GridPoint,
}

/// What a detected link points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LinkKind {
    /// A URL found in the text (`https://`, `file:`, `mailto:` and similar).
    Url,
    /// An absolute Windows path found in the text (`C:\dir\file`).
    Path,
    /// An explicit OSC 8 hyperlink set by the program.
    Hyperlink,
}

/// A link under a point, with the cells it covers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Link {
    /// What the target is.
    pub kind: LinkKind,
    /// The URL or path to open.
    pub target: String,
    /// First cell of the link.
    pub start: GridPoint,
    /// Last cell of the link.
    pub end: GridPoint,
}

/// Cell attributes as a bit set.
///
/// ```
/// use yien_vt::CellFlags;
///
/// let flags = CellFlags::BOLD | CellFlags::ITALIC;
/// assert!(flags.contains(CellFlags::BOLD));
/// assert!(!flags.contains(CellFlags::WIDE));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct CellFlags(u16);

impl CellFlags {
    /// No attributes.
    pub const EMPTY: Self = Self(0);
    /// SGR 1.
    pub const BOLD: Self = Self(1);
    /// SGR 3.
    pub const ITALIC: Self = Self(1 << 1);
    /// SGR 4.
    pub const UNDERLINE: Self = Self(1 << 2);
    /// SGR 21 or 4:2.
    pub const DOUBLE_UNDERLINE: Self = Self(1 << 3);
    /// SGR 4:3.
    pub const UNDERCURL: Self = Self(1 << 4);
    /// SGR 4:4.
    pub const DOTTED_UNDERLINE: Self = Self(1 << 5);
    /// SGR 4:5.
    pub const DASHED_UNDERLINE: Self = Self(1 << 6);
    /// SGR 9.
    pub const STRIKEOUT: Self = Self(1 << 7);
    /// SGR 8. The foreground already equals the background; the flag lets the
    /// UI skip the glyph.
    pub const HIDDEN: Self = Self(1 << 8);
    /// The cell holds a wide character that also covers the next cell.
    pub const WIDE: Self = Self(1 << 9);
    /// The cell is padding: the right half of a wide character, or the blank
    /// left at the end of a line when a wide character wrapped. Draw its
    /// background, not its character.
    pub const SPACER: Self = Self(1 << 10);
    /// The cell is inside the current selection.
    pub const SELECTED: Self = Self(1 << 11);

    /// Whether every flag in `other` is set.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Sets the flags in `other`.
    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    /// The raw bits.
    pub const fn bits(self) -> u16 {
        self.0
    }
}

impl BitOr for CellFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// One cell of the frame, with its colours already resolved.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Cell {
    /// The character; a space for an empty cell.
    pub ch: char,
    /// Combining marks and other zero-width characters drawn on top of `ch`
    /// (accents, variation selectors, emoji joiners). Usually empty.
    pub zerowidth: Vec<char>,
    /// Foreground, after inverse, faint and hidden are applied.
    pub fg: Rgb,
    /// Background, after inverse is applied.
    pub bg: Rgb,
    /// Attributes.
    pub flags: CellFlags,
}

/// Cursor shapes a program can ask for with DECSCUSR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CursorShape {
    /// A filled block.
    #[default]
    Block,
    /// An underscore.
    Underline,
    /// A vertical bar.
    Beam,
    /// An outlined block.
    HollowBlock,
}

/// The cursor as it should be drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Cursor {
    /// Where it is in the viewport.
    pub point: ViewportPoint,
    /// Its shape.
    pub shape: CursorShape,
}

/// The visible part of a pane, ready to draw.
///
/// ```
/// use yien_vt::{AlacrittyEngine, VtConfig, VtEngine};
///
/// let mut engine = AlacrittyEngine::new(VtConfig::default(), 20, 3);
/// engine.feed(b"hello");
/// let frame = engine.frame();
/// assert_eq!(frame.line_text(0), "hello");
/// assert_eq!(frame.lines.len(), 3);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// The size the frame was taken at.
    pub size: Size,
    /// `size.rows` rows of `size.cols` cells each, top to bottom.
    pub lines: Vec<Vec<Cell>>,
    /// The cursor, or `None` when it is hidden or scrolled out of view.
    pub cursor: Option<Cursor>,
    /// How many lines the viewport is scrolled back into the history.
    pub display_offset: usize,
    /// How many history lines exist above the live screen.
    pub history_len: usize,
    /// The default background, for clearing the area around the grid.
    pub background: Rgb,
    /// The cursor colour.
    pub cursor_color: Rgb,
}

impl Frame {
    /// The text of a viewport row, without spacers and trailing blanks.
    pub fn line_text(&self, row: usize) -> String {
        let Some(line) = self.lines.get(row) else {
            return String::new();
        };
        let mut text = String::with_capacity(line.len());
        for cell in line {
            if cell.flags.contains(CellFlags::SPACER) {
                continue;
            }
            text.push(cell.ch);
            text.extend(&cell.zerowidth);
        }
        text.truncate(text.trim_end().len());
        text
    }

    /// The text of the whole viewport, one line per row, without trailing
    /// empty rows.
    pub fn text(&self) -> String {
        let mut lines: Vec<String> = (0..self.lines.len()).map(|r| self.line_text(r)).collect();
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        lines.join("\n")
    }
}

/// How the program wants mouse events reported (RF-PAN-14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MouseTracking {
    /// The mouse belongs to the terminal: selection and scrolling.
    #[default]
    Off,
    /// Presses and releases (mode 1000).
    Click,
    /// Presses, releases and motion with a button held (mode 1002).
    Drag,
    /// All motion (mode 1003).
    Motion,
}

/// How mouse reports are encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MouseEncoding {
    /// The X10 byte encoding.
    #[default]
    Default,
    /// UTF-8 coordinates (mode 1005).
    Utf8,
    /// SGR (mode 1006).
    Sgr,
}

/// The terminal modes the input side needs to encode keys, paste and mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Modes {
    /// The alternate screen is active (full-screen programs).
    pub alt_screen: bool,
    /// Arrow keys send application sequences (DECCKM).
    pub app_cursor: bool,
    /// The keypad sends application sequences (DECKPAM).
    pub app_keypad: bool,
    /// Pasted text must be wrapped in bracketed paste markers (mode 2004).
    pub bracketed_paste: bool,
    /// Focus changes must be reported (mode 1004).
    pub focus_reporting: bool,
    /// The wheel sends arrow keys on the alternate screen (mode 1007).
    pub alternate_scroll: bool,
    /// Which mouse events the program wants.
    pub mouse: MouseTracking,
    /// How mouse reports are encoded.
    pub mouse_encoding: MouseEncoding,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewport_and_grid_round_trip() {
        let p = ViewportPoint::new(4, 7);
        assert_eq!(p.to_grid(10).to_viewport(10, 5), Some(p));
        assert_eq!(GridPoint::new(5, 0).to_viewport(0, 5), None);
        assert_eq!(GridPoint::new(i32::MIN, 0).to_viewport(usize::MAX, 5), None);
    }

    #[test]
    fn flags_insert() {
        let mut flags = CellFlags::EMPTY;
        flags.insert(CellFlags::WIDE);
        assert!(flags.contains(CellFlags::WIDE));
        assert!(flags.contains(CellFlags::EMPTY));
        assert_eq!(flags.bits(), CellFlags::WIDE.bits());
    }
}
