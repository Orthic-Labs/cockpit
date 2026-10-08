//! Cleanup commands for the hub. The only effect is a move to Trash (through
//! NSFileManager, not AppleScript) and, for Restore, a move back; both are
//! re-checked in core immediately before they happen.

use std::path::{Path, PathBuf};
use std::time::Instant;

use pulse_core::cleanup_scan as cs;
use trash::macos::{DeleteMethod, TrashContextExtMacos};

use crate::cache;

/// Format of `cleanup-findings-v1.json`: the last scan's report.
const FINDINGS_FILE: &str = "cleanup-findings-v1.json";
const FINDINGS_FORMAT: u32 = 1;

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// The last saved findings, shown at once on open. They may be stale: every
/// item is checked again (dev and inode) before anything moves.
#[tauri::command]
pub fn cleanup_cached() -> Option<cs::Report> {
    cache::load(FINDINGS_FILE, FINDINGS_FORMAT)
}

pub(crate) fn move_to_trash(path: &Path) -> Result<(), String> {
    let mut context = trash::TrashContext::default();
    context.set_delete_method(DeleteMethod::NsFileManager);
    context.delete(path).map_err(|e| e.to_string())
}

/// Run the findings scan on its own utility-QoS thread. The core scan is
/// single-threaded, so there is no pool to cap; it never overlaps a storage scan.
fn run_cleanup_scan() -> Result<cs::Report, String> {
    crate::scanner::low_priority();
    let _exclusive = crate::scanner::exclusive();
    let started = Instant::now();
    let running = cs::running_process_names();
    let report = cs::scan(&home(), &running)?;
    let scan_ms = started.elapsed().as_millis();
    let saved = Instant::now();
    if let Err(error) = cache::save(FINDINGS_FILE, FINDINGS_FORMAT, &report) {
        crate::scanner::log(&format!("saving cleanup findings failed: {error}"));
    }
    crate::scanner::log(&format!(
        "cleanup scan done: scan {scan_ms} ms, {} findings, save {} ms",
        report.findings.len(),
        saved.elapsed().as_millis(),
    ));
    Ok(report)
}

#[tauri::command]
pub async fn cleanup_scan() -> Result<cs::Report, String> {
    tauri::async_runtime::spawn_blocking(|| -> Result<cs::Report, String> {
        std::thread::Builder::new()
            .name("pulse-cleanup".into())
            .spawn(run_cleanup_scan)
            .map_err(|e| e.to_string())?
            .join()
            .map_err(|_| "cleanup scan failed".to_string())?
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
