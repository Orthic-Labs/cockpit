//! Unix-domain-socket transport for the settled IPC contract.
//!
//! Nothing here listens unless [`serve`] is explicitly called. No TCP.
//!
//! Endpoint safety: the endpoint's parent directory must exist (or is created
//! fresh with mode 0700), must not be a symlink, must be owned by the
//! effective user and must have no group/other permission bits. Violations
//! are `endpoint_unsafe`.
//!
//! Existing endpoint path: a socket that accepts a connection is
//! `endpoint_in_use`. Darwin also uses connection refusal for a full backlog,
//! so a refused existing socket is conservatively in use with unknown liveness.
//! Other existing entries are `endpoint_unsafe`; none is replaced or removed.
//! This module cannot prove it created a stale socket, so **a stale socket must be
//! removed by the user**. After a successful bind the socket's (dev, ino) is
//! recorded and the path is unlinked on exit only if it still is that same
//! socket.
//!
//! Every connection authenticates the peer uid (`getpeereid` on macOS/BSD,
//! `SO_PEERCRED` on Linux) before any request byte is read.

use super::{ErrorCode, Handler, IpcError, Limits, ServeExit, error_response};
use std::ffi::CString;
use std::fs::{File, Metadata};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Accept-loop poll interval (<= 100 ms).
const TICK: Duration = Duration::from_millis(50);

fn euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

fn unsafe_endpoint(message: impl Into<String>) -> IpcError {
    IpcError::new(ErrorCode::EndpointUnsafe, message)
}

fn pin_directory(path: &Path, create: bool) -> Result<File, IpcError> {
    let absolute = std::path::absolute(path).map_err(|e| IpcError::from_io(&e))?;
    let fd = unsafe {
        libc::open(
            c"/".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(IpcError::from_io(&io::Error::last_os_error()));
    }
    // SAFETY: each successful open transfers one fresh descriptor to File.
    let mut current = unsafe { File::from_raw_fd(fd) };
    for component in absolute.components() {
        let name = match component {
            std::path::Component::RootDir | std::path::Component::CurDir => continue,
            std::path::Component::Normal(name) => CString::new(name.as_bytes())
                .map_err(|_| unsafe_endpoint("directory component contains NUL"))?,
            _ => {
                return Err(unsafe_endpoint(
                    "endpoint directory contains unsupported traversal",
                ));
            }
        };
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        let mut next = unsafe { libc::openat(current.as_raw_fd(), name.as_ptr(), flags) };
        if next < 0 && create && io::Error::last_os_error().kind() == io::ErrorKind::NotFound {
            let result = unsafe { libc::mkdirat(current.as_raw_fd(), name.as_ptr(), 0o700) };
            if result != 0 && io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists {
                return Err(IpcError::from_io(&io::Error::last_os_error()));
            }
            next = unsafe { libc::openat(current.as_raw_fd(), name.as_ptr(), flags) };
        }
        if next < 0 {
            return Err(unsafe_endpoint(format!(
                "cannot pin endpoint directory: {}",
                io::Error::last_os_error()
            )));
        }
        current = unsafe { File::from_raw_fd(next) };
    }
    Ok(current)
}

/// Default private per-user endpoint. Creates the private directory only if
/// absent (mode 0700); never changes the mode of an existing directory.
pub fn default_endpoint() -> Result<PathBuf, IpcError> {
    let dir = default_dir()?;
    let endpoint = dir.join("worker.sock");
    check_parent(&endpoint)?;
    Ok(dir.join("worker.sock"))
}

#[cfg(target_os = "macos")]
fn default_dir() -> Result<PathBuf, IpcError> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| unsafe_endpoint("HOME is unset or not absolute"))?;
    Ok(home
        .join("Library")
        .join("Application Support")
        .join("Cockpit")
        .join("run"))
}

