//! [`VtEngine`] on top of `alacritty_terminal` (ADR-04).

use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use alacritty_terminal::Term;
use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::{Cell as TermCell, Flags};
use alacritty_terminal::term::color::COUNT as COLOR_COUNT;
use alacritty_terminal::term::search::{RegexIter, RegexSearch};
use alacritty_terminal::term::{Config, Osc52, TermMode, point_to_viewport};
use alacritty_terminal::vte::ansi::{
    self, Color, CursorShape as TermCursorShape, CursorStyle, Processor, Timeout,
};

use crate::frame::{
    Cell, CellFlags, Cursor, CursorShape, Frame, GridPoint, Link, LinkKind, Match, Modes,
    MouseEncoding, MouseTracking, SelectionKind, Side, Size, ViewportPoint,
};
use crate::osc_scan::OscScanner;
use crate::palette::{Palette, Rgb};
use crate::{TitleChange, VtConfig, VtEngine, VtError, VtOutput};

/// The most matches one search returns.
pub const MAX_MATCHES: usize = 10_000;

/// The most scrollback lines kept in memory, whatever the configuration asks.
pub const MAX_SCROLLBACK: usize = 100_000;

const MIN_COLS: u16 = 2;
const MIN_ROWS: u16 = 1;

const LINK_PATTERN: &str = concat!(
    r#"(?:https?://|file:|mailto:|ssh:|git://|ftp://)[^\x00-\x1F\x7F-\x9F<>"\s{}\^`\\]+"#,
    r#"|[A-Za-z]:[\\/][^\x00-\x1F<>"\s|?*:]+"#,
);

#[derive(Clone)]
struct Listener(Sender<Event>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        // The receiver lives in the same engine, so sending cannot fail while it exists.
        let _ = self.0.send(event);
    }
}

/// The synchronized-update (mode 2026) timer without a clock: it only records
/// that an update is open. The pane actor owns time and calls
/// [`VtEngine::flush_sync`] when the update has been open too long.
#[derive(Debug, Default)]
struct SyncFlag(bool);

impl Timeout for SyncFlag {
    fn set_timeout(&mut self, _: Duration) {
        self.0 = true;
    }

    fn clear_timeout(&mut self) {
        self.0 = false;
    }

    fn pending_timeout(&self) -> bool {
        self.0
    }
}

struct TermSize {
    cols: usize,
    rows: usize,
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

fn term_size(cols: u16, rows: u16) -> TermSize {
    TermSize {
        cols: usize::from(cols.max(MIN_COLS)),
        rows: usize::from(rows.max(MIN_ROWS)),
    }
}

/// The terminal engine one pane owns.
///
/// ```
/// use yien_vt::{AlacrittyEngine, VtConfig, VtEngine};
///
/// let mut engine = AlacrittyEngine::new(VtConfig::default(), 80, 24);
/// let output = engine.feed(b"\x1b[6n");
/// assert_eq!(output.replies, b"\x1b[1;1R");
/// ```
pub struct AlacrittyEngine {
    term: Term<Listener>,
    parser: Processor<SyncFlag>,
    events: Receiver<Event>,
    osc: OscScanner,
    palette: Palette,
    link_regex: Option<RegexSearch>,
}

impl AlacrittyEngine {
    /// An empty terminal of `cols` by `rows` cells (at least 2 by 1).
    pub fn new(config: VtConfig, cols: u16, rows: u16) -> Self {
        let term_config = Config {
            scrolling_history: config.scrollback_lines.min(MAX_SCROLLBACK),
            default_cursor_style: CursorStyle {
                shape: to_term_shape(config.cursor_shape),
                blinking: false,
            },
            osc52: Osc52::OnlyCopy,
            ..Config::default()
        };
        let (sender, events) = channel();
        let term = Term::new(term_config, &term_size(cols, rows), Listener(sender));
        Self {
            term,
            parser: Processor::new(),
            events,
            osc: OscScanner::new(),
            palette: config.palette,
            link_regex: None,
        }
    }

