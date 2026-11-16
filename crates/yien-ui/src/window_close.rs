//! What a request to close the window does (RF-SES-08, RF-SES-09,
//! RF-SES-13): with the default settings and a live notification-area icon,
//! closing hides it and YIEN keeps running — a turn in progress is never
//! cut (RF-SES-08). Without the setting, or without the icon, closing is
//! what it always was: ask first while agents work, then quit.

/// What closing the window does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CloseMode {
    /// Hide the window to the notification area; nothing quits.
    Hide,
    /// Quit as before: ask first while agents work, then quit.
    Quit,
}

/// The close button, Alt+F4 and the `window.close` test action. `icon_live`
/// is what the OS layer reports about the icon being in the notification
/// area right now: without it there would be no way back short of
/// launching YIEN again.
pub(crate) fn close_mode(close_to_tray: bool, icon_live: bool) -> CloseMode {
    if close_to_tray && icon_live {
        CloseMode::Hide
    } else {
        CloseMode::Quit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rf_ses_09_the_default_is_to_hide_to_the_tray_not_to_quit() {
        assert_eq!(close_mode(true, true), CloseMode::Hide);
    }

    #[test]
    fn rf_ses_09_the_user_can_keep_the_old_behaviour() {
        assert_eq!(close_mode(false, true), CloseMode::Quit);
    }

    #[test]
    fn rf_ses_09_no_icon_means_closing_quits_as_before() {
        // The icon failed to start, or was turned off: hide would strand the
        // window, so the close button quits.
        assert_eq!(close_mode(true, false), CloseMode::Quit);
        assert_eq!(close_mode(false, false), CloseMode::Quit);
    }
}
