//! Windows per-user named-pipe transport for the settled IPC contract.
//!
//! * One pipe instance (`FILE_FLAG_FIRST_PIPE_INSTANCE`, max 1), remote
//!   clients rejected, DACL granting `GENERIC_ALL` to the current user SID only.
//! * The server verifies the client process token SID BEFORE reading any
//!   request byte; the client opens with `SECURITY_IDENTIFICATION` and
//!   verifies the server process token SID. No impersonation is used.
//! * All waits use overlapped I/O + `WaitForSingleObject` ticks and are
//!   cancelled with `CancelIoEx` when a bound expires, then drained for at
//!   most `CANCEL_DRAIN`. Each operation's OVERLAPPED and buffer are owned
//!   by a heap `IoCtx`; if the kernel never confirms release, the box is
//!   deliberately leaked rather than freed while the kernel may own it.
//! * Responses never exceed `max_response_bytes`: an oversized serialized
//!   error is replaced by a minimal one, and if even that cannot fit the
//!   connection is closed with no frame. `max_response_bytes` below the
//!   minimal error frame is rejected by `validate_limits`.

use super::{
    ErrorCode, Handler, IpcError, Limits, ServeExit, error_response,
};
use ::windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND,
    ERROR_INSUFFICIENT_BUFFER, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_PIPE_BUSY,
    ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, HANDLE, HLOCAL, LocalFree, WAIT_OBJECT_0,
    WAIT_TIMEOUT, WIN32_ERROR,
};
use ::windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use ::windows::Win32::Security::{
    EqualSid, GetTokenInformation, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY,
    TOKEN_USER, TokenUser,
};
use ::windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_GENERIC_READ,
    FILE_GENERIC_WRITE, FILE_SHARE_NONE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile,
    SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT, WriteFile,
};
use ::windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use ::windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
    GetNamedPipeServerProcessId, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_WAIT, WaitNamedPipeW,
};
use ::windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, OpenProcess, OpenProcessToken,
    PROCESS_QUERY_LIMITED_INFORMATION, ResetEvent, WaitForSingleObject,
};
use ::windows::core::{HRESULT, PCWSTR, PWSTR};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const PIPE_PREFIX: &str = r"\\.\pipe\";
/// Maximum pipe name length after the prefix (Win32 limit is 256).
const MAX_PIPE_NAME: usize = 200;
/// Wake-up interval for shutdown / idle / deadline checks.
const TICK_MS: u32 = 50;
/// Bound for the kernel to confirm release of an OVERLAPPED after
/// CancelIoEx. Generous for real completions, far short of unbounded.
const CANCEL_DRAIN: Duration = Duration::from_secs(5);
const CHUNK: usize = 64 * 1024;
const PIPE_BUFFER: u32 = 64 * 1024;

fn hr(code: WIN32_ERROR) -> HRESULT {
    HRESULT::from_win32(code.0)
}

fn is(error: &::windows::core::Error, code: WIN32_ERROR) -> bool {
    error.code() == hr(code)
}

fn map_win(error: &::windows::core::Error) -> IpcError {
    let closed = [ERROR_BROKEN_PIPE, ERROR_PIPE_NOT_CONNECTED, ERROR_NO_DATA];
    if closed.iter().any(|code| is(error, *code)) {
        IpcError::new(ErrorCode::TransportClosed, error.to_string())
    } else {
        IpcError::new(ErrorCode::Io, error.to_string())
    }
}

/// Owned kernel handle, closed on drop.
struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: handle is owned by this wrapper and closed exactly once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

/// Owned security descriptor allocated by the SDDL converter.
struct SecurityDescriptor(PSECURITY_DESCRIPTOR);
impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        if !self.0.0.is_null() {
            // SAFETY: allocated with LocalAlloc by the converter; freed once.
            let _ = unsafe { LocalFree(Some(HLOCAL(self.0.0))) };
        }
    }
}

