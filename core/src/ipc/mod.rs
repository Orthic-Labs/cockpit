//! Settled, read-only local IPC contract shared by the worker and transports.
//!
//! * Opt-in and on demand: nothing listens unless a caller explicitly runs
//!   `cockpit worker serve`. Pills never start a listener.
//! * Local only: Unix-domain sockets on macOS, per-user named pipes on
//!   Windows. No TCP.
//! * Read-only: the only operations are `status`, `processes` and `scan`
//!   with explicit roots. Settings, cleanup, uninstall and termination are
//!   rejected as `unsupported_operation`.
//! * Transports authenticate the peer as the same user BEFORE reading any
//!   request byte, then exchange exactly one request frame and one response
//!   frame per connection.
//!
//! Framing: a 4-byte big-endian unsigned length followed by that many bytes
//! of UTF-8 JSON. A zero length or a length above the direction's limit is
//! `oversized_frame` / `malformed_request` and the connection is closed
//! without reading the body.

use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::time::Duration;

#[cfg(unix)]
pub mod unix;
#[cfg(windows)]
pub mod windows;

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
pub const TRANSPORT_WAIT: Duration = Duration::from_secs(30);
pub const IDLE_EXIT: Duration = Duration::from_secs(60);
/// Operation deadline for a child (`worker exec-op`); expiry triggers
/// termination plus a separately bounded confirmation attempt.
// Leave response-delivery time inside the client's 30-second exchange.
pub const OP_DEADLINE: Duration = Duration::from_secs(20);
/// Request IDs: 1..=64 bytes of `[A-Za-z0-9_-]`.
pub const MAX_REQUEST_ID_LEN: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
    /// Bound for connect, peer authentication, each frame read and write.
    pub transport_wait: Duration,
    /// Server exits after this long with no accepted connection.
    pub idle_exit: Duration,
    /// Hard deadline for one bounded operation in the `worker exec-op`
    /// child. Only meaningful for `Worker::bounded`; leaf workers run
    /// in-process and are bounded by their own parent's deadline.
    pub op_deadline: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_request_bytes: MAX_REQUEST_BYTES,
            max_response_bytes: MAX_RESPONSE_BYTES,
            transport_wait: TRANSPORT_WAIT,
            idle_exit: IDLE_EXIT,
            op_deadline: OP_DEADLINE,
        }
    }
}

impl Limits {
    /// True when the limits can carry every mandatory exchange: at least
    /// one request byte and the smallest possible serialized error
    /// response. Non-viable limits must be rejected at configuration time
    /// (`serve`/CLI startup) rather than overrun mid-connection; a
    /// `Handler` that still gets them returns an empty body, which
    /// transports must treat as "close without responding".
    pub fn viable(&self) -> bool {
        self.max_request_bytes >= 1 && self.max_response_bytes >= minimal_response_bytes()
    }
}

/// Required response budget for an empty-message error with any valid ID
/// & error code. Every accepted request must fit its correlated error.
pub fn minimal_response_bytes() -> usize {
    let codes = [
        ErrorCode::UnsupportedVersion,
        ErrorCode::UnsupportedOperation,
        ErrorCode::MalformedRequest,
        ErrorCode::InvalidArguments,
        ErrorCode::OversizedFrame,
        ErrorCode::ConflictingRequestId,
        ErrorCode::UnauthenticatedPeer,
        ErrorCode::EndpointUnsafe,
        ErrorCode::EndpointInUse,
        ErrorCode::Timeout,
        ErrorCode::TransportClosed,
        ErrorCode::Io,
        ErrorCode::Internal,
    ];
    codes
        .into_iter()
        .map(|code| {
            error_response(
                Some("x".repeat(MAX_REQUEST_ID_LEN)),
                IpcError::new(code, ""),
            )
            .len()
        })
        .max()
        .unwrap_or(0)
}

/// Request envelope. `op` stays a string so unknown operations produce a
/// typed `unsupported_operation` instead of a parse failure.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub version: u32,
    pub id: String,
    pub op: String,
    #[serde(default)]
    pub args: serde_json::Value,
}

/// `scan` arguments. Roots must be absolute; limits follow the CLI bounds
/// (depth <= 128, entries 1..=1_000_000).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanArgs {
    pub roots: Vec<std::path::PathBuf>,
    #[serde(default = "default_depth")]
    pub max_depth: usize,
    #[serde(default = "default_entries")]
    pub max_entries: usize,
}
fn default_depth() -> usize {
    64
}
fn default_entries() -> usize {
    100_000
}

