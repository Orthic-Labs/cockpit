//! Full Disk Access probe for the hub process, plus cleanup of stale TCC grants.
//! The notch launches the hub as its child, so the notch's Full Disk Access is
//! the one Pulse entry; the probe only confirms the hub inherited it. It opens
//! and reads one byte of a protected file; it never prompts and never keeps the
//! contents. TCC.db is only ever read (sqlite3 -readonly); grants are removed
//! with `tccutil reset`, never by writing the database.

use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};

const PANE: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// "granted" when a protected file reads, "needsApproval" when macOS refuses
/// it, "unknown" when no probe file exists.
fn probe(home: &Path) -> &'static str {
    for rel in [
        "Library/Application Support/com.apple.TCC/TCC.db",
        "Library/Safari/Bookmarks.plist",
    ] {
        match std::fs::File::open(home.join(rel)) {
            Ok(mut file) => {
                let mut byte = [0u8; 1];
                match file.read(&mut byte) {
                    Ok(_) => return "granted",
                    Err(e) if e.kind() == ErrorKind::PermissionDenied => return "needsApproval",
                    Err(_) => continue,
                }
            }
            Err(e) if e.kind() == ErrorKind::PermissionDenied => return "needsApproval",
            Err(_) => continue,
        }
    }
    "unknown"
}

/// The hub's own Full Disk Access state: "granted", "needsApproval" or "unknown".
#[tauri::command]
pub async fn fda_status() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(|| probe(&home()).to_string())
        .await
        .map_err(|e| e.to_string())
}

/// Open System Settings at Full Disk Access. Changes no permission.
#[tauri::command]
pub fn fda_request() -> Result<(), String> {
    std::process::Command::new("/usr/bin/open")
        .arg(PANE)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Clean up grants that belong to apps that no longer exist.
// ---------------------------------------------------------------------------

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::process::Command;

const USER_DB: &str = "Library/Application Support/com.apple.TCC/TCC.db";
const SYSTEM_DB: &str = "/Library/Application Support/com.apple.TCC/TCC.db";

#[derive(Serialize, Clone)]
pub struct StaleService {
    /// Short name `tccutil` takes, e.g. "Accessibility".
    pub service: String,
    pub label: String,
}

#[derive(Serialize, Clone)]
pub struct StaleApp {
    /// Bundle id, or the path for path-based grants.
    pub id: String,
    /// "bundle" can be reset with tccutil; "path" must be removed in System Settings.
    pub kind: String,
    pub services: Vec<StaleService>,
}

#[derive(Deserialize)]
pub struct ResetItem {
    pub id: String,
    pub services: Vec<String>,
}

#[derive(Serialize)]
pub struct ResetOutcome {
    pub id: String,
    pub service: String,
    pub ok: bool,
    pub message: String,
}

#[derive(Deserialize)]
struct TccRow {
    service: String,
    client: String,
    client_type: i64,
}

fn short_name(service: &str) -> String {
    service.strip_prefix("kTCCService").unwrap_or(service).to_string()
}

fn friendly(service: &str) -> String {
    match short_name(service).as_str() {
        "Accessibility" => "Accessibility".into(),
        "SystemPolicyAllFiles" => "Full Disk Access".into(),
        "ListenEvent" => "Input Monitoring".into(),
        "PostEvent" => "Send keystrokes".into(),
        "ScreenCapture" => "Screen Recording".into(),
        "AppleEvents" => "Automation".into(),
        "Camera" => "Camera".into(),
        "Microphone" => "Microphone".into(),
        "Photos" | "PhotosAdd" => "Photos".into(),
        "Calendar" => "Calendars".into(),
        "Reminders" => "Reminders".into(),
        "AddressBook" => "Contacts".into(),
        "SystemPolicyDesktopFolder" => "Desktop folder".into(),
        "SystemPolicyDocumentsFolder" => "Documents folder".into(),
        "SystemPolicyDownloadsFolder" => "Downloads folder".into(),
        "SystemPolicyRemovableVolumes" => "Removable volumes".into(),
        "SystemPolicyNetworkVolumes" => "Network volumes".into(),
        "SystemPolicyAppBundles" => "App data".into(),
        "Willow" => "Home Screen widgets".into(),
        other => other.to_string(),
    }
}

/// Read-only `SELECT` through the sqlite3 CLI. Empty when the database cannot
/// be opened (no Full Disk Access) or does not exist.
fn read_db(path: &str) -> Vec<TccRow> {
    let out = Command::new("/usr/bin/sqlite3")
        .args(["-readonly", "-json", path, "SELECT service, client, client_type FROM access"])
        .output();
    match out {
        Ok(o) if o.status.success() && !o.stdout.is_empty() => {
            serde_json::from_slice(&o.stdout).unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

/// Never offered for cleanup: Apple's own and Pulse's own entries.
fn protected(client: &str) -> bool {
    client.starts_with("com.apple.")
        || client == "dev.orthic.pulse"
        || client.starts_with("dev.orthic.pulse.")
        || client.contains("/Pulse.app")
}

/// True unless Spotlight positively reports no app with this bundle id and no
/// app in the usual folders carries it. Any doubt counts as installed.
fn bundle_installed(id: &str, folder_ids: &mut Option<Vec<String>>) -> bool {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_') {
        return true;
    }
    let found = Command::new("/usr/bin/mdfind")
        .arg(format!("kMDItemCFBundleIdentifier == '{id}'"))
        .output();
    match found {
        Ok(o) if o.status.success() => {
            if !o.stdout.iter().all(|b| b.is_ascii_whitespace()) {
                return true;
            }
        }
        _ => return true,
    }
    let ids = folder_ids.get_or_insert_with(|| {
        let mut dirs = vec![PathBuf::from("/Applications"), home().join("Applications")];
        let mut ids = Vec::new();
        while let Some(dir) = dirs.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "app") {
                    let plist = path.join("Contents/Info.plist");
                    if let Ok(o) = Command::new("/usr/bin/plutil")
                        .args(["-extract", "CFBundleIdentifier", "raw", "-o", "-"])
                        .arg(&plist)
                        .output()
                    {
                        if o.status.success() {
                            ids.push(String::from_utf8_lossy(&o.stdout).trim().to_string());
                        }
                    }
                } else if path.is_dir() && dir.parent().is_some_and(|p| p == Path::new("/") || p == home()) {
                    dirs.push(path);
                }
            }
        }
        ids
    });
    ids.iter().any(|i| i == id)
}

