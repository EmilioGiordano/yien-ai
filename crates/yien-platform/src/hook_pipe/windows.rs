//! The Windows transport: an overlapped named pipe whose DACL admits only the
//! current user, refusing remote clients.

#![allow(unsafe_code)]

use std::ffi::OsStr;
use std::io::{self, Read};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Instant;

use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE,
    LocalFree, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_INBOUND, ReadFile,
};
use windows_sys::Win32::System::IO::{
    CancelIoEx, GetOverlappedResult, GetOverlappedResultEx, OVERLAPPED,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, INFINITE, OpenProcessToken, ResetEvent, SetEvent,
    WaitForMultipleObjects, WaitForSingleObject,
};

use super::{HookPipeError, MAX_CONNECTIONS, READ_TIMEOUT, Rejection, Shared};

/// Inbound buffer of each pipe instance; frames larger than this stream.
const IN_BUFFER: u32 = 64 * 1024;

/// Pause before retrying after an instance could not be created.
const RETRY_MS: u32 = 200;

pub(super) struct Server {
    stop: Arc<OwnedHandle>,
    accept: Option<JoinHandle<()>>,
}

impl Server {
    pub(super) fn start(name: &str, shared: Arc<Shared>) -> Result<Self, HookPipeError> {
        let descriptor = user_only_descriptor()?;
        let name = wide(name);
        // The first instance is created here, so the name exists when start
        // returns and nobody else can have created it first.
        let first = create_instance(&name, &descriptor, true)?;
        let stop = Arc::new(event().map_err(|e| os_error("CreateEventW", &e))?);
        let thread_stop = Arc::clone(&stop);
        let accept = thread::Builder::new()
            .name("yien-hook-pipe".into())
            .spawn(move || accept_loop(first, &name, &descriptor, &thread_stop, &shared))
            .map_err(|e| os_error("spawn", &e))?;
        Ok(Self {
            stop,
            accept: Some(accept),
        })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // SAFETY: `stop` is a live event handle owned by this server.
        unsafe { SetEvent(self.stop.as_raw_handle()) };
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

fn accept_loop(
    first: OwnedHandle,
    name: &[u16],
    descriptor: &SecurityDescriptor,
    stop: &OwnedHandle,
    shared: &Arc<Shared>,
) {
    let connected = match event() {
        Ok(event) => event,
        Err(e) => {
            tracing::warn!(error = %e, "hook pipe stopped: no event for connects");
            return;
        }
    };
    let mut workers: Vec<JoinHandle<()>> = Vec::new();
    let mut next = Some(first);
    loop {
        let pipe = match next.take() {
            Some(pipe) => pipe,
            None => match create_instance(name, descriptor, false) {
                Ok(pipe) => pipe,
                Err(e) => {
                    tracing::warn!(error = %e, "hook pipe instance not created; retrying");
                    // SAFETY: `stop` is a live event handle.
                    if unsafe { WaitForSingleObject(stop.as_raw_handle(), RETRY_MS) }
                        == WAIT_OBJECT_0
                    {
                        break;
                    }
                    continue;
                }
            },
        };
        match wait_for_client(&pipe, &connected, stop) {
            Connect::Stopped => break,
            Connect::Failed => {}
            Connect::Client => {
                workers.retain(|w| !w.is_finished());
                if workers.len() >= MAX_CONNECTIONS {
                    shared.reject(Rejection::Busy, Instant::now());
                    continue;
                }
                let worker_shared = Arc::clone(shared);
                match thread::Builder::new()
                    .name("yien-hook-conn".into())
                    .spawn(move || serve(&pipe, &worker_shared))
                {
                    Ok(worker) => workers.push(worker),
                    Err(e) => {
                        tracing::warn!(error = %e, "hook connection thread not started");
                        shared.reject(Rejection::Busy, Instant::now());
                    }
                }
            }
        }
    }
    for worker in workers {
        let _ = worker.join();
    }
}

enum Connect {
    Client,
    Failed,
    Stopped,
}

fn wait_for_client(pipe: &OwnedHandle, connected: &OwnedHandle, stop: &OwnedHandle) -> Connect {
    let handle = pipe.as_raw_handle();
    // SAFETY: OVERLAPPED is plain data; all zeroes is its documented initial state.
    let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
    ov.hEvent = connected.as_raw_handle();
    // SAFETY: `connected` is a live manual-reset event.
    unsafe { ResetEvent(ov.hEvent) };
    // SAFETY: `pipe` was created with FILE_FLAG_OVERLAPPED; `ov` stays on this
    // frame and every path below lets the operation finish before returning.
    if unsafe { ConnectNamedPipe(handle, &mut ov) } != 0 {
        return Connect::Client;
    }
    match last_error() {
        ERROR_PIPE_CONNECTED => return Connect::Client,
        ERROR_IO_PENDING => {}
        _ => return Connect::Failed,
    }
    let handles: [HANDLE; 2] = [connected.as_raw_handle(), stop.as_raw_handle()];
    // SAFETY: both handles are live events.
    let woke = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) };
    if woke == WAIT_OBJECT_0 {
        let mut n = 0u32;
        // SAFETY: the connect on `ov` has completed; this only reads its result.
        if unsafe { GetOverlappedResult(handle, &ov, &mut n, 0) } != 0 {
            return Connect::Client;
        }
        return Connect::Failed;
    }
    // Stop requested, or the wait failed: never leave `ov` referenced.
    cancel_and_wait(handle, &ov);
    Connect::Stopped
}

