//! Minimal HTTPS GET over WinHTTP (system proxy settings, system certificate store). The
//! symbols are declared here against `winhttp.dll` so the notch needs no extra `windows`
//! crate features or dependencies. Request headers can carry bearer tokens: this module
//! never logs them, and callers must not either.

use std::ffi::c_void;
use windows::core::{BOOL, PCWSTR};

const ACCESS_TYPE_AUTOMATIC_PROXY: u32 = 4;
const FLAG_SECURE: u32 = 0x0080_0000;
const QUERY_STATUS_CODE: u32 = 19;
const QUERY_CONTENT_LENGTH: u32 = 5;
const QUERY_FLAG_NUMBER: u32 = 0x2000_0000;
/// `WINHTTP_QUERY_CUSTOM`: the header named by the `name` argument.
const QUERY_CUSTOM: u32 = 65_535;
/// A server-directed delay longer than this is not believed (a day).
const RETRY_AFTER_MAX_SECONDS: u64 = 24 * 3600;
/// `dwHeadersLength` value meaning "the header string is NUL-terminated".
const HEADERS_NUL_TERMINATED: u32 = u32::MAX;
const MAX_BODY_BYTES: usize = 512 * 1024;

#[allow(non_snake_case)]
#[link(name = "winhttp")]
unsafe extern "system" {
    fn WinHttpOpen(
        agent: PCWSTR,
        access_type: u32,
        proxy: PCWSTR,
        proxy_bypass: PCWSTR,
        flags: u32,
    ) -> *mut c_void;
    fn WinHttpConnect(
        session: *mut c_void,
        server: PCWSTR,
        port: u16,
        reserved: u32,
    ) -> *mut c_void;
    fn WinHttpOpenRequest(
        connection: *mut c_void,
        verb: PCWSTR,
        object: PCWSTR,
        version: PCWSTR,
        referrer: PCWSTR,
        accept_types: *const PCWSTR,
        flags: u32,
    ) -> *mut c_void;
    fn WinHttpSetTimeouts(
        handle: *mut c_void,
        resolve_ms: i32,
        connect_ms: i32,
        send_ms: i32,
        receive_ms: i32,
    ) -> BOOL;
    fn WinHttpSendRequest(
        request: *mut c_void,
        headers: PCWSTR,
        headers_len: u32,
        optional: *const c_void,
        optional_len: u32,
        total_len: u32,
        context: usize,
    ) -> BOOL;
    fn WinHttpReceiveResponse(request: *mut c_void, reserved: *mut c_void) -> BOOL;
    fn WinHttpQueryHeaders(
        request: *mut c_void,
        info_level: u32,
        name: PCWSTR,
        buffer: *mut c_void,
        buffer_len: *mut u32,
        index: *mut u32,
    ) -> BOOL;
    fn WinHttpQueryDataAvailable(request: *mut c_void, available: *mut u32) -> BOOL;
    fn WinHttpReadData(
        request: *mut c_void,
        buffer: *mut c_void,
        to_read: u32,
        read: *mut u32,
    ) -> BOOL;
    fn WinHttpCloseHandle(handle: *mut c_void) -> BOOL;
}

#[derive(Debug)]
pub struct Response {
    pub status: u32,
    pub body: Vec<u8>,
    /// The server's `Retry-After` in seconds (the numeric form), when it sent one.
    pub retry_after: Option<u64>,
}

/// Failure stage only: nothing request-specific (URL, headers) is carried.
#[derive(Debug)]
pub enum HttpError {
    Open,
    Connect,
    Request,
    Send,
    Receive,
    Read,
    TooLarge,
    /// A download got a status other than 200.
    BadStatus,
    /// The destination could not be written.
    Write,
}

/// Closes a WinHTTP handle on drop.
struct Handle(*mut c_void);