    /// Moves the terminal events raised so far into `out`, in the order they
    /// were raised, so replies reach the PTY in the order the program asked.
    fn collect(&mut self, out: &mut VtOutput) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::PtyWrite(text) => out.replies.extend_from_slice(text.as_bytes()),
                Event::ColorRequest(index, format) => {
                    let rgb = self.color_at(index);
                    let reply = format(ansi::Rgb {
                        r: rgb.r,
                        g: rgb.g,
                        b: rgb.b,
                    });
                    out.replies.extend_from_slice(reply.as_bytes());
                }
                Event::Title(title) => out.title = Some(TitleChange::Set(title)),
                Event::ResetTitle => out.title = Some(TitleChange::Reset),
                Event::Bell => out.bell = true,
                Event::ClipboardStore(_, text) => out.clipboard = Some(text),
                // OSC 52 reads are disabled, and there is no pixel size to
                // report for CSI 14 t; the rest only matter to alacritty's own UI.
                Event::ClipboardLoad(..)
                | Event::TextAreaSizeRequest(_)
                | Event::MouseCursorDirty
                | Event::CursorBlinkingChange
                | Event::Wakeup
                | Event::Exit
                | Event::ChildExit(_) => {}
            }
        }
    }

    /// The colour of an alacritty colour index: a program's OSC 4/10/11
    /// override first, then the palette.
    fn color_at(&self, index: usize) -> Rgb {
        if index < COLOR_COUNT
            && let Some(rgb) = self.term.colors()[index]
        {
            return Rgb::new(rgb.r, rgb.g, rgb.b);
        }
        let p = &self.palette;
        match index {
            0..256 => p.indexed(u8::try_from(index).unwrap_or(0)),
            256 | 267 => p.foreground,
            257 => p.background,
            258 => p.cursor,
            259..267 => p.indexed(u8::try_from(index - 259).unwrap_or(0)).dim(),
            268 => p.foreground.dim(),
            _ => p.foreground,
        }
    }

    fn resolve(&self, color: Color) -> Rgb {
        match color {
            Color::Named(named) => self.color_at(named as usize),
            Color::Indexed(index) => self.color_at(usize::from(index)),
            Color::Spec(rgb) => Rgb::new(rgb.r, rgb.g, rgb.b),
        }
    }

    fn frame_cell(&self, cell: &TermCell, selected: bool) -> Cell {
        let mut fg = self.resolve(cell.fg);
        let mut bg = self.resolve(cell.bg);
        if cell.flags.contains(Flags::DIM) {
            fg = fg.dim();
        }
        if cell.flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if cell.flags.contains(Flags::HIDDEN) {
            fg = bg;
        }
        Cell {
            ch: cell.c,
            zerowidth: cell.zerowidth().map(<[char]>::to_vec).unwrap_or_default(),
            fg,
            bg,
            flags: frame_flags(cell.flags, selected),
        }
    }

    fn clamp_line(&self, line: i32) -> Line {
        Line(line.clamp(self.term.topmost_line().0, self.term.bottommost_line().0))
    }

    fn clamp_col(&self, col: u16) -> Column {
        Column(usize::from(col).min(self.term.last_column().0))
    }

    fn viewport_to_point(&self, at: ViewportPoint) -> Point {
        let offset = i32::try_from(self.term.grid().display_offset()).unwrap_or(i32::MAX);
        let line = i32::from(at.row).saturating_sub(offset);
        Point::new(self.clamp_line(line), self.clamp_col(at.col))
    }

    fn grid_to_point(&self, at: GridPoint) -> Point {
        Point::new(self.clamp_line(at.line), self.clamp_col(at.col))
    }

    /// alacritty's block range can start one column past the last one (a drag
    /// starting on the right half of the last cell), and its
    /// `selection_to_string` then indexes out of bounds; clamp first.
    fn block_text(&self) -> Option<String> {
        let range = self.term.selection.as_ref()?.to_range(&self.term)?;
        let last = self.term.last_column();
        let (first_col, last_col) = (range.start.column.min(last), range.end.column.min(last));
        if range.start.column > last || first_col > last_col {
            return None;
        }
        let lines: Vec<String> = (range.start.line.0..=range.end.line.0)
            .map(|line| {
                let line = Line(line);
                let text = self
                    .term
                    .bounds_to_string(Point::new(line, first_col), Point::new(line, last_col));
                text.trim_end().to_owned()
            })
            .collect();
        Some(lines.join("\n"))
    }

    fn hyperlink_at(&self, point: Point) -> Option<Link> {
        let grid = self.term.grid();
        let link = grid[point].hyperlink()?;
        let row = &grid[point.line];
        let same = |col: usize| row[Column(col)].hyperlink().as_ref() == Some(&link);
        let mut start = point.column.0;
        while start > 0 && same(start - 1) {
            start -= 1;
        }
        let mut end = point.column.0;
        while end < self.term.last_column().0 && same(end + 1) {
            end += 1;
        }
        Some(Link {
            kind: LinkKind::Hyperlink,
            target: link.uri().to_owned(),
            start: to_grid_point(Point::new(point.line, Column(start))),
            end: to_grid_point(Point::new(point.line, Column(end))),
        })
    }
}

