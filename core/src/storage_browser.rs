//! Read-only, metadata-only browser over a [`ScanReport`].
//!
//! This module never touches the filesystem.  Search, drilldown, ranking, and
//! inspection all operate on observations already present in a scan report.
//! Missing metadata remains missing; in particular, allocation is never used
//! to manufacture compression or reclaim savings.

use crate::model::{
    CloneIdentity, EntryKind, FileIdentity, FolderAccounting, ReclaimEstimate, ScanReport,
    ScannedEntry, VolumeIdentity,
};
use rightkit_search::filename::{
    Entry as IndexEntry, Error as IndexError, FilenameIndex, Kind as IndexKind,
    Request as IndexRequest,
};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// Maximum query length accepted by [`search_filenames`], in bytes and chars.
pub const MAX_QUERY_LENGTH: usize = rightkit_search::filename::MAX_QUERY_LENGTH;
/// Maximum extension length accepted by [`search_filenames`].
pub const MAX_EXTENSION_LENGTH: usize = rightkit_search::filename::MAX_EXTENSION_LENGTH;
/// Maximum number of rows returned by one browser operation.
pub const MAX_PAGE_LIMIT: usize = 1_000;
/// Maximum offset accepted by a browser operation.
pub const MAX_PAGE_OFFSET: usize = 1_000_000;

/// Filesystem timestamps carried by scan metadata. Missing or pre-Unix values
/// remain explicit unknowns; browser code never rereads paths.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DateAvailability {
    pub available: bool,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<u64>,
}

impl Default for DateAvailability {
    fn default() -> Self {
        Self {
            available: false,
            reason: "dates are unavailable in filesystem metadata".into(),
            created_at: None,
            modified_at: None,
        }
    }
}

/// Stable error returned for malformed bounds or paths outside scan scope.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum StorageBrowserError {
    InvalidQuery(String),
    InvalidExtension(String),
    InvalidLimit(usize),
    InvalidOffset(usize),
    InvalidSizeRange,
    PathOutsideRoots(PathBuf),
    PathNotFound(PathBuf),
    NotDirectory(PathBuf),
}

impl fmt::Display for StorageBrowserError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidQuery(message) => write!(f, "invalid query: {message}"),
            Self::InvalidExtension(message) => write!(f, "invalid extension: {message}"),
            Self::InvalidLimit(limit) => {
                write!(f, "invalid limit {limit}; expected 1..={MAX_PAGE_LIMIT}")
            }
            Self::InvalidOffset(offset) => {
                write!(f, "invalid offset {offset}; maximum is {MAX_PAGE_OFFSET}")
            }
            Self::InvalidSizeRange => write!(f, "minimum size exceeds maximum size"),
            Self::PathOutsideRoots(path) => {
                write!(f, "path is outside scan roots: {}", path.display())
            }
            Self::PathNotFound(path) => {
                write!(f, "path is absent from scan report: {}", path.display())
            }
            Self::NotDirectory(path) => write!(f, "path is not a directory: {}", path.display()),
        }
    }
}

impl std::error::Error for StorageBrowserError {}

/// Filename search and pagination request.  Empty `query` means all names,
/// including hidden names such as `.config`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    pub kind: Option<EntryKind>,
    /// Extension without or with a leading dot; matching is case-insensitive.
    pub extension: Option<String>,
    /// Size predicates apply to logical size. Unknown logical sizes do not
    /// match a size predicate.
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    pub offset: usize,
    pub limit: usize,
}

impl Default for SearchRequest {
    fn default() -> Self {
        Self {
            query: String::new(),
            kind: None,
            extension: None,
            min_size: None,
            max_size: None,
            offset: 0,
            limit: 100,
        }
    }
}

impl SearchRequest {
    pub fn new(query: impl Into<String>, limit: usize) -> Self {
        Self {
            query: query.into(),
            limit,
            ..Self::default()
        }
    }
}

/// A report entry suitable for a browser row.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StorageItem {
    pub path: PathBuf,
    pub name: String,
    pub kind: EntryKind,
    pub volume: VolumeIdentity,
    pub logical_size: Option<u64>,
    pub allocation_size: Option<u64>,
    pub attributed_allocation_size: u64,
    pub metadata_complete: bool,
    pub is_placeholder: bool,
    pub accounting_owner: Option<PathBuf>,
    pub reclaim: Option<ReclaimEstimate>,
    pub dates: DateAvailability,
}

