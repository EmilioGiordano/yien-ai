//! The window's toasts, taskbar badge and flash, over `yien-platform`.

use yien_platform::attention::{AttentionError, OsAttention as Platform, Toast};
use yien_ui::{OsAttention, ToastText};

/// [`OsAttention`] on the platform layer. Failures go to the log at debug
/// level with the step and error code only (RF-ATN-16): the window's red
/// mark and counter do not depend on them.
pub(crate) struct Attention(Platform);

impl Attention {
    /// Takes YIEN's app id for the process; call it before the window opens.
    pub(crate) fn new() -> Self {
        Self(Platform::new())
    }
}

fn log(result: Result<(), AttentionError>) {
    if let Err(error) = result {
        tracing::debug!("{error}");
    }
}

impl OsAttention for Attention {
    fn attach(&self, hwnd: isize) {
        self.0.attach(hwnd);
    }

    fn toast(&self, toast: &ToastText) {
        log(self.0.toast(&Toast {
            title: toast.title.clone(),
            body: toast.body.clone(),
            launch: toast.launch.clone(),
            tag: toast.tag.clone(),
        }));
    }

    fn badge(&self, count: usize) {
        let set = self.0.set_badge(count);
        if set.is_ok() {
            tracing::debug!(count, "taskbar badge set");
        }
        log(set);
    }

    fn flash(&self) {
        log(self.0.flash());
    }

    fn on_activate(&self, callback: Box<dyn Fn(String) + Send + Sync>) {
        self.0.on_activate(callback);
    }
}
