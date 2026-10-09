//! Pulse hub: a small window over the Rust core. Storage and monitor
//! commands only read. The only commands that change anything are in `apps`:
//! uninstall (move to Trash) and process Quit / Force Quit.

#[cfg(unix)]
mod apps;
#[cfg(windows)]
#[path = "apps_windows.rs"]
mod apps;

mod cache;

mod cleanup;
mod disk_index;

mod duplicates;

mod files;

mod growth;

mod health;

mod metrics_history;

mod permissions;

mod scanner;

mod share;

mod watch;

#[cfg(target_os = "windows")]
mod win_bridge;

use std::path::PathBuf;

use serde::Serialize;
#[cfg(target_os = "macos")]
use tauri::Emitter;
use tauri::Manager;

// The notch's settings bridge (see mac/Notch/Sources/System/HubBridge.swift).
// Darwin notifications through libSystem; no payloads.
#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn notify_post(name: *const std::ffi::c_char) -> u32;
    fn notify_register_check(name: *const std::ffi::c_char, token: *mut i32) -> u32;
    fn notify_check(token: i32, changed: *mut i32) -> u32;
}

/// Where the notch and the hub meet: `~/Library/Application Support/Pulse`, or
/// `%LOCALAPPDATA%\Pulse` on Windows.
pub(crate) fn bridge_dir() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join("AppData").join("Local"))
            .join("Pulse")
    }
    #[cfg(not(windows))]
    {
        home().join("Library/Application Support/Pulse")
    }
}

/// Sections the hub understands for `--section` and the show/select events.
const SECTIONS: [&str; 11] = [
    "overview", "settings", "storage", "monitor", "cleanup", "apps", "accounts", "appearance",
    "notifications", "general", "permissions",
];

#[cfg(target_os = "macos")]
fn post(name: &str) {
    if let Ok(name) = std::ffi::CString::new(name) {
        unsafe { notify_post(name.as_ptr()) };
    }
}

/// Darwin notifications do not exist off macOS; the Windows bridge uses named events.
#[cfg(not(target_os = "macos"))]
fn post(_name: &str) {}

pub(crate) fn home() -> PathBuf {
    #[cfg(windows)]
    let variable = "USERPROFILE";
    #[cfg(not(windows))]
    let variable = "HOME";
    std::env::var_os(variable).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "C:\\" } else { "/" }))
}

#[tauri::command]
async fn status() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(|| {
        serde_json::to_value(pulse_core::system_status()).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn processes() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let mut list = pulse_core::procs();
        list.sort_by(|a, b| {
            b.memory.value.unwrap_or(0).cmp(&a.memory.value.unwrap_or(0))
        });
        list.truncate(12);
        serde_json::to_value(list).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The notch's last published settings and accounts.
#[tauri::command]
fn notch_state() -> Result<serde_json::Value, String> {
    let text = std::fs::read_to_string(bridge_dir().join("notch-state.json"))
        .map_err(|_| "The Pulse notch isn't running.".to_string())?;
    let mut state: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    // A migrated snapshot cannot authorize the new privileged helper.
    if state["product"] != "Pulse" {
        if let Some(object) = state.as_object_mut() {
            object.insert("helper".into(), serde_json::json!("needsReenable"));
            object.insert("helperError".into(), serde_json::Value::Null);
        }
    }
    Ok(state)
}

/// Ask the notch to change a setting or run an account action. The notch is
/// the only writer of its preferences; this only leaves it a request.
#[tauri::command]
fn notch_command(command: serde_json::Value) -> Result<(), String> {
    let dir = bridge_dir().join("hub-commands");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let temp = dir.join(format!("{stamp}.tmp"));
    std::fs::write(&temp, serde_json::to_vec(&command).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    std::fs::rename(&temp, dir.join(format!("{stamp}.json"))).map_err(|e| e.to_string())?;
    post("dev.orthic.pulse.hub.command");
    Ok(())
}

/// The section the hub was opened for (`--section <name>`), if any.
#[tauri::command]
fn initial_section() -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == "--section").and_then(|i| args.get(i + 1).cloned())
}

/// The app passed with `--app <path>`, opened in the Apps review.
#[tauri::command]
fn initial_app() -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == "--app").and_then(|i| args.get(i + 1).cloned())
}

