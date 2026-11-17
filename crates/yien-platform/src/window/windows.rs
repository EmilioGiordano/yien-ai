//! The Windows calls: `ShowWindowAsync` on the app's own window. Hiding is
//! `SW_HIDE`. Showing puts back a hidden window without activating it, and
//! restores a minimized one — `SW_RESTORE` when the caller may take the
//! foreground, `SW_SHOWNOACTIVATE` otherwise, which shows the window at its
//! last normal size and position without activating it. The caller decides
//! about the foreground.

#![allow(unsafe_code)]

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    IsIconic, IsWindow, IsWindowVisible, SW_HIDE, SW_RESTORE, SW_SHOWNOACTIVATE, ShowWindowAsync,
};
pub(super) fn hide(hwnd: isize) -> bool {
    if hwnd == 0 {
        return false;
    }
    let hwnd = hwnd as HWND;
    // SAFETY: `hwnd` is read only; a dead window answers 0.
    if unsafe { IsWindow(hwnd) } == 0 {
        return false;
    }
    // SAFETY: `hwnd` is this process's own window while it is alive; the
    // call only posts a message to it and fails harmlessly if it is gone.
    unsafe {
        ShowWindowAsync(hwnd, SW_HIDE);
    }
    true
}

pub(super) fn show(hwnd: isize, activate: bool) {
    if hwnd == 0 {
        return;
    }
    let hwnd = hwnd as HWND;
    // SAFETY: `hwnd` is this process's own window while it is alive; these
    // calls only read it or post to it, and fail harmlessly if it is gone.
    unsafe {
        if IsIconic(hwnd) != 0 {
            let command = if activate {
                SW_RESTORE
            } else {
                SW_SHOWNOACTIVATE
            };
            ShowWindowAsync(hwnd, command);
        } else if IsWindowVisible(hwnd) == 0 {
            ShowWindowAsync(hwnd, SW_SHOWNOACTIVATE);
        }
    }
}

pub(super) fn is_visible(hwnd: isize) -> bool {
    if hwnd == 0 {
        return false;
    }
    let hwnd = hwnd as HWND;
    // SAFETY: `hwnd` is read only; a dead window answers 0.
    unsafe { IsWindowVisible(hwnd) != 0 }
}