/// Calls DisconnectNamedPipe when a served connection ends, unless an
/// overlapped operation was orphaned. An orphaned pipe is terminal: it is
/// abandoned without another Win32 operation, then its handle is dropped.
struct Connection<'a> {
    pipe: &'a Handle,
    disconnect: bool,
}

impl<'a> Connection<'a> {
    fn new(pipe: &'a Handle) -> Self {
        Self {
            pipe,
            disconnect: true,
        }
    }

    fn abandon(&mut self) {
        self.disconnect = false;
    }
}

impl Drop for Connection<'_> {
    fn drop(&mut self) {
        if self.disconnect {
            // SAFETY: valid server pipe handle.
            let _ = unsafe { DisconnectNamedPipe(self.pipe.0) };
        }
    }
}

/// TOKEN_USER buffer; `u64` storage keeps it pointer-aligned.
struct UserSid {
    buffer: Vec<u64>,
}
impl UserSid {
    fn sid(&self) -> PSID {
        // SAFETY: buffer was filled by GetTokenInformation(TokenUser).
        unsafe { (*(self.buffer.as_ptr() as *const TOKEN_USER)).User.Sid }
    }
}

fn user_sid_of_token(token: &Handle) -> Result<UserSid, IpcError> {
    let mut needed = 0u32;
    // SAFETY: size probe; null buffer with zero length.
    let probe = unsafe { GetTokenInformation(token.0, TokenUser, None, 0, &mut needed) };
    match probe {
        Err(e) if is(&e, ERROR_INSUFFICIENT_BUFFER) && needed > 0 => {}
        Err(e) => return Err(IpcError::new(ErrorCode::Io, e.to_string())),
        Ok(()) => return Err(IpcError::new(ErrorCode::Internal, "empty token user")),
    }
    let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
    let mut written = 0u32;
    // SAFETY: buffer holds at least `needed` bytes.
    let filled = unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buffer.as_mut_ptr() as *mut core::ffi::c_void),
            needed,
            &mut written,
        )
    };
    filled.map_err(|e| IpcError::new(ErrorCode::Io, e.to_string()))?;
    Ok(UserSid { buffer })
}

fn own_user_sid() -> Result<UserSid, IpcError> {
    let mut token = HANDLE(core::ptr::null_mut());
    // SAFETY: pseudo handle for the current process; token closed by Handle.
    let opened = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) };
    opened.map_err(|e| IpcError::new(ErrorCode::Io, e.to_string()))?;
    user_sid_of_token(&Handle(token))
}

/// SID of the user owning process `pid`; fails closed.
fn process_user_sid(pid: u32) -> Result<UserSid, IpcError> {
    let denied = |detail: String| IpcError::new(ErrorCode::UnauthenticatedPeer, detail);
    // SAFETY: plain OpenProcess; handle closed by Handle.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
        .map(Handle)
        .map_err(|e| denied(format!("cannot open peer process: {e}")))?;
    let mut token = HANDLE(core::ptr::null_mut());
    // SAFETY: valid process handle; token closed by Handle.
    let opened = unsafe { OpenProcessToken(process.0, TOKEN_QUERY, &mut token) };
    opened.map_err(|e| denied(format!("cannot open peer token: {e}")))?;
    user_sid_of_token(&Handle(token))
        .map_err(|e| denied(format!("cannot read peer identity: {}", e.message)))
}

fn same_sid(a: &UserSid, b: &UserSid) -> bool {
    // SAFETY: both SIDs live inside their buffers.
    let equal = unsafe { EqualSid(a.sid(), b.sid()) };
    equal.is_ok()
}

fn sid_to_string(sid: PSID) -> Result<String, IpcError> {
    let mut text = PWSTR(core::ptr::null_mut());
    // SAFETY: sid is valid; the returned string is LocalAlloc'd and freed below.
    let converted = unsafe { ConvertSidToStringSidW(sid, &mut text) };
    converted.map_err(|e| IpcError::new(ErrorCode::Io, e.to_string()))?;
    // SAFETY: text is a NUL-terminated UTF-16 string.
    let value = unsafe {
        let mut len = 0usize;
        while *text.0.add(len) != 0 {
            len += 1;
        }
        let value = String::from_utf16_lossy(core::slice::from_raw_parts(text.0, len));
        let _ = LocalFree(Some(HLOCAL(text.0 as *mut core::ffi::c_void)));
        value
    };
    Ok(value)
}