impl VtEngine for AlacrittyEngine {
    fn feed(&mut self, bytes: &[u8]) -> VtOutput {
        let mut out = VtOutput::default();
        self.osc.scan_into(bytes, &mut out.signals);
        self.parser.advance(&mut self.term, bytes);
        self.collect(&mut out);
        out
    }

    fn sync_pending(&self) -> bool {
        self.parser.sync_timeout().pending_timeout()
    }

    fn flush_sync(&mut self) -> VtOutput {
        let mut out = VtOutput::default();
        if self.sync_pending() {
            self.parser.stop_sync(&mut self.term);
        }
        self.collect(&mut out);
        out
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        self.term.resize(term_size(cols, rows));
    }

    fn size(&self) -> Size {
        Size {
            cols: u16::try_from(self.term.columns()).unwrap_or(u16::MAX),
            rows: u16::try_from(self.term.screen_lines()).unwrap_or(u16::MAX),
        }
    }

    fn frame(&self) -> Frame {
        let size = self.size();
        let content = self.term.renderable_content();
        let offset = content.display_offset;
        let cols = usize::from(size.cols);
        let mut lines = vec![Vec::with_capacity(cols); usize::from(size.rows)];
        for indexed in content.display_iter {
            let Some(at) = point_to_viewport(offset, indexed.point) else {
                continue;
            };
            let Some(line) = lines.get_mut(at.line) else {
                continue;
            };
            let selected = content
                .selection
                .as_ref()
                .is_some_and(|range| range.contains(indexed.point));
            line.push(self.frame_cell(indexed.cell, selected));
        }
        let visible = content.cursor.shape != TermCursorShape::Hidden;
        let shape = from_term_shape(self.term.cursor_style().shape).unwrap_or_default();
        let cursor = point_to_viewport(offset, content.cursor.point).and_then(|at| {
            let row = u16::try_from(at.line).ok().filter(|r| *r < size.rows)?;
            let col = u16::try_from(at.column.0).ok()?;
            Some(Cursor {
                point: ViewportPoint::new(row, col),
                shape,
                visible,
            })
        });
        Frame {
            size,
            lines,
            cursor,
            display_offset: offset,
            history_len: self.term.history_size(),
            background: self.color_at(257),
            cursor_color: self.color_at(258),
        }
    }

    fn modes(&self) -> Modes {
        let mode = *self.term.mode();
        let mouse = if mode.contains(TermMode::MOUSE_MOTION) {
            MouseTracking::Motion
        } else if mode.contains(TermMode::MOUSE_DRAG) {
            MouseTracking::Drag
        } else if mode.contains(TermMode::MOUSE_REPORT_CLICK) {
            MouseTracking::Click
        } else {
            MouseTracking::Off
        };
        let mouse_encoding = if mode.contains(TermMode::SGR_MOUSE) {
            MouseEncoding::Sgr
        } else if mode.contains(TermMode::UTF8_MOUSE) {
            MouseEncoding::Utf8
        } else {
            MouseEncoding::Default
        };
        Modes {
            alt_screen: mode.contains(TermMode::ALT_SCREEN),
            app_cursor: mode.contains(TermMode::APP_CURSOR),
            app_keypad: mode.contains(TermMode::APP_KEYPAD),
            bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
            focus_reporting: mode.contains(TermMode::FOCUS_IN_OUT),
            alternate_scroll: mode.contains(TermMode::ALTERNATE_SCROLL),
            mouse,
            mouse_encoding,
        }
    }

