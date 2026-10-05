//! Opt-in local metadata history. Target files are never changed by a scan.
use crate::{ScanReport, rules::Finding};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

/// Current (and only writable) snapshot schema version.
pub const SCHEMA_VERSION: u32 = 1;
/// Largest snapshot file that will be written or read.
pub const MAX_SNAPSHOT_BYTES: u64 = 64 * 1024 * 1024;
/// Largest number of snapshot candidates `history` will examine.
pub const MAX_SNAPSHOTS: usize = 1000;
/// Longest accepted snapshot ID.
pub const MAX_ID_LEN: usize = 64;
const ID_PREFIX: &str = "scan-";

/// A snapshot file that `history_report` declined to load, with the reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedSnapshot {
    pub file: String,
    pub reason: String,
}

/// Loaded snapshots (ordered by `created_at`, then `id`) plus explicit skips.
#[derive(Clone, Debug, Default)]
pub struct HistoryReport {
    pub snapshots: Vec<Snapshot>,
    pub skipped: Vec<SkippedSnapshot>,
}

/// An ID is never a path: `scan-` prefix, then ASCII alphanumerics, `-` or `_`,
/// at most `MAX_ID_LEN` bytes in total.
pub fn validate_id(id: &str) -> Result<(), String> {
    if id.len() > MAX_ID_LEN {
        return Err(format!("snapshot id longer than {MAX_ID_LEN} bytes"));
    }
    let rest = id
        .strip_prefix(ID_PREFIX)
        .ok_or_else(|| format!("snapshot id lacks `{ID_PREFIX}` prefix"))?;
    if rest.is_empty()
        || !rest
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("snapshot id has characters outside [A-Za-z0-9_-]".into());
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema_version: u32,
    pub id: String,
    pub created_at: u64,
    pub report: ScanReport,
    pub findings: Vec<Finding>,
}
impl Snapshot {
    pub fn new(report: ScanReport, findings: Vec<Finding>) -> Self {
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Self {
            schema_version: SCHEMA_VERSION,
            id: format!("scan-{}", time.as_nanos()),
            created_at: time.as_secs(),
            report,
            findings,
        }
    }
}

pub fn default_directory() -> io::Result<PathBuf> {
    #[cfg(target_os = "windows")]
    let root = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|p| p.join("Cockpit"));
    #[cfg(target_os = "macos")]
    let root = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|p| p.join("Library/Application Support/Cockpit"));
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let root = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .map(|p| p.join("cockpit"))
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|p| p.join(".local/state/cockpit"))
        });
    root.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "local metadata directory unavailable",
        )
    })
}
fn is_link(meta: &fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // FILE_ATTRIBUTE_REPARSE_POINT
        if meta.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    false
}