/// `\\.\pipe\cockpit-worker-<current user SID string>`.
pub fn default_endpoint() -> Result<String, IpcError> {
    let sid = own_user_sid()?;
    Ok(format!("{PIPE_PREFIX}cockpit-worker-{}", sid_to_string(sid.sid())?))
}

fn unsafe_endpoint(reason: &str) -> IpcError {
    IpcError::new(ErrorCode::EndpointUnsafe, reason)
}

/// Validates the endpoint and returns it as NUL-terminated UTF-16.
fn validate_endpoint(endpoint: &str) -> Result<Vec<u16>, IpcError> {
    let Some(name) = endpoint.strip_prefix(PIPE_PREFIX) else {
        return Err(unsafe_endpoint("endpoint must start with \\\\.\\pipe\\"));
    };
    if name.is_empty() || name.len() > MAX_PIPE_NAME {
        return Err(unsafe_endpoint("pipe name empty or too long"));
    }
    if name.contains('\\') || name.contains('/') || name.contains('\0') {
        return Err(unsafe_endpoint("pipe name contains a path separator"));
    }
    Ok(endpoint.encode_utf16().chain(Some(0)).collect())
}

// ---------------------------------------------------------------------
// Overlapped I/O helpers
// ---------------------------------------------------------------------

fn new_event() -> Result<Handle, IpcError> {
    // SAFETY: manual-reset, initially non-signalled, unnamed event.
    let created = unsafe { CreateEventW(None, true, false, PCWSTR(core::ptr::null())) };
    created
        .map(Handle)
        .map_err(|e| IpcError::new(ErrorCode::Io, e.to_string()))
}

fn new_overlapped(event: &Handle) -> Result<OVERLAPPED, IpcError> {
    // SAFETY: valid event handle.
    let reset = unsafe { ResetEvent(event.0) };
    reset.map_err(|e| IpcError::new(ErrorCode::Io, e.to_string()))?;
    Ok(OVERLAPPED {
        hEvent: event.0,
        ..OVERLAPPED::default()
    })
}

/// Heap-stable context for one in-flight overlapped operation. The event,
/// OVERLAPPED and I/O staging buffer live in a single boxed
/// allocation at fixed addresses, so they stay valid for as long as the
/// kernel may reference them. If the kernel never confirms release (see
/// `drain`), the caller MUST leak the box — via `orphan` — rather than
/// free memory the kernel may still own.
struct IoCtx {
    event: Handle,
    overlapped: OVERLAPPED,
    /// Staging buffer for one chunk; empty for ConnectNamedPipe.
    buffer: Vec<u8>,
}

fn new_io_ctx(capacity: usize) -> Result<Box<IoCtx>, IpcError> {
    let event = new_event()?;
    let overlapped = new_overlapped(&event)?;
    Ok(Box::new(IoCtx {
        event,
        overlapped,
        buffer: vec![0u8; capacity],
    }))
}

enum Completion {
    Done(u32),
    Cancelled,
    Failed(::windows::core::Error),
    /// CancelIoEx was issued but the kernel did not confirm release of
    /// the IoCtx within CANCEL_DRAIN. The caller must `orphan` the box.
    Orphaned,
}

