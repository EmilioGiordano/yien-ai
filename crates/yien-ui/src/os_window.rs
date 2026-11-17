//! The OS side of the window's visibility (RF-SES-09): how the close
//! button hides the window without ending anything, and how it comes back.
//! The window itself belongs to GPUI; these are the calls GPUI cannot do.
//!
//! ```
//! use yien_ui::OsWindow;
//!
//! struct Nowhere;
//! impl OsWindow for Nowhere {
//!     fn hide(&self) -> bool { false }
//!     fn show(&self) {}
//!     fn visible(&self) -> bool { true }
//! }
//! ```
/// The OS side of the window's visibility. Every call is best effort; an
/// implementation logs its own failures and never panics.
pub trait OsWindow {
    /// Hides the window. The app, the core and every pane keep running.
    /// Returns whether the window is now hidden.
    fn hide(&self) -> bool;
    /// Shows the window again: restored if it was minimized or hidden.
    fn show(&self);
    /// Whether the window is shown on the screen. Minimized counts as
    /// shown; hidden to the notification area does not.
    fn visible(&self) -> bool;
}
