//! The notification-area icon (RF-SES-09): the app's own icon beside the
//! clock, a tooltip saying how many panes need the user, and the menu that
//! reopens YIEN or quits it. Windows only; elsewhere every call is a no-op
//! and [`Tray::enabled`] is always false, so callers keep today's behaviour
//! (closing the window quits).
//!
//! ```no_run
//! use yien_platform::tray::{self, TrayEvent};
//!
//! let tray = tray::Tray::new(true, 0)?;
//! tray::set_handler(Some(Box::new(|event| match event {
//!     TrayEvent::Open => println!("open"),
//!     TrayEvent::Quit => println!("quit"),
//! })));
//! tray.set_needs(2);
//! # Ok::<(), yien_platform::tray::TrayError>(())
//! ```

/// What the user did with the icon or its menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    /// A left click or double click on the icon, or `Open YIEN`: reopen the
    /// window.
    Open,
    /// `Quit YIEN`: the real quit, with the window shown again.
    Quit,
}

/// The OS refused a tray call.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TrayError {
    /// The reason, with the call that failed.
    #[error("{call} failed: {message}")]
    Os {
        /// The step that failed, such as `Shell_NotifyIcon`.
        call: &'static str,
        /// What the OS said, in the OS's words.
        message: String,
    },
}

/// Replaces the process-wide handler both the icon's and the menu's events
/// are delivered to, or clears it with `None`. Called once, at start-up; a
/// later call wins. Events with no handler are dropped.
pub fn set_handler(callback: Option<Box<dyn Fn(TrayEvent) + Send + Sync + 'static>>) {
    imp::set_handler(callback);
}

/// What the tooltip says: just `YIEN`, or `YIEN` plus the count of panes
/// that need the user when there is one. No count when the window is the
/// only place the user could be.
///
/// ```
/// use yien_platform::tray::tooltip;
/// assert_eq!(tooltip(0), "YIEN");
/// assert_eq!(tooltip(1), "YIEN — 1 pane needs you");
/// assert_eq!(tooltip(3), "YIEN — 3 panes need you");
/// ```
pub fn tooltip(needs: usize) -> String {
    match needs {
        0 => "YIEN".to_owned(),
        1 => "YIEN — 1 pane needs you".to_owned(),
        n => format!("YIEN — {n} panes need you"),
    }
}

#[cfg(windows)]
mod imp {
    use std::cell::RefCell;
    use std::sync::{Mutex, OnceLock};

    use tray_icon::menu::{Menu, MenuItem};
    use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

    use super::{TrayError, TrayEvent};

    /// The menu's stable ids, matched against the menu events.
    const MENU_OPEN: &str = "yien-tray-open";
    const MENU_QUIT: &str = "yien-tray-quit";

    /// The app icon, rendered from `assets/tray-icon.png` (a 32 × 32
    /// copy of the app's mark).
    const ICON_PNG: &[u8] = include_bytes!("../assets/tray-icon.png");

    pub(super) struct Imp {
        state: RefCell<State>,
    }

    struct State {
        icon: Option<TrayIcon>,
        needs: usize,
    }

    impl Imp {
        pub(super) fn new(enabled: bool, needs: usize) -> Result<Self, TrayError> {
            let this = Self {
                state: RefCell::new(State { icon: None, needs }),
            };
            if enabled {
                this.create()?;
            }
            Ok(this)
        }

        pub(super) fn set_enabled(&self, enabled: bool) -> Result<(), TrayError> {
            match enabled {
                true => self.create(),
                false => {
                    self.state.borrow_mut().icon = None;
                    Ok(())
                }
            }
        }

        pub(super) fn set_needs(&self, needs: usize) -> Result<(), TrayError> {
            {
                self.state.borrow_mut().needs = needs;
            }
            let tooltip = super::tooltip(needs);
            if let Some(icon) = self.state.borrow().icon.as_ref() {
                icon.set_tooltip(Some(&tooltip))
                    .map_err(|error| TrayError::Os {
                        call: "Shell_NotifyIcon(NIM_MODIFY)",
                        message: error.to_string(),
                    })?;
            }
            Ok(())
        }