/// Bounded post-cancellation drain: after CancelIoEx, wait up to
/// CANCEL_DRAIN for the kernel to signal the OVERLAPPED event (which
/// confirms it no longer references the IoCtx). Never blocks unbounded.
fn drain(pipe: HANDLE, ctx: &IoCtx, failure: Option<::windows::core::Error>) -> Completion {
    // SAFETY: ctx is heap-stable and owned by the caller; the operation
    // was started against this same OVERLAPPED.
    let _ = unsafe { CancelIoEx(pipe, Some(&ctx.overlapped as *const OVERLAPPED)) };
    let deadline = Instant::now() + CANCEL_DRAIN;
    loop {
        // SAFETY: event handle is owned by this IoCtx and remains valid.
        let wait = unsafe { WaitForSingleObject(ctx.event.0, TICK_MS) };
        if wait == WAIT_OBJECT_0 {
            let mut bytes = 0u32;
            // SAFETY: the event is signalled, so the kernel has released
            // ctx; a non-waiting GetOverlappedResult just reads status.
            let outcome =
                unsafe { GetOverlappedResult(pipe, &ctx.overlapped, &mut bytes, false) };
            return match (outcome.map(|()| bytes), failure) {
                (_, Some(e)) => Completion::Failed(e),
                (Ok(done), None) => Completion::Done(done),
                (Err(_), None) => Completion::Cancelled,
            };
        }
        if wait != WAIT_TIMEOUT || Instant::now() >= deadline {
            return Completion::Orphaned;
        }
    }
}

/// Waits for a started overlapped operation in ticks. Returns only after
/// the kernel confirmed it released `ctx` (Done/Cancelled/Failed) or the
/// drain bound expired (Orphaned — caller must leak, never drop).
fn complete(pipe: HANDLE, ctx: &mut IoCtx, mut keep_going: impl FnMut() -> bool) -> Completion {
    loop {
        // SAFETY: event handle is owned by this IoCtx and remains valid.
        let wait = unsafe { WaitForSingleObject(ctx.event.0, TICK_MS) };
        if wait == WAIT_OBJECT_0 {
            let mut bytes = 0u32;
            // SAFETY: operation signalled complete; ctx may be reused/freed.
            return match unsafe { GetOverlappedResult(pipe, &ctx.overlapped, &mut bytes, false) }
            {
                Ok(()) => Completion::Done(bytes),
                Err(e) => Completion::Failed(e),
            };
        }
        let failure = if wait == WAIT_TIMEOUT {
            if keep_going() {
                continue;
            }
            None
        } else {
            Some(::windows::core::Error::from_win32())
        };
        return drain(pipe, ctx, failure);
    }
}

fn read_exact(
    pipe: &Handle,
    buffer: &mut [u8],
    deadline: Instant,
) -> Result<(), TransportFailure> {
    let mut offset = 0;
    while offset < buffer.len() {
        let chunk = CHUNK.min(buffer.len() - offset);
        let mut ctx = new_io_ctx(chunk).map_err(TransportFailure::Error)?;
        // SAFETY: ctx stays at a fixed heap address. `complete` returns
        // only after the kernel releases it; on Orphaned we leak the box
        // below rather than free memory the kernel may still own.
        let started = unsafe {
            ReadFile(
                pipe.0,
                Some(ctx.buffer.as_mut_slice()),
                None,
                Some(&mut ctx.overlapped as *mut OVERLAPPED),
            )
        };
        match started {
            Ok(()) => {}
            Err(e) if is(&e, ERROR_IO_PENDING) => {}
            Err(e) => return Err(TransportFailure::Error(map_win(&e))),
        }
        match complete(pipe.0, &mut ctx, || Instant::now() < deadline) {
            Completion::Done(0) => {
                return Err(TransportFailure::Error(IpcError::new(
                    ErrorCode::TransportClosed,
                    "peer closed",
                )));
            }
            Completion::Done(count) => {
                let count = (count as usize).min(chunk);
                buffer[offset..offset + count].copy_from_slice(&ctx.buffer[..count]);
                offset += count;
            }
            Completion::Cancelled => {
                return Err(TransportFailure::Error(IpcError::new(
                    ErrorCode::Timeout,
                    "read timed out",
                )));
            }
            Completion::Orphaned => {
                let _ = Box::into_raw(ctx); // deliberate leak, see IoCtx
                return Err(TransportFailure::Orphaned);
            }
            Completion::Failed(e) => return Err(TransportFailure::Error(map_win(&e))),
        }
    }
    Ok(())
}

