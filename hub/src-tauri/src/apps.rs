//! Apps and process commands. Uninstall only moves to the Trash; Quit is
//! graceful; Force Quit is a separate explicit command. All identity and
//! safety re-checks live in the core.
//!
//! Apps load in steps so the window never waits on disk walks: the saved
//! inventory paints at once (`apps_cached`), a background refresh streams rows
//! (`apps-row`), opening an app shows its header while leftovers stream in
//! (`apps-leftovers`). Icons (`apps-icon`) and update checks (`apps-update-row`)
//! also run in the background and report by event.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use pulse_core::app_manager::{
    self, AppDetail, AppEntry, AppUpdate, LeftoverPart, UninstallResult, UpdateAction,
    UpdateReport,
};
use pulse_core::process_control::{self, ProcessRow, QuitOutcome};
use pulse_core::ProcessIdentity;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

async fn blocking<T, F>(work: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(work).await.map_err(|e| e.to_string())?
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Clears a background job's busy flag when the job ends, even if it panics.
struct Busy(&'static AtomicBool);

impl Drop for Busy {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
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

// ---------------------------------------------------------------------------
// Inventory
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct CachedApps {
    /// Unix seconds when the saved inventory was written; None when there is none.
    saved_at: Option<i64>,
    apps: Vec<AppEntry>,
}

/// The saved inventory, returned at once so the list paints before any disk walk.
#[tauri::command]
pub async fn apps_cached() -> Result<CachedApps, String> {
    blocking(|| {
        Ok(match app_manager::cached_apps() {
            Some((saved_at, apps)) => CachedApps {
                saved_at: Some(saved_at),
                apps,
            },
            None => CachedApps {
                saved_at: None,
                apps: Vec::new(),
            },
        })
    })
    .await
}

static INVENTORY_BUSY: AtomicBool = AtomicBool::new(false);

/// Starts an inventory refresh in the background. Each app arrives as an
/// `apps-row` event as its size is known; the full list follows as `apps-inventory`.
#[tauri::command]
pub fn apps_refresh(app: AppHandle) -> Result<(), String> {
    if INVENTORY_BUSY.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    std::thread::spawn(move || {
        let _busy = Busy(&INVENTORY_BUSY);
        let apps = app_manager::list_apps_streaming(&|row: &AppEntry| {
            let _ = app.emit("apps-row", row);
        });
        let _ = app.emit("apps-inventory", &apps);
    });
    Ok(())
}

/// Header facts for one app opened by path (the list supplies them otherwise).
#[tauri::command]
pub async fn app_summary(path: String) -> Result<AppEntry, String> {
    blocking(move || app_manager::app_summary(&path)).await
}

#[derive(Serialize, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum LeftoversEvent {
    Part { path: String, part: LeftoverPart },
    Done { path: String, error: Option<String> },
}

/// Streams one app's bundle row and leftovers as `apps-leftovers` events: one
/// `part` per source as it completes, then a single `done`.
#[tauri::command]
pub fn app_leftovers(app: AppHandle, path: String) -> Result<(), String> {
    std::thread::spawn(move || {
        let result = app_manager::app_leftovers(&path, &|part: LeftoverPart| {
            let _ = app.emit(
                "apps-leftovers",
                LeftoversEvent::Part {
                    path: path.clone(),
                    part,
                },
            );
        });
        let _ = app.emit(
            "apps-leftovers",
            LeftoversEvent::Done {
                path: path.clone(),
                error: result.err(),
            },
        );
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// Icons
// ---------------------------------------------------------------------------

const ICON_WORKERS: usize = 2;
const ICON_MAX_BYTES: usize = 512 * 1024;
const ICON_BATCH: usize = 500;

static ICONS_PENDING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn pending_icons() -> &'static Mutex<HashSet<String>> {
    ICONS_PENDING.get_or_init(|| Mutex::new(HashSet::new()))
}

#[derive(Serialize, Clone)]
struct IconEvent {
    path: String,
    /// None when the app has no usable icon; the UI shows a neutral one.
    data_url: Option<String>,
}

/// Standard base64 (RFC 4648) with padding.
fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A cached PNG as a data URL the webview can show.
fn png_data_url(png: &Path) -> Option<String> {
    let bytes = std::fs::read(png).ok()?;
    (bytes.len() <= ICON_MAX_BYTES).then(|| format!("data:image/png;base64,{}", base64(&bytes)))
}

/// Cached icons come back at once. The rest are rendered in the background and
/// arrive as `apps-icon` events, so the list never waits for sips.
#[tauri::command]
pub async fn app_icons(app: AppHandle, paths: Vec<String>) -> Result<HashMap<String, String>, String> {
    let (ready, missing) = blocking(move || {
        let mut ready: HashMap<String, String> = HashMap::new();
        let mut missing: Vec<String> = Vec::new();
        for path in paths.into_iter().take(ICON_BATCH) {
            match app_manager::app_icon(&path, false).and_then(|png| png_data_url(&png)) {
                Some(url) => {
                    ready.insert(path, url);
                }
                None => missing.push(path),
            }
        }
        Ok((ready, missing))
    })
    .await?;

    let pending: Vec<String> = {
        let mut set = lock(pending_icons());
        missing.into_iter().filter(|path| set.insert(path.clone())).collect()
    };
    if !pending.is_empty() {
        std::thread::spawn(move || {
            let queue = Mutex::new(pending.into_iter());
            std::thread::scope(|scope| {
                for _ in 0..ICON_WORKERS {
                    scope.spawn(|| loop {
                        let next = lock(&queue).next();
                        let Some(path) = next else {
                            break;
                        };
                        let data_url = app_manager::app_icon(&path, true)
                            .and_then(|png| png_data_url(&png));
                        lock(pending_icons()).remove(&path);
                        let _ = app.emit("apps-icon", IconEvent { path, data_url });
                    });
                }
            });
        });
    }
    Ok(ready)
}

// ---------------------------------------------------------------------------
// Updates
// ---------------------------------------------------------------------------

static UPDATES_BUSY: AtomicBool = AtomicBool::new(false);

/// The saved update results, returned at once.
#[tauri::command]
pub async fn apps_updates_cached() -> Result<UpdateReport, String> {
    blocking(|| Ok(app_manager::cached_updates())).await
}

/// Checks every app in the background. Each row arrives as `apps-update-row`,
/// then `apps-updates-done` with the full report. `force` ignores the six-hour cache.
#[tauri::command]
pub fn apps_updates_refresh(app: AppHandle, force: bool) -> Result<(), String> {
    if UPDATES_BUSY.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    std::thread::spawn(move || {
        let _busy = Busy(&UPDATES_BUSY);
        let report = app_manager::check_updates(force, &|row: &AppUpdate| {
            let _ = app.emit("apps-update-row", row);
        });
        let _ = app.emit("apps-updates-done", &report);
    });
    Ok(())
}

#[derive(Serialize, Clone)]
struct UpdateJob {
    path: String,
    /// "running", "done" or "failed".
    state: &'static str,
    message: String,
}

fn emit_job(app: &AppHandle, path: &str, state: &'static str, message: String) {
    let _ = app.emit(
        "apps-update-job",
        UpdateJob {
            path: path.to_string(),
            state,
            message,
        },
    );
}

/// Starts the update for one app. Homebrew upgrades in the background and
/// reports as `apps-update-job`. Sparkle apps are opened to run their own
/// updater. App Store apps are opened in the store. Returns "running",
/// "opened" or "store".
#[tauri::command]
pub async fn app_update(app: AppHandle, path: String) -> Result<String, String> {
    let action = {
        let path = path.clone();
        blocking(move || app_manager::update_action(&path)).await?
    };
    match action {
        UpdateAction::Homebrew { cask } => {
            emit_job(&app, &path, "running", "Upgrading with Homebrew…".into());
            std::thread::spawn(move || match app_manager::homebrew_upgrade(&cask) {
                Ok(_) => emit_job(&app, &path, "done", "Updated with Homebrew.".into()),
                Err(error) => emit_job(&app, &path, "failed", error),
            });
            Ok("running".into())
        }
        UpdateAction::Sparkle => {
            blocking(move || app_manager::open_app(&path)).await?;
            Ok("opened".into())
        }
        UpdateAction::AppStore { url } => {
            blocking(move || app_manager::open_store(url.as_deref())).await?;
            Ok("store".into())
        }
    }
}
