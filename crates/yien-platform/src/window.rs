//! Showing and hiding the main window, without quitting the app
//! (RF-SES-08, RF-SES-09): closing the window hides it and YIEN keeps
//! running; the notification-area icon or a second launch brings it back.
//!
//! Only Windows can really hide a window; elsewhere [`show`] and [`hide`]
//! are no-ops and [`is_visible`] answers true, so callers keep today's
//! behaviour.
//!
//! ```
//! // Nothing was hidden yet: the call works on every platform.
//! yien_platform::window::show(0, false);
//! ```

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

#[cfg(not(windows))]
mod imp {
    pub(super) fn hide(_hwnd: isize) -> bool {
        false
    }

    pub(super) fn show(_hwnd: isize, _activate: bool) {}

    pub(super) fn is_visible(_hwnd: isize) -> bool {
        true
    }
}

/// Hides the window, without touching the app. The pane processes and the
/// core stay alive. Returns whether the window is now hidden.
pub fn hide(hwnd: isize) -> bool {
    imp::hide(hwnd)
}

/// Shows the window again: a hidden window is shown without activating; a
/// minimized one is restored — with the foreground when `activate`, without
/// it otherwise. Does nothing when there is no window handle, or when the
/// window is already shown and not minimized. Never steals the activation
/// when `activate` is false.
pub fn show(hwnd: isize, activate: bool) {
    imp::show(hwnd, activate)
}

/// Whether the window is shown on the screen. Minimized counts as shown;
/// hidden to the notification area does not.
pub fn is_visible(hwnd: isize) -> bool {
    imp::is_visible(hwnd)
}
