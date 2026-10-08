//! Bounded, metadata-only filesystem scanning.
//!
//! The read-ahead (`Pipeline`) follows the parallel-walk idea in Petal's
//! `src/scan.rs` (MIT, Copyright (c) 2026 Henry Dennis; see `docs/donors.md`),
//! rebuilt on standard-library threads. Its decisions stay with this walk.

use crate::model::*;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::UNIX_EPOCH;

use crate::platform;

/// Hard per-directory enumeration budget. A directory is never read past
/// `min(remaining global entries, this)` entries; the rest is reported as
/// truncation. Selection among the entries actually read is sorted, but when
/// truncated it is NOT a lexicographic prefix of the directory: the OS
/// decides which entries were read first.
pub const DIRECTORY_ENUMERATION_BUDGET: usize = 500_000;

/// Filesystem boundary used by the scanner. Implementations must inspect
/// directory entries without opening file contents and must report symlinks as
/// symlinks. The scanner never calls a method that can hydrate a placeholder.
/// Children with the file facts read alongside them, and whether more were left unread.
pub type ChildrenWithFiles = (Vec<(PathBuf, Option<FileMetadata>)>, bool);

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
    /// Bounded listing that may also return a regular file's metadata, read in
    /// the same call (`Some`). `None` means the entry is inspected as usual.
    /// The default lists names only.
    fn children_with_files(&self, path: &Path, limit: usize) -> Result<ChildrenWithFiles, FsError> {
        self.children_bounded(path, limit)
            .map(|(children, truncated)| {
                (
                    children.into_iter().map(|child| (child, None)).collect(),
                    truncated,
                )
            })
    }
    fn volume_usage(&self, volume: &VolumeIdentity) -> Result<VolumeUsage, FsError>;
    /// Called once at the start of each scan so providers can reset per-scan
    /// caches. The default does nothing.
    fn begin_scan(&self) {}
}

fn map_io(e: std::io::Error) -> FsError {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        FsError::permission_denied(e.to_string())
    } else {
        FsError::new(e.to_string())
    }
}

/// Mount identity observed with `statfs`, used to validate a cached volume.
/// Includes opaque mount fsid bytes without accessing libc's private fields.
#[cfg(target_os = "macos")]
#[derive(Clone, Debug, PartialEq, Eq)]
struct MountKey {
    dev: u64,
    mount: Vec<u8>,
    fsid: [u8; std::mem::size_of::<libc::fsid_t>()],
}

#[cfg(target_os = "macos")]
fn mount_key(path: &Path, metadata: &fs::Metadata) -> Option<MountKey> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    // Query the parent for a symlink so statfs never follows its target.
    let query = if metadata.file_type().is_symlink() {
        path.parent()?
    } else {
        path
    };
    let c_path = std::ffi::CString::new(query.as_os_str().as_bytes()).ok()?;
    let mut statistics = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    if unsafe { libc::statfs(c_path.as_ptr(), statistics.as_mut_ptr()) } != 0 {
        return None;
    }
    let statistics = unsafe { statistics.assume_init() };
    let mount = unsafe { std::ffi::CStr::from_ptr(statistics.f_mntonname.as_ptr()) };
    Some(MountKey {
        dev: metadata.dev(),
        mount: mount.to_bytes().to_vec(),
        // SAFETY: Apple fsid_t is a C struct containing two i32 values,
        // with no padding. A successful statfs initializes this mount ID.
        fsid: unsafe {
            std::mem::transmute::<libc::fsid_t, [u8; std::mem::size_of::<libc::fsid_t>()]>(
                statistics.f_fsid,
            )
        },
    })
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct CachedVolume {
    volume: VolumeIdentity,
    key: MountKey,
}

/// State owned by one `CachingStdProvider`; never shared or process-global.
#[derive(Debug)]
struct ScanState {
    /// st_dev -> stable volume identity (macOS UUID lookups only).
    #[cfg(target_os = "macos")]
    volumes: BTreeMap<u64, CachedVolume>,
    /// Directory (st_dev, st_ino) recorded at inspection time.
    #[cfg(unix)]
    directories: HashMap<PathBuf, (u64, u64)>,
}

impl ScanState {
    fn new() -> Self {
        Self {
            #[cfg(target_os = "macos")]
            volumes: BTreeMap::new(),
            #[cfg(unix)]
            directories: HashMap::new(),
        }
    }
}

/// Shared inspection; `native` supplies the platform facts for the entry.
fn inspect_with(
    path: &Path,
    native: impl FnOnce(&Path, &fs::Metadata, EntryKind) -> platform::NativeInfo,
) -> Result<(FileMetadata, Vec<String>), FsError> {
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
    let native = native(path, &metadata, kind);
    let mut reasons = Vec::new();
    let created_at = metadata
        .created()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs());
    if created_at.is_none() {
        reasons.push("created_at unavailable or before Unix epoch".to_owned());
    }
    let modified_at = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs());
    if modified_at.is_none() {
        reasons.push("modified_at unavailable or before Unix epoch".to_owned());
    }
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
        EntryKind::Symlink => (Some(0), Some(0), None),
        // st_blocks is meaningless for device/FIFO/socket/other entries:
        // allocation is unknown, never a confident zero.
        EntryKind::Other => {
            reasons.push("allocation is not meaningful for a non-regular entry".to_owned());
            (Some(0), None, None)
        }
    };
    let needs_native = matches!(kind, EntryKind::File | EntryKind::Directory);
    let metadata_complete = kind != EntryKind::Other
        && (!needs_native
            || (allocation_size.is_some()
                && logical_size.is_some()
                && file_id.is_some()
                && native.volume_stable));
    if !metadata_complete {
        reasons.extend(native.unavailable.iter().cloned());
    }
    // Identity recheck: reject an entry that was replaced while it was being
    // inspected, so a stale observation never reaches the report. On unix the
    // (st_dev, st_ino) pair is authoritative; elsewhere the file type is the
    // conservative proxy. This cannot detect a swap between enumeration and
    // the first lstat — the provider seam does not return enumeration-time
    // identity — so it is a conservative post-check only.
    //
    // Only directories are rechecked: a regular file is read once (one lstat
    // per entry), and a directory that is swapped is refused again before it
    // is listed (`children_bounded`, `directory_unchanged`).
    let same_identity = if kind == EntryKind::Directory {
        let after = fs::symlink_metadata(path).map_err(map_io)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            after.dev() == metadata.dev()
                && after.ino() == metadata.ino()
                && after.file_type() == metadata.file_type()
        }
        #[cfg(not(unix))]
        {
            after.file_type() == metadata.file_type()
        }
    } else {
        true
    };
    if !same_identity {
        return Err(FsError::new(format!(
            "entry identity changed during inspection: {}",
            path.display()
        )));
    }
    Ok((
        FileMetadata {
            kind,
            volume: native.volume,
            logical_size,
            allocation_size,
            file_id,
            clone_id: None,
            created_at,
            modified_at,
            is_placeholder: native.is_placeholder,
            metadata_complete,
        },
        reasons,
    ))
}

/// Stateless real-filesystem provider: no cache, no recorded identities.
#[derive(Clone, Copy, Debug, Default)]
pub struct StdFilesystemProvider;

impl FilesystemProvider for StdFilesystemProvider {
    fn inspect(&self, path: &Path) -> Result<FileMetadata, FsError> {
        self.inspect_detailed(path).map(|(metadata, _)| metadata)
    }

    fn inspect_detailed(&self, path: &Path) -> Result<(FileMetadata, Vec<String>), FsError> {
        inspect_with(path, |path, metadata, _| platform::inspect(path, metadata))
    }

    fn children(&self, path: &Path) -> Result<Vec<PathBuf>, FsError> {
        std_children(path)
    }

    /// Descriptor-based, no-follow, bounded listing (see the platform adapter).
    fn children_bounded(&self, path: &Path, limit: usize) -> Result<(Vec<PathBuf>, bool), FsError> {
        platform::children_bounded(path, limit)
    }

    fn volume_usage(&self, volume: &VolumeIdentity) -> Result<VolumeUsage, FsError> {
        platform::volume_usage(volume)
    }
}

fn std_children(path: &Path) -> Result<Vec<PathBuf>, FsError> {
    let mut children = Vec::new();
    for entry in fs::read_dir(path).map_err(map_io)? {
        children.push(entry.map_err(map_io)?.path());
    }
    children.sort();
    Ok(children)
}

/// Real-filesystem provider with per-scan state: a validated volume-UUID cache
/// (macOS) and the identities of directories inspected in this scan. Create
/// one per scan; `begin_scan` also resets it.
#[derive(Debug)]
pub(crate) struct CachingStdProvider {
    state: Mutex<ScanState>,
}

