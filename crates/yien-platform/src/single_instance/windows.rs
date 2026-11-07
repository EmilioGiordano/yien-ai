//! The Windows claim: a named mutex in the session's namespace, and a named
//! pipe, admitting only the current user, that carries the show request.

#![allow(unsafe_code)]

use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, ERROR_NO_DATA, ERROR_PIPE_CONNECTED, GetLastError, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_INBOUND, ReadFile,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeServerProcessId,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CreateMutexW, GetStartupInfoW, STARTF_USESHOWWINDOW, STARTUPINFOW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, FLASHW_TIMERNOFG, FLASHW_TRAY, FLASHWINFO, FlashWindowEx, IsIconic,
    SW_HIDE, SW_MINIMIZE, SW_RESTORE, SW_SHOWMINIMIZED, SW_SHOWMINNOACTIVE, SW_SHOWNA,
    SW_SHOWNOACTIVATE, SetForegroundWindow, ShowWindowAsync,
};

use super::{Claim, Instance, InstanceError, ShowRequest};
use crate::hook_pipe::windows::{SecurityDescriptor, user_only_descriptor};

/// The request bytes: show and take the foreground, or only show.
const ACTIVATE: u8 = b'A';
const SHOW: u8 = b'S';

/// How long a later start waits for the running one's pipe: it may hold the
/// mutex and not have created the pipe yet.
const CONNECT_WAIT: Duration = Duration::from_secs(2);

/// Pause after a failed connect, so a repeating failure cannot spin.
const BACKOFF: Duration = Duration::from_millis(50);

pub(super) struct Guard {
    _mutex: OwnedHandle,
    /// The pipe, until [`Guard::serve`] hands it to its thread.
    pipe: Option<OwnedHandle>,
}

fn mutex_name(key: &str) -> String {
    format!(r"Local\yien-instance-{key}")
}

fn pipe_name(key: &str) -> String {
    format!(r"\\.\pipe\yien-instance-{key}")
}

pub(super) fn claim(_dir: &std::path::Path, key: &str) -> Result<Claim, InstanceError> {
    let name = wide(&mutex_name(key));
    // SAFETY: null attributes are allowed and `name` is NUL-terminated.
    let handle = unsafe { CreateMutexW(null(), 0, name.as_ptr()) };
    // Read before any other call can overwrite it.
    // SAFETY: plain read of this thread's last error.
    let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if handle.is_null() {
        return Err(last_error("CreateMutexW"));
    }
    // SAFETY: CreateMutexW succeeded, so the handle is ours to close.
    let mutex = unsafe { OwnedHandle::from_raw_handle(handle) };
    if existed {
        return Ok(Claim::Running {
            delivered: ask_to_show(key),
        });
    }
    // The pipe exists from here on, so a later start can connect and write
    // before `serve` runs; the bytes wait in the pipe.
    let pipe = match create_pipe(key) {
        Ok(pipe) => Some(pipe),
        Err(error) => {
            tracing::warn!("a later start cannot ask for the window: {error}");
            None
        }
    };
    Ok(Claim::First(Instance {
        guard: Guard {
            _mutex: mutex,
            pipe,
        },
    }))
}

impl Guard {
    pub(super) fn serve(mut self, on_show: Box<dyn Fn(ShowRequest) + Send>) -> Self {
        if let Some(pipe) = self.pipe.take() {
            let started = thread::Builder::new()
                .name("yien-instance".into())
                .spawn(move || serve(&pipe, &*on_show));
            if let Err(error) = started {
                tracing::warn!("a later start cannot ask for the window: {error}");
            }
        }
        self
    }
}

/// Answers one later start at a time, for as long as the process runs. The
/// one instance is reused, so the name is never free for someone else.
fn serve(pipe: &OwnedHandle, on_show: &dyn Fn(ShowRequest)) {
    let handle = pipe.as_raw_handle();
    loop {
        // SAFETY: `pipe` is a live, non-overlapped pipe instance.
        let connected = unsafe { ConnectNamedPipe(handle, null_mut()) } != 0
            || matches!(
                io::Error::last_os_error().raw_os_error().map(|c| c as u32),
                Some(ERROR_PIPE_CONNECTED | ERROR_NO_DATA)
            );
        if connected {
            let mut buf = [0u8; 8];
            let mut read = 0u32;
            // SAFETY: `buf` is valid for its length; the call is synchronous.
            let ok = unsafe {
                ReadFile(
                    handle,
                    buf.as_mut_ptr(),
                    buf.len() as u32,
                    &mut read,
                    null_mut(),
                )
            };
            if ok != 0 && read > 0 && matches!(buf[0], ACTIVATE | SHOW) {
                on_show(ShowRequest {
                    activate: buf[0] == ACTIVATE,
                });
            }
        } else {
            thread::sleep(BACKOFF);
        }
        // SAFETY: `pipe` is a live pipe instance with no I/O in flight.
        unsafe { DisconnectNamedPipe(handle) };
    }
}

