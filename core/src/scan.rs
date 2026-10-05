//! Bounded, metadata-only filesystem scanning.

use crate::model::*;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
use std::os::macos::fs::MetadataExt as MacMetadataExt;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(windows)]
use std::os::windows::fs::MetadataExt as WindowsMetadataExt;

/// Filesystem boundary used by the scanner. Implementations must inspect
/// directory entries without opening file contents and must report symlinks as
/// symlinks. The scanner never calls a method that can hydrate a placeholder.
pub trait FilesystemProvider {
    fn inspect(&self, path: &Path) -> Result<FileMetadata, FsError>;
    fn children(&self, path: &Path) -> Result<Vec<PathBuf>, FsError>;
    fn children_bounded(&self, path: &Path, limit: usize) -> Result<(Vec<PathBuf>, bool), FsError> {
        let mut children = self.children(path)?;
        children.sort();
        let truncated = children.len() > limit;
        if truncated {
            children.truncate(limit);
        }
        Ok((children, truncated))
    }
    fn volume_usage(&self, volume: &VolumeIdentity) -> Result<VolumeUsage, FsError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct StdFilesystemProvider;

impl FilesystemProvider for StdFilesystemProvider {
    fn inspect(&self, path: &Path) -> Result<FileMetadata, FsError> {
        let metadata = fs::symlink_metadata(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                FsError::permission_denied(e.to_string())
            } else {
                FsError::new(e.to_string())
            }
        })?;
        let kind = if metadata.file_type().is_symlink() {
            EntryKind::Symlink
        } else if metadata.is_dir() {
            EntryKind::Directory
        } else if metadata.is_file() {
            EntryKind::File
        } else {
            EntryKind::Other
        };
        let volume = VolumeIdentity::new(volume_id(&metadata, path));
        let (logical_size, allocation_size, file_id) = if kind == EntryKind::File {
            let logical = Some(metadata.len());
            #[cfg(unix)]
            let allocation = Some(metadata.blocks().saturating_mul(512));
            #[cfg(not(unix))]
            let allocation = None;
            #[cfg(unix)]
            let identity = Some(FileIdentity {
                volume: volume.clone(),
                id: format!("{}:{}", metadata.dev(), metadata.ino()),
            });
            #[cfg(not(unix))]
            let identity = None;
            (logical, allocation, identity)
        } else {
            (Some(0), Some(0), None)
        };
        let is_placeholder = dataless_or_placeholder(&metadata);
        let metadata_complete =
            kind != EntryKind::File || (allocation_size.is_some() && file_id.is_some());
        Ok(FileMetadata {
            kind,
            volume,
            logical_size,
            allocation_size,
            file_id,
            clone_id: None,
            is_placeholder,
            metadata_complete,
        })
    }

    fn children(&self, path: &Path) -> Result<Vec<PathBuf>, FsError> {
        let mut children = Vec::new();
        let entries = fs::read_dir(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                FsError::permission_denied(e.to_string())
            } else {
                FsError::new(e.to_string())
            }
        })?;
        for entry in entries {
            match entry {
                Ok(entry) => children.push(entry.path()),
                Err(e) => {
                    return Err(if e.kind() == std::io::ErrorKind::PermissionDenied {
                        FsError::permission_denied(e.to_string())
                    } else {
                        FsError::new(e.to_string())
                    });
                }
            }
        }
        children.sort();
        Ok(children)
    }

    fn children_bounded(&self, path: &Path, limit: usize) -> Result<(Vec<PathBuf>, bool), FsError> {
        let mut children = Vec::new();
        let mut truncated = false;
        let entries = fs::read_dir(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                FsError::permission_denied(e.to_string())
            } else {
                FsError::new(e.to_string())
            }
        })?;
        for entry in entries {
            match entry {
                Ok(entry) if children.len() < limit => children.push(entry.path()),
                Ok(_) => {
                    truncated = true;
                    break;
                }
                Err(e) => {
                    return Err(if e.kind() == std::io::ErrorKind::PermissionDenied {
                        FsError::permission_denied(e.to_string())
                    } else {
                        FsError::new(e.to_string())
                    });
                }
            }
        }
        children.sort();
        Ok((children, truncated))
    }

    fn volume_usage(&self, volume: &VolumeIdentity) -> Result<VolumeUsage, FsError> {
        // Portable std metadata has no reliable volume accounting API. Keep
        // this explicitly unavailable instead of deriving a false total.
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

#[cfg(unix)]
fn volume_id(metadata: &fs::Metadata, _path: &Path) -> String {
    metadata.dev().to_string()
}

#[cfg(not(unix))]
fn volume_id(_metadata: &fs::Metadata, path: &Path) -> String {
    path.components()
        .next()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".to_owned())
}

#[cfg(target_os = "macos")]
fn dataless_or_placeholder(metadata: &fs::Metadata) -> bool {
    // UF_DATAlESS. Reading stat flags does not hydrate an item.
    metadata.st_flags() & 0x4000_0000 != 0
}

#[cfg(windows)]
fn dataless_or_placeholder(metadata: &fs::Metadata) -> bool {
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    const FILE_ATTRIBUTE_OFFLINE: u32 = 0x1000;
    const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x40000;
    const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x400000;
    let attributes = metadata.file_attributes();
    attributes
        & (FILE_ATTRIBUTE_REPARSE_POINT
            | FILE_ATTRIBUTE_OFFLINE
            | FILE_ATTRIBUTE_RECALL_ON_OPEN
            | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
        != 0
}

#[cfg(not(any(target_os = "macos", windows)))]
fn dataless_or_placeholder(_metadata: &fs::Metadata) -> bool {
    false
}

/// Scan roots in lexical order. Roots are inspected through the provider,
/// including placeholder checks, then descendants are bounded by both depth
/// and entry count. Overlapping roots and hard links are deterministic.
pub fn scan_with_provider<P: FilesystemProvider>(
    provider: &P,
    paths: &[PathBuf],
    options: &ScanOptions,
) -> ScanReport {
    let mut roots = paths.to_vec();
    roots.sort();
    roots.dedup();
    let mut report = ScanReport {
        roots: roots.clone(),
        entries: Vec::new(),
        folders: Vec::new(),
        accounting: Accounting {
            reclaim: ReclaimEstimateSummary {
                upper_bytes: Some(0),
                ..Default::default()
            },
            ..Default::default()
        },
        volume_usage: Vec::new(),
        volume_deltas: options.volume_deltas.clone(),
        inspection_errors: Vec::new(),
        skipped_links: Vec::new(),
        incomplete_reasons: Vec::new(),
    };
    let mut seen_paths = BTreeSet::new();
    let mut seen_files: HashMap<FileIdentity, PathBuf> = HashMap::new();
    let mut seen_dirs: HashSet<FileIdentity> = HashSet::new();
    let mut volumes = BTreeSet::new();
    for root in roots {
        if seen_paths.len() >= options.max_entries {
            report
                .incomplete_reasons
                .push("entry limit reached across roots".into());
            break;
        }
        walk(
            provider,
            root,
            0,
            options,
            None,
            &mut report,
            &mut seen_paths,
            &mut seen_files,
            &mut seen_dirs,
            &mut volumes,
        );
    }
    let mut usage_by_volume = BTreeMap::new();
    let mut attributed_by_volume: BTreeMap<VolumeIdentity, u64> = BTreeMap::new();
    for entry in &report.entries {
        let value = attributed_by_volume
            .entry(entry.metadata.volume.clone())
            .or_default();
        *value = value.saturating_add(entry.attributed_allocation_bytes);
    }
    for volume in volumes {
        match provider.volume_usage(&volume) {
            Ok(usage) => {
                if let Some(used) = usage.used_bytes {
                    let discrepancy = i128::from(*attributed_by_volume.get(&volume).unwrap_or(&0))
                        - i128::from(used);
                    if attributed_by_volume.len() == 1 {
                        report.accounting.signed_discrepancy_bytes = Some(discrepancy);
                    }
                    report
                        .accounting
                        .volume_discrepancies
                        .push(VolumeDiscrepancy {
                            volume: volume.clone(),
                            scanned_minus_used_bytes: discrepancy,
                        });
                }
                usage_by_volume.insert(volume, usage);
            }
            Err(error) => report.inspection_errors.push(InspectionError {
                path: PathBuf::from(format!("<volume:{}>", volume.id)),
                operation: "volume_usage".into(),
                message: error.to_string(),
            }),
        }
    }
    report.volume_usage = usage_by_volume.into_values().collect();
    report.accounting.incomplete =
        !report.incomplete_reasons.is_empty() || !report.inspection_errors.is_empty() || !report.skipped_links.is_empty();
    if report.accounting.incomplete {
        report.accounting.reclaim.upper_bytes = None;
        report
            .accounting
            .reclaim
            .reasons
            .push("inspection incomplete; full-selection upper bound unavailable".into());
    }
    let mut folders: BTreeMap<PathBuf, FolderAccounting> = report.entries.iter()
        .filter(|entry| entry.metadata.kind == EntryKind::Directory)
        .map(|entry| (entry.path.clone(), FolderAccounting {
            path: entry.path.clone(), volume: entry.metadata.volume.clone(), logical_bytes: 0,
            attributed_allocation_bytes: 0, incomplete: report.accounting.incomplete,
        })).collect();
    for entry in report.entries.iter().filter(|entry| entry.metadata.kind == EntryKind::File) {
        for ancestor in entry.path.ancestors().skip(1) {
            if let Some(folder) = folders.get_mut(ancestor)
                && folder.volume == entry.metadata.volume
            {
                    folder.logical_bytes = folder.logical_bytes.saturating_add(entry.logical_bytes);
                    folder.attributed_allocation_bytes = folder.attributed_allocation_bytes.saturating_add(entry.attributed_allocation_bytes);
            }
        }
    }
    report.folders = folders.into_values().collect();
    report.folders.sort_by(|a,b| b.attributed_allocation_bytes.cmp(&a.attributed_allocation_bytes).then_with(|| a.path.cmp(&b.path)));
    report
}

pub fn scan(paths: &[PathBuf], options: &ScanOptions) -> ScanReport {
    scan_with_provider(&StdFilesystemProvider, paths, options)
}

pub fn scan_paths(paths: &[PathBuf], options: &ScanOptions) -> ScanReport {
    scan(paths, options)
}

#[allow(clippy::too_many_arguments)] // Traversal shares bounded scan state; no public API exposes these parameters.
fn walk<P: FilesystemProvider>(
    provider: &P,
    path: PathBuf,
    depth: usize,
    options: &ScanOptions,
    expected_volume: Option<VolumeIdentity>,
    report: &mut ScanReport,
    seen_paths: &mut BTreeSet<PathBuf>,
    seen_files: &mut HashMap<FileIdentity, PathBuf>,
    seen_dirs: &mut HashSet<FileIdentity>,
    volumes: &mut BTreeSet<VolumeIdentity>,
) {
    if seen_paths.len() >= options.max_entries {
        report
            .incomplete_reasons
            .push(format!("entry limit {} reached", options.max_entries));
        return;
    }
    if !seen_paths.insert(path.clone()) {
        return;
    }
    if let Some(ancestor) = symlink_ancestor(provider, &path, report) {
        report.skipped_links.push(SkippedLink {
            path: ancestor,
            reason: "symlink or placeholder ancestor traversal disabled".into(),
        });
        return;
    }
    let metadata = match provider.inspect(&path) {
        Ok(value) => value,
        Err(error) => {
            report.inspection_errors.push(InspectionError {
                path,
                operation: "inspect".into(),
                message: error.to_string(),
            });
            return;
        }
    };
    if metadata.kind == EntryKind::Symlink {
        report.skipped_links.push(SkippedLink {
            path,
            reason: "symlink traversal disabled".into(),
        });
        return;
    }
    if metadata.is_placeholder && options.reject_placeholders {
        report
            .incomplete_reasons
            .push(format!("placeholder rejected: {}", path.display()));
        return;
    }
    if !metadata.metadata_complete {
        report
            .incomplete_reasons
            .push(format!("incomplete metadata: {}", path.display()));
    }
    if let Some(parent_volume) = expected_volume.as_ref()
        && &metadata.volume != parent_volume
    {
            report.incomplete_reasons.push(format!(
                "cross-volume descendant rejected: {}",
                path.display()
            ));
        return;
    }
    let volume = metadata.volume.clone();
    volumes.insert(volume.clone());
    let is_file = metadata.kind == EntryKind::File;
    let mut logical = 0;
    let mut attributed = 0;
    let mut owner = None;
    let mut reclaim = None;
    if is_file {
        let mut candidate_upper = metadata.allocation_size;
        let mut reasons = Vec::new();
        if metadata.allocation_size.is_none() {
            reasons.push("allocation metadata unavailable".into());
        }
        reasons.push(
            if metadata.clone_id.is_some() {
                "clone sharing is unknown"
            } else {
                "clone/snapshot sharing is not proven"
            }
            .into(),
        );
        let hardlink_owner = metadata
            .file_id
            .as_ref()
            .and_then(|id| seen_files.get(id).cloned());
        if let Some(existing) = hardlink_owner {
            owner = Some(existing);
            candidate_upper = Some(0);
            reasons.push("hard-link identity already attributed".into());
        } else {
            logical = metadata.logical_size.unwrap_or(0);
            attributed = metadata.allocation_size.unwrap_or(0);
            if let Some(id) = metadata.file_id.clone() {
                seen_files.insert(id, path.clone());
                owner = Some(path.clone());
            }
        }
        let mut estimate = ReclaimEstimate {
            lower_bytes: 0,
            upper_bytes: candidate_upper,
            state: ReclaimState::Unknown,
            reasons,
        };
        if estimate.reasons.is_empty() {
            estimate
                .reasons
                .push("retention and sharing state are not proven".into());
        }
        reclaim = Some(estimate);
        report.accounting.logical_bytes = report.accounting.logical_bytes.saturating_add(logical);
        report.accounting.attributed_allocation_bytes = report
            .accounting
            .attributed_allocation_bytes
            .saturating_add(attributed);
        report.accounting.reclaim.upper_bytes =
            match (report.accounting.reclaim.upper_bytes, candidate_upper) {
                (Some(total), Some(value)) => total.checked_add(value),
                _ => None,
            };
        report.accounting.reclaim.state = Some(ReclaimState::Unknown);
        if let Some(reclaim) = reclaim.as_ref() {
            report
                .accounting
                .reclaim
                .reasons
                .extend(reclaim.reasons.clone());
        }
    }
    report.entries.push(ScannedEntry {
        path: path.clone(),
        metadata: metadata.clone(),
        logical_bytes: logical,
        attributed_allocation_bytes: attributed,
        accounting_owner: owner,
        reclaim,
    });
    if metadata.kind != EntryKind::Directory {
        return;
    }
    if depth >= options.max_depth {
        report.incomplete_reasons.push(format!(
            "depth limit {} reached at {}",
            options.max_depth,
            path.display()
        ));
        return;
    }
    if let Some(id) = metadata.file_id.clone()
        && !seen_dirs.insert(id)
    {
        return;
    }
    let remaining = options.max_entries.saturating_sub(seen_paths.len());
    let (children, truncated) = match provider.children_bounded(&path, remaining) {
        Ok(children) => children,
        Err(error) => {
            report.inspection_errors.push(InspectionError {
                path,
                operation: "enumerate".into(),
                message: error.to_string(),
            });
            return;
        }
    };
    if truncated {
        report.incomplete_reasons.push(format!(
            "entry limit {} reached below {}",
            options.max_entries,
            path.display()
        ));
    }
    let mut children = children;
    children.sort();
    for child in children {
        if seen_paths.len() >= options.max_entries {
            break;
        }
        walk(
            provider,
            child,
            depth + 1,
            options,
            Some(volume.clone()),
            report,
            seen_paths,
            seen_files,
            seen_dirs,
            volumes,
        );
    }
}

fn symlink_ancestor<P: FilesystemProvider>(
    provider: &P,
    path: &Path,
    report: &mut ScanReport,
) -> Option<PathBuf> {
    let ancestors: Vec<_> = path.ancestors().skip(1).filter(|p| !p.as_os_str().is_empty()).collect();
    for candidate in ancestors.into_iter().rev() {
        match provider.inspect(candidate) {
            Ok(metadata) if metadata.kind == EntryKind::Symlink || metadata.is_placeholder => {
                return Some(candidate.to_path_buf());
            }
            Ok(_) => {}
            Err(error) => {
                report.inspection_errors.push(InspectionError {
                    path: candidate.to_path_buf(),
                    operation: "inspect ancestor".into(),
                    message: error.to_string(),
                });
                return Some(candidate.to_path_buf());
            }
        }
    }
    None
}
