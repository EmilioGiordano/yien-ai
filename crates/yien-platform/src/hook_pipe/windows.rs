//! The Windows transport: an overlapped named pipe whose DACL admits only the
//! current user, refusing remote clients.

#![allow(unsafe_code)]

use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use yien_proto::hook_frame;

use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_PIPE_CONNECTED, HANDLE,
    INVALID_HANDLE_VALUE, LocalFree, WAIT_OBJECT_0, WAIT_TIMEOUT,
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
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeServerProcessId,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, INFINITE, OpenProcessToken, ResetEvent, SetEvent,
    WaitForMultipleObjects, WaitForSingleObject,
};

use super::{HookPipeError, MAX_CONNECTIONS, READ_TIMEOUT, Rejection, Shared};

/// Inbound buffer of each pipe instance; frames larger than this stream.
const IN_BUFFER: u32 = 64 * 1024;

/// Pause after a failed connect or instance creation, so a repeating failure
/// cannot spin the accept loop.
const BACKOFF_MS: u32 = 50;

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

/// Serves clients until `stop` is set. The server always owns at least one
/// instance of the name: the next listening instance is created before a
/// connected one is handed off, and an instance that cannot be handed off is
/// disconnected and reused. Otherwise, with zero instances, anyone could
/// create the name and receive the hooks.
fn accept_loop(
    first: OwnedHandle,
    name: &[u16],
    descriptor: &SecurityDescriptor,
    stop: &Arc<OwnedHandle>,
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
    let mut listening = first;
    loop {
        match wait_for_client(&listening, &connected, stop) {
            Connect::Stopped => break,
            Connect::Failed => {
                disconnect(&listening);
                if backoff(stop) {
                    break;
                }
            }
            Connect::Client => {
                workers.retain(|w| !w.is_finished());
                if workers.len() >= MAX_CONNECTIONS {
                    shared.reject(Rejection::Busy, Instant::now());
                    disconnect(&listening);
                    continue;
                }
                let next = match create_instance(name, descriptor, false) {
                    Ok(next) => next,
                    Err(e) => {
                        tracing::warn!(error = %e, "hook pipe instance not created");
                        shared.reject(Rejection::Busy, Instant::now());
                        disconnect(&listening);
                        if backoff(stop) {
                            break;
                        }
                        continue;
                    }
                };
                let client = std::mem::replace(&mut listening, next);
                let worker_shared = Arc::clone(shared);
                let worker_stop = Arc::clone(stop);
                match thread::Builder::new()
                    .name("yien-hook-conn".into())
                    .spawn(move || serve(&client, &worker_stop, &worker_shared))
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

/// Waits [`BACKOFF_MS`]; returns whether `stop` was set meanwhile.
fn backoff(stop: &OwnedHandle) -> bool {
    // SAFETY: `stop` is a live event handle.
    unsafe { WaitForSingleObject(stop.as_raw_handle(), BACKOFF_MS) == WAIT_OBJECT_0 }
}

/// Drops the client of `pipe`, leaving the instance ready for the next one.
fn disconnect(pipe: &OwnedHandle) {
    // SAFETY: `pipe` is a live pipe instance with no I/O in flight.
    unsafe { DisconnectNamedPipe(pipe.as_raw_handle()) };
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
        // The client connected, wrote and closed before this call; its bytes
        // may still be buffered, and a read reports EOF if not.
        ERROR_NO_DATA => return Connect::Client,
        ERROR_IO_PENDING => {}
        _ => return Connect::Failed,
    }
    match wait_io(handle, &ov, connected, stop, INFINITE) {
        Wait::Done(Ok(_)) => Connect::Client,
        Wait::Done(Err(_)) => Connect::Failed,
        Wait::Stopped | Wait::TimedOut => Connect::Stopped,
    }
}

fn serve(pipe: &OwnedHandle, stop: &OwnedHandle, shared: &Shared) {
    let Ok(event) = event() else {
        shared.reject(Rejection::Io(io::ErrorKind::Other), Instant::now());
        return;
    };
    let mut reader = PipeReader {
        pipe,
        event,
        stop,
        deadline: Instant::now() + READ_TIMEOUT,
    };
    shared.handle(&mut reader);
}

/// How an overlapped operation ended.
enum Wait {
    Done(io::Result<u32>),
    TimedOut,
    Stopped,
}

/// Waits for the operation on `ov` (whose event is `done`), for `stop` or for
/// `ms`. Unless it returns [`Wait::Done`], the operation has been cancelled,
/// and in every case the OS no longer references `ov` or its buffer.
fn wait_io(
    handle: HANDLE,
    ov: &OVERLAPPED,
    done: &OwnedHandle,
    stop: &OwnedHandle,
    ms: u32,
) -> Wait {
    let handles: [HANDLE; 2] = [done.as_raw_handle(), stop.as_raw_handle()];
    // SAFETY: both handles are live events.
    let woke = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, ms) };
    if woke == WAIT_OBJECT_0 {
        let mut n = 0u32;
        // SAFETY: the operation on `ov` has completed; this only reads its result.
        return Wait::Done(
            if unsafe { GetOverlappedResult(handle, ov, &mut n, 0) } != 0 {
                Ok(n)
            } else {
                Err(io::Error::last_os_error())
            },
        );
    }
    cancel_and_wait(handle, ov);
    if woke == WAIT_TIMEOUT {
        Wait::TimedOut
    } else {
        // Stop requested, or the wait itself failed.
        Wait::Stopped
    }
}

/// Reads from a connected overlapped pipe, giving up at `deadline` or when
/// the server stops.
struct PipeReader<'a> {
    pipe: &'a OwnedHandle,
    event: OwnedHandle,
    stop: &'a OwnedHandle,
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
        // read: `wait_io` below returns only once the OS is done with both.
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
        match wait_io(handle, &ov, &self.event, self.stop, ms) {
            Wait::Done(Ok(n)) => Ok(n as usize),
            Wait::Done(Err(e)) if e.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) => Ok(0),
            Wait::Done(Err(e)) => Err(e),
            Wait::TimedOut => Err(io::ErrorKind::TimedOut.into()),
            Wait::Stopped => Err(io::ErrorKind::ConnectionAborted.into()),
        }
    }
}

