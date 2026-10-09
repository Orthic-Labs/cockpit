//! Drive health for the hub's Storage view. Readings come from core (smartctl),
//! which keeps them in the Pulse state directory. A background sampler keeps
//! alerts current while the hub runs, and each new alert asks the notch for a
//! peek through the same hub-commands channel the notch settings use.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(not(windows))]
use pulse_core::drive_health as dh;
use pulse_core::drive_health::{Alert, Report};

/// Windows finds its physical drives itself (the core only maps macOS disks).
#[cfg(windows)]
#[path = "health_windows.rs"]
mod windows_drives;

/// One sampling pass at a time; a second caller waits and then finds the
/// interval not yet due.
static SAMPLING: Mutex<()> = Mutex::new(());

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// smartctl inside the app bundle first (Pulse.app/Contents/Helpers/smartctl),
/// then Homebrew. The hub runs from Helpers/Pulse.app/Contents/MacOS.
fn tool_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(helpers) = exe.ancestors().find(|a| a.file_name().and_then(|n| n.to_str()) == Some("Helpers")) {
            candidates.push(helpers.join("smartctl"));
        }
    }
    #[cfg(windows)]
    {
        if let Some(dir) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(PathBuf::from)) {
            candidates.push(dir.join("smartctl.exe"));
            candidates.push(dir.join("Helpers").join("smartctl.exe"));
        }
        candidates.push(PathBuf::from(r"C:\Program Files\smartmontools\bin\smartctl.exe"));
    }
    #[cfg(not(windows))]
    {
        candidates.push(PathBuf::from("/opt/homebrew/bin/smartctl"));
        candidates.push(PathBuf::from("/usr/local/bin/smartctl"));
    }
    candidates
}

/// A mount worth sampling: the startup disk or a drive under /Volumes (every
/// lettered drive on Windows).
fn is_drive_mount(mount: &str) -> bool {
    #[cfg(windows)]
    {
        let _ = mount;
        true
    }
    #[cfg(not(windows))]
    {
        mount == "/" || (mount.starts_with("/Volumes/") && !mount.contains("com.apple."))
    }
}

/// Mounted drives, not installer images or system volumes.
fn mounts() -> Vec<String> {
    let mut mounts: Vec<String> = pulse_core::system_status()
        .disks
        .into_iter()
        .map(|disk| disk.mount_point)
        .filter(|mount| is_drive_mount(mount))
        .filter(|mount| mount == "/" || !crate::is_disk_image(mount))
        .collect();
    mounts.sort();
    mounts.dedup();
    mounts
}

/// Sample when due, returning the alerts this pass produced.
fn sample(mounts: &[String]) -> Vec<Alert> {
    let _guard = SAMPLING.lock().unwrap_or_else(|e| e.into_inner());
    #[cfg(windows)]
    {
        windows_drives::refresh(&crate::cache::dir(), &tool_candidates(), mounts, now())
    }
    #[cfg(not(windows))]
    {
        dh::refresh(&crate::cache::dir(), &tool_candidates(), mounts, now(), false)
    }
}

/// Asks the notch to peek for one alert. Written the way the hub writes its
/// other notch requests: a whole file in hub-commands, then the Darwin ping.
fn tell_notch(alert: &Alert) {
    let command = serde_json::json!({
        "command": "driveAlert",
        "id": alert.id,
        "disk": alert.disk,
        "message": alert.message,
    });
    let dir = crate::bridge_dir().join("hub-commands");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
    let temp = dir.join(format!("{stamp}.tmp"));
    if std::fs::write(&temp, command.to_string()).is_ok()
        && std::fs::rename(&temp, dir.join(format!("{stamp}.json"))).is_ok()
    {
        crate::post("dev.orthic.pulse.hub.command");
    }
}

/// Health for the given mounts: samples when the ten-minute interval has
/// passed, then returns the saved view and notifies the notch of new alerts.
#[tauri::command]
pub async fn drive_health(mounts: Vec<String>) -> Result<Report, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<Report, String> {
        for alert in sample(&mounts) {
            tell_notch(&alert);
        }
        #[cfg(windows)]
        {
            Ok(windows_drives::report(&crate::cache::dir(), &mounts, now()))
        }
        #[cfg(not(windows))]
        {
            Ok(dh::report(&crate::cache::dir(), &tool_candidates(), &mounts, now()))
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Keeps alerts current while the hub runs, even with the window closed.
pub fn start_background() {
    std::thread::spawn(|| loop {
        for alert in sample(&mounts()) {
            tell_notch(&alert);
        }
        std::thread::sleep(Duration::from_secs(120));
    });
}
