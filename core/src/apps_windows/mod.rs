//! Installed apps on Windows, shared by `pulse apps` and the hub's Apps page.
//!
//! The inventory comes from the Uninstall registry keys (HKLM 64-bit and 32-bit
//! views, HKCU), without updates and system components. This module owns what is
//! decided about those keys (what is an app, de-duplication, the install folder,
//! why uninstall is not offered) and the types both programs share. Reading the
//! registry itself is split: the hub reads it with its `winreg` dependency and
//! hands the raw values to `assemble`; the command line, which has no registry
//! crate, reads the same keys through Windows PowerShell (`read_installed`).
//!
//! `usage` fills "last used" from UserAssist; `updates` asks winget; `running`
//! marks apps with a live process (by program path); `icons` extracts app icons to
//! PNG; `appx` adds Microsoft Store (MSIX/AppX) packages.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub mod appx;
pub mod icons;
pub mod running;
pub mod updates;
pub mod usage;

pub use usage::norm;

pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const READ_TIMEOUT: Duration = Duration::from_secs(90);

pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The fields the hub's Apps page reads. `bundle_id` has no Windows source, so
/// it stays empty. `last_used` comes from UserAssist when it has a record of the
/// app being launched; `running` from the live process list (`running::apply`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppEntry {
    pub name: String,
    /// The install folder when it is known and unique; otherwise the registry key.
    pub path: String,
    pub bundle_id: Option<String>,
    pub version: Option<String>,
    /// `EstimatedSize` from the registry; 0 when the installer did not record one.
    pub size_bytes: u64,
    pub last_used: Option<i64>,
    pub running: bool,
    /// Why uninstall is not offered, if it is not.
    pub protected: Option<String>,
}

pub struct Installed {
    pub entry: AppEntry,
    pub uninstall: Option<String>,
    /// The Uninstall key's own name (a GUID, or something like `Git_is1`).
    pub key_name: String,
    pub publisher: Option<String>,
    /// The install folder, only when exactly one app registered it.
    pub folder: Option<String>,
    /// A program, icon or PNG file the app's icon is read from, when one is known.
    pub icon_source: Option<String>,
    /// The folder whose programs mean this app is running (the install folder, or a
    /// Store package's location). Never offered as a leftover.
    pub run_folder: Option<String>,
}

/// One Uninstall key that has a `DisplayName`, as read from the registry.
pub struct RawUninstall {
    /// Where it was read: "HKLM", "HKLM32" or "HKCU".
    pub label: String,
    pub key_name: String,
    pub display_name: String,
    pub version: Option<String>,
    pub uninstall: Option<String>,
    pub publisher: Option<String>,
    pub install_location: Option<String>,
    /// `EstimatedSize` (kilobytes); 0 when absent.
    pub size_kb: u32,
    pub system_component: bool,
    pub no_remove: bool,
    /// The key has a `ParentKeyName` or `ParentDisplayName` (a part of another product).
    pub has_parent: bool,
    pub release_type: Option<String>,
    /// `DisplayIcon`: a program or icon file, optionally `,index`.
    pub display_icon: Option<String>,
}

/// The file a `DisplayIcon` value names (`"C:\\x\\a.exe",0` or `C:\\x\\a.ico`), when it
/// exists and can carry an icon.
fn display_icon_file(raw: &str) -> Option<String> {
    let mut text = raw.trim().to_string();
    if let Some(rest) = text.strip_prefix('"') {
        text = rest.split('"').next()?.to_string();
    } else if let Some((head, tail)) = text.rsplit_once(',')
        && tail.trim().trim_start_matches('-').chars().all(|c| c.is_ascii_digit())
    {
        text = head.to_string();
    }
    // Expand %NAME% once (Windows keeps DisplayIcon unexpanded when it is REG_EXPAND_SZ).
    for _ in 0..8 {
        let Some(start) = text.find('%') else { break };
        let Some(len) = text[start + 1..].find('%') else { break };
        let name = &text[start + 1..start + 1 + len];
        let value = std::env::var(name).ok()?;
        text.replace_range(start..start + len + 2, &value);
    }
    let path = Path::new(text.trim());
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    (path.is_absolute() && matches!(ext.as_str(), "exe" | "ico" | "dll") && path.is_file())
        .then(|| path.to_string_lossy().into_owned())
}

