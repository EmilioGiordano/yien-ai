//! The window's visibility, over `yien-platform` (RF-SES-09).

use std::sync::Arc;
use std::sync::atomic::{AtomicIsize, Ordering};

use yien_ui::OsWindow;

/// [`OsWindow`] on the platform layer. The handle starts at 0 until GPUI
/// has made the window, so calls before that do nothing.
pub(crate) struct WindowOs {
    /// The window's handle once it exists; shared with the attention
    /// service and the single-instance check.
    window: Arc<AtomicIsize>,
}

impl WindowOs {
    pub(crate) fn new(window: Arc<AtomicIsize>) -> Self {
        Self { window }
    }

    fn hwnd(&self) -> isize {
        self.window.load(Ordering::Acquire)
    }
}

impl OsWindow for WindowOs {
    fn hide(&self) -> bool {
        let hwnd = self.hwnd();
        if hwnd == 0 {
            return false;
        }
        let hidden = yien_platform::window::hide(hwnd);
        if !hidden {
            tracing::warn!("the window could not be hidden");
        }
        hidden
    }

    fn show(&self) {
        let hwnd = self.hwnd();
        if hwnd == 0 {
            return;
        }
        yien_platform::window::show(hwnd, true);
    }

    fn visible(&self) -> bool {
        let hwnd = self.hwnd();
        hwnd != 0 && yien_platform::window::is_visible(hwnd)
    }
}
