use pulse_core::Accounting;
use pulse_core::model::ScanReport;
use pulse_core::store::{self, Snapshot};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static UNIQUE: AtomicU64 = AtomicU64::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new(tag: &str) -> Self {
        let base = fs::canonicalize(std::env::temp_dir()).unwrap();
        let n = UNIQUE.fetch_add(1, Ordering::Relaxed);
        let path = base.join(format!(
            "pulse-store-safety-{tag}-{}-{n}",
            std::process::id()
        ));
        // Unique per process and call: nothing pre-existing is ever removed.
        fs::create_dir(&path).unwrap();
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
fn history_rejects_too_many_unrelated_directory_entries() {
    let t = Temp::new("entry-cap");
    let d = t.state();
    fs::create_dir_all(&d).unwrap();
    for i in 0..=store::MAX_ENUMERATED_ENTRIES {
        write_raw(&d, &format!("unrelated-{i}"), b"ignored");
    }
    let error = store::history_report(&d).unwrap_err();
    assert!(error.to_string().contains("entry cap"), "{error}");
}

#[test]
fn interrupted_temp_files_are_reported_never_parsed() {
    let t = Temp::new("tmp");
    let d = t.state();
    store::save(&d, &snap("scan-ok", 1)).unwrap();
    // Valid-looking JSON in a temp file must still not be loaded.
    let valid = serde_json::to_vec(&snap("scan-partial", 1)).unwrap();
    write_raw(&d, ".scan-partial.1-2-3.tmp", &valid);
    write_raw(&d, ".scan-old.tmp", b"garbage");
    write_raw(&d, "scan-half.json.tmp", b"garbage");
    write_raw(&d, "unrelated.txt", b"ignored");
    let report = store::history_report(&d).unwrap();
    assert_eq!(report.snapshots.len(), 1);
    assert_eq!(report.snapshots[0].id, "scan-ok");
    let files: Vec<_> = report.skipped.iter().map(|s| s.file.as_str()).collect();
    assert_eq!(
        files,
        [
            ".scan-old.tmp",
            ".scan-partial.1-2-3.tmp",
            "scan-half.json.tmp"
        ]
    );
    assert!(
        report
            .skipped
            .iter()
            .all(|s| s.reason == "interrupted publication")
    );
    // Never deleted automatically.
    assert!(d.join(".scan-old.tmp").exists());
}

#[test]
fn size_cap_boundary_exact_vs_plus_one() {
    let t = Temp::new("sizecap");
    let d = t.state();
    fs::create_dir_all(&d).unwrap();
    let set = |name: &str, len: u64| {
        fs::File::create(d.join(name))
            .unwrap()
            .set_len(len)
            .unwrap();
    };
    set("scan-exact.json", store::MAX_SNAPSHOT_BYTES);
    set("scan-over.json", store::MAX_SNAPSHOT_BYTES + 1);
    let report = store::history_report(&d).unwrap();
    let reason = |f: &str| {
        report
            .skipped
            .iter()
            .find(|s| s.file == f)
            .unwrap()
            .reason
            .clone()
    };
    // Exactly the cap is read (and then fails to parse); +1 is refused by size.
    assert!(reason("scan-exact.json").contains("malformed"));
    assert!(reason("scan-over.json").contains("size cap"));
}

#[test]
fn snapshot_count_cap_boundary() {
    let t = Temp::new("countcap");
    let d = t.state();
    fs::create_dir_all(&d).unwrap();
    for i in 0..store::MAX_SNAPSHOTS {
        fs::write(d.join(format!("scan-{i:05}.json")), b"{}").unwrap();
    }
    let report = store::history_report(&d).unwrap();
    assert_eq!(report.skipped.len(), store::MAX_SNAPSHOTS);
    fs::write(d.join("scan-extra.json"), b"{}").unwrap();
    let err = store::history_report(&d).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("count cap"));
}

#[test]
fn temp_files_count_toward_candidate_cap() {
    let t = Temp::new("tmpcap");
    let d = t.state();
    fs::create_dir_all(&d).unwrap();
    for i in 0..=store::MAX_SNAPSHOTS {
        fs::write(d.join(format!(".scan-{i}.tmp")), b"x").unwrap();
    }
    assert!(store::history_report(&d).is_err());
}