/// A program in an install folder that stands for the app: the one named like it,
/// else the only non-uninstaller program in the folder's top level.
fn folder_program(folder: &str, app_name: &str) -> Option<String> {
    let wanted = usage::norm(app_name);
    let mut programs: Vec<PathBuf> = std::fs::read_dir(folder)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"))
                && !path
                    .file_stem()
                    .is_some_and(|s| s.to_string_lossy().to_lowercase().starts_with("unins"))
        })
        .collect();
    if let Some(named) = programs
        .iter()
        .find(|p| p.file_stem().is_some_and(|s| usage::norm(&s.to_string_lossy()) == wanted))
    {
        return Some(named.to_string_lossy().into_owned());
    }
    (programs.len() == 1).then(|| programs.remove(0).to_string_lossy().into_owned())
}

fn install_folder(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_matches('"').trim_end_matches(['\\', '/']);
    (!trimmed.is_empty() && Path::new(trimmed).is_dir()).then(|| trimmed.to_string())
}

/// Apps from raw Uninstall keys (in the order the views were read), sorted by name.
/// Updates, hotfixes and parts of other products are not apps. "Last used" is not
/// filled in here (see `usage::apply`).
pub fn assemble(raws: Vec<RawUninstall>) -> Vec<Installed> {
    let mut found: Vec<(Installed, Option<String>)> = Vec::new();
    let mut seen: HashSet<(String, Option<String>)> = HashSet::new();
    for raw in raws {
        if raw.system_component
            || raw.has_parent
            || matches!(
                raw.release_type.as_deref(),
                Some("Update" | "Hotfix" | "Security Update" | "Update Rollup" | "ServicePack")
            )
        {
            continue;
        }
        if !seen.insert((raw.display_name.to_lowercase(), raw.version.clone())) {
            continue;
        }
        let protected = if raw.no_remove {
            Some("Windows does not allow this app to be removed.".to_string())
        } else if raw.uninstall.is_none() {
            Some("This app registered no uninstaller.".to_string())
        } else {
            None
        };
        let icon_source = raw.display_icon.as_deref().and_then(display_icon_file);
        found.push((
            Installed {
                entry: AppEntry {
                    name: raw.display_name,
                    path: format!(r"{}\{UNINSTALL}\{}", raw.label, raw.key_name),
                    bundle_id: None,
                    version: raw.version,
                    size_bytes: u64::from(raw.size_kb) * 1024,
                    last_used: None,
                    running: false,
                    protected,
                },
                uninstall: raw.uninstall,
                key_name: raw.key_name,
                publisher: raw.publisher,
                folder: None,
                icon_source,
                run_folder: None,
            },
            raw.install_location
                .and_then(|location| install_folder(&location)),
        ));
    }
    // An install folder stands in for the registry key only when exactly one app has it.
    let mut counts: HashMap<String, usize> = HashMap::new();
    for folder in found.iter().filter_map(|(_, folder)| folder.as_ref()) {
        *counts.entry(folder.to_lowercase()).or_default() += 1;
    }
    let mut out: Vec<Installed> = found
        .into_iter()
        .map(|(mut app, folder)| {
            if let Some(folder) = folder
                && counts.get(&folder.to_lowercase()) == Some(&1)
            {
                app.entry.path = folder.clone();
                app.run_folder = Some(folder.clone());
                if app.icon_source.is_none() {
                    app.icon_source = folder_program(&folder, &app.entry.name);
                }
                app.folder = Some(folder);
            }
            app
        })
        .collect();
    out.sort_by_key(|app| app.entry.name.to_lowercase());
    out
}

pub const UNINSTALL: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";

// ---------------------------------------------------------------------------
// Running programs
// ---------------------------------------------------------------------------

/// A program's exit code and what it printed to standard output, or an error
/// when it could not be started (`missing` says what to tell the person then) or
/// did not finish in time. Standard input and error are closed.
pub(crate) fn run_capture(
    mut command: Command,
    timeout: Duration,
    name: &str,
    missing: &str,
) -> Result<(u32, String), String> {
    let mut child = command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                missing.to_string()
            } else {
                format!("Could not start {name}: {e}")
            }
        })?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("{name} gave no output stream"))?;
    let reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stdout.read_to_end(&mut buffer);
        buffer
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{name} did not finish in time."));
            }
            Err(e) => return Err(e.to_string()),
        }
    };
    let output = reader
        .join()
        .map_err(|_| format!("{name} output could not be read"))?;
    Ok((
        status.code().unwrap_or(-1) as u32,
        String::from_utf8_lossy(&output).into_owned(),
    ))
}

// ---------------------------------------------------------------------------
// Reading the registry without a registry crate (the command line)
// ---------------------------------------------------------------------------

