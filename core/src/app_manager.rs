//! Installed-app inventory, leftover discovery and uninstall-to-Trash (macOS).
//!
//! Inventory reads `/Applications` and `~/Applications`. Leftovers are found
//! in both `~/Library` and `/Library` by bundle id, helper ids harvested from
//! the bundle, app groups and team id from the code signature, installer
//! receipts and (review only) app name. Every item carries a confidence:
//! exact and helper-id matches are pre-selected, everything else is shown but
//! left unselected. Uninstall quits the app gracefully first, re-validates
//! every item right before acting, and only ever moves items to the Trash
//! (Finder, which asks for an administrator password for root-owned items).
//! Nothing is deleted permanently or force-quit here.
//!
//! The matching rules are ported from Uninstally (MIT, (c) 2026 Codenta,
//! github.com/gostonx/uninstally at 26ac1a0e): `AssociatedFileScanner`
//! (identifier-first matching, name matches only for a few roots and never
//! for short names), `IdentifierMatcher` (own ids and nested-id prefixes),
//! `LibraryPaths` (the user and system Library roots) and `PathValidator`
//! (never offer protected or shared system locations). Pearcleaner was read
//! for ideas only (team-id group containers, installer receipts); no source
//! was copied.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

use crate::apps::valid_bundle_id;

/// Folders searched under `~/Library`, and under `/Library` for `SYSTEM_DIRS`.
const USER_DIRS: &[&str] = &[
    "Application Support",
    "Caches",
    "Preferences",
    "Preferences/ByHost",
    "Containers",
    "Group Containers",
    "Saved Application State",
    "HTTPStorages",
    "WebKit",
    "Logs",
    "Cookies",
    "LaunchAgents",
    "Application Scripts",
    "Internet Plug-Ins",
    "PreferencePanes",
    "Services",
    "QuickLook",
    "Spotlight",
];
const SYSTEM_DIRS: &[&str] = &[
    "Application Support",
    "Caches",
    "Preferences",
    "Logs",
    "LaunchAgents",
    "LaunchDaemons",
    "PrivilegedHelperTools",
    "Internet Plug-Ins",
    "PreferencePanes",
    "Services",
    "QuickLook",
    "Spotlight",
    "Extensions",
];
/// Folders where a folder named exactly like the app is accepted (review only).
const NAME_MATCH_DIRS: &[&str] = &["Application Support", "Caches", "Logs"];
/// Suffixes after `<bundle id>.` that are plainly that app's own files.
const EXACT_SUFFIXES: &[&str] = &[
    "plist",
    "savedstate",
    "binarycookies",
    "lssharedfilelist.plist",
    "plist.lockfile",
];
/// Apple apps that come from the App Store or Apple's downloads and can be
/// removed. Anything else `com.apple.*` without an App Store receipt is a
/// system app and stays protected.
const APPLE_REMOVABLE: &[&str] = &[
    "com.apple.imovie",
    "com.apple.garageband",
    "com.apple.iwork.",
    "com.apple.pages",
    "com.apple.numbers",
    "com.apple.keynote",
    "com.apple.dt.xcode",
    "com.apple.finalcut",
    "com.apple.logic",
    "com.apple.motionapp",
    "com.apple.compressor",
    "com.apple.mainstage",
    "com.apple.testflight",
    "com.apple.clips",
    "com.apple.creator",
    "com.apple.apple-creator",
    "com.apple.applecreator",
    "com.apple.ibooksauthor",
    "com.apple.reality",
    "com.apple.swift-playgrounds",
];
/// Standard folders under a Library root. They hold many apps' data, so they
/// are never offered themselves (Uninstally `PathValidator`, protected set).
const SHARED_LIBRARY_FOLDERS: &[&str] = &[
    "Application Support",
    "Application Scripts",
    "Audio",
    "Audio/Plug-Ins",
    "Audio/Plug-Ins/Components",
    "Audio/Plug-Ins/VST",
    "Audio/Plug-Ins/VST3",
    "Caches",
    "ColorSync",
    "ColorSync/Profiles",
    "Colors",
    "Containers",
    "CoreMediaIO",
    "Developer",
    "Documentation",
    "Extensions",
    "Filesystems",
    "Fonts",
    "Frameworks",
    "Group Containers",
    "HTTPStorages",
    "Input Methods",
    "Internet Plug-Ins",
    "Java",
    "Keyboard Layouts",
    "LaunchAgents",
    "LaunchDaemons",
    "Logs",
    "Preferences",
    "Preferences/ByHost",
    "PreferencePanes",
    "Printers",
    "PrivilegedHelperTools",
    "QuickLook",
    "Receipts",
    "Saved Application State",
    "Screen Savers",
    "Scripts",
    "Services",
    "Spotlight",
    "StartupItems",
    "WebKit",
    "Cookies",
    "Security",
];
const SHARED_ROOTS: &[&str] = &[
    "/",
    "/Applications",
    "/Applications/Utilities",
    "/Library",
    "/System",
    "/Users",
    "/Users/Shared",
    "/Volumes",
    "/usr",
    "/usr/bin",
    "/usr/lib",
    "/usr/sbin",
    "/usr/share",
    "/usr/local",
    "/usr/local/bin",
    "/usr/local/lib",
    "/usr/local/share",
    "/usr/local/etc",
    "/usr/local/include",
    "/bin",
    "/sbin",
    "/etc",
    "/var",
    "/tmp",
    "/opt",
    "/opt/local",
    "/private",
    "/private/var",
    "/private/etc",
    "/cores",
];
const PROTECTED_TREES: &[&str] = &[
    "/System",
    "/usr/bin",
    "/usr/sbin",
    "/usr/lib",
    "/bin",
    "/sbin",
    "/private/var/db",
    "/private/etc",
];
const MAX_RECEIPT_ITEMS: usize = 300;

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
    /// Group for display: "Application", "User Library", "System Library"
    /// or "Installer receipt".
    pub location: String,
    /// Matched by exact bundle id (true) or by something looser.
    pub exact: bool,
    /// "exact", "helper", "group", "prefix", "team", "name" or "receipt".
    pub confidence: String,
    /// Plain-language reason for the match.
    pub reason: String,
    /// Root-owned or under /Library: Finder will ask for an administrator.
    pub admin: bool,
    pub size_bytes: u64,
    pub preselected: bool,
}

/// A login item or background job that belongs to the app. Informational:
/// the launchd plists themselves are listed (and removable) as items.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BackgroundEntry {
    pub kind: String,
    pub label: String,
    pub path: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppDetail {
    pub app: AppEntry,
    pub items: Vec<RelatedItem>,
    pub background: Vec<BackgroundEntry>,
    /// Installer packages (`pkgutil --pkgs`) that matched this app.
    pub receipts: Vec<String>,
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
    /// Id of the activity-log entry written for this run.
    #[serde(default)]
    pub activity_id: Option<String>,
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
        if let Component::Normal(name) = component
            && name.to_string_lossy().ends_with(".app")
        {
            return Some(root);
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
        if meta.is_dir()
            && let Ok(entries) = std::fs::read_dir(&current)
        {
            stack.extend(entries.flatten().map(|e| e.path()));
        }
    }
    total
}

struct Captured {
    ok: bool,
    stdout: Vec<u8>,
    stderr: String,
}

/// Run a command with a time limit, reading both pipes on threads so large
/// output (`pkgutil --files`) cannot stall it. `input` is fed to stdin.
fn run_captured(
    mut command: Command,
    limit: Duration,
    input: Option<&[u8]>,
) -> Result<Captured, String> {
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    if let (Some(bytes), Some(mut stdin)) = (input, child.stdin.take()) {
        let data = bytes.to_vec();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&data);
        });
    }
    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    let out_thread = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(pipe) = out_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut buffer);
        }
        buffer
    });
    let err_thread = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(pipe) = err_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut buffer);
        }
        buffer
    });
    let start = Instant::now();
    loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => {
                let stdout = out_thread.join().unwrap_or_default();
                let stderr = err_thread.join().unwrap_or_default();
                return Ok(Captured {
                    ok: status.success(),
                    stdout,
                    stderr: String::from_utf8_lossy(&stderr).trim().to_string(),
                });
            }
            None if start.elapsed() > limit => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("timed out".into());
            }
            None => std::thread::sleep(Duration::from_millis(40)),
        }
    }
}

fn run_with_timeout(command: Command, limit: Duration) -> Result<String, String> {
    let done = run_captured(command, limit, None)?;
    if done.ok {
        Ok(String::from_utf8_lossy(&done.stdout).trim().to_string())
    } else {
        Err(done.stderr)
    }
}

/// Stdout of a command, only when it succeeded.
fn stdout_of(command: Command, limit: Duration) -> Option<String> {
    run_with_timeout(command, limit).ok()
}

fn plist_json_value(bytes: &[u8]) -> Option<serde_json::Value> {
    serde_json::from_slice(bytes).ok()
}

/// A plist file as JSON (`plutil` reads binary and XML plists).
fn plist_json(path: &Path) -> Option<serde_json::Value> {
    let mut command = Command::new("/usr/bin/plutil");
    command.args(["-convert", "json", "-o", "-"]).arg(path);
    let done = run_captured(command, Duration::from_secs(10), None).ok()?;
    if !done.ok {
        return None;
    }
    plist_json_value(&done.stdout)
}