#[test]
fn aggregate_read_budget_stops_and_records_skips() {
    let t = Temp::new("budget");
    let d = t.state();
    for id in ["scan-a", "scan-b", "scan-c"] {
        store::save(&d, &snap(id, 1)).unwrap();
    }
    let size = fs::metadata(d.join("scan-a.json")).unwrap().len();
    let report = store::history_report_with_budget(&d, size * 2).unwrap();
    let ids: Vec<_> = report.snapshots.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["scan-a", "scan-b"]);
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].file, "scan-c.json");
    assert_eq!(report.skipped[0].reason, "aggregate read budget");
    // Zero budget skips everything with the same reason; files are untouched.
    let none = store::history_report_with_budget(&d, 0).unwrap();
    assert!(none.snapshots.is_empty());
    assert!(
        none.skipped
            .iter()
            .all(|s| s.reason == "aggregate read budget")
    );
    assert_eq!(none.skipped.len(), 3);
    // Default budget loads all.
    assert_eq!(store::history_report(&d).unwrap().snapshots.len(), 3);
}

#[test]
fn case_variant_names_are_duplicate_id_skips() {
    let t = Temp::new("dup");
    let d = t.state();
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("scan-Dup.json"), b"{}").unwrap();
    fs::write(d.join("scan-dup.json"), b"{}").unwrap();
    let listed = fs::read_dir(&d).unwrap().count();
    if listed < 2 {
        return; // case-insensitive filesystem: the duplicate cannot exist
    }
    let report = store::history_report(&d).unwrap();
    assert!(report.snapshots.is_empty());
    assert_eq!(report.skipped.len(), 2);
    assert!(report.skipped.iter().all(|s| s.reason == "duplicate id"));
}

#[test]
fn concurrent_distinct_ids_all_publish() {
    let t = Temp::new("distinct");
    let d = t.state();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let workers: Vec<_> = (0..8u64)
        .map(|i| {
            let d = d.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store::save(&d, &snap(&format!("scan-t{i}"), i))
            })
        })
        .collect();
    for w in workers {
        w.join().unwrap().unwrap();
    }
    let report = store::history_report(&d).unwrap();
    assert_eq!(report.snapshots.len(), 8);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
}

#[cfg(unix)]
#[test]
fn dangling_symlink_destination_is_not_written_through() {
    // Publication must fail closed (AlreadyExists) rather than replace or
    // follow whatever occupies the destination. A filesystem without hard
    // links yields a distinct "atomic no-replace publication unavailable"
    // error from the same code path; that cannot be simulated portably here.
    let t = Temp::new("dangling");
    let d = t.state();
    fs::create_dir_all(&d).unwrap();
    let target = t.0.join("outside");
    std::os::unix::fs::symlink(&target, d.join("scan-x.json")).unwrap();
    let err = store::save(&d, &snap("scan-x", 1)).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    assert!(!target.exists());
    assert!(
        fs::read_dir(&d).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp"))
    );
}

#[cfg(windows)]
#[test]
fn windows_capability_note_is_reported() {
    let t = Temp::new("note");
    let d = t.state();
    store::save(&d, &snap("scan-1", 1)).unwrap();
    let report = store::history_report(&d).unwrap();
    assert!(report.capability_notes.iter().any(|n| n.contains("ACL")));
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

#[cfg(windows)]
#[test]
fn windows_real_directory_replacement_keeps_operations_inside_owned_dirs() {
    let t = Temp::new("real-swap");
    let state = t.state();
    let held = t.0.join("held");
    let replacement = t.0.join("replacement");
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(&replacement).unwrap();
    for i in 0..300 {
        fs::write(replacement.join(format!("scan-win-{i}.json")), b"sentinel").unwrap();
    }
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let swapper = {
        let state = state.clone();
        let held = held.clone();
        let replacement = replacement.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                if fs::rename(&state, &held).is_ok() {
                    if fs::rename(&replacement, &state).is_ok() {
                        std::thread::yield_now();
                        let _ = fs::rename(&state, &replacement);
                    }
                    let _ = fs::rename(&held, &state);
                }
            }
        })
    };
    let saver = {
        let state = state.clone();
        std::thread::spawn(move || {
            (0..300)
                .filter(|i| store::save(&state, &snap(&format!("scan-win-{i}"), *i)).is_ok())
                .count()
        })
    };
    let saved = saver.join().unwrap();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    swapper.join().unwrap();
    for dir in [&state, &held, &replacement] {
        if dir.is_dir() {
            for entry in fs::read_dir(dir).unwrap() {
                let name = entry.unwrap().file_name().to_string_lossy().into_owned();
                assert!(
                    name.starts_with("scan-win-") || name.ends_with(".tmp"),
                    "{name}"
                );
            }
        }
    }
    // Racing creation may leave either owned directory under any of these
    // names; every decoy sentinel must survive somewhere, unchanged.
    for i in 0..300 {
        let name = format!("scan-win-{i}.json");
        assert!(
            [&state, &held, &replacement]
                .iter()
                .any(|dir| { fs::read(dir.join(&name)).is_ok_and(|bytes| bytes == b"sentinel") }),
            "lost or replaced sentinel {name}"
        );
    }
    assert!(
        saved > 0,
        "expected at least one save during directory replacement"
    );
}

