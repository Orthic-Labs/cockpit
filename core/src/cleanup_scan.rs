//! Cleanup findings and the Move-to-Trash flow for the hub.
//!
//! Rules live in `rules/cleanup.json` (data, not code). `scan` expands each
//! rule's path patterns on disk, measures what it finds and decides whether
//! each item may be moved. `apply` re-checks every item immediately before
//! the move and records what happened in an activity log so it can be put
//! back. Nothing here deletes anything: the only effect is a move to Trash
//! (supplied by the caller) and, for Restore, a move back.
//!
//! Unknown is never eligible: an item whose owner cannot be determined, or
//! whose owner is running, is reported but not offered.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub const PACK_JSON: &str = include_str!("../../rules/cleanup.json");
pub const ACTIVITY_SCHEMA_VERSION: u32 = 1;
const MAX_ENTRIES_PER_ITEM: usize = 400_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleRisk {
    /// Regenerates on its own; preselected when eligible.
    Safe,
    /// Needs a look; never preselected.
    Review,
    /// Explanation only; never eligible.
    Info,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LivenessCheck {
    /// In use when any listed owner process is running.
    Owners,
    /// Owners, plus an owner guessed from the item's own name.
    Name,
    /// Not process-bound; age is the only check.
    Age,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CleanupRule {
    pub id: String,
    pub name: String,
    pub category: String,
    pub risk: RuleRisk,
    pub paths: Vec<String>,
    #[serde(default)]
    pub exclude_names: Vec<String>,
    pub liveness: LivenessCheck,
    #[serde(default)]
    pub owners: Vec<String>,
    #[serde(default)]
    pub min_age_days: Option<u64>,
    #[serde(default)]
    pub keep_newest: usize,
    pub reason: String,
    pub action: String,
    /// Build outputs sit inside projects, so no fixed path names them. A
    /// rule with `discover` finds directories by name and project marker.
    #[serde(default)]
    pub discover: Option<Discover>,
}

/// Find directories called `dir` next to one of the `markers` (for example
/// `node_modules` beside `package.json`).
///
/// The idea of treating project build outputs as findings comes from Petal's
/// `findings.rs` (MIT, https://github.com/henrydennis/petal): its walk stays
/// out of hidden folders, `Library`, `Applications` and app bundles because
/// those `node_modules` belong to apps and cannot be reinstalled. This is a
/// fresh implementation of that rule, not a copy of the code.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Discover {
    pub dir: String,
    pub markers: Vec<String>,
}

const DISCOVER_MAX_DEPTH: usize = 8;
const DISCOVER_MAX_ENTRIES: usize = 400_000;
const BUNDLE_EXTENSIONS: &[&str] = &[
    "app",
    "appex",
    "framework",
    "bundle",
    "plugin",
    "xpc",
    "kext",
    "photoslibrary",
    "musiclibrary",
    "fcpbundle",
    "pkg",
    "mpkg",
    "xcarchive",
];

/// Folders a project search never enters: where apps and the system keep
/// their own trees, and media libraries.
fn not_a_project_area(name: &str) -> bool {
    name.starts_with('.')
        || matches!(
            name,
            "Library" | "Applications" | "Movies" | "Music" | "Pictures"
        )
        || name.rsplit_once('.').is_some_and(|(stem, ext)| {
            !stem.is_empty() && BUNDLE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
        })
}

fn has_marker(parent: &Path, d: &Discover) -> bool {
    d.markers
        .iter()
        .any(|m| parent.join(m).symlink_metadata().is_ok())
}

fn discover_dirs(home: &Path, d: &Discover) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut seen = 0usize;
    let mut stack: Vec<(PathBuf, usize)> = vec![(home.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > DISCOVER_MAX_ENTRIES {
                return found;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if !kind.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            if name == d.dir && has_marker(&dir, d) {
                found.push(path);
            } else if !not_a_project_area(&name)
                && name != "node_modules"
                && depth < DISCOVER_MAX_DEPTH
            {
                stack.push((path, depth + 1));
            }
        }
    }
    found
}

/// Whether `path` is something `discover_dirs` would report, checked again
/// at move time.
fn is_discovered(path: &Path, home: &Path, d: &Discover) -> bool {
    let Ok(rest) = path.strip_prefix(home) else {
        return false;
    };
    let parts: Vec<String> = rest
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let Some((last, ancestors)) = parts.split_last() else {
        return false;
    };
    last == &d.dir
        && ancestors.len() <= DISCOVER_MAX_DEPTH + 1
        && !ancestors
            .iter()
            .any(|a| not_a_project_area(a) || a == "node_modules")
        && path.parent().is_some_and(|p| has_marker(p, d))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CleanupPack {
    pub schema_version: u32,
    pub pack_version: u32,
    pub rules: Vec<CleanupRule>,
}

pub fn load_pack() -> Result<CleanupPack, String> {
    let pack: CleanupPack = serde_json::from_str(PACK_JSON).map_err(|e| e.to_string())?;
    if pack.schema_version != 1 {
        return Err(format!(
            "unsupported cleanup pack schema {}",
            pack.schema_version
        ));
    }
    Ok(pack)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Finding {
    pub id: String,
    pub rule_id: String,
    pub category: String,
    pub name: String,
    pub path: String,
    pub bytes: u64,
    /// The size is a lower bound because the walk hit its entry limit.
    pub partial: bool,
    pub risk: RuleRisk,
    pub eligible: bool,
    pub preselected: bool,
    pub reason: String,
    pub action: String,
    /// Decimal strings: inode numbers can exceed what a JS number holds.
    pub dev: String,
    pub ino: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Report {
    pub findings: Vec<Finding>,
    pub safe_bytes: u64,
    pub review_bytes: u64,
    pub scanned_at: u64,
}

/// Names (lowercased) of running processes; the liveness evidence.
pub fn running_process_names() -> Vec<String> {
    let mut system = sysinfo::System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    system
        .processes()
        .values()
        .map(|p| p.name().to_string_lossy().to_lowercase())
        .collect()
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `*` matches any run of characters inside one path segment.
fn wild(pattern: &str, value: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let v: Vec<char> = value.chars().collect();
    let (mut pi, mut vi) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut mark = 0usize;
    while vi < v.len() {
        if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = vi;
            pi += 1;
        } else if pi < p.len() && p[pi] == v[vi] {
            pi += 1;
            vi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            vi = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

fn resolve_pattern(pattern: &str, home: &Path) -> String {
    match pattern.strip_prefix("~/") {
        Some(rest) => format!("{}/{}", home.display(), rest),
        None => pattern.to_string(),
    }
}

fn expand(pattern: &str, home: &Path) -> Vec<PathBuf> {
    let full = resolve_pattern(pattern, home);
    let mut current = vec![PathBuf::from("/")];
    for segment in full.split('/').filter(|s| !s.is_empty()) {
        let mut next = Vec::new();
        for base in &current {
            if segment.contains('*') {
                if let Ok(entries) = fs::read_dir(base) {
                    for entry in entries.flatten() {
                        let name = entry.file_name();
                        if name.to_str().is_some_and(|n| wild(segment, n)) {
                            next.push(base.join(name));
                        }
                    }
                }
            } else {
                let path = base.join(segment);
                if path.symlink_metadata().is_ok() {
                    next.push(path);
                }
            }
        }
        current = next;
        if current.is_empty() {
            break;
        }
    }
    current
}

fn path_matches(pattern: &str, path: &Path, home: &Path) -> bool {
    let full = resolve_pattern(pattern, home);
    let want: Vec<&str> = full.split('/').filter(|s| !s.is_empty()).collect();
    let have: Vec<String> = path
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => s.to_str().map(|s| s.to_string()),
            _ => None,
        })
        .collect();
    want.len() == have.len() && want.iter().zip(have.iter()).all(|(p, v)| wild(p, v))
}

fn excluded(rule: &CleanupRule, name: &str) -> bool {
    name == ".DS_Store"
        || name == ".localized"
        || rule.exclude_names.iter().any(|pattern| wild(pattern, name))
}

#[cfg(unix)]
fn identity(md: &fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (md.dev(), md.ino())
}

#[cfg(not(unix))]
fn identity(_md: &fs::Metadata) -> (u64, u64) {
    (0, 0)
}

#[cfg(unix)]
fn allocated(md: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    md.blocks().saturating_mul(512)
}

#[cfg(not(unix))]
fn allocated(md: &fs::Metadata) -> u64 {
    md.len()
}

/// Cloud placeholders (macOS "dataless" files) are never touched.
#[cfg(target_os = "macos")]
fn is_dataless(md: &fs::Metadata) -> bool {
    use std::os::macos::fs::MetadataExt;
    md.st_flags() & 0x4000_0000 != 0
}

#[cfg(not(target_os = "macos"))]
fn is_dataless(_md: &fs::Metadata) -> bool {
    false
}

/// Allocated bytes under `path` without following symlinks, and whether the
/// walk stopped early.
fn measure(path: &Path) -> (u64, bool) {
    let mut total = 0u64;
    let mut seen = 0usize;
    let mut stack = vec![path.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(md) = current.symlink_metadata() else {
            continue;
        };
        total = total.saturating_add(allocated(&md));
        seen += 1;
        if seen >= MAX_ENTRIES_PER_ITEM {
            return (total, true);
        }
        if md.is_dir()
            && let Ok(entries) = fs::read_dir(&current)
        {
            for entry in entries.flatten() {
                stack.push(entry.path());
            }
        }
    }
    (total, false)
}

fn age_days(md: &fs::Metadata) -> Option<u64> {
    let modified = md.modified().ok()?;
    let elapsed = SystemTime::now().duration_since(modified).ok()?;
    Some(elapsed.as_secs() / 86_400)
}

fn name_matches(process: &str, owner: &str) -> bool {
    let owner = owner.to_lowercase();
    process == owner
        || process.starts_with(&format!("{owner} "))
        || process.starts_with(&format!("{owner}-"))
        || process.starts_with(&format!("{owner}("))
}

/// Some(process) when an owner is running, None when not, and Err when the
/// rule cannot say who owns the item (unknown).
fn in_use(rule: &CleanupRule, name: &str, running: &[String]) -> Result<Option<String>, String> {
    let mut owners: Vec<String> = rule.owners.clone();
    match rule.liveness {
        LivenessCheck::Age => {}
        LivenessCheck::Owners => {
            if owners.is_empty() {
                return Err("Can't tell which app owns this".into());
            }
        }
        LivenessCheck::Name => {
            owners.push(name.to_string());
            for token in name.split('.') {
                if token.len() >= 4 {
                    owners.push(token.to_string());
                }
            }
        }
    }
    for owner in &owners {
        if let Some(process) = running.iter().find(|p| name_matches(p, owner)) {
            return Ok(Some(process.clone()));
        }
    }
    Ok(None)
}

fn finding_id(rule_id: &str, path: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    rule_id.hash(&mut hasher);
    path.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string()
}

pub fn scan(home: &Path, running: &[String]) -> Result<Report, String> {
    let pack = load_pack()?;
    let mut report = Report {
        scanned_at: now_millis() / 1000,
        ..Report::default()
    };
    let mut seen: HashSet<PathBuf> = HashSet::new();

    for rule in &pack.rules {
        let groups: Vec<Vec<PathBuf>> = match &rule.discover {
            Some(d) => vec![discover_dirs(home, d)],
            None => rule.paths.iter().map(|p| expand(p, home)).collect(),
        };
        for group in groups {
            let mut matched: Vec<(PathBuf, fs::Metadata)> = Vec::new();
            for path in group {
                let name = display_name(&path);
                if excluded(rule, &name) || seen.contains(&path) {
                    continue;
                }
                let Ok(md) = path.symlink_metadata() else {
                    continue;
                };
                if md.file_type().is_symlink() {
                    continue;
                }
                matched.push((path, md));
            }
            if rule.keep_newest > 0 {
                matched.sort_by_key(|(_, md)| {
                    std::cmp::Reverse(md.modified().ok().unwrap_or(UNIX_EPOCH))
                });
                let _ = matched.drain(..rule.keep_newest.min(matched.len()));
            }
            for (path, md) in matched {
                if let Some(min) = rule.min_age_days {
                    match age_days(&md) {
                        Some(age) if age >= min => {}
                        _ => continue,
                    }
                }
                let (bytes, partial) = measure(&path);
                if bytes == 0 && rule.risk != RuleRisk::Info {
                    continue;
                }
                let name = display_name(&path);
                let (dev, ino) = identity(&md);
                let (eligible, reason) = judge(rule, &name, &md, running);
                seen.insert(path.clone());
                let path_text = path.to_string_lossy().into_owned();
                if eligible {
                    match rule.risk {
                        RuleRisk::Safe => report.safe_bytes += bytes,
                        _ => report.review_bytes += bytes,
                    }
                }
                report.findings.push(Finding {
                    id: finding_id(&rule.id, &path_text),
                    rule_id: rule.id.clone(),
                    category: rule.category.clone(),
                    name: if rule.discover.is_some() {
                        let project = path.parent().map(display_name).unwrap_or_default();
                        format!("{} · {}", rule.name, project)
                    } else if rule.risk == RuleRisk::Info || name.is_empty() {
                        rule.name.clone()
                    } else {
                        name
                    },
                    path: path_text,
                    bytes,
                    partial,
                    risk: rule.risk,
                    eligible,
                    preselected: eligible && rule.risk == RuleRisk::Safe,
                    reason,
                    action: rule.action.clone(),
                    dev: dev.to_string(),
                    ino: ino.to_string(),
                });
            }
        }
    }
    report
        .findings
        .sort_by(|a, b| a.category.cmp(&b.category).then(b.bytes.cmp(&a.bytes)));
    Ok(report)
}

fn judge(rule: &CleanupRule, name: &str, md: &fs::Metadata, running: &[String]) -> (bool, String) {
    if rule.risk == RuleRisk::Info {
        return (false, rule.reason.clone());
    }
    if is_dataless(md) {
        return (false, "Stored in the cloud; not touched".into());
    }
    match in_use(rule, name, running) {
        Err(why) => (false, why),
        Ok(Some(process)) => (false, format!("In use: {process} is running")),
        Ok(None) => (true, rule.reason.clone()),
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Request {
    pub rule_id: String,
    pub path: String,
    pub dev: String,
    pub ino: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Skipped {
    pub path: String,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ActivityItem {
    pub path: String,
    pub trash_path: Option<String>,
    pub bytes: u64,
    #[serde(default)]
    pub restored: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Activity {
    pub id: String,
    /// Unix seconds.
    pub at: u64,
    pub bytes: u64,
    pub items: Vec<ActivityItem>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ApplyResult {
    pub moved_items: usize,
    pub moved_bytes: u64,
    pub skipped: Vec<Skipped>,
    pub activity_id: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct RestoreResult {
    pub restored_items: usize,
    pub restored_bytes: u64,
    pub skipped: Vec<Skipped>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct ActivityLog {
    #[serde(default)]
    schema_version: u32,
    #[serde(default)]
    actions: Vec<Activity>,
}

fn log_path(home: &Path) -> PathBuf {
    home.join("Library/Application Support/Cockpit/cleanup-activity.json")
}

fn read_log(home: &Path) -> ActivityLog {
    fs::read_to_string(log_path(home))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write_log(home: &Path, log: &ActivityLog) -> Result<(), String> {
    let path = log_path(home);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let temp = path.with_extension("json.tmp");
    let body = serde_json::to_vec_pretty(log).map_err(|e| e.to_string())?;
    fs::write(&temp, body).map_err(|e| e.to_string())?;
    fs::rename(&temp, &path).map_err(|e| e.to_string())
}

/// Past actions, newest first.
pub fn history(home: &Path) -> Vec<Activity> {
    let mut actions = read_log(home).actions;
    actions.reverse();
    actions
}

fn listing(dir: &Path) -> HashSet<OsString> {
    fs::read_dir(dir)
        .map(|entries| entries.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default()
}

/// Check one request against the rule pack and the disk, right now.
fn revalidate(
    request: &Request,
    pack: &CleanupPack,
    home: &Path,
    running: &[String],
) -> Result<(PathBuf, u64, (u64, u64)), String> {
    let wanted: (u64, u64) = match (request.dev.parse(), request.ino.parse()) {
        (Ok(dev), Ok(ino)) => (dev, ino),
        _ => return Err("Bad identity".into()),
    };
    let rule = pack
        .rules
        .iter()
        .find(|r| r.id == request.rule_id)
        .ok_or("Unknown rule")?;
    if rule.risk == RuleRisk::Info {
        return Err("Not something to move".into());
    }
    let path = PathBuf::from(&request.path);
    let covered = match &rule.discover {
        Some(d) => is_discovered(&path, home, d),
        None => rule.paths.iter().any(|p| path_matches(p, &path, home)),
    };
    if !covered {
        return Err("Not a location this rule covers".into());
    }
    if excluded(rule, &display_name(&path)) {
        return Err("Excluded by the rule".into());
    }
    let md = path
        .symlink_metadata()
        .map_err(|_| "Already gone".to_string())?;
    if md.file_type().is_symlink() {
        return Err("Is now a link".into());
    }
    if identity(&md) != wanted {
        return Err("Changed since the scan".into());
    }
    if is_dataless(&md) {
        return Err("Stored in the cloud".into());
    }
    if let Some(min) = rule.min_age_days {
        match age_days(&md) {
            Some(age) if age >= min => {}
            _ => return Err("Modified since the scan".into()),
        }
    }
    match in_use(rule, &display_name(&path), running)? {
        Some(process) => Err(format!("In use: {process} is running")),
        None => {
            let bytes = measure(&path).0;
            Ok((path, bytes, wanted))
        }
    }
}

/// Move each requested item to Trash through `trasher`, re-checking it first.
/// `trasher` must only move to Trash; it is the one place an effect happens.
pub fn apply(
    home: &Path,
    requests: &[Request],
    running: &[String],
    trasher: &mut dyn FnMut(&Path) -> Result<(), String>,
) -> Result<ApplyResult, String> {
    let pack = load_pack()?;
    let trash_dir = home.join(".Trash");
    let mut result = ApplyResult::default();
    let mut items: Vec<ActivityItem> = Vec::new();

    for request in requests {
        let (path, bytes, wanted) = match revalidate(request, &pack, home, running) {
            Ok(ok) => ok,
            Err(reason) => {
                result.skipped.push(Skipped {
                    path: request.path.clone(),
                    reason,
                });
                continue;
            }
        };
        let before = listing(&trash_dir);
        if let Err(reason) = trasher(&path) {
            result.skipped.push(Skipped {
                path: request.path.clone(),
                reason,
            });
            continue;
        }
        // A rename keeps the inode, so the new Trash entry that carries the
        // item's identity is where it went.
        let trash_path = listing(&trash_dir)
            .into_iter()
            .filter(|name| !before.contains(name))
            .map(|name| trash_dir.join(name))
            .find(|candidate| {
                candidate
                    .symlink_metadata()
                    .map(|md| identity(&md) == wanted)
                    .unwrap_or(false)
            })
            .map(|p| p.to_string_lossy().into_owned());
        result.moved_items += 1;
        result.moved_bytes += bytes;
        items.push(ActivityItem {
            path: request.path.clone(),
            trash_path,
            bytes,
            restored: false,
        });
    }

    if !items.is_empty() {
        let at = now_millis();
        let activity = Activity {
            id: format!("{at}"),
            at: at / 1000,
            bytes: result.moved_bytes,
            items,
        };
        let mut log = read_log(home);
        log.schema_version = ACTIVITY_SCHEMA_VERSION;
        result.activity_id = Some(activity.id.clone());
        log.actions.push(activity);
        write_log(home, &log)?;
    }
    Ok(result)
}

/// Put an action's items back where they came from. An item goes back only if
/// it is still in the Trash and its original path is free.
pub fn restore(home: &Path, activity_id: &str) -> Result<RestoreResult, String> {
    let trash_dir = home.join(".Trash");
    let mut log = read_log(home);
    let activity = log
        .actions
        .iter_mut()
        .find(|a| a.id == activity_id)
        .ok_or("No such action")?;
    let mut result = RestoreResult::default();
    for item in activity.items.iter_mut() {
        if item.restored {
            continue;
        }
        let skip = |reason: &str| Skipped {
            path: item.path.clone(),
            reason: reason.to_string(),
        };
        let Some(trash_path) = item.trash_path.clone() else {
            result.skipped.push(skip("Trash location wasn't recorded"));
            continue;
        };
        let from = PathBuf::from(&trash_path);
        let to = PathBuf::from(&item.path);
        let allowed_target = to.starts_with(home) || to.starts_with("/private/var/folders");
        if !from.starts_with(&trash_dir) || !allowed_target {
            result.skipped.push(skip("Location not allowed"));
            continue;
        }
        if from.symlink_metadata().is_err() {
            result.skipped.push(skip("No longer in the Trash"));
            continue;
        }
        if to.symlink_metadata().is_ok() {
            result
                .skipped
                .push(skip("Something is already at the original location"));
            continue;
        }
        if let Some(parent) = to.parent()
            && let Err(e) = fs::create_dir_all(parent)
        {
            result.skipped.push(skip(&e.to_string()));
            continue;
        }
        match fs::rename(&from, &to) {
            Ok(()) => {
                item.restored = true;
                result.restored_items += 1;
                result.restored_bytes += item.bytes;
            }
            Err(e) => result.skipped.push(skip(&e.to_string())),
        }
    }
    write_log(home, &log)?;
    Ok(result)
}