#[cfg(not(target_os = "macos"))]
fn default_dir() -> Result<PathBuf, IpcError> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| unsafe_endpoint("XDG_RUNTIME_DIR is unset or not absolute"))?;
    Ok(runtime.join("cockpit"))
}

/// Validates (creating fresh with 0700 if absent) the endpoint's parent.
fn check_parent(endpoint: &Path) -> Result<File, IpcError> {
    let parent = match endpoint.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    if endpoint.file_name().is_none() {
        return Err(unsafe_endpoint("endpoint has no file name"));
    }
    let pinned = pin_directory(parent, true)?;
    let meta = pinned.metadata().map_err(|e| IpcError::from_io(&e))?;
    if meta.file_type().is_symlink() {
        return Err(unsafe_endpoint("endpoint parent is a symlink"));
    }
    if !meta.is_dir() {
        return Err(unsafe_endpoint("endpoint parent is not a directory"));
    }
    if meta.uid() != euid() {
        return Err(unsafe_endpoint("endpoint parent is not owned by this user"));
    }
    if meta.mode() & 0o077 != 0 {
        return Err(unsafe_endpoint(
            "endpoint parent is accessible by group or others (mode must have no 077 bits)",
        ));
    }
    Ok(pinned)
}

fn revalidate_parent(endpoint: &Path, pinned: &File) -> Result<(), IpcError> {
    let current = pin_directory(endpoint.parent().unwrap_or(Path::new(".")), false)?;
    let before = pinned.metadata().map_err(|e| IpcError::from_io(&e))?;
    let after = current.metadata().map_err(|e| IpcError::from_io(&e))?;
    if identity(&before) != identity(&after) {
        return Err(unsafe_endpoint("endpoint parent identity changed"));
    }
    Ok(())
}

fn validate_limits(limits: &Limits) -> Result<(), IpcError> {
    if !limits.viable() {
        return Err(IpcError::new(
            ErrorCode::InvalidArguments,
            "IPC limits cannot carry a correlated error response",
        ));
    }
    Ok(())
}

fn check_existing(endpoint: &Path, wait: Duration) -> Result<(), IpcError> {
    match std::fs::symlink_metadata(endpoint) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(IpcError::from_io(&e)),
        Ok(meta) if meta.file_type().is_socket() => match connect_nb(endpoint, wait) {
            // Connected, or the connect could not finish because the live
            // listener's backlog is saturated / never drained: the endpoint
            // is in use either way, never "stale".
            Ok(ConnectOutcome::Connected(_))
            | Err(IpcError {
                code: ErrorCode::Timeout,
                ..
            }) => Err(IpcError::new(
                ErrorCode::EndpointInUse,
                "another listener is serving this endpoint",
            )),
            Ok(ConnectOutcome::Refused(e)) if matches!(e.raw_os_error(), Some(code) if code == libc::EAGAIN || code == libc::EWOULDBLOCK) => {
                Err(IpcError::new(
                    ErrorCode::EndpointInUse,
                    "another listener is serving this endpoint",
                ))
            }
            // Darwin reports ECONNREFUSED both for abandoned sockets & a
            // live listener whose queue is full. Refusal cannot prove stale.
            Ok(ConnectOutcome::Refused(e))
                if cfg!(target_vendor = "apple")
                    && e.raw_os_error() == Some(libc::ECONNREFUSED) =>
            {
                Err(IpcError::new(
                    ErrorCode::EndpointInUse,
                    "existing socket refused connection; listener liveness is unknown, endpoint preserved",
                ))
            }
            Ok(ConnectOutcome::Refused(_)) => Err(unsafe_endpoint(
                "a socket exists at the endpoint that is not accepting connections; \
                 it is not replaced automatically, remove a stale socket manually",
            )),
            Err(e) => Err(e),
        },
        Ok(_) => Err(unsafe_endpoint(
            "a non-socket entry exists at the endpoint and is left untouched",
        )),
    }
}

