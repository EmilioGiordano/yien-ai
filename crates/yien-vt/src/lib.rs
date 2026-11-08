//! The terminal emulation core one pane owns: it parses PTY bytes, keeps the
//! grid and the in-memory (hot) scrollback, extracts agent signals from OSC
//! sequences, and exposes the visible rows as plain data so the UI never
//! touches emulator types.
//!
//! No threads, no OS calls, no window: the pane actor feeds bytes in and
//! writes [`VtOutput::replies`] back to the PTY.
//!
//! ```
//! use yien_vt::{AlacrittyEngine, OscSignal, VtConfig, VtEngine};
//!
//! let mut engine = AlacrittyEngine::new(VtConfig::default(), 80, 24);
//! let out = engine.feed(b"\x1b]9;done\x07ready");
//! assert!(matches!(&out.signals[..], [OscSignal::Notify { .. }]));
//! assert_eq!(engine.frame().line_text(0), "ready");
//! ```

#![forbid(unsafe_code)]

mod alacritty;
pub mod osc_scan;
mod palette;

use std::time::Duration;

pub use alacritty::{AlacrittyEngine, MAX_MATCHES, MAX_SCROLLBACK};
pub use osc_scan::{OscSignal, PromptMark};
pub use palette::{Palette, Rgb};
pub use yien_proto::frame::{
    Cell, CellFlags, Cursor, CursorShape, Frame, GridPoint, Link, LinkKind, Match, Modes,
    MouseEncoding, MouseTracking, SelectionKind, Side, Size, ViewportPoint,
};

/// How long a synchronized update (mode 2026) may stay open before the owner
/// should call [`VtEngine::flush_sync`]. The same value alacritty uses.
pub const SYNC_UPDATE_TIMEOUT: Duration = Duration::from_millis(150);

/// The default number of scrollback lines (RF-PAN-07).
pub const DEFAULT_SCROLLBACK: usize = 10_000;

/// How an engine is set up.
///
/// ```
/// use yien_vt::VtConfig;
///
/// let config = VtConfig { scrollback_lines: 5_000, ..VtConfig::default() };
/// assert_eq!(config.scrollback_lines, 5_000);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VtConfig {
    /// Scrollback lines kept in memory, capped at [`MAX_SCROLLBACK`].
    pub scrollback_lines: usize,
    /// The colours frames are resolved with.
    pub palette: Palette,
    /// The cursor shape until a program asks for another.
    pub cursor_shape: CursorShape,
}

impl Default for VtConfig {
    fn default() -> Self {
        Self {
            scrollback_lines: DEFAULT_SCROLLBACK,
            palette: Palette::default(),
            cursor_shape: CursorShape::Block,
        }
    }
}

/// A change of the window title requested by the program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TitleChange {
    /// Show this title.
    Set(String),
    /// Go back to the default title.
    Reset,
}

/// What feeding bytes produced, besides the grid changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VtOutput {
    /// Bytes to write back to the PTY, in order: answers to device status,
    /// device attributes and colour queries. They must never be dropped:
    /// ConPTY blocks the child until `ESC [ 6 n` is answered.
    pub replies: Vec<u8>,
    /// The last title change, as the terminal shows it (title stack included).
    pub title: Option<TitleChange>,
    /// Whether the program rang the bell.
    pub bell: bool,
    /// Text the program asked to copy to the clipboard (OSC 52).
    pub clipboard: Option<String>,
    /// OSC signals found in the bytes, in order, for agent state detection.
    pub signals: Vec<OscSignal>,
}

/// Errors an engine reports.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VtError {
    /// The search query could not be compiled, usually because it is too long.
    #[error("search query rejected: {0}")]
    Search(String),
}

/// The terminal emulator behind a pane (ADR-04). The pane actor is its only
/// owner (ADR-06).
///
/// Positions come in two kinds: [`ViewportPoint`] for what the user points at,
/// and [`GridPoint`] for buffer positions that do not move with scrolling.
///
/// ```
/// use yien_vt::{AlacrittyEngine, SelectionKind, Side, ViewportPoint, VtConfig, VtEngine};
///
/// let mut engine = AlacrittyEngine::new(VtConfig::default(), 20, 3);
/// engine.feed(b"copy me");
/// engine.start_selection(ViewportPoint::new(0, 0), Side::Left, SelectionKind::Simple);
/// engine.update_selection(ViewportPoint::new(0, 3), Side::Right);
/// assert_eq!(engine.selection_text().as_deref(), Some("copy"));
/// assert_eq!(engine.search("me").unwrap().len(), 1);
/// ```
pub trait VtEngine: Send {
    /// Parses PTY output. Bytes may be cut anywhere, including inside an
    /// escape sequence or a UTF-8 character.
    fn feed(&mut self, bytes: &[u8]) -> VtOutput;

    /// Whether a synchronized update is holding output back. The owner calls
    /// [`VtEngine::flush_sync`] once it has been open for
    /// [`SYNC_UPDATE_TIMEOUT`].
    fn sync_pending(&self) -> bool;

    /// Changes each time a synchronized update is opened, reopened or
    /// extended. When it differs from the value the owner saw when it started
    /// its [`SYNC_UPDATE_TIMEOUT`] timer, the owner restarts the timer.
    fn sync_generation(&self) -> u64;

    /// Applies the output held by an open synchronized update and closes it.
    fn flush_sync(&mut self) -> VtOutput;

    /// Changes the size, reflowing wrapped lines. Sizes below 2 columns or
    /// 1 row are raised to that.
    fn resize(&mut self, cols: u16, rows: u16);

    /// The current size.
    fn size(&self) -> Size;

    /// Resolves colours with `palette` from the next frame on. The screen,
    /// the scrollback and the colours a program set (OSC 4/10/11, truecolor)
    /// are kept.
    fn set_palette(&mut self, palette: Palette);

    /// The visible rows at the current scroll position.
    fn frame(&self) -> Frame;

    /// The modes the input side needs to encode keys, paste and mouse.
    fn modes(&self) -> Modes;

    /// Scrolls the viewport; positive goes back into the history. Clamped at
    /// both ends.
    fn scroll(&mut self, delta: i32);

    /// Returns the viewport to the live screen.
    fn scroll_to_bottom(&mut self);

    /// Scrolls just enough to bring `at` into view, for example a search match.
    fn scroll_to(&mut self, at: GridPoint);

    /// Starts a selection at a viewport cell, replacing any previous one.
    fn start_selection(&mut self, at: ViewportPoint, side: Side, kind: SelectionKind);

    /// Extends the selection to a viewport cell.
    fn update_selection(&mut self, at: ViewportPoint, side: Side);

    /// Drops the selection.
    fn clear_selection(&mut self);

    /// The selected text, or `None` when nothing is selected.
    fn selection_text(&self) -> Option<String>;

    /// Finds `query` as literal text in the scrollback and the screen, top to
    /// bottom, up to [`MAX_MATCHES`]. Case-insensitive unless the query has an
    /// uppercase letter. An empty query finds nothing.
    fn search(&self, query: &str) -> Result<Vec<Match>, VtError>;

    /// The link under a viewport cell: an OSC 8 hyperlink, a URL or an
    /// absolute Windows path.
    fn link_at(&mut self, at: ViewportPoint) -> Option<Link>;
}