/// Plist bytes (for example codesign's entitlements) as JSON.
fn plist_bytes_json(bytes: &[u8]) -> Option<serde_json::Value> {
    if bytes.is_empty() {
        return None;
    }
    let mut command = Command::new("/usr/bin/plutil");
    command.args(["-convert", "json", "-o", "-", "-"]);
    let done = run_captured(command, Duration::from_secs(10), Some(bytes)).ok()?;
    if !done.ok {
        return None;
    }
    plist_json_value(&done.stdout)
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

/// Apple apps from the App Store or Apple's downloads (iMovie, GarageBand,
/// Pages, Numbers, Keynote, the Creator Studio apps) are ordinary apps and can
/// be removed. Only the real system apps stay protected.
fn apple_app_removable(root: &Path, bundle_id: &str) -> bool {
    if root.join("Contents/_MASReceipt/receipt").exists() {
        return true;
    }
    let lower = bundle_id.to_lowercase();
    APPLE_REMOVABLE.iter().any(|p| lower.starts_with(p))
}

fn protected_reason(root: &Path, bundle_id: Option<&str>) -> Option<String> {
    let text = root.to_string_lossy();
    if text.starts_with("/System/") {
        return Some("System app.".into());
    }
    if let Some(bundle_id) = bundle_id
        && bundle_id.to_lowercase().starts_with("com.apple.")
        && !apple_app_removable(root, bundle_id)
    {
        return Some("System app that macOS protects.".into());
    }
    if bundle_id
        .map(|b| b.starts_with("dev.orthic.pulse"))
        .unwrap_or(false)
    {
        return Some("This is Pulse.".into());
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

/// Pulse's own state folder (inventory, icon and update caches).
fn support_dir() -> PathBuf {
    home().join("Library/Application Support/Pulse")
}

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Waits for a scoped thread. A panic in it is re-raised, as `thread::scope` would.
fn join<T>(handle: std::thread::ScopedJoinHandle<'_, T>) -> T {
    handle
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

/// Writes JSON through a temporary file, so a reader never sees a half-written cache.
fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

const INVENTORY_WORKERS: usize = 4;
const APPS_CACHE_SCHEMA: u32 = 1;

#[derive(Serialize, Deserialize)]
struct AppsCacheFile {
    schema: u32,
    saved_at: i64,
    apps: Vec<AppEntry>,
}

/// The last saved inventory and when it was saved, for an instant first paint.
pub fn cached_apps() -> Option<(i64, Vec<AppEntry>)> {
    let bytes = std::fs::read(support_dir().join("apps-cache.json")).ok()?;
    let file: AppsCacheFile = serde_json::from_slice(&bytes).ok()?;
    (file.schema == APPS_CACHE_SCHEMA).then_some((file.saved_at, file.apps))
}

/// Every app in the application folders, largest first. Each app goes to
/// `on_app` as soon as its sizes are known, from a small worker pool, so one
/// large bundle does not hold up the rest. The result replaces the saved cache.
pub fn list_apps_streaming(on_app: &(dyn Fn(&AppEntry) + Sync)) -> Vec<AppEntry> {
    let paths = find_apps();
    let running = running_roots();
    let next = AtomicUsize::new(0);
    let done = Mutex::new(Vec::<AppEntry>::with_capacity(paths.len()));
    std::thread::scope(|scope| {
        for _ in 0..INVENTORY_WORKERS.min(paths.len()) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, AtomicOrdering::Relaxed);
                    let Some(path) = paths.get(index) else {
                        break;
                    };
                    let entry = inspect(path, &running);
                    on_app(&entry);
                    lock(&done).push(entry);
                }
            });
        }
    });
    let mut apps: Vec<AppEntry> = done
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    apps.sort_by(|a: &AppEntry, b: &AppEntry| {
        b.size_bytes.cmp(&a.size_bytes).then(a.path.cmp(&b.path))
    });
    let file = AppsCacheFile {
        schema: APPS_CACHE_SCHEMA,
        saved_at: now_epoch(),
        apps: apps.clone(),
    };
    let _ = write_json_atomic(&support_dir().join("apps-cache.json"), &file);
    apps
}

/// Every app in the application folders, largest first.
pub fn list_apps() -> Vec<AppEntry> {
    list_apps_streaming(&|_: &AppEntry| {})
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Conf {
    Exact,
    Helper,
    Group,
    Family,
    Team,
    Name,
}

impl Conf {
    fn as_str(self) -> &'static str {
        match self {
            Conf::Exact => "exact",
            Conf::Helper => "helper",
            Conf::Group => "group",
            Conf::Family => "prefix",
            Conf::Team => "team",
            Conf::Name => "name",
        }
    }
}

struct Hit {
    conf: Conf,
    reason: String,
    selected: bool,
}

/// Everything that identifies one app on disk. All strings are lower case.
struct Identity {
    /// The app's own id first, then ids of helpers nested in the bundle.
    ids: Vec<String>,
    main: Option<String>,
    /// Vendor-ish prefixes with a trailing dot, for example `com.adobe.acc.`.
    families: Vec<String>,
    /// App groups from the code-signing entitlements.
    groups: Vec<String>,
    team: Option<String>,
    /// Names the user would recognise (4+ characters only).
    names: Vec<String>,
    /// Bundle ids of every other installed app.
    others: Vec<String>,
    /// Another installed app shares this app's vendor namespace.
    vendor_sibling: bool,
}

impl Identity {
    fn group_selectable(&self, group: &str) -> bool {
        self.main.as_deref().is_some_and(|m| group.contains(m)) || !self.vendor_sibling
    }
}

/// Length of the longest id in `ids` that `name` equals or sits under
/// (`name == id` or `name` starts with `id.`); 0 when none.
fn longest_match(name: &str, ids: &[String]) -> usize {
    ids.iter()
        .filter(|id| {
            name.strip_prefix(id.as_str())
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
        })
        .map(String::len)
        .max()
        .unwrap_or(0)
}