        pub(super) fn enabled(&self) -> bool {
            self.state.borrow().icon.is_some()
        }

        fn create(&self) -> Result<(), TrayError> {
            if self.state.borrow().icon.is_some() {
                return Ok(());
            }
            let open = MenuItem::with_id(MENU_OPEN, "Open YIEN", true, None);
            let quit = MenuItem::with_id(MENU_QUIT, "Quit YIEN", true, None);
            let menu = Menu::new();
            menu.append(&open).map_err(|error| TrayError::Os {
                call: "muda::Menu::append(Open YIEN)",
                message: error.to_string(),
            })?;
            menu.append(&quit).map_err(|error| TrayError::Os {
                call: "muda::Menu::append(Quit YIEN)",
                message: error.to_string(),
            })?;
            let tooltip = super::tooltip(self.state.borrow().needs);
            let icon = TrayIconBuilder::new()
                .with_tooltip(&tooltip)
                .with_icon(app_icon()?)
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false)
                .build()
                .map_err(|error| TrayError::Os {
                    call: "Shell_NotifyIcon(NIM_ADD)",
                    message: error.to_string(),
                })?;
            self.state.borrow_mut().icon = Some(icon);
            Ok(())
        }
    }

    /// Decodes the embedded icon once per creation.
    fn app_icon() -> Result<Icon, TrayError> {
        let decoder = png::Decoder::new(std::io::Cursor::new(ICON_PNG));
        let mut reader = decoder.read_info().map_err(|error| TrayError::Os {
            call: "png::Decoder",
            message: error.to_string(),
        })?;
        let size = reader.output_buffer_size().ok_or(TrayError::Os {
            call: "png::Decoder::output_buffer_size",
            message: "the icon is too large to hold".to_owned(),
        })?;
        let mut buf = vec![0; size];
        let info = reader.next_frame(&mut buf).map_err(|error| TrayError::Os {
            call: "png::Decoder::next_frame",
            message: error.to_string(),
        })?;
        if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
            return Err(TrayError::Os {
                call: "png::Decoder",
                message: format!(
                    "the icon is {:?}/{:?}, not rgba8",
                    info.color_type, info.bit_depth
                ),
            });
        }
        buf.truncate(info.buffer_size());
        Icon::from_rgba(buf, info.width, info.height).map_err(|error| TrayError::Os {
            call: "Icon::from_rgba",
            message: error.to_string(),
        })
    }

    /// The tray event the app acts on, if the raw one is one we handle.
    fn tray_event(event: tray_icon::TrayIconEvent) -> Option<TrayEvent> {
        use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent};
        match event {
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            }
            | TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } => Some(TrayEvent::Open),
            _ => None,
        }
    }

    /// The menu event the app acts on, if the raw one is one we handle.
    fn menu_event(event: tray_icon::menu::MenuEvent) -> Option<TrayEvent> {
        match event.id.as_ref() {
            MENU_OPEN => Some(TrayEvent::Open),
            MENU_QUIT => Some(TrayEvent::Quit),
            _ => None,
        }
    }

    /// The tray event callback a living tray registers.
    type Handler = Box<dyn Fn(TrayEvent) + Send + Sync>;

    /// The one handler both raw event streams report through. Installed on
    /// the first handler call; it delegates to whatever callback the living
    /// tray registered.
    static HANDLER: OnceLock<Mutex<Option<Handler>>> = OnceLock::new();

    fn handler() -> &'static Mutex<Option<Handler>> {
        HANDLER.get_or_init(|| {
            tray_icon::TrayIconEvent::set_event_handler(Some(|event| {
                if let Some(fired) = tray_event(event) {
                    fire(fired);
                }
            }));
            tray_icon::menu::MenuEvent::set_event_handler(Some(|event| {
                if let Some(fired) = menu_event(event) {
                    fire(fired);
                }
            }));
            Mutex::new(None)
        })
    }

    fn fire(event: TrayEvent) {
        if let Ok(guard) = handler().lock()
            && let Some(callback) = guard.as_deref()
        {
            callback(event);
        }
    }

    pub(super) fn set_handler(callback: Option<Handler>) {
        *handler().lock().unwrap_or_else(|e| e.into_inner()) = callback;
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn rf_ses_09_a_left_click_and_double_click_open_but_the_others_do_not() {
            let id = tray_icon::TrayIconId::new("t");
            let rect = tray_icon::Rect::default();
            let position = tray_icon::dpi::PhysicalPosition::new(0.0, 0.0);
            for (button, state, wanted) in [
                (
                    tray_icon::MouseButton::Left,
                    tray_icon::MouseButtonState::Up,
                    Some(TrayEvent::Open),
                ),
                (
                    tray_icon::MouseButton::Left,
                    tray_icon::MouseButtonState::Down,
                    None,
                ),
                (
                    tray_icon::MouseButton::Right,
                    tray_icon::MouseButtonState::Up,
                    None,
                ),
                (
                    tray_icon::MouseButton::Middle,
                    tray_icon::MouseButtonState::Up,
                    None,
                ),
            ] {
                assert_eq!(
                    tray_event(tray_icon::TrayIconEvent::Click {
                        id: id.clone(),
                        rect,
                        position,
                        button,
                        button_state: state,
                    }),
                    wanted
                );
            }
            for button in [
                tray_icon::MouseButton::Left,
                tray_icon::MouseButton::Right,
                tray_icon::MouseButton::Middle,
            ] {
                let wanted = (button == tray_icon::MouseButton::Left).then_some(TrayEvent::Open);
                assert_eq!(
                    tray_event(tray_icon::TrayIconEvent::DoubleClick {
                        id: id.clone(),
                        rect,
                        position,
                        button,
                    }),
                    wanted
                );
            }
        }

        #[test]
        fn rf_ses_09_the_menu_opens_and_quits() {
            for (id, wanted) in [
                (MENU_OPEN, Some(TrayEvent::Open)),
                (MENU_QUIT, Some(TrayEvent::Quit)),
                ("other", None),
            ] {
                assert_eq!(
                    menu_event(tray_icon::menu::MenuEvent {
                        id: tray_icon::menu::MenuId::new(id),
                    }),
                    wanted
                );
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::{TrayError, TrayEvent};

    pub(super) struct Imp;

    impl Imp {
        pub(super) fn new(_enabled: bool, _needs: usize) -> Result<Self, TrayError> {
            Ok(Self)
        }

        pub(super) fn set_enabled(&self, _enabled: bool) -> Result<(), TrayError> {
            Ok(())
        }

        pub(super) fn set_needs(&self, _needs: usize) -> Result<(), TrayError> {
            Ok(())
        }

        pub(super) fn enabled(&self) -> bool {
            false
        }
    }

    pub(super) fn set_handler(_callback: Option<Box<dyn Fn(TrayEvent) + Send + Sync>>) {}
}

/// The notification-area icon, while it lives. On other platforms the calls
/// are no-ops and the icon never exists.
pub struct Tray {
    imp: imp::Imp,
}

impl Tray {
    /// Prepares the icon, creating it right away when `enabled`. `needs` is
    /// the count the tooltip starts with. The events the icon and its menu
    /// produce go to the handler set with [`set_handler`].
    pub fn new(enabled: bool, needs: usize) -> Result<Self, TrayError> {
        imp::Imp::new(enabled, needs).map(|imp| Self { imp })
    }

    /// Shows the icon (creating it on first use) or removes it.
    pub fn set_enabled(&self, enabled: bool) -> Result<(), TrayError> {
        self.imp.set_enabled(enabled)
    }

    /// Sets the count the tooltip carries; 0 clears it. Safe to call while
    /// the icon does not exist: the count applies when it is created.
    pub fn set_needs(&self, needs: usize) -> Result<(), TrayError> {
        self.imp.set_needs(needs)
    }

    /// Whether the icon is in the notification area now.
    pub fn enabled(&self) -> bool {
        self.imp.enabled()
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        imp::set_handler(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rf_ses_09_the_tooltip_says_only_what_it_knows() {
        assert_eq!(tooltip(0), "YIEN");
        assert_eq!(tooltip(1), "YIEN — 1 pane needs you");
        assert_eq!(tooltip(9), "YIEN — 9 panes need you");
    }
}