#[cfg(unix)]
#[test]
fn parent_replacement_never_redirects_pinned_writes_or_reads() {
    // While a saver repeatedly publishes through `state`, another thread
    // swaps the `state` name between the real directory and a symlink to a
    // different directory. The pinned descriptor must make every operation
    // land in whatever directory it opened — never in the swap target — so
    // `evil` must remain empty throughout, and every successful save must be
    // readable back from a real directory under `t.0`.
    use std::os::unix::fs::symlink;
    let t = Temp::new("swap");
    let real = t.state();
    let hold = t.0.join("hold");
    let evil = t.0.join("evil");
    fs::create_dir_all(&real).unwrap();
    fs::create_dir_all(&evil).unwrap();
    // Establish a real publication before racing. Every raced operation may
    // safely refuse a changing path; thread scheduling cannot guarantee one
    // sees a stable directory. This snapshot must survive all replacements.
    store::save(&real, &snap("scan-before", 0)).unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let swapper = {
        let real = real.clone();
        let hold = hold.clone();
        let evil = evil.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                if fs::rename(&real, &hold).is_ok() {
                    if symlink(&evil, &real).is_ok() {
                        std::thread::yield_now();
                        let _ = fs::remove_file(&real); // the symlink itself
                    }
                    let _ = fs::rename(&hold, &real);
                }
            }
        })
    };
    let saver = {
        let real = real.clone();
        std::thread::spawn(move || {
            let mut ok = 0usize;
            for i in 0..300 {
                if store::save(&real, &snap(&format!("scan-s{i}"), i as u64)).is_ok() {
                    ok += 1;
                }
            }
            ok
        })
    };
    let saved = saver.join().unwrap();
    stop.store(true, Ordering::Relaxed);
    swapper.join().unwrap();
    // Restore `state` as a real directory if the swapper left it a link.
    if fs::symlink_metadata(&real)
        .unwrap()
        .file_type()
        .is_symlink()
    {
        fs::remove_file(&real).unwrap();
        let _ = fs::rename(&hold, &real);
    }
    // The decoy directory was never written through.
    assert!(fs::read_dir(&evil).unwrap().next().is_none());
    // Everything published is still readable through a real directory and
    // landed under this temp root, not in `evil`.
    let mut found = 0usize;
    for dir in [&real, &hold] {
        if dir.is_dir() && !fs::symlink_metadata(dir).unwrap().file_type().is_symlink() {
            found += store::history_report(dir).unwrap().snapshots.len();
        }
    }
    assert_eq!(found, saved + 1);
    assert!(
        [&real, &hold].iter().any(|dir| {
            store::history_report(dir).is_ok_and(|history| {
                history
                    .snapshots
                    .iter()
                    .any(|snapshot| snapshot.id == "scan-before")
            })
        }),
        "pre-race publication lost during parent replacement"
    );
    // Stable access must recover after the race, even if all raced saves
    // refused. This checks write & read behavior through production storage.
    store::save(&real, &snap("scan-after-race", 301)).unwrap();
    assert!(
        store::history(&real)
            .unwrap()
            .iter()
            .any(|snapshot| snapshot.id == "scan-after-race")
    );
    // Reads through a swapped-in symlink are refused, never redirected.
    fs::remove_dir_all(&real).unwrap();
    symlink(&evil, &real).unwrap();
    assert!(store::history(&real).is_err());
    assert!(store::save(&real, &snap("scan-after", 1)).is_err());
    fs::remove_file(&real).unwrap();
}

#[cfg(unix)]
#[test]
fn refuses_group_or_other_writable_existing_directory() {
    use std::os::unix::fs::PermissionsExt;
    let t = Temp::new("writeperm");
    let d = t.state();
    fs::create_dir_all(&d).unwrap();
    fs::set_permissions(&d, fs::Permissions::from_mode(0o777)).unwrap();
    let save_err = store::save(&d, &snap("scan-1", 1)).unwrap_err();
    assert_eq!(save_err.kind(), std::io::ErrorKind::PermissionDenied);
    let read_err = store::history(&d).unwrap_err();
    assert_eq!(read_err.kind(), std::io::ErrorKind::PermissionDenied);
    // Permissions were refused, never rewritten.
    assert_eq!(
        fs::metadata(&d).unwrap().permissions().mode() & 0o777,
        0o777
    );
    // And a group/other-writable leaf is still refused when it is the only
    // problem (parent chain fine).
    fs::set_permissions(&d, fs::Permissions::from_mode(0o755)).unwrap();
    store::save(&d, &snap("scan-1", 1)).unwrap();
}

#[test]
fn concurrent_save_publishes_exactly_one_snapshot() {
    let t = Temp::new("race");
    let d = t.state();
    fs::create_dir_all(&d).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let workers: Vec<_> = (0..8)
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