fn is_team_id(text: &str) -> bool {
    text.len() == 10
        && text
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

/// Port of Uninstally's `AssociatedFileScanner.matchChildren` plus its
/// `IdentifierMatcher`: `entry` is one file name inside the Library folder
/// `dir`. Another installed app that owns the name more specifically wins.
fn classify(dir: &str, entry: &str, identity: &Identity) -> Option<Hit> {
    if entry.starts_with('.') {
        return None;
    }
    let lower = entry.to_lowercase();
    let mut core: &str = &lower;
    let mut team_hit = false;
    if matches!(dir, "Group Containers" | "Application Scripts") {
        if let Some((first, _)) = entry.split_once('.')
            && is_team_id(first)
        {
            team_hit = identity
                .team
                .as_deref()
                .is_some_and(|t| t.eq_ignore_ascii_case(first));
            core = &lower[first.len() + 1..];
        }
        core = core.strip_prefix("group.").unwrap_or(core);
    }

    // Declared app groups (entitlements) are authoritative for this app.
    if dir == "Group Containers" && identity.groups.contains(&lower) {
        return Some(Hit {
            conf: Conf::Group,
            reason: "App group declared by this app".into(),
            selected: identity.group_selectable(&lower),
        });
    }

    let other = longest_match(core, &identity.others);

    // Own ids: exact, or nested (helpers, siblings such as `<id>.canary`).
    let mut best: Option<(&String, &str)> = None;
    for id in &identity.ids {
        if let Some(rest) = core.strip_prefix(id.as_str())
            && (rest.is_empty() || rest.starts_with('.'))
            && best.is_none_or(|(b, _)| id.len() > b.len())
        {
            best = Some((id, rest));
        }
    }
    if let Some((id, rest)) = best
        && id.len() >= other
    {
        let is_main = identity.main.as_deref() == Some(id.as_str());
        let plain = rest.is_empty() || EXACT_SUFFIXES.contains(&rest.trim_start_matches('.'));
        let conf = if is_main && plain {
            Conf::Exact
        } else {
            Conf::Helper
        };
        let reason = if conf == Conf::Exact {
            "Exact bundle id".to_string()
        } else if is_main {
            format!("Nested under bundle id {id}")
        } else {
            format!("Id of a helper inside the app ({id})")
        };
        return Some(Hit {
            conf,
            reason,
            selected: true,
        });
    }

    // Vendor-ish prefixes: shown, never selected.
    for family in &identity.families {
        let bare = family.trim_end_matches('.');
        if core.starts_with(family.as_str()) || core == bare {
            if other > bare.len() {
                return None;
            }
            return Some(Hit {
                conf: Conf::Family,
                reason: format!("Shares the prefix {family}"),
                selected: false,
            });
        }
    }

    if team_hit && other == 0 {
        return Some(Hit {
            conf: Conf::Team,
            reason: "Same developer team id".into(),
            selected: false,
        });
    }

    // Uninstally's name rule: only a few roots, never short names.
    if NAME_MATCH_DIRS.contains(&dir) && other == 0 && identity.names.iter().any(|n| n == core) {
        return Some(Hit {
            conf: Conf::Name,
            reason: "Folder named like the app".into(),
            selected: false,
        });
    }
    None
}

fn needs_admin(path: &Path) -> bool {
    let me = std::fs::metadata(home()).map(|m| m.uid()).ok();
    let owner = std::fs::symlink_metadata(path).map(|m| m.uid()).ok();
    match (me, owner) {
        (Some(a), Some(b)) => a != b,
        _ => !path.starts_with(home()),
    }
}

/// Port of Uninstally's `PathValidator` protected set: locations that hold
/// many apps' data or the system itself are never offered.
fn is_shared_system_path(path: &Path) -> bool {
    let text = path.to_string_lossy();
    let text = text.trim_end_matches('/');
    if text.is_empty() || SHARED_ROOTS.contains(&text) {
        return true;
    }
    if PROTECTED_TREES
        .iter()
        .any(|tree| text == *tree || text.starts_with(&format!("{tree}/")))
    {
        return true;
    }
    if path.components().count() <= 2 {
        return true;
    }
    let home = home();
    let home_text = home.to_string_lossy();
    let home_text = home_text.trim_end_matches('/');
    if text == home_text {
        return true;
    }
    for base in ["/Library".to_string(), format!("{home_text}/Library")] {
        if let Some(rest) = text.strip_prefix(&format!("{base}/"))
            && SHARED_LIBRARY_FOLDERS.contains(&rest)
        {
            return true;
        }
    }
    if let Some(rest) = text.strip_prefix(&format!("{home_text}/"))
        && matches!(
            rest,
            "Desktop"
                | "Documents"
                | "Downloads"
                | "Movies"
                | "Music"
                | "Pictures"
                | "Public"
                | "Applications"
                | "Library"
        )
    {
        return true;
    }
    false
}

/// Code-signing facts: app groups and the developer team id.
fn signing_info(root: &Path) -> (Vec<String>, Option<String>) {
    let mut groups = Vec::new();
    let mut team: Option<String> = None;
    let mut command = Command::new("/usr/bin/codesign");
    command.args(["-d", "--entitlements", ":-"]).arg(root);
    if let Ok(done) = run_captured(command, Duration::from_secs(15), None)
        && let Some(value) = plist_bytes_json(&done.stdout)
    {
        if let Some(list) = value
            .get("com.apple.security.application-groups")
            .and_then(|v| v.as_array())
        {
            groups.extend(
                list.iter()
                    .filter_map(|g| g.as_str())
                    .map(str::to_lowercase),
            );
        }
        team = value
            .get("com.apple.developer.team-identifier")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        if team.is_none() {
            team = ["com.apple.application-identifier", "application-identifier"]
                .iter()
                .filter_map(|k| value.get(*k).and_then(|v| v.as_str()))
                .filter_map(|v| v.split_once('.').map(|(t, _)| t.to_string()))
                .find(|t| is_team_id(t));
        }
    }
    if team.is_none() {
        let mut command = Command::new("/usr/bin/codesign");
        command.args(["-dvv"]).arg(root);
        if let Ok(done) = run_captured(command, Duration::from_secs(15), None) {
            team = done
                .stderr
                .lines()
                .find_map(|l| l.strip_prefix("TeamIdentifier="))
                .map(str::trim)
                .filter(|t| is_team_id(t))
                .map(str::to_string);
        }
    }
    groups.sort();
    groups.dedup();
    (groups, team.map(|t| t.to_lowercase()))
}

/// Ids of helpers nested in the bundle (login items, XPC services, plug-ins,
/// privileged helpers) and the background entries they imply.
fn harvest_bundle(
    root: &Path,
    main_plist: Option<&serde_json::Value>,
) -> (Vec<String>, Vec<BackgroundEntry>) {
    let mut ids: Vec<String> = Vec::new();
    let mut background = Vec::new();
    let contents = root.join("Contents");
    let nested = [
        ("Library/LoginItems", "Login item"),
        ("XPCServices", "XPC service"),
        ("PlugIns", "Plug-in"),
        ("Frameworks", "Helper app"),
        ("Helpers", "Helper app"),
    ];
    for (sub, kind) in nested {
        let Ok(entries) = std::fs::read_dir(contents.join(sub)) else {
            continue;
        };
        for entry in entries.flatten().take(60) {
            let path = entry.path();
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if !matches!(ext.as_str(), "app" | "xpc" | "appex") {
                continue;
            }
            let Some(info) = plist_json(&path.join("Contents/Info.plist")) else {
                continue;
            };
            let Some(id) = info
                .get("CFBundleIdentifier")
                .and_then(|v| v.as_str())
                .filter(|b| valid_bundle_id(b))
            else {
                continue;
            };
            ids.push(id.to_string());
            if sub == "Library/LoginItems" {
                background.push(BackgroundEntry {
                    kind: kind.into(),
                    label: id.to_string(),
                    path: Some(path.to_string_lossy().into_owned()),
                });
            }
        }
    }
    // Privileged helpers and embedded launchd jobs are named by their label.
    for (sub, kind) in [
        ("Library/LaunchServices", "Privileged helper"),
        ("Library/LaunchAgents", "Background item"),
        ("Library/LaunchDaemons", "Background item"),
    ] {
        let Ok(entries) = std::fs::read_dir(contents.join(sub)) else {
            continue;
        };
        for entry in entries.flatten().take(60) {
            let name = entry.file_name().to_string_lossy().into_owned();
            let label = name.strip_suffix(".plist").unwrap_or(&name).to_string();
            if valid_bundle_id(&label) {
                ids.push(label.clone());
                background.push(BackgroundEntry {
                    kind: kind.into(),
                    label,
                    path: Some(entry.path().to_string_lossy().into_owned()),
                });
            }
        }
    }
    if let Some(map) = main_plist
        .and_then(|p| p.get("SMPrivilegedExecutables"))
        .and_then(|v| v.as_object())
    {
        for label in map.keys().filter(|k| valid_bundle_id(k)) {
            ids.push(label.clone());
            background.push(BackgroundEntry {
                kind: "Privileged helper".into(),
                label: label.clone(),
                path: None,
            });
        }
    }
    (ids, background)
}

/// Bundle ids and roots of every installed app except `this`.
fn other_apps(this: &Path) -> (Vec<String>, Vec<PathBuf>) {
    let paths: Vec<PathBuf> = find_apps().into_iter().filter(|p| p != this).collect();
    let chunk = paths.len().div_ceil(8).max(1);
    let mut ids = Vec::new();
    std::thread::scope(|scope| {
        let handles: Vec<_> = paths
            .chunks(chunk)
            .map(|part| {
                scope.spawn(move || {
                    part.iter()
                        .filter_map(|p| bundle_info(p).and_then(|i| i.bundle_id))
                        .map(|b| b.to_lowercase())
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        for handle in handles {
            if let Ok(part) = handle.join() {
                ids.extend(part);
            }
        }
    });
    ids.sort();
    ids.dedup();
    (ids, paths)
}

/// Facts that need only this bundle: its plist, nested helper ids and code signature.
struct BundleFacts {
    main_plist: Option<serde_json::Value>,
    nested: Vec<String>,
    background: Vec<BackgroundEntry>,
    groups: Vec<String>,
    team: Option<String>,
}

fn bundle_facts(root: &Path) -> BundleFacts {
    let main_plist = plist_json(&root.join("Contents/Info.plist"));
    let (nested, background) = harvest_bundle(root, main_plist.as_ref());
    let (groups, team) = signing_info(root);
    BundleFacts {
        main_plist,
        nested,
        background,
        groups,
        team,
    }
}

fn build_identity(
    root: &Path,
    bundle_id: Option<&str>,
    others: Vec<String>,
    facts: BundleFacts,
) -> (Identity, Vec<BackgroundEntry>) {
    let BundleFacts {
        main_plist,
        nested: extra,
        background,
        groups,
        team,
    } = facts;
    let main = bundle_id.map(str::to_lowercase);
    let mut ids: Vec<String> = Vec::new();
    ids.extend(main.clone());
    for id in extra {
        let id = id.to_lowercase();
        // Apple frameworks and other installed apps are never ours.
        if !id.starts_with("com.apple.") && !others.contains(&id) {
            ids.push(id);
        }
    }
    let mut seen = HashSet::new();
    ids.retain(|id| id.contains('.') && seen.insert(id.clone()));

    let mut families: Vec<String> = Vec::new();
    for id in &ids {
        let parts: Vec<&str> = id.split('.').collect();
        // Drop the last part only when three parts remain, so the vendor-wide
        // `com.vendor.` is never produced.
        if parts.len() >= 4 && !id.starts_with("com.apple.") {
            let family = format!("{}.", parts[..parts.len() - 1].join("."));
            if !families.contains(&family) {
                families.push(family);
            }
        }
    }

    let mut names: Vec<String> = Vec::new();
    let file_name = root
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    names.push(file_name);
    for key in ["CFBundleDisplayName", "CFBundleName"] {
        if let Some(name) = main_plist
            .as_ref()
            .and_then(|p| p.get(key))
            .and_then(|v| v.as_str())
        {
            names.push(name.to_string());
        }
    }
    let mut names: Vec<String> = names
        .into_iter()
        .map(|n| n.to_lowercase())
        .filter(|n| n.chars().count() >= 4)
        .collect();
    names.sort();
    names.dedup();

    let team = team.filter(|_| !main.as_deref().is_some_and(|m| m.starts_with("com.apple.")));
    let vendor_sibling = main.as_deref().is_some_and(|m| {
        let take = if m.starts_with("com.apple.") { 3 } else { 2 };
        let parts: Vec<&str> = m.split('.').collect();
        let ns = format!("{}.", parts[..parts.len().min(take)].join("."));
        others.iter().any(|o| o.starts_with(&ns))
    });
    (
        Identity {
            ids,
            main,
            families,
            groups,
            team,
            names,
            others,
            vendor_sibling,
        },
        background,
    )
}

fn library_label(base: &Path, home_library: &Path) -> &'static str {
    if base == home_library {
        "User Library"
    } else {
        "System Library"
    }
}

fn library_items(identity: &Identity) -> Vec<RelatedItem> {
    let user_library = home().join("Library");
    let system_library = PathBuf::from("/Library");
    let mut items = Vec::new();
    let roots: [(&Path, &[&str]); 2] = [
        (user_library.as_path(), USER_DIRS),
        (system_library.as_path(), SYSTEM_DIRS),
    ];
    for (base, dirs) in roots {
        for dir in dirs.iter().copied() {
            let Ok(entries) = std::fs::read_dir(base.join(dir)) else {
                continue;
            };
            for entry in entries.flatten() {
                let file_name = entry.file_name().to_string_lossy().into_owned();
                let Some(hit) = classify(dir, &file_name, identity) else {
                    continue;
                };
                let path = entry.path();
                items.push(RelatedItem {
                    size_bytes: disk_size(&path),
                    admin: needs_admin(&path),
                    path: path.to_string_lossy().into_owned(),
                    label: dir.into(),
                    location: library_label(base, user_library.as_path()).into(),
                    exact: hit.conf == Conf::Exact,
                    confidence: hit.conf.as_str().into(),
                    reason: hit.reason,
                    preselected: hit.selected,
                });
            }
        }
    }
    items
}

fn parse_pkg_info(text: &str) -> (String, String) {
    let mut volume = "/".to_string();
    let mut location = String::new();
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("volume:") {
            volume = v.trim().to_string();
        } else if let Some(l) = line.strip_prefix("location:") {
            location = l.trim().to_string();
        }
    }
    (volume, location)
}

/// Files from installer receipts whose package id belongs to this app. Only
/// paths that still exist, are not shared system paths and are not other
/// installed apps are offered; they are never pre-selected.
fn receipt_items(
    identity: &Identity,
    other_roots: &[PathBuf],
    taken: &[String],
) -> (Vec<RelatedItem>, Vec<String>) {
    let mut command = Command::new("/usr/sbin/pkgutil");
    command.arg("--pkgs");
    let Some(listing) = stdout_of(command, Duration::from_secs(20)) else {
        return (Vec::new(), Vec::new());
    };
    let mut matched: Vec<String> = listing
        .lines()
        .map(str::trim)
        .filter(|pkg| {
            let lower = pkg.to_lowercase();
            let own = longest_match(&lower, &identity.ids);
            own > 0
                && !lower.starts_with("com.apple.")
                && longest_match(&lower, &identity.others) <= own
        })
        .map(str::to_string)
        .collect();
    matched.truncate(12);

    let mut paths: BTreeSet<PathBuf> = BTreeSet::new();
    for pkg in &matched {
        let mut info = Command::new("/usr/sbin/pkgutil");
        info.args(["--pkg-info", pkg]);
        let (volume, location) = stdout_of(info, Duration::from_secs(10))
            .map(|t| parse_pkg_info(&t))
            .unwrap_or_else(|| ("/".into(), String::new()));
        let mut files = Command::new("/usr/sbin/pkgutil");
        files.args(["--files", pkg]);
        let Some(list) = stdout_of(files, Duration::from_secs(30)) else {
            continue;
        };
        let base = Path::new(&volume).join(location.trim_start_matches('/'));
        for file in list.lines().filter(|l| !l.is_empty()) {
            paths.insert(base.join(file.trim_start_matches('/')));
        }
    }

    let owned = |name: &str| {
        let lower = name.to_lowercase();
        longest_match(&lower, &identity.ids) > 0 || identity.names.contains(&lower)
    };
    let user_library = home().join("Library");
    let mut chosen: Vec<PathBuf> = Vec::new();
    for path in paths {
        if chosen.iter().any(|c| path.starts_with(c)) {
            continue;
        }
        if is_shared_system_path(&path) || std::fs::symlink_metadata(&path).is_err() {
            continue;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        // A vendor folder straight under a Library folder holds other
        // products too; offer what the package put inside it instead.
        let under_library = path.parent().is_some_and(|p| {
            is_shared_system_path(p) && (p.starts_with("/Library") || p.starts_with(&user_library))
        });
        if under_library && path.is_dir() && !owned(&name) {
            continue;
        }
        if other_roots
            .iter()
            .any(|o| path.starts_with(o) || o.starts_with(&path))
        {
            continue;
        }
        chosen.push(path);
        if chosen.len() >= MAX_RECEIPT_ITEMS {
            break;
        }
    }
    let items: Vec<RelatedItem> = chosen
        .into_iter()
        .filter(|p| !taken.iter().any(|t| p.starts_with(Path::new(t))))
        .map(|path| RelatedItem {
            size_bytes: disk_size(&path),
            admin: needs_admin(&path),
            label: "Installer receipt".into(),
            location: "Installer receipt".into(),
            exact: false,
            confidence: "receipt".into(),
            reason: "Listed by the app's installer package".into(),
            preselected: false,
            path: path.to_string_lossy().into_owned(),
        })
        .collect();
    (items, matched)
}

/// Launchd jobs currently loaded for this user whose label is the app's.
fn loaded_jobs(identity: &Identity) -> Vec<BackgroundEntry> {
    let mut command = Command::new("/bin/launchctl");
    command.arg("list");
    let Some(listing) = stdout_of(command, Duration::from_secs(8)) else {
        return Vec::new();
    };
    listing
        .lines()
        .skip(1)
        .filter_map(|line| {
            let mut cols = line.split_whitespace();
            let pid = cols.next()?;
            let _status = cols.next()?;
            let label = cols.next()?;
            let lower = label.to_lowercase();
            let own = longest_match(&lower, &identity.ids);
            (own > 0 && longest_match(&lower, &identity.others) <= own).then(|| BackgroundEntry {
                kind: if pid == "-" {
                    "Loaded job".into()
                } else {
                    format!("Loaded job (pid {pid})")
                },
                label: label.to_string(),
                path: None,
            })
        })
        .collect()
}

/// An app path is acceptable only when it is a real (non-symlink) `.app`
/// inside an application folder.
/// The subfolder of /Applications (or ~/Applications) that holds this app,
/// when everything else in it is the vendor's own uninstaller or helper apps
/// (same team id, or named "Uninstall..."), the folder icon file, .DS_Store
/// or .localized. Never the Applications folder itself.
fn vendor_folder(root: &Path, team: Option<String>) -> Option<PathBuf> {
    let folder = root.parent()?;
    if folder.extension().is_some_and(|x| x == "app") {
        return None;
    }
    let dirs = app_dirs();
    if dirs.iter().any(|d| d == folder)
        || !dirs.iter().any(|d| folder.parent() == Some(d.as_path()))
    {
        return None;
    }
    if std::fs::symlink_metadata(folder)
        .ok()?
        .file_type()
        .is_symlink()
    {
        return None;
    }
    for entry in std::fs::read_dir(folder).ok()? {
        let path = entry.ok()?.path();
        if path == root {
            continue;
        }
        let name = path.file_name()?.to_string_lossy().into_owned();
        if matches!(name.as_str(), "Icon\r" | ".DS_Store" | ".localized") {
            continue;
        }
        let is_app = path.extension().is_some_and(|x| x == "app");
        let meta = std::fs::symlink_metadata(&path).ok()?;
        if meta.file_type().is_symlink() || !is_app {
            return None;
        }
        let same_team = team.is_some() && signing_info(&path).1 == team;
        if same_team || name.to_lowercase().contains("uninstall") {
            continue;
        }
        return None;
    }
    Some(folder.to_path_buf())
}

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

/// The bundle row, shown with its size once known.
fn bundle_row(root: &Path, preselected: bool, size_bytes: u64) -> RelatedItem {
    RelatedItem {
        path: root.to_string_lossy().into_owned(),
        label: "Application".into(),
        location: "Application".into(),
        exact: true,
        confidence: "exact".into(),
        reason: "The application bundle".into(),
        admin: needs_admin(root),
        size_bytes,
        preselected,
    }
}

/// One source's share of an app's leftovers, sent as soon as that source finishes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LeftoverPart {
    /// "bundle", "vendor", "background", "library" or "receipts".
    pub source: String,
    pub items: Vec<RelatedItem>,
    pub background: Vec<BackgroundEntry>,
    /// Installer packages that matched (receipts source only).
    pub receipts: Vec<String>,
}

impl LeftoverPart {
    fn new(source: &str) -> Self {
        Self {
            source: source.into(),
            items: Vec::new(),
            background: Vec::new(),
            receipts: Vec::new(),
        }
    }
}

/// Position of each source in the assembled list. The bundle is always first.
fn source_rank(source: &str) -> u8 {
    match source {
        "bundle" => 0,
        "vendor" => 1,
        "library" => 2,
        "receipts" => 3,
        _ => 4,
    }
}

fn confidence_rank(confidence: &str) -> u8 {
    match confidence {
        "exact" => 0,
        "helper" => 1,
        "group" => 2,
        "prefix" => 3,
        "team" => 4,
        _ => 5,
    }
}

/// Runs the leftover sources once the bundle's identity is known. The Library
/// scan, the vendor folder, installer receipts and loaded launchd jobs run
/// concurrently, and each is sent to `emit` as it completes. Nothing is sent
/// when the app is not `eligible` (protected apps are never matched).
fn stream_leftovers(
    root: &Path,
    bundle_id: Option<&str>,
    eligible: bool,
    emit: &(dyn Fn(LeftoverPart) + Sync),
) {
    if !eligible {
        return;
    }
    // Identity first: the code signature and helper ids, alongside the other installed apps.
    let (facts, others) = std::thread::scope(|scope| {
        let facts = scope.spawn(|| bundle_facts(root));
        let others = scope.spawn(|| other_apps(root));
        (join(facts), join(others))
    });
    let team = facts.team.clone();
    let (other_ids, other_roots) = others;
    let (identity, embedded) = build_identity(root, bundle_id, other_ids, facts);
    emit(LeftoverPart {
        background: embedded,
        ..LeftoverPart::new("background")
    });

    std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut part = LeftoverPart::new("vendor");
            if let Some(folder) = vendor_folder(root, team.clone()) {
                part.items.push(RelatedItem {
                    path: folder.to_string_lossy().into_owned(),
                    label: "Application".into(),
                    location: "Application".into(),
                    exact: true,
                    confidence: "exact".into(),
                    reason: "The vendor folder holding only this app and its uninstaller".into(),
                    admin: needs_admin(&folder),
                    size_bytes: disk_size(&folder),
                    preselected: true,
                });
            }
            emit(part);
        });
        scope.spawn(|| {
            let mut found = library_items(&identity);
            found.sort_by(|a, b| {
                confidence_rank(&a.confidence)
                    .cmp(&confidence_rank(&b.confidence))
                    .then(b.size_bytes.cmp(&a.size_bytes))
                    .then(a.path.cmp(&b.path))
            });
            found.dedup_by(|a, b| a.path == b.path);
            let mut part = LeftoverPart::new("library");
            for item in &found {
                if matches!(item.label.as_str(), "LaunchAgents" | "LaunchDaemons") {
                    part.background.push(BackgroundEntry {
                        kind: if item.label == "LaunchAgents" {
                            "Launch agent".into()
                        } else {
                            "Launch daemon".into()
                        },
                        label: Path::new(&item.path)
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        path: Some(item.path.clone()),
                    });
                }
            }
            part.items = found;
            emit(part);
        });
        scope.spawn(|| {
            // Receipt files under the app or a Library item are dropped when the
            // list is assembled; the bundle path alone is excluded here.
            let taken = [root.to_string_lossy().into_owned()];
            let (items, pkgs) = receipt_items(&identity, &other_roots, &taken);
            let mut part = LeftoverPart::new("receipts");
            part.items = items;
            part.receipts = pkgs;
            emit(part);
        });
        scope.spawn(|| {
            let mut part = LeftoverPart::new("background");
            part.background = loaded_jobs(&identity);
            emit(part);
        });
    });
}

