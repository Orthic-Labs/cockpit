//! Live process list grouped by application, with Quit and Force Quit.
//!
//! Identity is `(pid, start_time)` and is re-checked against a fresh snapshot
//! immediately before any signal or quit request. Quit is always graceful and
//! never escalates; Force Quit is a separate, explicit call. Other users'
//! processes, system processes and Cockpit itself are never listed or touched.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sysinfo::{Pid, Process, ProcessRefreshKind, ProcessesToUpdate, System};

use crate::ProcessIdentity;
use crate::app_manager::{app_root_of, bundle_info, quit_bundle};

const SYSTEM_PREFIXES: [&str; 6] = [
    "/System/",
    "/usr/",
    "/sbin/",
    "/bin/",
    "/Library/Apple/",
    "/private/",
];
const NEVER_QUIT_BUNDLES: [&str; 4] = [
    "com.apple.finder",
    "com.apple.dock",
    "com.apple.systemuiserver",
    "com.apple.loginwindow",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessMember {
    pub identity: ProcessIdentity,
    pub name: String,
    pub cpu_usage_percent: f32,
    pub memory_bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessRow {
    /// Stable key: the app bundle path, or `pid:start` for a plain process.
    pub key: String,
    pub name: String,
    pub bundle_id: Option<String>,
    pub app_path: Option<String>,
    /// The main process of the group; identity for Quit.
    pub lead: ProcessIdentity,
    pub cpu_usage_percent: f32,
    pub memory_bytes: u64,
    pub members: Vec<ProcessMember>,
    /// False when Quit/Force Quit must not be offered, with the reason.
    pub can_act: bool,
    pub refusal: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuitOutcome {
    /// The process or app is gone.
    Quit,
    /// A graceful request was sent but the target is still running.
    StillRunning,
}

#[derive(Clone)]
struct Snap {
    identity: ProcessIdentity,
    name: String,
    parent: Option<u32>,
    exe: Option<PathBuf>,
    cpu: f32,
    memory: u64,
    mine: bool,
}

fn current_uid() -> u32 {
    unsafe { libc::getuid() }
}

fn snapshot(with_cpu: bool) -> Vec<Snap> {
    let mut system = System::new();
    let kind = ProcessRefreshKind::everything();
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
    if with_cpu {
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        system.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
    }
    let me = current_uid();
    system
        .processes()
        .values()
        .map(|p| snap_of(p, me))
        .collect()
}

fn snap_of(p: &Process, me: u32) -> Snap {
    Snap {
        identity: ProcessIdentity {
            pid: p.pid().as_u32(),
            start_time: p.start_time(),
        },
        name: p.name().to_string_lossy().into_owned(),
        parent: p.parent().map(|pid| pid.as_u32()),
        exe: p.exe().map(Path::to_path_buf),
        cpu: p.cpu_usage(),
        memory: p.memory(),
        mine: p.user_id().map(|u| **u == me).unwrap_or(false),
    }
}

fn is_system_path(path: &Path) -> bool {
    let text = path.to_string_lossy();
    if text.starts_with("/usr/local/") {
        return false;
    }
    SYSTEM_PREFIXES
        .iter()
        .any(|prefix| text.starts_with(prefix))
}

fn is_cockpit(root: Option<&Path>, bundle_id: Option<&str>, name: &str) -> bool {
    bundle_id
        .map(|b| b.starts_with("dev.orthic.cockpit"))
        .unwrap_or(false)
        || root
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().starts_with("Cockpit"))
            .unwrap_or(false)
        || name.to_lowercase().starts_with("cockpit")
}

/// Which app bundle a process belongs to: its own executable's outermost
/// `.app`, else the nearest ancestor (verified: the parent started no later
/// than the child, so a reused PID cannot adopt it) that lives in one.
fn owning_app(snap: &Snap, by_pid: &HashMap<u32, &Snap>) -> Option<PathBuf> {
    if let Some(root) = snap.exe.as_deref().and_then(app_root_of) {
        return Some(root);
    }
    let mut current = snap;
    for _ in 0..16 {
        let parent = by_pid.get(&current.parent?)?;
        if parent.identity.start_time > current.identity.start_time || parent.identity.pid <= 1 {
            return None;
        }
        if let Some(root) = parent.exe.as_deref().and_then(app_root_of) {
            return Some(root);
        }
        current = parent;
    }
    None
}

type InfoCache = Mutex<HashMap<PathBuf, Option<(String, String)>>>;

fn infos() -> &'static InfoCache {
    static CACHE: OnceLock<InfoCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `(bundle id, display name)` for an app bundle, cached.
fn cached_info(root: &Path) -> Option<(String, String)> {
    if let Ok(cache) = infos().lock() {
        if let Some(found) = cache.get(root) {
            return found.clone();
        }
    }
    let found = bundle_info(root).and_then(|i| i.bundle_id.map(|b| (b, i.name)));
    if let Ok(mut cache) = infos().lock() {
        cache.insert(root.to_path_buf(), found.clone());
    }
    found
}

/// Processes of the current user, grouped under their app, largest memory first.
pub fn process_rows() -> Vec<ProcessRow> {
    build_rows(&snapshot(true))
}

fn member(s: &Snap) -> ProcessMember {
    ProcessMember {
        identity: s.identity.clone(),
        name: s.name.clone(),
        cpu_usage_percent: s.cpu,
        memory_bytes: s.memory,
    }
}

fn refusal_for(
    root: Option<&Path>,
    bundle_id: Option<&str>,
    name: &str,
    members: &[&Snap],
) -> Option<String> {
    if members.iter().any(|m| m.identity.pid == std::process::id())
        || is_cockpit(root, bundle_id, name)
    {
        return Some("Cockpit does not quit itself.".into());
    }
    if let Some(b) = bundle_id {
        if NEVER_QUIT_BUNDLES.contains(&b) {
            return Some("macOS restarts this automatically; quit it from the system.".into());
        }
    }
    None
}

fn build_rows(snaps: &[Snap]) -> Vec<ProcessRow> {
    let by_pid: HashMap<u32, &Snap> = snaps.iter().map(|s| (s.identity.pid, s)).collect();
    let mut apps: HashMap<PathBuf, Vec<&Snap>> = HashMap::new();
    let mut loose: Vec<&Snap> = Vec::new();
    for snap in snaps.iter().filter(|s| s.mine && s.identity.pid > 1) {
        if snap.exe.as_deref().map(is_system_path).unwrap_or(false) {
            continue;
        }
        match owning_app(snap, &by_pid) {
            Some(root) => apps.entry(root).or_default().push(snap),
            None => loose.push(snap),
        }
    }
    let mut rows = Vec::new();
    for (root, members) in apps {
        let info = cached_info(&root);
        let bundle_id = info.as_ref().map(|i| i.0.clone());
        let name = info.map(|i| i.1).unwrap_or_else(|| {
            root.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        let main_prefix = root.join("Contents/MacOS");
        let Some(lead) = members
            .iter()
            .filter(|m| {
                m.exe
                    .as_deref()
                    .map(|e| e.starts_with(&main_prefix))
                    .unwrap_or(false)
            })
            .min_by_key(|m| m.identity.start_time)
            .or_else(|| members.iter().min_by_key(|m| m.identity.start_time))
            .map(|m| m.identity.clone())
        else {
            continue;
        };
        let refusal = refusal_for(Some(&root), bundle_id.as_deref(), &name, &members);
        rows.push(ProcessRow {
            key: root.to_string_lossy().into_owned(),
            name,
            bundle_id,
            app_path: Some(root.to_string_lossy().into_owned()),
            lead,
            cpu_usage_percent: members.iter().map(|m| m.cpu).sum(),
            memory_bytes: members.iter().map(|m| m.memory).sum(),
            members: members.iter().map(|m| member(m)).collect(),
            can_act: refusal.is_none(),
            refusal,
        });
    }
    for snap in loose {
        let refusal = refusal_for(None, None, &snap.name, &[snap]);
        rows.push(ProcessRow {
            key: format!("{}:{}", snap.identity.pid, snap.identity.start_time),
            name: snap.name.clone(),
            bundle_id: None,
            app_path: None,
            lead: snap.identity.clone(),
            cpu_usage_percent: snap.cpu,
            memory_bytes: snap.memory,
            members: vec![member(snap)],
            can_act: refusal.is_none(),
            refusal,
        });
    }
    rows.sort_by(|a, b| b.memory_bytes.cmp(&a.memory_bytes));
    rows
}

/// Re-read the system and return the row `lead` currently heads, or the
/// reason it cannot be acted on. Identity must match exactly.
fn recheck(key: &str, lead: &ProcessIdentity) -> Result<ProcessRow, String> {
    let snaps = snapshot(false);
    let current = snaps
        .iter()
        .find(|s| s.identity.pid == lead.pid)
        .ok_or("That process has already exited.")?;
    if current.identity.start_time != lead.start_time {
        return Err("That process was replaced by a different one; nothing was changed.".into());
    }
    let row = build_rows(&snaps)
        .into_iter()
        .find(|r| r.key == key && r.lead.pid == lead.pid && r.lead.start_time == lead.start_time)
        .ok_or("That is no longer a listed app or process; nothing was changed.")?;
    match &row.refusal {
        Some(reason) => Err(reason.clone()),
        None => Ok(row),
    }
}

fn alive(id: &ProcessIdentity) -> bool {
    let mut system = System::new();
    let pid = Pid::from_u32(id.pid);
    system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    system
        .process(pid)
        .map(|p| p.start_time() == id.start_time)
        .unwrap_or(false)
}

fn wait_gone(id: &ProcessIdentity, limit: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if !alive(id) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    !alive(id)
}

fn signal(id: &ProcessIdentity, sig: i32) -> Result<(), String> {
    // Identity is verified again here, immediately before the signal.
    if !alive(id) {
        return Ok(());
    }
    let result = unsafe { libc::kill(id.pid as libc::pid_t, sig) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().to_string())
    }
}

/// Graceful quit. Apps get a Quit Apple event; plain processes get SIGTERM.
/// Never escalates: a target that stays up is reported as `StillRunning`.
pub fn quit(key: &str, lead: &ProcessIdentity) -> Result<QuitOutcome, String> {
    let row = recheck(key, lead)?;
    match &row.bundle_id {
        Some(bundle_id) if row.app_path.is_some() => quit_bundle(bundle_id)?,
        _ => signal(&row.lead, libc::SIGTERM)?,
    }
    Ok(if wait_gone(&row.lead, Duration::from_secs(8)) {
        QuitOutcome::Quit
    } else {
        QuitOutcome::StillRunning
    })
}

/// Explicit SIGKILL of the group's current members (each identity re-checked).
pub fn force_quit(key: &str, lead: &ProcessIdentity) -> Result<QuitOutcome, String> {
    let row = recheck(key, lead)?;
    for m in &row.members {
        signal(&m.identity, libc::SIGKILL)?;
    }
    Ok(if wait_gone(&row.lead, Duration::from_secs(3)) {
        QuitOutcome::Quit
    } else {
        QuitOutcome::StillRunning
    })
}
