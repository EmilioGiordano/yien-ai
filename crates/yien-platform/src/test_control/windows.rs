//! The Windows transport: one overlapped duplex pipe instance whose DACL
//! admits only the current user, refusing remote clients, serving one client
//! at a time.

#![allow(unsafe_code)]

use std::io::{self, Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::null_mut;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows_sys::Win32::System::IO::OVERLAPPED;
use windows_sys::Win32::System::Pipes::{
    CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{INFINITE, SetEvent};

use super::{Handler, IDLE_TIMEOUT, TestControlError, WRITE_TIMEOUT, serve_connection};
use crate::hook_pipe::HookPipeError;
use crate::hook_pipe::windows::{
    Connect, SecurityDescriptor, Wait, backoff, disconnect, event, last_error,
    user_only_descriptor, wait_for_client, wait_io, wide,
};

/// Each direction's buffer; a larger reply streams.
const BUFFER: u32 = 64 * 1024;

pub(super) struct Server {
    stop: Arc<OwnedHandle>,
    accept: Option<JoinHandle<()>>,
}

impl Server {
    pub(super) fn start(path: &str, handler: Handler) -> Result<Self, TestControlError> {
        let descriptor = user_only_descriptor().map_err(from_hook)?;
        // Created here with FIRST_PIPE_INSTANCE and a limit of one instance:
        // when start returns, the name is ours and nobody can add an instance.
        let pipe = create_instance(&wide(path), &descriptor)?;
        let stop = Arc::new(event().map_err(|e| os_error("CreateEventW", &e))?);
        let thread_stop = Arc::clone(&stop);
        let accept = thread::Builder::new()
            .name("yien-test-control".into())
            .spawn(move || serve_clients(&pipe, &thread_stop, &handler))
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

/// Serves one client after another on the single instance until `stop` is
/// set. The instance is disconnected, never closed, between clients, so the
/// name stays this process's.
fn serve_clients(pipe: &OwnedHandle, stop: &OwnedHandle, handler: &Handler) {
    let connected = match event() {
        Ok(event) => event,
        Err(e) => {
            tracing::warn!(error = %e, "test control stopped: no event for connects");
            return;
        }
    };
    let io_event = match event() {
        Ok(event) => event,
        Err(e) => {
            tracing::warn!(error = %e, "test control stopped: no event for reads");
            return;
        }
    };
    loop {
        match wait_for_client(pipe, &connected, stop) {
            Connect::Stopped => break,
            Connect::Failed => {
                disconnect(pipe);
                if backoff(stop) {
                    break;
                }
            }
            Connect::Client => {
                let mut io = PipeIo {
                    pipe,
                    event: &io_event,
                    stop,
                };
                let ended = serve_connection(&mut io, handler.as_ref());
                tracing::debug!(?ended, "test control client gone");
                disconnect(pipe);
            }
        }
    }
}

/// Reads and writes a connected overlapped pipe, giving up after a timeout
/// or when the server stops.
struct PipeIo<'a> {
    pipe: &'a OwnedHandle,
    event: &'a OwnedHandle,
    stop: &'a OwnedHandle,
}

impl PipeIo<'_> {
    fn finish(&self, ov: &OVERLAPPED, timeout: Duration) -> io::Result<usize> {
        let ms = u32::try_from(timeout.as_millis())
            .unwrap_or(INFINITE - 1)
            .clamp(1, INFINITE - 1);
        match wait_io(self.pipe.as_raw_handle(), ov, self.event, self.stop, ms) {
            Wait::Done(Ok(n)) => Ok(n as usize),
            Wait::Done(Err(e)) if e.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) => Ok(0),
            Wait::Done(Err(e)) => Err(e),
            Wait::TimedOut => Err(io::ErrorKind::TimedOut.into()),
            Wait::Stopped => Err(io::ErrorKind::ConnectionAborted.into()),
        }
    }
}

impl Read for PipeIo<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        // SAFETY: OVERLAPPED is plain data; all zeroes is its documented initial state.
        let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        ov.hEvent = self.event.as_raw_handle();
        // SAFETY: `buf` is valid for `len` bytes. `buf` and `ov` outlive the
        // read: `finish` returns only once the OS is done with both.
        let started = unsafe {
            ReadFile(
                self.pipe.as_raw_handle(),
                buf.as_mut_ptr(),
                len,
                null_mut(),
                &mut ov,
            )
        };
        if started == 0 {
            match last_error() {
                ERROR_IO_PENDING => {}
                ERROR_BROKEN_PIPE => return Ok(0),
                code => return Err(io::Error::from_raw_os_error(code as i32)),
            }
        }
        self.finish(&ov, IDLE_TIMEOUT)
    }
}

