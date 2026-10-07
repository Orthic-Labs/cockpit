//! Installed-app inventory, leftover discovery and uninstall-to-Trash (macOS).
//!
//! Inventory reads `/Applications` and `~/Applications`. Leftovers under
//! `~/Library` are matched by exact bundle id (pre-selectable) or by name
//! (review only, never pre-selected). Uninstall quits the app gracefully
//! first, re-validates every item right before acting, and only ever moves
//! items to the Trash. Nothing is deleted permanently or force-quit here.

use std::collections::HashSet;
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

use crate::apps::valid_bundle_id;

const LIBRARY_DIRS: [&str; 10] = [
    "Application Support",
    "Caches",
    "Preferences",
    "Containers",
    "Group Containers",
    "Saved Application State",
    "Logs",
    "HTTPStorages",
    "WebKit",
    "LaunchAgents",
];
const NAME_MATCH_DIRS: [&str; 3] = ["Application Support", "Caches", "Logs"];
/// Suffixes after `<bundle id>.` that still belong to that exact app.
const EXACT_SUFFIXES: [&str; 5] = [
    "plist",
    "savedstate",
    "binarycookies",
    "lssharedfilelist.plist",
    "plist.lockfile",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppEntry {
    pub name: String,
    pub path: String,
    pub bundle_id: Option<String>,
    pub version: Option<String>,
    pub size_bytes: u64,
    /// Seconds since the Unix epoch, from Spotlight; absent when unknown.
    pub last_used: Option<i64>,
    pub running: bool,
    /// Why uninstall is not offered, if it is not.
    pub protected: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RelatedItem {
    pub path: String,
    /// Library folder the item was found in, or "Application".
    pub label: String,
    /// Matched by exact bundle id (true) or only by name / loose prefix.
    pub exact: bool,
    pub size_bytes: u64,
    pub preselected: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppDetail {
    pub app: AppEntry,
    pub items: Vec<RelatedItem>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MovedItem {
    pub path: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FailedItem {
    pub path: String,
    pub error: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UninstallResult {
    pub moved: Vec<MovedItem>,
    pub failed: Vec<FailedItem>,
    pub moved_bytes: u64,
}

pub struct BundleInfo {
    pub bundle_id: Option<String>,
    pub name: String,
    pub version: Option<String>,
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// The outermost `.app` directory in `path`, if any.
pub fn app_root_of(path: &Path) -> Option<PathBuf> {
    let mut root = PathBuf::new();
    for component in path.components() {
        root.push(component.as_os_str());
        if let Component::Normal(name) = component {
            if name.to_string_lossy().ends_with(".app") {
                return Some(root);
            }
        }
    }
    None
}

/// Bundle id, display name and version from `Contents/Info.plist`.
pub fn bundle_info(root: &Path) -> Option<BundleInfo> {
    let plist = root.join("Contents/Info.plist");
    let output = Command::new("/usr/bin/plutil")
        .args(["-convert", "json", "-o", "-"])
        .arg(&plist)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let text = |key: &str| value.get(key).and_then(|v| v.as_str()).map(str::to_string);
    let bundle_id = text("CFBundleIdentifier").filter(|b| valid_bundle_id(b));
    let version = text("CFBundleShortVersionString").or_else(|| text("CFBundleVersion"));
    let name = root
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    Some(BundleInfo {
        bundle_id,
        name,
        version,
    })
}

/// Disk space used by a file or folder (allocated blocks; symlinks not followed).
pub fn disk_size(path: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(meta) = std::fs::symlink_metadata(&current) else {
            continue;
        };
        total += meta.blocks() * 512;
        if meta.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&current) {
                stack.extend(entries.flatten().map(|e| e.path()));
            }
        }
    }
    total
}

fn run_with_timeout(mut command: Command, limit: Duration) -> Result<String, String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let start = Instant::now();
    loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => {
                let output = child.wait_with_output().map_err(|e| e.to_string())?;
                return if status.success() {
                    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
                } else {
                    Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
                };
            }
            None if start.elapsed() > limit => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("timed out".into());
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// Ask an app to quit (graceful Apple event). Never forces.
pub fn quit_bundle(bundle_id: &str) -> Result<(), String> {
    if !valid_bundle_id(bundle_id) {
        return Err("Unrecognised bundle id; not sending a quit request.".into());
    }
    let mut command = Command::new("/usr/bin/osascript");
    command
        .arg("-e")
        .arg(format!("tell application id \"{bundle_id}\" to quit"));
    run_with_timeout(command, Duration::from_secs(6)).map(|_| ())
}

fn running_roots() -> HashSet<PathBuf> {
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::everything(),
    );
    system
        .processes()
        .values()
        .filter_map(|p| p.exe().and_then(app_root_of))
        .collect()
}

fn civil_to_epoch(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146_097 + doe - 719_468) * 86_400
}

/// Parse Spotlight's `YYYY-MM-DD HH:MM:SS +0000` into epoch seconds.
fn parse_spotlight_date(text: &str) -> Option<i64> {
    let mut parts = text.split_whitespace();
    let date = parts.next()?;
    let time = parts.next()?;
    let mut d = date.split('-').map(|n| n.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let mut t = time.split(':').map(|n| n.parse::<i64>().ok());
    let (h, mi, s) = (t.next()??, t.next()??, t.next()??);
    Some(civil_to_epoch(y, m, day) + h * 3600 + mi * 60 + s)
}

fn last_used(path: &Path) -> Option<i64> {
    let mut command = Command::new("/usr/bin/mdls");
    command
        .args(["-raw", "-name", "kMDItemLastUsedDate"])
        .arg(path);
    let text = run_with_timeout(command, Duration::from_secs(5)).ok()?;
    parse_spotlight_date(&text)
}

fn protected_reason(root: &Path, bundle_id: Option<&str>) -> Option<String> {
    let text = root.to_string_lossy();
    if text.starts_with("/System/") {
        return Some("System app.".into());
    }
    if bundle_id
        .map(|b| b.starts_with("com.apple."))
        .unwrap_or(false)
    {
        return Some("Apple app that macOS protects.".into());
    }
    if bundle_id
        .map(|b| b.starts_with("dev.orthic.cockpit"))
        .unwrap_or(false)
    {
        return Some("This is Cockpit.".into());
    }
    if bundle_id.is_none() {
        return Some("No bundle identifier, so leftovers cannot be matched safely.".into());
    }
    None
}

fn inspect(root: &Path, running: &HashSet<PathBuf>) -> AppEntry {
    let info = bundle_info(root);
    let bundle_id = info.as_ref().and_then(|i| i.bundle_id.clone());
    AppEntry {
        name: root
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        path: root.to_string_lossy().into_owned(),
        protected: protected_reason(root, bundle_id.as_deref()),
        bundle_id,
        version: info.and_then(|i| i.version),
        size_bytes: disk_size(root),
        last_used: last_used(root),
        running: running.contains(root),
    }
}

fn app_dirs() -> Vec<PathBuf> {
    vec![PathBuf::from("/Applications"), home().join("Applications")]
}

fn find_apps() -> Vec<PathBuf> {
    let mut found = Vec::new();
    for dir in app_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".app") {
                found.push(path);
            } else if !name.starts_with('.') && path.is_dir() {
                // One level of grouping folders such as Utilities.
                if let Ok(inner) = std::fs::read_dir(&path) {
                    found.extend(
                        inner
                            .flatten()
                            .map(|e| e.path())
                            .filter(|p| p.extension().map(|x| x == "app").unwrap_or(false)),
                    );
                }
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

/// Every app in the application folders, largest first.
pub fn list_apps() -> Vec<AppEntry> {
    let paths = find_apps();
    let running = running_roots();
    let chunk = paths.len().div_ceil(8).max(1);
    let mut apps = Vec::new();
    std::thread::scope(|scope| {
        let handles: Vec<_> = paths
            .chunks(chunk)
            .map(|part| {
                let running = &running;
                scope.spawn(move || part.iter().map(|p| inspect(p, running)).collect::<Vec<_>>())
            })
            .collect();
        for handle in handles {
            if let Ok(part) = handle.join() {
                apps.extend(part);
            }
        }
    });
    apps.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
    apps
}

/// Is `entry` (a file name under a Library folder) this app's? `Some(true)`
/// is an exact bundle-id match, `Some(false)` is review-only.
fn classify(dir: &str, entry: &str, bundle_id: Option<&str>, name: &str) -> Option<bool> {
    let lower = entry.to_lowercase();
    if let Some(bid) = bundle_id.map(str::to_lowercase) {
        if bid.matches('.').count() >= 2 {
            if lower == bid {
                return Some(true);
            }
            if let Some(rest) = lower.strip_prefix(&format!("{bid}.")) {
                // Sibling products such as `<id>.canary` are not assumed to be ours.
                return Some(EXACT_SUFFIXES.contains(&rest));
            }
            if dir == "Group Containers" && lower.ends_with(&format!(".{bid}")) {
                return Some(true);
            }
        }
    }
    let wanted = name.to_lowercase();
    if wanted.chars().count() >= 4 && NAME_MATCH_DIRS.contains(&dir) && lower == wanted {
        return Some(false);
    }
    None
}

fn related_items(bundle_id: Option<&str>, name: &str) -> Vec<RelatedItem> {
    let library = home().join("Library");
    let mut items = Vec::new();
    for dir in LIBRARY_DIRS {
        let Ok(entries) = std::fs::read_dir(library.join(dir)) else {
            continue;
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().into_owned();
            let Some(exact) = classify(dir, &file_name, bundle_id, name) else {
                continue;
            };
            let path = entry.path();
            items.push(RelatedItem {
                size_bytes: disk_size(&path),
                path: path.to_string_lossy().into_owned(),
                label: dir.into(),
                exact,
                preselected: exact,
            });
        }
    }
    items.sort_by(|a, b| b.exact.cmp(&a.exact).then(b.size_bytes.cmp(&a.size_bytes)));
    items
}

/// An app path is acceptable only when it is a real (non-symlink) `.app`
/// inside an application folder.
fn validate_app_path(path: &str) -> Result<PathBuf, String> {
    let root = PathBuf::from(path);
    if root.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("Unexpected path.".into());
    }
    let inside = app_dirs().iter().any(|dir| root.starts_with(dir));
    if !inside || root.extension().map(|x| x != "app").unwrap_or(true) {
        return Err("Only apps in /Applications or ~/Applications can be uninstalled.".into());
    }
    let meta =
        std::fs::symlink_metadata(&root).map_err(|_| "That app is no longer there.".to_string())?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err("That is not an app bundle.".into());
    }
    Ok(root)
}

/// The bundle and its related Library items, with sizes.
pub fn app_detail(path: &str) -> Result<AppDetail, String> {
    let root = validate_app_path(path)?;
    let app = inspect(&root, &running_roots());
    let mut items = vec![RelatedItem {
        path: app.path.clone(),
        label: "Application".into(),
        exact: true,
        size_bytes: app.size_bytes,
        preselected: app.protected.is_none(),
    }];
    if app.protected.is_none() {
        items.extend(related_items(app.bundle_id.as_deref(), &app.name));
    }
    Ok(AppDetail { app, items })
}

fn trash_dir() -> PathBuf {
    home().join(".Trash")
}

fn trash_with_finder(path: &Path) -> Result<(), String> {
    let mut command = Command::new("/usr/bin/osascript");
    command
        .arg("-e")
        .arg("on run argv")
        .arg("-e")
        .arg("tell application \"Finder\" to delete (POSIX file (item 1 of argv))")
        .arg("-e")
        .arg("end run")
        .arg(path);
    run_with_timeout(command, Duration::from_secs(60)).map(|_| ())
}

fn trash_by_rename(path: &Path) -> Result<(), String> {
    let name = path
        .file_name()
        .ok_or("No file name.")?
        .to_string_lossy()
        .into_owned();
    let trash = trash_dir();
    let mut target = trash.join(&name);
    let mut n = 2;
    while std::fs::symlink_metadata(&target).is_ok() {
        target = trash.join(format!("{name} {n}"));
        n += 1;
    }
    std::fs::rename(path, target).map_err(|e| e.to_string())
}

fn move_to_trash(path: &Path) -> Result<(), String> {
    if let Err(first) = trash_with_finder(path) {
        trash_by_rename(path).map_err(|second| format!("{first}; {second}"))?;
    }
    if std::fs::symlink_metadata(path).is_ok() {
        return Err("Still in its original place after moving to Trash.".into());
    }
    Ok(())
}

fn log_path() -> PathBuf {
    home().join("Library/Application Support/Cockpit/apps-activity.json")
}

fn log_activity(app: &AppEntry, result: &UninstallResult) {
    let path = log_path();
    let mut entries: Vec<serde_json::Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    entries.push(serde_json::json!({
        "time": time,
        "action": "move_to_trash",
        "app": app.name,
        "bundle_id": app.bundle_id,
        "moved": result.moved,
        "failed": result.failed,
        "moved_bytes": result.moved_bytes,
    }));
    if entries.len() > 500 {
        entries.drain(..entries.len() - 500);
    }
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let temp = path.with_extension("json.tmp");
    let write = std::fs::File::create(&temp)
        .and_then(|mut f| f.write_all(&serde_json::to_vec_pretty(&entries).unwrap_or_default()));
    if write.is_ok() {
        let _ = std::fs::rename(temp, path);
    }
}

/// Quit the app gracefully, then move the chosen items to the Trash.
///
/// `items` must be paths the current detail view still reports for this app;
/// anything else is refused. If the app is still running after the quit
/// request, nothing is moved.
pub fn uninstall(
    app_path: &str,
    expected_bundle_id: Option<&str>,
    items: &[String],
) -> Result<UninstallResult, String> {
    let root = validate_app_path(app_path)?;
    let fresh = app_detail(app_path)?;
    if let Some(reason) = &fresh.app.protected {
        return Err(format!(
            "{} is not removable here: {reason}",
            fresh.app.name
        ));
    }
    if fresh.app.bundle_id.as_deref() != expected_bundle_id {
        return Err("The app changed since it was listed; nothing was moved.".into());
    }
    if fresh.app.running {
        let bundle_id = fresh
            .app
            .bundle_id
            .as_deref()
            .ok_or("Cannot quit an app without a bundle id.")?;
        quit_bundle(bundle_id)?;
        let start = Instant::now();
        while running_roots().contains(&root) {
            if start.elapsed() > Duration::from_secs(10) {
                return Err(format!(
                    "{} is still running, so nothing was moved. Quit it, then try again.",
                    fresh.app.name
                ));
            }
            std::thread::sleep(Duration::from_millis(400));
        }
    }

    // Bundle first: if it cannot be moved, its data stays where it is.
    let mut ordered: Vec<&String> = items.iter().filter(|p| **p == fresh.app.path).collect();
    ordered.extend(items.iter().filter(|p| **p != fresh.app.path));
    let mut result = UninstallResult {
        moved: Vec::new(),
        failed: Vec::new(),
        moved_bytes: 0,
    };
    let mut bundle_failed = false;
    for item in ordered {
        let is_bundle = *item == fresh.app.path;
        if bundle_failed && !is_bundle {
            result.failed.push(FailedItem {
                path: item.clone(),
                error: "Skipped because the app itself could not be moved.".into(),
            });
            continue;
        }
        let outcome = revalidate(&fresh, item)
            .and_then(|bytes| move_to_trash(Path::new(item)).map(|_| bytes));
        match outcome {
            Ok(bytes) => {
                result.moved_bytes += bytes;
                result.moved.push(MovedItem {
                    path: item.clone(),
                    bytes,
                });
            }
            Err(error) => {
                bundle_failed |= is_bundle;
                result.failed.push(FailedItem {
                    path: item.clone(),
                    error,
                });
            }
        }
    }
    log_activity(&fresh.app, &result);
    Ok(result)
}

/// Right before acting: the item must still be one the app owns, must not be
/// a symlink, and must still exist. Returns its current size.
fn revalidate(fresh: &AppDetail, item: &str) -> Result<u64, String> {
    if !fresh.items.iter().any(|i| i.path == item) {
        return Err("No longer matches this app; left in place.".into());
    }
    let path = Path::new(item);
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("Unexpected path; left in place.".into());
    }
    let meta = std::fs::symlink_metadata(path).map_err(|_| "Already gone.".to_string())?;
    if meta.file_type().is_symlink() {
        return Err("Is a symbolic link; left in place.".into());
    }
    Ok(disk_size(path))
}
