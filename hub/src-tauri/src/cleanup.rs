//! Cleanup commands for the hub. The only effect is a move to Trash (through
//! NSFileManager on macOS, not AppleScript; the Recycle Bin on Windows) and,
//! for Restore, a move back; both are re-checked in core immediately before
//! they happen.

use std::path::Path;
use std::time::Instant;

use pulse_core::cleanup_scan as cs;
#[cfg(target_os = "macos")]
use trash::macos::{DeleteMethod, TrashContextExtMacos};

use crate::cache;

/// Format of `cleanup-findings-v1.json`: the last scan's report.
const FINDINGS_FILE: &str = "cleanup-findings-v1.json";
const FINDINGS_FORMAT: u32 = 1;

use crate::home;

/// The last saved findings, shown at once on open. They may be stale: every
/// item is checked again (dev and inode) before anything moves.
#[tauri::command]
pub fn cleanup_cached() -> Option<cs::Report> {
    cache::load(FINDINGS_FILE, FINDINGS_FORMAT)
}

pub(crate) fn move_to_trash(path: &Path) -> Result<(), String> {
    #[allow(unused_mut)]
    let mut context = trash::TrashContext::default();
    #[cfg(target_os = "macos")]
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
    tauri::async_runtime::spawn_blocking(move || restore(&id))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(not(windows))]
fn restore(id: &str) -> Result<cs::RestoreResult, String> {
    cs::restore(&home(), id)
}

/// Windows: the core only knows `~/.Trash`, so each item is found in the
/// Recycle Bin by its original path and restored there. An item goes back only
/// if it is still in the bin, its original path is free and it lies under home.
#[cfg(windows)]
fn restore(id: &str) -> Result<cs::RestoreResult, String> {
    let home = home();
    let activity = cs::history(&home).into_iter().find(|a| a.id == id).ok_or("No such action")?;
    let bin = trash::os_limited::list().map_err(|e| e.to_string())?;
    let mut result = cs::RestoreResult::default();
    for item in activity.items.iter().filter(|item| !item.restored) {
        let wanted = std::path::PathBuf::from(&item.path);
        let skip = |reason: &str| cs::Skipped { path: item.path.clone(), reason: reason.to_string() };
        if !wanted.starts_with(&home) {
            result.skipped.push(skip("Location not allowed"));
            continue;
        }
        let found = bin
            .iter()
            .filter(|entry| entry.original_path() == wanted)
            .max_by_key(|entry| entry.time_deleted)
            .cloned();
        let Some(found) = found else {
            result.skipped.push(skip("No longer in the Recycle Bin"));
            continue;
        };
        if wanted.symlink_metadata().is_ok() {
            result.skipped.push(skip("Something is already at the original location"));
            continue;
        }
        match trash::os_limited::restore_all([found]) {
            Ok(()) => {
                result.restored_items += 1;
                result.restored_bytes += item.bytes;
            }
            Err(error) => result.skipped.push(skip(&error.to_string())),
        }
    }
    Ok(result)
}
