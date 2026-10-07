//! Apps and process commands. Uninstall only moves to the Trash; Quit is
//! graceful; Force Quit is a separate explicit command. All identity and
//! safety re-checks live in the core.

use cockpit_core::app_manager::{self, AppDetail, AppEntry, UninstallResult};
use cockpit_core::process_control::{self, ProcessRow, QuitOutcome};
use cockpit_core::ProcessIdentity;

async fn blocking<T, F>(work: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(work).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn apps_list() -> Result<Vec<AppEntry>, String> {
    blocking(|| Ok(app_manager::list_apps())).await
}

#[tauri::command]
pub async fn app_detail(path: String) -> Result<AppDetail, String> {
    blocking(move || app_manager::app_detail(&path)).await
}

#[tauri::command]
pub async fn app_uninstall(
    path: String,
    bundle_id: Option<String>,
    items: Vec<String>,
) -> Result<UninstallResult, String> {
    blocking(move || app_manager::uninstall(&path, bundle_id.as_deref(), &items)).await
}

#[tauri::command]
pub async fn process_rows() -> Result<Vec<ProcessRow>, String> {
    blocking(|| Ok(process_control::process_rows())).await
}

#[tauri::command]
pub async fn process_quit(key: String, pid: u32, start_time: u64) -> Result<QuitOutcome, String> {
    blocking(move || process_control::quit(&key, &ProcessIdentity { pid, start_time })).await
}

#[tauri::command]
pub async fn process_force_quit(key: String, pid: u32, start_time: u64) -> Result<QuitOutcome, String> {
    blocking(move || process_control::force_quit(&key, &ProcessIdentity { pid, start_time })).await
}
