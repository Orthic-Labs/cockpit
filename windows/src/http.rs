//! Minimal HTTPS GET over WinHTTP (system proxy settings, system certificate store). The
//! symbols are declared here against `winhttp.dll` so the notch needs no extra `windows`
//! crate features or dependencies. Request headers can carry bearer tokens: this module
//! never logs them, and callers must not either.

use std::ffi::c_void;
use windows::core::{BOOL, PCWSTR};

const ACCESS_TYPE_AUTOMATIC_PROXY: u32 = 4;
const FLAG_SECURE: u32 = 0x0080_0000;
const QUERY_STATUS_CODE: u32 = 19;
const QUERY_FLAG_NUMBER: u32 = 0x2000_0000;
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

/// HTTPS GET `https://{host}{path}` with the given request headers. Blocking; bounded by
/// `timeout_ms` per network stage and by a body size cap.
pub fn get(
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    timeout_ms: i32,
) -> Result<Response, HttpError> {
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
    // (guards drop in reverse declaration order: request, connection, session).
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
        let mut body = Vec::new();
        loop {
            let mut available = 0u32;
            if !WinHttpQueryDataAvailable(request.0, &mut available).as_bool() {
                return Err(HttpError::Read);
            }
            if available == 0 {
                break;
            }
            if body.len() + available as usize > MAX_BODY_BYTES {
                return Err(HttpError::TooLarge);
            }
            let start = body.len();
            body.resize(start + available as usize, 0);
            let mut read = 0u32;
            if !WinHttpReadData(
                request.0,
                body[start..].as_mut_ptr().cast(),
                available,
                &mut read,
            )
            .as_bool()
            {
                return Err(HttpError::Read);
            }
            body.truncate(start + read as usize);
            if read == 0 {
                break;
            }
        }
        Ok(Response { status, body })
    }
}
