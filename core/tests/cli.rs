use std::process::Command;

#[test]
fn apply_never_accepts_paths_or_unissued_ids() {
    for argument in ["/tmp/anything", "plan-unissued"] {
        let result = Command::new(env!("CARGO_BIN_EXE_cockpit")).args(["apply",argument,"--json"]).output().unwrap();
        assert!(!result.status.success());
        let error: serde_json::Value = serde_json::from_slice(&result.stderr).unwrap();
        assert!(error["error"].as_str().unwrap().contains("mutation is disabled"));
    }
}

#[test]
fn missing_usage_is_unavailable_not_zero() {
    let result = Command::new(env!("CARGO_BIN_EXE_cockpit")).args(["usage","--json"]).output().unwrap();
    assert!(result.status.success());
    let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["claude"]["state"],"unavailable");
    assert!(value["claude"]["value"].is_null());
    assert!(value["codex"]["value"].is_null());
}

#[test]
fn scan_is_metadata_only_and_requires_opt_in_for_history() {
    let root = std::fs::canonicalize(std::env::temp_dir()).unwrap().join(format!("cockpit-cli-fixture-{}",std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let data = root.join("data");
    let state = root.join("state");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("fixture.txt"),b"unchanged contents").unwrap();
    let execute = |save| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cockpit"));
        command.arg("scan").arg(&data).arg("--state-dir").arg(&state).arg("--json");
        if save {command.arg("--save");}
        command.output().unwrap()
    };
    let result = execute(false);
    assert!(result.status.success(),"{}",String::from_utf8_lossy(&result.stderr));
    assert!(!state.exists());
    assert_eq!(std::fs::read(data.join("fixture.txt")).unwrap(),b"unchanged contents");
    let result = execute(true);
    assert!(result.status.success(),"{}",String::from_utf8_lossy(&result.stderr));
    let history = cockpit_core::store::history(&state).unwrap();
    assert_eq!(history.len(),1);
    assert!(history[0].findings.iter().all(|finding| !finding.eligible));
    std::fs::remove_dir_all(root).unwrap();
}