/// A browser folder row. Folder sizes come from ScanReport folder aggregates.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StorageFolder {
    pub path: PathBuf,
    pub name: String,
    pub volume: VolumeIdentity,
    pub logical_size: u64,
    pub allocation_size: u64,
    pub incomplete: bool,
    pub dates: DateAvailability,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchPage {
    pub items: Vec<StorageItem>,
    pub offset: usize,
    pub limit: usize,
    pub total_matches: usize,
    pub has_more: bool,
    pub incomplete: bool,
    pub dates: DateAvailability,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChildrenPage {
    pub parent: PathBuf,
    pub items: Vec<StorageItem>,
    pub offset: usize,
    pub limit: usize,
    pub total_children: usize,
    pub has_more: bool,
    pub incomplete: bool,
    pub dates: DateAvailability,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct StorageCounts {
    pub direct_children: usize,
    pub descendant_entries: usize,
    pub files: usize,
    pub directories: usize,
    pub symlinks: usize,
    pub other: usize,
}

/// Sharing facts are optional because absence of identity is an unknown, not
/// proof that a file is unshared.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ShareInfo {
    pub file_id: Option<FileIdentity>,
    pub clone_id: Option<CloneIdentity>,
    pub paths_with_same_file_id: Option<usize>,
    pub paths_with_same_clone_id: Option<usize>,
    pub hardlink_shared: Option<bool>,
    pub clone_shared: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StorageInspection {
    pub path: PathBuf,
    pub name: String,
    pub kind: EntryKind,
    pub volume: VolumeIdentity,
    pub logical_size: Option<u64>,
    pub allocation_size: Option<u64>,
    pub attributed_allocation_size: u64,
    pub reclaim: Option<ReclaimEstimate>,
    pub incomplete: bool,
    pub counts: StorageCounts,
    pub share: ShareInfo,
    pub dates: DateAvailability,
}

/// Validate a page bound shared by search and drilldown APIs.
pub fn validate_page_bounds(offset: usize, limit: usize) -> Result<(), StorageBrowserError> {
    if limit == 0 || limit > MAX_PAGE_LIMIT {
        return Err(StorageBrowserError::InvalidLimit(limit));
    }
    if offset > MAX_PAGE_OFFSET {
        return Err(StorageBrowserError::InvalidOffset(offset));
    }
    Ok(())
}

fn basename(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn report_incomplete(report: &ScanReport) -> bool {
    report.accounting.incomplete
        || !report.incomplete_reasons.is_empty()
        || !report.inspection_errors.is_empty()
        || !report.skipped_links.is_empty()
}

fn item_from_entry(entry: &ScannedEntry) -> StorageItem {
    StorageItem {
        path: entry.path.clone(),
        name: basename(&entry.path),
        kind: entry.metadata.kind,
        volume: entry.metadata.volume.clone(),
        logical_size: entry.metadata.logical_size,
        allocation_size: entry.metadata.allocation_size,
        attributed_allocation_size: entry.attributed_allocation_bytes,
        metadata_complete: entry.metadata.metadata_complete,
        is_placeholder: entry.metadata.is_placeholder,
        accounting_owner: entry.accounting_owner.clone(),
        reclaim: entry.reclaim.clone(),
        dates: dates_from_metadata(&entry.metadata),
    }
}

fn dates_from_metadata(metadata: &crate::model::FileMetadata) -> DateAvailability {
    let (available, reason) = match (metadata.created_at, metadata.modified_at) {
        (Some(_), Some(_)) => (true, "filesystem metadata".to_owned()),
        (Some(_), None) | (None, Some(_)) => {
            (true, "one filesystem timestamp unavailable".to_owned())
        }
        (None, None) => (
            false,
            "dates are unavailable in filesystem metadata".to_owned(),
        ),
    };
    DateAvailability {
        available,
        reason,
        created_at: metadata.created_at,
        modified_at: metadata.modified_at,
    }
}

fn entry_preference(a: &ScannedEntry, b: &ScannedEntry) -> Ordering {
    b.metadata
        .metadata_complete
        .cmp(&a.metadata.metadata_complete)
        .then_with(|| {
            b.metadata
                .logical_size
                .is_some()
                .cmp(&a.metadata.logical_size.is_some())
        })
        .then_with(|| {
            b.metadata
                .allocation_size
                .is_some()
                .cmp(&a.metadata.allocation_size.is_some())
        })
        .then_with(|| {
            b.attributed_allocation_bytes
                .cmp(&a.attributed_allocation_bytes)
        })
        .then_with(|| b.logical_bytes.cmp(&a.logical_bytes))
        .then_with(|| a.metadata.is_placeholder.cmp(&b.metadata.is_placeholder))
        .then_with(|| format!("{:?}", a.metadata.file_id).cmp(&format!("{:?}", b.metadata.file_id)))
}

/// Return one deterministic representative for every path, collapsing overlap
/// duplicates without merging distinct hardlink paths.
fn canonical_entries(report: &ScanReport) -> Vec<&ScannedEntry> {
    let mut entries: Vec<_> = report
        .entries
        .iter()
        .filter(|entry| root_contains(&report.roots, &entry.path))
        .collect();
    entries.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| entry_preference(a, b)));
    let mut result = Vec::with_capacity(entries.len());
    for entry in entries {
        if result
            .last()
            .is_some_and(|last: &&ScannedEntry| last.path == entry.path)
        {
            continue;
        }
        result.push(entry);
    }
    result
}

fn root_contains(roots: &[PathBuf], path: &Path) -> bool {
    roots
        .iter()
        .any(|root| path == root || path.starts_with(root))
}

/// Resolve platform alias prefixes (macOS /var -> /private/var) in a caller
/// supplied path so it compares equal to scanned (canonical) paths. Resolves the top-level
/// alias only; other symlinks are not resolved.
fn normalize_input(report: &ScanReport, path: &Path) -> PathBuf {
    if root_contains(&report.roots, path) {
        return path.to_path_buf();
    }
    crate::scan::canonical_root(path)
}

fn validate_path(report: &ScanReport, path: &Path) -> Result<(), StorageBrowserError> {
    if root_contains(&report.roots, path) {
        Ok(())
    } else {
        Err(StorageBrowserError::PathOutsideRoots(path.to_path_buf()))
    }
}

fn page<T: Clone>(all: &[T], offset: usize, limit: usize) -> (Vec<T>, bool) {
    let end = offset.saturating_add(limit).min(all.len());
    let rows = if offset >= all.len() {
        Vec::new()
    } else {
        all[offset..end].to_vec()
    };
    (rows, end < all.len())
}

/// Search filenames against report metadata. Hidden files are included by
/// default. No contents are opened and placeholders are never hydrated.
/// Name, extension, size and pagination matching is delegated to
/// `rightkit_search::filename`; this function only maps report entries onto it.
pub fn search_filenames(
    report: &ScanReport,
    request: &SearchRequest,
) -> Result<SearchPage, StorageBrowserError> {
    let entries = canonical_entries(report);
    let mut index = FilenameIndex::new();
    for entry in &entries {
        if request.kind.is_none_or(|kind| entry.metadata.kind == kind) {
            index.insert(index_entry(entry));
        }
    }
    let page = index.search(&index_request(request))?;
    let by_path: HashMap<&Path, &ScannedEntry> = entries
        .iter()
        .map(|entry| (entry.path.as_path(), *entry))
        .collect();
    let items = page
        .items
        .iter()
        .filter_map(|hit| by_path.get(hit.path.as_path()))
        .map(|entry| item_from_entry(entry))
        .collect();
    Ok(SearchPage {
        items,
        offset: page.offset,
        limit: page.limit,
        total_matches: page.total_matches,
        has_more: page.has_more,
        incomplete: report_incomplete(report),
        dates: DateAvailability::default(),
    })
}

fn index_entry(entry: &ScannedEntry) -> IndexEntry {
    IndexEntry {
        path: entry.path.clone(),
        kind: match entry.metadata.kind {
            EntryKind::File => IndexKind::File,
            EntryKind::Directory => IndexKind::Dir,
            EntryKind::Symlink | EntryKind::Other => IndexKind::Other,
        },
        size: entry.metadata.logical_size,
    }
}

fn index_request(request: &SearchRequest) -> IndexRequest {
    IndexRequest {
        query: request.query.clone(),
        // The kind filter is applied while building the index, because the
        // index kind cannot distinguish symlinks from other entries.
        kind: None,
        extension: request.extension.clone(),
        min_size: request.min_size,
        max_size: request.max_size,
        offset: request.offset,
        limit: request.limit,
    }
}

impl From<IndexError> for StorageBrowserError {
    fn from(error: IndexError) -> Self {
        match error {
            IndexError::InvalidLimit(limit) => Self::InvalidLimit(limit),
            IndexError::InvalidOffset(offset) => Self::InvalidOffset(offset),
            IndexError::InvalidQuery(message) => Self::InvalidQuery(message),
            IndexError::InvalidExtension(message) => Self::InvalidExtension(message),
            IndexError::InvalidSizeRange => Self::InvalidSizeRange,
        }
    }
}

/// Short alias for callers that already have a browser context.
pub fn search(
    report: &ScanReport,
    request: &SearchRequest,
) -> Result<SearchPage, StorageBrowserError> {
    search_filenames(report, request)
}

/// Return largest files ranked by attributed allocation, then logical bytes,
/// then path. Ties are fully deterministic.
pub fn largest_files(
    report: &ScanReport,
    limit: usize,
) -> Result<Vec<StorageItem>, StorageBrowserError> {
    validate_page_bounds(0, limit)?;
    let mut entries: Vec<_> = canonical_entries(report)
        .into_iter()
        .filter(|entry| entry.metadata.kind == EntryKind::File)
        .collect();
    entries.sort_by(|a, b| {
        b.attributed_allocation_bytes
            .cmp(&a.attributed_allocation_bytes)
            .then_with(|| b.logical_bytes.cmp(&a.logical_bytes))
            .then_with(|| a.path.cmp(&b.path))
    });
    Ok(entries
        .into_iter()
        .take(limit)
        .map(item_from_entry)
        .collect())
}

fn canonical_folders(report: &ScanReport) -> Vec<&FolderAccounting> {
    let mut folders: Vec<_> = report
        .folders
        .iter()
        .filter(|folder| root_contains(&report.roots, &folder.path))
        .collect();
    folders.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then_with(|| {
                b.attributed_allocation_bytes
                    .cmp(&a.attributed_allocation_bytes)
            })
            .then_with(|| a.incomplete.cmp(&b.incomplete))
    });
    let mut result = Vec::with_capacity(folders.len());
    for folder in folders {
        if result
            .last()
            .is_some_and(|last: &&FolderAccounting| last.path == folder.path)
        {
            continue;
        }
        result.push(folder);
    }
    result
}

