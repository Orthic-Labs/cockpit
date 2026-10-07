//! Cockpit hub: a small window over the read-only Rust core. Every command
//! reads; nothing here deletes, moves or changes a file.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use cockpit_core::storage_browser::{self, SearchRequest};
use cockpit_core::{EntryKind, ScanOptions, ScanReport};
use serde::Serialize;
use tauri::State;

#[derive(Default)]
struct Hub {
    report: Mutex<Option<ScanReport>>,
}

#[derive(Serialize)]
struct Row {
    path: PathBuf,
    name: String,
    is_dir: bool,
    bytes: u64,
}

#[derive(Serialize)]
struct Folder {
    path: PathBuf,
    root: PathBuf,
    rows: Vec<Row>,
    total_children: usize,
    incomplete: bool,
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// Children of `path` from the held scan, largest first. Folder sizes are the
/// scan's per-folder totals; file sizes are their attributed allocation.
fn folder(report: &ScanReport, path: &PathBuf) -> Result<Folder, String> {
    let page = storage_browser::drilldown_children(report, path, 0, 10_000)
        .map_err(|e| e.to_string())?;
    let totals: HashMap<&PathBuf, u64> = report
        .folders
        .iter()
        .map(|f| (&f.path, f.attributed_allocation_bytes))
        .collect();
    let mut rows: Vec<Row> = page
        .items
        .into_iter()
        .map(|item| {
            let is_dir = item.kind == EntryKind::Directory;
            let bytes = if is_dir {
                totals.get(&item.path).copied().unwrap_or(item.attributed_allocation_size)
            } else {
                item.attributed_allocation_size
            };
            Row { path: item.path, name: item.name, is_dir, bytes }
        })
        .collect();
    rows.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    rows.truncate(200);
    Ok(Folder {
        path: path.clone(),
        root: report.roots.first().cloned().unwrap_or_default(),
        rows,
        total_children: page.total_children,
        incomplete: page.incomplete,
    })
}

#[tauri::command]
async fn status() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(|| {
        serde_json::to_value(cockpit_core::system_status()).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn processes() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let mut list = cockpit_core::procs();
        list.sort_by(|a, b| {
            b.memory.value.unwrap_or(0).cmp(&a.memory.value.unwrap_or(0))
        });
        list.truncate(12);
        serde_json::to_value(list).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Scan `path` (default: home), keep the report, and return its top level.
#[tauri::command]
async fn scan(path: Option<String>, hub: State<'_, Hub>) -> Result<Folder, String> {
    let root = path.map(PathBuf::from).unwrap_or_else(home);
    let scan_root = root.clone();
    let report = tauri::async_runtime::spawn_blocking(move || {
        let options = ScanOptions { max_entries: 400_000, ..ScanOptions::default() };
        cockpit_core::scan(&[scan_root], &options)
    })
    .await
    .map_err(|e| e.to_string())?;
    let result = folder(&report, &root);
    *hub.report.lock().map_err(|e| e.to_string())? = Some(report);
    result
}

#[tauri::command]
fn children(path: String, hub: State<'_, Hub>) -> Result<Folder, String> {
    let guard = hub.report.lock().map_err(|e| e.to_string())?;
    let report = guard.as_ref().ok_or("scan first")?;
    folder(report, &PathBuf::from(path))
}

#[tauri::command]
fn search(query: String, hub: State<'_, Hub>) -> Result<Vec<Row>, String> {
    let guard = hub.report.lock().map_err(|e| e.to_string())?;
    let report = guard.as_ref().ok_or("scan first")?;
    let request = SearchRequest { query, limit: 100, ..SearchRequest::default() };
    let page = storage_browser::search(report, &request).map_err(|e| e.to_string())?;
    Ok(page
        .items
        .into_iter()
        .map(|item| Row {
            is_dir: item.kind == EntryKind::Directory,
            bytes: item.attributed_allocation_size,
            name: item.name,
            path: item.path,
        })
        .collect())
}

/// Show a file or folder in Finder. Read-only: it only opens a window.
#[tauri::command]
fn reveal(path: String) -> Result<(), String> {
    std::process::Command::new("/usr/bin/open")
        .arg("-R")
        .arg(&path)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

pub fn run() {
    tauri::Builder::default()
        .manage(Hub::default())
        .setup(|app| {
            // No Dock icon: Cockpit lives in the notch.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            // An accessory app is not brought forward on launch; do it here.
            if let Some(window) = tauri::Manager::get_webview_window(app, "main") {
                let _ = window.set_focus();
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![status, processes, scan, children, search, reveal])
        .run(tauri::generate_context!())
        .expect("error while running Cockpit hub");
}