/// The app's bundle row, then its leftovers, streamed part by part to `emit`.
/// Callers show the header from the list at once and wait for nothing here.
pub fn app_leftovers(path: &str, emit: &(dyn Fn(LeftoverPart) + Sync)) -> Result<(), String> {
    let root = validate_app_path(path)?;
    let bundle_id = bundle_info(&root).and_then(|info| info.bundle_id);
    let eligible = protected_reason(&root, bundle_id.as_deref()).is_none();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut part = LeftoverPart::new("bundle");
            part.items.push(bundle_row(&root, eligible, disk_size(&root)));
            emit(part);
        });
        stream_leftovers(&root, bundle_id.as_deref(), eligible, emit);
    });
    Ok(())
}

/// The header facts of one app, without sizes or leftovers, for opening an app by path.
pub fn app_summary(path: &str) -> Result<AppEntry, String> {
    let root = validate_app_path(path)?;
    let info = bundle_info(&root);
    let bundle_id = info.as_ref().and_then(|i| i.bundle_id.clone());
    Ok(AppEntry {
        name: root
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        path: root.to_string_lossy().into_owned(),
        protected: protected_reason(&root, bundle_id.as_deref()),
        bundle_id,
        version: info.and_then(|i| i.version),
        size_bytes: 0,
        last_used: None,
        running: running_roots().contains(&root),
    })
}