/// Opens the client end of hook pipe `name`, retrying while every instance is
/// busy until `deadline`. Only a name of the exact shape `hook_pipe_name`
/// makes is opened, through `\\?\` so Win32 does not normalize it, and only
/// when the server is the process the name says.
pub(super) fn connect(name: &str, deadline: Instant) -> Option<File> {
    const ERROR_PIPE_BUSY: i32 = 231;
    // With SECURITY_SQOS_PRESENT, which std adds: the server cannot
    // impersonate this process.
    const SECURITY_ANONYMOUS: u32 = 0;
    const BUSY_RETRY: Duration = Duration::from_millis(5);

    let pid = hook_frame::hook_pipe_pid(name)?;
    let path = format!(r"\\?\{}", name.strip_prefix(r"\\.\")?);
    let file = loop {
        match OpenOptions::new()
            .write(true)
            .security_qos_flags(SECURITY_ANONYMOUS)
            .open(&path)
        {
            Ok(file) => break file,
            Err(e)
                if e.raw_os_error() == Some(ERROR_PIPE_BUSY)
                    && Instant::now() + BUSY_RETRY < deadline =>
            {
                thread::sleep(BUSY_RETRY);
            }
            Err(_) => return None,
        }
    };
    let mut server = 0u32;
    // SAFETY: `file` is an open pipe client handle; `server` is a valid out pointer.
    let ok = unsafe { GetNamedPipeServerProcessId(file.as_raw_handle(), &mut server) };
    (ok != 0 && server == pid).then_some(file)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rnf_11_a_taken_pipe_name_cannot_be_created_again() {
        let descriptor = user_only_descriptor().unwrap();
        let name = wide(&format!(
            "{}test-{}-{}",
            super::super::HOOK_PIPE_PREFIX,
            std::process::id(),
            super::super::random_hex::<8>().unwrap()
        ));
        let _first = create_instance(&name, &descriptor, true).unwrap();
        assert!(create_instance(&name, &descriptor, true).is_err());
    }

    fn listening_pipe(pid: u32) -> (String, OwnedHandle) {
        let descriptor = user_only_descriptor().unwrap();
        let nonce = getrandom::u64().unwrap();
        let name = hook_frame::hook_pipe_name(pid, nonce);
        let pipe = create_instance(&wide(&name), &descriptor, true).unwrap();
        (name, pipe)
    }

    #[test]
    fn client_connects_when_the_server_is_the_named_process() {
        let (name, _pipe) = listening_pipe(std::process::id());
        let deadline = Instant::now() + Duration::from_secs(1);
        assert!(connect(&name, deadline).is_some());
    }

    #[test]
    fn rnf_11_client_refuses_a_server_that_is_not_the_named_process() {
        let (name, _pipe) = listening_pipe(std::process::id().wrapping_add(1));
        let deadline = Instant::now() + Duration::from_secs(1);
        assert!(connect(&name, deadline).is_none());
    }
}
