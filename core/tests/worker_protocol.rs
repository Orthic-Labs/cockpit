//! Transport-free protocol tests for the read-only worker.

use cockpit_core::ipc::{
    ErrorCode, Event, Handler, Limits, Outcome, PROTOCOL_VERSION, Phase, Response,
};
use cockpit_core::worker::Worker;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn call(worker: &mut Worker, request: Value) -> Response {
    call_raw(worker, &serde_json::to_vec(&request).unwrap())
}

fn call_raw(worker: &mut Worker, bytes: &[u8]) -> Response {
    serde_json::from_slice(&worker.handle(bytes)).expect("response parses")
}

fn req(id: &str, op: &str, args: Value) -> Value {
    json!({"version": PROTOCOL_VERSION, "id": id, "op": op, "args": args})
}

fn code(response: &Response) -> ErrorCode {
    match &response.outcome {
        Outcome::Error { error } => error.code,
        Outcome::Ok { .. } => panic!("expected error, got {response:?}"),
    }
}

fn ok(response: &Response) -> (&Value, bool) {
    match &response.outcome {
        Outcome::Ok { data, truncated } => (data, *truncated),
        Outcome::Error { error } => panic!("expected ok, got {error:?}"),
    }
}

struct TempTree(PathBuf);

impl TempTree {
    fn new(files: usize) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let root = base.join(format!("cockpit-worker-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        for i in 0..files {
            std::fs::write(root.join(format!("file-with-a-long-name-{i:04}.bin")), b"data")
                .unwrap();
        }
        Self(root)
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn oversized_request_is_rejected() {
    let limits = Limits {
        max_request_bytes: 32,
        ..Limits::default()
    };
    let mut worker = Worker::new(limits);
    let body = serde_json::to_vec(&req("big-1", "status", json!({"pad": "x".repeat(64)}))).unwrap();
    let response = call_raw(&mut worker, &body);
    assert_eq!(code(&response), ErrorCode::OversizedFrame);
}

#[test]
fn malformed_requests_have_typed_errors_and_unreadable_ids_are_none() {
    let mut worker = Worker::new(Limits::default());
    let response = call_raw(&mut worker, b"not json");
    assert_eq!(code(&response), ErrorCode::MalformedRequest);
    assert_eq!(response.id, None);

    let response = call(&mut worker, req("../escape", "status", Value::Null));
    assert_eq!(code(&response), ErrorCode::MalformedRequest);
    assert_eq!(response.id, None);

    let response = call(&mut worker, json!({"version": 1, "op": "status"}));
    assert_eq!(code(&response), ErrorCode::MalformedRequest);
    assert_eq!(response.id, None);

    // Readable id is echoed when only another field is wrong.
    let response = call(&mut worker, json!({"version": "one", "id": "m-4", "op": "status"}));
    assert_eq!(code(&response), ErrorCode::MalformedRequest);
    assert_eq!(response.id.as_deref(), Some("m-4"));
}

#[test]
fn unsupported_version_is_rejected() {
    let mut worker = Worker::new(Limits::default());
    let response = call(
        &mut worker,
        json!({"version": PROTOCOL_VERSION + 1, "id": "v-1", "op": "status"}),
    );
    assert_eq!(code(&response), ErrorCode::UnsupportedVersion);
    assert_eq!(response.id.as_deref(), Some("v-1"));
}

#[test]
fn reused_request_id_conflicts_even_after_failure() {
    let mut worker = Worker::new(Limits::default());
    let first = call(&mut worker, req("dup-1", "settings", Value::Null));
    assert_eq!(code(&first), ErrorCode::UnsupportedOperation);
    let second = call(&mut worker, req("dup-1", "status", Value::Null));
    assert_eq!(code(&second), ErrorCode::ConflictingRequestId);
}

#[test]
fn mutating_and_settings_operations_are_unsupported() {
    let mut worker = Worker::new(Limits::default());
    for (n, op) in [
        "settings",
        "cleanup",
        "apply",
        "quit",
        "uninstall",
        "terminate",
        "bogus",
    ]
    .into_iter()
    .enumerate()
    {
        let response = call(&mut worker, req(&format!("op-{n}"), op, json!({})));
        assert_eq!(code(&response), ErrorCode::UnsupportedOperation, "{op}");
    }
}

#[test]
fn invalid_arguments_are_rejected() {
    let mut worker = Worker::new(Limits::default());
    let tree = TempTree::new(0);
    let root = tree.0.to_string_lossy().into_owned();
    let too_many: Vec<String> = (0..65).map(|_| root.clone()).collect();
    let cases = [
        req("a-1", "status", json!({"x": 1})),
        req("a-2", "processes", json!({"unknown": true})),
        req("a-3", "scan", Value::Null),
        req("a-4", "scan", json!({"roots": []})),
        req("a-5", "scan", json!({"roots": ["relative/path"]})),
        req("a-6", "scan", json!({"roots": too_many})),
        req("a-7", "scan", json!({"roots": [root], "max_depth": 129})),
        req("a-8", "scan", json!({"roots": [root], "max_entries": 0})),
        req("a-9", "scan", json!({"roots": [root], "max_entries": 1_000_001})),
        req("a-10", "scan", json!({"roots": [root], "extra": 1})),
    ];
    for case in cases {
        let id = case["id"].as_str().unwrap().to_owned();
        let response = call(&mut worker, case);
        assert_eq!(code(&response), ErrorCode::InvalidArguments, "{id}");
        assert_eq!(response.id.as_deref(), Some(id.as_str()));
    }
}

#[test]
fn scan_on_temp_dir_with_small_limits() {
    let tree = TempTree::new(5);
    let mut worker = Worker::new(Limits::default());
    let response = call(
        &mut worker,
        req(
            "scan-1",
            "scan",
            json!({"roots": [tree.0], "max_depth": 2, "max_entries": 100}),
        ),
    );
    let (data, truncated) = ok(&response);
    assert!(!truncated);
    assert_eq!(data["entries_omitted"], 0);
    assert!(data["report"]["entries"].as_array().unwrap().len() >= 5);
}

#[test]
fn scan_entry_limit_is_reported_not_hidden() {
    let tree = TempTree::new(10);
    let mut worker = Worker::new(Limits::default());
    let response = call(
        &mut worker,
        req("scan-2", "scan", json!({"roots": [tree.0], "max_entries": 3})),
    );
    let (data, _) = ok(&response);
    assert!(!data["report"]["incomplete_reasons"].as_array().unwrap().is_empty());
}

#[test]
fn oversized_scan_response_is_truncated_and_marked_incomplete() {
    let tree = TempTree::new(60);
    let args = json!({"roots": [tree.0], "max_entries": 1000});
    let mut full_worker = Worker::new(Limits::default());
    let full = full_worker.handle(&serde_json::to_vec(&req("full-1", "scan", args.clone())).unwrap());
    let full_total = {
        let response: Response = serde_json::from_slice(&full).unwrap();
        let (data, truncated) = ok(&response);
        assert!(!truncated);
        data["report"]["entries"].as_array().unwrap().len()
    };

    let limits = Limits {
        max_response_bytes: full.len() - 1,
        ..Limits::default()
    };
    let mut worker = Worker::new(limits);
    let bytes = worker.handle(&serde_json::to_vec(&req("trunc-1", "scan", args)).unwrap());
    assert!(bytes.len() <= limits.max_response_bytes);
    let response: Response = serde_json::from_slice(&bytes).unwrap();
    let (data, truncated) = ok(&response);
    assert!(truncated);
    let kept = data["report"]["entries"].as_array().unwrap().len();
    let omitted = data["entries_omitted"].as_u64().unwrap() as usize;
    assert!(omitted > 0);
    assert_eq!(kept + omitted, full_total);
    // Output truncation is reported at the response level, distinct from
    // the scan's own accounting completeness.
    assert!(
        data["report"]["incomplete_reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r.as_str().unwrap().starts_with("output truncated"))
    );
}

#[test]
fn reused_request_id_conflicts_after_version_failure() {
    let mut worker = Worker::new(Limits::default());
    let first = call(
        &mut worker,
        json!({"version": PROTOCOL_VERSION + 1, "id": "ver-dup", "op": "status"}),
    );
    assert_eq!(code(&first), ErrorCode::UnsupportedVersion);
    let second = call(&mut worker, req("ver-dup", "status", Value::Null));
    assert_eq!(code(&second), ErrorCode::ConflictingRequestId);
}

#[test]
fn tiny_but_viable_error_limit_uses_minimal_error() {
    // Above the minimal-response floor but below a full error message:
    // the worker must still emit a parseable, bounded error.
    let floor = cockpit_core::ipc::minimal_response_bytes();
    let limits = Limits {
        max_response_bytes: floor,
        ..Limits::default()
    };
    let mut worker = Worker::new(limits);
    let bytes = worker.handle(&serde_json::to_vec(&req("min-1", "bogus", Value::Null)).unwrap());
    assert!(bytes.len() <= limits.max_response_bytes);
    assert_eq!(code(&serde_json::from_slice(&bytes).unwrap()), ErrorCode::UnsupportedOperation);
}

#[test]
fn maximal_request_id_retains_correlation_at_error_budget_floor() {
    let limits = Limits {
        max_response_bytes: cockpit_core::ipc::minimal_response_bytes(),
        ..Limits::default()
    };
    let id = "x".repeat(cockpit_core::ipc::MAX_REQUEST_ID_LEN);
    let mut worker = Worker::new(limits);
    let bytes = worker.handle(&serde_json::to_vec(&req(&id, &"unsupported".repeat(50), Value::Null)).unwrap());
    assert!(bytes.len() <= limits.max_response_bytes);
    let response: Response = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(response.id.as_deref(), Some(id.as_str()));
    assert_eq!(code(&response), ErrorCode::UnsupportedOperation);
}

#[test]
fn impossible_response_limit_closes_instead_of_overrunning() {
    // Below the minimal-response floor: no serialized error can fit, so
    // the handler returns empty — the transport closes the connection.
    let limits = Limits {
        max_response_bytes: 1,
        ..Limits::default()
    };
    assert!(!limits.viable());
    let mut worker = Worker::new(limits);
    let bytes = worker.handle(&serde_json::to_vec(&req("tiny-1", "status", Value::Null)).unwrap());
    assert!(bytes.is_empty());
}

#[test]
fn bounded_worker_executes_ops_in_killable_child() {
    // Exercises the real `cockpit` binary's `worker exec-op` path. The bin
    // path is required: cargo supplies it for integration tests, so this
    // coverage must never silently skip.
    let exe = PathBuf::from(env!("CARGO_BIN_EXE_cockpit"));
    let events: Arc<Mutex<Vec<Event>>> = Arc::default();
    let sink = events.clone();
    let mut worker = Worker::bounded(
        Limits::default(),
        Box::new(move |event| sink.lock().unwrap().push(event.clone())),
        exe,
    );
    let response = call(&mut worker, req("sub-1", "status", Value::Null));
    let (data, truncated) = ok(&response);
    assert!(!truncated);
    assert_eq!(data["schema_version"], 1);
    assert!(data["system"].is_object());
    let tree = TempTree::new(4);
    let response = call(
        &mut worker,
        req("sub-2", "scan", json!({"roots": [tree.0], "max_depth": 2})),
    );
    let (data, _) = ok(&response);
    assert!(data["report"]["entries"].as_array().unwrap().len() >= 4);
    // Unsupported ops fail in the child and surface as typed errors.
    let response = call(&mut worker, req("sub-3", "cleanup", Value::Null));
    assert_eq!(code(&response), ErrorCode::UnsupportedOperation);
    let events = events.lock().unwrap();
    assert!(events.iter().any(|e| e.phase == Phase::Completed));
    assert!(events.iter().any(|e| e.phase == Phase::Failed));
}

/// A "child" executable that ignores its argv and hangs without ever
/// reading stdin — exercises the deadline kill/reap and blocked-stdin path
/// through the production `Worker::bounded` machinery.
#[cfg(unix)]
fn hanging_exe() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir()
        .join(format!("cockpit-hang-{}-{nanos}.sh", std::process::id()));
    std::fs::write(&path, "#!/bin/sh\nexec sleep 600\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

/// A "child" that floods stdout (and stderr, which production code
/// discards) forever — exercises the response cap and early kill.
#[cfg(unix)]
fn flooding_exe() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir()
        .join(format!("cockpit-flood-{}-{nanos}.sh", std::process::id()));
    std::fs::write(&path, "#!/bin/sh\nwhile :; do printf 'stdout-flood-01234567890123456789\\n'; printf 'stderr-flood-01234567890123456789\\n' >&2; done\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

#[cfg(unix)]
fn bounded(limits: Limits, exe: PathBuf) -> Worker {
    Worker::bounded(limits, Box::new(|_| {}), exe)
}

#[cfg(unix)]
#[test]
fn blocked_stdin_child_is_killed_at_deadline() {
    let exe = hanging_exe();
    // Larger than any pipe buffer, so the stdin write can only complete if
    // the child reads — this one never does. The op_deadline, not the pipe,
    // must bound the call.
    let limits = Limits {
        max_request_bytes: 1024 * 1024,
        op_deadline: std::time::Duration::from_millis(300),
        ..Limits::default()
    };
    let mut worker = bounded(limits, exe.clone());
    let big = req(
        "hang-1",
        "scan",
        json!({"roots": ["/"], "pad": "x".repeat(512 * 1024)}),
    );
    let start = std::time::Instant::now();
    let response = call(&mut worker, big);
    assert!(
        start.elapsed() < std::time::Duration::from_secs(5),
        "blocked stdin escaped the deadline"
    );
    assert_eq!(code(&response), ErrorCode::Timeout);
    std::fs::remove_file(&exe).ok();
}

#[cfg(unix)]
#[test]
fn flooding_stdout_is_capped_and_killed() {
    let exe = flooding_exe();
    let limits = Limits {
        max_response_bytes: 4096,
        op_deadline: std::time::Duration::from_secs(10),
        ..Limits::default()
    };
    let mut worker = bounded(limits, exe.clone());
    let start = std::time::Instant::now();
    let response = call(&mut worker, req("flood-1", "status", Value::Null));
    // Overflow kills the child well before the 10s op deadline.
    assert!(start.elapsed() < std::time::Duration::from_secs(5));
    assert_eq!(code(&response), ErrorCode::Internal);
    std::fs::remove_file(&exe).ok();
}

#[test]
fn exec_op_spawn_failure_is_typed() {
    let mut worker = Worker::bounded(
        Limits::default(),
        Box::new(|_| {}),
        PathBuf::from("/nonexistent/cockpit-does-not-exist"),
    );
    let response = call(&mut worker, req("noexe-1", "status", Value::Null));
    assert_eq!(code(&response), ErrorCode::Internal);
}

#[test]
fn status_and_processes_use_core_data() {
    let mut worker = Worker::new(Limits::default());
    let response = call(&mut worker, req("st-1", "status", Value::Null));
    let (data, truncated) = ok(&response);
    assert!(!truncated);
    assert_eq!(data["schema_version"], 1);
    assert!(data["system"].is_object());

    let response = call(&mut worker, req("pr-1", "processes", json!({"grouped": true})));
    let (data, truncated) = ok(&response);
    assert_eq!(data["schema_version"], 1);
    assert!(data["processes"].is_array());
    assert!(data["processes"].as_array().unwrap().len() <= 5000);
    assert_eq!(truncated, data["omitted"].as_u64().unwrap() > 0);
    assert!(data.get("groups").is_some());
}

#[test]
fn events_follow_request_lifecycle() {
    let events: Arc<Mutex<Vec<Event>>> = Arc::default();
    let sink = events.clone();
    let mut worker = Worker::with_events(
        Limits::default(),
        Box::new(move |event| sink.lock().unwrap().push(event.clone())),
    );
    call(&mut worker, req("ev-1", "status", Value::Null));
    call(&mut worker, req("ev-2", "cleanup", Value::Null));
    call_raw(&mut worker, b"garbage");

    let events = events.lock().unwrap();
    let phases: Vec<_> = events
        .iter()
        .map(|e| (e.id.as_deref(), e.op.as_deref(), e.phase, e.error))
        .collect();
    assert_eq!(
        phases,
        vec![
            (Some("ev-1"), Some("status"), Phase::Started, None),
            (Some("ev-1"), Some("status"), Phase::Completed, None),
            (Some("ev-2"), Some("cleanup"), Phase::Started, None),
            (
                Some("ev-2"),
                Some("cleanup"),
                Phase::Failed,
                Some(ErrorCode::UnsupportedOperation)
            ),
            (None, None, Phase::Failed, Some(ErrorCode::MalformedRequest)),
        ]
    );
}

#[test]
fn limits_expose_idle_and_transport_bounds() {
    let limits = Limits {
        idle_exit: std::time::Duration::from_secs(7),
        transport_wait: std::time::Duration::from_secs(3),
        ..Limits::default()
    };
    assert_eq!(Worker::new(limits).limits(), limits);
}
