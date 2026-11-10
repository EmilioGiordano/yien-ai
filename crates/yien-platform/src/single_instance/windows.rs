//! The Windows claim: a named mutex in the session's namespace, and a named
//! pipe for this session, admitting only the current user, that carries the
//! show request.

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
    ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_IO_PENDING, ERROR_NO_DATA,
    ERROR_PIPE_CONNECTED, ERROR_SUCCESS, GetLastError, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
    WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, GetSecurityInfo, SE_KERNEL_OBJECT,
};
use windows_sys::Win32::Security::{
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_INBOUND, ReadFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeServerProcessId,
    GetNamedPipeServerSessionId, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateMutexW, GetCurrentProcessId, GetStartupInfoW, INFINITE, ResetEvent,
    STARTF_USESHOWWINDOW, STARTUPINFOW, WaitForSingleObject,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, FLASHW_TIMERNOFG, FLASHW_TRAY, FLASHWINFO, FlashWindowEx, IsIconic,
    SW_HIDE, SW_MINIMIZE, SW_RESTORE, SW_SHOWMINIMIZED, SW_SHOWMINNOACTIVE, SW_SHOWNA,
    SW_SHOWNOACTIVATE, SetForegroundWindow, ShowWindowAsync,
};

use super::{Claim, Instance, InstanceError, ShowRequest};
use crate::hook_pipe::HookPipeError;
use crate::hook_pipe::windows::{
    SecurityDescriptor, current_default_owner_sid, current_user_sid, user_only_descriptor,
};

/// The request bytes: show and take the foreground, or only show.
const ACTIVATE: u8 = b'A';
const SHOW: u8 = b'S';

/// How long a later start waits for the running one's pipe: it may hold the
/// mutex and not have created the pipe yet.
const CONNECT_WAIT: Duration = Duration::from_secs(2);

/// How long the running instance waits for a connected client's byte; a
/// client that never writes is dropped after it.
/// Shorter in tests, so a silent client is dropped well inside the 2 s a
/// later start waits for the pipe.
const READ_WAIT_MS: u32 = if cfg!(test) { 250 } else { 1000 };

/// Pause after a failed connect, so a repeating failure cannot spin.
const BACKOFF: Duration = Duration::from_millis(50);

pub(super) struct Guard {
    _mutex: OwnedHandle,
    _installer_mutex: OwnedHandle,
    /// The pipe, until [`Guard::serve`] hands it to its thread.
    pipe: Option<OwnedHandle>,
}

/// What `CreateMutexW` said about the claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MutexOutcome {
    First,
    Running,
    Failed,
}

/// Reads `CreateMutexW`'s result: whether it gave a handle, and the last
/// error. A mutex this process may not open is another YIEN's, for example
/// an elevated one, so it counts as running rather than as a failure.
fn mutex_outcome(handle: bool, error: u32) -> MutexOutcome {
    match (handle, error) {
        (true, ERROR_ALREADY_EXISTS) => MutexOutcome::Running,
        (true, _) => MutexOutcome::First,
        (false, ERROR_ACCESS_DENIED) => MutexOutcome::Running,
        (false, _) => MutexOutcome::Failed,
    }
}

fn mutex_name(key: &str) -> String {
    format!(r"Local\yien-instance-{key}")
}

/// The pipe name is machine-wide, the mutex is not: the session in the name
/// keeps another session's YIEN on the same data dir apart.
fn pipe_name(key: &str, session: u32) -> String {
    format!(r"\\.\pipe\yien-instance-{key}-{session}")
}

fn own_session() -> u32 {
    let mut session = 0u32;
    // SAFETY: plain call with this process's id and a valid out pointer.
    unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) };
    session
}

fn descriptor() -> Result<SecurityDescriptor, InstanceError> {
    user_only_descriptor().map_err(|e| match e {
        HookPipeError::Os { call, code } => InstanceError { call, code },
        _ => InstanceError {
            call: "user_only_descriptor",
            code: 0,
        },
    })
}