fn create_pipe(key: &str) -> Result<OwnedHandle, InstanceError> {
    let descriptor: SecurityDescriptor = user_only_descriptor().map_err(|e| match e {
        crate::hook_pipe::HookPipeError::Os { call, code } => InstanceError { call, code },
        _ => InstanceError {
            call: "user_only_descriptor",
            code: 0,
        },
    })?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let name = wide(&pipe_name(key));
    // SAFETY: `name` is NUL-terminated and `attributes` points to a live
    // security descriptor for the duration of the call. FIRST_PIPE_INSTANCE
    // fails if anyone created the name before us.
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            0,
            64,
            0,
            &attributes,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(last_error("CreateNamedPipeW"));
    }
    // SAFETY: CreateNamedPipeW succeeded, so the handle is ours to close.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

/// Sends the show request to the running instance; whether it got there.
fn ask_to_show(key: &str) -> bool {
    const ERROR_FILE_NOT_FOUND: i32 = 2;
    const ERROR_PIPE_BUSY: i32 = 231;
    // With SECURITY_SQOS_PRESENT, which std adds: the server cannot
    // impersonate this process.
    const SECURITY_ANONYMOUS: u32 = 0;
    let path = format!(r"\\?\pipe\yien-instance-{key}");
    let deadline = Instant::now() + CONNECT_WAIT;
    let mut file = loop {
        match OpenOptions::new()
            .write(true)
            .security_qos_flags(SECURITY_ANONYMOUS)
            .open(&path)
        {
            Ok(file) => break file,
            Err(e)
                if matches!(
                    e.raw_os_error(),
                    Some(ERROR_FILE_NOT_FOUND | ERROR_PIPE_BUSY)
                ) && Instant::now() + BACKOFF < deadline =>
            {
                thread::sleep(BACKOFF);
            }
            Err(e) => {
                tracing::warn!("could not reach the running YIEN: {e}");
                return false;
            }
        }
    };
    let mut server = 0u32;
    // SAFETY: `file` is an open pipe client handle; `server` is a valid out pointer.
    let known = unsafe { GetNamedPipeServerProcessId(file.as_raw_handle(), &mut server) } != 0;
    // Only a start that may take the foreground can hand that right on.
    // SAFETY: plain call with a process id.
    let allowed = known && wants_foreground() && unsafe { AllowSetForegroundWindow(server) } != 0;
    let request = if allowed { ACTIVATE } else { SHOW };
    file.write_all(&[request]).is_ok()
}

/// Whether whoever started this process wanted its window in front: a
/// start that asked for a window shown without activation (or hidden, or
/// minimized) did not, and the running instance then only flashes.
fn wants_foreground() -> bool {
    // SAFETY: STARTUPINFOW is plain data; all zeroes is a valid value.
    let mut info: STARTUPINFOW = unsafe { std::mem::zeroed() };
    info.cb = size_of::<STARTUPINFOW>() as u32;
    // SAFETY: `info` is a valid out pointer of the size given in `cb`.
    unsafe { GetStartupInfoW(&mut info) };
    if info.dwFlags & STARTF_USESHOWWINDOW == 0 {
        return true;
    }
    !matches!(
        i32::from(info.wShowWindow),
        SW_HIDE
            | SW_MINIMIZE
            | SW_SHOWMINIMIZED
            | SW_SHOWMINNOACTIVE
            | SW_SHOWNA
            | SW_SHOWNOACTIVATE
    )
}

/// Brings the window forward for a show request: restored if minimized,
/// then in front when the request may take the foreground, or flashing on
/// the taskbar when it may not. Never synthesizes input to force it.
pub(super) fn show_window(hwnd: isize, request: ShowRequest) {
    let hwnd = hwnd as windows_sys::Win32::Foundation::HWND;
    // SAFETY: `hwnd` is this process's own window; these calls only read it
    // or post to it, and fail harmlessly if it is gone.
    unsafe {
        if IsIconic(hwnd) != 0 {
            let show = if request.activate {
                SW_RESTORE
            } else {
                SW_SHOWNOACTIVATE
            };
            ShowWindowAsync(hwnd, show);
        }
        if request.activate && SetForegroundWindow(hwnd) != 0 {
            return;
        }
        let info = FLASHWINFO {
            cbSize: size_of::<FLASHWINFO>() as u32,
            hwnd,
            dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG,
            uCount: 1,
            dwTimeout: 0,
        };
        FlashWindowEx(&info);
    }
}

fn wide(text: &str) -> Vec<u16> {
    OsStr::new(text).encode_wide().chain(Some(0)).collect()
}

fn last_error(call: &'static str) -> InstanceError {
    InstanceError {
        call,
        code: io::Error::last_os_error().raw_os_error().unwrap_or(0),
    }
}
