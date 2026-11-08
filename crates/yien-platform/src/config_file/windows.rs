//! `ShellExecuteExW`: the file's default app, or the configured editor.

#![allow(unsafe_code)]

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::Win32::Foundation::{ERROR_NO_ASSOCIATION, WIN32_ERROR};
use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
use windows::Win32::UI::Shell::{
    SE_ERR_NOASSOC, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::PCWSTR;

use super::{batch_safe, is_batch, quote_arg, quote_path};

fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(Some(0)).collect()
}

/// Why a launch failed.
enum Failure {
    NoAssociation,
    Other(String),
}

/// Runs on the caller's own thread, which it initialises for COM as
/// `ShellExecuteExW` asks (some handlers are COM objects).
pub(super) fn open(path: &Path, command: Option<&[String]>) -> Result<(), String> {
    if let Some([program, args @ ..]) = command
        && is_batch(program)
        && !batch_safe(args, path)
    {
        // cmd.exe parses a .cmd or .bat's arguments again, and no quoting
        // protects `%`, `!` or `"` from it.
        return Err(format!(
            "{program} is a batch file, and its arguments or the file's path hold a \
             character cmd.exe would interpret"
        ));
    }
    // SAFETY: plain call on a thread this module owns; the matching
    // CoUninitialize runs below when it succeeded.
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
    let result = match command {
        Some([program, args @ ..]) => {
            let mut params: Vec<String> = args.iter().map(|a| quote_arg(a)).collect();
            params.push(quote_path(path));
            execute(OsStr::new(program), Some(&params.join(" ")))
        }
        _ => match execute(path.as_os_str(), None) {
            // No app is set for .toml files: Notepad ships with Windows.
            Err(Failure::NoAssociation) => {
                execute(OsStr::new("notepad.exe"), Some(&quote_path(path)))
            }
            other => other,
        },
    };
    if com.is_ok() {
        // SAFETY: pairs the successful CoInitializeEx above, same thread.
        unsafe { CoUninitialize() };
    }
    result.map_err(|failure| match failure {
        Failure::NoAssociation => "no app is set to open it".to_owned(),
        Failure::Other(why) => why,
    })
}

/// `SEE_MASK_NOASYNC` because the calling thread ends right after the call;
/// `SEE_MASK_FLAG_NO_UI` so a missing association comes back as an error
/// to handle, not as a dialog.
fn execute(file: &OsStr, params: Option<&str>) -> Result<(), Failure> {
    let verb = wide("open");
    let file = wide(file);
    let params = params.map(wide);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: params
            .as_ref()
            .map_or(PCWSTR::null(), |p| PCWSTR(p.as_ptr())),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: `info` carries its own size, and every string it points to is
    // NUL-terminated and outlives the call.
    match unsafe { ShellExecuteExW(&mut info) } {
        Ok(()) => Ok(()),
        Err(error) => {
            let no_assoc = info.hInstApp.0 as usize == SE_ERR_NOASSOC as usize
                || WIN32_ERROR::from_error(&error) == Some(ERROR_NO_ASSOCIATION);
            if no_assoc {
                Err(Failure::NoAssociation)
            } else {
                Err(Failure::Other(error.message()))
            }
        }
    }
}