fn stale_grants() -> Vec<StaleApp> {
    let mut rows = read_db(&home().join(USER_DB).to_string_lossy());
    rows.extend(read_db(SYSTEM_DB));
    let mut folder_ids: Option<Vec<String>> = None;
    let mut verdict: BTreeMap<(String, i64), bool> = BTreeMap::new();
    let mut grouped: BTreeMap<(String, i64), Vec<StaleService>> = BTreeMap::new();
    for row in rows {
        if protected(&row.client) || !matches!(row.client_type, 0 | 1) {
            continue;
        }
        let key = (row.client.clone(), row.client_type);
        let stale = *verdict.entry(key.clone()).or_insert_with(|| {
            if row.client_type == 0 {
                !bundle_installed(&row.client, &mut folder_ids)
            } else {
                row.client.starts_with('/') && !Path::new(&row.client).exists()
            }
        });
        if !stale {
            continue;
        }
        let services = grouped.entry(key).or_default();
        let name = short_name(&row.service);
        if !services.iter().any(|s| s.service == name) {
            services.push(StaleService { label: friendly(&row.service), service: name });
        }
    }
    grouped
        .into_iter()
        .map(|((id, kind), services)| StaleApp {
            id,
            kind: if kind == 0 { "bundle".into() } else { "path".into() },
            services,
        })
        .collect()
}

/// Grants held by apps that are no longer installed. Read-only; needs Full
/// Disk Access to read the databases (an unreadable database lists nothing).
#[tauri::command]
pub async fn tcc_stale_scan() -> Result<Vec<StaleApp>, String> {
    tauri::async_runtime::spawn_blocking(stale_grants)
        .await
        .map_err(|e| e.to_string())
}

/// `tccutil reset <service> <bundle-id>` for each selected grant that is still
/// stale. Never writes TCC.db; never touches Apple's or Pulse's entries.
#[tauri::command]
pub async fn tcc_reset(items: Vec<ResetItem>) -> Result<Vec<ResetOutcome>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let stale = stale_grants();
        let mut outcomes = Vec::new();
        for item in items {
            for service in item.services {
                let allowed = stale.iter().any(|app| {
                    app.kind == "bundle"
                        && app.id == item.id
                        && app.services.iter().any(|s| s.service == service)
                });
                let outcome = if !allowed {
                    (false, "Not a removed app's grant any more; skipped.".to_string())
                } else {
                    match Command::new("/usr/bin/tccutil").args(["reset", &service, &item.id]).output() {
                        Ok(o) if o.status.success() => (true, "Cleared".to_string()),
                        Ok(o) => {
                            let text = String::from_utf8_lossy(&o.stderr).trim().to_string();
                            (false, if text.is_empty() { "tccutil failed".into() } else { text })
                        }
                        Err(e) => (false, e.to_string()),
                    }
                };
                outcomes.push(ResetOutcome {
                    id: item.id.clone(),
                    service,
                    ok: outcome.0,
                    message: outcome.1,
                });
            }
        }
        outcomes
    })
    .await
    .map_err(|e| e.to_string())
}