impl CachingStdProvider {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(ScanState::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ScanState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// If the directory was inspected earlier in this scan, its identity must
    /// still match before its listing is opened.
    fn expect_directory(&self, path: &Path) -> Result<(), FsError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let expected = self.lock().directories.remove(path);
            if let Some(expected) = expected {
                let current = fs::symlink_metadata(path).map_err(map_io)?;
                if (current.dev(), current.ino()) != expected {
                    return Err(FsError::new(format!(
                        "directory identity changed since inspection: {}",
                        path.display()
                    )));
                }
            }
        }
        #[cfg(not(unix))]
        let _ = path;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn native_info(
        &self,
        path: &Path,
        metadata: &fs::Metadata,
        kind: EntryKind,
    ) -> platform::NativeInfo {
        use std::os::unix::fs::MetadataExt;
        // Symlinks and special files always take the full uncached lookup.
        if !matches!(kind, EntryKind::File | EntryKind::Directory) {
            return platform::inspect(path, metadata);
        }
        let dev = metadata.dev();
        // A file shares its parent directory's mount: once this st_dev has a
        // validated volume (checked with statfs at a directory), reuse it
        // instead of a statfs per file.
        if kind == EntryKind::File {
            let hit = self.lock().volumes.get(&dev).map(|c| c.volume.clone());
            if let Some(volume) = hit {
                return cached_native_info(path, metadata, volume);
            }
        }
        let key = mount_key(path, metadata);
        if let Some(key) = key.as_ref() {
            let mut state = self.lock();
            let hit = match state.volumes.get(&dev) {
                Some(cached) if &cached.key == key => Some(cached.volume.clone()),
                _ => None,
            };
            if let Some(volume) = hit {
                drop(state);
                return cached_native_info(path, metadata, volume);
            }
            // Absent or the mount changed behind this st_dev: drop the binding.
            state.volumes.remove(&dev);
        }
        let native = platform::inspect(path, metadata);
        // Only a stable UUID is cached, and only if the mount did not change
        // while it was being resolved.
        if native.volume_stable
            && let Some(before) = key
            && mount_key(path, metadata).as_ref() == Some(&before)
        {
            self.lock().volumes.insert(
                dev,
                CachedVolume {
                    volume: native.volume.clone(),
                    key: before,
                },
            );
        }
        native
    }

    #[cfg(not(target_os = "macos"))]
    fn native_info(
        &self,
        path: &Path,
        metadata: &fs::Metadata,
        _kind: EntryKind,
    ) -> platform::NativeInfo {
        platform::inspect(path, metadata)
    }
}

/// Volume comparison that does not mistake a weaker identity encoding of the
/// same volume for a different volume. Windows reports `serial:<64-bit>` when
/// `FILE_ID_INFO` works, but `serial32-unstable:<32-bit>` or
/// `path-prefix-unstable:<drive>` when a handle cannot supply it (placeholders,
/// locked or cloud-backed files). Those fallbacks are not evidence of a
/// different volume: a 32-bit serial is compared with the low 32 bits of the
/// 64-bit one, and a path-prefix fallback proves nothing (such an entry is
/// already reported as incomplete metadata). Two stable ids that differ remain
/// a true cross-volume entry.
fn same_volume(child: &VolumeIdentity, parent: &VolumeIdentity) -> bool {
    if child == parent {
        return true;
    }
    let (Some((ck, cv)), Some((pk, pv))) = (child.id.split_once(':'), parent.id.split_once(':'))
    else {
        return false;
    };
    let low32 = |hex: &str| u64::from_str_radix(hex, 16).ok().map(|v| v & 0xffff_ffff);
    match (ck, pk) {
        ("path-prefix-unstable", _) | (_, "path-prefix-unstable") => true,
        ("serial32-unstable", "serial") | ("serial", "serial32-unstable") => {
            low32(cv).is_some() && low32(cv) == low32(pv)
        }
        ("serial32-unstable", "serial32-unstable") => cv == pv,
        _ => false,
    }
}

/// Same facts as the unix adapter, with a cached (validated) volume.
#[cfg(target_os = "macos")]
fn cached_native_info(
    path: &Path,
    metadata: &fs::Metadata,
    volume: VolumeIdentity,
) -> platform::NativeInfo {
    use std::os::macos::fs::MetadataExt as MacMetadataExt;
    use std::os::unix::fs::MetadataExt;
    let is_placeholder = metadata.st_flags() & 0x4000_0000 != 0;
    let mut unavailable = Vec::new();
    let allocation_size = if is_placeholder {
        unavailable.push(format!(
            "allocation unavailable: {} is a dataless placeholder",
            path.display()
        ));
        None
    } else {
        Some(metadata.blocks().saturating_mul(512))
    };
    platform::NativeInfo {
        file_id: Some(FileIdentity {
            volume: volume.clone(),
            id: {
                use std::fmt::Write;
                let mut id = String::with_capacity(24);
                let _ = write!(id, "{}:{}", metadata.dev(), metadata.ino());
                id
            },
        }),
        volume,
        volume_stable: true,
        allocation_size,
        is_placeholder,
        unavailable,
    }
}

impl FilesystemProvider for CachingStdProvider {
    fn begin_scan(&self) {
        *self.lock() = ScanState::new();
    }

    fn inspect(&self, path: &Path) -> Result<FileMetadata, FsError> {
        self.inspect_detailed(path).map(|(metadata, _)| metadata)
    }

    fn inspect_detailed(&self, path: &Path) -> Result<(FileMetadata, Vec<String>), FsError> {
        inspect_with(path, |path, metadata, kind| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if kind == EntryKind::Directory {
                    self.lock()
                        .directories
                        .insert(path.to_path_buf(), (metadata.dev(), metadata.ino()));
                }
            }
            self.native_info(path, metadata, kind)
        })
    }

    fn children(&self, path: &Path) -> Result<Vec<PathBuf>, FsError> {
        std_children(path)
    }

    /// If the directory was inspected earlier in this scan, its identity must
    /// still match before the descriptor-based listing is opened.
    fn children_bounded(&self, path: &Path, limit: usize) -> Result<(Vec<PathBuf>, bool), FsError> {
        self.expect_directory(path)?;
        platform::children_bounded(path, limit)
    }

    /// macOS: one bulk read gives the names and, for regular files on a volume
    /// this scan has already identified, their metadata, so those files are
    /// not inspected a second time. Everything else is inspected as before.
    #[cfg(target_os = "macos")]
    fn children_with_files(&self, path: &Path, limit: usize) -> Result<ChildrenWithFiles, FsError> {
        self.expect_directory(path)?;
        let (listed, truncated) = platform::bulk_children_bounded(path, limit)?;
        let state = self.lock();
        let children = listed
            .into_iter()
            .map(|(child, file)| {
                let known = file.and_then(|file| {
                    let volume = state.volumes.get(&file.dev)?.volume.clone();
                    Some(FileMetadata {
                        kind: EntryKind::File,
                        volume: volume.clone(),
                        logical_size: Some(file.logical),
                        allocation_size: Some(file.allocation),
                        file_id: Some(FileIdentity {
                            volume,
                            id: format!("{}:{}", file.dev, file.ino),
                        }),
                        clone_id: None,
                        created_at: Some(file.created),
                        modified_at: Some(file.modified),
                        is_placeholder: false,
                        metadata_complete: true,
                    })
                });
                (child, known)
            })
            .collect();
        Ok((children, truncated))
    }

    fn volume_usage(&self, volume: &VolumeIdentity) -> Result<VolumeUsage, FsError> {
        platform::volume_usage(volume)
    }
}

/// Scan roots in lexical order. Roots are inspected through the provider,
/// including placeholder checks, then descendants are bounded by both depth
/// and entry count. Overlapping roots and hard links are deterministic.
/// This walk reads one directory at a time and needs no `Sync` provider.
pub fn scan_with_provider<P: FilesystemProvider>(
    provider: &P,
    paths: &[PathBuf],
    options: &ScanOptions,
) -> ScanReport {
    provider.begin_scan();
    run(provider, paths, options, None, false).0
}

