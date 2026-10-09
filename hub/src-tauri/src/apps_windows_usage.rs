//! "Last used" for the Apps page on Windows.
//!
//! The only per-user source that needs no administrator rights is UserAssist
//! (`HKCU\...\Explorer\UserAssist\{guid}\Count`): Explorer records every program
//! or shortcut the person launches from the shell, with the time of the last
//! launch. Prefetch is administrator-only and file access times are switched off
//! on NTFS by default, so neither is used. An app UserAssist has no record of
//! keeps `last_used: None`, which the page shows as "Unknown" and never counts
//! as unused.

use std::collections::HashMap;
use std::path::PathBuf;

use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
use winreg::RegKey;

use super::Installed;

const USER_ASSIST: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\UserAssist";
/// Seconds between 1601-01-01 (FILETIME) and 1970-01-01 (Unix).
const FILETIME_TO_UNIX: u64 = 11_644_473_600;

fn rot13(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            'a'..='m' | 'A'..='M' => ((c as u8) + 13) as char,
            'n'..='z' | 'N'..='Z' => ((c as u8) - 13) as char,
            other => other,
        })
        .collect()
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from).filter(|p| !p.as_os_str().is_empty())
}

/// The folder a Known Folder GUID names, for the ones programs are launched from.
fn known_folder(guid: &str) -> Option<PathBuf> {
    match guid.to_ascii_uppercase().as_str() {
        "6D809377-6AF0-444B-8957-A3773F02200E" => env_path("ProgramW6432").or_else(|| env_path("ProgramFiles")),
        "7C5A40EF-A0FB-4BFC-874A-C0F2E0B9FA8E" => env_path("ProgramFiles(x86)"),
        "F1B32785-6FBA-4FCF-9D55-7B8E7F157091" => env_path("LOCALAPPDATA"),
        "3EB685DB-65F9-4CF6-A03A-E3EF65729F3D" => env_path("APPDATA"),
        "62AB5D82-FDC1-4DC3-A9DD-070D1D495D97" => env_path("ProgramData"),
        "5E6C858F-0E22-4760-9AFE-EA3317B67173" => env_path("USERPROFILE"),
        "F38BF404-1D43-42F2-9305-67DE0B28FC23" => env_path("SystemRoot"),
        "1AC14E77-02E7-4E5D-B744-2EB1AE5198B7" => env_path("SystemRoot").map(|p| p.join("System32")),
        _ => None,
    }
}

/// The path a decoded UserAssist name stands for, when it is a file path.
fn resolve(decoded: &str) -> Option<String> {
    let text = decoded.trim();
    if let Some(rest) = text.strip_prefix('{') {
        let end = rest.find('}')?;
        let base = known_folder(&rest[..end])?;
        let tail = rest[end + 1..].trim_start_matches(['\\', '/']);
        return Some(base.join(tail).to_string_lossy().to_lowercase());
    }
    let bytes = text.as_bytes();
    (bytes.len() > 3 && bytes[1] == b':' && bytes[2] == b'\\').then(|| text.to_lowercase())
}

/// Lower-case letters and digits only, for comparing a file name with an app name.
pub(super) fn norm(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

fn stem(path: &str) -> String {
    let file = path.rsplit(['\\', '/']).next().unwrap_or(path);
    let lower = file.to_lowercase();
    let trimmed = lower.strip_suffix(".exe").or_else(|| lower.strip_suffix(".lnk")).unwrap_or(&lower);
    norm(trimmed)
}

/// Every launch UserAssist remembers: lower-case path and Unix seconds of the last one.
fn launches() -> Vec<(String, i64)> {
    let mut out: Vec<(String, i64)> = Vec::new();
    let Ok(root) = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(USER_ASSIST, KEY_READ) else {
        return out;
    };
    for guid in root.enum_keys().flatten() {
        let Ok(count) = root.open_subkey_with_flags(format!(r"{guid}\Count"), KEY_READ) else { continue };
        for (name, value) in count.enum_values().flatten() {
            let decoded = rot13(&name);
            // Entries are 72 bytes on Windows 7 and later; the last launch is the FILETIME at offset 60.
            let bytes = &value.bytes;
            if bytes.len() < 68 {
                continue;
            }
            let mut raw = [0u8; 8];
            raw.copy_from_slice(&bytes[60..68]);
            let seconds = u64::from_le_bytes(raw) / 10_000_000;
            if seconds <= FILETIME_TO_UNIX {
                continue;
            }
            let at = (seconds - FILETIME_TO_UNIX) as i64;
            let key = resolve(&decoded).unwrap_or_else(|| decoded.to_lowercase());
            out.push((key, at));
        }
    }
    out
}

/// Fills `last_used` for every app UserAssist has a record of. An app matches when a
/// recorded launch is inside its (unique) install folder, or when the launched
/// program or shortcut is named exactly like the app.
pub(super) fn apply(apps: &mut [Installed]) {
    let launches = launches();
    if launches.is_empty() {
        return;
    }
    let mut by_stem: HashMap<String, i64> = HashMap::new();
    for (path, at) in &launches {
        let entry = by_stem.entry(stem(path)).or_insert(*at);
        *entry = (*entry).max(*at);
    }
    for app in apps.iter_mut() {
        let mut best: Option<i64> = None;
        if let Some(folder) = &app.folder {
            let prefix = format!("{}\\", folder.to_lowercase());
            for (path, at) in &launches {
                if path.starts_with(&prefix) {
                    best = Some(best.map_or(*at, |old| old.max(*at)));
                }
            }
        }
        let name = norm(&app.entry.name);
        if name.len() >= 3 {
            if let Some(at) = by_stem.get(&name) {
                best = Some(best.map_or(*at, |old| old.max(*at)));
            }
        }
        app.entry.last_used = best;
    }
}
