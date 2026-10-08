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
//! the folders that changed; `refresh_subtree` reads just those again and
//! updates both parts in place.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use pulse_core::scan::NameIndex;
use pulse_core::{EntryKind, ScanOptions, ScanReport};
use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::{cache, growth, home, watch};

/// Format of the two saved files (`storage-index-v1.json` and
/// `storage-view-v1.json`). A different version is ignored, never migrated.
const FORMAT: u32 = 1;
const INDEX_FILE: &str = "storage-index-v1.json";
const VIEW_FILE: &str = "storage-view-v1.json";
/// The name rows (`NameIndex::to_bytes`), saved beside the folder index.
const NAMES_FILE: &str = "storage-names-v1.bin";

/// Newest scan time saved so far; an older scan finishing later never overwrites it.
static SAVED_AT: AtomicU64 = AtomicU64::new(0);
/// Serialises loading the saved index, so concurrent requests read it once.
static LOADING: Mutex<()> = Mutex::new(());

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

/// Event the page listens to: folders read again after a change (see `Updated`).
pub(crate) const UPDATED_EVENT: &str = "storage-updated";

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
}

/// QOS_CLASS_UTILITY for the calling thread.
fn low_priority() {
    #[cfg(target_os = "macos")]
    unsafe {
        pthread_set_qos_class_self_np(0x11, 0);
    }
}

/// Held while any heavy scan (storage or cleanup findings) runs.
static RUN: Mutex<()> = Mutex::new(());
static NEXT_JOB: AtomicU64 = AtomicU64::new(1);
static JOB: Mutex<Option<Job>> = Mutex::new(None);
static INDEX: Mutex<Option<Arc<Index>>> = Mutex::new(None);
/// Bumped each time a scan replaces the index. A live refresh applies only to
/// the index it started from.
static INDEX_EPOCH: AtomicU64 = AtomicU64::new(0);
/// The index has changes that are not saved yet.
static DIRTY: AtomicBool = AtomicBool::new(false);
static LAST_SAVE: Mutex<Option<Instant>> = Mutex::new(None);

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
    limited: bool,
    nodes: HashMap<PathBuf, Node>,
    /// Every name the scan visited. `None` for a saved index that has no name
    /// file, and after a refresh that could not keep the rows in step.
    names: Option<NameIndex>,
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
        limited: limited(report),
        nodes: fold_nodes(report, label),
        names: None,
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
        limited: index.limited,
        root_label: index.root_label.clone(),
        scanned_at: index.scanned_at,
        from_snapshot: index.from_snapshot,
    })
}

fn held() -> Option<Arc<Index>> {
    lock(&INDEX).clone()
}

/// One folder as saved: path, child count and the rows the browser shows.
#[derive(Serialize, Deserialize)]
struct SavedFolder(String, usize, Vec<SavedRow>);

/// One row as saved: name, is a folder, is a summary row, bytes.
#[derive(Serialize, Deserialize)]
struct SavedRow(String, bool, bool, u64);

#[derive(Serialize, Deserialize)]
struct SavedIndex {
    root: PathBuf,
    root_label: String,
    scanned_at: u64,
    incomplete: bool,
    needs_access: bool,
    limited: bool,
    folders: Vec<SavedFolder>,
}

fn saved_index(index: &Index) -> SavedIndex {
    SavedIndex {
        root: index.root.clone(),
        root_label: index.root_label.clone(),
        scanned_at: index.scanned_at,
        incomplete: index.incomplete,
        needs_access: index.needs_access,
        limited: index.limited,
        folders: index
            .nodes
            .iter()
            .map(|(path, node)| {
                SavedFolder(
                    path.to_string_lossy().into_owned(),
                    node.children,
                    node.items
                        .iter()
                        .map(|i| SavedRow(i.name.to_string(), i.is_dir, i.summary, i.bytes))
                        .collect(),
                )
            })
            .collect(),
    }
}

fn restored_index(saved: SavedIndex) -> Index {
    let nodes: HashMap<PathBuf, Node> = saved
        .folders
        .into_iter()
        .map(|SavedFolder(path, children, rows)| {
            let items = rows
                .into_iter()
                .map(|SavedRow(name, is_dir, summary, bytes)| Item {
                    name: name.into_boxed_str(),
                    is_dir,
                    summary,
                    bytes,
                })
                .collect();
            (PathBuf::from(path), Node { children, items, ..Node::default() })
        })
        .collect();
    Index {
        root: saved.root,
        root_label: saved.root_label,
        scanned_at: saved.scanned_at,
        from_snapshot: true,
        incomplete: saved.incomplete,
        needs_access: saved.needs_access,
        limited: saved.limited,
        nodes,
        names: None,
    }
}

/// Write the name rows, or remove the file when the index has none.
fn save_names(index: &Index) -> std::io::Result<()> {
    match index.names.as_ref() {
        Some(names) => cache::write_bytes(NAMES_FILE, &names.to_bytes()),
        None => cache::remove(NAMES_FILE),
    }
}

/// Write the folder index, the root view and the name rows on a background
/// thread, so the next launch can show them at once. Failures are logged, never shown.
fn persist(index: Arc<Index>, view: Folder) {
    let at = view.scanned_at;
    let spawned = std::thread::Builder::new().name("pulse-save".into()).spawn(move || {
        low_priority();
        if at < SAVED_AT.load(Ordering::SeqCst) {
            return;
        }
        let result = cache::save(INDEX_FILE, FORMAT, &saved_index(&index))
            .and_then(|()| cache::save(VIEW_FILE, FORMAT, &view))
            .and_then(|()| save_names(&index));
        match result {
            Ok(()) => SAVED_AT.store(at, Ordering::SeqCst),
            Err(error) => log(&format!("saving the storage scan failed: {error}")),
        }
    });
    if let Err(error) = spawned {
        log(&format!("saving the storage scan failed: {error}"));
    }
}

