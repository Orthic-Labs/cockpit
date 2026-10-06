use cockpit_core::model::{
    Accounting, CloneIdentity, EntryKind, FileIdentity, FileMetadata, FolderAccounting,
    ReclaimEstimate, ReclaimState, ScanReport, ScannedEntry, VolumeIdentity,
};
use cockpit_core::storage_browser::{
    ChildrenPage, DateAvailability, SearchRequest, StorageBrowserError, drilldown_children,
    inspect, largest_files, largest_folders, search_filenames,
};
use std::path::{Path, PathBuf};

fn report(
    entries: Vec<ScannedEntry>,
    folders: Vec<FolderAccounting>,
    incomplete: bool,
) -> ScanReport {
    let volume = VolumeIdentity::new("fixture");
    ScanReport {
        roots: vec![PathBuf::from("/scope")],
        entries,
        folders,
        accounting: Accounting {
            incomplete,
            ..Accounting::default()
        },
        volume_usage: Vec::new(),
        volume_deltas: Vec::new(),
        inspection_errors: Vec::new(),
        skipped_links: Vec::new(),
        incomplete_reasons: if incomplete {
            vec!["fixture truncation".into()]
        } else {
            Vec::new()
        },
    }
}

fn file(
    path: &str,
    logical: Option<u64>,
    allocation: Option<u64>,
    id: Option<&str>,
) -> ScannedEntry {
    let volume = VolumeIdentity::new("fixture");
    ScannedEntry {
        path: PathBuf::from(path),
        metadata: FileMetadata {
            kind: EntryKind::File,
            volume: volume.clone(),
            logical_size: logical,
            allocation_size: allocation,
            file_id: id.map(|id| FileIdentity {
                volume: volume.clone(),
                id: id.into(),
            }),
            clone_id: None,
            is_placeholder: false,
            metadata_complete: logical.is_some() && allocation.is_some() && id.is_some(),
        },
        logical_bytes: logical.unwrap_or(0),
        attributed_allocation_bytes: allocation.unwrap_or(0),
        accounting_owner: Some(PathBuf::from(path)),
        reclaim: Some(ReclaimEstimate {
            lower_bytes: 0,
            upper_bytes: allocation,
            state: ReclaimState::Unknown,
            reasons: vec!["sharing unproven".into()],
        }),
    }
}

fn dir(path: &str) -> ScannedEntry {
    ScannedEntry {
        path: PathBuf::from(path),
        metadata: FileMetadata {
            kind: EntryKind::Directory,
            volume: VolumeIdentity::new("fixture"),
            logical_size: Some(0),
            allocation_size: Some(0),
            file_id: None,
            clone_id: None,
            is_placeholder: false,
            metadata_complete: true,
        },
        logical_bytes: 0,
        attributed_allocation_bytes: 0,
        accounting_owner: None,
        reclaim: None,
    }
}

#[test]
fn search_includes_hidden_files_filters_extension_and_paginates() {
    let entries = vec![
        file("/scope/.cache", Some(4), Some(4), Some("hidden")),
        file("/scope/Alpha.TXT", Some(10), Some(8), Some("alpha")),
        file(
            "/scope/alpha-two.txt",
            Some(20),
            Some(12),
            Some("alpha-two"),
        ),
        file("/scope/other.bin", Some(100), Some(100), Some("other")),
    ];
    let report = report(entries, Vec::new(), false);
    let mut request = SearchRequest::new("ALPHA", 1);
    request.extension = Some(".txt".into());
    let first = search_filenames(&report, &request).unwrap();
    assert_eq!(first.total_matches, 2);
    assert_eq!(first.items[0].name, "Alpha.TXT");
    assert!(first.has_more);
    request.offset = 1;
    let second = search_filenames(&report, &request).unwrap();
    assert_eq!(second.items[0].name, "alpha-two.txt");
    let hidden = search_filenames(&report, &SearchRequest::new(".cache", 10)).unwrap();
    assert_eq!(hidden.items.len(), 1);
}

