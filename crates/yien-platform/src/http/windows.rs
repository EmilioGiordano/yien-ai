//! WinHTTP: the system proxy, the system certificate store and Schannel TLS,
//! with no TLS stack of our own.

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::io::Write;
use std::ops::ControlFlow;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, GetLastError};
use windows_sys::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_ACCESS_TYPE_NO_PROXY, WINHTTP_FLAG_SECURE,
    WINHTTP_OPTION_REDIRECT_POLICY, WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP,
    WINHTTP_OPTION_REDIRECT_POLICY_NEVER, WINHTTP_OPTION_URL, WINHTTP_QUERY_FLAG_NUMBER,
    WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpQueryOption, WinHttpReadData,
    WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetOption, WinHttpSetTimeouts,
};

use super::{HttpError, Options, Target, final_url_ok, take_chunk};

/// One WinHTTP handle, closed once on drop.
struct Handle(*mut c_void);

impl Handle {
    fn new(raw: *mut c_void, call: &'static str) -> Result<Self, HttpError> {
        if raw.is_null() {
            Err(os_error(call))
        } else {
            Ok(Self(raw))
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from a successful WinHttpOpen, WinHttpConnect
        // or WinHttpOpenRequest and is closed only here, once.
        unsafe { WinHttpCloseHandle(self.0) };
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn os_error(call: &'static str) -> HttpError {
    // SAFETY: reads the calling thread's last error; no preconditions.
    let code = unsafe { GetLastError() };
    HttpError::Os { call, code }
}

fn check(ok: i32, call: &'static str) -> Result<(), HttpError> {
    if ok == 0 { Err(os_error(call)) } else { Ok(()) }
}

pub(super) fn get(
    target: &Target,
    options: &Options,
    sink: &mut dyn Write,
) -> Result<u64, HttpError> {
    let request = open(target, options, true)?;
    let mut buffer = vec![0u8; 64 * 1024];
    let mut written = 0u64;
    loop {
        let read = read(&request.request, &mut buffer)?;
        if read.is_empty() {
            return Ok(written);
        }
        take_chunk(read, &mut written, options.limit, sink)?;
    }
}

pub(super) fn get_stream(
    target: &Target,
    options: &Options,
    on_chunk: &mut dyn FnMut(&[u8]) -> ControlFlow<()>,
) -> Result<(), HttpError> {
    let request = open(target, options, false)?;
    let mut buffer = vec![0u8; 16 * 1024];
    loop {
        let mut available = 0u32;
        // SAFETY: `request` is a live request whose response was received;
        // `available` receives the byte count. In synchronous mode this
        // waits, up to the receive timeout, until data or the end arrives.
        check(
            unsafe { WinHttpQueryDataAvailable(request.request.0, &mut available) },
            "WinHttpQueryDataAvailable",
        )?;
        if available == 0 {
            return Ok(());
        }
        // Reading no more than is available never blocks on the next piece.
        let want = buffer.len().min(available as usize);
        let read = read(&request.request, &mut buffer[..want])?;
        if read.is_empty() {
            return Ok(());
        }
        if on_chunk(read).is_break() {
            return Ok(());
        }
    }
}

/// Reads the next piece of the body into `buffer`; empty at its end.
fn read<'a>(request: &Handle, buffer: &'a mut [u8]) -> Result<&'a [u8], HttpError> {
    let mut read = 0u32;
    let len = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
    // SAFETY: `buffer` has at least `len` writable bytes and `read`
    // receives how many WinHTTP wrote.
    check(
        unsafe { WinHttpReadData(request.0, buffer.as_mut_ptr().cast(), len, &mut read) },
        "WinHttpReadData",
    )?;
    Ok(buffer.get(..read as usize).unwrap_or_default())
}

/// A request whose answer was received with a 2xx status. Fields drop in
/// order, so the request closes before its connection and session.
struct Request {
    request: Handle,
    _connection: Handle,
    _session: Handle,
}

/// Sends the request and receives the answer's headers.
fn open(target: &Target, options: &Options, redirects: bool) -> Result<Request, HttpError> {
    let agent = wide(&options.user_agent);
    // Loopback servers must not go through a configured proxy.
    let access = if target.secure {
        WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY
    } else {
        WINHTTP_ACCESS_TYPE_NO_PROXY
    };
    // SAFETY: `agent` is a NUL-terminated UTF-16 string that outlives the
    // call; null proxy name and bypass are allowed with these access types.
    let session = Handle::new(
        unsafe { WinHttpOpen(agent.as_ptr(), access, null(), null(), 0) },
        "WinHttpOpen",
    )?;
    let ms = i32::try_from(options.timeout.as_millis()).unwrap_or(i32::MAX);
    // SAFETY: `session` is a live session handle.
    check(
        unsafe { WinHttpSetTimeouts(session.0, ms, ms, ms, ms) },
        "WinHttpSetTimeouts",
    )?;
    // A stream carries credentials, which a redirect would send on.
    let policy: u32 = if redirects {
        WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP
    } else {
        WINHTTP_OPTION_REDIRECT_POLICY_NEVER
    };
    // SAFETY: the option takes a u32 by pointer, with its size.
    check(
        unsafe {
            WinHttpSetOption(
                session.0,
                WINHTTP_OPTION_REDIRECT_POLICY,
                (&raw const policy).cast(),
                size_of::<u32>() as u32,
            )
        },
        "WinHttpSetOption",
    )?;
    let host = wide(&target.host);
    // SAFETY: `host` is NUL-terminated and outlives the call.
    let connection = Handle::new(
        unsafe { WinHttpConnect(session.0, host.as_ptr(), target.port, 0) },
        "WinHttpConnect",
    )?;
    let verb = wide("GET");
    let path = wide(&target.path);
    let flags = if target.secure {
        WINHTTP_FLAG_SECURE
    } else {
        0
    };
    // SAFETY: the strings are NUL-terminated and outlive the call; null
    // version, referrer and accept types mean HTTP/1.1, none and none.
    let request = Handle::new(
        unsafe {
            WinHttpOpenRequest(
                connection.0,
                verb.as_ptr(),
                path.as_ptr(),
                null(),
                null(),
                null(),
                flags,
            )
        },
        "WinHttpOpenRequest",
    )?;
    let headers: String = options
        .headers
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect();
    let headers = wide(&headers);
    let headers_len = (headers.len() - 1) as u32;
    // SAFETY: `headers` holds `headers_len` UTF-16 units and outlives the
    // call; there is no request body.
    check(
        unsafe {
            WinHttpSendRequest(
                request.0,
                if headers_len == 0 {
                    null()
                } else {
                    headers.as_ptr()
                },
                headers_len,
                null(),
                0,
                0,
                0,
            )
        },
        "WinHttpSendRequest",
    )?;
    // SAFETY: `request` was sent above; the reserved argument must be null.
    check(
        unsafe { WinHttpReceiveResponse(request.0, null_mut()) },
        "WinHttpReceiveResponse",
    )?;
    if !final_url_ok(&final_url(&request)?, options) {
        return Err(HttpError::Insecure);
    }
    let status = status(&request)?;
    if !(200..300).contains(&status) {
        return Err(HttpError::Status(status));
    }
    Ok(Request {
        request,
        _connection: connection,
        _session: session,
    })
}

fn status(request: &Handle) -> Result<u32, HttpError> {
    let mut code = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: with FLAG_NUMBER the status is written as one u32 into `code`,
    // whose size `size` gives; a null name and index ask by info level.
    check(
        unsafe {
            WinHttpQueryHeaders(
                request.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                null(),
                (&raw mut code).cast(),
                &mut size,
                null_mut(),
            )
        },
        "WinHttpQueryHeaders",
    )?;
    Ok(code)
}

/// The URL the response came from, after redirects; a signed download URL
/// can be longer than the first buffer.
fn final_url(request: &Handle) -> Result<String, HttpError> {
    let mut buffer = vec![0u16; 2048];
    for _ in 0..2 {
        let mut size = (buffer.len() * 2) as u32;
        // SAFETY: `buffer` has `size` writable bytes; WinHTTP writes the URL
        // with its NUL and sets `size` to the bytes written without it, or
        // to the bytes needed when the buffer is too small.
        let ok = unsafe {
            WinHttpQueryOption(
                request.0,
                WINHTTP_OPTION_URL,
                buffer.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if ok != 0 {
            let units = (size as usize / 2).min(buffer.len());
            return Ok(String::from_utf16_lossy(&buffer[..units]));
        }
        let error = os_error("WinHttpQueryOption");
        if !matches!(
            error,
            HttpError::Os {
                code: ERROR_INSUFFICIENT_BUFFER,
                ..
            }
        ) {
            return Err(error);
        }
        buffer = vec![0u16; size as usize / 2 + 1];
    }
    Err(os_error("WinHttpQueryOption"))
}
