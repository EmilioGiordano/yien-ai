//! The window's toasts, taskbar badge and flash, over `yien-platform`.

use std::sync::Arc;
use std::sync::atomic::{AtomicIsize, Ordering};

use yien_platform::attention::{AttentionError, OsAttention as Platform, Toast};
use yien_ui::{OsAttention, ToastText};

/// [`OsAttention`] on the platform layer. Failures go to the log at debug
/// level with the step and error code only (RF-ATN-16): the window's red
/// mark and counter do not depend on them.
pub(crate) struct Attention {
    os: Platform,
    /// The window's handle once it exists, 0 before; shared with whoever
    /// else must bring the window forward.
    window: Arc<AtomicIsize>,
}

impl Attention {
    /// Takes YIEN's app id for the process; call it before the window opens.
    /// `window` receives the window's handle when it attaches.
    pub(crate) fn new(window: Arc<AtomicIsize>) -> Self {
        Self {
            os: Platform::new(),
            window,
        }
    }
}

fn log(result: Result<(), AttentionError>) {
    if let Err(error) = result {
        tracing::debug!("{error}");
    }
}

impl OsAttention for Attention {
    fn attach(&self, hwnd: isize) {
        self.window.store(hwnd, Ordering::Release);
        self.os.attach(hwnd);
    }

    fn toast(&self, toast: &ToastText) {
        log(self.os.toast(&Toast {
            title: toast.title.clone(),
            body: toast.body.clone(),
            launch: toast.launch.clone(),
            tag: toast.tag.clone(),
        }));
    }

    fn badge(&self, count: usize) {
        let set = self.os.set_badge(count);
        if set.is_ok() {
            tracing::debug!(count, "taskbar badge set");
        }
        log(set);
    }

    fn flash(&self) {
        log(self.os.flash());
    }

    fn on_activate(&self, callback: Box<dyn Fn(String) + Send + Sync>) {
        self.os.on_activate(callback);
    }
}
