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

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Measurement {
    /// Allocated blocks, as `du` counts them.
    #[default]
    Allocated,
    /// Bytes no APFS clone shares: what deleting the item gives back.
    CloneAware,
}

/// The rule whose items are Chrome's leftover signing snapshots.
pub const CHROME_SNAPSHOT_RULE: &str = "chrome-signing-copies";
/// Most files whose private size is read in one scan; the rest stay unmeasured.
const CLONE_AWARE_BUDGET: usize = 600_000;

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
    #[serde(default)]
    pub measurement: Measurement,
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
    /// The rule's own name: what a group of findings is called.
    #[serde(default)]
    pub rule_name: String,
    pub category: String,
    pub name: String,
    pub path: String,
    /// What moving it can give back. For clone-aware rules this is the
    /// unshared part only; 0 when that could not be measured.
    pub bytes: u64,
    /// Allocated size as `du` shows it, counting blocks shared with clones.
    /// Equals `bytes` for rules that are not clone-aware.
    #[serde(default)]
    pub apparent_bytes: u64,
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
    #[serde(default)]
    pub chrome_snapshots: Option<ChromeSnapshots>,
    pub findings: Vec<Finding>,
    pub safe_bytes: u64,
    pub review_bytes: u64,
    pub scanned_at: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ChromeSnapshots {
    pub count: u64,
    pub apparent_bytes: u64,
    /// Unshared bytes; only meaningful when `reclaimable_known`.
    pub reclaimable_bytes: u64,
    pub reclaimable_known: bool,
    /// A Chrome-family process is running, so none are offered.
    pub running: bool,
    /// Count and time of the earlier sample the change is measured from.
    pub since_at: Option<u64>,
    pub since_count: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CountSample {
    at: u64,
    count: u64,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct CountLog {
    #[serde(default)]
    samples: Vec<CountSample>,
}

const COUNT_SAMPLE_GAP_SECS: u64 = 86_400;
const COUNT_WINDOW_SECS: u64 = 30 * 86_400;
const COUNT_KEEP: usize = 60;

fn count_log_path(home: &Path) -> PathBuf {
    home.join("Library/Application Support/Pulse/chrome-snapshots.json")
}

/// Add a sample when the last one is a day old or more, and pick the baseline
/// the change is shown from: the oldest earlier sample inside 30 days.
fn track_count(log: &mut CountLog, count: u64, now: u64) -> (bool, Option<CountSample>) {
    let due = log
        .samples
        .last()
        .is_none_or(|last| now >= last.at.saturating_add(COUNT_SAMPLE_GAP_SECS));
    let baseline = log
        .samples
        .iter()
        .find(|s| s.at < now && now - s.at <= COUNT_WINDOW_SECS)
        .cloned();
    if due {
        log.samples.push(CountSample { at: now, count });
        let extra = log.samples.len().saturating_sub(COUNT_KEEP);
        log.samples.drain(..extra);
    }
    (due, baseline)
}

fn record_chrome_count(home: &Path, count: u64, now: u64) -> Option<CountSample> {
    let path = count_log_path(home);
    let mut log: CountLog = fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let (due, baseline) = track_count(&mut log, count, now);
    if due && (count > 0 || log.samples.len() > 1) {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let temp = path.with_extension("json.tmp");
        if let Ok(body) = serde_json::to_vec_pretty(&log)
            && fs::write(&temp, body).is_ok()
        {
            let _ = fs::rename(&temp, &path);
        }
    }
    baseline
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

/// Apparent (allocated) bytes under `path` and, when the file system can say,
/// the bytes no clone shares. The second value is `None` if any file could not
/// be asked or the budget ran out: a partial sum would understate, so the
/// answer is "unknown", never a smaller number.
fn measure_clone_aware(path: &Path, budget: &mut usize) -> (u64, Option<u64>) {
    let mut apparent = 0u64;
    let mut unique = Some(0u64);
    let mut seen = 0usize;
    let mut stack = vec![path.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(md) = current.symlink_metadata() else {
            continue;
        };
        apparent = apparent.saturating_add(allocated(&md));
        seen += 1;
        if seen >= MAX_ENTRIES_PER_ITEM {
            unique = None;
            break;
        }
        if md.is_file() {
            if *budget == 0 {
                unique = None;
            } else {
                *budget -= 1;
                match (unique, crate::platform::private_size(&current)) {
                    (Some(total), Some(bytes)) => unique = Some(total.saturating_add(bytes)),
                    _ => unique = None,
                }
            }
        } else if md.is_dir()
            && let Ok(entries) = fs::read_dir(&current)
        {
            for entry in entries.flatten() {
                stack.push(entry.path());
            }
        }
    }
    (apparent, unique)
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
    let mut clone_budget = CLONE_AWARE_BUDGET;
    let mut snapshots = ChromeSnapshots {
        reclaimable_known: true,
        ..ChromeSnapshots::default()
    };
    let mut snapshots_seen = false;

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
            if rule.id == CHROME_SNAPSHOT_RULE {
                snapshots_seen = true;
                snapshots.running = matches!(in_use(rule, "", running), Ok(Some(_)));
            }
            for (path, md) in matched {
                if let Some(min) = rule.min_age_days {
                    match age_days(&md) {
                        Some(age) if age >= min => {}
                        _ => continue,
                    }
                }
                let clone_aware = rule.measurement == Measurement::CloneAware;
                let (bytes, apparent_bytes, partial) = if clone_aware {
                    let (apparent, unique) = measure_clone_aware(&path, &mut clone_budget);
                    if rule.id == CHROME_SNAPSHOT_RULE {
                        snapshots.count += 1;
                        snapshots.apparent_bytes += apparent;
                        match unique {
                            Some(u) => snapshots.reclaimable_bytes += u,
                            None => snapshots.reclaimable_known = false,
                        }
                    }
                    (unique.unwrap_or(0), apparent, unique.is_none())
                } else {
                    let (b, p) = measure(&path);
                    (b, b, p)
                };
                if bytes == 0 && !clone_aware && rule.risk != RuleRisk::Info {
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
                    rule_name: rule.name.clone(),
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
                    apparent_bytes,
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
    if snapshots_seen {
        let now = report.scanned_at;
        let baseline = record_chrome_count(home, snapshots.count, now);
        snapshots.since_at = baseline.as_ref().map(|b| b.at);
        snapshots.since_count = baseline.map(|b| b.count);
        if snapshots.count > 0 {
            report.chrome_snapshots = Some(snapshots);
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
    home.join("Library/Application Support/Pulse/cleanup-activity.json")
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
            let bytes = match rule.measurement {
                Measurement::Allocated => measure(&path).0,
                Measurement::CloneAware => {
                    let mut budget = CLONE_AWARE_BUDGET;
                    measure_clone_aware(&path, &mut budget).1.unwrap_or(0)
                }
            };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_log_samples_daily_and_reports_change() {
        let mut log = CountLog::default();
        let day = 86_400;
        let (due, base) = track_count(&mut log, 100, 10 * day);
        assert!(due && base.is_none());
        let (due, _) = track_count(&mut log, 120, 10 * day + 3600);
        assert!(!due);
        let (due, base) = track_count(&mut log, 133, 12 * day);
        assert!(due);
        let base = base.expect("baseline");
        assert_eq!((base.at, base.count), (10 * day, 100));
        assert_eq!(log.samples.len(), 2);
    }

    #[test]
    fn chrome_clone_paths_match_the_rule() {
        let pack = load_pack().unwrap();
        let rule = pack
            .rules
            .iter()
            .find(|r| r.id == CHROME_SNAPSHOT_RULE)
            .unwrap();
        let home = Path::new("/Users/x");
        for id in [
            "com.google.Chrome",
            "com.google.chrome.for.testing",
            "org.chromium.Chromium",
        ] {
            let p = PathBuf::from(format!(
                "/private/var/folders/ab/cd/X/{id}.code_sign_clone/code_sign_clone.AbC123"
            ));
            assert!(
                rule.paths.iter().any(|pat| path_matches(pat, &p, home)),
                "{id}"
            );
        }
        let other = PathBuf::from(
            "/private/var/folders/ab/cd/X/com.example.App.code_sign_clone/code_sign_clone.1",
        );
        assert!(!rule.paths.iter().any(|pat| path_matches(pat, &other, home)));
    }
}
