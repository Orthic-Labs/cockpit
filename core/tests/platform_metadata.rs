//! Native metadata adapter tests: synthetic-provider fixtures (no filesystem)
//! plus cfg-gated real-filesystem checks under a unique temp directory.

use cockpit_core::{
    EntryKind, FileIdentity, FileMetadata, FilesystemProvider, FsError, ScanOptions, SnapshotState,
    StdFilesystemProvider, VolumeIdentity, VolumeUsage, scan_with_provider,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Default)]
struct Synthetic {
    meta: BTreeMap<PathBuf, (FileMetadata, Vec<String>)>,
    kids: BTreeMap<PathBuf, Vec<PathBuf>>,
}

fn p(s: &str) -> PathBuf {
    PathBuf::from(s)
}

fn meta(kind: EntryKind, volume: &str, id: Option<&str>, size: Option<u64>) -> FileMetadata {
    let v = VolumeIdentity::new(volume);
    FileMetadata {
        kind,
        volume: v.clone(),
        logical_size: size.map(|_| size.unwrap_or(0)),
        allocation_size: size,
        file_id: id.map(|id| FileIdentity {
            volume: v.clone(),
            id: id.into(),
        }),
        clone_id: None,
        is_placeholder: false,
        metadata_complete: true,
    }
}

impl Synthetic {
    fn add(&mut self, path: &str, metadata: FileMetadata, reasons: &[&str]) {
        self.meta.insert(
            p(path),
            (metadata, reasons.iter().map(|r| r.to_string()).collect()),
        );
    }
}