/// Watch for the notch asking a running hub to show a section, and for new
/// notch state; forward both to the page as events.
#[cfg(target_os = "macos")]
fn watch_notch(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        let mut names: Vec<(String, &str, &str)> = SECTIONS
            .iter()
            .map(|section| (format!("dev.orthic.pulse.hub.show.{section}"), "show-section", *section))
            .collect();
        names.push(("dev.orthic.pulse.notch.state".to_string(), "notch-state", ""));
        // `hub.select.<section>` switches the page without showing or focusing
        // the window, so checks can look at a page while someone else works.
        let quiet: Vec<(String, &str, &str)> = names
            .iter()
            .filter(|(_, event, _)| *event == "show-section")
            .map(|(name, _, payload)| (name.replace(".hub.show.", ".hub.select."), "select-section", *payload))
            .collect();
        let mut tokens = Vec::new();
        for (name, event, payload) in names.iter().map(|(n, e, p)| (n.clone(), *e, *p)).chain(quiet) {
            let Ok(cname) = std::ffi::CString::new(name) else { continue };
            let mut token = 0i32;
            if unsafe { notify_register_check(cname.as_ptr(), &mut token) } == 0 {
                // The first check after registering always reports a change;
                // consume it so only real posts reach the page.
                let mut changed = 0i32;
                unsafe { notify_check(token, &mut changed) };
                tokens.push((token, event, payload));
            }
        }
        // A section passed at launch (`--section`) is also sent once the page
        // has had time to load and subscribe.
        if let Some(section) = initial_section() {
            std::thread::sleep(std::time::Duration::from_millis(900));
            let _ = app.emit("show-section", section);
        }
        let mut visibility_posted = std::time::Instant::now() - std::time::Duration::from_secs(2);
        let mut was_visible = false;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(250));
            let visible = app.get_webview_window("main")
                .and_then(|window| window.is_visible().ok()).unwrap_or(false);
            if visible && (!was_visible || visibility_posted.elapsed() >= std::time::Duration::from_secs(2)) {
                post("dev.orthic.pulse.hub.visible");
                visibility_posted = std::time::Instant::now();
            } else if !visible && was_visible {
                post("dev.orthic.pulse.hub.hidden");
            }
            was_visible = visible;
            for (token, event, payload) in &tokens {
                let mut changed = 0i32;
                if unsafe { notify_check(*token, &mut changed) } == 0 && changed != 0 {
                    if *event == "show-section" {
                        show_in_dock(&app);
                    }
                    let event = if *event == "select-section" { "show-section" } else { *event };
                    let _ = app.emit(event, payload.to_string());
                }
            }
        }
    });
}

#[derive(Serialize)]
struct Volume {
    name: String,
    mount_point: String,
    total_bytes: u64,
    available_bytes: u64,
    removable: bool,
    /// The startup disk; its scan starts at the home folder.
    internal: bool,
    /// A mounted disk image (an installer), shown apart from the drives.
    disk_image: bool,
}

/// The name Finder shows for the startup disk: the entry in /Volumes that
/// links to "/".
#[cfg(unix)]
fn startup_name() -> String {
    std::fs::read_dir("/Volumes")
        .ok()
        .and_then(|entries| {
            entries.flatten().find_map(|e| {
                let target = std::fs::canonicalize(e.path()).ok()?;
                (target == std::path::Path::new("/"))
                    .then(|| e.file_name().to_string_lossy().into_owned())
            })
        })
        .unwrap_or_else(|| "Macintosh HD".into())
}

/// A mounted disk image (an installer DMG), not a drive. Windows has none here.
#[cfg(windows)]
pub(crate) fn is_disk_image(_mount: &str) -> bool {
    false
}

/// A mounted disk image (an installer DMG), not a drive.
#[cfg(unix)]
pub(crate) fn is_disk_image(mount: &str) -> bool {
    std::process::Command::new("/usr/sbin/diskutil")
        .args(["info", "-plist", mount])
        .output()
        .map(|o| {
            let text = String::from_utf8_lossy(&o.stdout).replace(['\t', '\n'], "");
            text.contains("<key>BusProtocol</key><string>Disk Image</string>")
        })
        .unwrap_or(false)
}

