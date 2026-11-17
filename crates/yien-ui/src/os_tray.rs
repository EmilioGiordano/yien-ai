//! The OS side of the notification-area icon (RF-SES-09): what the user
//! does with it, and what YIEN shows there. The icon itself is a Win32
//! notification-area icon on Windows and does not exist elsewhere.

/// What the user did with the icon or its menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    /// A left click or double click on the icon, or `Open YIEN`: reopen
    /// the window.
    Open,
    /// `Quit YIEN`: the real quit, with the window shown again.
    Quit,
}

/// The OS side of the icon. Every call is best effort; an implementation
/// logs its own failures and never panics.
///
/// ```
/// use yien_ui::{OsTray, TrayEvent};
///
/// struct Nowhere;
/// impl OsTray for Nowhere {
///     fn live(&self) -> bool { false }
///     fn on_event(&self, _callback: Box<dyn Fn(TrayEvent) + Send + Sync>) {}
///     fn set_enabled(&self, _enabled: bool) {}
///     fn set_needs(&self, _count: usize) {}
/// }
/// ```
pub trait OsTray {
    /// Whether the icon is in the notification area now: the close button
    /// only hides the window while it is, so there is always a way back
    /// short of launching YIEN again.
    fn live(&self) -> bool;
    /// Registers the callback that receives the icon's events, on any
    /// thread. Called once, before the window opens.
    fn on_event(&self, callback: Box<dyn Fn(TrayEvent) + Send + Sync>);
    /// Creates the icon, or removes it. Creating an existing icon, or
    /// removing an absent one, does nothing.
    fn set_enabled(&self, enabled: bool);
    /// Sets the "panes that need you" count the tooltip carries; 0 clears
    /// it. Applies when the icon is created too.
    fn set_needs(&self, count: usize);
}