fn folder_from_accounting(folder: &FolderAccounting, incomplete: bool) -> StorageFolder {
    StorageFolder {
        path: folder.path.clone(),
        name: basename(&folder.path),
        volume: folder.volume.clone(),
        logical_size: folder.logical_bytes,
        allocation_size: folder.attributed_allocation_bytes,
        incomplete: incomplete || folder.incomplete,
        dates: DateAvailability::default(),
    }
}

/// Return largest folders ranked by attributed allocation, then logical bytes,
/// then path.
pub fn largest_folders(
    report: &ScanReport,
    limit: usize,
) -> Result<Vec<StorageFolder>, StorageBrowserError> {
    validate_page_bounds(0, limit)?;
    let mut folders = canonical_folders(report);
    folders.sort_by(|a, b| {
        b.attributed_allocation_bytes
            .cmp(&a.attributed_allocation_bytes)
            .then_with(|| b.logical_bytes.cmp(&a.logical_bytes))
            .then_with(|| a.path.cmp(&b.path))
    });
    Ok(folders
        .into_iter()
        .take(limit)
        .map(|folder| folder_from_accounting(folder, report_incomplete(report)))
        .collect())
}

fn directory_exists(report: &ScanReport, path: &Path, entries: &[&ScannedEntry]) -> bool {
    entries
        .iter()
        .any(|entry| entry.path == path && entry.metadata.kind == EntryKind::Directory)
        || report.folders.iter().any(|folder| folder.path == path)
}