/// The walk behind every scan. `pipeline`, when given, supplies directory
/// listings that worker threads read ahead; `collect_names` also builds the
/// `NameIndex`. Callers reset the provider first.
fn run<P: FilesystemProvider>(
    provider: &P,
    paths: &[PathBuf],
    options: &ScanOptions,
    pipeline: Option<Arc<Pipeline>>,
    collect_names: bool,
) -> (ScanReport, Option<NameIndex>) {
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
    let mut ctx = Ctx {
        seen_paths: (roots.len() > 1).then(HashSet::new),
        count: 0,
        seen_files: HashMap::new(),
        seen_dirs: HashSet::new(),
        volumes: BTreeSet::new(),
        attributed_by_volume: BTreeMap::new(),
        folders: Vec::new(),
        reclaim_reasons: HashSet::new(),
        keep: options.keep_files_per_folder,
        cancelled: false,
        pipeline,
        names: collect_names.then(NameIndex::default),
        current_parent: NO_PARENT,
    };
    let started = std::time::Instant::now();
    for root in roots {
        if ctx.count >= options.max_entries {
            report
                .incomplete_reasons
                .push("entry limit reached across roots".into());
            break;
        }
        if ctx.cancelled {
            break;
        }
        ctx.current_parent = NO_PARENT;
        let _ = walk(
            provider,
            root,
            0,
            options,
            None,
            None,
            &mut report,
            &mut ctx,
        );
    }
    if ctx.cancelled {
        report.incomplete_reasons.push("scan cancelled".into());
    }
    let Ctx {
        volumes,
        attributed_by_volume,
        folders,
        count: seen_count,
        names,
        ..
    } = ctx;
    let mut usage_by_volume = BTreeMap::new();
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
    // A skipped link is a deliberate non-follow, not missing data: its target
    // is accounted where it lives (or is out of scope). Links therefore do not
    // make the accounting incomplete. (A scan root that is itself, or sits
    // behind, a link records an explicit "root skipped" reason instead.) The
    // reclaim upper bound stays conservative: it is withheld for links too.
    report.accounting.incomplete =
        !report.incomplete_reasons.is_empty() || !report.inspection_errors.is_empty();
    if report.accounting.incomplete || !report.skipped_links.is_empty() {
        report.accounting.reclaim.upper_bytes = None;
        report
            .accounting
            .reclaim
            .reasons
            .push("inspection incomplete; full-selection upper bound unavailable".into());
    }
    // Folder totals were summed bottom-up during the walk (one add per file,
    // no per-ancestor path lookups). A folder is incomplete only when a
    // material gap (see `ScanGaps`) lies inside it; benign gaps elsewhere do
    // not taint it.
    let gaps = scan_gaps(&report);
    report.folders = folders;
    for folder in &mut report.folders {
        folder.incomplete = gaps.covers(&folder.path);
    }
    report.folders.sort_by(|a, b| {
        b.attributed_allocation_bytes
            .cmp(&a.attributed_allocation_bytes)
            .then_with(|| a.path.cmp(&b.path))
    });
    if std::env::var_os("PULSE_SCAN_LOG").is_some() {
        eprintln!(
            "scan: {} entries visited, {} kept, {} folders, {} ms{}",
            seen_count,
            report.entries.len(),
            report.folders.len(),
            started.elapsed().as_millis(),
            if report
                .incomplete_reasons
                .iter()
                .any(|r| r == "scan cancelled")
            {
                " (cancelled)"
            } else {
                ""
            }
        );
    }
    (report, names)
}

/// Row kinds in a `NameIndex` (the low bits of a row's flags), and the flag
/// that marks a row a refresh removed.
const NAME_FILE: u8 = 0;
const NAME_DIR: u8 = 1;
const NAME_OTHER: u8 = 2;
const NAME_KIND: u8 = 3;
const NAME_REMOVED: u8 = 4;
/// Parent of a scan's top row.
const NO_PARENT: u32 = u32::MAX;
/// Row of an entry that is not indexed: the index is full, or its folder is not.
const UNINDEXED: u32 = u32::MAX - 1;
const NAME_MAGIC: [u8; 4] = *b"PNIX";
const NAME_VERSION: u32 = 1;
/// Bytes per row in the encoding, not counting names: start, length, parent, size, kind.
const NAME_ROW_BYTES: usize = 4 + 2 + 4 + 8 + 1;

fn shift_bytes(value: u64, delta: i128) -> u64 {
    (i128::from(value) + delta).clamp(0, i128::from(u64::MAX)) as u64
}

/// Every name a scan visited, kept compactly: one row per entry, with the row
/// of its parent, its allocation size and its kind. Rows are appended in walk
/// order, so a parent always has a smaller row number than its children. A
/// refresh marks rows removed rather than moving the others; `rebuilt` drops
/// them. The top row (row 0) is the scanned folder, named by its full path.
#[derive(Clone, Debug, Default)]
pub struct NameIndex {
    arena: Vec<u8>,
    start: Vec<u32>,
    len: Vec<u16>,
    parent: Vec<u32>,
    size: Vec<u64>,
    /// Kind (`NAME_FILE`, `NAME_DIR` or `NAME_OTHER`), plus `NAME_REMOVED`.
    flags: Vec<u8>,
}

impl NameIndex {
    /// Rows, removed ones included.
    pub fn rows(&self) -> usize {
        self.parent.len()
    }

    /// Rows that are not removed.
    pub fn live_rows(&self) -> usize {
        self.flags
            .iter()
            .filter(|&&flag| flag & NAME_REMOVED == 0)
            .count()
    }

    pub fn is_live(&self, row: usize) -> bool {
        self.flags[row] & NAME_REMOVED == 0
    }

    pub fn name(&self, row: usize) -> &str {
        let start = self.start[row] as usize;
        let end = start + usize::from(self.len[row]);
        std::str::from_utf8(&self.arena[start..end]).unwrap_or("")
    }

    pub fn is_dir(&self, row: usize) -> bool {
        self.flags[row] & NAME_KIND == NAME_DIR
    }

    pub fn is_other(&self, row: usize) -> bool {
        self.flags[row] & NAME_KIND == NAME_OTHER
    }

    /// Allocated bytes of the entry (the folder's total, for a folder).
    pub fn size(&self, row: usize) -> u64 {
        self.size[row]
    }

    /// Full path of a row, built from its ancestors' names.
    pub fn path_of(&self, row: usize) -> Option<PathBuf> {
        if row >= self.rows() {
            return None;
        }
        let mut names: Vec<&str> = Vec::new();
        let mut at = row;
        while self.parent[at] != NO_PARENT {
            names.push(self.name(at));
            at = self.parent[at] as usize;
        }
        let mut path = PathBuf::from(self.name(at));
        for name in names.iter().rev() {
            path.push(*name);
        }
        Some(path)
    }

    /// The row of the folder at `path`, if the index holds it.
    pub fn find_dir(&self, path: &Path) -> Option<usize> {
        if self.parent.is_empty() {
            return None;
        }
        let relative = path.strip_prefix(self.name(0)).ok()?;
        let mut at: u32 = 0;
        for part in relative.components() {
            let wanted = part.as_os_str().to_string_lossy();
            let child = (0..self.parent.len()).find(|&row| {
                self.is_live(row) && self.parent[row] == at && self.name(row) == &*wanted
            })?;
            at = child as u32;
        }
        let row = at as usize;
        self.is_dir(row).then_some(row)
    }

    /// Append one row. `None` when the index is full or the parent is not a row.
    fn push(&mut self, parent: u32, name: &str, kind: u8, size: u64) -> Option<u32> {
        if parent == UNINDEXED || (parent != NO_PARENT && parent as usize >= self.parent.len()) {
            return None;
        }
        let id = u32::try_from(self.parent.len())
            .ok()
            .filter(|&id| id < UNINDEXED)?;
        let start = u32::try_from(self.arena.len()).ok()?;
        let len = u16::try_from(name.len()).ok()?;
        // The arena must stay addressable with u32 offsets.
        u32::try_from(self.arena.len().checked_add(name.len())?).ok()?;
        self.arena.extend_from_slice(name.as_bytes());
        self.start.push(start);
        self.len.push(len);
        self.parent.push(parent);
        self.size.push(size);
        self.flags.push(kind);
        Some(id)
    }

    fn set_size(&mut self, row: u32, bytes: u64) {
        if let Some(slot) = self.size.get_mut(row as usize) {
            *slot = bytes;
        }
    }

    /// Add `delta` to the size of every ancestor of `row`.
    fn add_to_ancestors(&mut self, row: usize, delta: i128) {
        let mut at = self.parent.get(row).copied().unwrap_or(NO_PARENT);
        while at != NO_PARENT {
            let up = at as usize;
            self.size[up] = shift_bytes(self.size[up], delta);
            at = self.parent[up];
        }
    }

    /// Mark the rows below `row` removed, and `row` too when `include_self`.
    fn mark_below(&mut self, row: usize, include_self: bool) {
        let mut below = vec![false; self.rows()];
        for (at, &parent) in self.parent.iter().enumerate().skip(row) {
            let hit = if at == row {
                include_self
            } else {
                parent != NO_PARENT && (parent as usize == row || below[parent as usize])
            };
            if hit {
                below[at] = true;
                self.flags[at] |= NAME_REMOVED;
            }
        }
    }

    /// Remove a folder (or file) and everything below it, taking its size out
    /// of its ancestors.
    pub fn remove_subtree(&mut self, row: usize) {
        if row >= self.rows() || !self.is_live(row) {
            return;
        }
        self.add_to_ancestors(row, -i128::from(self.size[row]));
        self.mark_below(row, true);
    }