fn write_all(
    pipe: &Handle,
    buffer: &[u8],
    deadline: Instant,
) -> Result<(), TransportFailure> {
    let mut offset = 0;
    while offset < buffer.len() {
        let end = (offset + CHUNK).min(buffer.len());
        let mut ctx = new_io_ctx(end - offset).map_err(TransportFailure::Error)?;
        ctx.buffer.copy_from_slice(&buffer[offset..end]);
        // SAFETY: same invariants as `read_exact` — fixed heap address,
        // released by the kernel before `complete` returns, else leaked.
        let started = unsafe {
            WriteFile(
                pipe.0,
                Some(ctx.buffer.as_slice()),
                None,
                Some(&mut ctx.overlapped as *mut OVERLAPPED),
            )
        };
        match started {
            Ok(()) => {}
            Err(e) if is(&e, ERROR_IO_PENDING) => {}
            Err(e) => return Err(TransportFailure::Error(map_win(&e))),
        }
        match complete(pipe.0, &mut ctx, || Instant::now() < deadline) {
            Completion::Done(0) => {
                return Err(TransportFailure::Error(IpcError::new(
                    ErrorCode::TransportClosed,
                    "peer closed",
                )));
            }
            Completion::Done(count) => offset += count as usize,
            Completion::Cancelled => {
                return Err(TransportFailure::Error(IpcError::new(
                    ErrorCode::Timeout,
                    "write timed out",
                )));
            }
            Completion::Orphaned => {
                let _ = Box::into_raw(ctx); // deliberate leak, see IoCtx
                return Err(TransportFailure::Orphaned);
            }
            Completion::Failed(e) => return Err(TransportFailure::Error(map_win(&e))),
        }
    }
    Ok(())
}

fn send_frame(
    pipe: &Handle,
    body: &[u8],
    max: usize,
    wait: Duration,
) -> Result<(), TransportFailure> {
    if body.is_empty() || body.len() > max {
        return Err(TransportFailure::Error(IpcError::new(
            ErrorCode::OversizedFrame,
            format!("frame of {} bytes outside 1..={max}", body.len()),
        )));
    }
    let length = u32::try_from(body.len())
        .map_err(|_| {
            TransportFailure::Error(IpcError::new(
                ErrorCode::OversizedFrame,
                "frame exceeds u32",
            ))
        })?;
    let deadline = Instant::now() + wait;
    write_all(pipe, &length.to_be_bytes(), deadline)?;
    write_all(pipe, body, deadline)
}

enum TransportFailure {
    Error(IpcError),
    /// The kernel did not confirm cancellation before the drain bound. The
    /// owning pipe must be discarded; its IoCtx is intentionally leaked.
    Orphaned,
}

/// Reads a frame; refuses the body when the declared length is zero or
/// above `max`.
fn receive_frame(
    pipe: &Handle,
    max: usize,
    wait: Duration,
) -> Result<Vec<u8>, TransportFailure> {
    let deadline = Instant::now() + wait;
    let mut header = [0u8; 4];
    read_exact(pipe, &mut header, deadline)?;
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 {
        return Err(TransportFailure::Error(IpcError::new(
            ErrorCode::MalformedRequest,
            "empty frame",
        )));
    }
    if length > max {
        return Err(TransportFailure::Error(IpcError::new(
            ErrorCode::OversizedFrame,
            format!("declared frame of {length} bytes exceeds {max}"),
        )));
    }
    let mut body = vec![0u8; length];
    read_exact(pipe, &mut body, deadline)?;
    Ok(body)
}

// ---------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------

fn create_pipe(wide_name: &[u16], descriptor: &SecurityDescriptor) -> Result<Handle, IpcError> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: core::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0.0,
        bInheritHandle: false.into(),
    };
    // SAFETY: wide_name is NUL-terminated; attributes outlive the call.
    let pipe = unsafe {
        CreateNamedPipeW(
            PCWSTR(wide_name.as_ptr()),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_REJECT_REMOTE_CLIENTS | PIPE_WAIT,
            1,
            PIPE_BUFFER,
            PIPE_BUFFER,
            0,
            Some(&attributes as *const SECURITY_ATTRIBUTES),
        )
    };
    if pipe.is_invalid() {
        // Read the last error immediately, before any other call.
        let error = ::windows::core::Error::from_win32();
        return Err(if is(&error, ERROR_ACCESS_DENIED) || is(&error, ERROR_PIPE_BUSY) {
            IpcError::new(ErrorCode::EndpointInUse, "pipe instance already exists")
        } else {
            IpcError::new(ErrorCode::Io, format!("CreateNamedPipeW: {error}"))
        });
    }
    Ok(Handle(pipe))
}

