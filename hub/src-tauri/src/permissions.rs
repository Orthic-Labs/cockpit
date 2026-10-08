//! Full Disk Access for the hub process itself. TCC grants are per app, and the
//! hub (dev.orthic.pulse.hub) is its own responsible app, separate from the notch.
//! The probe only opens and reads one byte of a protected file; it never prompts
//! and never keeps the contents.

use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};

const PANE: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// The hub's own `.app` bundle (Pulse.app/Contents/Helpers/Pulse.app when
/// installed). None in a dev build that is not inside a bundle.
fn hub_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .map(Path::to_path_buf)
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

/// Open System Settings at Full Disk Access and reveal the hub bundle in Finder,
/// so it can be dragged into the list or added with +. Changes no permission.
#[tauri::command]
pub fn fda_request() -> Result<(), String> {
    std::process::Command::new("/usr/bin/open")
        .arg(PANE)
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Some(hub) = hub_bundle() {
        let _ = std::process::Command::new("/usr/bin/open").arg("-R").arg(hub).spawn();
    }
    Ok(())
}
