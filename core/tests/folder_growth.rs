use cockpit_core::folder_growth::compare_folders;
use cockpit_core::model::{
    Accounting, EntryKind, FileMetadata, FolderAccounting, ScanReport, ScannedEntry, SkippedLink,
    SnapshotState,
};
use cockpit_core::store::Snapshot;
use cockpit_core::{VolumeIdentity, VolumeUsage};
use std::path::PathBuf;

fn folder(volume: &str, path: &str, logical: u64, attributed: u64) -> FolderAccounting {
    FolderAccounting {
        path: path.into(),
        volume: VolumeIdentity::new(volume),
        logical_bytes: logical,
        attributed_allocation_bytes: attributed,
        incomplete: false,
    }
}

fn report(roots: &[&str], volumes: &[&str], folders: Vec<FolderAccounting>) -> ScanReport {
    ScanReport {
        roots: roots.iter().map(PathBuf::from).collect(),
        entries: Vec::new(),
        folders,
        accounting: Accounting::default(),
        volume_usage: volumes
            .iter()
            .map(|id| VolumeUsage {
                volume: VolumeIdentity::new(*id),
                total_bytes: None,
                used_bytes: None,
                available_bytes: None,
                purgeable_bytes: None,
                snapshots: SnapshotState::Unknown,
            })
            .collect(),
        volume_deltas: Vec::new(),
        inspection_errors: Vec::new(),
        skipped_links: Vec::new(),
        incomplete_reasons: Vec::new(),
    }
}

fn snapshot(report: ScanReport) -> Snapshot {
    Snapshot {
        schema_version: 1,
        id: "scan-test".into(),
        created_at: 1,
        report,
        findings: Vec::new(),
    }
}

fn comparable(
    previous: Vec<FolderAccounting>,
    current: Vec<FolderAccounting>,
) -> (Snapshot, Snapshot) {
    (
        snapshot(report(&["/scan"], &["vol-a"], previous)),
        snapshot(report(&["/scan"], &["vol-a"], current)),
    )
}

#[test]
fn signed_changes_added_removed_and_deterministic_order_are_reported() {
    let (previous, current) = comparable(
        vec![
            folder("vol-a", "/scan/keep", 100, 100),
            folder("vol-a", "/scan/shrink", 500, 500),
            folder("vol-a", "/scan/removed", 7, 9),
        ],
        vec![
            folder("vol-a", "/scan/keep", 150, 180),
            folder("vol-a", "/scan/shrink", 200, 250),
            folder("vol-a", "/scan/added", 11, 13),
        ],
    );

    let result = compare_folders(&previous, &current, 10);
    assert!(result.comparable);
    assert_eq!(result.top_growth.len(), 1);
    assert_eq!(result.top_growth[0].path, PathBuf::from("/scan/keep"));
    assert_eq!(result.top_growth[0].logical_growth_bytes, 50);
    assert_eq!(result.top_growth[0].attributed_growth_bytes, 80);
    assert_eq!(result.top_shrink[0].path, PathBuf::from("/scan/shrink"));
    assert_eq!(result.top_shrink[0].logical_growth_bytes, -300);
    assert_eq!(result.top_shrink[0].attributed_growth_bytes, -250);
    assert_eq!(result.added_folders[0].path, PathBuf::from("/scan/added"));
    assert_eq!(result.added_folders[0].logical_growth_bytes, 11);
    assert_eq!(
        result.removed_folders[0].path,
        PathBuf::from("/scan/removed")
    );
    assert_eq!(result.removed_folders[0].attributed_growth_bytes, -9);
    assert!(!result.truncated);
}

#[test]
fn incomplete_snapshot_refuses_folder_totals() {
    let (previous, mut current) = comparable(
        vec![folder("vol-a", "/scan/folder", 10, 10)],
        vec![folder("vol-a", "/scan/folder", 20, 20)],
    );
    current.report.accounting.incomplete = true;

    let result = compare_folders(&previous, &current, 10);
    assert!(!result.comparable);
    assert!(result.top_growth.is_empty());
    assert!(
        result
            .reasons
            .iter()
            .any(|reason| reason.contains("incomplete"))
    );
}

