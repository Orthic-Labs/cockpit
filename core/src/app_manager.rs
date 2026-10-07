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

use std::collections::{BTreeSet, HashSet};
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
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

fn build_identity(
    root: &Path,
    bundle_id: Option<&str>,
    others: Vec<String>,
) -> (Identity, Vec<BackgroundEntry>) {
    let main_plist = plist_json(&root.join("Contents/Info.plist"));
    let main = bundle_id.map(str::to_lowercase);
    let (extra, background) = harvest_bundle(root, main_plist.as_ref());
    let (groups, team) = signing_info(root);
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
fn vendor_folder(root: &Path) -> Option<PathBuf> {
    let folder = root.parent()?;
    if folder.extension().is_some_and(|x| x == "app") {
        return None;
    }
    let dirs = app_dirs();
    if dirs.iter().any(|d| d == folder) || !dirs.iter().any(|d| folder.parent() == Some(d.as_path()))
    {
        return None;
    }
    if std::fs::symlink_metadata(folder).ok()?.file_type().is_symlink() {
        return None;
    }
    let team = signing_info(root).1;
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

/// The bundle, its Library leftovers (user and system), installer-receipt
/// files and background items, with sizes and a confidence on each item.
pub fn app_detail(path: &str) -> Result<AppDetail, String> {
    let root = validate_app_path(path)?;
    let app = inspect(&root, &running_roots());
    let mut items = vec![RelatedItem {
        path: app.path.clone(),
        label: "Application".into(),
        location: "Application".into(),
        exact: true,
        confidence: "exact".into(),
        reason: "The application bundle".into(),
        admin: needs_admin(&root),
        size_bytes: app.size_bytes,
        preselected: app.protected.is_none(),
    }];
    let mut background = Vec::new();
    let mut receipts = Vec::new();
    if app.protected.is_none()
        && let Some(folder) = vendor_folder(&root)
    {
        items.push(RelatedItem {
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
    if app.protected.is_none() {
        let (others, other_roots) = other_apps(&root);
        let (identity, embedded) = build_identity(&root, app.bundle_id.as_deref(), others);
        let mut found = library_items(&identity);
        found.sort_by(|a, b| {
            let rank = |i: &RelatedItem| match i.confidence.as_str() {
                "exact" => 0,
                "helper" => 1,
                "group" => 2,
                "prefix" => 3,
                "team" => 4,
                _ => 5,
            };
            rank(a)
                .cmp(&rank(b))
                .then(b.size_bytes.cmp(&a.size_bytes))
                .then(a.path.cmp(&b.path))
        });
        found.dedup_by(|a, b| a.path == b.path);
        background.extend(embedded);
        for item in &found {
            if matches!(item.label.as_str(), "LaunchAgents" | "LaunchDaemons") {
                background.push(BackgroundEntry {
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
        background.extend(loaded_jobs(&identity));
        let mut taken: Vec<String> = vec![app.path.clone()];
        taken.extend(found.iter().map(|i| i.path.clone()));
        let (receipt_files, pkgs) = receipt_items(&identity, &other_roots, &taken);
        receipts = pkgs;
        items.extend(found);
        items.extend(receipt_files);
        let mut seen = HashSet::new();
        background.retain(|b| seen.insert((b.kind.clone(), b.label.clone())));
    }
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

/// `cockpit-elevate` next to this program or in the app's `Contents/Helpers`.
fn elevate_tool() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors().skip(1).take(6).find_map(|dir| {
        [dir.join("cockpit-elevate"), dir.join("Helpers/cockpit-elevate")]
            .into_iter()
            .find(|p| p.is_file())
    })
}

/// The notch publishes the privileged helper's state ("enabled" once approved).
fn helper_enabled() -> bool {
    std::fs::read_to_string(home().join("Library/Application Support/Cockpit/notch-state.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .is_some_and(|v| v["helper"] == "enabled")
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
    home().join("Library/Application Support/Cockpit/apps-activity.json")
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
    let run_admin = |result: &mut UninstallResult,
                         admin: &[(&String, u64)],
                         bundle_failed: &mut bool| {
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
    if folder.is_some() && result.moved.iter().any(|m| Some(&m.path) == folder.as_ref())
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
