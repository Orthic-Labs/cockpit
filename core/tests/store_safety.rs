use cockpit_core::Accounting;
use cockpit_core::model::ScanReport;
use cockpit_core::store::{self, Snapshot};
use std::{
    fs,
    path::{Path, PathBuf},
};

struct Temp(PathBuf);
impl Temp {
    fn new(tag: &str) -> Self {
        let base = fs::canonicalize(std::env::temp_dir()).unwrap();
        let path = base.join(format!("cockpit-store-safety-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Temp(path)
    }
    fn state(&self) -> PathBuf {
        self.0.join("state")
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn report() -> ScanReport {
    ScanReport {
        roots: vec![PathBuf::from("fixture-root")],
        entries: Vec::new(),
        folders: Vec::new(),
        accounting: Accounting::default(),
        volume_usage: Vec::new(),
        volume_deltas: Vec::new(),
        inspection_errors: Vec::new(),
        skipped_links: Vec::new(),
        incomplete_reasons: Vec::new(),
    }
}
fn snap(id: &str, created_at: u64) -> Snapshot {
    let mut s = Snapshot::new(report(), Vec::new());
    s.id = id.into();
    s.created_at = created_at;
    s
}
fn write_raw(dir: &Path, name: &str, bytes: &[u8]) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join(name), bytes).unwrap();
}

#[test]
fn round_trip_and_deterministic_order() {
    let t = Temp::new("order");
    let d = t.state();
    store::save(&d, &snap("scan-b", 5)).unwrap();
    store::save(&d, &snap("scan-a", 5)).unwrap();
    store::save(&d, &snap("scan-z", 1)).unwrap();
    let ids: Vec<_> = store::history(&d)
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(ids, ["scan-z", "scan-a", "scan-b"]);
}

#[test]
fn rejects_path_like_and_malformed_ids() {
    let t = Temp::new("ids");
    let d = t.state();
    for id in [
        "",
        "scan-",
        "../scan-x",
        "scan-a/b",
        "scan-a\\b",
        "scan-a.b",
        "other-1",
        "scan-é",
        &format!("scan-{}", "a".repeat(80)),
    ] {
        let err = store::save(&d, &snap(id, 1)).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{id}");
    }
    assert!(!d.exists() || fs::read_dir(&d).unwrap().next().is_none());
}

#[test]
fn save_never_overwrites_existing_file() {
    let t = Temp::new("overwrite");
    let d = t.state();
    write_raw(&d, "scan-keep.json", b"unrelated");
    let err = store::save(&d, &snap("scan-keep", 1)).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read(d.join("scan-keep.json")).unwrap(), b"unrelated");
    store::save(&d, &snap("scan-new", 1)).unwrap();
    assert_eq!(
        store::save(&d, &snap("scan-new", 2)).unwrap_err().kind(),
        std::io::ErrorKind::AlreadyExists
    );
    let leftovers: Vec<_> = fs::read_dir(&d)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn refuses_state_path_that_is_a_file() {
    let t = Temp::new("file");
    fs::write(t.state(), b"x").unwrap();
    assert!(store::history(&t.state()).is_err());
    assert!(store::save(&t.state(), &snap("scan-1", 1)).is_err());
}

#[test]
fn malformed_and_future_snapshots_are_skipped_with_reasons() {
    let t = Temp::new("bad");
    let d = t.state();
    store::save(&d, &snap("scan-good", 1)).unwrap();
    write_raw(&d, "scan-trunc.json", b"{\"schema_version\":1,\"id\":");
    write_raw(&d, "scan-empty.json", b"");
    write_raw(
        &d,
        "scan-future.json",
        b"{\"schema_version\":2,\"id\":\"scan-future\"}",
    );
    write_raw(&d, "scan-noversion.json", b"{}");
    write_raw(&d, "scan-bad.json", b"{\"schema_version\":1}");
    // valid content but id disagrees with file name
    let mut other = serde_json::to_vec(&snap("scan-other", 1)).unwrap();
    other.extend_from_slice(b"\n");
    write_raw(&d, "scan-liar.json", &other);
    let report = store::history_report(&d).unwrap();
    assert_eq!(report.snapshots.len(), 1);
    assert_eq!(report.snapshots[0].id, "scan-good");
    let mut files: Vec<_> = report.skipped.iter().map(|s| s.file.as_str()).collect();
    files.sort();
    assert_eq!(
        files,
        [
            "scan-bad.json",
            "scan-empty.json",
            "scan-future.json",
            "scan-liar.json",
            "scan-noversion.json",
            "scan-trunc.json"
        ]
    );
    let future = report
        .skipped
        .iter()
        .find(|s| s.file == "scan-future.json")
        .unwrap();
    assert!(future.reason.contains("unsupported schema version 2"));
    assert_eq!(store::history(&d).unwrap().len(), 1);
}

#[test]
fn oversized_snapshot_is_skipped_not_parsed() {
    let t = Temp::new("big");
    let d = t.state();
    fs::create_dir_all(&d).unwrap();
    let f = fs::File::create(d.join("scan-huge.json")).unwrap();
    f.set_len(store::MAX_SNAPSHOT_BYTES + 1).unwrap(); // sparse where supported
    let report = store::history_report(&d).unwrap();
    assert!(report.snapshots.is_empty());
    assert!(report.skipped[0].reason.contains("size cap"));
}

#[test]
fn interrupted_temp_files_are_ignored() {
    let t = Temp::new("tmp");
    let d = t.state();
    store::save(&d, &snap("scan-ok", 1)).unwrap();
    write_raw(
        &d,
        ".scan-partial.1-2-3.tmp",
        b"{\"schema_version\":1,\"id\":\"scan-pa",
    );
    write_raw(&d, ".scan-old.tmp", b"garbage");
    write_raw(&d, "scan-half.json.tmp", b"garbage");
    let report = store::history_report(&d).unwrap();
    assert_eq!(report.snapshots.len(), 1);
    assert!(report.skipped.is_empty());
}

#[test]
fn missing_directory_is_empty_history() {
    let t = Temp::new("missing");
    assert!(store::history(&t.state()).unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn refuses_symlinked_state_directory_and_snapshot_links() {
    use std::os::unix::fs::symlink;
    let t = Temp::new("link");
    let real = t.0.join("real");
    fs::create_dir_all(&real).unwrap();
    let link = t.0.join("link");
    symlink(&real, &link).unwrap();
    assert!(store::history(&link).is_err());
    assert!(store::save(&link, &snap("scan-1", 1)).is_err());
    assert!(fs::read_dir(&real).unwrap().next().is_none());
    // a snapshot-named symlink inside a real directory is skipped, not followed
    let d = t.state();
    store::save(&d, &snap("scan-real", 1)).unwrap();
    symlink(d.join("scan-real.json"), d.join("scan-link.json")).unwrap();
    let report = store::history_report(&d).unwrap();
    assert_eq!(report.snapshots.len(), 1);
    assert_eq!(report.skipped[0].file, "scan-link.json");
}

#[cfg(unix)]
#[test]
fn permissions_new_dir_private_existing_dir_preserved() {
    use std::os::unix::fs::PermissionsExt;
    let t = Temp::new("perm");
    let created = t.0.join("a").join("b");
    let path = store::save(&created, &snap("scan-1", 1)).unwrap();
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&created), 0o700);
    assert_eq!(mode(&path), 0o600);
    let existing = t.0.join("existing");
    fs::create_dir_all(&existing).unwrap();
    fs::set_permissions(&existing, fs::Permissions::from_mode(0o755)).unwrap();
    let path = store::save(&existing, &snap("scan-2", 1)).unwrap();
    assert_eq!(mode(&existing), 0o755);
    assert_eq!(mode(&path), 0o600);
}

#[cfg(windows)]
#[test]
fn windows_directory_junction_is_refused() {
    let t = Temp::new("junction");
    let real = t.0.join("real");
    fs::create_dir_all(&real).unwrap();
    let link = t.0.join("junction");
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&link)
        .arg(&real)
        .output();
    if !matches!(status, Ok(ref o) if o.status.success()) {
        return; // cannot create a junction in this environment
    }
    assert!(store::history(&link).is_err());
    assert!(store::save(&link, &snap("scan-1", 1)).is_err());
    let _ = fs::remove_dir(&link);
}

#[cfg(windows)]
#[test]
fn windows_rejects_reserved_and_stream_ids() {
    let t = Temp::new("winids");
    for id in ["scan-a:stream", "scan-a*", "scan-a?", "scan-a<b"] {
        assert!(store::save(&t.state(), &snap(id, 1)).is_err());
    }
}

#[test]
fn concurrent_save_publishes_exactly_one_snapshot() {
    let t = Temp::new("race");
    let d = t.state();
    fs::create_dir_all(&d).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let workers: Vec<_> = (0..2)
        .map(|i| {
            let d = d.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store::save(&d, &snap("scan-race", i))
            })
        })
        .collect();
    let results: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert!(
        results
            .iter()
            .filter_map(|r| r.as_ref().err())
            .all(|e| e.kind() == std::io::ErrorKind::AlreadyExists)
    );
    assert_eq!(store::history(&d).unwrap().len(), 1);
}
