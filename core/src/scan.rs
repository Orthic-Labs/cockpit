//! Bounded, metadata-only filesystem scanning.

use crate::model::*;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
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
    fn children_with_files(
        &self,
        path: &Path,
        limit: usize,
    ) -> Result<(Vec<(PathBuf, Option<FileMetadata>)>, bool), FsError> {
        self.children_bounded(path, limit).map(|(children, truncated)| {
            (children.into_iter().map(|child| (child, None)).collect(), truncated)
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
    fn children_with_files(
        &self,
        path: &Path,
        limit: usize,
    ) -> Result<(Vec<(PathBuf, Option<FileMetadata>)>, bool), FsError> {
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
pub fn scan_with_provider<P: FilesystemProvider>(
    provider: &P,
    paths: &[PathBuf],
    options: &ScanOptions,
) -> ScanReport {
    provider.begin_scan();
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
        let _ = walk(provider, root, 0, options, None, None, &mut report, &mut ctx);
    }
    if ctx.cancelled {
        report.incomplete_reasons.push("scan cancelled".into());
    }
    let Ctx {
        volumes,
        attributed_by_volume,
        folders,
        count: seen_count,
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
    // Folder totals were summed bottom-up during the walk (one add per file,
    // no per-ancestor path lookups).
    let incomplete = report.accounting.incomplete;
    report.folders = folders;
    for folder in &mut report.folders {
        folder.incomplete = incomplete;
    }
    report.folders.sort_by(|a, b| {
        b.attributed_allocation_bytes
            .cmp(&a.attributed_allocation_bytes)
            .then_with(|| a.path.cmp(&b.path))
    });
    if std::env::var_os("PULSE_SCAN_LOG")
        .or_else(|| std::env::var_os("COCKPIT_SCAN_LOG"))
        .is_some()
    {
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
    report
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
}

impl Ctx {
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
    // Fresh per-scan cache for every call.
    let roots: Vec<PathBuf> = paths.iter().map(|p| canonical_root(p.as_path())).collect();
    scan_with_provider(&CachingStdProvider::new(), &roots, options)
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

fn walk<P: FilesystemProvider>(
    provider: &P,
    path: PathBuf,
    depth: usize,
    options: &ScanOptions,
    expected_volume: Option<&VolumeIdentity>,
    // Metadata already read with the parent's listing; `None` means inspect.
    prefetched: Option<FileMetadata>,
    report: &mut ScanReport,
    ctx: &mut Ctx,
) -> Sub {
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
            report.skipped_links.push(SkippedLink {
                path: ancestor,
                reason: "symlink or placeholder ancestor traversal disabled".into(),
            });
        }
        return Sub::empty();
    }
    let inspected = match prefetched {
        Some(metadata) => Ok((metadata, Vec::new())),
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
        && &metadata.volume != parent_volume
    {
        report.incomplete_reasons.push(format!(
            "cross-volume descendant rejected: {}",
            path.display()
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
        return visit_file(path, metadata, options, report, ctx);
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
    let (logical, attributed) =
        walk_children(provider, path, depth, options, &metadata, report, ctx);
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
    let (listed, truncated) = match provider.children_with_files(&path, budget) {
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
    let mut logical = 0u64;
    let mut attributed = 0u64;
    let mut candidates: Vec<ScannedEntry> = Vec::new();
    for (child, prefetched) in children {
        if ctx.count >= options.max_entries || ctx.cancelled {
            break;
        }
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