impl Handle {
    fn new(raw: *mut c_void) -> Option<Self> {
        (!raw.is_null()).then_some(Self(raw))
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from a WinHTTP open call and is closed exactly once here.
        let _ = unsafe { WinHttpCloseHandle(self.0) };
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// An open, answered request. Fields drop in declaration order: request, connection, session.
struct Exchange {
    request: Handle,
    _connection: Handle,
    _session: Handle,
    status: u32,
}

/// Sends the request and reads the status line; the body is left to the caller.
fn begin(
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    timeout_ms: i32,
) -> Result<Exchange, HttpError> {
    let agent = wide("PulseNotch/1");
    let host_w = wide(host);
    let path_w = wide(path);
    let verb = wide("GET");
    let mut header_text = String::new();
    for (name, value) in headers {
        header_text.push_str(name);
        header_text.push_str(": ");
        header_text.push_str(value);
        header_text.push_str("\r\n");
    }
    let header_w = wide(&header_text);

    // SAFETY: every pointer passed below is either null, a NUL-terminated UTF-16 buffer that
    // outlives the call, or a handle owned by a `Handle` guard that outlives its children
    // (the returned `Exchange` keeps them in child-first drop order).
    unsafe {
        let session = Handle::new(WinHttpOpen(
            PCWSTR(agent.as_ptr()),
            ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ))
        .ok_or(HttpError::Open)?;
        let _ = WinHttpSetTimeouts(session.0, timeout_ms, timeout_ms, timeout_ms, timeout_ms);
        let connection = Handle::new(WinHttpConnect(session.0, PCWSTR(host_w.as_ptr()), 443, 0))
            .ok_or(HttpError::Connect)?;
        let request = Handle::new(WinHttpOpenRequest(
            connection.0,
            PCWSTR(verb.as_ptr()),
            PCWSTR(path_w.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            FLAG_SECURE,
        ))
        .ok_or(HttpError::Request)?;
        if !WinHttpSendRequest(
            request.0,
            PCWSTR(header_w.as_ptr()),
            HEADERS_NUL_TERMINATED,
            std::ptr::null(),
            0,
            0,
            0,
        )
        .as_bool()
        {
            return Err(HttpError::Send);
        }
        if !WinHttpReceiveResponse(request.0, std::ptr::null_mut()).as_bool() {
            return Err(HttpError::Receive);
        }
        let mut status = 0u32;
        let mut status_len = std::mem::size_of::<u32>() as u32;
        if !WinHttpQueryHeaders(
            request.0,
            QUERY_STATUS_CODE | QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            (&mut status as *mut u32).cast(),
            &mut status_len,
            std::ptr::null_mut(),
        )
        .as_bool()
        {
            return Err(HttpError::Receive);
        }
        Ok(Exchange {
            request,
            _connection: connection,
            _session: session,
            status,
        })
    }
}

/// Reads the next chunk of the body into `buffer`; `Ok(0)` is the end.
fn read_chunk(request: &Handle, buffer: &mut [u8]) -> Result<usize, HttpError> {
    // SAFETY: `buffer` is valid for `to_read` bytes and the handle is live.
    unsafe {
        let mut available = 0u32;
        if !WinHttpQueryDataAvailable(request.0, &mut available).as_bool() {
            return Err(HttpError::Read);
        }
        let to_read = available.min(buffer.len() as u32);
        if to_read == 0 {
            return Ok(0);
        }
        let mut read = 0u32;
        if !WinHttpReadData(request.0, buffer.as_mut_ptr().cast(), to_read, &mut read).as_bool() {
            return Err(HttpError::Read);
        }
        Ok(read as usize)
    }
}

/// The text of one response header, or `None` when the response has none.
fn header_text(request: &Handle, name: &str) -> Option<String> {
    let name_w = wide(name);
    let mut len = 0u32;
    // SAFETY: size probe with a null buffer; the call reports the needed byte count in `len`
    // (and fails with "insufficient buffer", which is expected and ignored).
    let _ = unsafe {
        WinHttpQueryHeaders(
            request.0,
            QUERY_CUSTOM,
            PCWSTR(name_w.as_ptr()),
            std::ptr::null_mut(),
            &mut len,
            std::ptr::null_mut(),
        )
    };
    if len == 0 || len > 512 {
        return None;
    }
    let mut buffer = vec![0u16; (len as usize).div_ceil(2) + 1];
    let mut capacity = (buffer.len() * 2) as u32;
    // SAFETY: `buffer` is valid for `capacity` bytes; the name outlives the call.
    let found = unsafe {
        WinHttpQueryHeaders(
            request.0,
            QUERY_CUSTOM,
            PCWSTR(name_w.as_ptr()),
            buffer.as_mut_ptr().cast(),
            &mut capacity,
            std::ptr::null_mut(),
        )
        .as_bool()
    };
    if !found {
        return None;
    }
    let units = (capacity as usize / 2).min(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..units]))
}

/// `Retry-After` as whole seconds. The HTTP-date form is not read (a missing delay is
/// handled like no header at all).
fn retry_after(request: &Handle) -> Option<u64> {
    let seconds: u64 = header_text(request, "Retry-After")?.trim().parse().ok()?;
    Some(seconds.min(RETRY_AFTER_MAX_SECONDS))
}

/// HTTPS GET `https://{host}{path}` with the given request headers. Blocking; bounded by
/// `timeout_ms` per network stage and by a body size cap.
pub fn get(
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    timeout_ms: i32,
) -> Result<Response, HttpError> {
    let exchange = begin(host, path, headers, timeout_ms)?;
    let mut body = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        let read = read_chunk(&exchange.request, &mut chunk)?;
        if read == 0 {
            break;
        }
        if body.len() + read > MAX_BODY_BYTES {
            return Err(HttpError::TooLarge);
        }
        body.extend_from_slice(&chunk[..read]);
    }
    let retry_after = retry_after(&exchange.request);
    Ok(Response {
        status: exchange.status,
        body,
        retry_after,
    })
}

/// HTTPS GET streamed into `out` (redirects are followed by WinHTTP). Only a 200 answer is
/// written. `progress(received, total)` runs per chunk; `total` is the Content-Length when
/// the server gave one. Returns the bytes written; fails with `TooLarge` past `max_bytes`.
pub fn download(
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    timeout_ms: i32,
    max_bytes: u64,
    out: &mut impl std::io::Write,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<u64, HttpError> {
    let exchange = begin(host, path, headers, timeout_ms)?;
    if exchange.status != 200 {
        return Err(HttpError::BadStatus);
    }
    let mut length = 0u32;
    let mut length_len = std::mem::size_of::<u32>() as u32;
    // SAFETY: the out buffer is a live u32 of the declared size; the handle is live.
    let total = unsafe {
        WinHttpQueryHeaders(
            exchange.request.0,
            QUERY_CONTENT_LENGTH | QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            (&mut length as *mut u32).cast(),
            &mut length_len,
            std::ptr::null_mut(),
        )
        .as_bool()
    }
    .then_some(u64::from(length));
    if total.is_some_and(|t| t > max_bytes) {
        return Err(HttpError::TooLarge);
    }
    let mut received = 0u64;
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let read = read_chunk(&exchange.request, &mut chunk)?;
        if read == 0 {
            break;
        }
        received += read as u64;
        if received > max_bytes {
            return Err(HttpError::TooLarge);
        }
        out.write_all(&chunk[..read])
            .map_err(|_| HttpError::Write)?;
        progress(received, total);
    }
    Ok(received)
}
