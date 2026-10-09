//! Apps and process commands on Windows. The page and the command names are the
//! macOS ones (see `apps.rs`); this module answers them from what Windows keeps.
//!
//! Inventory: the Uninstall registry keys (HKLM 64-bit and 32-bit views, HKCU).
//! Uninstall: starts the app's own registered uninstaller, which asks for its own
//! confirmation and elevation; nothing is moved to the Recycle Bin here.
//! Processes: grouped by executable name from the core's process list; Quit sends
//! the graceful close request (`taskkill`), Force Quit ends the process tree.
//!
//! Not available yet, and reported plainly rather than faked: leftover scanning,
//! app icons, update checks, "last used" and running state of an app.

use std::collections::{HashMap, HashSet};
use std::os::windows::process::CommandExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pulse_core::ProcessIdentity;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY};
use winreg::RegKey;

use crate::cache;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const UNINSTALL: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";
const INVENTORY_FILE: &str = "apps-inventory-windows-v1.json";
const INVENTORY_FORMAT: u32 = 1;

async fn blocking<T, F>(work: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(work).await.map_err(|e| e.to_string())?
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Clears a background job's busy flag when the job ends, even if it panics.
struct Busy(&'static AtomicBool);

impl Drop for Busy {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// Inventory
// ---------------------------------------------------------------------------

/// The same fields the macOS page reads. `bundle_id`, `last_used` and `running`
/// have no Windows source here, so they stay empty/false.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppEntry {
    pub name: String,
    /// The install folder when it is known and unique; otherwise the registry key.
    pub path: String,
    pub bundle_id: Option<String>,
    pub version: Option<String>,
    /// `EstimatedSize` from the registry; 0 when the installer did not record one.
    pub size_bytes: u64,
    pub last_used: Option<i64>,
    pub running: bool,
    /// Why uninstall is not offered, if it is not.
    pub protected: Option<String>,
}

struct Installed {
    entry: AppEntry,
    uninstall: Option<String>,
}

fn read_value(key: &RegKey, name: &str) -> Option<String> {
    key.get_value::<String, _>(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn read_flag(key: &RegKey, name: &str) -> bool {
    key.get_value::<u32, _>(name).map(|v| v != 0).unwrap_or(false)
}

fn install_folder(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_matches('"').trim_end_matches(['\\', '/']);
    (!trimmed.is_empty() && std::path::Path::new(trimmed).is_dir()).then(|| trimmed.to_string())
}

/// Every app listed in the Uninstall keys, without updates and system components.
fn read_installed() -> Vec<Installed> {
    let views = [
        ("HKLM", RegKey::predef(HKEY_LOCAL_MACHINE), KEY_WOW64_64KEY),
        ("HKLM32", RegKey::predef(HKEY_LOCAL_MACHINE), KEY_WOW64_32KEY),
        ("HKCU", RegKey::predef(HKEY_CURRENT_USER), KEY_WOW64_64KEY),
    ];
    let mut found: Vec<(Installed, Option<String>)> = Vec::new();
    let mut seen: HashSet<(String, Option<String>)> = HashSet::new();
    for (label, hive, view) in views {
        let Ok(root) = hive.open_subkey_with_flags(UNINSTALL, KEY_READ | view) else { continue };
        for name in root.enum_keys().flatten() {
            let Ok(key) = root.open_subkey_with_flags(&name, KEY_READ | view) else { continue };
            let Some(display) = read_value(&key, "DisplayName") else { continue };
            // Updates, hotfixes and parts of other products are not apps.
            if read_flag(&key, "SystemComponent")
                || read_value(&key, "ParentKeyName").is_some()
                || read_value(&key, "ParentDisplayName").is_some()
                || matches!(
                    read_value(&key, "ReleaseType").as_deref(),
                    Some("Update" | "Hotfix" | "Security Update" | "Update Rollup" | "ServicePack")
                )
            {
                continue;
            }
            let version = read_value(&key, "DisplayVersion");
            if !seen.insert((display.to_lowercase(), version.clone())) {
                continue;
            }
            let uninstall = read_value(&key, "UninstallString");
            let protected = if read_flag(&key, "NoRemove") {
                Some("Windows does not allow this app to be removed.".to_string())
            } else if uninstall.is_none() {
                Some("This app registered no uninstaller.".to_string())
            } else {
                None
            };
            let size_kb = key.get_value::<u32, _>("EstimatedSize").unwrap_or(0);
            found.push((
                Installed {
                    entry: AppEntry {
                        name: display,
                        path: format!(r"{label}\{UNINSTALL}\{name}"),
                        bundle_id: None,
                        version,
                        size_bytes: u64::from(size_kb) * 1024,
                        last_used: None,
                        running: false,
                        protected,
                    },
                    uninstall,
                },
                read_value(&key, "InstallLocation").and_then(|raw| install_folder(&raw)),
            ));
        }
    }
    // An install folder stands in for the registry key only when exactly one app has it.
    let mut counts: HashMap<String, usize> = HashMap::new();
    for (_, folder) in &found {
        if let Some(folder) = folder {
            *counts.entry(folder.to_lowercase()).or_default() += 1;
        }
    }
    let mut out: Vec<Installed> = found
        .into_iter()
        .map(|(mut app, folder)| {
            if let Some(folder) = folder {
                if counts.get(&folder.to_lowercase()) == Some(&1) {
                    app.entry.path = folder;
                }
            }
            app
        })
        .collect();
    out.sort_by_key(|app| app.entry.name.to_lowercase());
    out
}

fn find(path: &str) -> Result<Installed, String> {
    read_installed()
        .into_iter()
        .find(|app| app.entry.path.eq_ignore_ascii_case(path))
        .ok_or_else(|| "That app is no longer installed.".to_string())
}

#[derive(Serialize, Deserialize)]
struct SavedInventory {
    saved_at: i64,
    apps: Vec<AppEntry>,
}

#[derive(Serialize)]
pub struct CachedApps {
    /// Unix seconds when the saved inventory was written; None when there is none.
    saved_at: Option<i64>,
    apps: Vec<AppEntry>,
}

#[tauri::command]
pub async fn apps_list() -> Result<Vec<AppEntry>, String> {
    blocking(|| Ok(read_installed().into_iter().map(|app| app.entry).collect())).await
}

/// The saved inventory, returned at once so the list paints before the registry is read.
#[tauri::command]
pub async fn apps_cached() -> Result<CachedApps, String> {
    blocking(|| {
        Ok(match cache::load::<SavedInventory>(INVENTORY_FILE, INVENTORY_FORMAT) {
            Some(saved) => CachedApps { saved_at: Some(saved.saved_at), apps: saved.apps },
            None => CachedApps { saved_at: None, apps: Vec::new() },
        })
    })
    .await
}

static INVENTORY_BUSY: AtomicBool = AtomicBool::new(false);

/// Starts an inventory refresh in the background. Each app arrives as an
/// `apps-row` event; the full list follows as `apps-inventory`.
#[tauri::command]
pub fn apps_refresh(app: AppHandle) -> Result<(), String> {
    if INVENTORY_BUSY.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    std::thread::spawn(move || {
        let _busy = Busy(&INVENTORY_BUSY);
        let apps: Vec<AppEntry> = read_installed().into_iter().map(|installed| installed.entry).collect();
        for row in &apps {
            let _ = app.emit("apps-row", row);
        }
        let _ = cache::save(INVENTORY_FILE, INVENTORY_FORMAT, &SavedInventory { saved_at: now_secs(), apps: apps.clone() });
        let _ = app.emit("apps-inventory", &apps);
    });
    Ok(())
}

/// Header facts for one app opened by path.
#[tauri::command]
pub async fn app_summary(path: String) -> Result<AppEntry, String> {
    blocking(move || find(&path).map(|app| app.entry)).await
}

/// The macOS page's detail shape. Windows lists no related items yet.
#[derive(Serialize)]
pub struct AppDetail {
    app: AppEntry,
    items: Vec<serde_json::Value>,
    background: Vec<serde_json::Value>,
    receipts: Vec<String>,
}

#[tauri::command]
pub async fn app_detail(path: String) -> Result<AppDetail, String> {
    blocking(move || {
        Ok(AppDetail { app: find(&path)?.entry, items: Vec::new(), background: Vec::new(), receipts: Vec::new() })
    })
    .await
}

#[derive(Serialize)]
pub struct UninstallResult {
    moved: Vec<serde_json::Value>,
    failed: Vec<serde_json::Value>,
    moved_bytes: u64,
    activity_id: Option<String>,
}

/// Starts the app's own uninstaller (from its Uninstall key) and returns. The
/// uninstaller shows its own prompts and asks for administrator rights itself.
/// `items` (leftovers) is ignored: none are offered on Windows.
#[tauri::command]
pub async fn app_uninstall(
    path: String,
    bundle_id: Option<String>,
    items: Vec<String>,
) -> Result<UninstallResult, String> {
    let _ = (bundle_id, items);
    blocking(move || {
        let installed = find(&path)?;
        if let Some(reason) = installed.entry.protected {
            return Err(reason);
        }
        let command = installed.uninstall.ok_or_else(|| "This app registered no uninstaller.".to_string())?;
        // The registered string is a full command line; cmd runs it as the app's installer wrote it.
        std::process::Command::new("cmd.exe")
            .raw_arg(format!("/S /C \"{command}\""))
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| format!("Could not start the uninstaller: {e}"))?;
        Ok(UninstallResult { moved: Vec::new(), failed: Vec::new(), moved_bytes: 0, activity_id: None })
    })
    .await
}

#[derive(Serialize, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum LeftoversEvent {
    Done { path: String, error: Option<String> },
}

/// Leftover scanning is not built for Windows; the page is told so on `done`.
#[tauri::command]
pub fn app_leftovers(app: AppHandle, path: String) -> Result<(), String> {
    let _ = app.emit(
        "apps-leftovers",
        LeftoversEvent::Done {
            path,
            error: Some("Looking for leftover files is not available on Windows yet.".into()),
        },
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Icons and updates (not available yet)
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone)]
struct IconEvent {
    path: String,
    data_url: Option<String>,
}

/// No icons are read on Windows. Each path is answered with "no usable icon" so
/// the page settles on its neutral one.
#[tauri::command]
pub async fn app_icons(app: AppHandle, paths: Vec<String>) -> Result<HashMap<String, String>, String> {
    for path in paths.into_iter().take(500) {
        let _ = app.emit("apps-icon", IconEvent { path, data_url: None });
    }
    Ok(HashMap::new())
}

#[derive(Serialize, Clone)]
pub struct UpdateReport {
    checked_at: Option<i64>,
    apps: Vec<serde_json::Value>,
}

fn no_updates() -> UpdateReport {
    UpdateReport { checked_at: None, apps: Vec::new() }
}

#[tauri::command]
pub async fn apps_updates_cached() -> Result<UpdateReport, String> {
    Ok(no_updates())
}

/// Update checks are not built for Windows: finishes at once with an empty report.
#[tauri::command]
pub fn apps_updates_refresh(app: AppHandle, force: bool) -> Result<(), String> {
    let _ = force;
    let _ = app.emit("apps-updates-done", no_updates());
    Ok(())
}

#[tauri::command]
pub async fn app_update(app: AppHandle, path: String) -> Result<String, String> {
    let _ = (app, path);
    Err("Updating apps from Pulse is not available on Windows yet.".into())
}

// ---------------------------------------------------------------------------
// Processes
// ---------------------------------------------------------------------------

#[derive(Clone, Serialize)]
pub struct ProcessMember {
    identity: ProcessIdentity,
    name: String,
    cpu_usage_percent: f32,
    memory_bytes: u64,
}

#[derive(Clone, Serialize)]
pub struct ProcessRow {
    /// `name:<lower-case executable name>` for a group of processes.
    key: String,
    name: String,
    bundle_id: Option<String>,
    app_path: Option<String>,
    /// The main process of the group; identity for Quit.
    lead: ProcessIdentity,
    cpu_usage_percent: f32,
    memory_bytes: u64,
    members: Vec<ProcessMember>,
    can_act: bool,
    refusal: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuitOutcome {
    Quit,
    StillRunning,
}

/// Windows processes that Quit must never be offered for.
const SYSTEM_NAMES: [&str; 14] = [
    "system", "system idle process", "registry", "smss.exe", "csrss.exe", "wininit.exe", "services.exe",
    "lsass.exe", "winlogon.exe", "svchost.exe", "dwm.exe", "fontdrvhost.exe", "memory compression", "explorer.exe",
];

fn refusal_for(name: &str, pid: u32) -> Option<String> {
    let lower = name.to_lowercase();
    if pid <= 4 || SYSTEM_NAMES.contains(&lower.as_str()) {
        Some("A Windows system process.".into())
    } else if pid == std::process::id() || lower.starts_with("pulse") {
        Some("Part of Pulse.".into())
    } else {
        None
    }
}

fn grouped_rows() -> Vec<ProcessRow> {
    let mut groups: HashMap<String, Vec<pulse_core::ProcessInfo>> = HashMap::new();
    for process in pulse_core::procs() {
        groups.entry(process.name.to_lowercase()).or_default().push(process);
    }
    let mut rows: Vec<ProcessRow> = groups
        .into_iter()
        .map(|(lower, members)| {
            let pids: HashSet<u32> = members.iter().map(|m| m.identity.pid).collect();
            // The lead is a member whose parent is outside the group; the largest if several.
            let lead = members
                .iter()
                .filter(|m| m.parent_pid.is_none_or(|parent| !pids.contains(&parent)))
                .max_by_key(|m| m.memory.value.unwrap_or(0))
                .or_else(|| members.iter().max_by_key(|m| m.memory.value.unwrap_or(0)))
                .map(|m| (m.identity.clone(), m.name.clone()))
                .unwrap_or_else(|| (ProcessIdentity { pid: 0, start_time: 0 }, lower.clone()));
            let refusal = refusal_for(&lead.1, lead.0.pid);
            ProcessRow {
                key: format!("name:{lower}"),
                name: lead.1.clone(),
                bundle_id: None,
                app_path: None,
                lead: lead.0,
                cpu_usage_percent: members.iter().map(|m| m.cpu_usage_percent).sum(),
                memory_bytes: members.iter().map(|m| m.memory.value.unwrap_or(0)).sum(),
                members: members
                    .iter()
                    .map(|m| ProcessMember {
                        identity: m.identity.clone(),
                        name: m.name.clone(),
                        cpu_usage_percent: m.cpu_usage_percent,
                        memory_bytes: m.memory.value.unwrap_or(0),
                    })
                    .collect(),
                can_act: refusal.is_none(),
                refusal,
            }
        })
        .collect();
    rows.sort_by(|a, b| b.memory_bytes.cmp(&a.memory_bytes).then_with(|| a.name.cmp(&b.name)));
    rows
}

#[tauri::command]
pub async fn process_rows() -> Result<Vec<ProcessRow>, String> {
    blocking(|| Ok(grouped_rows())).await
}

/// The group's row, only if `identity` is still its lead (same pid and start time).
fn verified(key: &str, identity: &ProcessIdentity) -> Result<ProcessRow, String> {
    let row = grouped_rows()
        .into_iter()
        .find(|row| row.key == key)
        .ok_or_else(|| "That app has already quit.".to_string())?;
    if row.lead.pid != identity.pid || row.lead.start_time != identity.start_time {
        return Err("That process changed since the list was read. Nothing was closed.".into());
    }
    if let Some(reason) = &row.refusal {
        return Err(reason.clone());
    }
    Ok(row)
}

fn taskkill(pid: u32, force: bool) -> Result<(), String> {
    let mut command = std::process::Command::new("taskkill.exe");
    command.args(["/PID", &pid.to_string()]);
    if force {
        command.args(["/T", "/F"]);
    }
    let output = command.creation_flags(CREATE_NO_WINDOW).output().map_err(|e| e.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    let text = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let text = if text.is_empty() { String::from_utf8_lossy(&output.stdout).trim().to_string() } else { text };
    Err(if text.is_empty() { "taskkill failed".into() } else { text })
}

fn still_running(key: &str, pid: u32) -> bool {
    grouped_rows().iter().any(|row| row.key == key && row.members.iter().any(|m| m.identity.pid == pid))
}

fn close(key: &str, identity: &ProcessIdentity, force: bool) -> Result<QuitOutcome, String> {
    let row = verified(key, identity)?;
    taskkill(row.lead.pid, force)?;
    // Give the process a moment; a graceful request may be answered with a save prompt.
    for _ in 0..8 {
        std::thread::sleep(Duration::from_millis(250));
        if !still_running(key, row.lead.pid) {
            return Ok(QuitOutcome::Quit);
        }
    }
    Ok(QuitOutcome::StillRunning)
}

#[tauri::command]
pub async fn process_quit(key: String, pid: u32, start_time: u64) -> Result<QuitOutcome, String> {
    blocking(move || close(&key, &ProcessIdentity { pid, start_time }, false)).await
}

#[tauri::command]
pub async fn process_force_quit(key: String, pid: u32, start_time: u64) -> Result<QuitOutcome, String> {
    blocking(move || close(&key, &ProcessIdentity { pid, start_time }, true)).await
}
