//! Static-provider safety tests for the metadata-only scanner. No real
//! filesystem access: every path below is synthetic.

use cockpit_core::{
    EntryKind, FileIdentity, FileMetadata, FilesystemProvider, FsError, ScanOptions, SnapshotState,
    VolumeIdentity, VolumeUsage, scan_with_provider,
};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Default)]
struct Mock {
    meta: BTreeMap<PathBuf, Result<FileMetadata, FsError>>,
    kids: BTreeMap<PathBuf, Vec<PathBuf>>,
    enumerated: RefCell<Vec<PathBuf>>,
    inspected: RefCell<Vec<PathBuf>>,
}

fn p(s: &str) -> PathBuf {
    PathBuf::from(s)
}

fn vol(id: &str) -> VolumeIdentity {
    VolumeIdentity::new(id)
}

fn dir(v: &str) -> FileMetadata {
    FileMetadata {
        kind: EntryKind::Directory,
        volume: vol(v),
        logical_size: Some(0),
        allocation_size: Some(0),
        file_id: None,
        clone_id: None,
        is_placeholder: false,
        metadata_complete: true,
    }
}

fn file(v: &str, id: &str, size: u64) -> FileMetadata {
    FileMetadata {
        kind: EntryKind::File,
        volume: vol(v),
        logical_size: Some(size),
        allocation_size: Some(size),
        file_id: Some(FileIdentity {
            volume: vol(v),
            id: id.into(),
        }),
        clone_id: None,
        is_placeholder: false,
        metadata_complete: true,
    }
}

impl Mock {
    fn add(&mut self, path: &str, meta: FileMetadata) {
        self.meta.insert(p(path), Ok(meta));
    }
    fn kids(&mut self, path: &str, kids: &[&str]) {
        self.kids
            .insert(p(path), kids.iter().map(|k| p(k)).collect());
    }
}

impl FilesystemProvider for Mock {
    fn inspect(&self, path: &Path) -> Result<FileMetadata, FsError> {
        self.inspected.borrow_mut().push(path.to_path_buf());
        match self.meta.get(path) {
            Some(r) => r.clone(),
            None => Err(FsError::new("missing")),
        }
    }
    fn children(&self, path: &Path) -> Result<Vec<PathBuf>, FsError> {
        self.enumerated.borrow_mut().push(path.to_path_buf());
        Ok(self.kids.get(path).cloned().unwrap_or_default())
    }
    fn volume_usage(&self, volume: &VolumeIdentity) -> Result<VolumeUsage, FsError> {
        Ok(VolumeUsage {
            volume: volume.clone(),
            total_bytes: None,
            used_bytes: None,
            available_bytes: None,
            purgeable_bytes: None,
            snapshots: SnapshotState::Unknown,
        })
    }
}

fn has(reasons: &[String], needle: &str) -> bool {
    reasons.iter().any(|r| r.contains(needle))
}

#[test]
fn denied_ancestor_blocks_root_and_is_not_reported_as_symlink() {
    let mut m = Mock::default();
    m.add("/", dir("a"));
    m.meta
        .insert(p("/x"), Err(FsError::permission_denied("denied")));
    m.add("/x/y", dir("a"));
    let r = scan_with_provider(&m, &[p("/x/y")], &ScanOptions::default());
    assert!(r.entries.is_empty());
    assert!(r.accounting.incomplete);
    assert_eq!(r.inspection_errors.len(), 1);
    assert!(r.skipped_links.is_empty());
    assert!(has(&r.incomplete_reasons, "ancestor not inspectable"));
    assert!(m.enumerated.borrow().is_empty());
    assert_eq!(r.accounting.reclaim.upper_bytes, None);
}

#[test]
fn placeholder_ancestor_blocks_root() {
    let mut m = Mock::default();
    m.add("/", dir("a"));
    let mut ph = dir("a");
    ph.is_placeholder = true;
    m.add("/x", ph);
    m.add("/x/f", file("a", "1", 10));
    let r = scan_with_provider(&m, &[p("/x/f")], &ScanOptions::default());
    assert!(r.entries.is_empty());
    assert_eq!(r.skipped_links.len(), 1);
    assert!(r.accounting.incomplete);
}

#[test]
fn cross_volume_descendant_is_rejected_explicitly() {
    let mut m = Mock::default();
    m.add("/", dir("a"));
    m.add("/r", dir("a"));
    m.add("/r/mnt", dir("b"));
    m.add("/r/mnt/f", file("b", "9", 100));
    m.add("/r/ok", file("a", "1", 5));
    m.kids("/r", &["/r/mnt", "/r/ok"]);
    m.kids("/r/mnt", &["/r/mnt/f"]);
    let r = scan_with_provider(&m, &[p("/r")], &ScanOptions::default());
    assert!(r.entries.iter().all(|e| e.metadata.volume == vol("a")));
    assert!(has(&r.incomplete_reasons, "cross-volume"));
    assert!(r.accounting.incomplete);
    assert_eq!(r.accounting.reclaim.upper_bytes, None);
    assert!(!m.enumerated.borrow().contains(&p("/r/mnt")));
    assert_eq!(r.accounting.attributed_allocation_bytes, 5);
}

