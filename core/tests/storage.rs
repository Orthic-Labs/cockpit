use cockpit_core::{
    EntryKind, FileIdentity, FileMetadata, FilesystemProvider, FsError, ScanOptions, SnapshotState,
    VolumeIdentity, VolumeUsage, scan_with_provider,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Default)]
struct Fixture {
    metadata: BTreeMap<PathBuf, FileMetadata>,
    children: BTreeMap<PathBuf, Vec<PathBuf>>,
}

impl FilesystemProvider for Fixture {
    fn inspect(&self, path: &Path) -> Result<FileMetadata, FsError> {
        self.metadata
            .get(path)
            .cloned()
            .ok_or_else(|| FsError::new(format!("missing fixture {}", path.display())))
    }
    fn children(&self, path: &Path) -> Result<Vec<PathBuf>, FsError> {
        Ok(self.children.get(path).cloned().unwrap_or_default())
    }
    fn volume_usage(&self, volume: &VolumeIdentity) -> Result<VolumeUsage, FsError> {
        Ok(VolumeUsage {
            volume: volume.clone(),
            total_bytes: Some(10_000),
            used_bytes: Some(100),
            available_bytes: Some(9_900),
            purgeable_bytes: None,
            snapshots: SnapshotState::Unknown,
        })
    }
}

fn dir(volume: &VolumeIdentity) -> FileMetadata {
    FileMetadata {
        kind: EntryKind::Directory,
        volume: volume.clone(),
        logical_size: Some(0),
        allocation_size: Some(0),
        file_id: None,
        clone_id: None,
        is_placeholder: false,
        metadata_complete: true,
    }
}
fn file(volume: &VolumeIdentity, id: &str, bytes: u64) -> FileMetadata {
    FileMetadata {
        kind: EntryKind::File,
        volume: volume.clone(),
        logical_size: Some(bytes),
        allocation_size: Some(bytes),
        file_id: Some(FileIdentity {
            volume: volume.clone(),
            id: id.into(),
        }),
        clone_id: None,
        is_placeholder: false,
        metadata_complete: true,
    }
}

#[test]
fn hardlinks_are_attributed_once_in_lexical_order() {
    let volume = VolumeIdentity::new("fixture-volume");
    let mut fixture = Fixture::default();
    fixture.metadata.insert("root".into(), dir(&volume));
    fixture
        .metadata
        .insert("root/a".into(), file(&volume, "same", 40));
    fixture
        .metadata
        .insert("root/b".into(), file(&volume, "same", 40));
    fixture
        .children
        .insert("root".into(), vec!["root/b".into(), "root/a".into()]);
    let report = scan_with_provider(&fixture, &[PathBuf::from("root")], &ScanOptions::default());
    assert_eq!(report.accounting.logical_bytes, 40);
    assert_eq!(report.accounting.attributed_allocation_bytes, 40);
    assert_eq!(report.accounting.reclaim.upper_bytes, Some(40));
    assert_eq!(report.folders[0].attributed_allocation_bytes,40);
    assert_eq!(
        report.entries[1].accounting_owner,
        Some(PathBuf::from("root/a"))
    );
}

#[test]
fn entry_limit_is_explicitly_incomplete() {
    let volume = VolumeIdentity::new("fixture-volume");
    let mut fixture = Fixture::default();
    fixture.metadata.insert("root".into(), dir(&volume));
    for name in ["root/a", "root/b", "root/c"] {
        fixture.metadata.insert(name.into(), file(&volume, name, 1));
    }
    fixture.children.insert(
        "root".into(),
        vec!["root/a".into(), "root/b".into(), "root/c".into()],
    );
    let options = ScanOptions {
        max_entries: 2,
        ..Default::default()
    };
    let report = scan_with_provider(&fixture, &[PathBuf::from("root")], &options);
    assert!(report.accounting.incomplete);
    assert!(
        report
            .incomplete_reasons
            .iter()
            .any(|reason| reason.contains("entry limit"))
    );
}

#[test]
fn placeholder_root_is_rejected_without_enumeration() {
    let volume = VolumeIdentity::new("fixture-volume");
    let mut fixture = Fixture::default();
    let mut root = dir(&volume);
    root.is_placeholder = true;
    fixture.metadata.insert("root".into(), root);
    fixture
        .children
        .insert("root".into(), vec!["root/secret".into()]);
    let report = scan_with_provider(&fixture, &[PathBuf::from("root")], &ScanOptions::default());
    assert!(report.entries.is_empty());
    assert!(
        report
            .incomplete_reasons
            .iter()
            .any(|reason| reason.contains("placeholder"))
    );
}

#[test]
fn links_are_reported_and_overlapping_roots_are_deduplicated() {
    let volume = VolumeIdentity::new("fixture-volume");
    let mut fixture = Fixture::default();
    fixture.metadata.insert("root".into(), dir(&volume));
    fixture
        .metadata
        .insert("root/a".into(), file(&volume, "a", 9));
    fixture.metadata.insert(
        "root/link".into(),
        FileMetadata {
            kind: EntryKind::Symlink,
            volume: volume.clone(),
            logical_size: None,
            allocation_size: None,
            file_id: None,
            clone_id: None,
            is_placeholder: false,
            metadata_complete: true,
        },
    );
    fixture
        .children
        .insert("root".into(), vec!["root/link".into(), "root/a".into()]);
    let report = scan_with_provider(
        &fixture,
        &[PathBuf::from("root"), PathBuf::from("root/a")],
        &ScanOptions::default(),
    );
    assert_eq!(
        report
            .entries
            .iter()
            .filter(|entry| entry.path == Path::new("root/a"))
            .count(),
        1
    );
    assert_eq!(report.skipped_links.len(), 1);
}

#[test]
fn missing_allocation_metadata_has_no_guessed_upper_bound() {
    let volume = VolumeIdentity::new("fixture-volume");
    let mut fixture = Fixture::default();
    fixture.metadata.insert("root".into(), dir(&volume));
    fixture.metadata.insert(
        "root/unknown".into(),
        FileMetadata {
            kind: EntryKind::File,
            volume: volume.clone(),
            logical_size: Some(20),
            allocation_size: None,
            file_id: None,
            clone_id: None,
            is_placeholder: false,
            metadata_complete: false,
        },
    );
    fixture
        .children
        .insert("root".into(), vec!["root/unknown".into()]);
    let report = scan_with_provider(&fixture, &[PathBuf::from("root")], &ScanOptions::default());
    assert!(report.accounting.incomplete);
    assert_eq!(report.accounting.reclaim.upper_bytes, None);
}

#[test]
fn placeholder_ancestor_stops_before_descendant_inspection() {
    let volume = VolumeIdentity::new("fixture-volume");
    let mut fixture = Fixture::default();
    let mut placeholder = dir(&volume);
    placeholder.is_placeholder = true;
    fixture.metadata.insert("root".into(),placeholder);
    let report = scan_with_provider(&fixture,&[PathBuf::from("root/child")],&ScanOptions::default());
    assert!(report.entries.is_empty());
    assert!(report.inspection_errors.is_empty()); // Missing child fixture proves no child inspection.
    assert_eq!(report.skipped_links[0].path,Path::new("root"));
}