    /// Replace the rows below folder `row` with those of `sub`, a scan of that
    /// folder (its row 0 is the folder itself). Returns false, leaving the index
    /// unusable, if the rows could not be appended.
    pub fn replace_subtree(&mut self, row: usize, sub: &NameIndex) -> bool {
        if row >= self.rows() || !self.is_live(row) || sub.rows() == 0 {
            return false;
        }
        self.mark_below(row, false);
        let (old, new) = (self.size[row], sub.size[0]);
        self.size[row] = new;
        self.add_to_ancestors(row, i128::from(new) - i128::from(old));
        let mut map: Vec<u32> = Vec::with_capacity(sub.rows());
        map.push(row as u32);
        for (at, &sub_parent) in sub.parent.iter().enumerate().skip(1) {
            let Some(&parent) = map.get(sub_parent as usize) else {
                return false;
            };
            let Some(id) = self.push(
                parent,
                sub.name(at),
                sub.flags[at] & NAME_KIND,
                sub.size[at],
            ) else {
                return false;
            };
            map.push(id);
        }
        true
    }

    /// Drop removed rows once they make up over half of the index.
    pub fn compact_if_sparse(&mut self) {
        if self.rows() > 2 * self.live_rows() + 1024 {
            *self = self.rebuilt();
        }
    }

    pub fn shrink_to_fit(&mut self) {
        self.arena.shrink_to_fit();
        self.start.shrink_to_fit();
        self.len.shrink_to_fit();
        self.parent.shrink_to_fit();
        self.size.shrink_to_fit();
        self.flags.shrink_to_fit();
    }

    /// For each row, its new number among the live rows (`NO_PARENT` if it is
    /// removed, or below a removed row).
    fn live_map(&self) -> Vec<u32> {
        let mut map: Vec<u32> = Vec::with_capacity(self.rows());
        let mut next: u32 = 0;
        for (row, &parent) in self.parent.iter().enumerate() {
            let parent_kept = parent == NO_PARENT || map[parent as usize] != NO_PARENT;
            if self.is_live(row) && parent_kept {
                map.push(next);
                next += 1;
            } else {
                map.push(NO_PARENT);
            }
        }
        map
    }

    /// The live rows only, renumbered.
    fn rebuilt(&self) -> NameIndex {
        let map = self.live_map();
        let mut out = NameIndex::default();
        for row in (0..self.rows()).filter(|&row| map[row] != NO_PARENT) {
            let parent = if self.parent[row] == NO_PARENT {
                NO_PARENT
            } else {
                map[self.parent[row] as usize]
            };
            out.push(
                parent,
                self.name(row),
                self.flags[row] & NAME_KIND,
                self.size[row],
            );
        }
        out
    }

    /// The live rows as bytes: magic, version, row count, arena length, then
    /// each column (start, length, parent, size, kind) and the names.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        // Writing to a Vec cannot fail.
        let _ = self.write_to(&mut out);
        out
    }

    /// Write the rows in the `to_bytes` format. Pass a buffered writer to stream
    /// them to a file without building the whole encoding in memory.
    pub fn write_to<W: std::io::Write>(&self, out: &mut W) -> std::io::Result<()> {
        let map = self.live_map();
        let live: Vec<usize> = (0..self.rows())
            .filter(|&row| map[row] != NO_PARENT)
            .collect();
        let arena_len: usize = live.iter().map(|&row| usize::from(self.len[row])).sum();
        out.write_all(&NAME_MAGIC)?;
        out.write_all(&NAME_VERSION.to_le_bytes())?;
        out.write_all(&(live.len() as u64).to_le_bytes())?;
        out.write_all(&(arena_len as u64).to_le_bytes())?;
        let mut offset: u32 = 0;
        for &row in &live {
            out.write_all(&offset.to_le_bytes())?;
            offset += u32::from(self.len[row]);
        }
        for &row in &live {
            out.write_all(&self.len[row].to_le_bytes())?;
        }
        for &row in &live {
            let parent = if self.parent[row] == NO_PARENT {
                NO_PARENT
            } else {
                map[self.parent[row] as usize]
            };
            out.write_all(&parent.to_le_bytes())?;
        }
        for &row in &live {
            out.write_all(&self.size[row].to_le_bytes())?;
        }
        for &row in &live {
            out.write_all(&[self.flags[row] & NAME_KIND])?;
        }
        for &row in &live {
            out.write_all(self.name(row).as_bytes())?;
        }
        Ok(())
    }

    /// Decode `to_bytes` output. `None` for another version or a damaged file.
    pub fn from_bytes(bytes: &[u8]) -> Option<NameIndex> {
        let header = bytes.get(..24)?;
        if header[..4] != NAME_MAGIC {
            return None;
        }
        let version = u32::from_le_bytes(header[4..8].try_into().ok()?);
        let rows = usize::try_from(u64::from_le_bytes(header[8..16].try_into().ok()?)).ok()?;
        let arena_len =
            usize::try_from(u64::from_le_bytes(header[16..24].try_into().ok()?)).ok()?;
        if version != NAME_VERSION || rows >= UNINDEXED as usize {
            return None;
        }
        let body = &bytes[24..];
        if body.len() != rows.checked_mul(NAME_ROW_BYTES)?.checked_add(arena_len)? {
            return None;
        }
        let (starts, rest) = body.split_at(rows * 4);
        let (lens, rest) = rest.split_at(rows * 2);
        let (parents, rest) = rest.split_at(rows * 4);
        let (sizes, rest) = rest.split_at(rows * 8);
        let (kinds, arena) = rest.split_at(rows);
        let index = NameIndex {
            arena: arena.to_vec(),
            start: starts
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| u32::from_le_bytes(*c))
                .collect(),
            len: lens
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes(*c))
                .collect(),
            parent: parents
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| u32::from_le_bytes(*c))
                .collect(),
            size: sizes
                .as_chunks::<8>()
                .0
                .iter()
                .map(|c| u64::from_le_bytes(*c))
                .collect(),
            flags: kinds.to_vec(),
        };
        let valid = (0..rows).all(|row| {
            let start = index.start[row] as usize;
            let end = start + usize::from(index.len[row]);
            end <= index.arena.len()
                && std::str::from_utf8(&index.arena[start..end]).is_ok()
                && (index.parent[row] == NO_PARENT || (index.parent[row] as usize) < row)
                && index.flags[row] <= NAME_OTHER
        });
        valid.then_some(index)
    }
}

/// Child entries the read-ahead may hold at once: listed or inspected, and not
/// yet used by the walk.
const PREFETCH_ENTRIES: usize = 300_000;
/// Directories waiting in the read-ahead queue at once.
const PREFETCH_DIRS: usize = 4_096;

type Children = Vec<(PathBuf, Option<FileMetadata>)>;
type Inspected = (FileMetadata, Vec<String>);

/// A directory listing read ahead, with the metadata it was checked against.
struct Listed {
    meta: FileMetadata,
    children: Children,
    truncated: bool,
}

enum Slot {
    /// In the queue, waiting for a worker.
    Queued,
    /// A worker is reading it.
    Running,
    Ready(Box<Listed>),
    /// Not read ahead: the walk reads it itself.
    Failed,
}

#[derive(Default)]
struct Pipe {
    queue: VecDeque<PathBuf>,
    slots: HashMap<PathBuf, Slot>,
    /// Children inspected ahead, for the walk to reuse instead of inspecting again.
    inspected: HashMap<PathBuf, Inspected>,
    /// Entries held ahead of the walk (listed children and inspected ones).
    pending: usize,
    finished: bool,
}

/// Directory listings read ahead of the walk by worker threads. The walk makes
/// every decision itself: a read-ahead listing is used only for the same
/// directory (same identity), with the same checks before and after it is used.
/// Anything the workers cannot read cleanly is left to the walk.
#[derive(Default)]
struct Pipeline {
    state: Mutex<Pipe>,
    /// Wakes idle workers.
    wake: Condvar,
    /// Wakes the walk when a listing it waits for is ready.
    ready: Condvar,
}

impl Pipeline {
    fn lock(&self) -> MutexGuard<'_, Pipe> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Entries the read-ahead may still take.
    fn room(&self) -> usize {
        PREFETCH_ENTRIES.saturating_sub(self.lock().pending)
    }

    /// The read-ahead listing of `path`, if there is one. A directory a worker is
    /// reading right now is waited for; a queued one is taken off the queue and
    /// read by the walk itself, which is then not behind the rest of the queue.
    /// `None` means the walk lists the directory itself.
    fn claim(&self, path: &Path) -> Option<Listed> {
        let mut state = self.lock();
        while matches!(state.slots.get(path), Some(Slot::Running)) {
            state = self.ready.wait(state).unwrap_or_else(|e| e.into_inner());
        }
        match state.slots.remove(path) {
            Some(Slot::Ready(listed)) => {
                state.pending = state.pending.saturating_sub(listed.children.len());
                Some(*listed)
            }
            _ => None,
        }
    }

