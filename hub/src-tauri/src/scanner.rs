//! Storage scans for the hub. At most one scan runs at a time; starting
//! another cancels the running one. The scan runs on its own thread at
//! utility QoS so it stays out of the way of the UI and other apps.
//!
//! The finished scan is not kept as a `ScanReport` (millions of entries). It
//! is folded into an `Index`: per folder, its size plus only its largest
//! children (`TOP` of them), which is all the browser ever shows. Files below
//! `MIN_KEPT_FILE_BYTES`, and any beyond `TOP` per folder, are summed into one
//! "Smaller files" row so a folder's rows still add up to its size.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use pulse_core::{EntryKind, ScanOptions, ScanReport};
use serde::{Deserialize, Serialize};

use crate::{cache, growth, home};

/// Format of the two saved files (`storage-index-v1.json` and
/// `storage-view-v1.json`). A different version is ignored, never migrated.
const FORMAT: u32 = 1;
const INDEX_FILE: &str = "storage-index-v1.json";
const VIEW_FILE: &str = "storage-view-v1.json";

/// Newest scan time saved so far; an older scan finishing later never overwrites it.
static SAVED_AT: AtomicU64 = AtomicU64::new(0);
/// Serialises loading the saved index, so concurrent requests read it once.
static LOADING: Mutex<()> = Mutex::new(());

/// Largest children kept per folder.
const TOP: usize = 200;
/// Files smaller than this are only counted in their folder's total.
const MIN_KEPT_FILE_BYTES: u64 = 256 * 1024;
const MAX_ENTRIES: usize = 2_000_000;

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

struct Item {
    name: Box<str>,
    is_dir: bool,
    summary: bool,
    bytes: u64,
}

#[derive(Default)]
struct Node {
    total: u64,
    dirs: u64,
    kept: u64,
    children: usize,
    items: Vec<Item>,
}

struct Index {
    root: PathBuf,
    root_label: String,
    scanned_at: u64,
    from_snapshot: bool,
    incomplete: bool,
    needs_access: bool,
    limited: bool,
    nodes: HashMap<PathBuf, Node>,
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

/// Fold a report into the browsable index. `files_label` names the row that
/// holds bytes not itemised (small files, or all files for a saved scan).
fn build(report: &ScanReport, scanned_at: u64, from_snapshot: bool) -> Index {
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
    let label = if from_snapshot { "Files in this folder" } else { "Smaller files" };
    for node in nodes.values_mut() {
        let other = node.total.saturating_sub(node.dirs.saturating_add(node.kept));
        node.children = node.items.len();
        if other > 0 {
            node.items.push(Item { name: label.into(), is_dir: false, summary: true, bytes: other });
        }
        node.items.sort_by(|a, b| b.bytes.cmp(&a.bytes));
        node.items.truncate(TOP);
        node.items.shrink_to_fit();
    }
    Index {
        root_label: root_label(&root),
        root,
        scanned_at,
        from_snapshot,
        incomplete: report.accounting.incomplete && material(report),
        needs_access: needs_access(report),
        limited: limited(report),
        nodes,
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
    }
}

/// Write the folder index and the root view on a background thread, so the
/// next launch can show them at once. Failures are logged, never shown.
fn persist(index: Arc<Index>, view: Folder) {
    let at = view.scanned_at;
    let spawned = std::thread::Builder::new().name("pulse-save".into()).spawn(move || {
        low_priority();
        if at < SAVED_AT.load(Ordering::SeqCst) {
            return;
        }
        let result = cache::save(INDEX_FILE, FORMAT, &saved_index(&index))
            .and_then(|()| cache::save(VIEW_FILE, FORMAT, &view));
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
    let index = Arc::new(restored_index(saved));
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
    if std::env::var_os("PULSE_SCAN_LOG").or_else(|| std::env::var_os("COCKPIT_SCAN_LOG")).is_some() {
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

fn run_scan(root: PathBuf, id: u64, cancel: Arc<AtomicBool>) -> Result<Folder, String> {
    low_priority();
    let _run = exclusive();
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    let options = ScanOptions {
        max_entries: MAX_ENTRIES,
        keep_files_per_folder: Some(TOP),
        min_kept_file_bytes: MIN_KEPT_FILE_BYTES,
        cancel: Some(cancel.clone()),
        ..ScanOptions::default()
    };
    let started = Instant::now();
    let report = pulse_core::scan(&[root.clone()], &options);
    let scan_ms = started.elapsed().as_millis();
    if cancel.load(Ordering::Relaxed) {
        log(&format!("scan {} cancelled after {scan_ms} ms", root.display()));
        return Err("cancelled".into());
    }
    if root == home() {
        growth::save_in_background(&report);
    }
    let indexed = Instant::now();
    let index = Arc::new(build(&report, now(), false));
    let (kept, folders) = (report.entries.len(), report.folders.len());
    drop(report);
    let result = folder(&index, &index.root);
    {
        // Only the newest scan is shown.
        let mut job = lock(&JOB);
        if job.as_ref().is_some_and(|j| j.id == id) {
            *job = None;
            *lock(&INDEX) = Some(index.clone());
        } else {
            return Err("cancelled".into());
        }
    }
    if let Ok(view) = &result {
        persist(index.clone(), view.clone());
    }
    log(&format!(
        "scan {} done: {kept} entries kept, {folders} folders, scan {scan_ms} ms, index {} ms, rss {} MB, {} index folders",
        root.display(),
        indexed.elapsed().as_millis(),
        rss_mb().map_or_else(|| "?".to_string(), |m| m.to_string()),
        index.nodes.len(),
    ));
    result
}

/// Scan `path` (default: home), replacing any scan in progress, and return its
/// top level. Fails with "cancelled" when a newer scan took over.
#[tauri::command]
pub async fn scan(path: Option<String>) -> Result<Folder, String> {
    let root = path.map(PathBuf::from).unwrap_or_else(home);
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
            .spawn(move || run_scan(root, id, cancel))
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

#[tauri::command]
pub async fn search(query: String) -> Result<Vec<Row>, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<Vec<Row>, String> {
        let index = restore().ok_or("scan first")?;
        let needle = query.to_lowercase();
        let mut found: Vec<Row> = Vec::new();
        for (parent, node) in &index.nodes {
            for item in node.items.iter().filter(|i| !i.summary) {
                if item.name.to_lowercase().contains(&needle) {
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
        found.truncate(100);
        Ok(found)
    })
    .await
    .map_err(|e| e.to_string())?
}