#[test]
fn remount_and_root_change_use_history_compatibility_guard() {
    let (previous, mut current) = comparable(
        vec![folder("vol-a", "/scan/folder", 10, 10)],
        vec![folder("vol-a", "/scan/folder", 20, 20)],
    );
    current.report.volume_usage[0].volume = VolumeIdentity::new("vol-b");
    let remount = compare_folders(&previous, &current, 10);
    assert!(!remount.comparable);
    assert!(
        remount
            .reasons
            .iter()
            .any(|reason| reason.contains("volume identities differ"))
    );

    let mut changed_root = current;
    changed_root.report.volume_usage[0].volume = VolumeIdentity::new("vol-a");
    changed_root.report.roots = vec![PathBuf::from("/other")];
    let roots = compare_folders(&previous, &changed_root, 10);
    assert!(!roots.comparable);
    assert!(roots.reasons.iter().any(|reason| reason.contains("roots")));
}

#[test]
fn missing_identity_duplicate_and_out_of_scope_rows_refuse_ambiguity() {
    let (previous, mut current) = comparable(
        vec![folder("vol-a", "/scan/folder", 10, 10)],
        vec![folder("vol-a", "/scan/folder", 20, 20)],
    );
    current.report.folders[0].volume = VolumeIdentity::new("unknown");
    let missing = compare_folders(&previous, &current, 10);
    assert!(!missing.comparable);
    assert!(
        missing
            .reasons
            .iter()
            .any(|reason| reason.contains("identity"))
    );

    let (previous, mut current) = comparable(
        vec![folder("vol-a", "/scan/folder", 10, 10)],
        vec![folder("vol-a", "/scan/folder", 20, 20)],
    );
    current
        .report
        .folders
        .push(folder("vol-a", "/scan/folder", 20, 20));
    let duplicate = compare_folders(&previous, &current, 10);
    assert!(!duplicate.comparable);
    assert!(
        duplicate
            .reasons
            .iter()
            .any(|reason| reason.contains("duplicate"))
    );

    let (previous, mut current) = comparable(
        vec![folder("vol-a", "/scan/folder", 10, 10)],
        vec![folder("vol-a", "/scan/folder", 20, 20)],
    );
    current.report.folders[0].path = PathBuf::from("/outside");
    let outside = compare_folders(&previous, &current, 10);
    assert!(!outside.comparable);
    assert!(
        outside
            .reasons
            .iter()
            .any(|reason| reason.contains("outside"))
    );
}

#[test]
fn u64_extremes_use_signed_i128_without_underflow() {
    let (previous, current) = comparable(
        vec![
            folder("vol-a", "/scan/grow", 0, 0),
            folder("vol-a", "/scan/shrink", u64::MAX, u64::MAX),
        ],
        vec![
            folder("vol-a", "/scan/grow", u64::MAX, u64::MAX),
            folder("vol-a", "/scan/shrink", 0, 0),
        ],
    );
    let result = compare_folders(&previous, &current, 10);
    assert!(result.comparable);
    assert_eq!(
        result.top_growth[0].logical_growth_bytes,
        i128::from(u64::MAX)
    );
    assert_eq!(
        result.top_growth[0].attributed_growth_bytes,
        i128::from(u64::MAX)
    );
    assert_eq!(
        result.top_shrink[0].logical_growth_bytes,
        -i128::from(u64::MAX)
    );
    assert_eq!(
        result.top_shrink[0].attributed_growth_bytes,
        -i128::from(u64::MAX)
    );
    let encoded = serde_json::to_value(&result).unwrap();
    assert_eq!(
        encoded["top_growth"][0]["logical_growth_bytes"].as_u64(),
        Some(u64::MAX)
    );
    let minimum_text = (-i128::from(u64::MAX)).to_string();
    assert_eq!(
        encoded["top_shrink"][0]["logical_growth_bytes"].as_str(),
        Some(minimum_text.as_str())
    );
}

#[test]
fn each_output_list_is_bounded_and_truncation_is_explicit() {
    let (previous, current) = comparable(
        vec![
            folder("vol-a", "/scan/a", 0, 0),
            folder("vol-a", "/scan/b", 0, 0),
            folder("vol-a", "/scan/c", 0, 0),
        ],
        vec![
            folder("vol-a", "/scan/a", 3, 3),
            folder("vol-a", "/scan/b", 2, 2),
            folder("vol-a", "/scan/c", 1, 1),
        ],
    );
    let limited = compare_folders(&previous, &current, 2);
    assert!(limited.comparable);
    assert_eq!(limited.top_growth.len(), 2);
    assert!(limited.truncated);

    let zero = compare_folders(&previous, &current, 0);
    assert!(zero.comparable);
    assert!(zero.top_growth.is_empty());
    assert!(zero.truncated);
}

