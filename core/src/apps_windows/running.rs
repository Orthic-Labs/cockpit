//! Which installed apps are running now, from the process list.
//!
//! A process counts for an app when its program file (the full image path, which
//! sysinfo reads without elevation for the user's own processes) is
//!  * inside the app's install folder (`Installed::run_folder`), or
//!  * the program the app's icon comes from (`Installed::icon_source`), or
//!  * a program named exactly like the app (`Claude` and `claude.exe`).
//!
//! Processes whose image path cannot be read (other users, protected system
//! processes) are skipped, so an app only they run shows as not running; the page
//! never says "running" without a process to point at.

use std::collections::HashSet;
use std::path::Path;

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use super::Installed;
use super::usage::norm;

/// Lower-case image paths of every process whose program file can be read.
pub fn running_programs() -> Vec<String> {
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::OnlyIfNotSet),
    );
    let mut seen = HashSet::new();
    system
        .processes()
        .values()
        .filter_map(|process| process.exe())
        .map(|exe| exe.to_string_lossy().to_lowercase())
        .filter(|exe| seen.insert(exe.clone()))
        .collect()
}

/// A folder deep enough to belong to one app (never a drive root or a shared root
/// such as Program Files).
fn specific_folder(folder: &str) -> bool {
    let path = Path::new(folder);
    if path.components().count() < 3 {
        return false;
    }
    let lower = folder.trim_end_matches('\\').to_lowercase();
    ![
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramW6432",
        "ProgramData",
        "LOCALAPPDATA",
        "APPDATA",
        "USERPROFILE",
        "SystemRoot",
    ]
    .iter()
    .filter_map(|name| std::env::var(name).ok())
    .any(|root| root.trim_end_matches('\\').to_lowercase() == lower)
}

/// `Installed::entry.running` from a list of running program paths (lower case).
pub fn mark(apps: &mut [Installed], programs: &[String]) {
    let stems: HashSet<String> = programs
        .iter()
        .filter_map(|p| p.rsplit('\\').next())
        .map(|file| norm(file.strip_suffix(".exe").unwrap_or(file)))
        .collect();
    for app in apps.iter_mut() {
        let in_folder = app
            .run_folder
            .as_deref()
            .filter(|f| specific_folder(f))
            .is_some_and(|folder| {
                let prefix = format!("{}\\", folder.trim_end_matches('\\').to_lowercase());
                programs.iter().any(|p| p.starts_with(&prefix))
            });
        let by_icon = app
            .icon_source
            .as_deref()
            .map(str::to_lowercase)
            .is_some_and(|icon| icon.ends_with(".exe") && programs.contains(&icon));
        let name = norm(&app.entry.name);
        app.entry.running = in_folder || by_icon || (name.len() >= 3 && stems.contains(&name));
    }
}

/// Reads the process list and marks every app that is running.
pub fn apply(apps: &mut [Installed]) {
    mark(apps, &running_programs());
}