    fn scroll(&mut self, delta: i32) {
        // alacritty adds the delta to the offset as i32 and overflows on large values.
        let offset = i32::try_from(self.term.grid().display_offset()).unwrap_or(i32::MAX);
        let history = i32::try_from(self.term.history_size()).unwrap_or(i32::MAX);
        let delta = delta.clamp(-offset, history - offset);
        self.term.scroll_display(Scroll::Delta(delta));
    }

    fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    fn scroll_to(&mut self, at: GridPoint) {
        let point = self.grid_to_point(at);
        self.term.scroll_to_point(point);
    }

    fn start_selection(&mut self, at: ViewportPoint, side: Side, kind: SelectionKind) {
        let point = self.viewport_to_point(at);
        self.term.selection = Some(Selection::new(
            to_term_selection(kind),
            point,
            to_term_side(side),
        ));
    }

    fn update_selection(&mut self, at: ViewportPoint, side: Side) {
        let point = self.viewport_to_point(at);
        if let Some(selection) = self.term.selection.as_mut() {
            selection.update(point, to_term_side(side));
        }
    }

    fn clear_selection(&mut self) {
        self.term.selection = None;
    }

    fn selection_text(&self) -> Option<String> {
        let selection = self.term.selection.as_ref()?;
        let text = if selection.ty == SelectionType::Block {
            self.block_text()?
        } else {
            self.term.selection_to_string()?
        };
        (!text.is_empty()).then_some(text)
    }

    fn search(&self, query: &str) -> Result<Vec<Match>, VtError> {
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let mut regex =
            RegexSearch::new(&escape_regex(query)).map_err(|e| VtError::Search(e.to_string()))?;
        let start = Point::new(self.term.topmost_line(), Column(0));
        let end = Point::new(self.term.bottommost_line(), self.term.last_column());
        let matches = RegexIter::new(start, end, Direction::Right, &self.term, &mut regex)
            .take(MAX_MATCHES)
            .map(|m| Match {
                start: to_grid_point(*m.start()),
                end: to_grid_point(*m.end()),
            })
            .collect();
        Ok(matches)
    }

    fn link_at(&mut self, at: ViewportPoint) -> Option<Link> {
        let point = self.viewport_to_point(at);
        if let Some(link) = self.hyperlink_at(point) {
            return Some(link);
        }
        if self.link_regex.is_none() {
            self.link_regex = RegexSearch::new(LINK_PATTERN).ok();
        }
        let regex = self.link_regex.as_mut()?;
        let term = &self.term;
        let start = term.line_search_left(point);
        let end = term.line_search_right(point);
        let found = RegexIter::new(start, end, Direction::Right, term, regex)
            .take_while(|m| *m.start() <= point)
            .find(|m| point <= *m.end())?;
        let (start, mut end) = (*found.start(), *found.end());
        let mut target = term.bounds_to_string(start, end);
        while let Some(last) = target.chars().last()
            && is_trailing_punctuation(last, &target)
        {
            target.pop();
            end = end.sub(term, Boundary::Grid, 1);
        }
        if target.is_empty() || point > end {
            return None;
        }
        let kind = if target.as_bytes().get(1) == Some(&b':')
            && target
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphabetic)
        {
            LinkKind::Path
        } else {
            LinkKind::Url
        };
        Some(Link {
            kind,
            target,
            start: to_grid_point(start),
            end: to_grid_point(end),
        })
    }
}

/// Characters that end a sentence around a link rather than belong to it.
fn is_trailing_punctuation(last: char, text: &str) -> bool {
    let unbalanced = |open: char, close: char| {
        text.chars().filter(|c| *c == close).count() > text.chars().filter(|c| *c == open).count()
    };
    match last {
        '.' | ',' | ';' | ':' | '!' | '?' | '\'' => true,
        ')' => unbalanced('(', ')'),
        ']' => unbalanced('[', ']'),
        _ => false,
    }
}