/// Every mounted volume a person would recognise: the startup disk, external
/// drives and mounted disk images (flagged) under /Volumes, not system
/// volumes or Time Machine snapshots. On Windows: every lettered drive.
#[tauri::command]
async fn volumes() -> Result<Vec<Volume>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let mut out: Vec<Volume> = Vec::new();
        #[cfg(windows)]
        let system_drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into()).to_uppercase();
        for disk in pulse_core::system_status().disks {
            let mount = disk.mount_point.clone();
            #[cfg(unix)]
            let internal = mount == "/";
            #[cfg(windows)]
            let internal = mount.to_uppercase().starts_with(&system_drive);
            #[cfg(unix)]
            if !internal && !mount.starts_with("/Volumes/") {
                continue;
            }
            #[cfg(unix)]
            if mount.contains("com.apple.") {
                continue;
            }
            if out.iter().any(|v| v.mount_point == mount) {
                continue;
            }
            #[cfg(unix)]
            let disk_image = !internal && is_disk_image(&mount);
            #[cfg(windows)]
            let disk_image = false;
            let (Some(total), Some(available)) = (disk.total_bytes, disk.available_bytes) else {
                continue;
            };
            if total == 0 {
                continue;
            }
            #[cfg(unix)]
            let name = if internal {
                startup_name()
            } else {
                mount.rsplit('/').next().unwrap_or(&mount).to_string()
            };
            #[cfg(windows)]
            let name = {
                let letter = mount.trim_end_matches(['\\', '/']);
                if internal { format!("Windows ({letter})") } else { format!("Local Disk ({letter})") }
            };
            out.push(Volume {
                name,
                mount_point: mount,
                total_bytes: total,
                available_bytes: available,
                removable: disk.removable,
                internal,
                disk_image,
            });
        }
        out.sort_by_key(|v| !v.internal);
        Ok(out)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Eject a mounted disk image. Only a direct child of /Volumes that is a disk
/// image is accepted, never a drive. A busy image is reported, not forced.
#[cfg(unix)]
#[tauri::command]
async fn eject(mount: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let path = std::fs::canonicalize(&mount).map_err(|_| "That image is no longer mounted.".to_string())?;
        if path.parent() != Some(std::path::Path::new("/Volumes")) {
            return Err("Only mounted disk images under /Volumes can be ejected here.".into());
        }
        let path = path.to_string_lossy().into_owned();
        if !is_disk_image(&path) {
            return Err("That is a drive, not a disk image. Eject it from Finder.".into());
        }
        let out = std::process::Command::new("/usr/bin/hdiutil")
            .args(["detach", &path])
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            return Ok(());
        }
        let text = String::from_utf8_lossy(&out.stderr).to_lowercase();
        if text.contains("busy") {
            Err("Busy: something is still using it. Close what is open from it and try again.".into())
        } else {
            Err(format!("Could not eject: {}", String::from_utf8_lossy(&out.stderr).trim()))
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Disk images are a macOS idea; drives are ejected from File Explorer.
#[cfg(windows)]
#[tauri::command]
async fn eject(mount: String) -> Result<(), String> {
    let _ = mount;
    Err("Eject drives from File Explorer.".into())
}

/// Open System Settings at Full Disk Access. Opens a window; changes nothing.
#[tauri::command]
fn open_full_disk_access() -> Result<(), String> {
    permissions::fda_request()
}

/// Show an item in File Explorer.
#[cfg(windows)]
pub(crate) fn explorer_show(path: &std::path::Path, is_dir: bool) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let mut command = std::process::Command::new("explorer.exe");
    if is_dir {
        command.arg(path);
    } else {
        command.raw_arg(format!("/select,\"{}\"", path.display()));
    }
    command.spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// Show a file or folder in Finder or File Explorer. Read-only: it only opens a window.
#[tauri::command]
fn reveal(path: String) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("/usr/bin/open")
            .arg("-R")
            .arg(&path)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(windows)]
    {
        let target = std::path::Path::new(&path);
        explorer_show(target, target.is_dir())
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = path;
        Err("Not available on this system.".to_string())
    }
}

#[cfg(all(not(debug_assertions), feature = "qa-native"))]
compile_error!("qa-native must never be enabled in release builds");

/// The hub has a Dock icon while its window is open; closing the window
/// hides it and drops back to notch-only (accessory).
fn show_in_dock(app: &tauri::AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        #[cfg(target_os = "macos")]
        let _ = handle.set_activation_policy(tauri::ActivationPolicy::Regular);
        if let Some(window) = handle.get_webview_window("main") {
            let _ = window.show();
            let _ = window.set_focus();
        }
    });
}

