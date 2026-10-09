//! Claude Desktop account sync for the Accounts page. The notch has the same
//! restart-and-sync button; this is the visible side: the
//! accounts found, "Restart Claude and sync chats", and backups with Restore.
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
        let accounts = sync::accounts(&root).unwrap_or_default();
        let backups = sync::default_backups()
            .map(|d| sync::backups(&d))
            .unwrap_or_default();
        json!({
            "supported": true,
            "running": sync::claude_running(),
            "accounts": accounts,
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
        let only = sync_set(&root)?;
        let plan = sync::plan_for(&root, Some(&only))?;
        Ok(json!({"plan": plan, "running": sync::claude_running()}))
    })
    .await
}

/// Every account on disk, minus any explicitly excluded with the CLI.
fn sync_set(root: &std::path::Path) -> Result<std::collections::BTreeSet<String>, sync::SyncError> {
    let registry = sync::load_registry(&sync::registry_path()?);
    Ok(registry.sync_set(&sync::accounts(root)?))
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

/// "Restart Claude and sync chats": quit Claude if open (up to 20 s), sync
/// every account's Code sessions with a backup, reopen Claude. Nothing is
/// detected; the owner presses it after signing in to a new account.
#[tauri::command]
pub async fn claude_restart_sync() -> Result<Value, String> {
    blocking(|| {
        let root = sync::default_root()?;
        let backups = sync::default_backups()?;
        if !quit_claude() {
            return Ok(json!({"status": "still_open"}));
        }
        let outcome = (|| -> Result<Value, sync::SyncError> {
            let only = sync_set(&root)?;
            let applied = sync::apply_for(
                &root,
                &backups,
                &sync::claude_running,
                sync::now_ms(),
                Some(&only),
            )?;
            Ok(json!({
                "status": "done",
                "accounts": only.len(),
                "sessions": applied.totals.sessions,
                "files_changed": applied.files_changed,
                "conflicts": applied.conflicts.len(),
                "backup": applied.backup,
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