    /// The child's metadata inspected ahead, if one was taken.
    fn take_inspected(&self, path: &Path) -> Option<Inspected> {
        let mut state = self.lock();
        let found = state.inspected.remove(path);
        if found.is_some() {
            state.pending = state.pending.saturating_sub(1);
        }
        found
    }

    /// Children the walk inspected while listing a directory itself.
    fn publish_children(&self, found: Vec<(PathBuf, Inspected)>) {
        let queued = add_children(&mut self.lock(), found);
        if queued {
            self.wake.notify_all();
        }
    }

    /// The outcome of a worker's read of `path`: its listing (or `None`, so the
    /// walk reads it) and the children it inspected.
    fn publish(&self, path: &Path, listed: Option<Listed>, found: Vec<(PathBuf, Inspected)>) {
        let queued = {
            let mut guard = self.lock();
            let state: &mut Pipe = &mut guard;
            let queued = add_children(state, found);
            if let Some(slot) = state.slots.get_mut(path) {
                *slot = match listed {
                    Some(listed) => {
                        state.pending += listed.children.len();
                        Slot::Ready(Box::new(listed))
                    }
                    None => Slot::Failed,
                };
            }
            queued
        };
        if queued {
            self.wake.notify_all();
        }
        self.ready.notify_all();
    }

    /// The next directory a worker should read, or `None` once the scan ended.
    fn next_job(&self) -> Option<PathBuf> {
        let mut state = self.lock();
        loop {
            if state.finished {
                return None;
            }
            while let Some(path) = state.queue.pop_front() {
                // A directory the walk already took off the queue has no slot left.
                if matches!(state.slots.get(&path), Some(Slot::Queued)) {
                    state.slots.insert(path.clone(), Slot::Running);
                    return Some(path);
                }
            }
            state = self.wake.wait(state).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Worker loop: read the queued directories until the scan ends.
    fn work<P: FilesystemProvider>(&self, provider: &P, options: &ScanOptions) {
        while let Some(path) = self.next_job() {
            let job = Claimed {
                pipe: self,
                path: Some(path),
            };
            let cancelled = options
                .cancel
                .as_ref()
                .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed));
            let read = if cancelled {
                None
            } else {
                list_for_read_ahead(provider, job.path())
            };
            match read {
                Some((meta, children, truncated)) if children.len() <= self.room() => {
                    let budget = self.room().saturating_sub(children.len());
                    let found = discover(provider, &children, budget);
                    job.finish(
                        Some(Listed {
                            meta,
                            children,
                            truncated,
                        }),
                        found,
                    );
                }
                _ => job.finish(None, Vec::new()),
            }
        }
    }

    fn finish(&self) {
        {
            let mut state = self.lock();
            state.finished = true;
        }
        self.wake.notify_all();
        self.ready.notify_all();
    }
}

/// A directory a worker has claimed. If the worker unwinds first, the walk is
/// told to read the directory itself, so it never waits for a result that
/// will not come.
struct Claimed<'a> {
    pipe: &'a Pipeline,
    path: Option<PathBuf>,
}

impl Claimed<'_> {
    fn path(&self) -> &Path {
        self.path.as_deref().unwrap_or(Path::new(""))
    }

    fn finish(mut self, listed: Option<Listed>, found: Vec<(PathBuf, Inspected)>) {
        if let Some(path) = self.path.take() {
            self.pipe.publish(&path, listed, found);
        }
    }
}

impl Drop for Claimed<'_> {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            self.pipe.publish(&path, None, Vec::new());
        }
    }
}

/// Ends the read-ahead when the walk returns or unwinds, so the workers exit.
struct Stop<'a>(&'a Pipeline);

impl Drop for Stop<'_> {
    fn drop(&mut self) {
        self.0.finish();
    }
}

/// Queue the directories among `found` and keep every inspected child for the
/// walk. Returns whether anything was queued.
fn add_children(state: &mut Pipe, found: Vec<(PathBuf, Inspected)>) -> bool {
    let mut queued = false;
    for (child, inspected) in found {
        state.pending += 1;
        let directory = inspected.0.kind == EntryKind::Directory && !inspected.0.is_placeholder;
        if directory && state.queue.len() < PREFETCH_DIRS {
            state.slots.insert(child.clone(), Slot::Queued);
            state.queue.push_back(child.clone());
            queued = true;
        }
        state.inspected.insert(child, inspected);
    }
    queued
}

/// Inspect the children whose facts the listing did not carry, up to `budget`,
/// so the walk does not have to. A child that cannot be inspected is left to the
/// walk, which reports the error itself.
fn discover<P: FilesystemProvider>(
    provider: &P,
    children: &[(PathBuf, Option<FileMetadata>)],
    budget: usize,
) -> Vec<(PathBuf, Inspected)> {
    let mut found = Vec::new();
    for (child, known) in children {
        if found.len() >= budget {
            break;
        }
        if known.is_some() {
            continue;
        }
        if let Ok(inspected) = provider.inspect_detailed(child) {
            found.push((child.clone(), inspected));
        }
    }
    found
}

/// A directory's listing read ahead, with the checks the walk makes around a
/// read: it must be a real directory, with the same identity, before and after.
/// `None` means the walk reads the directory itself.
fn list_for_read_ahead<P: FilesystemProvider>(
    provider: &P,
    path: &Path,
) -> Option<(FileMetadata, Children, bool)> {
    let before = provider.inspect(path).ok()?;
    if before.kind != EntryKind::Directory || before.is_placeholder {
        return None;
    }
    let (children, truncated) = provider
        .children_with_files(path, DIRECTORY_ENUMERATION_BUDGET)
        .ok()?;
    let after = provider.inspect(path).ok()?;
    let same = after.kind == EntryKind::Directory
        && !after.is_placeholder
        && after.file_id == before.file_id
        && after.volume == before.volume;
    same.then_some((before, children, truncated))
}

/// A read-ahead listing cut to the walk's own budget, as a direct read would be.
fn limit_listing(mut children: Children, truncated: bool, limit: usize) -> ChildrenWithFiles {
    if children.len() <= limit {
        return (children, truncated);
    }
    children.sort_by(|a, b| a.0.cmp(&b.0));
    children.truncate(limit);
    (children, true)
}

/// Worker threads for the read-ahead: `PULSE_SCAN_THREADS` when set (1 or less
/// walks without workers), else the CPU count between 2 and 6.
fn scan_threads() -> usize {
    if let Some(n) = std::env::var("PULSE_SCAN_THREADS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
    {
        return n;
    }
    std::thread::available_parallelism()
        .map_or(2, |n| n.get())
        .clamp(2, 6)
}

/// Mutable traversal state shared by one scan.
struct Ctx {
    /// Only tracked for several roots (overlapping roots would otherwise
    /// visit a subtree twice); a single root is a tree.
    seen_paths: Option<HashSet<PathBuf>>,
    count: usize,
    /// Hashed (volume, file id) -> first path (empty in lean mode).
    seen_files: HashMap<u128, PathBuf>,
    seen_dirs: HashSet<FileIdentity>,
    volumes: BTreeSet<VolumeIdentity>,
    attributed_by_volume: BTreeMap<VolumeIdentity, u64>,
    folders: Vec<FolderAccounting>,
    reclaim_reasons: HashSet<&'static str>,
    keep: Option<usize>,
    cancelled: bool,
    /// Read-ahead listings, when the scan runs with worker threads.
    pipeline: Option<Arc<Pipeline>>,
    /// Name rows, when the scan collects them.
    names: Option<NameIndex>,
    /// Row of the folder whose children the walk is about to visit.
    current_parent: u32,
}

impl Ctx {
    /// Add a name row for `path` under `parent`; `UNINDEXED` when not collected.
    fn name_row(&mut self, parent: u32, path: &Path, depth: usize, kind: u8) -> u32 {
        let Some(names) = self.names.as_mut() else {
            return UNINDEXED;
        };
        let name = if depth == 0 {
            path.to_string_lossy().into_owned()
        } else {
            path.file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
        };
        names.push(parent, &name, kind, 0).unwrap_or(UNINDEXED)
    }

    fn name_size(&mut self, row: u32, bytes: u64) {
        if let Some(names) = self.names.as_mut() {
            names.set_size(row, bytes);
        }
    }

    fn add_attributed(&mut self, volume: &VolumeIdentity, bytes: u64) {
        if let Some(slot) = self.attributed_by_volume.get_mut(volume) {
            *slot = slot.saturating_add(bytes);
        } else {
            self.attributed_by_volume.insert(volume.clone(), bytes);
        }
    }
}

/// 128-bit digest of a file identity, used instead of storing the identity.
fn identity_key(id: &FileIdentity) -> u128 {
    use std::hash::{Hash, Hasher};
    let mut a = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut a);
    let mut b = std::collections::hash_map::DefaultHasher::new();
    0x9e37_79b9_7f4a_7c15_u64.hash(&mut b);
    id.id.hash(&mut b);
    id.volume.hash(&mut b);
    (u128::from(a.finish()) << 64) | u128::from(b.finish())
}