enum Accept {
    Connected,
    Exit(ServeExit),
    Failed,
    /// The pipe instance has an overlapped operation whose release was not
    /// confirmed. It must not be disconnected and reused.
    Orphaned,
}

fn accept(
    pipe: &Handle,
    shutdown: &AtomicBool,
    idle_exit: Duration,
    since: Instant,
) -> Accept {
    let Ok(mut ctx) = new_io_ctx(0) else {
        return Accept::Failed;
    };
    // SAFETY: ctx is heap-stable; `complete` returns only after the kernel
    // released it, and on Orphaned the box is leaked rather than freed.
    match unsafe { ConnectNamedPipe(pipe.0, Some(&mut ctx.overlapped as *mut OVERLAPPED)) } {
        Ok(()) => {}
        Err(e) if is(&e, ERROR_PIPE_CONNECTED) => return Accept::Connected,
        Err(e) if is(&e, ERROR_IO_PENDING) => {}
        Err(_) => return Accept::Failed,
    }
    let waiting = || !shutdown.load(Ordering::Acquire) && since.elapsed() < idle_exit;
    match complete(pipe.0, &mut ctx, waiting) {
        Completion::Done(_) => Accept::Connected,
        Completion::Cancelled => {
            if shutdown.load(Ordering::Acquire) {
                Accept::Exit(ServeExit::ShutdownRequested)
            } else {
                Accept::Exit(ServeExit::IdleTimeout)
            }
        }
        Completion::Orphaned => {
            let _ = Box::into_raw(ctx); // deliberate leak, see IoCtx
            Accept::Orphaned
        }
        Completion::Failed(_) => Accept::Failed,
    }
}

fn verify_client(pipe: &Handle, own: &UserSid) -> Result<(), IpcError> {
    let mut pid = 0u32;
    // SAFETY: valid connected server pipe handle.
    let queried = unsafe { GetNamedPipeClientProcessId(pipe.0, &mut pid) };
    queried
        .map_err(|e| IpcError::new(ErrorCode::UnauthenticatedPeer, e.to_string()))?;
    let peer = process_user_sid(pid)?;
    if same_sid(own, &peer) {
        Ok(())
    } else {
        Err(IpcError::new(ErrorCode::UnauthenticatedPeer, "peer user differs"))
    }
}

/// Serialized error frame guaranteed to be no larger than `max` when the
/// limit can carry one at all. The full message is tried first; on
/// overflow a minimal error with an empty message is used; if even that
/// exceeds `max`, None and the caller must close the connection rather
/// than send an oversized frame.
fn error_frame_within(code: ErrorCode, message: &str, max: usize) -> Option<Vec<u8>> {
    let full = error_response(None, IpcError::new(code, message));
    if full.len() <= max {
        return Some(full);
    }
    let minimal = error_response(None, IpcError::new(code, ""));
    (minimal.len() <= max).then_some(minimal)
}

/// Rejects limits that cannot carry even a minimal error frame (or any
/// request body). Never sends anything on such a configuration.
fn validate_limits(limits: &Limits) -> Result<(), IpcError> {
    if !limits.viable() {
        return Err(IpcError::new(
            ErrorCode::InvalidArguments,
            format!(
                "limits too small: need max_request_bytes >= 1 and \
                 max_response_bytes >= {} (minimal error frame)",
                super::minimal_response_bytes()
            ),
        ));
    }
    Ok(())
}

fn orphaned_error() -> IpcError {
    IpcError::new(
        ErrorCode::Io,
        "overlapped cancellation was not confirmed; pipe instance discarded",
    )
}