/// The folder index in memory: the newest scan, else the saved one, read from
/// disk on first use. `None` when there is neither.
fn restore() -> Option<Arc<Index>> {
    if let Some(index) = held() {
        return Some(index);
    }
    let _loading = lock(&LOADING);
    if let Some(index) = held() {
        return Some(index);
    }
    let saved: SavedIndex = cache::load(INDEX_FILE, FORMAT)?;
    let mut index = restored_index(saved);
    index.names = cache::read_bytes(NAMES_FILE).and_then(|bytes| NameIndex::from_bytes(&bytes));
    let index = Arc::new(index);
    // A scan that finished while this was loading is newer and wins.
    let mut slot = lock(&INDEX);
    let shown = slot.get_or_insert(index).clone();
    Some(shown)
}

/// Resident memory of this process in MB, from `ps` (logging only).
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
    let dir = home().join("Library/Application Support/Pulse");
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
    let mut built = build(&report, now(), false);
    let (kept, folders) = (report.entries.len(), report.folders.len());
    drop(report);
    let mut names = names;
    names.shrink_to_fit();
    let entries = names.live_rows();
    built.names = Some(names);
    let index = Arc::new(built);
    let result = folder(&index, &index.root);
    let epoch = {
        // Only the newest scan is shown.
        let mut job = lock(&JOB);
        if !job.as_ref().is_some_and(|j| j.id == id) {
            return Err("cancelled".into());
        }
        *job = None;
        *lock(&INDEX) = Some(index.clone());
        INDEX_EPOCH.fetch_add(1, Ordering::SeqCst) + 1
    };
    if let Ok(view) = &result {
        persist(index.clone(), view.clone());
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
}

#[tauri::command]
pub fn scan_status() -> Status {
    let running_root = lock(&JOB).as_ref().map(|j| j.root.clone());
    let index = held();
    Status {
        running: running_root.is_some(),
        running_root,
        has_index: index.is_some(),
        root: index.as_ref().map(|i| i.root.clone()),
        scanned_at: index.as_ref().map(|i| i.scanned_at),
        from_snapshot: index.as_ref().is_some_and(|i| i.from_snapshot),
    }
}

/// The newest folder view: the one held in memory, else the root view saved
/// with the last scan, shown at once. `None` when there is neither. Never scans.
#[tauri::command]
pub async fn last_scan() -> Result<Option<Folder>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        if let Some(index) = held() {
            return folder(&index, &index.root).map(Some);
        }
        if let Some(mut view) = cache::load::<Folder>(VIEW_FILE, FORMAT) {
            view.from_snapshot = true;
            // Load the full index behind the view, so drilling down does not wait for it.
            let _ = std::thread::Builder::new().name("pulse-index".into()).spawn(|| {
                low_priority();
                let _ = restore();
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
        let index = restore().ok_or("scan first")?;
        let limit = limit.unwrap_or(100).clamp(1, 1000);
        let needle = query.trim().to_lowercase();
        let exts: Vec<String> = extensions
            .unwrap_or_default()
            .iter()
            .map(|ext| ext.trim().trim_start_matches('.').to_lowercase())
            .filter(|ext| !ext.is_empty())
            .collect();
        if needle.is_empty() && exts.is_empty() {
            return Ok(Vec::new());
        }
        Ok(match index.names.as_ref() {
            Some(names) => search_names(names, &needle, &exts, limit),
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

/// What a live refresh of one folder did.
pub(crate) enum Refresh {
    /// The folder's subtree was read again and the index was updated.
    Applied,
    /// The subtree is larger than `LIVE_MAX_ENTRIES`.
    TooLarge,
    /// Nothing was done: a scan is running, or the index was replaced.
    Skipped,
}

/// The indexed folder that holds `path`: the folder itself, else its nearest
/// indexed parent. `None` when `path` is outside the scan.
pub(crate) fn indexed_folder(path: &Path) -> Option<PathBuf> {
    let index = held()?;
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
pub(crate) fn may_refresh(epoch: u64) -> bool {
    same_index(epoch) && lock(&JOB).is_none()
}

/// Read `folder`'s subtree again and update the index in place. Runs after
/// any cleanup scan in progress, never alongside a storage scan.
pub(crate) fn refresh_subtree(folder: &Path, epoch: u64) -> Refresh {
    if !may_refresh(epoch) {
        return Refresh::Skipped;
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
    if !same_index(epoch) {
        return Refresh::Skipped;
    }
    let Some(shared) = slot.as_mut() else {
        return Refresh::Skipped;
    };
    apply_subtree(Arc::make_mut(shared), folder, fresh, &names);
    DIRTY.store(true, Ordering::SeqCst);
    Refresh::Applied
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
fn apply_subtree(index: &mut Index, folder: &Path, mut fresh: HashMap<PathBuf, Node>, names: &NameIndex) {
    let exists = fresh.contains_key(folder);
    let new_total = fresh.get(folder).map_or(0, |node| node.total);
    let old_total = index.nodes.get(folder).map_or(0, |node| node.total);
    let delta = i128::from(new_total) - i128::from(old_total);

    // Name rows first: they are found by path in the rows as they were.
    let rows_ok = match index.names.as_mut() {
        None => true,
        Some(rows) => match rows.find_dir(folder) {
            Some(row) if exists => rows.replace_subtree(row, names),
            Some(row) => {
                rows.remove_subtree(row);
                rows.compact_if_sparse();
                true
            }
            None => false,
        },
    };
    if !rows_ok {
        index.names = None;
    }

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
    if let Ok(view) = folder(&index, &index.root) {
        persist(index, view);
    }
}
