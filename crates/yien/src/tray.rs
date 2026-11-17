//! The notification-area icon, over `yien-platform` (RF-SES-09): the app's
//! own icon, the count of panes that need the user in its tooltip, and
//! `Open YIEN` / `Quit YIEN`. Hiding the window is offered only while the
//! icon is in the notification area ([`OsTray::live`]); without it, the
//! close button quits the app as it always did. Failures go to the log and
//! never stop the app.

use std::cell::RefCell;

use yien_ui::{OsTray, TrayEvent};

pub(crate) struct TrayOs {
    tray: RefCell<Option<yien_platform::tray::Tray>>,
}

impl TrayOs {
    /// Prepares the icon machinery; the icon itself is created when the
    /// settings are applied with `window.close_to_tray` on.
    pub(crate) fn new() -> Self {
        let created = yien_platform::tray::Tray::new(false, 0);
        let tray = match created {
            Ok(tray) => Some(tray),
            Err(error) => {
                tracing::warn!("the notification-area icon did not start: {error}");
                None
            }
        };
        Self {
            tray: RefCell::new(tray),
        }
    }
}

impl OsTray for TrayOs {
    fn live(&self) -> bool {
        self.tray
            .borrow()
            .as_ref()
            .is_some_and(|tray| tray.enabled())
    }

    fn on_event(&self, callback: Box<dyn Fn(TrayEvent) + Send + Sync>) {
        yien_platform::tray::set_handler(Some(Box::new(move |event| {
            callback(match event {
                yien_platform::tray::TrayEvent::Open => TrayEvent::Open,
                yien_platform::tray::TrayEvent::Quit => TrayEvent::Quit,
            });
        })));
    }

    fn set_enabled(&self, enabled: bool) {
        let tray = self.tray.borrow();
        let Some(tray) = tray.as_ref() else {
            return;
        };
        if let Err(error) = tray.set_enabled(enabled) {
            tracing::warn!(
                enabled,
                "the notification-area icon did not follow the setting: {error}"
            );
        }
    }

    fn set_needs(&self, count: usize) {
        let tray = self.tray.borrow();
        let Some(tray) = tray.as_ref() else {
            return;
        };
        if let Err(error) = tray.set_needs(count) {
            tracing::warn!("the tray icon's needs count could not be set: {error}");
        }
    }
}