/// Refuse the state directory or any existing ancestor that is a symlink or
/// reparse point, and refuse a state path that exists but is not a directory.
fn reject_links(path: &Path) -> io::Result<()> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(meta) if is_link(&meta) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "metadata directory contains symlink or reparse point",
                ));
            }
            Ok(meta) => {
                if ancestor == path && !meta.is_dir() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "metadata path is not a directory",
                    ));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn create_directory(directory: &Path) -> io::Result<()> {
    // Existing directories are left untouched (including permissions); only
    // directories created here get owner-only mode on unix.
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(directory)
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write `snapshot` as `<id>.json`. Never overwrites an existing snapshot or
/// unrelated file (`AlreadyExists`). The write goes to an exclusively created
/// `.<id>.<unique>.tmp` file, is synced, then published atomically. Leftover
/// temp files from interrupted writes are ignored by `history`.
pub fn save(directory: &Path, snapshot: &Snapshot) -> io::Result<PathBuf> {
    validate_id(&snapshot.id).map_err(|m| io::Error::new(io::ErrorKind::InvalidInput, m))?;
    if snapshot.schema_version != SCHEMA_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to write unsupported snapshot schema version",
        ));
    }
    let bytes = serde_json::to_vec(snapshot).map_err(io::Error::other)?;
    if bytes.len() as u64 > MAX_SNAPSHOT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "snapshot exceeds size cap",
        ));
    }
    reject_links(directory)?;
    create_directory(directory)?;
    reject_links(directory)?;
    let destination = directory.join(format!("{}.json", snapshot.id));
    match fs::symlink_metadata(&destination) {
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "snapshot already exists",
            ));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => (),
        Err(e) => return Err(e),
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = directory.join(format!(
        ".{}.{}-{}-{}.tmp",
        snapshot.id,
        std::process::id(),
        nanos,
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    let result = (|| {
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        publish(&temporary, &destination)?;
        Ok(destination.clone())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn publish(temporary: &Path, destination: &Path) -> io::Result<()> {
    // Hard-link creation is atomic & refuses an existing destination on both
    // APFS & NTFS. Unsupported filesystems fail instead of risking replacement.
    fs::hard_link(temporary, destination)?;
    fs::remove_file(temporary)
}

#[derive(Deserialize)]
struct Header {
    schema_version: u64,
}

fn read_bounded(path: &Path) -> io::Result<Vec<u8>> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Inspect the opened reparse point itself; never follow a raced link.
        options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let file = options.open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || is_link(&meta) {
        return Err(invalid("not a regular file"));
    }
    if meta.len() > MAX_SNAPSHOT_BYTES {
        return Err(invalid("snapshot exceeds size cap"));
    }
    let mut bytes = Vec::new();
    // Read one extra byte so a file that grew after the check is still caught.
    file.take(MAX_SNAPSHOT_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SNAPSHOT_BYTES {
        return Err(invalid("snapshot exceeds size cap"));
    }
    Ok(bytes)
}

fn load(path: &Path, stem: &str) -> Result<Snapshot, String> {
    let bytes = read_bounded(path).map_err(|e| e.to_string())?;
    let header: Header =
        serde_json::from_slice(&bytes).map_err(|e| format!("malformed snapshot: {e}"))?;
    if header.schema_version != u64::from(SCHEMA_VERSION) {
        return Err(format!(
            "unsupported schema version {} (supported: {SCHEMA_VERSION})",
            header.schema_version
        ));
    }
    let snapshot: Snapshot =
        serde_json::from_slice(&bytes).map_err(|e| format!("malformed snapshot: {e}"))?;
    validate_id(&snapshot.id)?;
    if snapshot.id != stem {
        return Err("snapshot id does not match file name".into());
    }
    Ok(snapshot)
}

/// Load all snapshots, skipping (with a reason) any individual bad file:
/// malformed, oversized, unknown schema version, bad or mismatched id,
/// symlink or non-regular entries named like snapshots. Non-snapshot names,
/// including dot-prefixed temp files from interrupted writes, are ignored
/// without being opened. Errors are reserved for the directory itself
/// (link/reparse point, unreadable) or more than `MAX_SNAPSHOTS` candidates.
pub fn history_report(directory: &Path) -> io::Result<HistoryReport> {
    reject_links(directory)?;
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(HistoryReport::default()),
        Err(e) => return Err(e),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(ID_PREFIX) && name.ends_with(".json") {
            if names.len() == MAX_SNAPSHOTS {
                return Err(invalid("history exceeds snapshot count cap"));
            }
            names.push((name, entry));
        }
    }
    if names.len() > MAX_SNAPSHOTS {
        return Err(invalid("history exceeds snapshot count cap"));
    }
    names.sort_by(|a, b| a.0.cmp(&b.0));
    let mut report = HistoryReport::default();
    for (name, entry) in names {
        let skip = |reason: String| SkippedSnapshot {
            file: name.clone(),
            reason,
        };
        match entry.file_type() {
            Ok(t) if t.is_file() => (),
            Ok(_) => {
                report.skipped.push(skip("not a regular file".into()));
                continue;
            }
            Err(e) => {
                report.skipped.push(skip(e.to_string()));
                continue;
            }
        }
        let stem = &name[..name.len() - ".json".len()];
        match validate_id(stem).and_then(|()| load(&entry.path(), stem)) {
            Ok(snapshot) => report.snapshots.push(snapshot),
            Err(reason) => report.skipped.push(skip(reason)),
        }
    }
    report
        .snapshots
        .sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
    Ok(report)
}

/// Valid snapshots only, deterministically ordered. Bad files are skipped; use
/// `history_report` to see why.
pub fn history(directory: &Path) -> io::Result<Vec<Snapshot>> {
    history_report(directory).map(|r| r.snapshots)
}