#[test]
fn depth_limit_is_explicit() {
    let mut m = Mock::default();
    m.add("/", dir("a"));
    m.add("/r", dir("a"));
    m.add("/r/d", dir("a"));
    m.add("/r/d/f", file("a", "1", 1));
    m.kids("/r", &["/r/d"]);
    m.kids("/r/d", &["/r/d/f"]);
    let o = ScanOptions {
        max_depth: 1,
        ..Default::default()
    };
    let r = scan_with_provider(&m, &[p("/r")], &o);
    assert_eq!(r.entries.len(), 2);
    assert!(has(&r.incomplete_reasons, "depth limit"));
    assert!(r.accounting.incomplete);
    assert!(!m.enumerated.borrow().contains(&p("/r/d")));
}

#[test]
fn entry_limit_is_explicit_and_deterministic() {
    let mut m = Mock::default();
    m.add("/", dir("a"));
    m.add("/r", dir("a"));
    let names = ["/r/c", "/r/a", "/r/d", "/r/b"];
    for (i, n) in names.iter().enumerate() {
        m.add(n, file("a", &i.to_string(), 1));
    }
    m.kids("/r", &names);
    let o = ScanOptions {
        max_entries: 3,
        ..Default::default()
    };
    let r = scan_with_provider(&m, &[p("/r")], &o);
    assert_eq!(r.entries.len(), 3);
    let paths: Vec<_> = r.entries.iter().map(|e| e.path.clone()).collect();
    assert_eq!(paths, vec![p("/r"), p("/r/a"), p("/r/b")]);
    assert!(has(&r.incomplete_reasons, "entry limit"));
    assert!(r.accounting.incomplete);
}

#[test]
fn entry_limit_applies_across_roots() {
    let mut m = Mock::default();
    m.add("/", dir("a"));
    m.add("/a", file("a", "1", 1));
    m.add("/b", file("a", "2", 1));
    let o = ScanOptions {
        max_entries: 1,
        ..Default::default()
    };
    let r = scan_with_provider(&m, &[p("/b"), p("/a")], &o);
    assert_eq!(r.entries.len(), 1);
    assert!(has(&r.incomplete_reasons, "entry limit"));
}

#[test]
fn placeholder_file_rejected_by_default() {
    let mut m = Mock::default();
    m.add("/", dir("a"));
    m.add("/r", dir("a"));
    let mut f = file("a", "1", 1000);
    f.is_placeholder = true;
    m.add("/r/cloud", f);
    m.kids("/r", &["/r/cloud"]);
    let r = scan_with_provider(&m, &[p("/r")], &ScanOptions::default());
    assert_eq!(r.entries.len(), 1);
    assert!(has(&r.incomplete_reasons, "placeholder rejected"));
    assert_eq!(r.accounting.attributed_allocation_bytes, 0);
    assert_eq!(r.accounting.reclaim.upper_bytes, None);
}

#[test]
fn placeholder_directory_is_never_enumerated_even_when_allowed() {
    let mut m = Mock::default();
    m.add("/", dir("a"));
    let mut d = dir("a");
    d.is_placeholder = true;
    m.add("/cloud", d);
    m.add("/cloud/f", file("a", "1", 1));
    m.kids("/cloud", &["/cloud/f"]);
    let o = ScanOptions {
        reject_placeholders: false,
        ..Default::default()
    };
    let r = scan_with_provider(&m, &[p("/cloud")], &o);
    assert!(m.enumerated.borrow().is_empty());
    assert!(has(&r.incomplete_reasons, "placeholder directory"));
    assert!(r.accounting.incomplete);
}

#[test]
fn overlapping_roots_count_once() {
    let mut m = Mock::default();
    m.add("/", dir("a"));
    m.add("/r", dir("a"));
    m.add("/r/s", dir("a"));
    m.add("/r/s/f", file("a", "1", 7));
    m.kids("/r", &["/r/s"]);
    m.kids("/r/s", &["/r/s/f"]);
    let r = scan_with_provider(
        &m,
        &[p("/r/s"), p("/r"), p("/r/s/f"), p("/r")],
        &ScanOptions::default(),
    );
    assert_eq!(r.entries.len(), 3);
    assert_eq!(r.accounting.attributed_allocation_bytes, 7);
    assert_eq!(r.accounting.logical_bytes, 7);
    assert_eq!(r.roots.len(), 3);
}

