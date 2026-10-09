//! "Last used" for the Apps page on Windows: UserAssist read with `winreg`
//! (`HKCU\...\Explorer\UserAssist\{guid}\Count`). Decoding the records and matching
//! them to apps is in the core (`pulse_core::apps_windows::usage`, with the reasoning
//! for UserAssist); an app with no record keeps `last_used: None`, which the page
//! shows as "Unknown" and never counts as unused.

use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
use winreg::RegKey;

use pulse_core::apps_windows::usage as shared;

use super::Installed;

pub(super) use pulse_core::apps_windows::norm;

const USER_ASSIST: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\UserAssist";

/// Every launch UserAssist remembers: lower-case path and Unix seconds of the last one.
fn launches() -> Vec<(String, i64)> {
    let mut out: Vec<(String, i64)> = Vec::new();
    let Ok(root) = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(USER_ASSIST, KEY_READ) else {
        return out;
    };
    for guid in root.enum_keys().flatten() {
        let Ok(count) = root.open_subkey_with_flags(format!(r"{guid}\Count"), KEY_READ) else { continue };
        for (name, value) in count.enum_values().flatten() {
            if let Some(launch) = shared::launch(&name, &value.bytes) {
                out.push(launch);
            }
        }
    }
    out
}

/// Fills `last_used` for every app UserAssist has a record of.
pub(super) fn apply(apps: &mut [Installed]) {
    shared::apply(apps, &launches());
}