/// Resolve the macOS top-level aliases `/var`, `/tmp` and `/etc` (symlinks to
/// `/private/...`) at the start of a path. Only that first component is
/// resolved, so any other symlink in the path is still refused by the scan.
pub(crate) fn canonical_root(path: &Path) -> PathBuf {
    let mut parts = path.components();
    let (Some(std::path::Component::RootDir), Some(std::path::Component::Normal(first))) =
        (parts.next(), parts.next())
    else {
        return path.to_path_buf();
    };
    if !matches!(first.to_str(), Some("var" | "tmp" | "etc")) {
        return path.to_path_buf();
    }
    let top = Path::new("/").join(first);
    if !fs::symlink_metadata(&top).is_ok_and(|m| m.file_type().is_symlink()) {
        return path.to_path_buf();
    }
    fs::canonicalize(&top).map_or_else(|_| path.to_path_buf(), |real| real.join(parts.as_path()))
}

pub fn scan(paths: &[PathBuf], options: &ScanOptions) -> ScanReport {
    scan_internal(paths, options, false).0
}

/// `scan`, also returning the name index of every entry the walk visited (the
/// roots are row 0, named by their full path).
pub fn scan_with_names(paths: &[PathBuf], options: &ScanOptions) -> (ScanReport, NameIndex) {
    let (report, names) = scan_internal(paths, options, true);
    (report, names.unwrap_or_default())
}

/// Fresh per-scan cache for every call. Directory listings are read ahead by
/// worker threads (see `Pipeline`) unless `PULSE_SCAN_THREADS` is 1 or less.
fn scan_internal(
    paths: &[PathBuf],
    options: &ScanOptions,
    collect_names: bool,
) -> (ScanReport, Option<NameIndex>) {
    let roots: Vec<PathBuf> = paths.iter().map(|p| canonical_root(p.as_path())).collect();
    let provider = CachingStdProvider::new();
    provider.begin_scan();
    let threads = scan_threads();
    if threads <= 1 {
        return run(&provider, &roots, options, None, collect_names);
    }
    let pipeline = Arc::new(Pipeline::default());
    let provider_ref = &provider;
    std::thread::scope(|scope| {
        // Declared first: if a worker cannot start, this still ends the read-ahead.
        let _stop = Stop(&pipeline);
        for _ in 0..threads {
            let worker = Arc::clone(&pipeline);
            let _handle = scope.spawn(move || worker.work(provider_ref, options));
        }
        run(
            provider_ref,
            &roots,
            options,
            Some(Arc::clone(&pipeline)),
            collect_names,
        )
    })
}

pub fn scan_paths(paths: &[PathBuf], options: &ScanOptions) -> ScanReport {
    scan(paths, options)
}

/// What one visited entry contributes to its parent folder.
#[derive(Default)]
struct Sub {
    logical: u64,
    attributed: u64,
    /// Lean mode: a file worth keeping, handed to the parent for ranking.
    candidate: Option<ScannedEntry>,
}

