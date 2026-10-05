//! Real-filesystem runtime tests for the std provider. Every test works in a
//! unique directory under the canonical temp dir and removes only that dir.

use cockpit_core::{
    EntryKind, FileMetadata, FilesystemProvider, FsError, ScanOptions, StdFilesystemProvider,
    VolumeIdentity, VolumeUsage, scan_paths, scan_with_provider,
};
use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let base = fs::canonicalize(std::env::temp_dir()).expect("canonical temp dir");
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = base.join(format!(
            "cockpit-scan-runtime-{label}-{}-{nanos}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create unique temp dir");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Delegates to the real provider and counts scan starts.
struct Counting {
    inner: StdFilesystemProvider,
    begins: Cell<usize>,
}

impl FilesystemProvider for Counting {
    fn begin_scan(&self) {
        self.begins.set(self.begins.get() + 1);
        self.inner.begin_scan();
    }
    fn inspect(&self, path: &Path) -> Result<FileMetadata, FsError> {
        self.inner.inspect(path)
    }
    fn children(&self, path: &Path) -> Result<Vec<PathBuf>, FsError> {
        self.inner.children(path)
    }
    fn children_bounded(&self, path: &Path, limit: usize) -> Result<(Vec<PathBuf>, bool), FsError> {
        self.inner.children_bounded(path, limit)
    }
    fn volume_usage(&self, volume: &VolumeIdentity) -> Result<VolumeUsage, FsError> {
        self.inner.volume_usage(volume)
    }
}

#[test]
fn per_scan_cache_is_reset_and_results_are_consistent() {
    let temp = TempDir::new("cache");
    fs::create_dir(temp.0.join("sub")).unwrap();
    fs::write(temp.0.join("a.bin"), b"abc").unwrap();
    fs::write(temp.0.join("sub").join("b.bin"), b"defg").unwrap();
    let provider = Counting {
        inner: StdFilesystemProvider,
        begins: Cell::new(0),
    };
    let options = ScanOptions::default();
    let first = scan_with_provider(&provider, std::slice::from_ref(&temp.0), &options);
    let second = scan_with_provider(&provider, std::slice::from_ref(&temp.0), &options);
    assert_eq!(
        provider.begins.get(),
        2,
        "state must be reset once per scan"
    );
    assert_eq!(first.entries.len(), 4);
    assert_eq!(first.entries.len(), second.entries.len());
    let volumes = |report: &cockpit_core::ScanReport| {
        report
            .entries
            .iter()
            .map(|e| {
                (
                    e.path.clone(),
                    e.metadata.volume.clone(),
                    e.metadata.file_id.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(volumes(&first), volumes(&second));
    // One volume for the whole tree, and every entry agrees on it.
    let root_volume = &first.entries[0].metadata.volume;
    assert!(
        first
            .entries
            .iter()
            .all(|e| &e.metadata.volume == root_volume)
    );
    // The public entry point builds a fresh caching provider per call.
    let a = scan_paths(std::slice::from_ref(&temp.0), &options);
    let b = scan_paths(std::slice::from_ref(&temp.0), &options);
    assert_eq!(volumes(&a), volumes(&first));
    assert_eq!(volumes(&a), volumes(&b));
}

#[cfg(unix)]
#[test]
fn bounded_listing_truncates_after_limit() {
    let temp = TempDir::new("bounded");
    for i in 0..5 {
        fs::write(temp.0.join(format!("f{i}")), b"x").unwrap();
    }
    let provider = StdFilesystemProvider;
    let (three, truncated) = provider.children_bounded(&temp.0, 3).unwrap();
    assert_eq!(three.len(), 3);
    assert!(truncated);
    assert!(three.iter().all(|p| p.starts_with(&temp.0)));
    let (all, truncated) = provider.children_bounded(&temp.0, 5).unwrap();
    assert_eq!(all.len(), 5);
    assert!(!truncated, "exactly `limit` entries is not truncation");
    let (none, truncated) = provider.children_bounded(&temp.0, 0).unwrap();
    assert!(none.is_empty());
    assert!(truncated);
    let (many, truncated) = provider.children_bounded(&temp.0, 100).unwrap();
    assert_eq!(many.len(), 5);
    assert!(!truncated);
}

#[cfg(unix)]
#[test]
fn listing_does_not_follow_symlinked_directory() {
    let temp = TempDir::new("nofollow");
    let real = temp.0.join("real");
    fs::create_dir(&real).unwrap();
    fs::write(real.join("inside.txt"), b"x").unwrap();
    let link = temp.0.join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let provider = StdFilesystemProvider;
    assert!(provider.children_bounded(&link, 10).is_err());
    assert!(provider.children_bounded(&real, 10).is_ok());

    let report = scan_with_provider(
        &provider,
        std::slice::from_ref(&link),
        &ScanOptions::default(),
    );
    assert!(report.entries.is_empty());
    assert_eq!(report.skipped_links.len(), 1);
    assert_eq!(report.skipped_links[0].path, link);
}

#[cfg(unix)]
#[test]
fn special_file_is_unknown_not_zero_allocated() {
    use std::os::unix::ffi::OsStrExt;
    let temp = TempDir::new("fifo");
    let fifo = temp.0.join("pipe");
    let c_path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // Guard: if mkfifo is unavailable in this environment, skip.
    if unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) } != 0 {
        return;
    }
    fs::write(temp.0.join("a.bin"), b"abc").unwrap();
    let report = scan_paths(std::slice::from_ref(&temp.0), &ScanOptions::default());
    let entry = report
        .entries
        .iter()
        .find(|e| e.path == fifo)
        .expect("fifo must appear as an entry");
    assert_eq!(entry.metadata.kind, EntryKind::Other);
    assert_eq!(
        entry.metadata.allocation_size, None,
        "st_blocks is meaningless for a FIFO; allocation must be unknown"
    );
    assert!(!entry.metadata.metadata_complete);
    assert_eq!(entry.attributed_allocation_bytes, 0);
    assert!(report.accounting.incomplete);
    assert!(
        report
            .incomplete_reasons
            .iter()
            .any(|r| r.contains("non-regular") && r.contains("pipe")),
        "reasons must name the special file: {:?}",
        report.incomplete_reasons
    );
    // Totals reflect only the regular file; the FIFO adds nothing.
    let attributed: u64 = report
        .entries
        .iter()
        .map(|e| e.attributed_allocation_bytes)
        .sum();
    assert_eq!(attributed, report.accounting.attributed_allocation_bytes);
}

/// Provider that deletes a listed child before the scanner inspects it,
/// exercising the enumeration-to-inspection race conservatively.
struct RemoveOnList {
    inner: StdFilesystemProvider,
    victim: PathBuf,
    removed: Cell<bool>,
}

impl FilesystemProvider for RemoveOnList {
    fn inspect(&self, path: &Path) -> Result<FileMetadata, FsError> {
        self.inner.inspect(path)
    }
    fn inspect_detailed(&self, path: &Path) -> Result<(FileMetadata, Vec<String>), FsError> {
        self.inner.inspect_detailed(path)
    }
    fn children(&self, path: &Path) -> Result<Vec<PathBuf>, FsError> {
        self.inner.children(path)
    }
    fn children_bounded(&self, path: &Path, limit: usize) -> Result<(Vec<PathBuf>, bool), FsError> {
        let (children, truncated) = self.inner.children_bounded(path, limit)?;
        if !self.removed.get() && children.iter().any(|c| c == &self.victim) {
            fs::remove_file(&self.victim).expect("remove victim");
            self.removed.set(true);
        }
        Ok((children, truncated))
    }
    fn volume_usage(&self, volume: &VolumeIdentity) -> Result<VolumeUsage, FsError> {
        self.inner.volume_usage(volume)
    }
    fn begin_scan(&self) {
        self.inner.begin_scan();
    }
}

#[cfg(unix)]
#[test]
fn removed_between_listing_and_inspection_is_rejected() {
    let temp = TempDir::new("race");
    let victim = temp.0.join("victim.bin");
    fs::write(&victim, b"gone").unwrap();
    fs::write(temp.0.join("kept.bin"), b"stay").unwrap();
    let provider = RemoveOnList {
        inner: StdFilesystemProvider,
        victim: victim.clone(),
        removed: Cell::new(false),
    };
    let report = scan_with_provider(
        &provider,
        std::slice::from_ref(&temp.0),
        &ScanOptions::default(),
    );
    assert!(
        provider.removed.get(),
        "fixture must have removed the child"
    );
    assert!(
        report.entries.iter().all(|e| e.path != victim),
        "a removed entry must never contribute to totals"
    );
    assert!(
        report
            .inspection_errors
            .iter()
            .any(|e| e.path == victim && e.operation == "inspect"),
        "removal must surface as an inspection error: {:?}",
        report.inspection_errors
    );
    assert!(report.accounting.incomplete);
    assert!(
        report.entries.iter().any(|e| e.path.ends_with("kept.bin")),
        "unrelated entries still scan"
    );
}

#[cfg(unix)]
#[test]
fn one_scan_reports_consistent_identities_on_one_volume() {
    let temp = TempDir::new("onevolume");
    fs::create_dir(temp.0.join("sub")).unwrap();
    for name in ["a", "b", "sub/c"] {
        fs::write(temp.0.join(name), b"x").unwrap();
    }
    let report = scan_paths(std::slice::from_ref(&temp.0), &ScanOptions::default());
    assert_eq!(report.entries.len(), 5);
    let volume = &report.entries[0].metadata.volume;
    assert!(
        report.entries.iter().all(|e| &e.metadata.volume == volume),
        "every entry in one mount must share one cached volume identity"
    );
    let mut ids: Vec<_> = report
        .entries
        .iter()
        .filter_map(|e| e.metadata.file_id.clone())
        .collect();
    ids.sort_by(|a, b| a.id.cmp(&b.id));
    let before = ids.len();
    ids.dedup_by(|a, b| a.id == b.id);
    assert_eq!(ids.len(), before, "distinct files must keep distinct ids");
}