fn serve(pipe: &OwnedHandle, shared: &Shared) {
    let Ok(event) = event() else {
        shared.reject(Rejection::Io(io::ErrorKind::Other), Instant::now());
        return;
    };
    let mut reader = PipeReader {
        pipe,
        event,
        deadline: Instant::now() + READ_TIMEOUT,
    };
    shared.handle(&mut reader);
}

/// Reads from a connected overlapped pipe, giving up at `deadline`.
struct PipeReader<'a> {
    pipe: &'a OwnedHandle,
    event: OwnedHandle,
    deadline: Instant,
}

impl Read for PipeReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let handle = self.pipe.as_raw_handle();
        let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        // SAFETY: OVERLAPPED is plain data; all zeroes is its documented initial state.
        let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        ov.hEvent = self.event.as_raw_handle();
        // SAFETY: `buf` is valid for `len` bytes. `buf` and `ov` outlive the
        // read: every path below waits for it to complete, or cancels it and
        // waits for the cancellation, before returning.
        if unsafe { ReadFile(handle, buf.as_mut_ptr(), len, null_mut(), &mut ov) } == 0 {
            match last_error() {
                ERROR_IO_PENDING => {}
                ERROR_BROKEN_PIPE => return Ok(0),
                code => return Err(io::Error::from_raw_os_error(code as i32)),
            }
        }
        let ms = u32::try_from(remaining.as_millis())
            .unwrap_or(INFINITE - 1)
            .clamp(1, INFINITE - 1);
        let mut n = 0u32;
        // SAFETY: `ov` belongs to the read started above.
        if unsafe { GetOverlappedResultEx(handle, &ov, &mut n, ms, 0) } != 0 {
            return Ok(n as usize);
        }
        match last_error() {
            WAIT_TIMEOUT => {
                cancel_and_wait(handle, &ov);
                Err(io::ErrorKind::TimedOut.into())
            }
            ERROR_BROKEN_PIPE => Ok(0),
            code => {
                cancel_and_wait(handle, &ov);
                Err(io::Error::from_raw_os_error(code as i32))
            }
        }
    }
}

/// Cancels the operation on `ov`, if still pending, and waits until the OS no
/// longer references `ov` or its buffer.
fn cancel_and_wait(handle: HANDLE, ov: &OVERLAPPED) {
    let mut n = 0u32;
    // SAFETY: `handle` is live and `ov` is the operation's own OVERLAPPED.
    // Cancelling a finished operation fails harmlessly, and the blocking
    // GetOverlappedResult returns once the operation is over either way.
    unsafe {
        CancelIoEx(handle, ov);
        GetOverlappedResult(handle, ov, &mut n, 1);
    }
}