/// List direct children of a scanned directory. Child ordering is lexical by
/// complete path, and overlap duplicates are collapsed by path.
pub fn drilldown_children(
    report: &ScanReport,
    path: &Path,
    offset: usize,
    limit: usize,
) -> Result<ChildrenPage, StorageBrowserError> {
    validate_page_bounds(offset, limit)?;
    let normalized = normalize_input(report, path);
    let path = normalized.as_path();
    validate_path(report, path)?;
    let entries = canonical_entries(report);
    if !directory_exists(report, path, &entries) {
        if entries.iter().any(|entry| entry.path == path) {
            return Err(StorageBrowserError::NotDirectory(path.to_path_buf()));
        }
        return Err(StorageBrowserError::PathNotFound(path.to_path_buf()));
    }
    let mut children: Vec<_> = entries
        .into_iter()
        .filter(|entry| entry.path.parent() == Some(path))
        .collect();
    children.sort_by(|a, b| a.path.cmp(&b.path));
    let total_children = children.len();
    let rows: Vec<_> = children.into_iter().map(item_from_entry).collect();
    let (items, has_more) = page(&rows, offset, limit);
    Ok(ChildrenPage {
        parent: path.to_path_buf(),
        items,
        offset,
        limit,
        total_children,
        has_more,
        incomplete: report_incomplete(report),
        dates: DateAvailability::default(),
    })
}