impl Write for PipeIo<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        // SAFETY: OVERLAPPED is plain data; all zeroes is its documented initial state.
        let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        ov.hEvent = self.event.as_raw_handle();
        // SAFETY: `buf` is valid for `len` bytes. `buf` and `ov` outlive the
        // write: `finish` returns only once the OS is done with both.
        let started = unsafe {
            WriteFile(
                self.pipe.as_raw_handle(),
                buf.as_ptr(),
                len,
                null_mut(),
                &mut ov,
            )
        };
        if started == 0 {
            match last_error() {
                ERROR_IO_PENDING => {}
                ERROR_BROKEN_PIPE | ERROR_NO_DATA => {
                    return Err(io::ErrorKind::BrokenPipe.into());
                }
                code => return Err(io::Error::from_raw_os_error(code as i32)),
            }
        }
        match self.finish(&ov, WRITE_TIMEOUT)? {
            0 => Err(io::ErrorKind::BrokenPipe.into()),
            n => Ok(n),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn create_instance(
    name: &[u16],
    descriptor: &SecurityDescriptor,
) -> Result<OwnedHandle, TestControlError> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    // SAFETY: `name` is NUL-terminated and `attributes` points to a live
    // security descriptor for the duration of the call.
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            BUFFER,
            BUFFER,
            0,
            &attributes,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(os_error("CreateNamedPipeW", &io::Error::last_os_error()));
    }
    // SAFETY: CreateNamedPipeW succeeded, so the handle is ours to close.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

fn os_error(call: &'static str, error: &io::Error) -> TestControlError {
    TestControlError::Os {
        call,
        code: error.raw_os_error().unwrap_or(0),
    }
}

fn from_hook(error: HookPipeError) -> TestControlError {
    match error {
        HookPipeError::Os { call, code } => TestControlError::Os { call, code },
        HookPipeError::Random | HookPipeError::Unsupported => TestControlError::Os {
            call: "user_only_descriptor",
            code: 0,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{File, OpenOptions};
    use std::io::{BufRead, BufReader};
    use std::time::Instant;
    use yien_proto::test_control::{Reply, Request};

    fn unique_name() -> String {
        format!(
            "unit-{}-{:016x}",
            std::process::id(),
            getrandom::u64().unwrap()
        )
    }

    fn open(path: &str) -> io::Result<File> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match OpenOptions::new().read(true).write(true).open(path) {
                Err(e) if e.raw_os_error() == Some(231) && Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(5));
                }
                other => return other,
            }
        }
    }

    fn ask(file: &mut File, line: &str) -> Reply {
        file.write_all(line.as_bytes()).unwrap();
        file.write_all(b"\n").unwrap();
        let mut reply = String::new();
        BufReader::new(file).read_line(&mut reply).unwrap();
        serde_json::from_str(&reply).unwrap()
    }

    #[test]
    fn adr_21_a_client_gets_one_reply_per_line_and_the_next_client_is_served() {
        let name = unique_name();
        let server = super::super::TestControlServer::start(&name, |request| match request {
            Request::State { text } => Reply::key(text),
            _ => Reply::error("no"),
        })
        .unwrap();
        for _ in 0..2 {
            let mut client = open(server.path()).unwrap();
            assert_eq!(
                ask(&mut client, r#"{"op":"state","text":true}"#),
                Reply::key(true)
            );
            assert!(!ask(&mut client, r#"{"op":"resize"}"#).ok);
            assert_eq!(ask(&mut client, r#"{"op":"state"}"#), Reply::key(false));
        }
    }

    #[test]
    fn rnf_11_a_taken_name_cannot_be_served_twice() {
        let name = unique_name();
        let _first = super::super::TestControlServer::start(&name, |_| Reply::done()).unwrap();
        assert!(matches!(
            super::super::TestControlServer::start(&name, |_| Reply::done()),
            Err(TestControlError::Os { .. })
        ));
    }

    #[test]
    fn rnf_11_only_one_client_at_a_time() {
        let name = unique_name();
        let server = super::super::TestControlServer::start(&name, |_| Reply::done()).unwrap();
        let mut first = open(server.path()).unwrap();
        assert!(ask(&mut first, r#"{"op":"state"}"#).ok);
        // ERROR_PIPE_BUSY: the single instance is taken.
        let second = OpenOptions::new()
            .read(true)
            .write(true)
            .open(server.path());
        assert_eq!(second.err().and_then(|e| e.raw_os_error()), Some(231));
    }

    #[test]
    fn dropping_the_server_frees_the_name() {
        let name = unique_name();
        let server = super::super::TestControlServer::start(&name, |_| Reply::done()).unwrap();
        let path = server.path().to_owned();
        drop(server);
        assert!(
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .is_err()
        );
        assert!(super::super::TestControlServer::start(&name, |_| Reply::done()).is_ok());
    }
}