#[test]
fn malformed_query_and_limits_are_rejected() {
    let report = report(Vec::new(), Vec::new(), false);
    let mut zero = SearchRequest::default();
    zero.limit = 0;
    assert!(matches!(
        search_filenames(&report, &zero),
        Err(StorageBrowserError::InvalidLimit(0))
    ));
    let mut offset = SearchRequest::default();
    offset.offset = cockpit_core::storage_browser::MAX_PAGE_OFFSET + 1;
    assert!(matches!(
        search_filenames(&report, &offset),
        Err(StorageBrowserError::InvalidOffset(_))
    ));
    let mut long = SearchRequest::default();
    long.query = "x".repeat(cockpit_core::storage_browser::MAX_QUERY_LENGTH + 1);
    assert!(matches!(
        search_filenames(&report, &long),
        Err(StorageBrowserError::InvalidQuery(_))
    ));
    let mut range = SearchRequest::default();
    range.min_size = Some(10);
    range.max_size = Some(1);
    assert!(matches!(
        search_filenames(&report, &range),
        Err(StorageBrowserError::InvalidSizeRange)
    ));
}

#[test]
fn overlap_duplicates_collapse_without_merging_distinct_hardlinks() {
    let mut duplicate = file("/scope/x", Some(2), Some(2), Some("same"));
    duplicate.metadata.metadata_complete = false;
    let report = report(
        vec![
            duplicate,
            file("/scope/x", Some(2), Some(2), Some("same")),
            file("/scope/y", Some(2), Some(2), Some("same")),
        ],
        Vec::new(),
        false,
    );
    let page = search_filenames(&report, &SearchRequest::new("", 10)).unwrap();
    assert_eq!(page.total_matches, 2);
    assert_eq!(largest_files(&report, 10).unwrap().len(), 2);
    let inspection = inspect(&report, Path::new("/scope/y")).unwrap();
    assert_eq!(inspection.share.paths_with_same_file_id, Some(2));
    assert_eq!(inspection.share.hardlink_shared, Some(true));
}

#[test]
fn incomplete_and_placeholder_state_is_preserved_without_hydration() {
    let mut placeholder = file("/scope/cloud", None, None, None);
    placeholder.metadata.is_placeholder = true;
    let report = report(vec![placeholder], Vec::new(), true);
    let result = search_filenames(&report, &SearchRequest::new("cloud", 10)).unwrap();
    assert!(result.incomplete);
    assert_eq!(result.items[0].is_placeholder, true);
    assert_eq!(result.items[0].logical_size, None);
    let inspection = inspect(&report, Path::new("/scope/cloud")).unwrap();
    assert!(inspection.incomplete);
    assert_eq!(inspection.reclaim.as_ref().unwrap().upper_bytes, None);
    assert_eq!(inspection.dates, DateAvailability::default());
}

#[test]
fn drilldown_is_direct_child_only_and_rejects_outside_path() {
    let report = report(
        vec![
            dir("/scope"),
            dir("/scope/sub"),
            file("/scope/root.txt", Some(1), Some(1), Some("root")),
            file("/scope/sub/nested.txt", Some(1), Some(1), Some("nested")),
            file("/scope/submarine.txt", Some(1), Some(1), Some("other")),
        ],
        Vec::new(),
        false,
    );
    let children: ChildrenPage = drilldown_children(&report, Path::new("/scope"), 0, 10).unwrap();
    assert_eq!(children.total_children, 3);
    assert!(
        children
            .items
            .iter()
            .all(|item| item.path != Path::new("/scope/sub/nested.txt"))
    );
    assert!(matches!(
        drilldown_children(&report, Path::new("/outside"), 0, 10),
        Err(StorageBrowserError::PathOutsideRoots(_))
    ));
}

#[test]
fn largest_folders_are_deterministic_and_report_incompleteness() {
    let volume = VolumeIdentity::new("fixture");
    let folders = vec![
        FolderAccounting {
            path: PathBuf::from("/scope/z"),
            volume: volume.clone(),
            logical_bytes: 10,
            attributed_allocation_bytes: 20,
            incomplete: false,
        },
        FolderAccounting {
            path: PathBuf::from("/scope/a"),
            volume,
            logical_bytes: 10,
            attributed_allocation_bytes: 20,
            incomplete: false,
        },
    ];
    let report = report(Vec::new(), folders, true);
    let largest = largest_folders(&report, 10).unwrap();
    assert_eq!(largest[0].path, Path::new("/scope/a"));
    assert!(largest.iter().all(|folder| folder.incomplete));
}