fn identity(meta: &Metadata) -> (u64, u64) {
    (meta.dev(), meta.ino())
}

/// Unlinks the endpoint on drop only if it is still the socket we bound.
struct BoundSocket {
    directory: File,
    name: CString,
    id: (u64, u64),
}

impl Drop for BoundSocket {
    fn drop(&mut self) {
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe {
            libc::fstatat(
                self.directory.as_raw_fd(),
                self.name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0
        {
            let stat = unsafe { stat.assume_init() };
            if stat.st_mode & libc::S_IFMT == libc::S_IFSOCK
                && (stat.st_dev as u64, stat.st_ino) == self.id
            {
                let _ =
                    unsafe { libc::unlinkat(self.directory.as_raw_fd(), self.name.as_ptr(), 0) };
            }
        }
    }
}

/// Serves sequential single-request connections until idle or shutdown.
pub fn serve(
    endpoint: &Path,
    limits: &Limits,
    handler: &mut dyn Handler,
    shutdown: &AtomicBool,
) -> Result<ServeExit, IpcError> {
    validate_limits(limits)?;
    socket_addr(endpoint)?;
    let parent = check_parent(endpoint)?;
    check_existing(endpoint, timeout(limits))?;
    revalidate_parent(endpoint, &parent)?;
    let listener = UnixListener::bind(endpoint).map_err(|e| match e.kind() {
        io::ErrorKind::AddrInUse => IpcError::new(ErrorCode::EndpointInUse, e.to_string()),
        _ => IpcError::from_io(&e),
    })?;
    let meta = std::fs::symlink_metadata(endpoint).map_err(|e| IpcError::from_io(&e))?;
    let _guard = BoundSocket {
        directory: parent,
        name: CString::new(endpoint.file_name().unwrap().as_bytes())
            .map_err(|_| unsafe_endpoint("endpoint name contains NUL"))?,
        id: identity(&meta),
    };
    revalidate_parent(endpoint, &_guard.directory)?;
    listener
        .set_nonblocking(true)
        .map_err(|e| IpcError::from_io(&e))?;

    let mut last_activity = Instant::now();
    loop {
        if shutdown.load(Ordering::SeqCst) {
            return Ok(ServeExit::ShutdownRequested);
        }
        if last_activity.elapsed() >= limits.idle_exit {
            return Ok(ServeExit::IdleTimeout);
        }
        match listener.accept() {
            Ok((stream, _)) => {
                handle_connection(stream, limits, handler);
                last_activity = Instant::now();
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            // WouldBlock or a transient accept failure: never fatal.
            Err(_) => std::thread::sleep(TICK),
        }
    }
}

fn timeout(limits: &Limits) -> Duration {
    limits.transport_wait.max(Duration::from_millis(1))
}

/// Retain the typed OS error while identifying the failing transport step.
fn io_at(step: &str, error: &io::Error) -> IpcError {
    let mut mapped = IpcError::from_io(error);
    mapped.message = format!("{step}: {}", mapped.message);
    mapped
}

/// Wall-clock remaining until `deadline`; a trickling peer cannot extend it.
fn remaining(deadline: Instant) -> Result<Duration, IpcError> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| IpcError::new(ErrorCode::Timeout, "transport deadline exceeded"))
}

/// Wait for I/O readiness against one absolute deadline. Hangup is also
/// readiness: a read must consume buffered bytes before reporting EOF.
fn wait_ready(
    stream: &UnixStream,
    events: libc::c_short,
    deadline: Instant,
) -> Result<(), IpcError> {
    loop {
        let left = remaining(deadline)?;
        let mut pfd = libc::pollfd {
            fd: stream.as_raw_fd(),
            events,
            revents: 0,
        };
        let ms = left.as_millis().clamp(1, libc::c_int::MAX as u128) as libc::c_int;
        // SAFETY: pfd is a valid single element; stream owns its live fd.
        let result = unsafe { libc::poll(&mut pfd, 1, ms) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(io_at("frame poll", &error));
        }
        if result == 0 {
            continue;
        }
        if pfd.revents & libc::POLLNVAL != 0 {
            return Err(IpcError::new(
                ErrorCode::TransportClosed,
                "invalid frame socket",
            ));
        }
        if pfd.revents & (events | libc::POLLHUP | libc::POLLERR) != 0 {
            return Ok(());
        }
    }
}

/// Nonblocking reads share one deadline, including every partial read & wait.
/// No setsockopt is performed after peer shutdown; Darwin rejects that even
/// when a complete response remains buffered for reading.
fn read_exact_deadline(
    stream: &mut UnixStream,
    buf: &mut [u8],
    deadline: Instant,
) -> Result<(), IpcError> {
    let mut filled = 0;
    while filled < buf.len() {
        remaining(deadline)?;
        match stream.read(&mut buf[filled..]) {
            Ok(0) => {
                return Err(IpcError::new(
                    ErrorCode::TransportClosed,
                    "peer closed connection mid-frame",
                ));
            }
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                wait_ready(stream, libc::POLLIN, deadline)?;
            }
            Err(e) => return Err(io_at("read frame", &e)),
        }
    }
    Ok(())
}

