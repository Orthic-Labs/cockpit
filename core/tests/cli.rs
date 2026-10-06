use std::process::Command;

#[test]
fn apply_never_accepts_paths_or_unissued_ids() {
    for argument in ["/tmp/anything", "plan-unissued"] {
        let result = Command::new(env!("CARGO_BIN_EXE_cockpit"))
            .args(["apply", argument, "--json"])
            .output()
            .unwrap();
        assert!(!result.status.success());
        let error: serde_json::Value = serde_json::from_slice(&result.stderr).unwrap();
        assert!(
            error["error"]
                .as_str()
                .unwrap()
                .contains("mutation is disabled")
        );
    }
}

#[test]
fn missing_usage_is_unavailable_not_zero() {
    let result = Command::new(env!("CARGO_BIN_EXE_cockpit"))
        .args(["usage", "--json"])
        .output()
        .unwrap();
    assert!(result.status.success());
    let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["claude"]["state"], "unavailable");
    assert!(value["claude"]["value"].is_null());
    assert!(value["codex"]["value"].is_null());
}

#[test]
fn scan_is_metadata_only_and_requires_opt_in_for_history() {
    let root = std::fs::canonicalize(std::env::temp_dir())
        .unwrap()
        .join(format!("cockpit-cli-fixture-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let data = root.join("data");
    let state = root.join("state");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("fixture.txt"), b"unchanged contents").unwrap();
    let execute = |save| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cockpit"));
        command
            .arg("scan")
            .arg(&data)
            .arg("--state-dir")
            .arg(&state)
            .arg("--json");
        if save {
            command.arg("--save");
        }
        command.output().unwrap()
    };
    let result = execute(false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!state.exists());
    assert_eq!(
        std::fs::read(data.join("fixture.txt")).unwrap(),
        b"unchanged contents"
    );
    let result = execute(true);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let history = cockpit_core::store::history(&state).unwrap();
    assert_eq!(history.len(), 1);
    assert!(history[0].findings.iter().all(|finding| !finding.eligible));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn corrupt_history_is_reported_without_breaking_json_stdout() {
    let root = std::fs::canonicalize(std::env::temp_dir())
        .unwrap()
        .join(format!("cockpit-cli-corrupt-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("scan-bad.json"), b"{").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_cockpit"))
        .args(["history", "--json", "--state-dir"])
        .arg(&root)
        .output()
        .unwrap();
    assert!(result.status.success());
    let stdout: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(stdout["history"], serde_json::json!([]));
    let diagnostic: serde_json::Value = serde_json::from_slice(&result.stderr).unwrap();
    assert_eq!(diagnostic["event"], "snapshot_skipped");
    assert_eq!(diagnostic["file"], "scan-bad.json");
    std::fs::remove_dir_all(root).unwrap();
}

use serde_json::Value;
use std::path::{Path, PathBuf};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cockpit"))
}

fn fixture_dir(name: &str) -> PathBuf {
    let root = std::fs::canonicalize(std::env::temp_dir())
        .unwrap()
        .join(format!("cockpit-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn run_json(args: &[&str], state: &Path) -> Value {
    let out = bin()
        .args(args)
        .arg("--state-dir")
        .arg(state)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn save_then_history_findings_explain_round_trip() {
    let root = fixture_dir("roundtrip");
    let data = root.join("data");
    let state = root.join("state");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("a.txt"), b"abc").unwrap();
    for _ in 0..2 {
        let out = bin()
            .arg("scan")
            .arg(&data)
            .args(["--save", "--json", "--state-dir"])
            .arg(&state)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let history = run_json(&["history"], &state);
    assert_eq!(history["history"].as_array().unwrap().len(), 2);
    assert_eq!(history["history_diagnostics"], serde_json::json!([]));
    assert!(history["history"][1].get("comparison").is_some());
    let findings = run_json(&["findings"], &state);
    assert_eq!(findings["mode"], "report_only");
    assert_eq!(findings["history_diagnostics"], serde_json::json!([]));
    assert!(findings["snapshot_id"].is_string());
    let rule_id = findings["explanations"]
        .as_array()
        .and_then(|a| a.first())
        .map(|r| r["id"].as_str().unwrap().to_string());
    if let Some(finding) = findings["findings"].as_array().and_then(|a| a.first()) {
        let id = finding["id"].as_str().unwrap();
        let explained = run_json(&["explain", id], &state);
        assert_eq!(explained["finding"]["id"], id);
        assert!(explained["history_diagnostics"].is_array());
    }
    if let Some(rule_id) = rule_id {
        let explained = run_json(&["explain", &rule_id], &state);
        assert_eq!(explained["id"], rule_id.as_str());
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn corrupt_history_file_appears_in_history_diagnostics() {
    let root = fixture_dir("diag");
    std::fs::write(root.join("scan-bad.json"), b"{").unwrap();
    for command in ["history", "findings"] {
        let value = run_json(&[command], &root);
        let diagnostics = value["history_diagnostics"].as_array().unwrap();
        assert_eq!(diagnostics.len(), 1, "{command}");
        assert_eq!(diagnostics[0]["file"], "scan-bad.json");
        assert!(diagnostics[0]["reason"].is_string());
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn storage_cli_journey_is_bounded_opt_in_and_preserves_fixture_bytes() {
    let root = fixture_dir("storage-journey");
    let data = root.join("data");
    let state = root.join("state");
    std::fs::create_dir_all(data.join("nested")).unwrap();
    let first = data.join("alpha.txt");
    let second = data.join("nested/alpha-copy.txt");
    std::fs::write(&first, b"same fixture bytes").unwrap();
    std::fs::write(&second, b"same fixture bytes").unwrap();
    let before_first = std::fs::read(&first).unwrap();
    let before_second = std::fs::read(&second).unwrap();

    let inside_state = data.join("state-inside");
    let rejected = bin()
        .args(["scan", "--save", "--state-dir"])
        .arg(&inside_state)
        .arg(&data)
        .output()
        .unwrap();
    assert_eq!(rejected.status.code(), Some(2));
    let rejected: Value = serde_json::from_slice(&rejected.stderr).unwrap();
    assert!(
        rejected["error"]
            .as_str()
            .unwrap()
            .contains("outside selected scan roots")
    );
    assert!(!inside_state.exists());

    let scan = bin()
        .args(["scan", "--save", "--json", "--state-dir"])
        .arg(&state)
        .arg(&data)
        .output()
        .unwrap();
    assert!(
        scan.status.success(),
        "{}",
        String::from_utf8_lossy(&scan.stderr)
    );

    let duplicate_before_change = bin()
        .args(["duplicates", "--min-size", "1", "--json"])
        .arg(&data)
        .output()
        .unwrap();
    assert!(
        duplicate_before_change.status.success(),
        "{}",
        String::from_utf8_lossy(&duplicate_before_change.stderr)
    );
    let duplicate_before_change: Value =
        serde_json::from_slice(&duplicate_before_change.stdout).unwrap();
    assert_eq!(
        duplicate_before_change["duplicates"]["groups"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let updated_first = b"same fixture bytes plus";
    std::fs::write(&first, updated_first).unwrap();
    let second_scan = bin()
        .args(["scan", "--save", "--json", "--state-dir"])
        .arg(&state)
        .arg(&data)
        .output()
        .unwrap();
    assert!(
        second_scan.status.success(),
        "{}",
        String::from_utf8_lossy(&second_scan.stderr)
    );

    let found = run_json(&["find", "alpha", "--kind", "file"], &state);
    assert_eq!(found["operation"], "find");
    assert!(found["data"]["total_matches"].as_u64().unwrap_or(0) >= 2);

    let browsed = bin()
        .args(["browse", "--folder"])
        .arg(&data)
        .args(["--state-dir"])
        .arg(&state)
        .args(["--json"])
        .output()
        .unwrap();
    assert!(
        browsed.status.success(),
        "{}",
        String::from_utf8_lossy(&browsed.stderr)
    );
    let browsed: Value = serde_json::from_slice(&browsed.stdout).unwrap();
    assert!(browsed["data"]["total_children"].is_number());

    let exported = run_json(&["export"], &state);
    assert_eq!(exported["schema_version"], 1);
    assert!(exported["snapshot"]["report"]["entries"].is_array());
    assert!(exported["modules"]["storage"]["largest_files"].is_array());
    assert!(exported["modules"]["monitor"]["network"].is_object());
    assert_eq!(
        exported["modules"]["history"]["snapshots"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        exported["modules"]["activity"]["events"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let history = run_json(&["history"], &state);
    assert_eq!(history["history"].as_array().unwrap().len(), 2);
    let expected_growth = updated_first.len() as i64 - before_first.len() as i64;
    assert_eq!(
        history["history"][1]["comparison"]["logical_growth_bytes"],
        expected_growth
    );
    assert_eq!(
        history["history"][1]["folder_comparison"]["comparable"],
        true
    );

    let duplicate_after_change = bin()
        .args(["duplicates", "--min-size", "1", "--json"])
        .arg(&data)
        .output()
        .unwrap();
    assert!(
        duplicate_after_change.status.success(),
        "{}",
        String::from_utf8_lossy(&duplicate_after_change.stderr)
    );
    let duplicate_after_change: Value =
        serde_json::from_slice(&duplicate_after_change.stdout).unwrap();
    assert_eq!(
        duplicate_after_change["duplicates"]["groups"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    let malformed = bin()
        .args(["find", "alpha", "--min-size", "malformed", "--state-dir"])
        .arg(&state)
        .output()
        .unwrap();
    assert_eq!(malformed.status.code(), Some(2));
    let malformed: Value = serde_json::from_slice(&malformed.stderr).unwrap();
    assert!(
        malformed["error"]
            .as_str()
            .unwrap()
            .contains("invalid --min-size")
    );
    assert_cli_error(&["duplicates"], "explicit scan paths");
    assert_eq!(std::fs::read(&first).unwrap(), updated_first);
    assert_eq!(std::fs::read(&second).unwrap(), before_second);

    let parent = root.join("parent");
    let parent_data = parent.join("visible");
    let parent_state = parent.join(".cockpit-state");
    std::fs::create_dir_all(&parent_data).unwrap();
    std::fs::create_dir_all(&parent_state).unwrap();
    std::fs::write(parent_data.join("visible.txt"), b"visible").unwrap();
    let private_marker = parent_state.join("private.marker");
    std::fs::write(&private_marker, b"private state").unwrap();
    let parent_scan = bin()
        .args(["scan", "--save", "--exclude-state"])
        .arg(&parent_state)
        .args(["--state-dir"])
        .arg(&parent_state)
        .arg(&parent)
        .args(["--json"])
        .output()
        .unwrap();
    assert!(
        parent_scan.status.success(),
        "{}",
        String::from_utf8_lossy(&parent_scan.stderr)
    );
    let parent_snapshot: Value = serde_json::from_slice(&parent_scan.stdout).unwrap();
    let excluded = std::fs::canonicalize(&parent_state).unwrap();
    let entries = parent_snapshot["snapshot"]["report"]["entries"]
        .as_array()
        .unwrap();
    assert!(entries.iter().all(|entry| {
        !entry["path"]
            .as_str()
            .map(Path::new)
            .is_some_and(|path| path == excluded || path.starts_with(&excluded))
    }));
    let folders = parent_snapshot["snapshot"]["report"]["folders"]
        .as_array()
        .unwrap();
    assert!(folders.iter().all(|folder| {
        !folder["path"]
            .as_str()
            .map(Path::new)
            .is_some_and(|path| path == excluded || path.starts_with(&excluded))
    }));
    assert_eq!(
        parent_snapshot["snapshot"]["report"]["accounting"]["logical_bytes"],
        7
    );
    assert_eq!(
        parent_snapshot["snapshot"]["report"]["accounting"]["incomplete"],
        true
    );
    assert!(
        parent_snapshot["snapshot"]["report"]["incomplete_reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason
                .as_str()
                .unwrap()
                .contains("excluded state directory"))
    );
    assert_eq!(std::fs::read(&private_marker).unwrap(), b"private state");
    std::fs::remove_dir_all(root).unwrap();
}

fn assert_cli_error(args: &[&str], needle: &str) {
    let out = bin().args(args).output().unwrap();
    assert_eq!(out.status.code(), Some(2), "{args:?}");
    let error: Value = serde_json::from_slice(&out.stderr).unwrap();
    let message = error["error"].as_str().unwrap();
    assert!(message.contains(needle), "{args:?}: {message}");
}

#[test]
fn option_validation_errors_are_typed_and_exit_two() {
    assert_cli_error(
        &["scan", "/tmp", "--max-depth", "abc"],
        "invalid --max-depth",
    );
    assert_cli_error(&["scan", "/tmp", "--max-depth", "999"], "scan limits");
    assert_cli_error(&["scan", "/tmp", "--max-entries", "0"], "scan limits");
    assert_cli_error(&["scan", "/tmp", "--bogus"], "unknown scan option");
    assert_cli_error(
        &["scan", "/tmp", "--max-depth", "1", "--max-depth", "2"],
        "duplicate --max-depth",
    );
    assert_cli_error(&["status", "--bogus"], "unexpected arguments");
    assert_cli_error(
        &["worker", "serve", "--idle-seconds", "0"],
        "--idle-seconds",
    );
    assert_cli_error(
        &["worker", "serve", "--idle-seconds", "601"],
        "--idle-seconds",
    );
    assert_cli_error(
        &["worker", "serve", "--idle-seconds", "x"],
        "--idle-seconds",
    );
    assert_cli_error(&["worker", "bogus"], "unknown worker command");
    assert_cli_error(&["nonsense"], "unknown command");
}

#[test]
fn mutation_commands_refuse() {
    for command in ["plan", "apply", "quit", "force-quit", "uninstall-plan"] {
        assert_cli_error(&[command], "mutation is disabled");
    }
}

#[test]
fn human_output_is_bounded_with_truncation_notice() {
    let root = fixture_dir("bounded");
    let data = root.join("data");
    for i in 0..30 {
        let dir = data.join(format!("d{i:02}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f"), b"x").unwrap();
    }
    let out = bin().arg("scan").arg(&data).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("more not shown"), "{text}");
    assert!(text.lines().count() < 200);
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn worker_serves_status_then_exits_after_idle() {
    use std::time::{Duration, Instant};
    let root = fixture_dir("worker");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    // Unix socket paths are short-limited; keep the name compact.
    let socket = root.join("w.sock");
    let mut server = bin()
        .args(["worker", "serve", "--idle-seconds", "5", "--endpoint"])
        .arg(&socket)
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() && Instant::now() < deadline {
        if server.try_wait().unwrap().is_some() {
            let output = server.wait_with_output().unwrap();
            panic!(
                "worker exited before binding: {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if !socket.exists() {
        let _ = server.kill();
        let output = server.wait_with_output().unwrap();
        panic!(
            "worker socket never appeared: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let out = bin()
        .args(["worker", "request", "status", "--json", "--endpoint"])
        .arg(&socket)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let response: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(response["status"], "ok");
    assert!(response["id"].as_str().unwrap().starts_with("cli-"));

    let human = bin()
        .args(["worker", "request", "status", "--endpoint"])
        .arg(&socket)
        .output()
        .unwrap();
    assert!(human.status.success());
    assert!(String::from_utf8_lossy(&human.stdout).contains("Worker response"));

    let deadline = Instant::now() + Duration::from_secs(20);
    let exited = loop {
        if let Some(status) = server.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() > deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    if exited.is_none() {
        let _ = server.kill();
        let _ = server.wait();
    }
    assert!(exited.expect("worker did not exit after idle").success());

    // With the server gone, a request is a typed transport error.
    let gone = bin()
        .args(["worker", "request", "status", "--json", "--endpoint"])
        .arg(&socket)
        .output()
        .unwrap();
    assert_eq!(gone.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&gone.stderr).unwrap();
    assert!(error["error"].is_string() && error["code"].is_string());
    std::fs::remove_dir_all(root).unwrap();
}

/// `worker exec-op` is an internal verb, but it is real CLI surface: it
/// reads ONE request body on stdin and writes ONE response on stdout.
#[cfg(any(unix, windows))]
fn exec_op(args: &[&str], input: &[u8]) -> std::process::Output {
    use std::io::Write;
    let mut child = bin()
        .arg("worker")
        .arg("exec-op")
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

#[cfg(any(unix, windows))]
#[test]
fn exec_op_round_trip_one_bounded_request_one_response() {
    let request = serde_json::json!({"version":1,"id":"t1","op":"status","args":{}});
    let out = exec_op(&[], serde_json::to_string(&request).unwrap().as_bytes());
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let response: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(response["version"], 1);
    assert_eq!(response["id"], "t1");
    assert_eq!(response["status"], "ok");
    assert!(response["data"]["system"].is_object());
}

#[cfg(any(unix, windows))]
#[test]
fn exec_op_oversized_request_is_typed_not_fatal() {
    // Viable limits (request cap >= 1), but the body exceeds them: the
    // child still writes one typed error response and exits 0.
    let request = serde_json::json!({"version":1,"id":"t2","op":"status"});
    let out = exec_op(
        &["--max-request-bytes", "8"],
        serde_json::to_string(&request).unwrap().as_bytes(),
    );
    assert_eq!(out.status.code(), Some(0));
    let response: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(response["status"], "error");
    assert_eq!(response["error"]["code"], "oversized_frame");
}

#[cfg(any(unix, windows))]
#[test]
fn exec_op_rejects_bad_or_nonviable_limits() {
    assert_cli_error(
        &["worker", "exec-op", "--max-request-bytes", "abc"],
        "invalid --max-request-bytes",
    );
    assert_cli_error(
        &["worker", "exec-op", "--max-response-bytes", "xyz"],
        "invalid --max-response-bytes",
    );
    // Limits too small to carry even a minimal error response are
    // refused before any I/O, exit 2 with a typed code.
    let out = bin()
        .args(["worker", "exec-op", "--max-response-bytes", "1"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert!(error["error"].is_string() && error["code"].is_string());
    assert_cli_error(&["worker", "exec-op", "--bogus"], "unexpected arguments");
}

#[test]
fn history_outputs_capability_notes_field() {
    let root = fixture_dir("notes");
    for command in ["history", "findings"] {
        let value = run_json(&[command], &root);
        assert!(
            value["capability_notes"].is_array(),
            "{command}: capability_notes missing"
        );
        #[cfg(windows)]
        assert!(
            !value["capability_notes"].as_array().unwrap().is_empty(),
            "{command}: windows store must disclose ACL inheritance"
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}