/// Escapes regex syntax so a search query matches literally.
fn escape_regex(query: &str) -> String {
    let mut out = String::with_capacity(query.len());
    for c in query.chars() {
        if matches!(
            c,
            '\\' | '.'
                | '+'
                | '*'
                | '?'
                | '('
                | ')'
                | '|'
                | '['
                | ']'
                | '{'
                | '}'
                | '^'
                | '$'
                | '#'
                | '&'
                | '-'
                | '~'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn to_grid_point(point: Point) -> GridPoint {
    GridPoint::new(
        point.line.0,
        u16::try_from(point.column.0).unwrap_or(u16::MAX),
    )
}

fn frame_flags(flags: Flags, selected: bool) -> CellFlags {
    const MAP: [(Flags, CellFlags); 11] = [
        (Flags::BOLD, CellFlags::BOLD),
        (Flags::ITALIC, CellFlags::ITALIC),
        (Flags::UNDERLINE, CellFlags::UNDERLINE),
        (Flags::DOUBLE_UNDERLINE, CellFlags::DOUBLE_UNDERLINE),
        (Flags::UNDERCURL, CellFlags::UNDERCURL),
        (Flags::DOTTED_UNDERLINE, CellFlags::DOTTED_UNDERLINE),
        (Flags::DASHED_UNDERLINE, CellFlags::DASHED_UNDERLINE),
        (Flags::STRIKEOUT, CellFlags::STRIKEOUT),
        (Flags::HIDDEN, CellFlags::HIDDEN),
        (Flags::WIDE_CHAR, CellFlags::WIDE),
        (Flags::WIDE_CHAR_SPACER, CellFlags::SPACER),
    ];
    let mut out = CellFlags::EMPTY;
    for (from, to) in MAP {
        if flags.contains(from) {
            out.insert(to);
        }
    }
    if flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
        out.insert(CellFlags::SPACER);
    }
    if selected {
        out.insert(CellFlags::SELECTED);
    }
    out
}

fn to_term_shape(shape: CursorShape) -> TermCursorShape {
    match shape {
        CursorShape::Block => TermCursorShape::Block,
        CursorShape::Underline => TermCursorShape::Underline,
        CursorShape::Beam => TermCursorShape::Beam,
        CursorShape::HollowBlock => TermCursorShape::HollowBlock,
    }
}

fn from_term_shape(shape: TermCursorShape) -> Option<CursorShape> {
    match shape {
        TermCursorShape::Block => Some(CursorShape::Block),
        TermCursorShape::Underline => Some(CursorShape::Underline),
        TermCursorShape::Beam => Some(CursorShape::Beam),
        TermCursorShape::HollowBlock => Some(CursorShape::HollowBlock),
        TermCursorShape::Hidden => None,
    }
}

fn to_term_side(side: Side) -> alacritty_terminal::index::Side {
    match side {
        Side::Left => alacritty_terminal::index::Side::Left,
        Side::Right => alacritty_terminal::index::Side::Right,
    }
}

fn to_term_selection(kind: SelectionKind) -> SelectionType {
    match kind {
        SelectionKind::Simple => SelectionType::Simple,
        SelectionKind::Block => SelectionType::Block,
        SelectionKind::Semantic => SelectionType::Semantic,
        SelectionKind::Lines => SelectionType::Lines,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_makes_meta_literal() {
        assert_eq!(escape_regex("a.b*(c)"), r"a\.b\*\(c\)");
        assert_eq!(escape_regex("plain"), "plain");
    }

    #[test]
    fn trailing_punctuation_respects_parentheses() {
        assert!(is_trailing_punctuation('.', "https://a.b/c."));
        assert!(is_trailing_punctuation(')', "https://a.b/c)"));
        assert!(!is_trailing_punctuation(')', "https://a.b/w_(x)"));
        assert!(!is_trailing_punctuation('c', "https://a.b/c"));
    }

    #[test]
    fn color_indices_fall_back_to_palette() {
        let engine = AlacrittyEngine::new(VtConfig::default(), 10, 2);
        let p = Palette::default();
        assert_eq!(engine.color_at(1), p.ansi[1]);
        assert_eq!(engine.color_at(256), p.foreground);
        assert_eq!(engine.color_at(257), p.background);
        assert_eq!(engine.color_at(258), p.cursor);
        assert_eq!(engine.color_at(260), p.ansi[1].dim());
        assert_eq!(engine.color_at(268), p.foreground.dim());
        assert_eq!(engine.color_at(10_000), p.foreground);
    }
}