fn write_all_deadline(
    stream: &mut UnixStream,
    buf: &[u8],
    deadline: Instant,
) -> Result<(), IpcError> {
    let mut written = 0;
    while written < buf.len() {
        remaining(deadline)?;
        match stream.write(&buf[written..]) {
            Ok(0) => {
                return Err(IpcError::new(
                    ErrorCode::TransportClosed,
                    "peer closed connection mid-frame",
                ));
            }
            Ok(n) => written += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                wait_ready(stream, libc::POLLOUT, deadline)?;
            }
            Err(e) => return Err(io_at("write frame", &e)),
        }
    }
    stream.flush().map_err(|e| io_at("flush frame", &e))
}

/// `read_frame` under an absolute deadline (see `read_exact_deadline`).
fn read_frame_deadline(
    stream: &mut UnixStream,
    max: usize,
    deadline: Instant,
) -> Result<Vec<u8>, IpcError> {
    let mut header = [0u8; 4];
    read_exact_deadline(stream, &mut header, deadline)?;
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
    read_exact_deadline(stream, &mut body, deadline)?;
    Ok(body)
}

/// `write_frame` under an absolute deadline.
fn write_frame_deadline(
    stream: &mut UnixStream,
    body: &[u8],
    max: usize,
    deadline: Instant,
) -> Result<(), IpcError> {
    if body.is_empty() || body.len() > max {
        return Err(IpcError::new(
            ErrorCode::OversizedFrame,
            format!("frame of {} bytes outside 1..={max}", body.len()),
        ));
    }
    let length = u32::try_from(body.len())
        .map_err(|_| IpcError::new(ErrorCode::OversizedFrame, "frame exceeds u32"))?;
    write_all_deadline(stream, &length.to_be_bytes(), deadline)?;
    write_all_deadline(stream, body, deadline)
}

fn send_error(stream: &mut UnixStream, limits: &Limits, error: IpcError) {
    let deadline = Instant::now() + timeout(limits);
    let _ = write_frame_deadline(
        stream,
        &error_response(None, error),
        limits.max_response_bytes,
        deadline,
    );
}

