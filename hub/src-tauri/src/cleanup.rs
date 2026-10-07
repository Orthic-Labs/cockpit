//! Cleanup commands for the hub. The only effect is a move to Trash (through
//! NSFileManager, not AppleScript) and, for Restore, a move back; both are
//! re-checked in core immediately before they happen.

use std::path::{Path, PathBuf};

use cockpit_core::cleanup_scan as cs;
use trash::macos::{DeleteMethod, TrashContextExtMacos};

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn move_to_trash(path: &Path) -> Result<(), String> {
    let mut context = trash::TrashContext::default();
    context.set_delete_method(DeleteMethod::NsFileManager);
    context.delete(path).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cleanup_scan() -> Result<cs::Report, String> {
    tauri::async_runtime::spawn_blocking(|| {
        // Never alongside a storage scan.
        let _exclusive = crate::scanner::exclusive();
        let running = cs::running_process_names();
        cs::scan(&home(), &running)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn cleanup_apply(items: Vec<cs::Request>) -> Result<cs::ApplyResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let running = cs::running_process_names();
        cs::apply(&home(), &items, &running, &mut move_to_trash)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn cleanup_history() -> Vec<cs::Activity> {
    cs::history(&home())
}

#[tauri::command]
pub async fn cleanup_restore(id: String) -> Result<cs::RestoreResult, String> {
    tauri::async_runtime::spawn_blocking(move || cs::restore(&home(), &id))
        .await
        .map_err(|e| e.to_string())?
}