pub(super) fn claim(_dir: &std::path::Path, key: &str) -> Result<Claim, InstanceError> {
    let descriptor = descriptor()?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let installer_name = wide(r"Local\YIENRunning");
    // SAFETY: `installer_name` is NUL-terminated and remains alive for the call.
    let installer_handle = unsafe { CreateMutexW(std::ptr::null(), 0, installer_name.as_ptr()) };
    if installer_handle.is_null() {
        return Err(last_error("CreateMutexW(installer guard)"));
    }
    // Keeping a handle open makes this stable, per-user-session guard visible
    // to the installer even when YIEN uses a redirected data directory.
    let installer_mutex = unsafe { OwnedHandle::from_raw_handle(installer_handle) };
    let name = wide(&mutex_name(key));
    // SAFETY: `name` is NUL-terminated and `attributes` points to a live
    // security descriptor for the duration of the call.
    let handle = unsafe { CreateMutexW(&attributes, 0, name.as_ptr()) };
    // Read before any other call can overwrite it.
    // SAFETY: plain read of this thread's last error.
    let error = unsafe { GetLastError() };
    let session = own_session();
    match mutex_outcome(!handle.is_null(), error) {
        MutexOutcome::Failed => return Err(last_error("CreateMutexW")),
        MutexOutcome::Running => {
            if !handle.is_null() {
                // SAFETY: CreateMutexW gave us this handle; nothing else owns it.
                drop(unsafe { OwnedHandle::from_raw_handle(handle) });
            }
            return Ok(Claim::Running {
                delivered: ask_to_show(key, session),
            });
        }
        MutexOutcome::First => {}
    }
    // SAFETY: CreateMutexW succeeded, so the handle is ours to close.
    let mutex = unsafe { OwnedHandle::from_raw_handle(handle) };
    // The pipe exists from here on, so a later start can connect and write
    // before `serve` runs; the bytes wait in the pipe.
    let pipe = match create_pipe(key, session, &descriptor) {
        Ok(pipe) => Some(pipe),
        Err(error) => {
            tracing::warn!("a later start cannot ask for the window: {error}");
            None
        }
    };
    Ok(Claim::First(Instance {
        guard: Guard {
            _mutex: mutex,
            _installer_mutex: installer_mutex,
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
/// one instance is reused, so the name is never free for someone else; a
/// client that connects and stays silent is dropped after [`READ_WAIT_MS`].
fn serve(pipe: &OwnedHandle, on_show: &dyn Fn(ShowRequest)) {
    let Ok(event) = event() else {
        tracing::warn!("a later start cannot ask for the window: no event");
        return;
    };
    let handle = pipe.as_raw_handle();
    loop {
        if connect(handle, &event) {
            let mut buf = [0u8; 8];
            if let Some(read) = read(handle, &event, &mut buf)
                && read > 0
                && matches!(buf[0], ACTIVATE | SHOW)
            {
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

/// Waits for a client on the overlapped pipe; whether one connected.
fn connect(handle: HANDLE, event: &OwnedHandle) -> bool {
    // SAFETY: OVERLAPPED is plain data; all zeroes is its documented initial state.
    let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
    ov.hEvent = event.as_raw_handle();
    // SAFETY: `event` is a live manual-reset event; `ov` outlives the
    // operation, which ends before this returns.
    unsafe {
        ResetEvent(ov.hEvent);
        if ConnectNamedPipe(handle, &mut ov) != 0 {
            return true;
        }
    }
    match io::Error::last_os_error().raw_os_error().map(|c| c as u32) {
        Some(ERROR_PIPE_CONNECTED | ERROR_NO_DATA) => true,
        Some(ERROR_IO_PENDING) => finish(handle, &ov, INFINITE).is_some(),
        _ => false,
    }
}

/// Reads what the client sent, giving up after [`READ_WAIT_MS`].
fn read(handle: HANDLE, event: &OwnedHandle, buf: &mut [u8]) -> Option<u32> {
    // SAFETY: OVERLAPPED is plain data; all zeroes is its documented initial state.
    let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
    ov.hEvent = event.as_raw_handle();
    // SAFETY: `buf` and `ov` outlive the read: `finish` returns only once
    // the OS is done with both.
    unsafe {
        ResetEvent(ov.hEvent);
        let mut n = 0u32;
        if ReadFile(handle, buf.as_mut_ptr(), buf.len() as u32, &mut n, &mut ov) != 0 {
            return Some(n);
        }
    }
    match io::Error::last_os_error().raw_os_error().map(|c| c as u32) {
        Some(ERROR_IO_PENDING) => finish(handle, &ov, READ_WAIT_MS),
        _ => None,
    }
}

/// Waits `ms` for the operation on `ov`, cancelling it on timeout. In every
/// case the OS no longer uses `ov` or its buffer when this returns.
fn finish(handle: HANDLE, ov: &OVERLAPPED, ms: u32) -> Option<u32> {
    let mut n = 0u32;
    // SAFETY: `ov.hEvent` is a live event; the blocking GetOverlappedResult
    // after CancelIoEx returns once the operation is over.
    unsafe {
        if WaitForSingleObject(ov.hEvent, ms) == WAIT_OBJECT_0 {
            return (GetOverlappedResult(handle, ov, &mut n, 0) != 0).then_some(n);
        }
        CancelIoEx(handle, ov);
        GetOverlappedResult(handle, ov, &mut n, 1);
    }
    None
}

fn event() -> io::Result<OwnedHandle> {
    // SAFETY: null attributes and name are allowed.
    let handle = unsafe { CreateEventW(null(), 1, 0, null()) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateEventW succeeded, so the handle is ours to close.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

fn create_pipe(
    key: &str,
    session: u32,
    descriptor: &SecurityDescriptor,
) -> Result<OwnedHandle, InstanceError> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let name = wide(&pipe_name(key, session));
    // SAFETY: `name` is NUL-terminated and `attributes` points to a live
    // security descriptor for the duration of the call. FIRST_PIPE_INSTANCE
    // fails if anyone created the name before us.
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
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
/// A pipe served from another session or owned by another user is not
/// ours: nothing is sent to it, and no foreground right is handed on.
fn ask_to_show(key: &str, session: u32) -> bool {
    const ERROR_FILE_NOT_FOUND: i32 = 2;
    const ERROR_PIPE_BUSY: i32 = 231;
    // With SECURITY_SQOS_PRESENT, which std adds: the server cannot
    // impersonate this process.
    const SECURITY_ANONYMOUS: u32 = 0;
    let path = format!(r"\\?\pipe\yien-instance-{key}-{session}");
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
    let pipe = file.as_raw_handle();
    let mut server_session = u32::MAX;
    // SAFETY: `pipe` is an open pipe client handle; the out pointer is valid.
    let same_session = unsafe { GetNamedPipeServerSessionId(pipe, &mut server_session) } != 0
        && server_session == session;
    if !same_session || !owned_by_us(pipe) {
        tracing::warn!("the instance pipe is not this user's own; nothing sent");
        return false;
    }
    let mut server = 0u32;
    // SAFETY: `pipe` is an open pipe client handle; `server` is a valid out pointer.
    let known = unsafe { GetNamedPipeServerProcessId(pipe, &mut server) } != 0;
    // Only a start that may take the foreground can hand that right on.
    // SAFETY: plain call with a process id.
    let allowed = known && wants_foreground() && unsafe { AllowSetForegroundWindow(server) } != 0;
    let request = if allowed { ACTIVATE } else { SHOW };
    file.write_all(&[request]).is_ok()
}

/// Whether the pipe's owner is the user this process runs as.
fn owned_by_us(pipe: HANDLE) -> bool {
    let Ok(user) = current_user_sid() else {
        return false;
    };
    let default_owner = current_default_owner_sid().ok();
    pipe_owner(pipe).is_some_and(|owner| owner_is_ours(&owner, &user, default_owner.as_deref()))
}

/// Whether a pipe owned by `owner` is this user's own. YIEN names the user
/// as owner; a pipe made without naming one gets the token's default owner,
/// which for an elevated token is BUILTIN\Administrators rather than the
/// user, and is accepted too.
fn owner_is_ours(owner: &str, user: &str, default_owner: Option<&str>) -> bool {
    owner == user || default_owner == Some(owner)
}

/// The SID that owns `pipe`, as text.
fn pipe_owner(pipe: HANDLE) -> Option<String> {
    let mut owner: PSID = null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: `pipe` is a live handle; the out pointers are valid; the
    // descriptor the owner points into is freed below, after its last use.
    let status = unsafe {
        GetSecurityInfo(
            pipe,
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let mut text: *mut u16 = null_mut();
    // SAFETY: `owner` points into `descriptor`, still alive; `text` is an
    // out pointer freed below.
    let owner = if unsafe { ConvertSidToStringSidW(owner, &mut text) } != 0 {
        // SAFETY: on success `text` is a NUL-terminated UTF-16 string.
        let owner = unsafe {
            let mut n = 0;
            while *text.add(n) != 0 {
                n += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(text, n))
        };
        // SAFETY: allocated with LocalAlloc by ConvertSidToStringSidW.
        unsafe { LocalFree(text.cast()) };
        Some(owner)
    } else {
        None
    };
    // SAFETY: allocated with LocalAlloc by GetSecurityInfo.
    unsafe { LocalFree(descriptor) };
    owner
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rf_ses_14_a_mutex_this_process_may_not_open_is_another_yien() {
        assert_eq!(
            mutex_outcome(false, ERROR_ACCESS_DENIED),
            MutexOutcome::Running
        );
        assert_eq!(
            mutex_outcome(true, ERROR_ALREADY_EXISTS),
            MutexOutcome::Running
        );
        assert_eq!(mutex_outcome(true, 0), MutexOutcome::First);
        assert_eq!(mutex_outcome(false, 87), MutexOutcome::Failed);
    }

    #[test]
    fn rf_ses_14_the_pipe_name_carries_the_session() {
        assert_ne!(pipe_name("ab", 1), pipe_name("ab", 2));
        assert!(pipe_name("ab", 1).ends_with("yien-instance-ab-1"));
    }

    #[test]
    fn rnf_21_a_running_instance_holds_the_installer_mutex() {
        let key = format!("installer-test-{}", std::process::id());
        let instance = match claim(&std::env::temp_dir(), &key).unwrap() {
            Claim::First(instance) => instance,
            Claim::Running { .. } => panic!("test instance key unexpectedly exists"),
        };
        let name = wide(r"Local\YIENRunning");
        // SAFETY: `name` is NUL-terminated and valid for this call.
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        assert!(!handle.is_null());
        // SAFETY: read immediately after CreateMutexW, before another OS call.
        assert_eq!(unsafe { GetLastError() }, ERROR_ALREADY_EXISTS);
        // SAFETY: `handle` is a successful CreateMutexW result owned here.
        drop(unsafe { OwnedHandle::from_raw_handle(handle) });
        drop(instance);
    }

    #[test]
    fn a_pipe_this_process_created_is_owned_by_its_user() {
        let key = format!("test-owner-{}", std::process::id());
        let pipe = create_pipe(&key, own_session() ^ 0x5a5a, &descriptor().unwrap()).unwrap();
        // Named in the descriptor, so the same elevated or not.
        assert_eq!(
            pipe_owner(pipe.as_raw_handle()),
            Some(current_user_sid().unwrap())
        );
        assert!(owned_by_us(pipe.as_raw_handle()));
    }

    #[test]
    fn rf_ses_14_an_elevated_start_knows_its_own_pipe() {
        const USER: &str = "S-1-5-21-1-2-3-1001";
        const ADMINS: &str = "S-1-5-32-544";
        // The owner YIEN names, elevated (default owner Administrators) or not.
        assert!(owner_is_ours(USER, USER, Some(ADMINS)));
        assert!(owner_is_ours(USER, USER, Some(USER)));
        // A pipe owned by Administrators (`O:BA`), seen from an elevated
        // start: its own token would have made it so.
        assert!(owner_is_ours(ADMINS, USER, Some(ADMINS)));
        // The same pipe seen from a start that is not elevated, or another
        // user's pipe: not ours.
        assert!(!owner_is_ours(ADMINS, USER, Some(USER)));
        assert!(!owner_is_ours("S-1-5-21-1-2-3-1002", USER, Some(USER)));
        assert!(!owner_is_ours(ADMINS, USER, None));
    }

    #[test]
    fn a_silent_client_does_not_hold_the_server() {
        let key = format!("silent-{}", std::process::id());
        let session = own_session();
        let pipe = create_pipe(&key, session, &descriptor().unwrap()).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            serve(&pipe, &move |r| {
                let _ = tx.send(r);
            })
        });
        let path = format!(r"\\?\pipe\yien-instance-{key}-{session}");
        let silent = OpenOptions::new().write(true).open(&path).unwrap();
        assert!(ask_to_show(&key, session));
        let request = rx.recv_timeout(Duration::from_secs(5));
        assert!(request.is_ok(), "the silent client blocked the server");
        drop(silent);
    }
}