fn handle_connection(mut stream: UnixStream, limits: &Limits, handler: &mut dyn Handler) {
    // Deadline helpers require nonblocking I/O on every platform.
    if stream.set_nonblocking(true).is_err() {
        return;
    }
    // One absolute deadline bounds all partial reads & readiness waits.
    let read_deadline = Instant::now() + timeout(limits);
    // Authenticate BEFORE reading any request byte.
    match peer_uid(&stream) {
        Ok(uid) if uid == euid() => {}
        Ok(_) => {
            send_error(
                &mut stream,
                limits,
                IpcError::new(ErrorCode::UnauthenticatedPeer, "peer is a different user"),
            );
            return;
        }
        Err(e) => {
            send_error(&mut stream, limits, e);
            return;
        }
    }
    let request = match read_frame_deadline(&mut stream, limits.max_request_bytes, read_deadline) {
        Ok(body) => body,
        Err(e) => {
            send_error(&mut stream, limits, e);
            return;
        }
    };
    let response = match catch_unwind(AssertUnwindSafe(|| handler.handle(&request))) {
        Ok(body) => body,
        Err(_) => error_response(None, IpcError::new(ErrorCode::Internal, "handler panicked")),
    };
    let write_deadline = Instant::now() + timeout(limits);
    if let Err(e) = write_frame_deadline(
        &mut stream,
        &response,
        limits.max_response_bytes,
        write_deadline,
    ) && e.code == ErrorCode::OversizedFrame
    {
        send_error(
            &mut stream,
            limits,
            IpcError::new(ErrorCode::Internal, "response violates frame limits"),
        );
    }
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

/// Sends one request frame and returns the response frame body.
pub fn request(endpoint: &Path, body: &[u8], limits: &Limits) -> Result<Vec<u8>, IpcError> {
    validate_limits(limits)?;
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
    let wait = timeout(limits);
    let mut stream = connect_bounded(endpoint, wait)?;
    // The whole write-then-read exchange shares one absolute deadline.
    let deadline = Instant::now() + wait;
    // Authenticate the server before writing anything.
    if peer_uid(&stream)? != euid() {
        return Err(IpcError::new(
            ErrorCode::UnauthenticatedPeer,
            "server is a different user",
        ));
    }
    write_frame_deadline(&mut stream, body, limits.max_request_bytes, deadline)?;
    read_frame_deadline(&mut stream, limits.max_response_bytes, deadline)
}

/// `UnixStream::connect` has no timeout; bound it with a nonblocking
/// connect polled to a deadline. No helper thread is spawned: a timed-out
/// connect cannot leak a detached blocking thread.
fn connect_bounded(endpoint: &Path, wait: Duration) -> Result<UnixStream, IpcError> {
    match connect_nb(endpoint, wait)? {
        ConnectOutcome::Connected(stream) => Ok(stream),
        ConnectOutcome::Refused(e) => Err(io_at("connect", &e)),
    }
}

enum ConnectOutcome {
    Connected(UnixStream),
    /// `connect`/post-poll `SO_ERROR` failed with this OS error.
    Refused(io::Error),
}

/// Owns a raw fd and closes it on every exit path unless `release`d.
struct OwnedFd(RawFd);

impl OwnedFd {
    fn release(self) -> RawFd {
        let fd = self.0;
        std::mem::forget(self);
        fd
    }
}

impl Drop for OwnedFd {
    fn drop(&mut self) {
        // SAFETY: `fd` is owned by this guard and closed exactly once.
        unsafe { libc::close(self.0) };
    }
}

/// Builds a `sockaddr_un` for `endpoint`, refusing paths that cannot fit
/// `sun_path` (with its NUL terminator) instead of truncating them.
fn socket_addr(endpoint: &Path) -> Result<(libc::sockaddr_un, libc::socklen_t), IpcError> {
    let bytes = endpoint.as_os_str().as_bytes();
    // SAFETY: all-zero is a valid sockaddr_un; fields are set below.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let capacity = addr.sun_path.len();
    if bytes.is_empty() || bytes.contains(&0) || bytes.len() >= capacity {
        return Err(unsafe_endpoint(format!(
            "endpoint path is empty, interior-NUL or longer than sun_path ({capacity} bytes)"
        )));
    }
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let len =
        (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1) as libc::socklen_t;
    #[cfg(any(
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly"
    ))]
    {
        addr.sun_len = len as u8;
    }
    // SAFETY: `bytes` is shorter than `sun_path` (checked above); the
    // remaining bytes stay NUL from `zeroed`.
    unsafe {
        std::ptr::copy_nonoverlapping(
            bytes.as_ptr() as *const libc::c_char,
            addr.sun_path.as_mut_ptr(),
            bytes.len(),
        );
    }
    Ok((addr, len))
}

