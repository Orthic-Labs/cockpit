#![cfg(windows)]

use pulse_core::ipc::windows::{default_endpoint, request, serve};
use pulse_core::ipc::{ErrorCode, Handler, Limits, Outcome, Response, ServeExit};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

struct Echo;
impl Handler for Echo {
    fn handle(&mut self, request: &[u8]) -> Vec<u8> {
        let mut out = b"echo:".to_vec();
        out.extend_from_slice(request);
        out
    }
}

fn unique(tag: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!(r"\\.\pipe\pulse-test-{}-{tag}-{nanos}", std::process::id())
}

fn limits() -> Limits {
    Limits {
        transport_wait: Duration::from_secs(5),
        idle_exit: Duration::from_secs(20),
        ..Limits::default()
    }
}

type ServerThread = JoinHandle<Result<ServeExit, pulse_core::ipc::IpcError>>;

fn spawn_server(endpoint: &str, limits: Limits, flag: &Arc<AtomicBool>) -> ServerThread {
    let endpoint = endpoint.to_owned();
    let flag = Arc::clone(flag);
    std::thread::spawn(move || serve(&endpoint, &limits, &mut Echo, &flag))
}

#[test]
fn round_trip_and_shutdown_flag() {
    let endpoint = unique("rt");
    let flag = Arc::new(AtomicBool::new(false));
    let server = spawn_server(&endpoint, limits(), &flag);
    assert_eq!(request(&endpoint, b"one", &limits()).unwrap(), b"echo:one");
    assert_eq!(request(&endpoint, b"two", &limits()).unwrap(), b"echo:two");
    let started = Instant::now();
    flag.store(true, Ordering::Release);
    assert_eq!(
        server.join().unwrap().unwrap(),
        ServeExit::ShutdownRequested
    );
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn default_endpoint_is_a_safe_pipe_name() {
    let endpoint = default_endpoint().unwrap();
    let name = endpoint
        .strip_prefix(r"\\.\pipe\pulse-worker-S-1-")
        .unwrap();
    assert!(!name.contains('\\'));
}

#[test]
fn oversized_request_is_rejected_both_sides() {
    // Client-side limit.
    let endpoint = unique("big-local");
    let small = Limits {
        max_request_bytes: 16,
        ..limits()
    };
    assert_eq!(
        request(&endpoint, &[b'x'; 64], &small).unwrap_err().code,
        ErrorCode::OversizedFrame
    );

    // Server-side limit: the server answers with a typed error and never
    // reads the body.
    let endpoint = unique("big-remote");
    let flag = Arc::new(AtomicBool::new(false));
    let server = spawn_server(&endpoint, small, &flag);
    let response = request(&endpoint, &[b'x'; 64], &limits()).unwrap();
    let parsed: Response = serde_json::from_slice(&response).unwrap();
    match parsed.outcome {
        Outcome::Error { error } => assert_eq!(error.code, ErrorCode::OversizedFrame),
        Outcome::Ok { .. } => panic!("oversized request was accepted"),
    }
    flag.store(true, Ordering::Release);
    server.join().unwrap().unwrap();
}

#[test]
fn second_server_reports_endpoint_in_use() {
    let endpoint = unique("dup");
    let flag = Arc::new(AtomicBool::new(false));
    let server = spawn_server(&endpoint, limits(), &flag);
    // A completed round trip proves the first instance exists.
    assert_eq!(request(&endpoint, b"up", &limits()).unwrap(), b"echo:up");
    let other = AtomicBool::new(false);
    let error = serve(&endpoint, &limits(), &mut Echo, &other).unwrap_err();
    assert_eq!(error.code, ErrorCode::EndpointInUse);
    flag.store(true, Ordering::Release);
    server.join().unwrap().unwrap();
}

#[test]
fn bad_endpoint_names_are_unsafe() {
    for endpoint in [
        r"\\.\pipe\a\b",
        r"\\.\pipe\",
        r"\\server\pipe\x",
        r"C:\temp\x",
        "plain",
    ] {
        let flag = AtomicBool::new(false);
        assert_eq!(
            serve(endpoint, &limits(), &mut Echo, &flag)
                .unwrap_err()
                .code,
            ErrorCode::EndpointUnsafe,
            "{endpoint}"
        );
        assert_eq!(
            request(endpoint, b"x", &limits()).unwrap_err().code,
            ErrorCode::EndpointUnsafe,
            "{endpoint}"
        );
    }
}

#[test]
fn idle_exit() {
    let endpoint = unique("idle");
    let flag = AtomicBool::new(false);
    let short = Limits {
        idle_exit: Duration::from_millis(200),
        ..limits()
    };
    let started = Instant::now();
    assert_eq!(
        serve(&endpoint, &short, &mut Echo, &flag).unwrap(),
        ServeExit::IdleTimeout
    );
    assert!(started.elapsed() < Duration::from_secs(3));
}

/// Connects a raw client that never sends a byte. The server must drop
/// it within ~transport_wait (bounded CancelIoEx + drain) and stay
/// responsive, instead of hanging on an unbounded wait.
#[test]
fn silent_peer_is_dropped_within_bounded_wait() {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_NONE,
        OPEN_EXISTING,
    };
    use windows::core::PCWSTR;

    let endpoint = unique("silent");
    let flag = Arc::new(AtomicBool::new(false));
    let short = Limits {
        transport_wait: Duration::from_millis(300),
        idle_exit: Duration::from_secs(10),
        ..limits()
    };
    let server = spawn_server(&endpoint, short, &flag);

    let wide: Vec<u16> = endpoint.encode_utf16().chain(Some(0)).collect();
    let started = Instant::now();
    // Retry until the pipe instance exists.
    let raw = loop {
        let opened = unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        };
        match opened {
            Ok(handle) => break handle,
            Err(_) => {
                assert!(
                    started.elapsed() < Duration::from_secs(5),
                    "pipe never appeared"
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    // Stay silent well past transport_wait so the server's read deadline
    // fires and the bounded cancellation path runs.
    std::thread::sleep(Duration::from_millis(1500));
    unsafe {
        let _ = CloseHandle(raw);
    }
    // A hung cancellation would leave the server unable to accept: this
    // request must still complete.
    assert_eq!(request(&endpoint, b"ping", &short).unwrap(), b"echo:ping");
    assert!(started.elapsed() < Duration::from_secs(8));
    flag.store(true, Ordering::Release);
    server.join().unwrap().unwrap();
}

#[test]
fn tiny_response_limit_is_rejected() {
    let endpoint = unique("tiny");
    let flag = AtomicBool::new(false);
    let tiny = Limits {
        max_response_bytes: 1, // cannot carry even a minimal error frame
        ..limits()
    };
    assert_eq!(
        serve(&endpoint, &tiny, &mut Echo, &flag).unwrap_err().code,
        ErrorCode::InvalidArguments
    );
    assert_eq!(
        request(&endpoint, b"x", &tiny).unwrap_err().code,
        ErrorCode::InvalidArguments
    );
}

/// A handler body larger than max_response_bytes must be replaced by a
/// serialized error that itself fits the cap — never an overrun.
#[test]
fn oversized_handler_reply_becomes_capped_error() {
    let endpoint = unique("cap");
    let flag = Arc::new(AtomicBool::new(false));
    let capped = Limits {
        max_request_bytes: 1024,
        max_response_bytes: 512,
        ..limits()
    };
    let server = spawn_server(&endpoint, capped, &flag);
    // 600-byte request echoes to ~605 bytes, over the 512 cap.
    let response = request(&endpoint, &[b'y'; 600], &capped).unwrap();
    assert!(response.len() <= 512, "response overran the cap");
    let parsed: Response = serde_json::from_slice(&response).unwrap();
    match parsed.outcome {
        Outcome::Error { error } => assert_eq!(error.code, ErrorCode::Internal),
        Outcome::Ok { .. } => panic!("oversized reply was sent uncapped"),
    }
    flag.store(true, Ordering::Release);
    server.join().unwrap().unwrap();
}

#[test]
fn client_times_out_without_server() {
    let endpoint = unique("none");
    let short = Limits {
        transport_wait: Duration::from_millis(300),
        ..limits()
    };
    let started = Instant::now();
    assert_eq!(
        request(&endpoint, b"x", &short).unwrap_err().code,
        ErrorCode::Timeout
    );
    assert!(started.elapsed() < Duration::from_secs(3));
}