impl FilesystemProvider for Synthetic {
    fn inspect(&self, path: &Path) -> Result<FileMetadata, FsError> {
        self.meta
            .get(path)
            .map(|(m, _)| m.clone())
            .ok_or_else(|| FsError::new("missing"))
    }
    fn inspect_detailed(&self, path: &Path) -> Result<(FileMetadata, Vec<String>), FsError> {
        self.meta
            .get(path)
            .cloned()
            .ok_or_else(|| FsError::new("missing"))
    }
    fn children(&self, path: &Path) -> Result<Vec<PathBuf>, FsError> {
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
fn same_inode_text_on_different_volumes_is_not_a_hard_link() {
    // Two volumes each hand out file id "7"; they are different files.
    let mut s = Synthetic::default();
    s.add(
        "/",
        meta(EntryKind::Directory, "uuid-a", Some("1"), Some(0)),
        &[],
    );
    s.add(
        "/r",
        meta(EntryKind::Directory, "uuid-a", Some("2"), Some(0)),
        &[],
    );
    s.add(
        "/r/a",
        meta(EntryKind::File, "uuid-a", Some("7"), Some(100)),
        &[],
    );
    s.add(
        "/r/a2",
        meta(EntryKind::File, "uuid-a", Some("7"), Some(100)),
        &[],
    );
    s.add(
        "/r/b",
        meta(EntryKind::File, "uuid-b", Some("7"), Some(100)),
        &[],
    );
    s.kids
        .insert(p("/r"), vec![p("/r/a"), p("/r/a2"), p("/r/b")]);
    let report = scan_with_provider(&s, &[p("/r")], &ScanOptions::default());
    // Same-volume alias counted once; cross-volume entry rejected explicitly,
    // never merged into the hard-link owner.
    assert_eq!(report.accounting.attributed_allocation_bytes, 100);
    assert!(has(&report.incomplete_reasons, "cross-volume"));
    assert!(report.accounting.incomplete);
    let alias = report
        .entries
        .iter()
        .find(|e| e.path == p("/r/a2"))
        .unwrap();
    assert_eq!(alias.accounting_owner, Some(p("/r/a")));
    assert_eq!(alias.attributed_allocation_bytes, 0);
}

#[test]
fn missing_native_fields_are_reported_with_reasons() {
    let mut s = Synthetic::default();
    s.add(
        "/",
        meta(EntryKind::Directory, "v", Some("1"), Some(0)),
        &[],
    );
    s.add(
        "/r",
        meta(EntryKind::Directory, "v", Some("2"), Some(0)),
        &[],
    );
    let mut f = meta(EntryKind::File, "v", None, None);
    f.logical_size = Some(10);
    f.metadata_complete = false;
    s.add(
        "/r/f",
        f,
        &["file id unavailable (FILE_ID_INFO unsupported or failed)"],
    );
    s.kids.insert(p("/r"), vec![p("/r/f")]);
    let report = scan_with_provider(&s, &[p("/r")], &ScanOptions::default());
    assert!(has(&report.incomplete_reasons, "incomplete metadata: /r/f"));
    assert!(has(&report.incomplete_reasons, "FILE_ID_INFO"));
    assert!(report.accounting.incomplete);
    // No guessed allocation or upper bound.
    assert_eq!(report.accounting.attributed_allocation_bytes, 0);
    assert_eq!(report.accounting.reclaim.upper_bytes, None);
}

#[test]
fn missing_fields_without_provider_reasons_get_generic_reasons() {
    let mut s = Synthetic::default();
    s.add(
        "/",
        meta(EntryKind::Directory, "v", Some("1"), Some(0)),
        &[],
    );
    s.add("/f", meta(EntryKind::File, "v", None, None), &[]);
    let report = scan_with_provider(&s, &[p("/f")], &ScanOptions::default());
    assert!(has(
        &report.incomplete_reasons,
        "allocation size unavailable"
    ));
    assert!(has(&report.incomplete_reasons, "file id unavailable"));
}

#[test]
fn placeholder_is_refused_and_never_enumerated_or_counted() {
    let mut s = Synthetic::default();
    s.add(
        "/",
        meta(EntryKind::Directory, "v", Some("1"), Some(0)),
        &[],
    );
    s.add(
        "/r",
        meta(EntryKind::Directory, "v", Some("2"), Some(0)),
        &[],
    );
    let mut f = meta(EntryKind::File, "v", Some("3"), None);
    f.is_placeholder = true;
    f.logical_size = None;
    f.metadata_complete = false;
    s.add("/r/cloud", f, &["allocation unavailable: placeholder"]);
    let mut d = meta(EntryKind::Directory, "v", Some("4"), Some(0));
    d.is_placeholder = true;
    s.add("/r/clouddir", d, &[]);
    s.add(
        "/r/clouddir/x",
        meta(EntryKind::File, "v", Some("5"), Some(9)),
        &[],
    );
    s.kids
        .insert(p("/r"), vec![p("/r/cloud"), p("/r/clouddir")]);
    s.kids.insert(p("/r/clouddir"), vec![p("/r/clouddir/x")]);
    let report = scan_with_provider(&s, &[p("/r")], &ScanOptions::default());
    assert!(has(
        &report.incomplete_reasons,
        "placeholder rejected: /r/cloud"
    ));
    assert!(report.entries.iter().all(|e| e.path != p("/r/cloud")));
    assert!(report.entries.iter().all(|e| e.path != p("/r/clouddir/x")));
    assert_eq!(report.accounting.attributed_allocation_bytes, 0);
    assert!(report.accounting.incomplete);
}

// ---- real filesystem (unique temp dir, removed on drop) ----

#[cfg(any(unix, windows))]
mod real {
    use super::*;
    use std::fs;

    struct Temp(PathBuf);
    impl Temp {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "cockpit-platform-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            fs::create_dir(&root).unwrap();
            Self(root)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn hard_link_shares_file_id_and_directory_has_one() {
        let temp = Temp::new("ids");
        let a = temp.0.join("a");
        fs::write(&a, b"x").unwrap();
        let b = temp.0.join("b");
        fs::hard_link(&a, &b).unwrap();
        let provider = StdFilesystemProvider;
        let ma = provider.inspect(&a).unwrap();
        let mb = provider.inspect(&b).unwrap();
        assert!(ma.file_id.is_some());
        assert_eq!(ma.file_id, mb.file_id);
        let md = provider.inspect(&temp.0).unwrap();
        assert_eq!(md.kind, EntryKind::Directory);
        assert!(md.file_id.is_some());
        assert_ne!(md.file_id, ma.file_id);
        assert_eq!(ma.volume, md.volume);
        assert!(ma.allocation_size.is_some());
    }

    #[test]
    fn bounded_enumeration_stops_and_reports_truncation() {
        let temp = Temp::new("bounded");
        for i in 0..6 {
            fs::write(temp.0.join(format!("f{i}")), b"x").unwrap();
        }
        let provider = StdFilesystemProvider;
        let (kids, truncated) = provider.children_bounded(&temp.0, 3).unwrap();
        assert_eq!(kids.len(), 3);
        assert!(truncated);
        let (kids, truncated) = provider.children_bounded(&temp.0, 6).unwrap();
        assert_eq!(kids.len(), 6);
        assert!(!truncated);
        let options = ScanOptions {
            max_entries: 4,
            ..Default::default()
        };
        let report = scan_with_provider(&provider, std::slice::from_ref(&temp.0), &options);
        assert_eq!(report.entries.len(), 4);
        assert!(report.accounting.incomplete);
        assert!(has(&report.incomplete_reasons, "entry limit"));
    }
}

#[cfg(target_os = "macos")]
#[test]
fn macos_volume_identity_is_uuid_or_flagged_unstable() {
    let provider = StdFilesystemProvider;
    let (metadata, reasons) = provider.inspect_detailed(&std::env::temp_dir()).unwrap();
    if metadata.volume.id.starts_with("uuid:") {
        assert!(metadata.metadata_complete);
    } else {
        assert!(!metadata.metadata_complete);
        assert!(has(&reasons, "not stable"));
    }
}
