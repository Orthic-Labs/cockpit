#![cfg(unix)]

use cockpit_core::ipc::unix::{request, serve};
use cockpit_core::ipc::{
    read_frame, write_frame, ErrorCode, Handler, IpcError, Limits, Outcome, Response, ServeExit,
};
use std::fs::{self, DirBuilder};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct Echo;
impl Handler for Echo {
    fn handle(&mut self, request: &[u8]) -> Vec<u8> {
        let mut out = b"echo:".to_vec();
        out.extend_from_slice(request);
        out
    }
}

/// Isolated 0700 directory under canonicalize(temp_dir).
fn private_dir(name: &str) -> PathBuf {
    let base = fs::canonicalize(std::env::temp_dir()).unwrap();
    let dir = base.join(format!(
        "cockpit-ipc-{}-{}-{name}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    DirBuilder::new().mode(0o700).create(&dir).unwrap();
    dir
}

fn limits(idle_ms: u64) -> Limits {
    Limits {
        max_request_bytes: 1024,
        max_response_bytes: 4096,
        transport_wait: Duration::from_secs(5),
        idle_exit: Duration::from_millis(idle_ms),
        ..Limits::default()
    }
}

struct Server {
    shutdown: Arc<AtomicBool>,
    thread: JoinHandle<Result<ServeExit, IpcError>>,
}

fn start(endpoint: &Path, limits: Limits) -> Server {
    let shutdown = Arc::new(AtomicBool::new(false));
    let flag = shutdown.clone();
    let path = endpoint.to_path_buf();
    let thread = std::thread::spawn(move || serve(&path, &limits, &mut Echo, &flag));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !endpoint.exists() {
        assert!(Instant::now() < deadline, "server did not bind");
        std::thread::sleep(Duration::from_millis(10));
    }
    Server { shutdown, thread }
}

impl Server {
    fn stop(self) -> Result<ServeExit, IpcError> {
        self.shutdown.store(true, Ordering::SeqCst);
        self.thread.join().unwrap()
    }
}

fn error_code(frame: &[u8]) -> ErrorCode {
    let response: Response = serde_json::from_slice(frame).unwrap();
    match response.outcome {
        Outcome::Error { error } => error.code,
        other => panic!("expected error, got {other:?}"),
    }
}

#[test]
fn round_trip_and_cleanup() {
    let dir = private_dir("roundtrip");
    let endpoint = dir.join("worker.sock");
    let l = limits(60_000);
    let server = start(&endpoint, l);
    assert_eq!(request(&endpoint, b"one", &l).unwrap(), b"echo:one");
    assert_eq!(request(&endpoint, b"two", &l).unwrap(), b"echo:two");
    assert_eq!(server.stop().unwrap(), ServeExit::ShutdownRequested);
    assert!(!endpoint.exists(), "owned socket is unlinked on exit");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn invalid_limits_are_refused_before_creating_endpoint() {
    let dir = private_dir("invalid-limits");
    let endpoint = dir.join("missing/worker.sock");
    let l = Limits { max_response_bytes: 1, ..limits(60_000) };
    let shutdown = AtomicBool::new(false);
    assert_eq!(serve(&endpoint, &l, &mut Echo, &shutdown).unwrap_err().code, ErrorCode::InvalidArguments);
    assert_eq!(request(&endpoint, b"hello", &l).unwrap_err().code, ErrorCode::InvalidArguments);
    assert!(!dir.join("missing").exists());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn symlinked_ancestor_cannot_create_endpoint_parent() {
    let dir = private_dir("ancestor-link");
    let target = dir.join("target");
    fs::create_dir(&target).unwrap();
    std::os::unix::fs::symlink(&target, dir.join("alias")).unwrap();
    let endpoint = dir.join("alias/missing/worker.sock");
    let shutdown = AtomicBool::new(false);
    assert_eq!(serve(&endpoint, &limits(60_000), &mut Echo, &shutdown).unwrap_err().code, ErrorCode::EndpointUnsafe);
    assert!(!target.join("missing").exists());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn cleanup_uses_original_parent_after_rename() {
    let dir = private_dir("parent-rename");
    let endpoint = dir.join("worker.sock");
    let server = start(&endpoint, limits(60_000));
    let moved = dir.with_extension("moved");
    fs::rename(&dir, &moved).unwrap();
    DirBuilder::new().mode(0o700).create(&dir).unwrap();
    fs::write(&endpoint, b"replacement preserved").unwrap();
    server.stop().unwrap();
    assert!(!moved.join("worker.sock").exists());
    assert_eq!(fs::read(&endpoint).unwrap(), b"replacement preserved");
    fs::remove_dir_all(dir).unwrap();
    fs::remove_dir_all(moved).unwrap();
}

#[test]
fn oversized_request_is_rejected() {
    let dir = private_dir("oversized");
    let endpoint = dir.join("worker.sock");
    let l = limits(60_000);
    let server = start(&endpoint, l);

    // Client refuses locally.
    let big = vec![b'x'; l.max_request_bytes + 1];
    assert_eq!(
        request(&endpoint, &big, &l).unwrap_err().code,
        ErrorCode::OversizedFrame
    );

    // Server refuses a declared oversized length without reading a body.
    let mut raw = UnixStream::connect(&endpoint).unwrap();
    raw.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let declared = (l.max_request_bytes as u32 + 1).to_be_bytes();
    std::io::Write::write_all(&mut raw, &declared).unwrap();
    let frame = read_frame(&mut raw, 4096).unwrap();
    assert_eq!(error_code(&frame), ErrorCode::OversizedFrame);
    drop(raw);

    // The server survives and keeps serving.
    assert_eq!(request(&endpoint, b"ok", &l).unwrap(), b"echo:ok");
    server.stop().unwrap();
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn malformed_connection_does_not_kill_server() {
    let dir = private_dir("malformed");
    let endpoint = dir.join("worker.sock");
    let l = limits(60_000);
    let server = start(&endpoint, l);
    let mut raw = UnixStream::connect(&endpoint).unwrap();
    write_frame(&mut raw, b"hi", 16).unwrap();
    drop(raw);
    drop(UnixStream::connect(&endpoint).unwrap()); // connect and hang up
    assert_eq!(request(&endpoint, b"ok", &l).unwrap(), b"echo:ok");
    server.stop().unwrap();
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn slowly_arriving_request_cannot_extend_frame_deadline() {
    use std::io::Write;

    let dir = private_dir("slow-frame");
    let endpoint = dir.join("worker.sock");
    let mut l = limits(60_000);
    l.transport_wait = Duration::from_millis(400);
    let server = start(&endpoint, l);
    let mut raw = UnixStream::connect(&endpoint).unwrap();
    raw.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let started = Instant::now();
    raw.write_all(&32u32.to_be_bytes()).unwrap();
    let mut sender = raw.try_clone().unwrap();
    let trickle = std::thread::spawn(move || {
        for _ in 0..32 {
            if sender.write_all(b"x").is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    let frame = read_frame(&mut raw, 4096).unwrap();
    let elapsed = started.elapsed();
    let response = serde_json::from_slice::<Response>(&frame);
    drop(raw);
    trickle.join().unwrap();
    server.stop().unwrap();
    fs::remove_dir_all(&dir).unwrap();
    assert!(matches!(
        response.unwrap().outcome,
        Outcome::Error { error } if error.code == ErrorCode::Timeout
    ));
    assert!(elapsed < Duration::from_secs(2), "deadline took {elapsed:?}");
}

#[test]
fn symlinked_parent_is_refused() {
    let dir = private_dir("symlink");
    let real = dir.join("real");
    DirBuilder::new().mode(0o700).create(&real).unwrap();
    let link = dir.join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let shutdown = AtomicBool::new(true);
    let err = serve(&link.join("worker.sock"), &limits(100), &mut Echo, &shutdown).unwrap_err();
    assert_eq!(err.code, ErrorCode::EndpointUnsafe);
    assert!(!real.join("worker.sock").exists());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn world_writable_parent_is_refused() {
    let dir = private_dir("open");
    let open = dir.join("open");
    DirBuilder::new().mode(0o700).create(&open).unwrap();
    fs::set_permissions(&open, fs::Permissions::from_mode(0o770)).unwrap();
    let shutdown = AtomicBool::new(true);
    let err = serve(&open.join("worker.sock"), &limits(100), &mut Echo, &shutdown).unwrap_err();
    assert_eq!(err.code, ErrorCode::EndpointUnsafe);
    fs::set_permissions(&open, fs::Permissions::from_mode(0o777)).unwrap();
    let err = serve(&open.join("worker.sock"), &limits(100), &mut Echo, &shutdown).unwrap_err();
    assert_eq!(err.code, ErrorCode::EndpointUnsafe);
    assert!(!open.join("worker.sock").exists());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn missing_parent_is_created_private() {
    let dir = private_dir("fresh");
    let child = dir.join("run");
    let endpoint = child.join("worker.sock");
    let l = limits(60_000);
    let server = start(&endpoint, l);
    let mode = fs::metadata(&child).unwrap().permissions().mode();
    assert_eq!(mode & 0o077, 0);
    server.stop().unwrap();
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn live_endpoint_is_in_use() {
    let dir = private_dir("inuse");
    let endpoint = dir.join("worker.sock");
    let l = limits(60_000);
    let server = start(&endpoint, l);
    let shutdown = AtomicBool::new(true);
    let err = serve(&endpoint, &l, &mut Echo, &shutdown).unwrap_err();
    assert_eq!(err.code, ErrorCode::EndpointInUse);
    // The live server is unaffected and keeps its socket.
    assert_eq!(request(&endpoint, b"ok", &l).unwrap(), b"echo:ok");
    server.stop().unwrap();
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn unrelated_files_and_stale_sockets_are_not_removed() {
    let dir = private_dir("unrelated");
    let endpoint = dir.join("worker.sock");
    fs::write(&endpoint, b"precious").unwrap();
    let shutdown = AtomicBool::new(true);
    let err = serve(&endpoint, &limits(100), &mut Echo, &shutdown).unwrap_err();
    assert_eq!(err.code, ErrorCode::EndpointUnsafe);
    assert_eq!(fs::read(&endpoint).unwrap(), b"precious");
    fs::remove_file(&endpoint).unwrap();

    // A stale socket (bound, then abandoned) is refused and left for the user.
    drop(std::os::unix::net::UnixListener::bind(&endpoint).unwrap());
    let err = serve(&endpoint, &limits(100), &mut Echo, &shutdown).unwrap_err();
    assert_eq!(err.code, ErrorCode::EndpointUnsafe);
    assert!(fs::symlink_metadata(&endpoint).is_ok());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn idle_exit() {
    let dir = private_dir("idle");
    let endpoint = dir.join("worker.sock");
    let shutdown = AtomicBool::new(false);
    let started = Instant::now();
    let exit = serve(&endpoint, &limits(300), &mut Echo, &shutdown).unwrap();
    assert_eq!(exit, ServeExit::IdleTimeout);
    assert!(started.elapsed() >= Duration::from_millis(300));
    assert!(!endpoint.exists());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn shutdown_flag_stops_server() {
    let dir = private_dir("shutdown");
    let endpoint = dir.join("worker.sock");
    let server = start(&endpoint, limits(60_000));
    let started = Instant::now();
    assert_eq!(server.stop().unwrap(), ServeExit::ShutdownRequested);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(!endpoint.exists());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn socket_is_unlinked_only_when_owned() {
    let dir = private_dir("owned");
    let endpoint = dir.join("worker.sock");
    let server = start(&endpoint, limits(60_000));
    // Something else replaces the path while the server runs.
    fs::remove_file(&endpoint).unwrap();
    fs::write(&endpoint, b"someone else's").unwrap();
    assert_eq!(server.stop().unwrap(), ServeExit::ShutdownRequested);
    assert_eq!(fs::read(&endpoint).unwrap(), b"someone else's");
    fs::remove_file(&endpoint).unwrap();

    // Same for a different socket (different inode) at the same path.
    let server = start(&endpoint, limits(60_000));
    fs::remove_file(&endpoint).unwrap();
    let other = std::os::unix::net::UnixListener::bind(&endpoint).unwrap();
    server.stop().unwrap();
    assert!(fs::symlink_metadata(&endpoint).is_ok());
    drop(other);
    fs::remove_dir_all(&dir).unwrap();
}

/// Binds a raw AF_UNIX listener with the given listen(2) backlog. Used to
/// saturate a listener's pending queue, which std's fixed backlog cannot.
fn raw_listener(path: &Path, backlog: i32) -> RawFd {
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: all-zero sockaddr_un, fields set below; path fits sun_path.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    assert!(bytes.len() < addr.sun_path.len(), "test path too long");
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    unsafe {
        std::ptr::copy_nonoverlapping(
            bytes.as_ptr() as *const libc::c_char,
            addr.sun_path.as_mut_ptr(),
            bytes.len(),
        );
    }
    let len = (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1)
        as libc::socklen_t;
    // SAFETY: plain syscalls; `addr` is a valid sockaddr_un for `len` bytes.
    unsafe {
        let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
        assert!(fd >= 0, "socket: {}", std::io::Error::last_os_error());
        let rc = libc::bind(
            fd,
            &addr as *const libc::sockaddr_un as *const libc::sockaddr,
            len,
        );
        assert_eq!(rc, 0, "bind: {}", std::io::Error::last_os_error());
        assert_eq!(libc::listen(fd, backlog), 0);
        fd
    }
}

/// Nonblocking connect; returns the fd while pending/connected, `None`
/// when refused (e.g. backlog full). Blocking connects would hang the test
/// against a saturated listener on both Linux and macOS.
fn raw_connect_pending(path: &Path) -> Option<RawFd> {
    let bytes = path.as_os_str().as_bytes();
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    unsafe {
        std::ptr::copy_nonoverlapping(
            bytes.as_ptr() as *const libc::c_char,
            addr.sun_path.as_mut_ptr(),
            bytes.len(),
        );
    }
    let len = (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1)
        as libc::socklen_t;
    unsafe {
        let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
        assert!(fd >= 0);
        let flags = libc::fcntl(fd, libc::F_GETFL);
        assert!(flags >= 0 && libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) >= 0);
        let rc = libc::connect(
            fd,
            &addr as *const libc::sockaddr_un as *const libc::sockaddr,
            len,
        );
        if rc == 0 {
            return Some(fd);
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(code)
                if code == libc::EINPROGRESS || code == libc::EINTR || code == libc::EALREADY =>
            {
                Some(fd)
            }
            _ => {
                libc::close(fd);
                None
            }
        }
    }
}

/// A live listener whose backlog is full and that never accepts must be
/// reported as `endpoint_in_use` (a peer exists), never as a stale socket.
#[test]
fn saturated_backlog_is_in_use_not_stale() {
    let dir = private_dir("saturated");
    let endpoint = dir.join("worker.sock");
    let listener = raw_listener(&endpoint, 1);
    // Fill the kernel backlog so production connects hit the saturated path.
    let mut held = Vec::new();
    for _ in 0..64 {
        match raw_connect_pending(&endpoint) {
            Some(fd) => held.push(fd),
            None => break,
        }
    }
    assert!(!held.is_empty(), "no connection reached the backlog");
    let shutdown = AtomicBool::new(true);
    let err = serve(&endpoint, &limits(60_000), &mut Echo, &shutdown).unwrap_err();
    for fd in held {
        unsafe { libc::close(fd) };
    }
    // The endpoint must not be touched by the refused serve.
    let meta = fs::symlink_metadata(&endpoint).unwrap();
    assert!(meta.file_type().is_socket());
    unsafe { libc::close(listener) };
    fs::remove_dir_all(&dir).unwrap();
    assert_eq!(err.code, ErrorCode::EndpointInUse);
}

/// connect_bounded (via `request`) must fail fast against a saturated
/// listener and never park a thread or exceed transport_wait by much.
#[test]
fn connect_to_saturated_backlog_is_bounded() {
    let dir = private_dir("bounded");
    let endpoint = dir.join("worker.sock");
    let listener = raw_listener(&endpoint, 1);
    let mut held = Vec::new();
    for _ in 0..64 {
        match raw_connect_pending(&endpoint) {
            Some(fd) => held.push(fd),
            None => break,
        }
    }
    let mut l = limits(60_000);
    l.transport_wait = Duration::from_millis(300);
    let started = Instant::now();
    for _ in 0..8 {
        let err = request(&endpoint, b"ping", &l).unwrap_err();
        assert!(
            matches!(
                err.code,
                ErrorCode::Timeout | ErrorCode::Io | ErrorCode::TransportClosed
            ),
            "unexpected error: {err:?}"
        );
    }
    let elapsed = started.elapsed();
    for fd in held {
        unsafe { libc::close(fd) };
    }
    unsafe { libc::close(listener) };
    fs::remove_dir_all(&dir).unwrap();
    assert!(
        elapsed < Duration::from_secs(10),
        "saturated connects were not bounded: {elapsed:?}"
    );
}

/// A stale socket must still be refused promptly by `request`/`serve` now
/// that the probe is nonblocking.
#[test]
fn stale_socket_fails_fast() {
    let dir = private_dir("stale-fast");
    let endpoint = dir.join("worker.sock");
    drop(std::os::unix::net::UnixListener::bind(&endpoint).unwrap());
    let mut l = limits(60_000);
    l.transport_wait = Duration::from_millis(300);
    let started = Instant::now();
    let err = request(&endpoint, b"ping", &l).unwrap_err();
    let elapsed = started.elapsed();
    assert!(fs::symlink_metadata(&endpoint).is_ok(), "stale socket kept");
    fs::remove_dir_all(&dir).unwrap();
    assert!(matches!(
        err.code,
        ErrorCode::TransportClosed | ErrorCode::Io | ErrorCode::Timeout
    ));
    assert!(elapsed < Duration::from_secs(5), "stale probe took {elapsed:?}");
}

/// An endpoint path too long for `sun_path` is refused instead of
/// truncating to a different socket.
#[test]
fn oversized_endpoint_path_is_refused() {
    let dir = private_dir("longpath");
    let long = "a".repeat(200);
    let endpoint = dir.join(long);
    let l = limits(60_000);
    let err = request(&endpoint, b"ping", &l).unwrap_err();
    assert_eq!(err.code, ErrorCode::EndpointUnsafe);
    let shutdown = AtomicBool::new(true);
    let err = serve(&endpoint, &l, &mut Echo, &shutdown).unwrap_err();
    assert_eq!(err.code, ErrorCode::EndpointUnsafe);
    fs::remove_dir_all(&dir).unwrap();
}
