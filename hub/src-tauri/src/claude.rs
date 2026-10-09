//! Claude Desktop account sync for the Accounts page. The notch runs the
//! automatic sync; this is the visible side: the discovered accounts and their
//! Include switches, a dry-run preview, "Sync now", and backups with Restore.
//! All logic is `pulse_core::claude_sync`.

use pulse_core::claude_sync as sync;
use serde_json::{json, Value};

fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, sync::SyncError> + Send + 'static,
) -> impl std::future::Future<Output = Result<T, String>> {
    async move {
        tauri::async_runtime::spawn_blocking(work)
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())
    }
}

/// Everything the Claude row shows. `supported: false` carries the reason.
#[tauri::command]
pub async fn claude_sync_state() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(|| -> Value {
        let root = match sync::default_root() {
            Ok(r) => r,
            Err(e) => return json!({"supported": false, "reason": e.to_string()}),
        };
        let registry_file = sync::registry_path();
        let registry = registry_file
            .as_ref()
            .ok()
            .map(|p| {
                // Only the very first look records a baseline; later accounts are
                // added by the "New Claude account" button, so it can report them.
                if !p.exists() {
                    let _ = sync::discover(&root, p, sync::now_ms());
                }
                sync::load_registry(p)
            })
            .unwrap_or_default();
        let accounts = sync::accounts(&root).unwrap_or_default();
        let backups = sync::default_backups()
            .map(|d| sync::backups(&d))
            .unwrap_or_default();
        json!({
            "supported": true,
            "running": sync::claude_running(),
            "accounts": accounts,
            "registry": registry,
            "backups": backups,
        })
    })
    .await
    .map_err(|e| e.to_string())
}

/// The dry-run plan for the included accounts: adds, updates, deletes and conflicts.
#[tauri::command]
pub async fn claude_sync_preview() -> Result<Value, String> {
    blocking(|| {
        let root = sync::default_root()?;
        let included = sync::load_registry(&sync::registry_path()?).included();
        let plan = sync::plan_for(&root, Some(&included))?;
        Ok(json!({"plan": plan, "running": sync::claude_running()}))
    })
    .await
}

const CLAUDE_BUNDLE: &str = "com.anthropic.claudefordesktop";

/// Ask Claude to quit through the notch (NSRunningApplication.terminate, a
/// polite quit) and wait up to 20 s. Never force-kills. True once it is closed.
fn quit_claude() -> bool {
    if !sync::claude_running() {
        return true;
    }
    #[cfg(target_os = "macos")]
    {
        let _ = crate::notch_command(json!({"command": "quitClaude"}));
        for _ in 0..80 {
            std::thread::sleep(std::time::Duration::from_millis(250));
            if !sync::claude_running() {
                return true;
            }
        }
    }
    false
}

fn reopen_claude() -> bool {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("/usr/bin/open")
            .args(["-b", CLAUDE_BUNDLE])
            .status()
            .is_ok_and(|s| s.success())
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// The "New Claude account" button: quit Claude if open (up to 20 s), add
/// account ids not yet known, sync with a backup, reopen Claude.
#[tauri::command]
pub async fn claude_new_account() -> Result<Value, String> {
    blocking(|| {
        let root = sync::default_root()?;
        let registry = sync::registry_path()?;
        let backups = sync::default_backups()?;
        if !quit_claude() {
            return Ok(json!({"status": "still_open"}));
        }
        let outcome = (|| -> Result<Value, sync::SyncError> {
            let found = sync::discover(&root, &registry, sync::now_ms())?;
            let auto = sync::auto_sync(
                &root,
                &backups,
                &registry,
                &sync::claude_running,
                sync::now_ms(),
            )?;
            if let Some(error) = auto.error {
                return Err(sync::SyncError::Io(error));
            }
            let (files, sessions, backup, conflicts) = match &auto.synced {
                Some(s) => (
                    s.files_changed,
                    s.totals.sessions,
                    s.backup.clone(),
                    s.conflicts.len(),
                ),
                None => (0, 0, None, 0),
            };
            Ok(json!({
                "status": "done",
                "added": found.new_accounts.len(),
                "sessions": sessions,
                "files_changed": files,
                "conflicts": conflicts,
                "backup": backup,
            }))
        })();
        // Claude is brought back whatever the sync said.
        let reopened = reopen_claude();
        outcome.map(|mut v| {
            v["reopened"] = json!(reopened);
            v
        })
    })
    .await
}

#[tauri::command]
pub async fn claude_include(id: String, included: bool) -> Result<Value, String> {
    blocking(move || {
        let registry = sync::set_included(&sync::registry_path()?, &id, included)?;
        Ok(json!({"registry": registry}))
    })
    .await
}

/// Put a backup back. Without `force` it refuses when Claude changed a file since.
#[tauri::command]
pub async fn claude_restore(ts: String, force: bool) -> Result<Value, String> {
    blocking(move || {
        if !quit_claude() {
            return Ok(json!({"status": "still_open"}));
        }
        let result = sync::restore(&sync::default_backups()?, &ts, force, &sync::claude_running);
        let reopened = reopen_claude();
        Ok(json!({"status": "done", "restored": result?, "reopened": reopened}))
    })
    .await
}