/// Alias for callers using the shorter browser vocabulary.
pub fn children(
    report: &ScanReport,
    path: &Path,
    offset: usize,
    limit: usize,
) -> Result<ChildrenPage, StorageBrowserError> {
    drilldown_children(report, path, offset, limit)
}

/// Inspect one scanned entry or folder aggregate without touching the host
/// filesystem.
pub fn inspect(report: &ScanReport, path: &Path) -> Result<StorageInspection, StorageBrowserError> {
    let normalized = normalize_input(report, path);
    let path = normalized.as_path();
    validate_path(report, path)?;
    let entries = canonical_entries(report);
    let entry = entries.iter().find(|entry| entry.path == path).copied();
    let folder = canonical_folders(report)
        .into_iter()
        .find(|folder| folder.path == path);
    if entry.is_none() && folder.is_none() {
        return Err(StorageBrowserError::PathNotFound(path.to_path_buf()));
    }
    let (kind, volume, logical_size, allocation_size, attributed, reclaim, metadata_incomplete) =
        if let (Some(entry), Some(folder)) = (entry, folder.as_ref())
            && entry.metadata.kind == EntryKind::Directory
        {
            // Directory entries carry only own-size zero; folder aggregates
            // carry descendant totals and are the inspector's useful values.
            (
                EntryKind::Directory,
                folder.volume.clone(),
                Some(folder.logical_bytes),
                Some(folder.attributed_allocation_bytes),
                folder.attributed_allocation_bytes,
                None,
                folder.incomplete,
            )
        } else if let Some(entry) = entry {
            (
                entry.metadata.kind,
                entry.metadata.volume.clone(),
                entry.metadata.logical_size,
                entry.metadata.allocation_size,
                entry.attributed_allocation_bytes,
                entry.reclaim.clone(),
                !entry.metadata.metadata_complete,
            )
        } else {
            let folder = folder.expect("checked above");
            (
                EntryKind::Directory,
                folder.volume.clone(),
                Some(folder.logical_bytes),
                Some(folder.attributed_allocation_bytes),
                folder.attributed_allocation_bytes,
                None,
                folder.incomplete,
            )
        };
    let mut counts = StorageCounts::default();
    let descendants: Vec<_> = entries
        .iter()
        .filter(|candidate| candidate.path != path && candidate.path.starts_with(path))
        .collect();
    counts.descendant_entries = descendants.len();
    for candidate in &descendants {
        match candidate.metadata.kind {
            EntryKind::File => counts.files += 1,
            EntryKind::Directory => counts.directories += 1,
            EntryKind::Symlink => counts.symlinks += 1,
            EntryKind::Other => counts.other += 1,
        }
        if candidate.path.parent() == Some(path) {
            counts.direct_children += 1;
        }
    }
    let share = entry
        .map(|entry| share_info(entry, &entries))
        .unwrap_or_default();
    let dates = entry
        .map(|entry| dates_from_metadata(&entry.metadata))
        .unwrap_or_default();
    Ok(StorageInspection {
        path: path.to_path_buf(),
        name: basename(path),
        kind,
        volume,
        logical_size,
        allocation_size,
        attributed_allocation_size: attributed,
        reclaim,
        incomplete: report_incomplete(report) || metadata_incomplete,
        counts,
        share,
        dates,
    })
}

fn share_info(entry: &ScannedEntry, entries: &[&ScannedEntry]) -> ShareInfo {
    let file_count = entry.metadata.file_id.as_ref().map(|id| {
        entries
            .iter()
            .filter(|candidate| candidate.metadata.file_id.as_ref() == Some(id))
            .count()
    });
    let clone_count = entry.metadata.clone_id.as_ref().map(|id| {
        entries
            .iter()
            .filter(|candidate| candidate.metadata.clone_id.as_ref() == Some(id))
            .count()
    });
    ShareInfo {
        file_id: entry.metadata.file_id.clone(),
        clone_id: entry.metadata.clone_id.clone(),
        paths_with_same_file_id: file_count,
        paths_with_same_clone_id: clone_count,
        hardlink_shared: file_count.map(|count| count > 1),
        clone_shared: clone_count.map(|count| count > 1),
    }
}

/// Alias retained for CLI callers describing the operation as inspection.
pub fn inspect_path(
    report: &ScanReport,
    path: &Path,
) -> Result<StorageInspection, StorageBrowserError> {
    inspect(report, path)
}