/// Nonblocking AF_UNIX connect bounded by `wait`: issues `connect`, then
/// polls the fd for writability until the deadline and reports `SO_ERROR`.
/// The fd is closed on every error path via [`OwnedFd`].
fn connect_nb(endpoint: &Path, wait: Duration) -> Result<ConnectOutcome, IpcError> {
    let (addr, addr_len) = socket_addr(endpoint)?;
    // SAFETY: plain socket creation; failure returns -1 with errno set.
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if raw < 0 {
        return Err(io_at("socket", &io::Error::last_os_error()));
    }
    let fd = OwnedFd(raw);
    let deadline = Instant::now() + wait;
    // SAFETY: `fd.0` is a live fd owned by the guard.
    unsafe {
        let flags = libc::fcntl(fd.0, libc::F_GETFD);
        if flags < 0 || libc::fcntl(fd.0, libc::F_SETFD, flags | libc::FD_CLOEXEC) < 0 {
            return Err(io_at("close-on-exec flags", &io::Error::last_os_error()));
        }
        let flags = libc::fcntl(fd.0, libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd.0, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(io_at("nonblocking flags", &io::Error::last_os_error()));
        }
        if libc::connect(
            fd.0,
            &addr as *const libc::sockaddr_un as *const libc::sockaddr,
            addr_len,
        ) != 0
        {
            let err = io::Error::last_os_error();
            match err.raw_os_error() {
                // EINPROGRESS/EINTR/EAGAIN mean the connect is pending
                // (EAGAIN also covers a saturated listen backlog on BSD).
                Some(code) if connect_pending(code) => {}
                _ => return Ok(ConnectOutcome::Refused(err)),
            }
            loop {
                let left = remaining(deadline)?;
                let mut pfd = libc::pollfd {
                    fd: fd.0,
                    events: libc::POLLOUT,
                    revents: 0,
                };
                let ms = left.as_millis().clamp(1, libc::c_int::MAX as u128) as libc::c_int;
                let prc = libc::poll(&mut pfd, 1, ms);
                if prc < 0 {
                    let e = io::Error::last_os_error();
                    if e.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(io_at("connect poll", &e));
                }
                if prc == 0 {
                    continue; // `remaining` maps the expired deadline to Timeout.
                }
                let mut so_error: libc::c_int = 0;
                let mut opt_len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
                if libc::getsockopt(
                    fd.0,
                    libc::SOL_SOCKET,
                    libc::SO_ERROR,
                    &mut so_error as *mut libc::c_int as *mut libc::c_void,
                    &mut opt_len,
                ) != 0
                {
                    return Err(io_at("connect SO_ERROR", &io::Error::last_os_error()));
                }
                if so_error != 0 {
                    if connect_pending(so_error) {
                        let pause = remaining(deadline)?.min(TICK);
                        if !pause.is_zero() {
                            std::thread::sleep(pause);
                        }
                        continue;
                    }
                    return Ok(ConnectOutcome::Refused(io::Error::from_raw_os_error(
                        so_error,
                    )));
                }
                // SO_ERROR == 0 is not sufficient on every Unix: Linux can
                // report a saturated backlog with a clear SO_ERROR while the
                // socket remains unconnected. Verify the peer first; only
                // ENOTCONN permits a retry. Reconnecting a completed Darwin
                // socket can return EINVAL and must never be attempted.
                let mut peer: libc::sockaddr_un = std::mem::zeroed();
                let mut peer_len = std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;
                if libc::getpeername(
                    fd.0,
                    &mut peer as *mut libc::sockaddr_un as *mut libc::sockaddr,
                    &mut peer_len,
                ) == 0
                {
                    break;
                }
                let peer_error = io::Error::last_os_error();
                if peer_error.raw_os_error() != Some(libc::ENOTCONN) {
                    return Ok(ConnectOutcome::Refused(peer_error));
                }
                if libc::connect(
                    fd.0,
                    &addr as *const libc::sockaddr_un as *const libc::sockaddr,
                    addr_len,
                ) == 0
                {
                    break;
                }
                let err = io::Error::last_os_error();
                match err.raw_os_error() {
                    Some(code) if code == libc::EISCONN => break,
                    Some(code) if connect_pending(code) => {
                        let pause = remaining(deadline)?.min(TICK);
                        if !pause.is_zero() {
                            std::thread::sleep(pause);
                        }
                        continue;
                    }
                    _ => return Ok(ConnectOutcome::Refused(err)),
                }
            }
        }
        // Keep O_NONBLOCK: frame helpers wait with poll against a deadline.
    }
    // SAFETY: `fd.0` is a connected SOCK_STREAM AF_UNIX socket owned solely
    // by this guard; ownership moves into the UnixStream exactly once.
    Ok(ConnectOutcome::Connected(unsafe {
        UnixStream::from_raw_fd(fd.release())
    }))
}