impl Sub {
    const fn empty() -> Self {
        Self {
            logical: 0,
            attributed: 0,
            candidate: None,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn walk<P: FilesystemProvider>(
    provider: &P,
    path: PathBuf,
    depth: usize,
    options: &ScanOptions,
    expected_volume: Option<&VolumeIdentity>,
    // Metadata already read with the parent's listing, and the reasons it
    // carries; `None` means inspect.
    prefetched: Option<Inspected>,
    report: &mut ScanReport,
    ctx: &mut Ctx,
) -> Sub {
    // Row of the folder this entry sits in (set by `walk_children`).
    let parent_id = ctx.current_parent;
    if options
        .cancel
        .as_ref()
        .is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
    {
        ctx.cancelled = true;
    }
    if ctx.cancelled {
        return Sub::empty();
    }
    if ctx.count >= options.max_entries {
        report
            .incomplete_reasons
            .push(format!("entry limit {} reached", options.max_entries));
        return Sub::empty();
    }
    if let Some(seen) = ctx.seen_paths.as_mut()
        && !seen.insert(path.clone())
    {
        return Sub::empty();
    }
    ctx.count += 1;
    // Ancestors are verified once per root. Below a root every directory is
    // rechecked immediately before and after it is listed (and the listing
    // itself refuses symlinked components), so re-inspecting every ancestor
    // of every entry only repeated that work.
    if depth == 0
        && let Some((ancestor, uninspectable)) = symlink_ancestor(provider, &path, report)
    {
        if uninspectable {
            report.incomplete_reasons.push(format!(
                "ancestor not inspectable, root skipped: {}",
                path.display()
            ));
        } else {
            // The requested root itself was not scanned: that is missing data,
            // unlike a link met inside the tree.
            report
                .incomplete_reasons
                .push(format!("ancestor link, root skipped: {}", path.display()));
            report.skipped_links.push(SkippedLink {
                path: ancestor,
                reason: "symlink or placeholder ancestor traversal disabled".into(),
            });
        }
        return Sub::empty();
    }
    let inspected = match prefetched {
        Some(value) => Ok(value),
        None => provider.inspect_detailed(&path),
    };
    let (mut metadata, provider_reasons) = match inspected {
        Ok(value) => value,
        Err(error) => {
            report.inspection_errors.push(InspectionError {
                path,
                operation: "inspect".into(),
                message: error.to_string(),
            });
            return Sub::empty();
        }
    };
    if metadata.kind == EntryKind::Symlink {
        if depth == 0 {
            report
                .incomplete_reasons
                .push(format!("link root, root skipped: {}", path.display()));
        }
        report.skipped_links.push(SkippedLink {
            path,
            reason: "symlink traversal disabled".into(),
        });
        return Sub::empty();
    }
    if metadata.is_placeholder && options.reject_placeholders {
        report
            .incomplete_reasons
            .push(format!("placeholder rejected: {}", path.display()));
        return Sub::empty();
    }
    if let Some(parent_volume) = expected_volume
        && !same_volume(&metadata.volume, parent_volume)
    {
        report.incomplete_reasons.push(format!(
            "cross-volume descendant rejected: {} ({} vs parent {})",
            path.display(),
            metadata.volume.id,
            parent_volume.id
        ));
        return Sub::empty();
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
            report.incomplete_reasons.push(format!(
                "metadata unavailable: {}: {reason}",
                path.display()
            ));
        }
    }
    if !ctx.volumes.contains(&metadata.volume) {
        ctx.volumes.insert(metadata.volume.clone());
    }
    if metadata.kind == EntryKind::File {
        let row = ctx.name_row(parent_id, &path, depth, NAME_FILE);
        let sub = visit_file(path, metadata, options, report, ctx);
        ctx.name_size(row, sub.attributed);
        return sub;
    }
    ctx.add_attributed(&metadata.volume, 0);
    report.entries.push(ScannedEntry {
        path: path.clone(),
        metadata: metadata.clone(),
        logical_bytes: 0,
        attributed_allocation_bytes: 0,
        accounting_owner: None,
        reclaim: None,
    });
    let kind = if metadata.kind == EntryKind::Directory {
        NAME_DIR
    } else {
        NAME_OTHER
    };
    let row = ctx.name_row(parent_id, &path, depth, kind);
    if metadata.kind != EntryKind::Directory {
        return Sub::empty();
    }
    let slot = ctx.folders.len();
    ctx.folders.push(FolderAccounting {
        path: path.clone(),
        volume: metadata.volume.clone(),
        logical_bytes: 0,
        attributed_allocation_bytes: 0,
        incomplete: false,
    });
    // The folder's children hang under its row.
    ctx.current_parent = row;
    let (logical, attributed) =
        walk_children(provider, path, depth, options, &metadata, report, ctx);
    ctx.name_size(row, attributed);
    let folder = &mut ctx.folders[slot];
    folder.logical_bytes = logical;
    folder.attributed_allocation_bytes = attributed;
    Sub {
        logical,
        attributed,
        candidate: None,
    }
}

/// Account one regular file. Returns its bytes and, in lean mode, the entry
/// to rank among its folder's files (instead of recording it directly).
fn visit_file(
    path: PathBuf,
    metadata: FileMetadata,
    options: &ScanOptions,
    report: &mut ScanReport,
    ctx: &mut Ctx,
) -> Sub {
    let lean = ctx.keep.is_some();
    let mut candidate_upper = metadata.allocation_size;
    let mut reasons: Vec<&'static str> = Vec::with_capacity(5);
    if metadata.allocation_size.is_none() {
        reasons.push("allocation metadata unavailable");
    }
    if !metadata.metadata_complete {
        reasons.push("metadata incomplete");
    }
    if metadata.is_placeholder {
        reasons.push("placeholder content is not local");
    }
    reasons.push(if metadata.clone_id.is_some() {
        "clone sharing is unknown"
    } else {
        "clone/snapshot sharing is not proven"
    });
    let key = metadata.file_id.as_ref().map(identity_key);
    let hardlink_owner = key.and_then(|k| ctx.seen_files.get(&k).cloned());
    let mut logical = 0;
    let mut attributed = 0;
    let mut owner = None;
    if let Some(existing) = hardlink_owner {
        owner = (!lean).then_some(existing);
        candidate_upper = Some(0);
        reasons.push("hard-link identity already attributed");
    } else {
        logical = metadata.logical_size.unwrap_or(0);
        if let Some(k) = key {
            attributed = metadata.allocation_size.unwrap_or(0);
            let first = if lean { PathBuf::new() } else { path.clone() };
            if !lean {
                owner = Some(path.clone());
            }
            ctx.seen_files.insert(k, first);
        } else {
            candidate_upper = None;
            reasons.push("file identity unavailable; unique allocation not attributed");
        }
    }
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
    for &reason in &reasons {
        if ctx.reclaim_reasons.insert(reason) {
            report.accounting.reclaim.reasons.push(reason.to_string());
        }
    }
    ctx.add_attributed(&metadata.volume, attributed);
    let sub = |candidate| Sub {
        logical,
        attributed,
        candidate,
    };
    if lean {
        if attributed == 0 || attributed < options.min_kept_file_bytes {
            return sub(None);
        }
        let mut metadata = metadata;
        metadata.file_id = None;
        metadata.clone_id = None;
        return sub(Some(ScannedEntry {
            path,
            metadata,
            logical_bytes: logical,
            attributed_allocation_bytes: attributed,
            accounting_owner: None,
            reclaim: None,
        }));
    }
    let mut estimate = ReclaimEstimate {
        lower_bytes: 0,
        upper_bytes: candidate_upper,
        state: ReclaimState::Unknown,
        reasons: reasons.iter().map(|r| (*r).to_string()).collect(),
    };
    if estimate.reasons.is_empty() {
        estimate
            .reasons
            .push("retention and sharing state are not proven".into());
    }
    report.entries.push(ScannedEntry {
        path,
        metadata,
        logical_bytes: logical,
        attributed_allocation_bytes: attributed,
        accounting_owner: owner,
        reclaim: Some(estimate),
    });
    sub(None)
}

/// Keep the `keep` largest candidates (ties by path, so the result is stable).
fn prune_candidates(candidates: &mut Vec<ScannedEntry>, keep: usize) {
    if keep == 0 {
        candidates.clear();
        return;
    }
    if candidates.len() <= keep {
        return;
    }
    let order = |a: &ScannedEntry, b: &ScannedEntry| {
        b.attributed_allocation_bytes
            .cmp(&a.attributed_allocation_bytes)
            .then_with(|| a.path.cmp(&b.path))
    };
    candidates.select_nth_unstable_by(keep - 1, order);
    candidates.truncate(keep);
}

/// List `path` and walk its children. Returns the summed (logical, attributed)
/// bytes of everything below it.
fn walk_children<P: FilesystemProvider>(
    provider: &P,
    path: PathBuf,
    depth: usize,
    options: &ScanOptions,
    metadata: &FileMetadata,
    report: &mut ScanReport,
    ctx: &mut Ctx,
) -> (u64, u64) {
    let none = (0, 0);
    // This folder's row: its children are added under it.
    let dir_id = ctx.current_parent;
    if depth >= options.max_depth {
        report.incomplete_reasons.push(format!(
            "depth limit {} reached at {}",
            options.max_depth,
            path.display()
        ));
        return none;
    }
    if metadata.is_placeholder {
        // Enumerating a placeholder directory can hydrate it.
        report.incomplete_reasons.push(format!(
            "placeholder directory not enumerated: {}",
            path.display()
        ));
        return none;
    }
    if let Some(id) = metadata.file_id.clone()
        && !ctx.seen_dirs.insert(id)
    {
        report.incomplete_reasons.push(format!(
            "directory identity already visited: {}",
            path.display()
        ));
        return none;
    }
    let remaining = options.max_entries.saturating_sub(ctx.count);
    let budget = remaining.min(DIRECTORY_ENUMERATION_BUDGET);
    // Recheck this directory immediately before & after listing. Changes discard
    // the listing; pathname checks are conservative observations, not a sandbox.
    if !directory_unchanged(provider, &path, metadata, report) {
        return none;
    }
    // A listing read ahead is used only for this same directory (same identity).
    let ahead = ctx
        .pipeline
        .as_ref()
        .and_then(|pipeline| pipeline.claim(&path))
        .filter(|listed| {
            listed.meta.file_id == metadata.file_id && listed.meta.volume == metadata.volume
        });
    let listing = match ahead {
        Some(listed) => Ok(limit_listing(listed.children, listed.truncated, budget)),
        None => provider.children_with_files(&path, budget),
    };
    let (listed, truncated) = match listing {
        Ok(listing) => listing,
        Err(error) => {
            report.inspection_errors.push(InspectionError {
                path,
                operation: "enumerate".into(),
                message: error.to_string(),
            });
            return none;
        }
    };
    if !directory_unchanged(provider, &path, metadata, report) {
        return none;
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
    let mut children = listed;
    sort_children(&path, &mut children);
    // Read this folder's subfolders ahead while the walk works through it.
    if let Some(pipeline) = ctx.pipeline.as_ref() {
        let found = discover(provider, &children, pipeline.room());
        pipeline.publish_children(found);
    }
    let mut logical = 0u64;
    let mut attributed = 0u64;
    let mut candidates: Vec<ScannedEntry> = Vec::new();
    for (child, known) in children {
        if ctx.count >= options.max_entries || ctx.cancelled {
            break;
        }
        let prefetched = match known {
            Some(meta) => Some((meta, Vec::new())),
            None => ctx
                .pipeline
                .as_ref()
                .and_then(|pipeline| pipeline.take_inspected(&child)),
        };
        ctx.current_parent = dir_id;
        let sub = walk(
            provider,
            child,
            depth + 1,
            options,
            Some(&metadata.volume),
            prefetched,
            report,
            ctx,
        );
        logical = logical.saturating_add(sub.logical);
        attributed = attributed.saturating_add(sub.attributed);
        if let (Some(entry), Some(keep)) = (sub.candidate, ctx.keep) {
            candidates.push(entry);
            if candidates.len() >= keep.saturating_mul(4).max(256) {
                prune_candidates(&mut candidates, keep);
            }
        }
    }
    if let Some(keep) = ctx.keep {
        prune_candidates(&mut candidates, keep);
        candidates.sort_by(|a, b| a.path.cmp(&b.path));
        report.entries.append(&mut candidates);
    }
    (logical, attributed)
}

/// Sort and de-duplicate one directory's listing. Siblings share a parent, so
/// on unix they are ordered by their final name bytes (the same order as a
/// component-wise path comparison) instead of re-comparing whole paths.
fn sort_children<T>(parent: &Path, children: &mut Vec<(PathBuf, T)>) {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let prefix = parent.as_os_str().as_bytes();
        let cut = if prefix == b"/" { 1 } else { prefix.len() + 1 };
        let siblings = children.iter().all(|(child, _)| {
            let bytes = child.as_os_str().as_bytes();
            bytes.len() > cut
                && bytes.starts_with(prefix)
                && (cut == 1 || bytes[cut - 1] == b'/')
                && !bytes[cut..].contains(&b'/')
        });
        if siblings {
            children.sort_unstable_by(|a, b| {
                a.0.as_os_str().as_bytes()[cut..].cmp(&b.0.as_os_str().as_bytes()[cut..])
            });
            children.dedup_by(|a, b| a.0.as_os_str() == b.0.as_os_str());
            return;
        }
    }
    let _ = parent;
    children.sort_by(|a, b| a.0.cmp(&b.0));
    children.dedup_by(|a, b| a.0 == b.0);
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

fn directory_unchanged<P: FilesystemProvider>(
    provider: &P,
    path: &Path,
    expected: &FileMetadata,
    report: &mut ScanReport,
) -> bool {
    match provider.inspect(path) {
        Ok(current)
            if current.kind == EntryKind::Directory
                && !current.is_placeholder
                && current.volume == expected.volume
                && current.file_id == expected.file_id =>
        {
            true
        }
        Ok(current) => {
            if current.kind == EntryKind::Symlink || current.is_placeholder {
                report.skipped_links.push(SkippedLink {
                    path: path.into(),
                    reason: "directory changed to link or placeholder during enumeration".into(),
                });
            }
            report.incomplete_reasons.push(format!(
                "directory changed during enumeration: {}",
                path.display()
            ));
            false
        }
        Err(error) => {
            report.inspection_errors.push(InspectionError {
                path: path.into(),
                operation: "recheck_directory".into(),
                message: error.to_string(),
            });
            false
        }
    }
}

/// Scan gaps sorted by how they affect a comparison of folder totals.
///
/// *Material* gaps leave totals unreliable: access denied, a depth or
/// directory budget hit, a root that was not scanned, a directory that changed
/// identity mid-walk, an entry limit hit, a cancelled scan, an unreadable
/// volume identity, or any cause this scanner does not recognise. A material
/// gap with a known path only affects the folders on the way down to it
/// (`covers`); one without taints the whole scan (`whole_scan`). *Benign* gaps
/// are stable or tiny: a link, placeholder or other volume that was not
/// followed on purpose, an entry that vanished while the walk ran, or a file
/// whose metadata could not be completed (its size is known from the listing,
/// or it is a constant across scans). Benign gaps are only counted.
///
/// This classifies for comparison and display only. Cleanup keeps relying on
/// `accounting.incomplete` and the reclaim bounds, which count every gap.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ScanGaps {
    /// Material gaps that taint every folder. An incomplete flag with no
    /// recorded cause counts as one.
    pub whole_scan: usize,
    /// The first of those, for a reason line.
    pub first_whole_scan: Option<String>,
    /// Material gaps confined to the folders on the way down to them.
    pub in_folders: usize,
    /// Benign gaps, which never block a comparison.
    pub benign: usize,
    /// The folders that hold a material gap, and every ancestor of one.
    folders: HashSet<PathBuf>,
}

impl ScanGaps {
    /// Whether `folder` holds a material gap or lies on the way down to one.
    pub fn covers(&self, folder: &Path) -> bool {
        self.whole_scan > 0 || self.folders.contains(folder)
    }

    fn whole(&mut self, describe: impl FnOnce() -> String) {
        self.whole_scan += 1;
        if self.first_whole_scan.is_none() {
            self.first_whole_scan = Some(describe());
        }
    }

    fn inside(&mut self, path: &Path) {
        self.in_folders += 1;
        for ancestor in path.ancestors() {
            self.folders.insert(ancestor.to_path_buf());
        }
    }
}

const VANISHED: &[&str] = &[
    "no such file",
    "cannot find the file",
    "cannot find the path",
    "path not found",
    "file not found",
    "(os error 2)",
    "(os error 3)",
    "(0x80070002)",
    "(0x80070003)",
];
const DENIED: &[&str] = &[
    "denied",
    "permission",
    "not permitted",
    "(os error 5)",
    "(0x80070005)",
];

fn mentions(lowercase: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| lowercase.contains(needle))
}

/// Whether one inspection error leaves folder totals unreliable. An entry or
/// directory that vanished after it was listed is benign, and so is the
/// failure to read the usage of a volume whose identity was only a path-prefix
/// fallback (the unreadable entries behind it are classified on their own). A
/// file that could not be inspected at all has no known size, so anything else
/// there, a lock included, stays material.
pub fn material_inspection_error(error: &InspectionError) -> bool {
    let message = error.message.to_lowercase();
    match error.operation.as_str() {
        "inspect" | "enumerate" | "recheck_directory" => {
            mentions(&message, DENIED) || !mentions(&message, VANISHED)
        }
        "volume_usage" => !message.contains("path-prefix-unstable"),
        _ => true,
    }
}

/// Whether one incomplete-scan reason leaves folder totals unreliable (see
/// [`ScanGaps`]). Unrecognised reasons are material.
pub fn material_incomplete_reason(reason: &str) -> bool {
    let reason = reason.to_lowercase();
    // Per-entry families first: their text carries an arbitrary path.
    if reason.starts_with("incomplete metadata:") || reason.starts_with("metadata unavailable:") {
        // The size comes from the listing, or the entry is a constant gap
        // (special file, no file id), and a file that vanished or is locked is
        // the same. Only a refused open could hide real bytes.
        return mentions(&reason, DENIED);
    }
    if reason.starts_with("placeholder rejected")
        || reason.starts_with("placeholder directory not enumerated")
        || reason.starts_with("directory identity already visited")
    {
        return false;
    }
    if reason.starts_with("cross-volume descendant rejected") {
        // A real other volume is a deliberate boundary; an identity that could
        // not be read is not.
        return reason.contains("path-prefix-unstable") || reason.contains("unknown");
    }
    true
}

/// The path a material reason names, when it names one. Reasons without a path
/// (entry limit, cancelled, unrecognised) taint the whole scan.
fn material_reason_path(reason: &str) -> Option<&str> {
    let lower = reason.to_lowercase();
    if lower.contains("entry limit") || lower.contains("cancel") {
        return None;
    }
    // Same wording as the producers in this file; a changed wording only
    // widens the gap to the whole scan.
    for marker in [
        "root skipped: ",
        " reached at ",
        " unread below ",
        "during enumeration: ",
    ] {
        if let Some(at) = reason.find(marker) {
            return Some(&reason[at + marker.len()..]);
        }
    }
    if let Some(rest) = reason.strip_prefix("cross-volume descendant rejected: ") {
        return rest.rfind(" (").map(|end| &rest[..end]);
    }
    let rest = reason.strip_prefix("metadata unavailable: ")?;
    let end = rest
        .find(": attribute-only open failed")
        .or_else(|| rest.find(": "))?;
    Some(&rest[..end])
}

/// Classify every recorded gap of a report. A report flagged incomplete with no
/// recorded cause counts as one whole-scan material gap.
pub fn scan_gaps(report: &ScanReport) -> ScanGaps {
    let mut gaps = ScanGaps::default();
    for reason in &report.incomplete_reasons {
        if !material_incomplete_reason(reason) {
            gaps.benign += 1;
        } else if let Some(path) = material_reason_path(reason) {
            gaps.inside(Path::new(path));
        } else {
            gaps.whole(|| reason.clone());
        }
    }
    for error in &report.inspection_errors {
        if !material_inspection_error(error) {
            gaps.benign += 1;
        } else if error.path.to_string_lossy().starts_with("<volume:") {
            // Volume-level errors carry a placeholder path.
            gaps.whole(|| format!("{}: {}", error.operation, error.message));
        } else {
            gaps.inside(&error.path);
        }
    }
    if report.accounting.incomplete
        && report.incomplete_reasons.is_empty()
        && report.inspection_errors.is_empty()
    {
        gaps.whole(|| "accounting is incomplete with no recorded cause".into());
    }
    gaps
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn swapped_directory_is_refused_before_listing() {
        let base = fs::canonicalize(std::env::temp_dir()).expect("canonical temp dir");
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let root = base.join(format!(
            "pulse-scan-identity-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        let target = root.join("target");
        fs::create_dir(&target).unwrap();
        let provider = CachingStdProvider::new();
        provider.begin_scan();
        let inspected = provider.inspect(&target).expect("inspect");
        assert_eq!(inspected.kind, EntryKind::Directory);
        fs::rename(&target, root.join("moved")).unwrap();
        fs::create_dir(&target).unwrap();
        fs::write(target.join("impostor.txt"), b"y").unwrap();
        let result = provider.children_bounded(&target, 10);
        let _ = fs::remove_dir_all(&root);
        let error = result.expect_err("swapped directory must not be listed");
        assert!(
            error.message.contains("identity changed"),
            "{}",
            error.message
        );
    }

    #[test]
    fn root_given_through_var_alias_is_scanned() {
        let var = Path::new("/var");
        if !fs::symlink_metadata(var).is_ok_and(|m| m.file_type().is_symlink()) {
            return; // platform has no /var symlink
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let canon_var = fs::canonicalize(var).unwrap();
        let real = canon_var.join(format!("pulse-alias-{}-{nanos}", std::process::id()));
        if fs::create_dir(&real).is_err() {
            return; // not writable here
        }
        fs::write(real.join("a.txt"), b"hello").unwrap();
        let alias = var.join(real.strip_prefix(&canon_var).unwrap());
        let report = scan(&[alias], &ScanOptions::default());
        let found = report.entries.iter().any(|e| e.path.ends_with("a.txt"));
        let _ = fs::remove_dir_all(&real);
        assert!(
            found,
            "entries missing: {:?} {:?}",
            report.skipped_links, report.inspection_errors
        );
    }
}