/// After the response is written, wait for the client to close so
/// DisconnectNamedPipe cannot discard unread response bytes.
fn wait_for_client_close(pipe: &Handle, wait: Duration) -> Result<(), TransportFailure> {
    let deadline = Instant::now() + wait;
    let mut scratch = [0u8; 256];
    loop {
        match read_exact(pipe, &mut scratch[..1], deadline) {
            Ok(()) => {}
            Err(TransportFailure::Orphaned) => return Err(TransportFailure::Orphaned),
            Err(TransportFailure::Error(_)) => return Ok(()),
        }
    }
}

fn serve_connection(
    pipe: &Handle,
    own: &UserSid,
    limits: &Limits,
    handler: &mut dyn Handler,
) -> Result<(), IpcError> {
    let mut connection = Connection::new(pipe);
    if verify_client(pipe, own).is_err() {
        return Ok(()); // nothing is read from or sent to an unauthenticated peer
    }
    let reply = match receive_frame(pipe, limits.max_request_bytes, limits.transport_wait) {
        Ok(request) => {
            let response = catch_unwind(AssertUnwindSafe(|| handler.handle(&request)))
                .unwrap_or_else(|_| {
                    error_response(None, IpcError::new(ErrorCode::Internal, "handler panicked"))
                });
            if response.is_empty() || response.len() > limits.max_response_bytes {
                error_frame_within(
                    ErrorCode::Internal,
                    "response outside frame limits",
                    limits.max_response_bytes,
                )
            } else {
                Some(response)
            }
        }
        Err(TransportFailure::Orphaned) => {
            connection.abandon();
            return Err(orphaned_error());
        }
        Err(TransportFailure::Error(error)) if error.code == ErrorCode::TransportClosed => {
            return Ok(())
        }
        Err(TransportFailure::Error(error)) => {
            error_frame_within(error.code, &error.message, limits.max_response_bytes)
        }
    };
    // `reply` is never larger than max_response_bytes: oversized bodies
    // were replaced by a minimal error, and `None` means even that could
    // not be sent, so the connection is closed without a frame.
    let Some(reply) = reply else { return Ok(()) };
    match send_frame(pipe, &reply, limits.max_response_bytes, limits.transport_wait) {
        Ok(()) => {}
        Err(TransportFailure::Orphaned) => {
            connection.abandon();
            return Err(orphaned_error());
        }
        Err(TransportFailure::Error(_)) => return Ok(()),
    }
    match wait_for_client_close(pipe, limits.transport_wait) {
        Ok(()) => Ok(()),
        Err(TransportFailure::Orphaned) => {
            connection.abandon();
            Err(orphaned_error())
        }
        Err(TransportFailure::Error(_)) => Ok(()),
    }
}

pub fn serve(
    endpoint: &str,
    limits: &Limits,
    handler: &mut dyn Handler,
    shutdown: &AtomicBool,
) -> Result<ServeExit, IpcError> {
    validate_limits(limits)?;
    let wide_name = validate_endpoint(endpoint)?;
    let own = own_user_sid()?;
    let sddl: Vec<u16> = format!("D:P(A;;GA;;;{})", sid_to_string(own.sid())?)
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut raw = PSECURITY_DESCRIPTOR::default();
    // SAFETY: sddl is NUL-terminated; the result is freed by SecurityDescriptor.
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &mut raw,
            None,
        )
    };
    converted
        .map_err(|e| IpcError::new(ErrorCode::Io, e.to_string()))?;
    let descriptor = SecurityDescriptor(raw);

    let pipe = create_pipe(&wide_name, &descriptor)?;
    let mut since = Instant::now();
    loop {
        if shutdown.load(Ordering::Acquire) {
            return Ok(ServeExit::ShutdownRequested);
        }
        if since.elapsed() >= limits.idle_exit {
            return Ok(ServeExit::IdleTimeout);
        }
        match accept(&pipe, shutdown, limits.idle_exit, since) {
            Accept::Exit(exit) => return Ok(exit),
            Accept::Orphaned => return Err(orphaned_error()),
            Accept::Failed => {
                // SAFETY: reset the instance; a failed accept never ends serving.
                let _ = unsafe { DisconnectNamedPipe(pipe.0) };
                std::thread::sleep(Duration::from_millis(u64::from(TICK_MS)));
            }
            Accept::Connected => {
                since = Instant::now();
                serve_connection(&pipe, &own, limits, handler)?;
                since = Instant::now();
            }
        }
    }
}

