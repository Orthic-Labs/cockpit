//! Storage scans for the hub. At most one scan runs at a time; starting
//! another cancels the running one. The scan runs on its own thread at
//! utility QoS so it stays out of the way of the UI and other apps.
//!
//! The finished scan is not kept as a `ScanReport` (millions of entries). It
//! is folded into an `Index`: per folder, its size plus only its largest
//! children (`TOP` of them), which is all the browser ever shows. Files below
//! `MIN_KEPT_FILE_BYTES`, and any beyond `TOP` per folder, are summed into one
//! "Smaller files" row so a folder's rows still add up to its size.
//!
//! Alongside it the index keeps a `NameIndex`: every name the scan visited,
//! so search finds files of any size. While the hub is open, `watch` reports
//! the folders that changed; `apply_changes` reads just those again and
//! updates both parts in place.
//!
//! Memory: the root view is read from a small saved file at open. The folder
//! index loads on the first drill-down or search, and the name rows on the
//! first search. Both are dropped after `IDLE_UNLOAD` without Storage use. While
//! the index is not in memory, live changes are only collected as a set of
//! folders (`WAITING`), and applied when the index loads again.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Once};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use pulse_core::scan::NameIndex;
use pulse_core::{EntryKind, ScanOptions, ScanReport};
use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::{cache, growth, home, watch};

/// Format of the JSON view and the older JSON index (`storage-index-v1.json`).
const FORMAT: u32 = 1;
/// The folder index as the hub saved it before the binary format. Still read
/// when no binary index exists; removed once a binary index is written.
const INDEX_FILE: &str = "storage-index-v1.json";
/// The folder index, binary (see `write_index`).
const INDEX_FILE_V2: &str = "storage-index-v2.bin";
const VIEW_FILE: &str = "storage-view-v1.json";
/// The name rows (`NameIndex::to_bytes`), saved beside the folder index.
const NAMES_FILE: &str = "storage-names-v1.bin";
/// Folders whose name rows changed while the rows were not in memory. The saved
/// name rows are brought up to date with these when they are next loaded.
const STALE_FILE: &str = "storage-names-stale-v1.txt";

/// Magic and version of the binary folder index.
const INDEX_MAGIC: [u8; 4] = *b"PIX2";
const INDEX_VERSION: u32 = 3;
/// The earlier layout, without the unread count, is still read.
const INDEX_VERSION_V2: u32 = 2;
/// Longest path or name a saved index may hold; a longer length means damage.
const MAX_SAVED_TEXT: usize = 1 << 20;

/// Newest scan time saved so far; an older scan finishing later never overwrites it.
static SAVED_AT: AtomicU64 = AtomicU64::new(0);
/// Serialises loading the saved index and name rows, so concurrent requests read them once.
static LOADING: Mutex<()> = Mutex::new(());
/// Only one save writes the saved files at a time (they share temporary names).
static SAVE_LOCK: Mutex<()> = Mutex::new(());

/// Largest children kept per folder.
const TOP: usize = 200;
/// Files smaller than this are only counted in their folder's total.
const MIN_KEPT_FILE_BYTES: u64 = 256 * 1024;
const MAX_ENTRIES: usize = 2_000_000;
/// A live refresh reads at most this many entries of a changed folder's subtree;
/// a larger one is reported as stale instead of read.
const LIVE_MAX_ENTRIES: usize = 200_000;
/// Label of the row that holds files not itemised, in a live index.
const SMALLER_FILES: &str = "Smaller files";
/// While the watch runs, the index is written at most this often.
const SAVE_INTERVAL: Duration = Duration::from_secs(20);
/// Storage is unused for this long: the folder index and name rows leave memory.
const IDLE_UNLOAD: Duration = Duration::from_secs(120);
/// How often the idle check runs.
const IDLE_CHECK: Duration = Duration::from_secs(15);
/// A folder that changed is read again at most this often.
const RETRY: Duration = Duration::from_secs(10);
/// A folder found too large to read again is left alone for this long.
const OVERSIZED_PAUSE: Duration = Duration::from_secs(600);
/// Changes waiting for the index: more than this and they are reported as stale.
const WAITING_MAX: usize = 100_000;
/// Folders that may be waiting for their name rows. More than this and the
/// saved name rows are dropped; a new scan rebuilds them.
const STALE_MAX: usize = 5_000;

/// Event the page listens to: folders read again after a change (see `Updated`).
pub(crate) const UPDATED_EVENT: &str = "storage-updated";

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
}

/// QOS_CLASS_UTILITY for the calling thread.
pub(crate) fn low_priority() {
    #[cfg(target_os = "macos")]
    unsafe {
        pthread_set_qos_class_self_np(0x11, 0);
    }
}

/// Held while any heavy scan (storage or cleanup findings) runs.
static RUN: Mutex<()> = Mutex::new(());
static NEXT_JOB: AtomicU64 = AtomicU64::new(1);
static JOB: Mutex<Option<Job>> = Mutex::new(None);
/// The folder index while it is in memory; `None` after an idle unload.
static INDEX: Mutex<Option<Arc<Index>>> = Mutex::new(None);
/// The name rows while they are in memory; `None` until the first search.
static NAMES: Mutex<Option<Arc<NameIndex>>> = Mutex::new(None);
/// What the Storage page needs when the index is not in memory.
static SUMMARY: Mutex<Option<Summary>> = Mutex::new(None);
/// Bumped each time a scan replaces the index. A live refresh applies only to
/// the index it started from.
static INDEX_EPOCH: AtomicU64 = AtomicU64::new(0);
/// The index has changes that are not saved yet.
static DIRTY: AtomicBool = AtomicBool::new(false);
/// Saves in flight. A refresh waits (its folder stays waiting) while any runs,
/// so the saved data is never copied to make room for a change.
static SAVING: AtomicUsize = AtomicUsize::new(0);
/// A scan has finished in this session (so loaded data is not a saved snapshot).
static SCANNED: AtomicBool = AtomicBool::new(false);
/// The saved name rows can no longer be trusted: they are removed at the next save.
static NAMES_BROKEN: AtomicBool = AtomicBool::new(false);
/// Folders whose name rows changed while those rows were not in memory.
static STALE: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());
/// Changes not applied yet: raw paths from FSEvents, or the indexed folders that hold them.
static WAITING: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());
/// When each folder was last read again (only those within `RETRY` are kept).
static ATTEMPTED: Mutex<BTreeMap<PathBuf, Instant>> = Mutex::new(BTreeMap::new());
/// Folders whose subtree was too large to read again, and when. Changes that
/// land directly in one (a dotfile in home, a hive file) are not read again for
/// `OVERSIZED_PAUSE`; each try would read `LIVE_MAX_ENTRIES` entries for nothing.
static OVERSIZED: Mutex<BTreeMap<PathBuf, Instant>> = Mutex::new(BTreeMap::new());
static LAST_SAVE: Mutex<Option<Instant>> = Mutex::new(None);
/// When the Storage page last used the index.
static LAST_USE: Mutex<Option<Instant>> = Mutex::new(None);
static IDLE_TIMER: Once = Once::new();