fn connect_pending(code: i32) -> bool {
    code == libc::EINPROGRESS
        || code == libc::EINTR
        || code == libc::EAGAIN
        || code == libc::EWOULDBLOCK
        || code == libc::EALREADY
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
fn peer_uid(stream: &UnixStream) -> Result<u32, IpcError> {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: valid fd borrowed from `stream`; out-pointers are valid locals.
    let rc = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    if rc != 0 {
        return Err(IpcError::new(
            ErrorCode::UnauthenticatedPeer,
            format!("getpeereid failed: {}", io::Error::last_os_error()),
        ));
    }
    Ok(uid)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn peer_uid(stream: &UnixStream) -> Result<u32, IpcError> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: valid fd; `cred` and `len` are valid for the stated size.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut libc::ucred as *mut libc::c_void,
            &mut len,
        )
    };
    if rc != 0 || (len as usize) < std::mem::size_of::<libc::ucred>() {
        return Err(IpcError::new(
            ErrorCode::UnauthenticatedPeer,
            format!("SO_PEERCRED failed: {}", io::Error::last_os_error()),
        ));
    }
    Ok(cred.uid)
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "linux",
    target_os = "android"
)))]
fn peer_uid(_stream: &UnixStream) -> Result<u32, IpcError> {
    Err(IpcError::new(
        ErrorCode::UnauthenticatedPeer,
        "peer credentials are unsupported on this platform",
    ))
}

#[cfg(test)]
mod deadline_tests {
    use super::*;

    #[test]
    fn buffered_response_survives_peer_shutdown() {
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        sender.set_nonblocking(true).unwrap();
        receiver.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        write_frame_deadline(&mut sender, b"complete response", 1024, deadline).unwrap();
        sender.shutdown(std::net::Shutdown::Both).unwrap();
        drop(sender);
        assert_eq!(
            read_frame_deadline(&mut receiver, 1024, deadline).unwrap(),
            b"complete response"
        );
    }

    #[test]
    fn peer_shutdown_mid_body_is_transport_closed() {
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        sender.set_nonblocking(true).unwrap();
        receiver.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        write_all_deadline(&mut sender, &8u32.to_be_bytes(), deadline).unwrap();
        write_all_deadline(&mut sender, b"part", deadline).unwrap();
        sender.shutdown(std::net::Shutdown::Both).unwrap();
        drop(sender);
        assert_eq!(
            read_frame_deadline(&mut receiver, 1024, deadline)
                .unwrap_err()
                .code,
            ErrorCode::TransportClosed
        );
    }
}