#[test]
fn caller_limit_is_capped_and_reported() {
    let (previous, current) = comparable(
        vec![folder("vol-a", "/scan/folder", 0, 0)],
        vec![folder("vol-a", "/scan/folder", 1, 1)],
    );
    let result = compare_folders(&previous, &current, usize::MAX);
    assert!(result.comparable);
    assert_eq!(result.limit, cockpit_core::folder_growth::MAX_FOLDER_LIMIT);
    assert!(
        result
            .reasons
            .iter()
            .any(|reason| reason.contains("maximum"))
    );
}

#[test]
fn dot_components_in_roots_or_folder_rows_are_rejected() {
    let (previous, mut current) = comparable(
        vec![folder("vol-a", "/scan/folder", 10, 10)],
        vec![folder("vol-a", "/scan/folder", 20, 20)],
    );
    current.report.folders[0].path = PathBuf::from("/scan/./folder");
    let folder_path = compare_folders(&previous, &current, 10);
    assert!(!folder_path.comparable);
    assert!(
        folder_path
            .reasons
            .iter()
            .any(|reason| reason.contains("path component"))
    );

    let (mut previous, mut current) = comparable(
        vec![folder("vol-a", "/scan/folder", 10, 10)],
        vec![folder("vol-a", "/scan/folder", 20, 20)],
    );
    previous.report.roots = vec![PathBuf::from("/scan/../scan")];
    current.report.roots = previous.report.roots.clone();
    let root_path = compare_folders(&previous, &current, 10);
    assert!(!root_path.comparable);
    assert!(
        root_path
            .reasons
            .iter()
            .any(|reason| reason.contains("selected scan root"))
    );
}

#[test]
fn empty_folder_coverage_for_nonempty_report_is_rejected() {
    let (mut previous, current) = comparable(
        vec![folder("vol-a", "/scan/folder", 10, 10)],
        vec![folder("vol-a", "/scan/folder", 20, 20)],
    );
    previous.report.folders.clear();
    previous.report.entries.push(ScannedEntry {
        path: PathBuf::from("/scan/dir"),
        metadata: FileMetadata {
            kind: EntryKind::Directory,
            volume: VolumeIdentity::new("vol-a"),
            logical_size: Some(0),
            allocation_size: Some(0),
            file_id: None,
            clone_id: None,
            created_at: None,
            modified_at: None,
            is_placeholder: false,
            metadata_complete: true,
        },
        logical_bytes: 0,
        attributed_allocation_bytes: 0,
        accounting_owner: None,
        reclaim: None,
    });
    let result = compare_folders(&previous, &current, 10);
    assert!(!result.comparable);
    assert!(
        result
            .reasons
            .iter()
            .any(|reason| reason.contains("folder coverage"))
    );
}

#[test]
fn skipped_symlink_note_alone_does_not_invalidate_complete_comparison() {
    let (mut previous, current) = comparable(
        vec![folder("vol-a", "/scan/folder", 10, 10)],
        vec![folder("vol-a", "/scan/folder", 20, 20)],
    );
    previous.report.skipped_links.push(SkippedLink {
        path: PathBuf::from("/scan/link"),
        reason: "symlink".into(),
    });
    let result = compare_folders(&previous, &current, 10);
    assert!(result.comparable);
    assert_eq!(result.top_growth[0].logical_growth_bytes, 10);
}

#[test]
fn attributed_direction_wins_when_logical_direction_differs() {
    let (previous, current) = comparable(
        vec![folder("vol-a", "/scan/mixed", 10, 100)],
        vec![folder("vol-a", "/scan/mixed", 20, 90)],
    );
    let result = compare_folders(&previous, &current, 10);
    assert!(result.comparable);
    assert!(result.top_growth.is_empty());
    assert_eq!(result.top_shrink.len(), 1);
    assert_eq!(result.top_shrink[0].logical_growth_bytes, 10);
    assert_eq!(result.top_shrink[0].attributed_growth_bytes, -10);
}
