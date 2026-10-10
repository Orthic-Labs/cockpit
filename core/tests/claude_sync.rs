//! One journey through the Claude account sync: synthetic account folders in
//! the real on-disk layout (see docs/claude-account-switch.md) are merged
//! while Claude is "closed", refused while it is "running", backed up and put
//! back. Nothing here touches a real Claude install.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use pulse_core::claude_sync::{self as sync, SyncError};

fn uuid(n: u64) -> String {
    format!("{n:08x}-0000-4000-8000-{n:012x}")
}

fn record(id: &str, activity: u64, archived: bool, variant: &str) -> String {
    format!(
        "{{\"sessionId\":\"local_{id}\",\"cwd\":\"/work\",\"createdAt\":100,\"lastActivityAt\":{activity},\"isArchived\":{archived},\"title\":\"{variant}\"}}"
    )
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                walk(&path, out);
            } else {
                out.insert(path.clone(), fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, &mut out);
    out
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("local_") || n.starts_with("deleted_"))
        .collect();
    v.sort();
    v
}

#[test]
fn account_sync_journey() {
    let base = std::env::temp_dir().join(format!("pulse-claude-sync-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let root = base.join("Claude");
    let backups = base.join("backups");
    let transcripts = base.join("dot-claude").join("projects").join("t.jsonl");
    fs::create_dir_all(transcripts.parent().unwrap()).unwrap();
    fs::write(&transcripts, "shared transcript").unwrap();

    let (acct_a, acct_b, acct_c) = (uuid(0xa1), uuid(0xb2), uuid(0xc3));
    let (org1, org2) = (uuid(0x01), uuid(0x02));
    let dir = |a: &str, o: &str| root.join("claude-code-sessions").join(a).join(o);
    let (fa, fb, fc) = (
        dir(&acct_a, &org1),
        dir(&acct_b, &org1),
        dir(&acct_c, &org2),
    );
    for d in [&fa, &fb, &fc] {
        fs::create_dir_all(d.join("backlog")).unwrap();
        fs::write(d.join("backlog").join("tasks.json"), "{}").unwrap();
        fs::write(d.join("scheduled-tasks.json"), "{\"scheduledTasks\":[]}").unwrap();
    }
    // Sign-in material sits next to the data; only the active id may be read.
    fs::write(
        root.join("config.json"),
        format!("{{\"oauth:tokenCache\":\"SECRET-TOKEN\",\"lastKnownAccountUuid\":\"{acct_b}\"}}"),
    )
    .unwrap();

    let (n1, u1, a1, d1, r1, c1, s1, s2) = (
        uuid(0x11),
        uuid(0x12),
        uuid(0x13),
        uuid(0x14),
        uuid(0x15),
        uuid(0x16),
        uuid(0x17),
        uuid(0x18),
    );
    let put = |d: &Path, id: &str, body: String| {
        fs::write(d.join(format!("local_{id}.json")), body).unwrap()
    };
    let tomb = |d: &Path, id: &str, ms: u64| {
        fs::write(d.join(format!("deleted_{id}")), ms.to_string()).unwrap()
    };
    for d in [&fa, &fb, &fc] {
        put(d, &s1, record(&s1, 50, false, "same"));
        put(d, &s2, record(&s2, 60, true, "archived everywhere"));
    }
    put(&fa, &n1, record(&n1, 1000, false, "only in A"));
    put(&fa, &u1, record(&u1, 2000, false, "newer in A"));
    put(&fb, &u1, record(&u1, 1000, false, "older in B"));
    put(&fa, &a1, record(&a1, 2500, true, "archived in A"));
    put(&fb, &a1, record(&a1, 1500, false, "live in B"));
    put(&fc, &a1, record(&a1, 1500, false, "live in B"));
    tomb(&fa, &d1, 3000);
    put(&fb, &d1, record(&d1, 1000, false, "deleted in A"));
    put(&fc, &d1, record(&d1, 1000, false, "deleted in A"));
    tomb(&fa, &r1, 1000);
    put(&fb, &r1, record(&r1, 5000, false, "used after deletion"));
    put(&fa, &c1, record(&c1, 4000, false, "x"));
    put(&fb, &c1, record(&c1, 4000, false, "y"));
    // A's index is stale (lists n1), B's lacks a1, C has none.
    fs::write(
        fa.join("archived-sessions.idx"),
        format!("{{\"v\":1,\"archived\":[\"local_{n1}\",\"local_{s2}\"]}}"),
    )
    .unwrap();
    fs::write(
        fb.join("archived-sessions.idx"),
        format!("{{\"v\":1,\"archived\":[\"local_{s2}\"]}}"),
    )
    .unwrap();

    // Accounts, with the active one from config.json.
    let accounts = sync::accounts(&root).unwrap();
    assert_eq!(accounts.len(), 3);
    assert!(accounts.iter().find(|a| a.id == acct_b).unwrap().active);
    assert!(!accounts.iter().find(|a| a.id == acct_a).unwrap().active);

    // Dry run: the full plan, and not one byte written.
    let before = snapshot(&base);
    let plan = sync::plan(&root).unwrap();
    assert_eq!(snapshot(&base), before);
    assert!(plan.blockers.is_empty());
    assert_eq!(plan.totals.sessions, 8);
    assert_eq!(
        (plan.totals.add, plan.totals.update, plan.totals.delete),
        (5, 3, 2)
    );
    assert_eq!(
        (
            plan.totals.deletion_markers,
            plan.totals.indexes,
            plan.totals.files
        ),
        (3, 3, 16)
    );
    assert_eq!(plan.conflicts.len(), 1);
    assert_eq!(plan.conflicts[0].session_id, format!("local_{c1}"));
    let of = |a: &str| plan.folders.iter().find(|f| f.account == a).unwrap();
    assert_eq!(of(&acct_a).add, vec![format!("local_{r1}")]);
    assert_eq!(
        of(&acct_a).deletion_markers_removed,
        vec![format!("local_{r1}")]
    );
    assert_eq!(of(&acct_b).delete, vec![format!("local_{d1}")]);
    assert_eq!(of(&acct_c).add.len(), 3);
    let json = serde_json::to_string(&plan).unwrap();
    assert!(
        !json.contains("SECRET"),
        "sign-in material must never appear in a plan"
    );

    // Refused while Claude is running: nothing written, no backup.
    let running = || true;
    assert!(matches!(
        sync::apply(&root, &backups, &running, 1_000),
        Err(SyncError::Running)
    ));
    assert_eq!(snapshot(&base), before);
    assert!(!backups.exists());

    // Eleven older backups exist; applying keeps the newest ten.
    fs::create_dir_all(&backups).unwrap();
    for i in 0..11u64 {
        let ts = format!("20260101-0000{i:02}-000");
        let d = backups.join(&ts);
        fs::create_dir_all(&d).unwrap();
        fs::write(
            d.join("manifest.json"),
            format!("{{\"version\":1,\"ts\":\"{ts}\",\"created_ms\":{i},\"root\":\"x\",\"status\":\"applied\",\"ops\":[]}}"),
        )
        .unwrap();
    }

    // Apply while "closed".
    let closed = || false;
    let applied = sync::apply(&root, &backups, &closed, 1_791_000_000_000).unwrap();
    let ts = applied.backup.clone().expect("a backup was taken");
    assert_eq!(applied.files_changed, 16);
    assert_eq!(sync::backups(&backups).len(), sync::KEEP_BACKUPS);
    assert_eq!(sync::backups(&backups)[0].ts, ts);

    let l = |id: &str| format!("local_{id}.json");
    let expect_a = [&n1, &u1, &a1, &c1, &s1, &s2, &r1].map(|i| l(i));
    let mut want_a: Vec<String> = expect_a.to_vec();
    want_a.push(format!("deleted_{d1}"));
    want_a.sort();
    assert_eq!(names(&fa), want_a);
    let mut want_b: Vec<String> = [&n1, &u1, &a1, &r1, &c1, &s1, &s2].map(|i| l(i)).to_vec();
    want_b.push(format!("deleted_{d1}"));
    want_b.sort();
    assert_eq!(names(&fb), want_b);
    let mut want_c: Vec<String> = [&n1, &u1, &a1, &r1, &s1, &s2].map(|i| l(i)).to_vec();
    want_c.push(format!("deleted_{d1}"));
    want_c.sort();
    assert_eq!(names(&fc), want_c);
    // Newest record wins; deletion and archive state propagated.
    assert_eq!(
        fs::read(fb.join(l(&u1))).unwrap(),
        fs::read(fa.join(l(&u1))).unwrap()
    );
    assert_eq!(
        fs::read(fc.join(l(&a1))).unwrap(),
        fs::read(fa.join(l(&a1))).unwrap()
    );
    assert_eq!(
        fs::read_to_string(fb.join(format!("deleted_{d1}"))).unwrap(),
        "3000"
    );
    // The tie is kept as it was in both places, and never given to C.
    assert!(
        fs::read_to_string(fa.join(l(&c1)))
            .unwrap()
            .contains("\"x\"")
    );
    assert!(
        fs::read_to_string(fb.join(l(&c1)))
            .unwrap()
            .contains("\"y\"")
    );
    // The index matches the merged records in every folder, byte for byte.
    let mut archived = [format!("local_{a1}"), format!("local_{s2}")];
    archived.sort();
    let idx = format!(
        "{{\"v\":1,\"archived\":[{}]}}",
        archived
            .iter()
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(",")
    );
    for d in [&fa, &fb, &fc] {
        assert_eq!(
            fs::read_to_string(d.join("archived-sessions.idx")).unwrap(),
            idx
        );
        assert_eq!(
            fs::read_to_string(d.join("scheduled-tasks.json")).unwrap(),
            "{\"scheduledTasks\":[]}"
        );
        assert_eq!(
            fs::read_to_string(d.join("backlog").join("tasks.json")).unwrap(),
            "{}"
        );
    }
    assert_eq!(
        fs::read_to_string(&transcripts).unwrap(),
        "shared transcript"
    );
    assert!(
        fs::read_to_string(root.join("config.json"))
            .unwrap()
            .contains("SECRET-TOKEN")
    );
    // Merged: a second pass has nothing to do but still reports the tie.
    let again = sync::plan(&root).unwrap();
    assert_eq!(again.totals.files, 0);
    assert_eq!(again.conflicts.len(), 1);

    // A folder replaced by a symlink blocks the apply instead of being written through.
    #[cfg(unix)]
    {
        let acct_d = uuid(0xd4);
        let link_parent = root.join("claude-code-sessions").join(&acct_d);
        fs::create_dir_all(&link_parent).unwrap();
        std::os::unix::fs::symlink(&fa, link_parent.join(&org1)).unwrap();
        let blocked = sync::plan(&root).unwrap();
        assert!(!blocked.blockers.is_empty());
        assert!(matches!(
            sync::apply(&root, &backups, &closed, 1_791_000_000_500),
            Err(SyncError::Blocked(_))
        ));
        fs::remove_dir_all(&link_parent).unwrap();
    }

    // Restore after a tampered file refuses and writes nothing; --force overrides.
    let after = snapshot(&root);
    fs::write(fb.join(l(&u1)), "changed by Claude later").unwrap();
    let tampered = snapshot(&root);
    assert!(matches!(
        sync::restore(&backups, &ts, false, &closed),
        Err(SyncError::RestoreMismatch(_))
    ));
    assert_eq!(snapshot(&root), tampered);
    assert!(matches!(
        sync::restore(&backups, &ts, false, &running),
        Err(SyncError::Running)
    ));
    // Put the file back the way the sync left it, then undo cleanly.
    fs::write(fb.join(l(&u1)), fs::read(fa.join(l(&u1))).unwrap()).unwrap();
    assert_eq!(snapshot(&root), after);
    let restored = sync::restore(&backups, &ts, false, &closed).unwrap();
    assert_eq!(restored.restored, 16);
    // Every byte and every file is back as before the sync (the fixture's
    // transcripts and the backups themselves are outside the Claude folder).
    let original: BTreeMap<_, _> = before
        .iter()
        .filter(|(p, _)| p.starts_with(&root))
        .collect();
    let now = snapshot(&root);
    assert_eq!(now.iter().collect::<BTreeMap<_, _>>(), original);
    assert_eq!(
        sync::restore(&backups, &ts, false, &closed)
            .unwrap()
            .restored,
        0
    );
    assert_eq!(sync::backups(&backups)[0].status, "restored");

    // Sync again, tamper, force-restore.
    let second = sync::apply(&root, &backups, &closed, 1_791_000_001_000).unwrap();
    let ts2 = second.backup.unwrap();
    fs::write(fc.join(l(&n1)), "edited after sync").unwrap();
    assert!(sync::restore(&backups, &ts2, false, &closed).is_err());
    assert!(sync::restore(&backups, &ts2, true, &closed).is_ok());
    assert_eq!(now, snapshot(&root));

    // Discovery: the first scan only records what exists; a newly signed-in
    // account (and an agent-mode-only one) is found, included by default, and
    // gets the full merged list on the next sync while Claude is closed.
    let registry_file = base.join("Pulse").join("claude-accounts.json");
    let first = sync::discover(&root, &registry_file, 10).unwrap();
    assert!(first.new_accounts.is_empty());
    assert_eq!(first.registry.accounts.len(), 3);
    let acct_e = uuid(0xe5);
    let fe = dir(&acct_e, &org1);
    fs::create_dir_all(&fe).unwrap();
    let acct_f = uuid(0xf6);
    fs::create_dir_all(root.join("local-agent-mode-sessions").join(&acct_f)).unwrap();
    let found = sync::discover(&root, &registry_file, 20).unwrap();
    assert_eq!(found.new_accounts, vec![acct_e.clone(), acct_f.clone()]);
    // The registry never gates the sync: an empty one still includes everything on disk.
    assert!(
        sync::Registry::default()
            .sync_set(&sync::accounts(&root).unwrap())
            .contains(&acct_e)
    );
    assert!(found.registry.accounts.iter().all(|a| a.included));
    assert!(
        sync::discover(&root, &registry_file, 30)
            .unwrap()
            .new_accounts
            .is_empty()
    );
    // Excluded accounts are left out of the merge.
    sync::set_included(&registry_file, &acct_e, false).unwrap();
    let excluded =
        sync::auto_sync(&root, &backups, &registry_file, &closed, 1_791_000_002_000).unwrap();
    assert_eq!(excluded.synced.as_ref().unwrap().files_changed, 16);
    assert!(names(&fe).is_empty());
    // While Claude runs, nothing is read or synced.
    sync::set_included(&registry_file, &acct_e, true).unwrap();
    let waiting =
        sync::auto_sync(&root, &backups, &registry_file, &running, 1_791_000_003_000).unwrap();
    assert!(waiting.claude_running && waiting.synced.is_none());
    assert!(names(&fe).is_empty());
    // Closed: the new account receives every merged session (not the tie).
    let joined =
        sync::auto_sync(&root, &backups, &registry_file, &closed, 1_791_000_004_000).unwrap();
    assert_eq!(joined.synced.as_ref().unwrap().files_changed, 8);
    assert_eq!(names(&fe), want_c);
    let last = sync::load_registry(&registry_file).last_sync.unwrap();
    assert_eq!(last.files_changed, 8);
    // Undo from the result card.
    sync::restore(&backups, last.backup.as_deref().unwrap(), false, &closed).unwrap();
    assert!(names(&fe).is_empty());
    let _ = fs::remove_dir_all(&base);
}

fn ranked(id: &str, turns: u64, activity: u64, archived: bool, variant: &str) -> String {
    format!(
        "{{\"sessionId\":\"local_{id}\",\"createdAt\":100,\"lastActivityAt\":{activity},\"completedTurns\":{turns},\"isArchived\":{archived},\"title\":\"{variant}\"}}"
    )
}

/// The one-way mirror while Claude "runs": the signed-in account's folder is
/// only read, other included accounts' folders receive its records, and
/// excluded, folderless and fresher destinations are left alone.
#[test]
fn chat_mirror_journey() {
    let base = std::env::temp_dir().join(format!("pulse-claude-mirror-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let root = base.join("Claude");
    let registry = base.join("Pulse").join("claude-accounts.json");
    let (a, b, c, d, e) = (uuid(0xa1), uuid(0xb2), uuid(0xc3), uuid(0xd4), uuid(0xe5));
    let org = uuid(0x01);
    let dir = |acct: &str| root.join("claude-code-sessions").join(acct).join(&org);
    let (fa, fb, fc, fe) = (dir(&a), dir(&b), dir(&c), dir(&e));
    for f in [&fa, &fb, &fc, &fe] {
        fs::create_dir_all(f).unwrap();
    }
    // D has an account folder but no org folder yet: it gets nothing.
    fs::create_dir_all(root.join("claude-code-sessions").join(&d)).unwrap();
    let config = |active: &str| {
        fs::write(
            root.join("config.json"),
            format!("{{\"lastKnownAccountUuid\":\"{active}\",\"oauth:tokenCache\":\"SECRET\"}}"),
        )
        .unwrap()
    };
    config(&a);
    let (new, up, tie, high, gone, arch) = (
        uuid(0x21),
        uuid(0x22),
        uuid(0x23),
        uuid(0x24),
        uuid(0x25),
        uuid(0x26),
    );
    let put = |f: &Path, id: &str, body: String| {
        fs::write(f.join(format!("local_{id}.json")), body).unwrap()
    };
    put(&fa, &new, ranked(&new, 3, 500, false, "new"));
    put(&fa, &up, ranked(&up, 9, 900, false, "further"));
    put(&fb, &up, ranked(&up, 4, 400, false, "behind"));
    put(&fa, &tie, ranked(&tie, 5, 500, false, "x"));
    put(&fb, &tie, ranked(&tie, 5, 500, false, "y"));
    put(&fa, &high, ranked(&high, 2, 200, false, "behind in A"));
    put(&fb, &high, ranked(&high, 8, 800, true, "ahead in B"));
    fs::write(fa.join(format!("deleted_{gone}")), "3000").unwrap();
    put(&fb, &gone, ranked(&gone, 1, 1000, false, "deleted in A"));
    put(&fa, &arch, ranked(&arch, 1, 100, true, "archived"));
    // B's index is old (and stale); C has none. E is excluded.
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
    fs::write(
        fb.join("archived-sessions.idx"),
        "{\"v\":1,\"archived\":[]}",
    )
    .unwrap();
    fs::File::options()
        .write(true)
        .open(fb.join("archived-sessions.idx"))
        .unwrap()
        .set_modified(old)
        .unwrap();
    sync::discover(&root, &registry, 10).unwrap();
    sync::set_included(&registry, &e, false).unwrap();

    let before = snapshot(&root);
    let first = sync::mirror(&root, &registry).unwrap();
    assert_eq!(first.active.as_deref(), Some(a.as_str()));
    assert!(first.aborted.is_none());
    // B: new, up, arch, and gone removed (marker written). C: all but the deleted one.
    assert_eq!((first.folders, first.copied, first.skipped), (2, 9, 1));
    let after = snapshot(&root);
    // The signed-in account's folder, the excluded one and the folderless one never change.
    for (path, bytes) in &before {
        if path.starts_with(&fa) || path.starts_with(&fe) {
            assert_eq!(after.get(path), Some(bytes));
        }
    }
    assert_eq!(names(&fe), Vec::<String>::new());
    assert!(
        fs::read_dir(root.join("claude-code-sessions").join(&d))
            .unwrap()
            .next()
            .is_none()
    );
    let l = |id: &str| format!("local_{id}.json");
    assert_eq!(
        fs::read(fb.join(l(&new))).unwrap(),
        fs::read(fa.join(l(&new))).unwrap()
    );
    assert_eq!(
        fs::read(fb.join(l(&up))).unwrap(),
        fs::read(fa.join(l(&up))).unwrap()
    );
    assert!(
        fs::read_to_string(fb.join(l(&tie)))
            .unwrap()
            .contains("\"y\"")
    );
    assert!(
        fs::read_to_string(fb.join(l(&high)))
            .unwrap()
            .contains("ahead in B")
    );
    assert!(!fb.join(l(&gone)).exists());
    assert_eq!(
        fs::read_to_string(fb.join(format!("deleted_{gone}"))).unwrap(),
        "3000"
    );
    assert!(
        fs::read_to_string(fc.join(l(&tie)))
            .unwrap()
            .contains("\"x\"")
    );
    assert!(fc.join(l(&arch)).exists());
    // C was empty, so it gets every live record; its index lists the archived one.
    assert_eq!(
        fs::read_to_string(fc.join("archived-sessions.idx")).unwrap(),
        format!("{{\"v\":1,\"archived\":[\"local_{arch}\"]}}")
    );
    // B keeps its own archived record in its rebuilt index.
    let mut want = [format!("local_{arch}"), format!("local_{high}")];
    want.sort();
    assert_eq!(
        fs::read_to_string(fb.join("archived-sessions.idx")).unwrap(),
        format!("{{\"v\":1,\"archived\":[\"{}\",\"{}\"]}}", want[0], want[1])
    );
    assert_eq!(
        fs::read_to_string(root.join("config.json"))
            .unwrap()
            .matches("SECRET")
            .count(),
        1
    );

    // Nothing changed: the pass does no writes (the tie is still skipped).
    let settled = snapshot(&root);
    let second = sync::mirror(&root, &registry).unwrap();
    assert_eq!((second.copied, second.folders, second.skipped), (0, 0, 1));
    assert_eq!(snapshot(&root), settled);

    // Signed out: no pass. Switched account: the pass copies from the new one.
    fs::write(root.join("config.json"), "{}").unwrap();
    let signed_out = snapshot(&root);
    let none = sync::mirror(&root, &registry).unwrap();
    assert_eq!(none.aborted.as_deref(), Some("no signed-in account"));
    assert_eq!(snapshot(&root), signed_out);
    config(&b);
    let back = sync::mirror(&root, &registry).unwrap();
    assert_eq!(back.active.as_deref(), Some(b.as_str()));
    assert!(back.copied > 0);
    assert!(fa.join(l(&high)).exists());
    let _ = fs::remove_dir_all(&base);
}
