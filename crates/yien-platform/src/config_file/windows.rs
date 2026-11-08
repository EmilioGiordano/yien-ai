//! `ShellExecuteW`: the file's default app, or the configured editor.

#![allow(unsafe_code)]

use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
use windows::Win32::UI::Shell::{SE_ERR_NOASSOC, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::PCWSTR;

use super::quote_arg;

fn wide(s: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(Some(0)).collect()
}

/// Runs on the caller's own thread, which it initialises for COM as
/// `ShellExecuteW` asks (some handlers are COM objects).
pub(super) fn open(path: &Path, command: Option<&[String]>) -> Result<(), String> {
    // SAFETY: plain call on a thread this module owns; the matching
    // CoUninitialize runs below when it succeeded.
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
    let result = match command {
        Some([program, args @ ..]) => {
            let mut params: Vec<String> = args.iter().map(|a| quote_arg(a)).collect();
            params.push(quote_arg(&path.to_string_lossy()));
            execute(std::ffi::OsStr::new(program), Some(&params.join(" ")))
        }
        _ => match execute(path.as_os_str(), None) {
            // No app is set for .toml files: Notepad ships with Windows.
            Err(code) if code == SE_ERR_NOASSOC as usize => execute(
                std::ffi::OsStr::new("notepad.exe"),
                Some(&quote_arg(&path.to_string_lossy())),
            ),
            other => other,
        },
    };
    if com.is_ok() {
        // SAFETY: pairs the successful CoInitializeEx above, same thread.
        unsafe { CoUninitialize() };
    }
    result.map_err(|code| format!("ShellExecuteW returned {code}"))
}

/// `Err` carries the value `ShellExecuteW` returned, 32 or less on failure.
fn execute(file: &std::ffi::OsStr, params: Option<&str>) -> Result<(), usize> {
    let verb = wide("open");
    let file = wide(file);
    let params = params.map(wide);
    let params_ptr = params
        .as_ref()
        .map_or(PCWSTR::null(), |p| PCWSTR(p.as_ptr()));
    // SAFETY: every string is NUL-terminated and outlives the call.
    let code = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            params_ptr,
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    }
    .0 as usize;
    if code > 32 { Ok(()) } else { Err(code) }
}