#[test]
fn duplicate_children_are_visited_once() {
    let mut m = Mock::default();
    m.add("/", dir("a"));
    m.add("/r", dir("a"));
    m.add("/r/f", file("a", "1", 4));
    m.kids("/r", &["/r/f", "/r/f", "/r/f"]);
    let r = scan_with_provider(&m, &[p("/r")], &ScanOptions::default());
    assert_eq!(r.entries.len(), 2);
    assert_eq!(r.accounting.attributed_allocation_bytes, 4);
}

#[test]
fn hard_links_dedupe_within_volume_but_not_across_volumes() {
    let mut m = Mock::default();
    m.add("/", dir("a"));
    m.add("/r", dir("a"));
    m.add("/r/one", file("a", "5", 10));
    m.add("/r/two", file("a", "5", 10));
    m.kids("/r", &["/r/one", "/r/two"]);
    // Same inode number on another volume is a different identity.
    m.add("/s", dir("b"));
    m.add("/s/x", file("b", "5", 10));
    m.kids("/s", &["/s/x"]);
    let r = scan_with_provider(&m, &[p("/r"), p("/s")], &ScanOptions::default());
    assert_eq!(r.accounting.attributed_allocation_bytes, 20);
    let two = r.entries.iter().find(|e| e.path == p("/r/two")).unwrap();
    assert_eq!(two.attributed_allocation_bytes, 0);
    assert_eq!(two.accounting_owner, Some(p("/r/one")));
    let x = r.entries.iter().find(|e| e.path == p("/s/x")).unwrap();
    assert_eq!(x.attributed_allocation_bytes, 10);
}

#[test]
fn cross_volume_hard_link_alias_is_not_counted() {
    let mut m = Mock::default();
    m.add("/", dir("a"));
    m.add("/r", dir("a"));
    m.add("/r/one", file("a", "5", 10));
    // Provider claims a volume-b entry under a volume-a directory.
    m.add("/r/alias", file("b", "5", 10));
    m.kids("/r", &["/r/one", "/r/alias"]);
    let r = scan_with_provider(&m, &[p("/r")], &ScanOptions::default());
    assert_eq!(r.accounting.attributed_allocation_bytes, 10);
    assert!(has(&r.incomplete_reasons, "cross-volume"));
}

#[test]
fn missing_metadata_is_incomplete_even_if_provider_claims_complete() {
    let mut m = Mock::default();
    m.add("/", dir("a"));
    m.add("/r", dir("a"));
    let mut f = file("a", "1", 10);
    f.allocation_size = None; // metadata_complete deliberately left true
    m.add("/r/f", f);
    let mut g = file("a", "2", 10);
    g.file_id = None;
    m.add("/r/g", g);
    m.kids("/r", &["/r/f", "/r/g"]);
    let r = scan_with_provider(&m, &[p("/r")], &ScanOptions::default());
    assert!(has(&r.incomplete_reasons, "incomplete metadata: /r/f"));
    assert!(has(&r.incomplete_reasons, "incomplete metadata: /r/g"));
    assert!(r.accounting.incomplete);
    assert_eq!(r.accounting.reclaim.upper_bytes, None);
    for e in r
        .entries
        .iter()
        .filter(|e| e.metadata.kind == EntryKind::File)
    {
        assert!(!e.metadata.metadata_complete);
        let rc = e.reclaim.as_ref().unwrap();
        assert_eq!(rc.lower_bytes, 0);
        assert!(matches!(rc.state, cockpit_core::ReclaimState::Unknown));
    }
}

#[test]
fn inspect_failure_is_recorded_and_incomplete() {
    let mut m = Mock::default();
    m.add("/", dir("a"));
    m.add("/r", dir("a"));
    m.kids("/r", &["/r/gone"]);
    let r = scan_with_provider(&m, &[p("/r")], &ScanOptions::default());
    assert_eq!(r.inspection_errors.len(), 1);
    assert!(r.accounting.incomplete);
}

#[test]
fn revisited_directory_identity_is_explicit() {
    let mut m = Mock::default();
    m.add("/", dir("a"));
    m.add("/r", dir("a"));
    let mut d = dir("a");
    d.file_id = Some(FileIdentity {
        volume: vol("a"),
        id: "d1".into(),
    });
    m.add("/r/x", d.clone());
    m.add("/r/y", d);
    m.kids("/r", &["/r/x", "/r/y"]);
    let r = scan_with_provider(&m, &[p("/r")], &ScanOptions::default());
    assert!(has(
        &r.incomplete_reasons,
        "directory identity already visited"
    ));
    assert!(r.accounting.incomplete);
}

#[test]
fn scanning_never_marks_anything_cleanup_eligible() {
    let mut m = Mock::default();
    m.add("/", dir("a"));
    m.add("/f", file("a", "1", 10));
    let r = scan_with_provider(&m, &[p("/f")], &ScanOptions::default());
    for e in &r.entries {
        if let Some(rc) = &e.reclaim {
            assert_eq!(rc.lower_bytes, 0);
            assert!(matches!(rc.state, cockpit_core::ReclaimState::Unknown));
        }
    }
}