// ---------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------

fn connect_client(wide_name: &[u16], wait: Duration) -> Result<Handle, IpcError> {
    let deadline = Instant::now() + wait;
    loop {
        // SAFETY: wide_name is NUL-terminated. SQOS + IDENTIFICATION prevents
        // the server from impersonating us beyond identification.
        let opened = unsafe {
            CreateFileW(
                PCWSTR(wide_name.as_ptr()),
                FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                None,
            )
        };
        match opened {
            Ok(handle) => return Ok(Handle(handle)),
            Err(e) if is(&e, ERROR_ACCESS_DENIED) => {
                return Err(IpcError::new(
                    ErrorCode::UnauthenticatedPeer,
                    "pipe access denied",
                ));
            }
            Err(e) if is(&e, ERROR_PIPE_BUSY) || is(&e, ERROR_FILE_NOT_FOUND) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(IpcError::new(ErrorCode::Timeout, "no server within wait"));
                }
                let millis = remaining.as_millis().clamp(1, 100) as u32;
                // SAFETY: wide_name is NUL-terminated.
                let available = unsafe { WaitNamedPipeW(PCWSTR(wide_name.as_ptr()), millis) };
                if !available.as_bool() {
                    let pause = deadline
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(20));
                    if !pause.is_zero() {
                        std::thread::sleep(pause);
                    }
                }
            }
            Err(e) => return Err(map_win(&e)),
        }
    }
}

fn verify_server(pipe: &Handle) -> Result<(), IpcError> {
    let mut pid = 0u32;
    // SAFETY: valid client pipe handle.
    let queried = unsafe { GetNamedPipeServerProcessId(pipe.0, &mut pid) };
    queried
        .map_err(|e| IpcError::new(ErrorCode::UnauthenticatedPeer, e.to_string()))?;
    let own = own_user_sid()?;
    let server = process_user_sid(pid)?;
    if same_sid(&own, &server) {
        Ok(())
    } else {
        Err(IpcError::new(ErrorCode::UnauthenticatedPeer, "server user differs"))
    }
}

pub fn request(endpoint: &str, body: &[u8], limits: &Limits) -> Result<Vec<u8>, IpcError> {
    validate_limits(limits)?;
    let wide_name = validate_endpoint(endpoint)?;
    if body.is_empty() || body.len() > limits.max_request_bytes {
        return Err(IpcError::new(
            ErrorCode::OversizedFrame,
            format!(
                "request of {} bytes outside 1..={}",
                body.len(),
                limits.max_request_bytes
            ),
        ));
    }
    let pipe = connect_client(&wide_name, limits.transport_wait)?;
    verify_server(&pipe)?;
    match send_frame(&pipe, body, limits.max_request_bytes, limits.transport_wait) {
        Ok(()) => {}
        Err(TransportFailure::Orphaned) => return Err(orphaned_error()),
        // A peer can close after writing a typed response, so retain this
        // one fallback. Other write failures are terminal; waiting for a
        // second full transport interval would exceed request's useful bound.
        Err(TransportFailure::Error(error)) if error.code == ErrorCode::TransportClosed => {
            return receive_frame(&pipe, limits.max_response_bytes, limits.transport_wait)
                .map_err(|failure| match failure {
                    TransportFailure::Orphaned => orphaned_error(),
                    TransportFailure::Error(_) => error,
                });
        }
        Err(TransportFailure::Error(error)) => return Err(error),
    }
    receive_frame(&pipe, limits.max_response_bytes, limits.transport_wait).map_err(|failure| {
        match failure {
            TransportFailure::Orphaned => orphaned_error(),
            TransportFailure::Error(error) => error,
        }
    })
}