/// Prints `{"apps":[...],"launches":[...]}`: every Uninstall key with a
/// DisplayName (value types as the registry holds them) and every UserAssist
/// launch record (name, FILETIME). Read-only. No double quotes: it is passed as
/// one command-line argument.
const READ_SCRIPT: &str = concat!(
    "$ErrorActionPreference='SilentlyContinue';",
    "[Console]::OutputEncoding=[Text.Encoding]::UTF8;",
    "function ReadReg($k,$n){$k.GetValue($n,$null,[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)};",
    "$apps=@();",
    "foreach($r in @(",
    "@('HKLM','HKLM:\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall'),",
    "@('HKLM32','HKLM:\\SOFTWARE\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall'),",
    "@('HKCU','HKCU:\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall'))){",
    "foreach($k in Get-ChildItem -LiteralPath $r[1]){",
    "if(-not $k){continue};",
    "$n=ReadReg $k 'DisplayName'; if(-not $n){continue};",
    "$apps+=[pscustomobject]@{l=$r[0];k=$k.PSChildName;n=$n;v=(ReadReg $k 'DisplayVersion');u=(ReadReg $k 'UninstallString');",
    "d=(ReadReg $k 'DisplayIcon');p=(ReadReg $k 'Publisher');i=(ReadReg $k 'InstallLocation');s=(ReadReg $k 'EstimatedSize');c=(ReadReg $k 'SystemComponent');",
    "r=(ReadReg $k 'NoRemove');a=[bool]((ReadReg $k 'ParentKeyName') -or (ReadReg $k 'ParentDisplayName'));t=(ReadReg $k 'ReleaseType')}}};",
    "$launches=@();",
    "foreach($g in Get-ChildItem -LiteralPath 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\UserAssist'){",
    "$c=Get-Item -LiteralPath ($g.PSPath+'\\Count'); if(-not $c){continue};",
    "foreach($n in $c.GetValueNames()){$b=$c.GetValue($n);",
    "if($b -is [byte[]] -and $b.Length -ge 68){$launches+=[pscustomobject]@{n=$n;t=[BitConverter]::ToUInt64($b,60)}}}};",
    "[pscustomobject]@{apps=@($apps);launches=@($launches)}|ConvertTo-Json -Compress -Depth 4",
);

fn powershell() -> PathBuf {
    if let Some(root) = std::env::var_os("SystemRoot") {
        let exe = PathBuf::from(root)
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");
        if exe.is_file() {
            return exe;
        }
    }
    PathBuf::from("powershell.exe")
}

fn text_of(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn flag_of(value: &Value) -> bool {
    value.as_i64().is_some_and(|n| n != 0)
}

/// Every installed app, read-only, through Windows PowerShell. The hub's Apps page
/// reads the same keys with `winreg` and calls `assemble`/`usage::apply` itself.
pub fn read_installed() -> Result<Vec<Installed>, String> {
    let mut command = Command::new(powershell());
    command.args(["-NoProfile", "-NonInteractive", "-Command", READ_SCRIPT]);
    let (code, text) = run_capture(
        command,
        READ_TIMEOUT,
        "PowerShell",
        "Windows PowerShell is not available, so the installed apps cannot be read.",
    )?;
    let parsed: Value = serde_json::from_str(text.trim().trim_start_matches('\u{feff}'))
        .map_err(|_| format!("Could not read the installed apps (PowerShell exit code {code})."))?;
    let raws: Vec<RawUninstall> = parsed["apps"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    Some(RawUninstall {
                        label: text_of(&row["l"])?,
                        key_name: text_of(&row["k"])?,
                        display_name: text_of(&row["n"])?,
                        version: text_of(&row["v"]),
                        uninstall: text_of(&row["u"]),
                        publisher: text_of(&row["p"]),
                        install_location: text_of(&row["i"]),
                        size_kb: row["s"].as_i64().unwrap_or(0) as u32,
                        system_component: flag_of(&row["c"]),
                        no_remove: flag_of(&row["r"]),
                        has_parent: row["a"].as_bool().unwrap_or(false),
                        release_type: text_of(&row["t"]),
                        display_icon: text_of(&row["d"]),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let launches: Vec<(String, i64)> = parsed["launches"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| usage::launch_at(row["n"].as_str()?, row["t"].as_u64()?))
                .collect()
        })
        .unwrap_or_default();
    let mut apps = assemble(raws);
    add_store_apps(&mut apps);
    usage::apply(&mut apps, &launches);
    running::apply(&mut apps);
    Ok(apps)
}

/// Adds the Microsoft Store (MSIX/AppX) apps to an inventory built from the
/// Uninstall keys and keeps it sorted by name. A failed read adds nothing.
pub fn add_store_apps(apps: &mut Vec<Installed>) {
    if let Ok(store) = appx::read() {
        apps.extend(store);
        apps.sort_by_key(|app| app.entry.name.to_lowercase());
    }
}