fn hide_to_notch(window: &tauri::Window) {
    let _ = window.hide();
    #[cfg(target_os = "macos")]
    let _ = window.app_handle().set_activation_policy(tauri::ActivationPolicy::Accessory);
}

pub fn run() {
    // QA launches (rightkit-qa) can only pass RIGHTKIT_* keys on macOS, so the isolated
    // HOME (a fixture folder Storage scans) arrives as RIGHTKIT_PULSE_QA_HOME.
    #[cfg(all(debug_assertions, feature = "qa-native"))]
    if let Some(home) = std::env::var_os("RIGHTKIT_PULSE_QA_HOME") {
        #[cfg(windows)]
        std::env::set_var("USERPROFILE", &home);
        std::env::set_var("HOME", home);
    }
    #[cfg(target_os = "macos")]
    if let Err(error) = pulse_core::state_migration::migrate_mac_state(&home()) {
        eprintln!("Pulse could not migrate existing state: {error}");
        return;
    }
    // One hub per user: a second launch asks the first to show its section, then exits.
    #[cfg(target_os = "windows")]
    if !win_bridge::claim_instance() {
        return;
    }
    #[allow(unused_mut)]
    let mut builder = tauri::Builder::default();
    // Env-gated by the rightkit-qa launcher: inert in normal launches.
    #[cfg(all(debug_assertions, feature = "qa-native"))]
    {
        #[cfg(target_os = "macos")]
        {
            builder = builder.activate_ignoring_other_apps(false);
        }
        if let Some(plugin) = rightkit_control::embedded::Control::<tauri::Wry>::new().build_if_enabled() {
            builder = builder.plugin(plugin);
        }
    }
    builder
        // RightKit's shell asks the OS plugin for the platform (traffic-light room on macOS).
        .plugin(tauri_plugin_os::init())
        .setup(|app| {
            // Hidden QA runs stay accessory and must not take focus; otherwise
            // the open hub shows in the Dock until its window is closed.
            let qa_hidden = cfg!(feature = "qa-native") && std::env::var("RIGHTKIT_QA_HIDDEN").as_deref() == Ok("1");
            // `--background` is how the notch starts the hub just to share files:
            // no window and no Dock icon until something asks for them.
            let background = std::env::args().any(|a| a == "--background");
            if qa_hidden {
                #[cfg(target_os = "macos")]
                app.set_activation_policy(tauri::ActivationPolicy::Accessory);
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                }
            } else if background {
                #[cfg(target_os = "macos")]
                app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            } else {
                show_in_dock(app.handle());
            }
            #[cfg(target_os = "macos")]
            watch_notch(app.handle().clone());
            #[cfg(target_os = "windows")]
            win_bridge::watch(app.handle().clone());
            health::start_background();
            metrics_history::start_background();
            disk_index::start_background();
            share::start_background(app.handle().clone());
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window keeps the hub running for the notch.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                hide_to_notch(window);
            }
        })
        .invoke_handler(tauri::generate_handler![
            status, processes, metrics_history::metrics_history, apps::apps_list, apps::app_detail, apps::app_uninstall,
            apps::apps_cached, apps::apps_refresh, apps::app_summary, apps::app_leftovers, apps::app_icons,
            apps::apps_updates_cached, apps::apps_updates_refresh, apps::app_update,
            apps::process_rows, apps::process_quit, apps::process_force_quit, scanner::scan, scanner::scan_status, scanner::last_scan, growth::growth, scanner::children, scanner::search, disk_index::disk_index_status, disk_index::disk_index_sizes, reveal, volumes, eject, open_full_disk_access, notch_state, notch_command, initial_section, initial_app,
            cleanup::cleanup_scan, cleanup::cleanup_cached, cleanup::cleanup_apply, cleanup::cleanup_history, cleanup::cleanup_restore,
            health::drive_health, duplicates::duplicates_scan, duplicates::duplicates_trash, duplicates::home_path,
            files::file_identity, files::finder_open, files::file_choose_folder, files::file_move_plan, files::file_move, files::file_trash,
            permissions::fda_status, permissions::fda_request, permissions::tcc_stale_scan, permissions::tcc_reset,
            share::share_state, share::share_devices, share::share_send, share::share_accept, share::share_decline,
            share::share_cancel, share::share_dismiss, share::open_local_network_settings
        ])
        .run(tauri::generate_context!())
        .expect("error while running Pulse hub");
}
