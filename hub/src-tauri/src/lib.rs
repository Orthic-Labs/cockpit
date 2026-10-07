//! Cockpit hub: a small window over the Rust core. Storage and monitor
//! commands only read. The only commands that change anything are in `apps`:
//! uninstall (move to Trash) and process Quit / Force Quit.

mod apps;

mod cleanup;

mod growth;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use cockpit_core::storage_browser::{self, SearchRequest};
use cockpit_core::{EntryKind, ScanOptions, ScanReport};
use serde::Serialize;
use tauri::{Emitter, Manager, State};

// The notch's settings bridge (see mac/Notch/Sources/System/HubBridge.swift).
// Darwin notifications through libSystem; no payloads.
unsafe extern "C" {
    fn notify_post(name: *const std::ffi::c_char) -> u32;
    fn notify_register_check(name: *const std::ffi::c_char, token: *mut i32) -> u32;
    fn notify_check(token: i32, changed: *mut i32) -> u32;
}

fn bridge_dir() -> PathBuf {
    home().join("Library/Application Support/Cockpit")
}

fn post(name: &str) {
    if let Ok(name) = std::ffi::CString::new(name) {
        unsafe { notify_post(name.as_ptr()) };
    }
}

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
    reasons: Vec<String>,
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// Children of `path` from the held scan, largest first. Folder sizes are the
/// scan's per-folder totals; file sizes are their attributed allocation.
fn folder(report: &ScanReport, path: &PathBuf) -> Result<Folder, String> {
    // The browser pages at most 1,000 children per request.
    let mut items = Vec::new();
    let mut offset = 0;
    let (total_children, incomplete) = loop {
        let page = storage_browser::drilldown_children(report, path, offset, 1_000)
            .map_err(|e| e.to_string())?;
        items.extend(page.items);
        offset += 1_000;
        if !page.has_more || offset >= 20_000 {
            break (page.total_children, page.incomplete);
        }
    };
    let totals: HashMap<&PathBuf, u64> = report
        .folders
        .iter()
        .map(|f| (&f.path, f.attributed_allocation_bytes))
        .collect();
    let mut rows: Vec<Row> = items
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
        total_children,
        incomplete: incomplete && !material(report).is_empty(),
        reasons: material(report).into_iter().take(3).collect(),
    })
}

/// Reasons that make folder totals low: limits, budgets and unreadable
/// folders. Single entries without measurable size (sockets, special files)
/// do not change the totals and are not reported as a partial scan.
fn material(report: &ScanReport) -> Vec<String> {
    report
        .incomplete_reasons
        .iter()
        .filter(|r| {
            let r = r.to_lowercase();
            ["limit", "budget", "not inspectable", "denied", "not permitted"]
                .iter()
                .any(|k| r.contains(k))
        })
        .map(|r| r.replace(&home().display().to_string(), "~"))
        .collect()
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
        let options = ScanOptions { max_entries: 1_000_000, ..ScanOptions::default() };
        cockpit_core::scan(&[scan_root], &options)
    })
    .await
    .map_err(|e| e.to_string())?;
    if root == home() {
        growth::save_in_background(&report);
    }
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

/// The notch's last published settings and accounts.
#[tauri::command]
fn notch_state() -> Result<serde_json::Value, String> {
    let text = std::fs::read_to_string(bridge_dir().join("notch-state.json"))
        .map_err(|_| "The Cockpit notch isn't running.".to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

/// Ask the notch to change a setting or run an account action. The notch is
/// the only writer of its preferences; this only leaves it a request.
#[tauri::command]
fn notch_command(command: serde_json::Value) -> Result<(), String> {
    let dir = bridge_dir().join("hub-commands");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let temp = dir.join(format!("{stamp}.tmp"));
    std::fs::write(&temp, serde_json::to_vec(&command).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    std::fs::rename(&temp, dir.join(format!("{stamp}.json"))).map_err(|e| e.to_string())?;
    post("dev.orthic.cockpit.hub.command");
    Ok(())
}

/// The section the hub was opened for (`--section <name>`), if any.
#[tauri::command]
fn initial_section() -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == "--section").and_then(|i| args.get(i + 1).cloned())
}

/// Watch for the notch asking a running hub to show a section, and for new
/// notch state; forward both to the page as events.
fn watch_notch(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        let names = [
            ("dev.orthic.cockpit.hub.show.settings", "show-section", "settings"),
            ("dev.orthic.cockpit.hub.show.storage", "show-section", "storage"),
            ("dev.orthic.cockpit.hub.show.monitor", "show-section", "monitor"),
            ("dev.orthic.cockpit.hub.show.accounts", "show-section", "accounts"),
            ("dev.orthic.cockpit.hub.show.appearance", "show-section", "appearance"),
            ("dev.orthic.cockpit.hub.show.notifications", "show-section", "notifications"),
            ("dev.orthic.cockpit.hub.show.general", "show-section", "general"),
            ("dev.orthic.cockpit.notch.state", "notch-state", ""),
        ];
        let mut tokens = Vec::new();
        for (name, event, payload) in names {
            let Ok(cname) = std::ffi::CString::new(name) else { continue };
            let mut token = 0i32;
            if unsafe { notify_register_check(cname.as_ptr(), &mut token) } == 0 {
                // The first check after registering always reports a change;
                // consume it so only real posts reach the page.
                let mut changed = 0i32;
                unsafe { notify_check(token, &mut changed) };
                tokens.push((token, event, payload));
            }
        }
        // A section passed at launch (`--section`) is also sent once the page
        // has had time to load and subscribe.
        if let Some(section) = initial_section() {
            std::thread::sleep(std::time::Duration::from_millis(900));
            let _ = app.emit("show-section", section);
        }
        loop {
            std::thread::sleep(std::time::Duration::from_millis(250));
            for (token, event, payload) in &tokens {
                let mut changed = 0i32;
                if unsafe { notify_check(*token, &mut changed) } == 0 && changed != 0 {
                    if *event == "show-section" {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    let _ = app.emit(event, payload.to_string());
                }
            }
        }
    });
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
        // RightKit's shell asks the OS plugin for the platform (traffic-light room on macOS).
        .plugin(tauri_plugin_os::init())
        .manage(Hub::default())
        .setup(|app| {
            // No Dock icon: Cockpit lives in the notch.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            // An accessory app is not brought forward on launch; do it here.
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_focus();
            }
            watch_notch(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            status, processes, apps::apps_list, apps::app_detail, apps::app_uninstall,
            apps::process_rows, apps::process_quit, apps::process_force_quit, scan, growth::growth, children, search, reveal, notch_state, notch_command, initial_section,
            cleanup::cleanup_scan, cleanup::cleanup_apply, cleanup::cleanup_history, cleanup::cleanup_restore
        ])
        .run(tauri::generate_context!())
        .expect("error while running Cockpit hub");
}
