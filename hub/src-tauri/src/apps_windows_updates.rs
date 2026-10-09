//! Update checks and updates for the Apps page on Windows, through winget.
//!
//! The check (`winget upgrade` run hidden, its table parsed and tied to installed
//! apps) and the upgrade itself live in the core, `pulse_core::apps_windows::updates`,
//! which `pulse apps updates` uses too. This module keeps what is the hub's: the
//! saved result between launches, the six-hour freshness, and the events the page
//! listens for (`apps-update-row`, `apps-updates-done`, `apps-update-job`).
//!
//! Update: `winget upgrade --id <id> --exact` for the one app, in the background,
//! reported as `apps-update-job` like the Homebrew upgrade on macOS. winget asks
//! for its own elevation when the installer needs it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use pulse_core::apps_windows::updates as winget;
use pulse_core::apps_windows::updates::AppUpdate;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use super::{blocking, now_secs, read_installed, Busy};
use crate::cache;

const UPDATES_FILE: &str = "apps-updates-windows-v1.json";
const UPDATES_FORMAT: u32 = 1;
const FRESH_SECS: i64 = 6 * 3600;

#[derive(Clone, Serialize, Deserialize)]
pub struct UpdateReport {
    checked_at: Option<i64>,
    apps: Vec<AppUpdate>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Saved {
    checked_at: i64,
    apps: Vec<AppUpdate>,
    /// App path -> (winget id, winget source name).
    targets: HashMap<String, (String, String)>,
}

fn load_saved() -> Option<Saved> {
    cache::load::<Saved>(UPDATES_FILE, UPDATES_FORMAT)
}

fn report_of(saved: Option<&Saved>) -> UpdateReport {
    match saved {
        Some(saved) => UpdateReport { checked_at: Some(saved.checked_at), apps: saved.apps.clone() },
        None => UpdateReport { checked_at: None, apps: Vec::new() },
    }
}

pub(super) async fn cached() -> Result<UpdateReport, String> {
    blocking(|| Ok(report_of(load_saved().as_ref()))).await
}

fn check(app: &AppHandle) -> Result<Saved, String> {
    let installed = read_installed();
    let found = winget::check(&installed, &|row: &AppUpdate| {
        let _ = app.emit("apps-update-row", row);
    })?;
    Ok(Saved { checked_at: found.checked_at, apps: found.apps, targets: found.targets })
}

static UPDATES_BUSY: AtomicBool = AtomicBool::new(false);

/// Checks in the background: `apps-update-row` per app with an update, then
/// `apps-updates-done`. A check younger than six hours is reused unless `force`.
pub(super) fn refresh(app: AppHandle, force: bool) -> Result<(), String> {
    if UPDATES_BUSY.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    std::thread::spawn(move || {
        let _busy = Busy(&UPDATES_BUSY);
        if !force {
            if let Some(saved) = load_saved() {
                if now_secs() - saved.checked_at < FRESH_SECS {
                    let _ = app.emit("apps-updates-done", report_of(Some(&saved)));
                    return;
                }
            }
        }
        match check(&app) {
            Ok(saved) => {
                let _ = cache::save(UPDATES_FILE, UPDATES_FORMAT, &saved);
                let _ = app.emit("apps-updates-done", report_of(Some(&saved)));
            }
            Err(error) => {
                crate::scanner::log(&format!("winget update check failed: {error}"));
                // Keep what the last good check found; say nothing newer than it.
                let _ = app.emit("apps-updates-done", report_of(load_saved().as_ref()));
            }
        }
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
    let _ = app.emit("apps-update-job", UpdateJob { path: path.to_string(), state, message });
}

/// Upgrades one app with winget in the background and reports as `apps-update-job`.
/// Returns "running".
pub(super) async fn update(app: AppHandle, path: String) -> Result<String, String> {
    let target = {
        let path = path.clone();
        blocking(move || {
            let saved = load_saved().ok_or("Check for updates first.")?;
            saved
                .targets
                .get(&path)
                .cloned()
                .ok_or_else(|| "winget lists no update for this app. Check for updates again.".to_string())
        })
        .await?
    };
    let (id, source) = target;
    if !winget::safe_argument(&id) {
        return Err("winget gave an id that cannot be used.".into());
    }
    emit_job(&app, &path, "running", "Upgrading with winget…".into());
    std::thread::spawn(move || match winget::upgrade(&id, &source) {
        Ok(()) => {
            // The old answer is stale now; drop this app so the re-check does not show it again.
            if let Some(mut saved) = load_saved() {
                saved.apps.retain(|row| row.path != path);
                saved.targets.remove(&path);
                let _ = cache::save(UPDATES_FILE, UPDATES_FORMAT, &saved);
            }
            emit_job(&app, &path, "done", "Updated with winget.".into());
        }
        Err(error) => emit_job(&app, &path, "failed", error),
    });
    Ok("running".into())
}