struct Job {
    id: u64,
    root: PathBuf,
    cancel: Arc<AtomicBool>,
}

/// Serializes heavy scans: the cleanup scan takes this so it never overlaps a
/// storage scan.
pub fn exclusive() -> MutexGuard<'static, ()> {
    RUN.lock().unwrap_or_else(|e| e.into_inner())
}

fn lock<T>(m: &'static Mutex<T>) -> MutexGuard<'static, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Row {
    path: PathBuf,
    name: String,
    is_dir: bool,
    bytes: u64,
    /// A synthetic "smaller files" row: not a real path.
    summary: bool,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Folder {
    path: PathBuf,
    root: PathBuf,
    rows: Vec<Row>,
    total_children: usize,
    incomplete: bool,
    /// Some folders could not be read (permission): Full Disk Access helps.
    needs_access: bool,
    /// Items the scan could not read (long paths, access denied, ...).
    #[serde(default)]
    unread: u64,
    /// A size limit was reached, so totals may be a little low.
    limited: bool,
    /// Label for the scan root in the breadcrumbs.
    root_label: String,
    /// Unix seconds when this data was scanned.
    scanned_at: u64,
    /// Shown from the last saved scan, not a scan made in this session.
    from_snapshot: bool,
}

#[derive(Clone)]
struct Item {
    name: Box<str>,
    is_dir: bool,
    summary: bool,
    bytes: u64,
}

#[derive(Default, Clone)]
struct Node {
    total: u64,
    dirs: u64,
    kept: u64,
    children: usize,
    items: Vec<Item>,
}

#[derive(Clone)]
struct Index {
    root: PathBuf,
    root_label: String,
    scanned_at: u64,
    from_snapshot: bool,
    incomplete: bool,
    needs_access: bool,
    unread: u64,
    limited: bool,
    nodes: HashMap<PathBuf, Node>,
}

/// What the Storage page shows of the index, kept while the index is unloaded.
#[derive(Clone)]
struct Summary {
    root: PathBuf,
    scanned_at: u64,
    from_snapshot: bool,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn denied(text: &str) -> bool {
    let t = text.to_lowercase();
    t.contains("denied") || t.contains("not permitted")
}

/// Folders that could not be read for lack of permission. Placeholders
/// (iCloud files not stored locally) are expected and never counted.
fn needs_access(report: &ScanReport) -> bool {
    report.inspection_errors.iter().any(|e| denied(&e.message))
        || report
            .incomplete_reasons
            .iter()
            .any(|r| !r.to_lowercase().contains("placeholder") && denied(r))
}

/// How many distinct items could not be read: inspection errors plus the
/// per-item reasons (one item can carry several), without limits, budgets and
/// placeholders, which are not unreadable items.
fn unread_items(report: &ScanReport) -> u64 {
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut count = report.inspection_errors.len() as u64;
    for reason in &report.incomplete_reasons {
        let lower = reason.to_lowercase();
        if lower.contains("placeholder") || lower.contains("limit") || lower.contains("budget") {
            continue;
        }
        let item = reason
            .strip_prefix("metadata unavailable: ")
            .and_then(|rest| rest.split_once(": ").map(|(path, _)| path))
            .unwrap_or(reason.as_str());
        if seen.insert(item) {
            count += 1;
        }
    }
    count
}

fn limited(report: &ScanReport) -> bool {
    report.incomplete_reasons.iter().any(|r| {
        let r = r.to_lowercase();
        !r.contains("placeholder") && (r.contains("budget") || r.contains("entry limit"))
    })
}

/// Reasons that make folder totals low: limits, budgets and unreadable
/// folders. Single entries without measurable size (sockets, special files)
/// do not change the totals and are not reported as a partial scan.
fn material(report: &ScanReport) -> bool {
    report.incomplete_reasons.iter().any(|r| {
        let r = r.to_lowercase();
        !r.contains("placeholder")
            && ["limit", "budget", "not inspectable", "denied", "not permitted"]
                .iter()
                .any(|k| r.contains(k))
    })
}

fn root_label(root: &Path) -> String {
    if root == home() {
        "Home".into()
    } else if root == Path::new("/") {
        "Macintosh HD".into()
    } else if root.file_name().is_none() {
        // A drive root such as `C:\` has no file name; show it as written.
        root.to_string_lossy().trim_end_matches(['\\', '/']).to_string()
    } else {
        root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into())
    }
}

/// The folder nodes of a report. Each folder gets its total, its child folders
/// and kept files as rows, and only its `TOP` largest rows; `label` names the row
/// that holds the bytes not itemised.
fn fold_nodes(report: &ScanReport, label: &str) -> HashMap<PathBuf, Node> {
    let root = report.roots.first().cloned().unwrap_or_default();
    let mut nodes: HashMap<PathBuf, Node> = HashMap::with_capacity(report.folders.len());
    for f in &report.folders {
        nodes.entry(f.path.clone()).or_default().total = f.attributed_allocation_bytes;
    }
    for f in &report.folders {
        if f.path == root {
            continue;
        }
        let (Some(parent), Some(name)) = (f.path.parent(), f.path.file_name()) else {
            continue;
        };
        if let Some(node) = nodes.get_mut(parent) {
            node.dirs = node.dirs.saturating_add(f.attributed_allocation_bytes);
            node.items.push(Item {
                name: name.to_string_lossy().into(),
                is_dir: true,
                summary: false,
                bytes: f.attributed_allocation_bytes,
            });
        }
    }
    for e in &report.entries {
        if e.metadata.kind != EntryKind::File {
            continue;
        }
        let (Some(parent), Some(name)) = (e.path.parent(), e.path.file_name()) else {
            continue;
        };
        if let Some(node) = nodes.get_mut(parent) {
            node.kept = node.kept.saturating_add(e.attributed_allocation_bytes);
            node.items.push(Item {
                name: name.to_string_lossy().into(),
                is_dir: false,
                summary: false,
                bytes: e.attributed_allocation_bytes,
            });
        }
    }
    for node in nodes.values_mut() {
        let other = node.total.saturating_sub(node.dirs.saturating_add(node.kept));
        node.children = node.items.len();
        if other > 0 {
            node.items.push(Item {
                name: label.into(),
                is_dir: false,
                summary: true,
                bytes: other,
            });
        }
        node.items.sort_by(|a, b| b.bytes.cmp(&a.bytes));
        node.items.truncate(TOP);
        node.items.shrink_to_fit();
    }
    nodes
}

/// Fold a report into the browsable index. `from_snapshot` names the summary
/// row: bytes not itemised (small files, or all files for a saved scan).
fn build(report: &ScanReport, scanned_at: u64, from_snapshot: bool) -> Index {
    let root = report.roots.first().cloned().unwrap_or_default();
    let label = if from_snapshot { "Files in this folder" } else { SMALLER_FILES };
    Index {
        root_label: root_label(&root),
        root,
        scanned_at,
        from_snapshot,
        incomplete: report.accounting.incomplete && material(report),
        needs_access: needs_access(report),
        unread: unread_items(report),
        limited: limited(report),
        nodes: fold_nodes(report, label),
    }
}

fn folder(index: &Index, path: &Path) -> Result<Folder, String> {
    let node = index.nodes.get(path).ok_or_else(|| format!("not in the scan: {}", path.display()))?;
    let rows = node
        .items
        .iter()
        .map(|i| Row {
            path: if i.summary { path.to_path_buf() } else { path.join(&*i.name) },
            name: i.name.to_string(),
            is_dir: i.is_dir,
            bytes: i.bytes,
            summary: i.summary,
        })
        .collect();
    Ok(Folder {
        path: path.to_path_buf(),
        root: index.root.clone(),
        rows,
        total_children: node.children,
        incomplete: index.incomplete,
        needs_access: index.needs_access,
        unread: index.unread,
        limited: index.limited,
        root_label: index.root_label.clone(),
        scanned_at: index.scanned_at,
        from_snapshot: index.from_snapshot,
    })
}

/// The folder index if it is in memory. Never loads it.
fn held() -> Option<Arc<Index>> {
    lock(&INDEX).clone()
}

/// Note that the Storage page used the index now. The first use starts the idle check.
fn touch() {
    *lock(&LAST_USE) = Some(Instant::now());
    IDLE_TIMER.call_once(|| {
        let _ = std::thread::Builder::new().name("pulse-idle".into()).spawn(idle_loop);
    });
}

fn idle_loop() {
    low_priority();
    loop {
        std::thread::sleep(IDLE_CHECK);
        unload_if_idle();
    }
}

/// Resident memory of this process in MB, from `ps` (logging only).
#[cfg(not(unix))]
fn rss_mb() -> Option<u64> {
    None
}

#[cfg(unix)]
fn rss_mb() -> Option<u64> {
    let out = std::process::Command::new("/bin/ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    let kb: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
    Some(kb / 1024)
}

/// One line per scan in `~/Library/Application Support/Pulse/scan.log` (and
/// on stderr when `PULSE_SCAN_LOG` is set), so scan cost can be compared.
pub(crate) fn log(line: &str) {
    if std::env::var_os("PULSE_SCAN_LOG").is_some() {
        eprintln!("{line}");
    }
    let dir = cache::dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("scan.log");
    // Keep the log small: start over past 64 KiB.
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 64 * 1024) {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{} {line}", now());
    }
}

// --- Saved folder index (binary) -------------------------------------------------

/// Text as a 4-byte length, then its UTF-8 bytes.
fn write_text<W: Write>(out: &mut W, text: &str) -> std::io::Result<()> {
    let len = u32::try_from(text.len()).map_err(std::io::Error::other)?;
    out.write_all(&len.to_le_bytes())?;
    out.write_all(text.as_bytes())
}

/// Write the folder index, one folder after another, straight to `out`. The
/// layout: magic, version, root, root label, scan time, flags (bit 0 incomplete,
/// bit 1 needs access, bit 2 limited, bit 3 from a snapshot), folder count, then
/// per folder its path, child count, total, dirs, kept, row count and its rows
/// (name, kind bits: 1 folder, 2 summary row, then bytes).
fn write_index<W: Write>(out: &mut W, index: &Index) -> std::io::Result<()> {
    out.write_all(&INDEX_MAGIC)?;
    out.write_all(&INDEX_VERSION.to_le_bytes())?;
    write_text(out, &index.root.to_string_lossy())?;
    write_text(out, &index.root_label)?;
    out.write_all(&index.scanned_at.to_le_bytes())?;
    let flags = u8::from(index.incomplete)
        | (u8::from(index.needs_access) << 1)
        | (u8::from(index.limited) << 2)
        | (u8::from(index.from_snapshot) << 3);
    out.write_all(&[flags])?;
    out.write_all(&index.unread.to_le_bytes())?;
    out.write_all(&(index.nodes.len() as u64).to_le_bytes())?;
    for (path, node) in &index.nodes {
        write_text(out, &path.to_string_lossy())?;
        out.write_all(&(node.children as u64).to_le_bytes())?;
        out.write_all(&node.total.to_le_bytes())?;
        out.write_all(&node.dirs.to_le_bytes())?;
        out.write_all(&node.kept.to_le_bytes())?;
        let rows = u32::try_from(node.items.len()).map_err(std::io::Error::other)?;
        out.write_all(&rows.to_le_bytes())?;
        for item in &node.items {
            write_text(out, &item.name)?;
            out.write_all(&[u8::from(item.is_dir) | (u8::from(item.summary) << 1)])?;
            out.write_all(&item.bytes.to_le_bytes())?;
        }
    }
    Ok(())
}

fn invalid(what: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, what.to_string())
}

fn read_u8<R: Read>(input: &mut R) -> std::io::Result<u8> {
    let mut bytes = [0u8; 1];
    input.read_exact(&mut bytes)?;
    Ok(bytes[0])
}

fn read_u32<R: Read>(input: &mut R) -> std::io::Result<u32> {
    let mut bytes = [0u8; 4];
    input.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64<R: Read>(input: &mut R) -> std::io::Result<u64> {
    let mut bytes = [0u8; 8];
    input.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

fn read_text<R: Read>(input: &mut R) -> std::io::Result<String> {
    let len = read_u32(input)? as usize;
    if len > MAX_SAVED_TEXT {
        return Err(invalid("a saved name is too long"));
    }
    let mut bytes = vec![0u8; len];
    input.read_exact(&mut bytes)?;
    String::from_utf8(bytes).map_err(std::io::Error::other)
}

/// Read what `write_index` wrote. Streamed: the file is never held whole in memory.
fn read_index<R: Read>(input: &mut R) -> std::io::Result<Index> {
    let mut magic = [0u8; 4];
    input.read_exact(&mut magic)?;
    let version = if magic == INDEX_MAGIC { read_u32(input)? } else { 0 };
    if version != INDEX_VERSION && version != INDEX_VERSION_V2 {
        return Err(invalid("not a storage index"));
    }
    let root = PathBuf::from(read_text(input)?);
    let root_label = read_text(input)?;
    let scanned_at = read_u64(input)?;
    let flags = read_u8(input)?;
    let unread = if version == INDEX_VERSION { read_u64(input)? } else { 0 };
    let folders = read_u64(input)?;
    let mut nodes: HashMap<PathBuf, Node> = HashMap::new();
    for _ in 0..folders {
        let path = PathBuf::from(read_text(input)?);
        let children = usize::try_from(read_u64(input)?).map_err(std::io::Error::other)?;
        let total = read_u64(input)?;
        let dirs = read_u64(input)?;
        let kept = read_u64(input)?;
        let rows = read_u32(input)? as usize;
        if rows > TOP {
            return Err(invalid("a folder has too many rows"));
        }
        let mut items = Vec::with_capacity(rows);
        for _ in 0..rows {
            let name = read_text(input)?.into_boxed_str();
            let kind = read_u8(input)?;
            let bytes = read_u64(input)?;
            items.push(Item {
                name,
                is_dir: kind & 1 != 0,
                summary: kind & 2 != 0,
                bytes,
            });
        }
        nodes.insert(path, Node { total, dirs, kept, children, items });
    }
    Ok(Index {
        root,
        root_label,
        scanned_at,
        from_snapshot: flags & 8 != 0,
        incomplete: flags & 1 != 0,
        needs_access: flags & 2 != 0,
        unread,
        limited: flags & 4 != 0,
        nodes,
    })
}

/// One folder as saved before the binary format: path, child count and rows.
#[derive(Deserialize)]
struct SavedFolder(String, usize, Vec<SavedRow>);

/// One row as saved before the binary format: name, is a folder, is a summary row, bytes.
#[derive(Deserialize)]
struct SavedRow(String, bool, bool, u64);

#[derive(Deserialize)]
struct SavedIndex {
    root: PathBuf,
    root_label: String,
    scanned_at: u64,
    incomplete: bool,
    needs_access: bool,
    limited: bool,
    folders: Vec<SavedFolder>,
}

/// The JSON index of earlier versions. The totals were not saved then, so they
/// are rebuilt from the rows: exact unless a folder's rows were cut to `TOP`.
fn restored_index(saved: SavedIndex) -> Index {
    let nodes: HashMap<PathBuf, Node> = saved
        .folders
        .into_iter()
        .map(|SavedFolder(path, children, rows)| {
            let items: Vec<Item> = rows
                .into_iter()
                .map(|SavedRow(name, is_dir, summary, bytes)| Item {
                    name: name.into_boxed_str(),
                    is_dir,
                    summary,
                    bytes,
                })
                .collect();
            let total = items.iter().fold(0u64, |sum, i| sum.saturating_add(i.bytes));
            let dirs = items
                .iter()
                .filter(|i| i.is_dir && !i.summary)
                .fold(0u64, |sum, i| sum.saturating_add(i.bytes));
            let kept = items
                .iter()
                .filter(|i| !i.is_dir && !i.summary)
                .fold(0u64, |sum, i| sum.saturating_add(i.bytes));
            (PathBuf::from(path), Node { total, dirs, kept, children, items })
        })
        .collect();
    Index {
        root: saved.root,
        root_label: saved.root_label,
        scanned_at: saved.scanned_at,
        from_snapshot: true,
        incomplete: saved.incomplete,
        needs_access: saved.needs_access,
        unread: 0,
        limited: saved.limited,
        nodes,
    }
}

/// The folder index saved on disk: the binary file, else the JSON file of
/// earlier versions. `None` when there is neither, or both are unreadable.
fn read_saved_index() -> Option<Index> {
    if let Some(mut input) = cache::open(INDEX_FILE_V2) {
        match read_index(&mut input) {
            Ok(index) => return Some(index),
            Err(error) => log(&format!("the saved storage index could not be read: {error}")),
        }
    }
    let saved: SavedIndex = cache::load(INDEX_FILE, FORMAT)?;
    Some(restored_index(saved))
}

// --- Saving ---------------------------------------------------------------------

/// Folders whose name rows are not in the saved name file, read from `STALE_FILE`.
fn read_stale() -> BTreeSet<PathBuf> {
    let Some(bytes) = cache::read_bytes(STALE_FILE) else {
        return BTreeSet::new();
    };
    let text = String::from_utf8_lossy(&bytes).into_owned();
    text.lines()
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// The folders in `STALE` as a list.
fn stale_snapshot() -> Vec<PathBuf> {
    lock(&STALE).iter().cloned().collect()
}

/// Write `stale` as the stale list, or remove the file when it is empty.
fn write_stale(stale: &[PathBuf]) -> std::io::Result<()> {
    if stale.is_empty() {
        return cache::remove(STALE_FILE);
    }
    let mut body = String::new();
    for path in stale {
        body.push_str(&path.to_string_lossy());
        body.push('\n');
    }
    cache::write_bytes(STALE_FILE, body.as_bytes())
}

/// Write the folder index, the root view and the name rows, or, when the name
/// rows are not in memory, the list of folders they are missing. Saves are
/// serialised; the index is streamed to its file, never built in memory first.
fn save_all(
    index: &Index,
    view: &Folder,
    names: Option<&NameIndex>,
    stale: &[PathBuf],
) -> std::io::Result<()> {
    let _saving = lock(&SAVE_LOCK);
    let started = Instant::now();
    cache::write_with(INDEX_FILE_V2, |out| write_index(out, index))?;
    // The binary index replaces the JSON one, which is removed to free its space.
    let _ = cache::remove(INDEX_FILE);
    cache::save(VIEW_FILE, FORMAT, view)?;
    let index_ms = started.elapsed().as_millis();
    let names_started = Instant::now();
    if NAMES_BROKEN.swap(false, Ordering::SeqCst) {
        cache::remove(NAMES_FILE)?;
        cache::remove(STALE_FILE)?;
        lock(&STALE).clear();
    } else if let Some(names) = names {
        cache::write_with(NAMES_FILE, |out| names.write_to(out))?;
        cache::remove(STALE_FILE)?;
        lock(&STALE).clear();
    } else {
        write_stale(stale)?;
    }
    log(&format!(
        "saved storage scan: index {index_ms} ms, names {} ms, total {} ms",
        names_started.elapsed().as_millis(),
        started.elapsed().as_millis(),
    ));
    Ok(())
}

/// Write the saved files on a background thread, so the next launch can show
/// them at once. `release_names` drops the name rows from memory once they are
/// saved (a search reads them back from the file). Failures are logged, never shown.
fn persist(
    index: Arc<Index>,
    view: Folder,
    names: Option<Arc<NameIndex>>,
    stale: Vec<PathBuf>,
    release_names: bool,
) {
    let at = view.scanned_at;
    SAVING.fetch_add(1, Ordering::SeqCst);
    let spawned = std::thread::Builder::new().name("pulse-save".into()).spawn(move || {
        low_priority();
        if at >= SAVED_AT.load(Ordering::SeqCst) {
            match save_all(&index, &view, names.as_deref(), &stale) {
                Ok(()) => {
                    SAVED_AT.store(at, Ordering::SeqCst);
                    if release_names {
                        release(&names);
                    }
                }
                Err(error) => log(&format!("saving the storage scan failed: {error}")),
            }
        }
        SAVING.fetch_sub(1, Ordering::SeqCst);
    });
    if let Err(error) = spawned {
        SAVING.fetch_sub(1, Ordering::SeqCst);
        log(&format!("saving the storage scan failed: {error}"));
    }
}

/// Drop `saved` from memory, if it is still the name rows in memory.
fn release(saved: &Option<Arc<NameIndex>>) {
    let Some(saved) = saved else {
        return;
    };
    let mut slot = lock(&NAMES);
    if slot.as_ref().is_some_and(|current| Arc::ptr_eq(current, saved)) {
        *slot = None;
    }
}

/// Save the index if it changed and the last save is old enough. Called from
/// the watch loop.
pub(crate) fn save_if_due() {
    if !DIRTY.load(Ordering::SeqCst) {
        return;
    }
    {
        let mut last = lock(&LAST_SAVE);
        if last.is_some_and(|at| at.elapsed() < SAVE_INTERVAL) {
            return;
        }
        *last = Some(Instant::now());
    }
    DIRTY.store(false, Ordering::SeqCst);
    let Some(index) = held() else {
        return;
    };
    let Ok(view) = folder(&index, &index.root) else {
        return;
    };
    let names = lock(&NAMES).clone();
    let stale = if names.is_none() { stale_snapshot() } else { Vec::new() };
    persist(index, view, names, stale, false);
}

/// Save the index in the calling thread, for an idle unload.
fn save_now(index: &Arc<Index>) -> Result<(), String> {
    let view = folder(index, &index.root)?;
    let names = lock(&NAMES).clone();
    let stale = if names.is_none() { stale_snapshot() } else { Vec::new() };
    save_all(index, &view, names.as_deref(), &stale).map_err(|e| e.to_string())
}

/// Drop the folder index and the name rows from memory after `IDLE_UNLOAD`
/// without Storage use. Saves first if anything changed. The live watch keeps
/// running and collects changes in `WAITING` meanwhile.
fn unload_if_idle() {
    let idle = match *lock(&LAST_USE) {
        Some(at) => at.elapsed() >= IDLE_UNLOAD,
        None => true,
    };
    if !idle || lock(&JOB).is_some() {
        return;
    }
    // A load in progress keeps the index and names in memory until it ends.
    let Ok(_loading) = LOADING.try_lock() else {
        return;
    };
    let Some(index) = held() else {
        return;
    };
    let dirty = DIRTY.load(Ordering::SeqCst);
    if dirty {
        SAVING.fetch_add(1, Ordering::SeqCst);
        let saved = save_now(&index);
        SAVING.fetch_sub(1, Ordering::SeqCst);
        if let Err(error) = saved {
            log(&format!("saving the storage scan before unloading failed: {error}"));
            return;
        }
    }
    let mut slot = lock(&INDEX);
    // A change or a new scan made a different index: keep this one for now.
    if !slot.as_ref().is_some_and(|current| Arc::ptr_eq(current, &index)) {
        return;
    }
    if dirty {
        DIRTY.store(false, Ordering::SeqCst);
    }
    *slot = None;
    *lock(&NAMES) = None;
    drop(slot);
    drop(index);
    log(&format!(
        "storage index unloaded after {} s without use, rss {} MB",
        IDLE_UNLOAD.as_secs(),
        rss_mb().map_or_else(|| "?".to_string(), |m| m.to_string()),
    ));
}

// --- Loading --------------------------------------------------------------------

/// The folder index in memory, read from disk when it is not there. `None`
/// when no saved index exists.
fn restore() -> Option<Arc<Index>> {
    touch();
    if let Some(index) = held() {
        return Some(index);
    }
    let _loading = lock(&LOADING);
    if let Some(index) = held() {
        return Some(index);
    }
    let started = Instant::now();
    let mut index = read_saved_index()?;
    index.from_snapshot = !SCANNED.load(Ordering::SeqCst);
    let folders = index.nodes.len();
    {
        // The saved stale list is the one to apply, unless this session kept its own.
        let mut stale = lock(&STALE);
        if stale.is_empty() {
            *stale = read_stale();
        }
    }
    let index = Arc::new(index);
    let mut slot = lock(&INDEX);
    // A scan that finished while this was loading is newer and wins.
    let shown = slot.get_or_insert(index).clone();
    drop(slot);
    log(&format!(
        "loaded storage index in {} ms, {folders} folders",
        started.elapsed().as_millis(),
    ));
    Some(shown)
}

/// Put the freshly read name rows of one folder into `rows`. `fresh` is `None`
/// when the folder is gone. `false` when the rows cannot be kept in step.
fn apply_names(rows: &mut NameIndex, folder: &Path, fresh: Option<&NameIndex>) -> bool {
    match (rows.find_dir(folder), fresh) {
        (Some(row), Some(sub)) => rows.replace_subtree(row, sub),
        (Some(row), None) => {
            rows.remove_subtree(row);
            rows.compact_if_sparse();
            true
        }
        (None, None) => true,
        (None, Some(_)) => false,
    }
}

/// The name rows, loaded on the first search. The saved rows are first brought
/// up to date with the folders that changed since they were written. `None`
/// when they are missing, cannot be kept in step, or a scan is running (search
/// then uses the folder rows only, without waiting for the scan).
fn load_names(index: &Index) -> Option<Arc<NameIndex>> {
    touch();
    if let Some(names) = lock(&NAMES).clone() {
        return Some(names);
    }
    let _loading = lock(&LOADING);
    if let Some(names) = lock(&NAMES).clone() {
        return Some(names);
    }
    if NAMES_BROKEN.load(Ordering::SeqCst) {
        return None;
    }
    let started = Instant::now();
    let mut names = NameIndex::from_bytes(&cache::read_bytes(NAMES_FILE)?)?;
    let changed = covering_folders(stale_snapshot());
    if changed.len() > STALE_MAX {
        NAMES_BROKEN.store(true, Ordering::SeqCst);
        return None;
    }
    // Reading folders again takes the scan lock; never wait for a scan to finish.
    let _run = if changed.is_empty() {
        None
    } else {
        match RUN.try_lock() {
            Ok(guard) => Some(guard),
            Err(_) => return None,
        }
    };
    for folder in &changed {
        let fresh = if index.nodes.contains_key(folder) {
            let options = ScanOptions {
                max_entries: LIVE_MAX_ENTRIES,
                ..scan_options(None)
            };
            let (report, rows) = pulse_core::scan::scan_with_names(&[folder.clone()], &options);
            if limited(&report) {
                NAMES_BROKEN.store(true, Ordering::SeqCst);
                return None;
            }
            drop(report);
            Some(rows)
        } else {
            None
        };
        if !apply_names(&mut names, folder, fresh.as_ref()) {
            NAMES_BROKEN.store(true, Ordering::SeqCst);
            return None;
        }
    }
    let names = Arc::new(names);
    let mut slot = lock(&NAMES);
    let shown = slot.get_or_insert(names).clone();
    drop(slot);
    log(&format!(
        "loaded storage names in {} ms, {} folders brought up to date",
        started.elapsed().as_millis(),
        changed.len(),
    ));
    Some(shown)
}

fn scan_options(cancel: Option<Arc<AtomicBool>>) -> ScanOptions {
    ScanOptions {
        max_entries: MAX_ENTRIES,
        keep_files_per_folder: Some(TOP),
        min_kept_file_bytes: MIN_KEPT_FILE_BYTES,
        cancel,
        ..ScanOptions::default()
    }
}

fn run_scan(root: PathBuf, id: u64, cancel: Arc<AtomicBool>, app: AppHandle) -> Result<Folder, String> {
    low_priority();
    let _run = exclusive();
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    // Changes made during the walk are replayed by the watch started below.
    let since = watch::current_event_id();
    let started = Instant::now();
    let (report, names) = pulse_core::scan::scan_with_names(
        &[root.clone()],
        &scan_options(Some(cancel.clone())),
    );
    let walk_ms = started.elapsed().as_millis();
    if cancel.load(Ordering::Relaxed) {
        log(&format!("scan {} cancelled after {walk_ms} ms", root.display()));
        return Err("cancelled".into());
    }
    if root == home() {
        growth::save_in_background(&report);
    }
    let indexed = Instant::now();
    let built = build(&report, now(), false);
    let (kept, folders) = (report.entries.len(), report.folders.len());
    drop(report);
    let mut names = names;
    names.shrink_to_fit();
    let entries = names.live_rows();
    let index = Arc::new(built);
    let names = Arc::new(names);
    let result = folder(&index, &index.root);
    let epoch = {
        // Only the newest scan is shown.
        let mut job = lock(&JOB);
        if !job.as_ref().is_some_and(|j| j.id == id) {
            return Err("cancelled".into());
        }
        *job = None;
        *lock(&INDEX) = Some(index.clone());
        *lock(&NAMES) = Some(names.clone());
        *lock(&SUMMARY) = Some(Summary {
            root: index.root.clone(),
            scanned_at: index.scanned_at,
            from_snapshot: false,
        });
        lock(&STALE).clear();
        lock(&WAITING).clear();
        lock(&ATTEMPTED).clear();
        lock(&OVERSIZED).clear();
        NAMES_BROKEN.store(false, Ordering::SeqCst);
        SCANNED.store(true, Ordering::SeqCst);
        INDEX_EPOCH.fetch_add(1, Ordering::SeqCst) + 1
    };
    touch();
    if let Ok(view) = &result {
        // The name rows leave memory once saved; the first search reads them back.
        persist(index.clone(), view.clone(), Some(names.clone()), Vec::new(), true);
        DIRTY.store(false, Ordering::SeqCst);
        *lock(&LAST_SAVE) = Some(Instant::now());
        // FSEvents reports real paths, so watch the canonical root the index holds.
        watch::start(app, index.root.clone(), since, epoch);
    }
    log(&format!(
        "scan {} done: walk {walk_ms} ms, {entries} entries, {kept} kept, {folders} folders, index {} ms, rss {} MB",
        root.display(),
        indexed.elapsed().as_millis(),
        rss_mb().map_or_else(|| "?".to_string(), |m| m.to_string()),
    ));
    result
}

/// Scan `path` (default: home), replacing any scan in progress, and return its
/// top level. Fails with "cancelled" when a newer scan took over.
#[tauri::command]
pub async fn scan(app: AppHandle, path: Option<String>) -> Result<Folder, String> {
    let root = path.map(PathBuf::from).unwrap_or_else(home);
    touch();
    // The live refresh belongs to the index it was started for; a new scan replaces it.
    watch::stop();
    let cancel = Arc::new(AtomicBool::new(false));
    let id = NEXT_JOB.fetch_add(1, Ordering::SeqCst);
    {
        let mut job = lock(&JOB);
        if let Some(old) = job.as_ref() {
            old.cancel.store(true, Ordering::Relaxed);
        }
        *job = Some(Job { id, root: root.clone(), cancel: cancel.clone() });
    }
    tauri::async_runtime::spawn_blocking(move || {
        std::thread::Builder::new()
            .name("pulse-scan".into())
            .spawn(move || run_scan(root, id, cancel, app))
            .map_err(|e| e.to_string())?
            .join()
            .map_err(|_| "scan failed".to_string())?
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Serialize)]
pub struct Status {
    running: bool,
    running_root: Option<PathBuf>,
    has_index: bool,
    root: Option<PathBuf>,
    scanned_at: Option<u64>,
    from_snapshot: bool,
    /// Whether the index is kept current while the hub runs (a change journal
    /// feeds `watch`). Where it is not, a saved scan only gets older.
    live_refresh: bool,
}

/// The scan's state. Reads what the Storage page shows, never loads the index.
#[tauri::command]
pub fn scan_status() -> Status {
    let running_root = lock(&JOB).as_ref().map(|j| j.root.clone());
    let shown = lock(&SUMMARY).clone();
    Status {
        running: running_root.is_some(),
        running_root,
        has_index: shown.is_some(),
        root: shown.as_ref().map(|s| s.root.clone()),
        scanned_at: shown.as_ref().map(|s| s.scanned_at),
        from_snapshot: shown.as_ref().is_some_and(|s| s.from_snapshot),
        // FSEvents (macOS) and ReadDirectoryChangesW (Windows) feed `watch`.
        live_refresh: watch::live_refresh(),
    }
}

/// The newest folder view: the one held in memory, else the root view saved
/// with the last scan, shown at once. `None` when there is neither. Never scans,
/// and never loads the folder index (that waits for the first drill-down).
#[tauri::command]
pub async fn last_scan() -> Result<Option<Folder>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        touch();
        if let Some(index) = held() {
            return folder(&index, &index.root).map(Some);
        }
        if let Some(mut view) = cache::load::<Folder>(VIEW_FILE, FORMAT) {
            view.from_snapshot = !SCANNED.load(Ordering::SeqCst);
            *lock(&SUMMARY) = Some(Summary {
                root: view.root.clone(),
                scanned_at: view.scanned_at,
                from_snapshot: view.from_snapshot,
            });
            return Ok(Some(view));
        }
        // Saves from before the index file: the newest home snapshot, if one can be read.
        let root = home();
        let Some((report, at)) = growth::latest_scan(&root) else {
            return Ok(None);
        };
        let index = Arc::new(build(&report, at, true));
        drop(report);
        let result = folder(&index, &index.root).map(Some);
        *lock(&SUMMARY) = Some(Summary {
            root: index.root.clone(),
            scanned_at: index.scanned_at,
            from_snapshot: true,
        });
        let mut slot = lock(&INDEX);
        if slot.is_none() {
            *slot = Some(index);
        }
        result
    })
    .await
    .map_err(|e| e.to_string())?
}

/// One folder of the scan. Loads the saved index first when it is not in memory.
#[tauri::command]
pub async fn children(path: String) -> Result<Folder, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<Folder, String> {
        let index = restore().ok_or("scan first")?;
        folder(&index, &PathBuf::from(path))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Whether `text` contains `needle` ignoring case. `needle` is lower-case already.
fn contains_ci(text: &str, needle: &str) -> bool {
    if text.is_ascii() && needle.is_ascii() {
        let (haystack, pattern) = (text.as_bytes(), needle.as_bytes());
        if pattern.is_empty() {
            return true;
        }
        return haystack.windows(pattern.len()).any(|w| w.eq_ignore_ascii_case(pattern));
    }
    text.to_lowercase().contains(needle)
}

/// Whether `name` ends in `.ext` (ignoring case), with something before the dot.
fn extension_is(name: &str, ext: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(stem, found)| !stem.is_empty() && found.eq_ignore_ascii_case(ext))
}

/// Matches over every name the scan kept, largest first.
fn search_names(names: &NameIndex, needle: &str, exts: &[String], limit: usize) -> Vec<Row> {
    let mut hits: Vec<(u64, usize)> = Vec::new();
    // Row 0 is the scanned folder itself, not a match.
    for row in 1..names.rows() {
        if !names.is_live(row) || names.is_other(row) {
            continue;
        }
        let name = names.name(row);
        if !exts.is_empty()
            && (names.is_dir(row) || !exts.iter().any(|ext| extension_is(name, ext)))
        {
            continue;
        }
        if !needle.is_empty() && !contains_ci(name, needle) {
            continue;
        }
        hits.push((names.size(row), row));
    }
    let by_size = |a: &(u64, usize), b: &(u64, usize)| b.0.cmp(&a.0).then(a.1.cmp(&b.1));
    if hits.len() > limit {
        hits.select_nth_unstable_by(limit - 1, by_size);
        hits.truncate(limit);
    }
    hits.sort_unstable_by(by_size);
    hits.into_iter()
        .filter_map(|(bytes, row)| {
            Some(Row {
                path: names.path_of(row)?,
                name: names.name(row).to_string(),
                is_dir: names.is_dir(row),
                bytes,
                summary: false,
            })
        })
        .collect()
}

/// Matches among the rows of the folder view only (used when no name rows exist).
fn search_folders(index: &Index, needle: &str, limit: usize) -> Vec<Row> {
    if needle.is_empty() {
        return Vec::new();
    }
    let mut found: Vec<Row> = Vec::new();
    for (parent, node) in &index.nodes {
        for item in node.items.iter().filter(|i| !i.summary) {
            if item.name.to_lowercase().contains(needle) {
                found.push(Row {
                    path: parent.join(&*item.name),
                    name: item.name.to_string(),
                    is_dir: item.is_dir,
                    bytes: item.bytes,
                    summary: false,
                });
            }
        }
    }
    found.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    found.truncate(limit);
    found
}

/// Files and folders in the last scan whose name contains `query` (ignoring
/// case), largest first. `extensions` (for example `["pdf", "mov"]`, with or
/// without the dot) keeps only files with those extensions. Up to `limit`
/// results (default 100, at most 1000). With no query and no extensions, nothing.
#[tauri::command]
pub async fn search(
    query: String,
    limit: Option<usize>,
    extensions: Option<Vec<String>>,
) -> Result<Vec<Row>, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<Vec<Row>, String> {
        let limit = limit.unwrap_or(100).clamp(1, 1000);
        let needle = query.trim().to_lowercase();
        let exts: Vec<String> = extensions
            .unwrap_or_default()
            .iter()
            .map(|ext| ext.trim().trim_start_matches('.').to_lowercase())
            .filter(|ext| !ext.is_empty())
            .collect();
        // The whole-disk name index answers when it is ready (ranked by name
        // match); otherwise, and for extension-only searches, the last scan does.
        if let Some(found) = crate::disk_index::search(&query, limit, &exts) {
            return Ok(found
                .into_iter()
                .map(|f| Row { path: f.path, name: f.name, is_dir: f.is_dir, bytes: f.bytes, summary: false })
                .collect());
        }
        let index = restore().ok_or("scan first")?;
        if needle.is_empty() && exts.is_empty() {
            return Ok(Vec::new());
        }
        Ok(match load_names(&index) {
            Some(names) => search_names(&names, &needle, &exts, limit),
            None => search_folders(&index, &needle, limit),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Payload of the `storage-updated` event: the folders that were read again,
/// and `stale` when the index can no longer be trusted (changes were lost, or a
/// folder is too large to read again here, so a new scan is needed).
#[derive(Serialize, Clone)]
pub(crate) struct Updated {
    pub folders: Vec<String>,
    pub stale: bool,
}

/// What `apply_changes` did: the folders read again, and whether the index can
/// no longer be trusted.
pub(crate) struct Applied {
    pub folders: Vec<String>,
    pub stale: bool,
}

/// What a live refresh of one folder did.
pub(crate) enum Refresh {
    /// The folder's subtree was read again and the index was updated.
    Applied,
    /// The subtree is larger than `LIVE_MAX_ENTRIES`.
    TooLarge,
    /// Nothing was done: a scan is running, or the index was replaced.
    Skipped,
    /// Not now: the index is not in memory, or a save is writing it. Try again later.
    Busy,
}

/// The indexed folder that holds `path`: the folder itself, else its nearest
/// indexed parent. `None` when `path` is outside the scan.
fn nearest_indexed(index: &Index, path: &Path) -> Option<PathBuf> {
    let mut at = Some(path);
    while let Some(folder) = at {
        if index.nodes.contains_key(folder) {
            return Some(folder.to_path_buf());
        }
        at = folder.parent();
    }
    None
}

/// Whether the index is still the one a refresh started from.
fn same_index(epoch: u64) -> bool {
    epoch == INDEX_EPOCH.load(Ordering::SeqCst)
}

/// Whether a refresh may run now: the same index, and no scan in progress.
fn may_refresh(epoch: u64) -> bool {
    same_index(epoch) && lock(&JOB).is_none()
}

/// Read `folder`'s subtree again and update the index in place. Runs after
/// any cleanup scan in progress, never alongside a storage scan. When the
/// name rows are not in memory, the folder is only noted for them.
fn refresh_subtree(folder: &Path, epoch: u64) -> Refresh {
    if !may_refresh(epoch) {
        return Refresh::Skipped;
    }
    if SAVING.load(Ordering::SeqCst) > 0 {
        return Refresh::Busy;
    }
    let _run = exclusive();
    if !may_refresh(epoch) {
        return Refresh::Skipped;
    }
    let options = ScanOptions {
        max_entries: LIVE_MAX_ENTRIES,
        ..scan_options(None)
    };
    let (report, names) = pulse_core::scan::scan_with_names(&[folder.to_path_buf()], &options);
    if limited(&report) {
        return Refresh::TooLarge;
    }
    let fresh = fold_nodes(&report, SMALLER_FILES);
    drop(report);
    let mut slot = lock(&INDEX);
    if SAVING.load(Ordering::SeqCst) > 0 {
        return Refresh::Busy;
    }
    if !same_index(epoch) {
        return Refresh::Skipped;
    }
    let Some(shared) = slot.as_mut() else {
        return Refresh::Busy;
    };
    let exists = fresh.contains_key(folder);
    apply_nodes(Arc::make_mut(shared), folder, fresh);
    let mut names_slot = lock(&NAMES);
    let kept = names_slot
        .as_mut()
        .map(|rows| apply_names(Arc::make_mut(rows), folder, exists.then_some(&names)));
    match kept {
        Some(true) => {}
        Some(false) => {
            *names_slot = None;
            NAMES_BROKEN.store(true, Ordering::SeqCst);
        }
        None => {
            let mut stale = lock(&STALE);
            stale.insert(folder.to_path_buf());
            if stale.len() > STALE_MAX {
                stale.clear();
                NAMES_BROKEN.store(true, Ordering::SeqCst);
            }
        }
    }
    DIRTY.store(true, Ordering::SeqCst);
    Refresh::Applied
}

/// The folders to read, without duplicates and without any folder that lies
/// inside another one in the list (reading the outer folder covers it).
fn covering_folders(mut folders: Vec<PathBuf>) -> Vec<PathBuf> {
    folders.sort();
    folders.dedup();
    let mut kept: Vec<PathBuf> = Vec::new();
    for folder in &folders {
        if !folders
            .iter()
            .any(|other| other != folder && folder.starts_with(other))
        {
            kept.push(folder.clone());
        }
    }
    kept
}

/// Take in the changed paths from the watch and read again the folders that
/// need it. While the index is not in memory the paths only wait; they are
/// applied once it loads. Returns the folders read again, and `stale` when the
/// changes can no longer be applied.
pub(crate) fn apply_changes(paths: Vec<PathBuf>, epoch: u64) -> Applied {
    let mut stale = false;
    {
        let mut waiting = lock(&WAITING);
        waiting.extend(paths);
        if waiting.len() > WAITING_MAX {
            waiting.clear();
            stale = true;
        }
    }
    let Some(index) = held() else {
        return Applied { folders: Vec::new(), stale };
    };
    let raw: Vec<PathBuf> = std::mem::take(&mut *lock(&WAITING)).into_iter().collect();
    let mut indexed: Vec<PathBuf> = raw
        .iter()
        .filter_map(|path| nearest_indexed(&index, path))
        .collect();
    drop(index);
    let now = Instant::now();
    lock(&ATTEMPTED).retain(|_, at| now.duration_since(*at) < RETRY);
    // Before the covering step: an oversized outer folder must not hide the small ones in it.
    {
        let mut oversized = lock(&OVERSIZED);
        oversized.retain(|_, at| now.duration_since(*at) < OVERSIZED_PAUSE);
        indexed.retain(|folder| !oversized.contains_key(folder));
    }
    let mut read: Vec<String> = Vec::new();
    for folder in covering_folders(indexed) {
        if lock(&ATTEMPTED).contains_key(&folder) {
            lock(&WAITING).insert(folder);
            continue;
        }
        lock(&ATTEMPTED).insert(folder.clone(), now);
        match refresh_subtree(&folder, epoch) {
            Refresh::Applied => read.push(folder.to_string_lossy().into_owned()),
            Refresh::TooLarge => {
                lock(&OVERSIZED).insert(folder, now);
                stale = true;
            }
            Refresh::Skipped => {}
            Refresh::Busy => {
                lock(&ATTEMPTED).remove(&folder);
                lock(&WAITING).insert(folder);
            }
        }
    }
    Applied { folders: read, stale }
}

/// `bytes` moved by `delta`, never below zero.
fn shift(bytes: u64, delta: i128) -> u64 {
    (i128::from(bytes) + delta).clamp(0, i128::from(u64::MAX)) as u64
}

/// Set the size shown for the folder `name` in `node`, if its row is kept.
fn set_dir_bytes(node: &mut Node, name: &str, bytes: u64) {
    if let Some(item) = node
        .items
        .iter_mut()
        .find(|i| !i.summary && i.is_dir && &*i.name == name)
    {
        item.bytes = bytes;
    }
}

/// Recompute the "smaller files" row of `node` and keep the largest rows.
fn refresh_summary(node: &mut Node) {
    node.items.retain(|i| !i.summary);
    let other = node.total.saturating_sub(node.dirs.saturating_add(node.kept));
    if other > 0 {
        node.items.push(Item {
            name: SMALLER_FILES.into(),
            is_dir: false,
            summary: true,
            bytes: other,
        });
    }
    node.items.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    node.items.truncate(TOP);
}

/// Put the freshly read `fresh` nodes of `folder`'s subtree into `index`, and
/// bring the folders above it in step: their totals and folder counts move by
/// the same change, and their rows for the folder below follow.
fn apply_nodes(index: &mut Index, folder: &Path, mut fresh: HashMap<PathBuf, Node>) {
    let exists = fresh.contains_key(folder);
    let new_total = fresh.get(folder).map_or(0, |node| node.total);
    let old_total = index.nodes.get(folder).map_or(0, |node| node.total);
    let delta = i128::from(new_total) - i128::from(old_total);

    index.nodes.retain(|path, _| !path.starts_with(folder));
    index.nodes.extend(fresh.drain());

    let mut child = folder.to_path_buf();
    let mut child_total = exists.then_some(new_total);
    loop {
        let Some(parent) = child.parent().map(Path::to_path_buf) else {
            break;
        };
        let Some(name) = child.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            break;
        };
        let Some(node) = index.nodes.get_mut(&parent) else {
            break;
        };
        node.total = shift(node.total, delta);
        node.dirs = shift(node.dirs, delta);
        match child_total {
            Some(bytes) => set_dir_bytes(node, &name, bytes),
            None => {
                node.items.retain(|i| i.summary || !i.is_dir || &*i.name != name.as_str());
                node.children = node.children.saturating_sub(1);
            }
        }
        refresh_summary(node);
        child_total = Some(node.total);
        child = parent;
    }
}