/// `processes` arguments.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessArgs {
    #[serde(default)]
    pub grouped: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub version: u32,
    /// Echoed request ID; `None` only when the request ID was unreadable.
    pub id: Option<String>,
    #[serde(flatten)]
    pub outcome: Outcome,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    /// `truncated` is true whenever any list in `data` omits entries; the
    /// omitted count is reported inside `data`, never presented as complete.
    Ok {
        data: serde_json::Value,
        truncated: bool,
    },
    Error {
        error: IpcError,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    UnsupportedVersion,
    UnsupportedOperation,
    MalformedRequest,
    InvalidArguments,
    OversizedFrame,
    ConflictingRequestId,
    UnauthenticatedPeer,
    EndpointUnsafe,
    EndpointInUse,
    Timeout,
    TransportClosed,
    Io,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct IpcError {
    pub code: ErrorCode,
    pub message: String,
}

impl IpcError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    pub fn from_io(error: &std::io::Error) -> Self {
        let code = match error.kind() {
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => ErrorCode::Timeout,
            std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionReset => ErrorCode::TransportClosed,
            _ => ErrorCode::Io,
        };
        Self::new(code, error.to_string())
    }
}

/// Worker lifecycle events, emitted per request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub id: Option<String>,
    pub op: Option<String>,
    pub phase: Phase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorCode>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Started,
    Completed,
    Failed,
}

/// What a transport hands each authenticated request body to. Implemented
/// by `worker::Worker`. Must always return a serialized `Response` no
/// larger than `max_response_bytes`. An EMPTY return means the configured
/// limits cannot carry even a minimal error (`Limits::viable()` is false):
/// the transport must close the connection without writing.
///
/// `handle` is synchronous: a transport cannot promise forced cancellation
/// of an in-progress call. Time-bounded execution exists only when the
/// handler itself implements it (see `worker::Worker::bounded`, which runs
/// operations in a killable `worker exec-op` child).
pub trait Handler {
    fn handle(&mut self, request: &[u8]) -> Vec<u8>;
}

/// Why `serve` returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServeExit {
    IdleTimeout,
    ShutdownRequested,
}

pub fn write_frame(writer: &mut impl Write, body: &[u8], max: usize) -> Result<(), IpcError> {
    if body.is_empty() || body.len() > max {
        return Err(IpcError::new(
            ErrorCode::OversizedFrame,
            format!("frame of {} bytes outside 1..={max}", body.len()),
        ));
    }
    let length = u32::try_from(body.len())
        .map_err(|_| IpcError::new(ErrorCode::OversizedFrame, "frame exceeds u32"))?;
    writer
        .write_all(&length.to_be_bytes())
        .and_then(|()| writer.write_all(body))
        .and_then(|()| writer.flush())
        .map_err(|e| IpcError::from_io(&e))
}

/// Reads one frame, refusing the body when the declared length is zero or
/// above `max`. Never allocates more than `max` bytes.
pub fn read_frame(reader: &mut impl Read, max: usize) -> Result<Vec<u8>, IpcError> {
    let mut header = [0u8; 4];
    reader
        .read_exact(&mut header)
        .map_err(|e| IpcError::from_io(&e))?;
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 {
        return Err(IpcError::new(ErrorCode::MalformedRequest, "empty frame"));
    }
    if length > max {
        return Err(IpcError::new(
            ErrorCode::OversizedFrame,
            format!("declared frame of {length} bytes exceeds {max}"),
        ));
    }
    let mut body = vec![0u8; length];
    reader
        .read_exact(&mut body)
        .map_err(|e| IpcError::from_io(&e))?;
    Ok(body)
}

pub fn valid_request_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_REQUEST_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Serialize an error response; used by transports when a frame cannot be
/// handed to the handler (oversized, timeout before body, ...).
pub fn error_response(id: Option<String>, error: IpcError) -> Vec<u8> {
    serde_json::to_vec(&Response {
        version: PROTOCOL_VERSION,
        id,
        outcome: Outcome::Error { error },
    })
    .expect("response serializes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trip_and_bounds() {
        let mut buffer = Vec::new();
        write_frame(&mut buffer, b"{}", 16).unwrap();
        assert_eq!(read_frame(&mut buffer.as_slice(), 16).unwrap(), b"{}");
        assert_eq!(
            read_frame(&mut buffer.as_slice(), 1).unwrap_err().code,
            ErrorCode::OversizedFrame
        );
        assert_eq!(
            write_frame(&mut Vec::new(), b"", 16).unwrap_err().code,
            ErrorCode::OversizedFrame
        );
        let zero = 0u32.to_be_bytes();
        assert_eq!(
            read_frame(&mut zero.as_slice(), 16).unwrap_err().code,
            ErrorCode::MalformedRequest
        );
        let short = [0u8, 0, 0, 5, b'{'];
        assert_eq!(
            read_frame(&mut short.as_slice(), 16).unwrap_err().code,
            ErrorCode::TransportClosed
        );
    }

    #[test]
    fn request_ids_are_bounded_tokens() {
        assert!(valid_request_id("req-1_A"));
        assert!(!valid_request_id(""));
        assert!(!valid_request_id("../x"));
        assert!(!valid_request_id(&"a".repeat(65)));
    }
}