/// The bundle, its Library leftovers (user and system), installer-receipt
/// files and background items, with sizes and a confidence on each item.
/// Synchronous: the CLI and uninstall use this.
pub fn app_detail(path: &str) -> Result<AppDetail, String> {
    let root = validate_app_path(path)?;
    let app = inspect(&root, &running_roots());
    let eligible = app.protected.is_none();
    let parts = Mutex::new(Vec::<LeftoverPart>::new());
    stream_leftovers(
        &root,
        app.bundle_id.as_deref(),
        eligible,
        &|part: LeftoverPart| {
            lock(&parts).push(part);
        },
    );
    let mut parts: Vec<LeftoverPart> = parts
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    parts.sort_by_key(|part| source_rank(&part.source));

    let mut items = vec![bundle_row(&root, eligible, app.size_bytes)];
    let mut background = Vec::new();
    let mut receipts = Vec::new();
    for part in parts {
        items.extend(part.items);
        background.extend(part.background);
        receipts.extend(part.receipts);
    }
    // Installer files under the app or a Library item are already listed there.
    let mut taken: Vec<PathBuf> = vec![root.clone()];
    taken.extend(
        items
            .iter()
            .filter(|i| i.location == "User Library" || i.location == "System Library")
            .map(|i| PathBuf::from(&i.path)),
    );
    items.retain(|i| {
        i.location != "Installer receipt"
            || !taken
                .iter()
                .any(|t| Path::new(&i.path).starts_with(t))
    });
    let mut seen = HashSet::new();
    background.retain(|b: &BackgroundEntry| seen.insert((b.kind.clone(), b.label.clone())));
    Ok(AppDetail {
        app,
        items,
        background,
        receipts,
    })
}

fn trash_dir() -> PathBuf {
    home().join(".Trash")
}

fn trash_with_finder(path: &Path, limit: Duration) -> Result<(), String> {
    let mut command = Command::new("/usr/bin/osascript");
    command
        .arg("-e")
        .arg("on run argv")
        .arg("-e")
        // Resolve the alias outside Finder's tell block: inside it, `POSIX file`
        // is Finder's own term and fails with -1728.
        .arg("set f to (POSIX file (item 1 of argv)) as alias")
        .arg("-e")
        // Finder waits on the administrator password; AppleScript's default
        // two-minute event timeout would give up first (-1712).
        .arg("with timeout of 600 seconds")
        .arg("-e")
        .arg("tell application \"Finder\" to delete f")
        .arg("-e")
        .arg("end timeout")
        .arg("-e")
        .arg("end run")
        .arg(path);
    run_with_timeout(command, limit).map(|_| ())
}

/// Move several root-owned items to the Trash in one Finder request, so the
/// administrator password is asked once.
fn trash_batch_with_finder(paths: &[&Path], limit: Duration) -> Result<(), String> {
    let mut command = Command::new("/usr/bin/osascript");
    command
        .arg("-e")
        .arg("on run argv")
        .arg("-e")
        .arg("set l to {}")
        .arg("-e")
        .arg("repeat with p in argv")
        .arg("-e")
        // Resolve aliases outside Finder's tell block: inside it, `POSIX file`
        // is Finder's own term and fails with -1728.
        .arg("set end of l to ((POSIX file (contents of p)) as alias)")
        .arg("-e")
        .arg("end repeat")
        .arg("-e")
        // Finder waits on the administrator password; AppleScript's default
        // two-minute event timeout would give up first (-1712).
        .arg("with timeout of 600 seconds")
        .arg("-e")
        .arg("tell application \"Finder\" to delete l")
        .arg("-e")
        .arg("end timeout")
        .arg("-e")
        .arg("end run");
    for p in paths {
        command.arg(p);
    }
    run_with_timeout(command, limit).map(|_| ())
}

/// `pulse-elevate` next to this program or in the app's `Contents/Helpers`.
fn elevate_tool() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors().skip(1).take(6).find_map(|dir| {
        [dir.join("pulse-elevate"), dir.join("Helpers/pulse-elevate")]
            .into_iter()
            .find(|p| p.is_file())
    })
}