fn create_instance(
    name: &[u16],
    descriptor: &SecurityDescriptor,
    first: bool,
) -> Result<OwnedHandle, HookPipeError> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let mut open_mode = PIPE_ACCESS_INBOUND | FILE_FLAG_OVERLAPPED;
    if first {
        open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    // SAFETY: `name` is NUL-terminated and `attributes` points to a live
    // security descriptor for the duration of the call.
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            open_mode,
            PIPE_TYPE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            0,
            IN_BUFFER,
            0,
            &attributes,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(last_os_error("CreateNamedPipeW"));
    }
    // SAFETY: CreateNamedPipeW succeeded, so the handle is ours to close.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

/// A manual-reset event, initially unset.
fn event() -> io::Result<OwnedHandle> {
    // SAFETY: null attributes and name are allowed.
    let handle = unsafe { CreateEventW(null(), 1, 0, null()) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateEventW succeeded, so the handle is ours to close.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

/// A self-relative security descriptor allocated by the OS.
struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

// SAFETY: the descriptor is immutable after creation and freed only on drop.
unsafe impl Send for SecurityDescriptor {}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: allocated with LocalAlloc by
        // ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(self.0) };
    }
}

/// A DACL with one entry: full access for the current user (RNF-11). `P`
/// blocks inherited entries.
fn user_only_descriptor() -> Result<SecurityDescriptor, HookPipeError> {
    let sid = current_user_sid()?;
    if !sid
        .bytes()
        .all(|b| b.is_ascii_digit() || b == b'-' || b == b'S')
    {
        return Err(HookPipeError::Os {
            call: "ConvertSidToStringSidW",
            code: 0,
        });
    }
    let sddl = wide(&format!("D:P(A;;GA;;;{sid})"));
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: `sddl` is NUL-terminated; `descriptor` is a valid out pointer;
    // the size out pointer may be null.
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        )
    };
    if ok == 0 {
        return Err(last_os_error(
            "ConvertStringSecurityDescriptorToSecurityDescriptorW",
        ));
    }
    Ok(SecurityDescriptor(descriptor))
}

/// The current process user's SID, as `S-1-5-21-...`.
fn current_user_sid() -> Result<String, HookPipeError> {
    let mut token: HANDLE = null_mut();
    // SAFETY: GetCurrentProcess returns a pseudo handle that needs no closing;
    // `token` is a valid out pointer.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(last_os_error("OpenProcessToken"));
    }
    // SAFETY: OpenProcessToken succeeded, so the handle is ours to close.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };

    let mut len = 0u32;
    // SAFETY: a null buffer of length 0 only asks for the size needed.
    unsafe { GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut len) };
    if len == 0 {
        return Err(last_os_error("GetTokenInformation"));
    }
    // u64 cells keep the TOKEN_USER at the start suitably aligned.
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: `buf` holds at least `len` bytes.
    let ok = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buf.as_mut_ptr().cast(),
            len,
            &mut len,
        )
    };
    if ok == 0 {
        return Err(last_os_error("GetTokenInformation"));
    }
    // SAFETY: on success the buffer starts with a TOKEN_USER whose SID points
    // inside `buf`, which outlives every use below.
    let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };

    let mut text: *mut u16 = null_mut();
    // SAFETY: `sid` is valid; `text` is an out pointer freed below.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(last_os_error("ConvertSidToStringSidW"));
    }
    // SAFETY: on success `text` is a NUL-terminated UTF-16 string.
    let sid_text = unsafe {
        let mut n = 0;
        while *text.add(n) != 0 {
            n += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(text, n))
    };
    // SAFETY: allocated with LocalAlloc by ConvertSidToStringSidW.
    unsafe { LocalFree(text.cast()) };
    Ok(sid_text)
}

fn wide(text: &str) -> Vec<u16> {
    OsStr::new(text).encode_wide().chain(Some(0)).collect()
}

fn last_error() -> u32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32
}

fn last_os_error(call: &'static str) -> HookPipeError {
    os_error(call, &io::Error::last_os_error())
}

fn os_error(call: &'static str, error: &io::Error) -> HookPipeError {
    HookPipeError::Os {
        call,
        code: error.raw_os_error().unwrap_or(0),
    }
}
