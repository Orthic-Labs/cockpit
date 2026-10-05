//! Bounded, metadata-only filesystem scanning.

use crate::model::*;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::platform;

/// Hard per-directory enumeration budget. A directory is never read past
/// `min(remaining global entries, this)` entries; the rest is reported as
/// truncation. Selection among the entries actually read is sorted, but when
/// truncated it is NOT a lexicographic prefix of the directory: the OS
/// decides which entries were read first.
pub const DIRECTORY_ENUMERATION_BUDGET: usize = 50_000;

/// Filesystem boundary used by the scanner. Implementations must inspect
/// directory entries without opening file contents and must report symlinks as
/// symlinks. The scanner never calls a method that can hydrate a placeholder.
pub trait FilesystemProvider {
    fn inspect(&self, path: &Path) -> Result<FileMetadata, FsError>;
    /// Like `inspect`, plus a reason for each field that is unavailable.
    /// The default carries no reasons; the scanner then derives generic ones.
    fn inspect_detailed(&self, path: &Path) -> Result<(FileMetadata, Vec<String>), FsError> {
        self.inspect(path).map(|metadata| (metadata, Vec::new()))
    }
    fn children(&self, path: &Path) -> Result<Vec<PathBuf>, FsError>;
    /// Default implementation must read everything via `children`; real
    /// providers should override it to stop reading after `limit` entries.
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

fn map_io(e: std::io::Error) -> FsError {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        FsError::permission_denied(e.to_string())
    } else {
        FsError::new(e.to_string())
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct StdFilesystemProvider;

impl FilesystemProvider for StdFilesystemProvider {
    fn inspect(&self, path: &Path) -> Result<FileMetadata, FsError> {
        self.inspect_detailed(path).map(|(metadata, _)| metadata)
    }

    fn inspect_detailed(&self, path: &Path) -> Result<(FileMetadata, Vec<String>), FsError> {
        let metadata = fs::symlink_metadata(path).map_err(map_io)?;
        let kind = if metadata.file_type().is_symlink() {
            EntryKind::Symlink
        } else if metadata.is_dir() {
            EntryKind::Directory
        } else if metadata.is_file() {
            EntryKind::File
        } else {
            EntryKind::Other
        };
        let native = platform::inspect(path, &metadata);
        let mut reasons = Vec::new();
        let (logical_size, allocation_size, file_id) = match kind {
            EntryKind::File => {
                let logical = if native.is_placeholder {
                    reasons.push("logical size is not local for a placeholder".to_owned());
                    None
                } else {
                    Some(metadata.len())
                };
                (logical, native.allocation_size, native.file_id.clone())
            }
            // Directory own-size is never attributed; the identity is kept
            // for loop detection.
            EntryKind::Directory => (Some(0), Some(0), native.file_id.clone()),
            EntryKind::Symlink | EntryKind::Other => (Some(0), Some(0), None),
        };
        let needs_native = matches!(kind, EntryKind::File | EntryKind::Directory);
        let metadata_complete = !needs_native
            || (allocation_size.is_some()
                && logical_size.is_some()
                && file_id.is_some()
                && native.volume_stable);
        if !metadata_complete {
            reasons.extend(native.unavailable.iter().cloned());
        }
        Ok((
            FileMetadata {
                kind,
                volume: native.volume,
                logical_size,
                allocation_size,
                file_id,
                clone_id: None,
                is_placeholder: native.is_placeholder,
                metadata_complete,
            },
            reasons,
        ))
    }

    fn children(&self, path: &Path) -> Result<Vec<PathBuf>, FsError> {
        let mut children = Vec::new();
        for entry in fs::read_dir(path).map_err(map_io)? {
            children.push(entry.map_err(map_io)?.path());
        }
        children.sort();
        Ok(children)
    }

    /// Reads at most `limit` entries plus one probe entry, then stops; the
    /// directory is never enumerated in full to choose a sorted prefix.
    fn children_bounded(&self, path: &Path, limit: usize) -> Result<(Vec<PathBuf>, bool), FsError> {
        let mut children = Vec::new();
        let mut truncated = false;
        for entry in fs::read_dir(path).map_err(map_io)? {
            let entry = entry.map_err(map_io)?;
            if children.len() >= limit {
                truncated = true;
                break;
            }
            children.push(entry.path());
        }
        children.sort();
        Ok((children, truncated))
    }

    // statvfs field widths vary across Unix ABIs.
    #[allow(clippy::unnecessary_cast)]
    fn volume_usage(&self, volume: &VolumeIdentity) -> Result<VolumeUsage, FsError> {
        #[cfg(unix)]
        {
            use std::ffi::CString;
            use std::os::unix::ffi::OsStrExt;
            for disk in sysinfo::Disks::new_with_refreshed_list().list() {
                let mount = disk.mount_point();
                // Same identity function as inspection, so UUID ids match.
                match platform::volume_identity_for_path(mount) {
                    Some((mount_volume, _)) if mount_volume.id == volume.id => {}
                    _ => continue,
                }
                let Ok(path) = CString::new(mount.as_os_str().as_bytes()) else {
                    continue;
                };
                let mut statistics = std::mem::MaybeUninit::<libc::statvfs>::uninit();
                // statvfs reads mount accounting only, never file contents.
                if unsafe { libc::statvfs(path.as_ptr(), statistics.as_mut_ptr()) } != 0 {
                    continue;
                }
                let statistics = unsafe { statistics.assume_init() };
                let unit = statistics.f_frsize as u64;
                let total = (statistics.f_blocks as u64).saturating_mul(unit);
                let free = (statistics.f_bfree as u64).saturating_mul(unit);
                let available = (statistics.f_bavail as u64).saturating_mul(unit);
                return Ok(VolumeUsage {
                    volume: volume.clone(),
                    total_bytes: Some(total),
                    used_bytes: Some(total.saturating_sub(free)),
                    available_bytes: Some(available),
                    purgeable_bytes: None,
                    snapshots: SnapshotState::Unknown,
                });
            }
        }
        // Unsupported or inaccessible mount accounting stays explicitly unknown.
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
    report.accounting.incomplete = !report.incomplete_reasons.is_empty()
        || !report.inspection_errors.is_empty()
        || !report.skipped_links.is_empty();
    if report.accounting.incomplete {
        report.accounting.reclaim.upper_bytes = None;
        report
            .accounting
            .reclaim
            .reasons
            .push("inspection incomplete; full-selection upper bound unavailable".into());
    }
    let mut folders: BTreeMap<PathBuf, FolderAccounting> = report
        .entries
        .iter()
        .filter(|entry| entry.metadata.kind == EntryKind::Directory)
        .map(|entry| {
            (
                entry.path.clone(),
                FolderAccounting {
                    path: entry.path.clone(),
                    volume: entry.metadata.volume.clone(),
                    logical_bytes: 0,
                    attributed_allocation_bytes: 0,
                    incomplete: report.accounting.incomplete,
                },
            )
        })
        .collect();
    for entry in report
        .entries
        .iter()
        .filter(|entry| entry.metadata.kind == EntryKind::File)
    {
        for ancestor in entry.path.ancestors().skip(1) {
            if let Some(folder) = folders.get_mut(ancestor)
                && folder.volume == entry.metadata.volume
            {
                folder.logical_bytes = folder.logical_bytes.saturating_add(entry.logical_bytes);
                folder.attributed_allocation_bytes = folder
                    .attributed_allocation_bytes
                    .saturating_add(entry.attributed_allocation_bytes);
            }
        }
    }
    report.folders = folders.into_values().collect();
    report.folders.sort_by(|a, b| {
        b.attributed_allocation_bytes
            .cmp(&a.attributed_allocation_bytes)
            .then_with(|| a.path.cmp(&b.path))
    });
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
    // Recheck ancestors before each inspection: a previously visited parent may change.
    if let Some((ancestor, uninspectable)) = symlink_ancestor(provider, &path, report) {
        if uninspectable {
            report.incomplete_reasons.push(format!(
                "ancestor not inspectable, root skipped: {}",
                path.display()
            ));
        } else {
            report.skipped_links.push(SkippedLink {
                path: ancestor,
                reason: "symlink or placeholder ancestor traversal disabled".into(),
            });
        }
        return;
    }
    let (mut metadata, provider_reasons) = match provider.inspect_detailed(&path) {
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
    if let Some(parent_volume) = expected_volume.as_ref()
        && &metadata.volume != parent_volume
    {
        report.incomplete_reasons.push(format!(
            "cross-volume descendant rejected: {}",
            path.display()
        ));
        return;
    }
    // Do not trust a provider's completeness flag when required file fields
    // are absent: missing metadata is explicit, never silently zero.
    if metadata.kind == EntryKind::File
        && (metadata.logical_size.is_none()
            || metadata.allocation_size.is_none()
            || metadata.file_id.is_none())
    {
        metadata.metadata_complete = false;
    }
    if !metadata.metadata_complete {
        report
            .incomplete_reasons
            .push(format!("incomplete metadata: {}", path.display()));
        let mut reasons = provider_reasons;
        {
            if metadata.kind != EntryKind::Directory && metadata.logical_size.is_none() {
                reasons.push("logical size unavailable".into());
            }
            if metadata.kind == EntryKind::File && metadata.allocation_size.is_none() {
                reasons.push("allocation size unavailable".into());
            }
            if metadata.file_id.is_none() {
                reasons.push("file id unavailable".into());
            }
        }
        for reason in reasons {
            report
                .incomplete_reasons
                .push(format!("metadata unavailable: {}: {reason}", path.display()));
        }
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
        if !metadata.metadata_complete {
            reasons.push("metadata incomplete".into());
        }
        if metadata.is_placeholder {
            reasons.push("placeholder content is not local".into());
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
            if metadata.file_id.is_some() {
                attributed = metadata.allocation_size.unwrap_or(0);
            } else {
                candidate_upper = None;
                reasons.push("file identity unavailable; unique allocation not attributed".into());
            }
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
    if metadata.is_placeholder {
        // Enumerating a placeholder directory can hydrate it.
        report.incomplete_reasons.push(format!(
            "placeholder directory not enumerated: {}",
            path.display()
        ));
        return;
    }
    if let Some(id) = metadata.file_id.clone()
        && !seen_dirs.insert(id)
    {
        report.incomplete_reasons.push(format!(
            "directory identity already visited: {}",
            path.display()
        ));
        return;
    }
    let remaining = options.max_entries.saturating_sub(seen_paths.len());
    let budget = remaining.min(DIRECTORY_ENUMERATION_BUDGET);
    // Recheck this directory immediately before & after listing. Changes discard
    // the listing; pathname checks are conservative observations, not a sandbox.
    if !directory_unchanged(provider, &path, &metadata, report) {
        return;
    }
    let (children, truncated) = match provider.children_bounded(&path, budget) {
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
    if !directory_unchanged(provider, &path, &metadata, report) {
        return;
    }
    if truncated && budget < remaining {
        report.incomplete_reasons.push(format!(
            "directory enumeration budget {} reached, remaining entries unread below {}",
            DIRECTORY_ENUMERATION_BUDGET,
            path.display()
        ));
    } else if truncated {
        report.incomplete_reasons.push(format!(
            "entry limit {} reached below {}",
            options.max_entries,
            path.display()
        ));
    }
    let mut children = children;
    children.sort();
    children.dedup();
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
) -> Option<(PathBuf, bool)> {
    let ancestors: Vec<_> = path
        .ancestors()
        .skip(1)
        .filter(|p| !p.as_os_str().is_empty())
        .collect();
    for candidate in ancestors.into_iter().rev() {
        match provider.inspect(candidate) {
            Ok(metadata) if metadata.kind == EntryKind::Symlink || metadata.is_placeholder => {
                return Some((candidate.to_path_buf(), false));
            }
            Ok(_) => {}
            Err(error) => {
                report.inspection_errors.push(InspectionError {
                    path: candidate.to_path_buf(),
                    operation: "inspect ancestor".into(),
                    message: error.to_string(),
                });
                return Some((candidate.to_path_buf(), true));
            }
        }
    }
    None
}

fn directory_unchanged<P: FilesystemProvider>(provider: &P, path: &Path, expected: &FileMetadata, report: &mut ScanReport) -> bool {
    match provider.inspect(path) {
        Ok(current) if current.kind == EntryKind::Directory
            && !current.is_placeholder && current.volume == expected.volume
            && current.file_id == expected.file_id => true,
        Ok(current) => {
            if current.kind == EntryKind::Symlink || current.is_placeholder {
                report.skipped_links.push(SkippedLink { path: path.into(), reason: "directory changed to link or placeholder during enumeration".into() });
            }
            report.incomplete_reasons.push(format!("directory changed during enumeration: {}", path.display()));
            false
        }
        Err(error) => {
            report.inspection_errors.push(InspectionError { path: path.into(), operation: "recheck_directory".into(), message: error.to_string() });
            false
        }
    }
}