/// The notch publishes the privileged helper's state ("enabled" once approved).
fn helper_enabled() -> bool {
    std::fs::read_to_string(home().join("Library/Application Support/Pulse/notch-state.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .is_some_and(|v| v["product"] == "Pulse" && v["helper"] == "enabled")
}

/// Move root-owned items through the privileged helper (no password). `None`
/// when the helper is off, not approved, or fails; otherwise the paths it moved.
fn trash_batch_with_helper(paths: &[&Path]) -> Option<Vec<PathBuf>> {
    if !helper_enabled() {
        return None;
    }
    let mut command = Command::new(elevate_tool()?);
    for p in paths {
        command.arg(p);
    }
    let out = run_with_timeout(command, Duration::from_secs(90)).ok()?;
    let rows = serde_json::from_str::<serde_json::Value>(&out).ok()?;
    let rows = rows["results"].as_array()?;
    Some(
        rows.iter()
            .filter(|r| r["status"] == "moved")
            .filter_map(|r| r["path"].as_str().map(PathBuf::from))
            .collect(),
    )
}

/// Root-owned items: first through the privileged helper when it is approved,
/// then one Finder request for whatever is left (the administrator password is
/// asked once). Each item is verified on its own. A refused batch fails every
/// remaining item with the reason.
fn move_admin_batch_to_trash(paths: &[&Path]) -> Vec<Result<(), String>> {
    if paths.is_empty() {
        return Vec::new();
    }
    let gone = |p: &Path| std::fs::symlink_metadata(p).is_err();
    let mut left: Vec<&Path> = paths.to_vec();
    if trash_batch_with_helper(paths).is_some() {
        left.retain(|p| !gone(p));
    }
    let refused = if left.is_empty() {
        None
    } else {
        trash_batch_with_finder(&left, Duration::from_secs(620)).err()
    };
    paths
        .iter()
        .map(|path| {
            if gone(path) {
                return Ok(());
            }
            Err(match &refused {
                Some(reason) => {
                    let reason = if reason.is_empty() { "No response" } else { reason };
                    format!(
                        "Root-owned: needs administrator approval in Finder, which did not complete ({reason})."
                    )
                }
                None => "Still in its original place after moving to Trash.".into(),
            })
        })
        .collect()
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

/// Move to the Trash through Finder. Root-owned items cannot be renamed by
/// the user, so Finder is the only route: it asks for an administrator
/// password, and if that is refused the item is reported, never removed.
fn move_to_trash(path: &Path) -> Result<(), String> {
    let admin = needs_admin(path);
    let limit = Duration::from_secs(if admin { 620 } else { 60 });
    if let Err(first) = trash_with_finder(path, limit) {
        if admin {
            let reason = if first.is_empty() {
                "No response".to_string()
            } else {
                first
            };
            return Err(format!(
                "Root-owned: needs administrator approval in Finder, which did not complete ({reason})."
            ));
        }
        trash_by_rename(path).map_err(|second| format!("{first}; {second}"))?;
    }
    if std::fs::symlink_metadata(path).is_ok() {
        return Err("Still in its original place after moving to Trash.".into());
    }
    Ok(())
}

fn log_path() -> PathBuf {
    home().join("Library/Application Support/Pulse/apps-activity.json")
}

fn log_activity(app: &AppEntry, result: &UninstallResult) -> Option<String> {
    let path = log_path();
    let mut entries: Vec<serde_json::Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let id = format!(
        "{time}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    );
    entries.push(serde_json::json!({
        "id": id,
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
    let dir = path.parent()?;
    std::fs::create_dir_all(dir).ok()?;
    let temp = path.with_extension("json.tmp");
    let write = std::fs::File::create(&temp)
        .and_then(|mut f| f.write_all(&serde_json::to_vec_pretty(&entries).unwrap_or_default()));
    if write.is_ok() && std::fs::rename(temp, path).is_ok() {
        return Some(id);
    }
    None
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

    // The vendor folder (when selected) is trashed whole, in place of the app.
    let folder: Option<String> = fresh
        .items
        .iter()
        .map(|i| i.path.clone())
        .find(|p| *p != fresh.app.path && Path::new(&fresh.app.path).starts_with(p))
        .filter(|p| items.contains(p));
    let bundle = folder.clone().unwrap_or_else(|| fresh.app.path.clone());
    // Bundle first: if it cannot be moved, its data stays where it is.
    let mut ordered: Vec<&String> = items.iter().filter(|p| **p == bundle).collect();
    ordered.extend(items.iter().filter(|p| **p != bundle));
    let mut result = UninstallResult {
        moved: Vec::new(),
        failed: Vec::new(),
        moved_bytes: 0,
        activity_id: None,
    };
    fn record(result: &mut UninstallResult, item: &str, outcome: Result<u64, String>) -> bool {
        match outcome {
            Ok(bytes) => {
                result.moved_bytes += bytes;
                result.moved.push(MovedItem {
                    path: item.to_string(),
                    bytes,
                });
                true
            }
            Err(error) => {
                result.failed.push(FailedItem {
                    path: item.to_string(),
                    error,
                });
                false
            }
        }
    }
    let mut bundle_failed = false;
    let mut plain: Vec<(&String, u64)> = Vec::new();
    let mut admin: Vec<(&String, u64)> = Vec::new();
    for item in ordered {
        if folder.is_some() && *item == fresh.app.path {
            continue; // goes with its folder
        }
        match revalidate(&fresh, item) {
            Ok(bytes) if needs_admin(Path::new(item)) => admin.push((item, bytes)),
            Ok(bytes) => plain.push((item, bytes)),
            Err(error) => {
                bundle_failed |= *item == bundle;
                record(&mut result, item, Err(error));
            }
        }
    }
    let run_admin =
        |result: &mut UninstallResult, admin: &[(&String, u64)], bundle_failed: &mut bool| {
            let paths: Vec<&Path> = admin.iter().map(|(p, _)| Path::new(p.as_str())).collect();
            for ((item, bytes), outcome) in admin.iter().zip(move_admin_batch_to_trash(&paths)) {
                let ok = record(result, item, outcome.map(|_| *bytes));
                if !ok && **item == bundle {
                    *bundle_failed = true;
                }
            }
        };
    let skipped = "Skipped because the app itself could not be moved.";
    let admin_first = admin.iter().any(|(p, _)| **p == bundle);
    if admin_first && !bundle_failed {
        run_admin(&mut result, &admin, &mut bundle_failed);
    }
    for (item, bytes) in &plain {
        if bundle_failed && **item != bundle {
            record(&mut result, item, Err(skipped.into()));
            continue;
        }
        let outcome = move_to_trash(Path::new(item.as_str())).map(|_| *bytes);
        if outcome.is_err() && **item == bundle {
            bundle_failed = true;
        }
        record(&mut result, item, outcome);
    }
    if !admin_first {
        if bundle_failed {
            for (item, _) in &admin {
                record(&mut result, item, Err(skipped.into()));
            }
        } else {
            run_admin(&mut result, &admin, &mut bundle_failed);
        }
    }
    if folder.is_some()
        && result
            .moved
            .iter()
            .any(|m| Some(&m.path) == folder.as_ref())
        && items.contains(&fresh.app.path)
    {
        result.moved.push(MovedItem {
            path: fresh.app.path.clone(),
            bytes: 0,
        });
    }
    result.activity_id = log_activity(&fresh.app, &result);
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
    if fresh.app.path != item && is_shared_system_path(path) {
        return Err("Shared system location; left in place.".into());
    }
    let meta = std::fs::symlink_metadata(path).map_err(|_| "Already gone.".to_string())?;
    if meta.file_type().is_symlink() {
        return Err("Is a symbolic link; left in place.".into());
    }
    Ok(disk_size(path))
}

// ---------------------------------------------------------------------------
// Icons: a 64-pixel PNG per app, rendered with sips and cached.
// ---------------------------------------------------------------------------

const ICON_PIXELS: &str = "64";

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Modification time of the bundle folder, in nanoseconds since the epoch.
fn bundle_stamp(root: &Path) -> Option<u128> {
    let modified = std::fs::metadata(root).ok()?.modified().ok()?;
    modified
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_nanos())
}

/// The bundle's icon file: CFBundleIconFile (".icns" optional), else AppIcon.icns.
fn icon_file(root: &Path) -> Option<PathBuf> {
    let resources = root.join("Contents/Resources");
    let plist = plist_json(&root.join("Contents/Info.plist"));
    let named = plist
        .as_ref()
        .and_then(|p| p.get("CFBundleIconFile"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|n| !n.is_empty() && !n.contains('/') && !n.contains(".."));
    let mut names: Vec<String> = Vec::new();
    if let Some(name) = named {
        names.push(if name.ends_with(".icns") {
            name.to_string()
        } else {
            format!("{name}.icns")
        });
    }
    names.push("AppIcon.icns".to_string());
    names
        .iter()
        .map(|name| resources.join(name))
        .find(|p| p.is_file())
}

/// Renders the bundle's icon to a 64-pixel PNG at `png`, through a temporary file.
fn render_icon(root: &Path, dir: &Path, png: &Path) -> bool {
    let Some(icns) = icon_file(root) else {
        return false;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let tmp = png.with_extension("tmp.png");
    let mut command = Command::new("/usr/bin/sips");
    command
        .args(["-s", "format", "png", "--resampleWidth", ICON_PIXELS])
        .arg(&icns)
        .arg("--out")
        .arg(&tmp);
    let ok = run_captured(command, Duration::from_secs(20), None)
        .map(|done| done.ok)
        .unwrap_or(false);
    if ok && std::fs::rename(&tmp, png).is_ok() {
        return true;
    }
    let _ = std::fs::remove_file(&tmp);
    false
}

/// Removes cached icons of one app that do not belong to the `keep` entry.
fn prune_icons(dir: &Path, prefix: &str, keep: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(prefix) && !name.starts_with(keep) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Path of the cached PNG icon for an app. With `generate`, a missing icon is
/// rendered now; a failure is remembered until the bundle changes. Without
/// it, only the cache is read. None means no icon (the caller shows a fallback).
pub fn app_icon(path: &str, generate: bool) -> Option<PathBuf> {
    let root = validate_app_path(path).ok()?;
    let stamp = bundle_stamp(&root)?;
    let dir = support_dir().join("icons");
    let prefix = format!("{:016x}-", fnv1a(root.to_string_lossy().as_bytes()));
    let base = format!("{prefix}{stamp}");
    let png = dir.join(format!("{base}.png"));
    if png.is_file() {
        return Some(png);
    }
    if !generate || dir.join(format!("{base}.none")).exists() {
        return None;
    }
    if render_icon(&root, &dir, &png) {
        prune_icons(&dir, &prefix, &format!("{base}."));
        return Some(png);
    }
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join(format!("{base}.none")), b"");
    prune_icons(&dir, &prefix, &format!("{base}."));
    None
}

// ---------------------------------------------------------------------------
// Updates: Sparkle appcasts, Homebrew casks and App Store apps. Read-only
// checks; the only actions hand off to the app, the App Store or Homebrew.
// ---------------------------------------------------------------------------

const UPDATE_TTL_SECS: i64 = 6 * 60 * 60;
const UPDATE_WORKERS: usize = 4;
const APPCAST_MAX_BYTES: u64 = 2 * 1024 * 1024;
const APPCAST_MAX_ITEMS: usize = 200;
const UPDATES_CACHE_SCHEMA: u32 = 1;
const STORE_PREFIX: &str = "macappstore://apps.apple.com/app/id";

/// The update state of one installed app.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppUpdate {
    pub path: String,
    pub name: String,
    pub bundle_id: Option<String>,
    pub installed_version: Option<String>,
    /// "app_store", "homebrew", "sparkle" or "none".
    pub source: String,
    /// "available", "current", "app_store", "unknown" or "unavailable".
    pub state: String,
    pub latest_version: Option<String>,
    /// Homebrew cask token, when Homebrew installed this app.
    pub cask: Option<String>,
    /// macappstore:// link, when the App Store id is known.
    pub store_url: Option<String>,
    /// Plain-language reason when no comparison was possible.
    pub reason: Option<String>,
    /// Unix seconds when this row was checked.
    pub checked_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UpdateReport {
    /// Unix seconds of the newest check; None before any check.
    pub checked_at: Option<i64>,
    pub apps: Vec<AppUpdate>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct BrewSnapshot {
    checked_at: i64,
    /// App bundle path -> cask token. A path claimed by two casks is left out.
    owners: BTreeMap<String, String>,
    /// Cask token -> newest version, for casks Homebrew reports as outdated.
    outdated: BTreeMap<String, String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct UpdatesCacheFile {
    #[serde(default)]
    schema: u32,
    #[serde(default)]
    brew: Option<BrewSnapshot>,
    /// Keyed by app path.
    #[serde(default)]
    apps: BTreeMap<String, AppUpdate>,
}

fn updates_cache_path() -> PathBuf {
    support_dir().join("updates-cache.json")
}

fn read_updates_cache() -> UpdatesCacheFile {
    std::fs::read(updates_cache_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write_updates_cache(cache: &UpdatesCacheFile) {
    let _ = write_json_atomic(&updates_cache_path(), cache);
}

/// The saved update results, shown at once while a fresh check runs.
pub fn cached_updates() -> UpdateReport {
    let cache = read_updates_cache();
    let checked_at = cache.apps.values().map(|a| a.checked_at).max();
    let mut apps: Vec<AppUpdate> = cache.apps.into_values().collect();
    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    UpdateReport { checked_at, apps }
}

/// Compares version strings part by part. Numeric parts compare by value; text
/// compares as text; a missing part counts as zero.
fn compare_versions(left: &str, right: &str) -> std::cmp::Ordering {
    let split = |text: &str| -> Vec<String> {
        text.split(|c: char| !c.is_alphanumeric())
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect()
    };
    let (a, b) = (split(left), split(right));
    for index in 0..a.len().max(b.len()) {
        let x = a.get(index).map(String::as_str).unwrap_or("0");
        let y = b.get(index).map(String::as_str).unwrap_or("0");
        let order = match (x.parse::<u128>(), y.parse::<u128>()) {
            (Ok(p), Ok(q)) => p.cmp(&q),
            _ => x.cmp(y),
        };
        if order != std::cmp::Ordering::Equal {
            return order;
        }
    }
    std::cmp::Ordering::Equal
}

fn version_is_newer(candidate: &str, installed: &str) -> bool {
    compare_versions(candidate, installed) == std::cmp::Ordering::Greater
}

/// The macOS version, read once (for appcast items that need a newer system).
fn macos_version() -> Option<String> {
    static VERSION: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    VERSION
        .get_or_init(|| {
            let mut command = Command::new("/usr/bin/sw_vers");
            command.arg("-productVersion");
            stdout_of(command, Duration::from_secs(5))
        })
        .clone()
}

/// Only https URLs with a host and no credentials.
fn https_url(raw: &str) -> bool {
    let Some(rest) = raw.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    raw.len() <= 2048
        && !authority.is_empty()
        && !authority.contains('@')
        && !raw.chars().any(|c| c.is_whitespace() || c == '\0')
}

/// Reads a feed with curl: https only (redirects included), 5 seconds in
/// total, no curl config file, and a size cap. Only the body is read.
fn fetch_appcast(url: &str) -> Result<String, String> {
    let mut command = Command::new("/usr/bin/curl");
    command
        .args([
            "-q",
            "-sS",
            "-f",
            "-L",
            "--max-redirs",
            "3",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--connect-timeout",
            "5",
            "--max-time",
            "5",
            "--max-filesize",
        ])
        .arg(APPCAST_MAX_BYTES.to_string())
        .arg(url);
    let done = run_captured(command, Duration::from_secs(8), None)
        .map_err(|_| "Feed did not answer in time.".to_string())?;
    if !done.ok {
        let reason = done.stderr.lines().next().unwrap_or("request failed").trim();
        return Err(format!("Feed could not be read: {reason}"));
    }
    String::from_utf8(done.stdout).map_err(|_| "Feed is not UTF-8 text.".to_string())
}

/// One appcast item this Mac could be offered.
struct Candidate {
    version: Option<String>,
    short: Option<String>,
}

fn clean_version(raw: &str) -> Option<String> {
    let text = raw.trim();
    (!text.is_empty() && text.len() <= 128 && !text.contains('<')).then(|| text.to_string())
}

/// Text of a plain element such as `<sparkle:version>1.2</sparkle:version>`.
fn xml_text(block: &str, element: &str) -> Option<String> {
    let open = format!("<{element}>");
    let close = format!("</{element}>");
    let start = block.find(&open)? + open.len();
    let end = start + block[start..].find(&close)?;
    clean_version(&block[start..end])
}

/// Value of an attribute such as ` sparkle:version="1.2"`, as enclosures carry them.
fn xml_attribute(block: &str, name: &str) -> Option<String> {
    let key = format!(" {name}=\"");
    let start = block.find(&key)? + key.len();
    let end = start + block[start..].find('"')?;
    clean_version(&block[start..end])
}

/// Items this Mac can use. Other channels and items that need a newer macOS
/// are left out. None when the feed is not an appcast, or has a DTD or entities.
fn parse_appcast(xml: &str, macos: Option<&str>) -> Option<Vec<Candidate>> {
    if xml.contains("<!DOCTYPE") || xml.contains("<!ENTITY") || !xml.contains("<rss") {
        return None;
    }
    let mut candidates = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<item") {
        let after = &rest[start..];
        let Some(end) = after.find("</item>") else {
            break;
        };
        let block = &after[..end];
        rest = &after[end + "</item>".len()..];
        // "<item" must be the whole tag name, not the start of "<items".
        let is_item = matches!(
            block.as_bytes().get(5).copied(),
            Some(b'>' | b' ' | b'\n' | b'\t' | b'\r')
        );
        if !is_item {
            continue;
        }
        if candidates.len() >= APPCAST_MAX_ITEMS {
            break;
        }
        if block.contains("<sparkle:channel") {
            continue;
        }
        if let (Some(minimum), Some(current)) =
            (xml_text(block, "sparkle:minimumSystemVersion"), macos)
            && compare_versions(&minimum, current) == std::cmp::Ordering::Greater
        {
            continue;
        }
        let version = xml_text(block, "sparkle:version")
            .or_else(|| xml_attribute(block, "sparkle:version"));
        let short = xml_text(block, "sparkle:shortVersionString")
            .or_else(|| xml_attribute(block, "sparkle:shortVersionString"));
        if version.is_some() || short.is_some() {
            candidates.push(Candidate { version, short });
        }
    }
    Some(candidates)
}

fn candidate_key(candidate: &Candidate) -> &str {
    candidate
        .version
        .as_deref()
        .or(candidate.short.as_deref())
        .unwrap_or("")
}

fn newest_candidate(candidates: Vec<Candidate>) -> Option<Candidate> {
    candidates
        .into_iter()
        .max_by(|a, b| compare_versions(candidate_key(a), candidate_key(b)))
}

/// Whether the candidate is newer than the installed bundle. Build numbers
/// compare when both sides have one, otherwise marketing versions. None when
/// neither pair is available.
fn candidate_newer(candidate: &Candidate, short: Option<&str>, build: Option<&str>) -> Option<bool> {
    if let (Some(version), Some(installed)) = (candidate.version.as_deref(), build) {
        return Some(version_is_newer(version, installed));
    }
    if let (Some(marketing), Some(installed)) = (candidate.short.as_deref(), short) {
        return Some(version_is_newer(marketing, installed));
    }
    None
}

/// The macappstore:// link for an App Store app, from its Spotlight adam id.
fn store_url(root: &Path) -> Option<String> {
    let mut command = Command::new("/usr/bin/mdls");
    command
        .args(["-raw", "-name", "kMDItemAppStoreAdamID"])
        .arg(root);
    let raw = stdout_of(command, Duration::from_secs(5))?;
    let id = raw.trim();
    (!id.is_empty() && id.chars().all(|c| c.is_ascii_digit()))
        .then(|| format!("{}{}", STORE_PREFIX, id))
}

/// Homebrew from /opt/homebrew or the PATH, nothing else.
fn brew_path() -> Option<PathBuf> {
    let mut candidates = vec![PathBuf::from("/opt/homebrew/bin/brew")];
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|dir| dir.join("brew")));
    }
    candidates.into_iter().find(|candidate| candidate.is_file())
}

fn brew_command(brew: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(brew);
    command
        .args(args)
        .env("HOMEBREW_NO_AUTO_UPDATE", "1")
        .env("HOMEBREW_NO_ANALYTICS", "1")
        .env("HOMEBREW_NO_ENV_HINTS", "1")
        .env("HOMEBREW_NO_INSTALL_CLEANUP", "1");
    command
}

fn brew_json(brew: &Path, args: &[&str], limit: Duration) -> Option<serde_json::Value> {
    let done = run_captured(brew_command(brew, args), limit, None).ok()?;
    if !done.ok {
        return None;
    }
    serde_json::from_slice(&done.stdout).ok()
}

fn json_array<'a>(value: &'a serde_json::Value, key: &str) -> &'a [serde_json::Value] {
    value
        .get(key)
        .and_then(|v| v.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn valid_cask_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 256
        && !token.starts_with('-')
        && !token.contains("..")
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '+' | '@' | '/'))
}

/// Bundle paths a cask's app artifact can install to: absolute as written,
/// otherwise under the two Applications folders.
fn app_candidates(name: &str) -> Vec<String> {
    if name.starts_with('/') {
        return if name.ends_with(".app") {
            vec![name.trim_end_matches('/').to_string()]
        } else {
            Vec::new()
        };
    }
    if !name.ends_with(".app") || name.contains('/') {
        return Vec::new();
    }
    vec![
        format!("/Applications/{name}"),
        format!("{}/Applications/{name}", home().to_string_lossy()),
    ]
}

/// Homebrew's installed casks and which of them are outdated. Reads only, and
/// uses the cached taps (no metadata refresh). None when Homebrew is absent or fails.
fn brew_snapshot() -> Option<BrewSnapshot> {
    let brew = brew_path()?;
    let outdated_json = brew_json(
        &brew,
        &["outdated", "--cask", "--greedy", "--json=v2"],
        Duration::from_secs(120),
    )?;
    let mut outdated = BTreeMap::new();
    for cask in json_array(&outdated_json, "casks") {
        let token = cask
            .get("token")
            .or_else(|| cask.get("name"))
            .and_then(|v| v.as_str());
        let latest = cask.get("current_version").and_then(|v| v.as_str());
        let pinned = cask.get("pinned").and_then(|v| v.as_bool()).unwrap_or(false);
        if let (Some(token), Some(latest), false) = (token, latest, pinned)
            && valid_cask_token(token)
            && !latest.trim().is_empty()
        {
            outdated.insert(token.to_string(), latest.trim().to_string());
        }
    }

    let installed = brew_json(
        &brew,
        &["info", "--json=v2", "--installed"],
        Duration::from_secs(120),
    )?;
    let mut owners: BTreeMap<String, String> = BTreeMap::new();
    for cask in json_array(&installed, "casks") {
        let Some(token) = cask.get("token").and_then(|v| v.as_str()) else {
            continue;
        };
        if !valid_cask_token(token) {
            continue;
        }
        for artifact in json_array(cask, "artifacts") {
            for app in json_array(artifact, "app") {
                let name = app
                    .as_str()
                    .or_else(|| app.get("target").and_then(|v| v.as_str()));
                let Some(name) = name else {
                    continue;
                };
                for path in app_candidates(name) {
                    let conflict = owners.get(&path).is_some_and(|existing| existing != token);
                    let entry = if conflict {
                        String::new()
                    } else {
                        token.to_string()
                    };
                    owners.insert(path, entry);
                }
            }
        }
    }
    owners.retain(|_, token| !token.is_empty());
    Some(BrewSnapshot {
        checked_at: now_epoch(),
        owners,
        outdated,
    })
}

/// Checks one app against the evidence available for it.
fn check_app(
    root: &Path,
    brew: Option<&BrewSnapshot>,
    previous: Option<&AppUpdate>,
    now: i64,
) -> AppUpdate {
    let plist = plist_json(&root.join("Contents/Info.plist")).unwrap_or(serde_json::Value::Null);
    let text = |key: &str| -> Option<String> {
        plist
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let short = text("CFBundleShortVersionString");
    let build = text("CFBundleVersion");
    let installed = short.clone().or_else(|| build.clone());
    let path = root.to_string_lossy().into_owned();
    let mut row = AppUpdate {
        path: path.clone(),
        name: root
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        bundle_id: text("CFBundleIdentifier").filter(|id| valid_bundle_id(id)),
        installed_version: installed.clone(),
        source: "none".into(),
        state: "unknown".into(),
        latest_version: None,
        cask: None,
        store_url: None,
        reason: None,
        checked_at: now,
    };

    if root.join("Contents/_MASReceipt/receipt").exists() {
        row.source = "app_store".into();
        row.state = "app_store".into();
        row.store_url = store_url(root);
        row.reason = Some("App Store apps update through the App Store.".into());
        return row;
    }

    if let Some(snapshot) = brew
        && let Some(token) = snapshot.owners.get(&path)
    {
        row.source = "homebrew".into();
        row.cask = Some(token.clone());
        match snapshot.outdated.get(token) {
            Some(latest) => {
                row.state = "available".into();
                row.latest_version = Some(latest.clone());
            }
            None => row.state = "current".into(),
        }
        return row;
    }

    let Some(feed) = text("SUFeedURL") else {
        row.reason = Some("This app declares no update feed.".into());
        return row;
    };
    row.source = "sparkle".into();
    if let Some(prior) = previous
        && prior.source == "sparkle"
        && prior.installed_version == installed
        && prior.state != "unavailable"
        && now - prior.checked_at < UPDATE_TTL_SECS
    {
        return prior.clone();
    }
    if !https_url(&feed) {
        row.state = "unknown".into();
        row.reason = Some("The update feed is not an HTTPS address.".into());
        return row;
    }
    let xml = match fetch_appcast(&feed) {
        Ok(xml) => xml,
        Err(reason) => {
            row.state = "unavailable".into();
            row.reason = Some(reason);
            return row;
        }
    };
    let Some(candidates) = parse_appcast(&xml, macos_version().as_deref()) else {
        row.state = "unknown".into();
        row.reason = Some("The feed is not a Sparkle appcast Pulse can read.".into());
        return row;
    };
    let Some(newest) = newest_candidate(candidates) else {
        row.state = "unknown".into();
        row.reason = Some("The feed lists no version for this Mac.".into());
        return row;
    };
    row.latest_version = newest.short.clone().or_else(|| newest.version.clone());
    match candidate_newer(&newest, short.as_deref(), build.as_deref()) {
        Some(true) => row.state = "available".into(),
        Some(false) => row.state = "current".into(),
        None => {
            row.state = "unknown".into();
            row.reason = Some("The installed and feed versions cannot be compared.".into());
        }
    }
    row
}

/// Checks every installed app for an update. Each row goes to `on_row` as it
/// is known. Sparkle results and the Homebrew list are reused for six hours
/// unless `force`. Only appcast XML and Homebrew's listings are read; nothing
/// is downloaded for installation and nothing is installed here.
pub fn check_updates(force: bool, on_row: &(dyn Fn(&AppUpdate) + Sync)) -> UpdateReport {
    let now = now_epoch();
    let cached = read_updates_cache();
    let brew = match cached.brew {
        Some(snapshot) if !force && now - snapshot.checked_at < UPDATE_TTL_SECS => Some(snapshot),
        _ => brew_snapshot(),
    };
    let previous = cached.apps;
    let paths = find_apps();
    let next = AtomicUsize::new(0);
    let rows = Mutex::new(Vec::<AppUpdate>::with_capacity(paths.len()));
    std::thread::scope(|scope| {
        for _ in 0..UPDATE_WORKERS.min(paths.len()) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, AtomicOrdering::Relaxed);
                    let Some(root) = paths.get(index) else {
                        break;
                    };
                    let key = root.to_string_lossy().into_owned();
                    let prior = if force { None } else { previous.get(&key) };
                    let row = check_app(root, brew.as_ref(), prior, now);
                    on_row(&row);
                    lock(&rows).push(row);
                }
            });
        }
    });
    let mut apps: Vec<AppUpdate> = rows
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    apps.sort_by(|a: &AppUpdate, b: &AppUpdate| {
        a.name.to_lowercase().cmp(&b.name.to_lowercase())
    });
    let cache = UpdatesCacheFile {
        schema: UPDATES_CACHE_SCHEMA,
        brew,
        apps: apps
            .iter()
            .map(|row| (row.path.clone(), row.clone()))
            .collect(),
    };
    write_updates_cache(&cache);
    UpdateReport {
        checked_at: Some(now),
        apps,
    }
}

/// What the Update button does for one app.
pub enum UpdateAction {
    /// `brew upgrade --cask <token>`, run by the caller in the background.
    Homebrew { cask: String },
    /// Open the app so that it offers its own update.
    Sparkle,
    /// Open the App Store, at the app's page when its id is known.
    AppStore { url: Option<String> },
}

/// Decides the update action for one app from the same evidence the check uses.
pub fn update_action(path: &str) -> Result<UpdateAction, String> {
    let root = validate_app_path(path)?;
    if root.join("Contents/_MASReceipt/receipt").exists() {
        return Ok(UpdateAction::AppStore {
            url: store_url(&root),
        });
    }
    let key = root.to_string_lossy().into_owned();
    let cached = read_updates_cache();
    let brew = match cached.brew {
        Some(snapshot) if now_epoch() - snapshot.checked_at < UPDATE_TTL_SECS => Some(snapshot),
        _ => brew_snapshot(),
    };
    if let Some(snapshot) = brew.as_ref()
        && let Some(token) = snapshot.owners.get(&key)
    {
        return if snapshot.outdated.contains_key(token) {
            Ok(UpdateAction::Homebrew {
                cask: token.clone(),
            })
        } else {
            Err("Homebrew reports this app is already up to date.".into())
        };
    }
    let plist = plist_json(&root.join("Contents/Info.plist"));
    let has_feed = plist
        .as_ref()
        .and_then(|p| p.get("SUFeedURL"))
        .and_then(|v| v.as_str())
        .is_some_and(|feed| https_url(feed.trim()));
    if has_feed {
        Ok(UpdateAction::Sparkle)
    } else {
        Err("This app has no update source Pulse can use.".into())
    }
}

fn tail(text: &str) -> String {
    let text = text.trim();
    let count = text.chars().count();
    if count <= 1500 {
        text.to_string()
    } else {
        text.chars().skip(count - 1500).collect()
    }
}

fn forget_brew_snapshot() {
    let mut cache = read_updates_cache();
    cache.brew = None;
    write_updates_cache(&cache);
}

/// Runs `brew upgrade --cask <token>`. Homebrew downloads and installs; Pulse
/// starts it and reports the outcome. The outdated list is then forgotten.
pub fn homebrew_upgrade(token: &str) -> Result<String, String> {
    if !valid_cask_token(token) {
        return Err("Unrecognised cask name.".into());
    }
    let brew = brew_path().ok_or_else(|| "Homebrew is not available.".to_string())?;
    let done = run_captured(
        brew_command(&brew, &["upgrade", "--cask", token]),
        Duration::from_secs(1800),
        None,
    )?;
    forget_brew_snapshot();
    let mut text = String::from_utf8_lossy(&done.stdout).into_owned();
    if !done.stderr.is_empty() {
        text.push('\n');
        text.push_str(&done.stderr);
    }
    let message = tail(&text);
    if done.ok { Ok(message) } else { Err(message) }
}

/// Opens an app so that it can offer its own update.
pub fn open_app(path: &str) -> Result<(), String> {
    let root = validate_app_path(path)?;
    let mut command = Command::new("/usr/bin/open");
    command.arg(&root);
    run_with_timeout(command, Duration::from_secs(10)).map(|_| ())
}

/// Opens the App Store at one app's page, or at the App Store itself.
pub fn open_store(url: Option<&str>) -> Result<(), String> {
    let mut command = Command::new("/usr/bin/open");
    match url {
        Some(url) => match url.strip_prefix(STORE_PREFIX) {
            Some(id) if !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) => {
                command.arg(url);
            }
            _ => return Err("Unrecognised App Store link.".into()),
        },
        None => {
            command.args(["-a", "App Store"]);
        }
    }
    run_with_timeout(command, Duration::from_secs(10)).map(|_| ())
}
