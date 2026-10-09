//! Update checks and updates for the Apps page on Windows, through winget.
//!
//! Check: `winget upgrade --include-unknown --disable-interactivity` run hidden
//! (CREATE_NO_WINDOW) and its table parsed. The Microsoft.WinGet.Client COM API was
//! not used: it needs WinRT/COM activation (and a new `windows` dependency in the
//! hub) for the same list the CLI prints, and the CLI is what a person would run.
//! The CLI has no JSON output for `upgrade`, so the table is read by splitting a
//! row at runs of two or more spaces (winget pads columns to at least that), which
//! also keeps the parse independent of wide characters and of the language's
//! column titles; only the column order (Name, Id, Version, Available, Source) is
//! assumed.
//!
//! Update: `winget upgrade --id <id> --exact` for the one app, in the background,
//! reported as `apps-update-job` like the Homebrew upgrade on macOS. winget asks
//! for its own elevation when the installer needs it.
//!
//! A winget row is tied to an installed app by the Uninstall key name inside its
//! id (`ARP\Machine\X64\Git_is1`) or by an unambiguous name match; rows that match
//! no listed app are dropped because the page has no row to show them on.

use std::collections::HashMap;
use std::io::Read;
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use super::usage::norm;
use super::{blocking, now_secs, read_installed, Busy, Installed, CREATE_NO_WINDOW};
use crate::cache;

const UPDATES_FILE: &str = "apps-updates-windows-v1.json";
const UPDATES_FORMAT: u32 = 1;
const FRESH_SECS: i64 = 6 * 3600;
const CHECK_TIMEOUT: Duration = Duration::from_secs(180);
const UPGRADE_TIMEOUT: Duration = Duration::from_secs(1800);
/// winget exit codes that mean "nothing to list" rather than a failure.
const NO_APPLICATIONS_FOUND: u32 = 0x8A15_0014;
const NO_APPLICABLE_UPGRADE: u32 = 0x8A15_002B;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppUpdate {
    path: String,
    name: String,
    bundle_id: Option<String>,
    installed_version: Option<String>,
    /// "winget".
    source: String,
    /// "available" (only state this module reports).
    state: String,
    latest_version: Option<String>,
    cask: Option<String>,
    store_url: Option<String>,
    reason: Option<String>,
    checked_at: i64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct UpdateReport {
    checked_at: Option<i64>,
    apps: Vec<AppUpdate>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Saved {
    checked_at: i64,
    apps: Vec<AppUpdate>,
    /// App path -> (winget id, winget source name).
    targets: HashMap<String, (String, String)>,
}

fn load_saved() -> Option<Saved> {
    cache::load::<Saved>(UPDATES_FILE, UPDATES_FORMAT)
}

fn report_of(saved: Option<&Saved>) -> UpdateReport {
    match saved {
        Some(saved) => UpdateReport { checked_at: Some(saved.checked_at), apps: saved.apps.clone() },
        None => UpdateReport { checked_at: None, apps: Vec::new() },
    }
}

pub(super) async fn cached() -> Result<UpdateReport, String> {
    blocking(|| Ok(report_of(load_saved().as_ref()))).await
}

// ---------------------------------------------------------------------------
// Running winget
// ---------------------------------------------------------------------------

fn winget_command() -> Command {
    let mut command = Command::new("winget.exe");
    command.stdin(Stdio::null()).stderr(Stdio::null()).creation_flags(CREATE_NO_WINDOW);
    command
}

/// winget's exit code (as the unsigned HRESULT) and what it printed, or an error
/// when it could not be started or did not finish in time.
fn run_winget(args: &[&str], timeout: Duration) -> Result<(u32, String), String> {
    let mut child = winget_command()
        .args(args)
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "winget is not installed. It comes with App Installer from the Microsoft Store.".to_string()
            } else {
                format!("Could not start winget: {e}")
            }
        })?;
    let mut stdout = child.stdout.take().ok_or("winget gave no output stream")?;
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
                return Err("winget did not finish in time.".into());
            }
            Err(e) => return Err(e.to_string()),
        }
    };
    let output = reader.join().map_err(|_| "winget output could not be read".to_string())?;
    Ok((status.code().unwrap_or(-1) as u32, String::from_utf8_lossy(&output).into_owned()))
}

// ---------------------------------------------------------------------------
// Parsing the table
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct WingetRow {
    name: String,
    id: String,
    version: String,
    available: String,
    source: Option<String>,
}

/// One printed line without progress-spinner carriage returns and backspaces.
fn clean(line: &str) -> String {
    let last = line.rsplit('\r').find(|segment| !segment.trim().is_empty()).unwrap_or("");
    last.chars().filter(|c| *c != '\u{8}').collect::<String>().trim_end().to_string()
}

fn is_rule(line: &str) -> bool {
    let text = line.trim();
    text.matches('-').count() >= 8 && text.chars().all(|c| matches!(c, '-' | ' ' | '\\' | '|' | '/'))
}

/// Splits at runs of two or more spaces; single spaces stay inside a cell.
fn cells(line: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut spaces = 0usize;
    for c in line.trim().chars() {
        if c == ' ' {
            spaces += 1;
            continue;
        }
        if spaces >= 2 && !current.is_empty() {
            out.push(std::mem::take(&mut current));
        } else if spaces == 1 {
            current.push(' ');
        }
        spaces = 0;
        current.push(c);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Every upgrade row of every table in `text` (winget prints a second table for
/// packages that need explicit targeting).
fn parse_upgrades(text: &str) -> Vec<WingetRow> {
    let lines: Vec<String> = text.split('\n').map(clean).collect();
    let mut rows = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if !is_rule(&lines[index]) {
            index += 1;
            continue;
        }
        index += 1;
        while index < lines.len() {
            let line = &lines[index];
            if line.trim().is_empty() {
                break;
            }
            let parts = cells(line);
            // Footers ("2 upgrades available.") have one cell and end the table.
            if parts.len() < 4 || parts[1].contains(' ') {
                break;
            }
            rows.push(WingetRow {
                name: parts[0].clone(),
                id: parts[1].clone(),
                version: parts[2].clone(),
                available: parts[3].clone(),
                source: parts.get(4).cloned(),
            });
            index += 1;
        }
    }
    rows
}

// ---------------------------------------------------------------------------
// Tying rows to installed apps
// ---------------------------------------------------------------------------

fn match_row<'a>(row: &WingetRow, apps: &'a [Installed]) -> Option<&'a Installed> {
    // An id of the form ARP\Machine\X64\<Uninstall key name>.
    if let Some(tail) = row.id.rsplit('\\').next() {
        if row.id.contains('\\') {
            let hits: Vec<&Installed> = apps.iter().filter(|a| a.key_name.eq_ignore_ascii_case(tail)).collect();
            if hits.len() == 1 {
                return Some(hits[0]);
            }
        }
    }
    let wanted = norm(&row.name.trim_end_matches('\u{2026}'));
    if wanted.len() < 3 {
        return None;
    }
    let exact: Vec<&Installed> = apps.iter().filter(|a| norm(&a.entry.name) == wanted).collect();
    if exact.len() == 1 {
        return Some(exact[0]);
    }
    if !exact.is_empty() {
        return None;
    }
    // winget's name is often the product name and the key's name carries a version or "(x64)".
    let loose: Vec<&Installed> = apps
        .iter()
        .filter(|a| {
            let have = norm(&a.entry.name);
            wanted.len() >= 5 && (have.starts_with(&wanted) || wanted.starts_with(&have) && have.len() >= 5)
        })
        .collect();
    (loose.len() == 1).then(|| loose[0])
}

/// A target that cannot be mistaken for an option by winget.
fn safe_argument(text: &str) -> bool {
    !text.is_empty() && !text.starts_with('-') && !text.chars().any(char::is_control)
}

fn check(app: &AppHandle) -> Result<Saved, String> {
    let (code, text) = run_winget(
        &["upgrade", "--include-unknown", "--disable-interactivity", "--accept-source-agreements"],
        CHECK_TIMEOUT,
    )?;
    let rows = parse_upgrades(&text);
    if rows.is_empty() && code != 0 && code != NO_APPLICATIONS_FOUND && code != NO_APPLICABLE_UPGRADE {
        return Err(format!("winget could not list updates (exit code {code:#010X})."));
    }
    let installed = read_installed();
    let now = now_secs();
    let mut saved = Saved { checked_at: now, apps: Vec::new(), targets: HashMap::new() };
    for row in rows {
        let Some(found) = match_row(&row, &installed) else { continue };
        let path = found.entry.path.clone();
        if saved.targets.contains_key(&path) || !safe_argument(&row.id) {
            continue;
        }
        let unknown_installed = row.version.eq_ignore_ascii_case("unknown");
        let update = AppUpdate {
            path: path.clone(),
            name: found.entry.name.clone(),
            bundle_id: None,
            installed_version: found.entry.version.clone().or_else(|| (!unknown_installed).then(|| row.version.clone())),
            source: "winget".into(),
            state: "available".into(),
            latest_version: Some(row.available.clone()),
            cask: None,
            store_url: None,
            reason: unknown_installed.then(|| "winget could not read the installed version.".to_string()),
            checked_at: now,
        };
        let _ = app.emit("apps-update-row", &update);
        saved.targets.insert(path, (row.id.clone(), row.source.clone().unwrap_or_default()));
        saved.apps.push(update);
    }
    Ok(saved)
}

static UPDATES_BUSY: AtomicBool = AtomicBool::new(false);

/// Checks in the background: `apps-update-row` per app with an update, then
/// `apps-updates-done`. A check younger than six hours is reused unless `force`.
pub(super) fn refresh(app: AppHandle, force: bool) -> Result<(), String> {
    if UPDATES_BUSY.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    std::thread::spawn(move || {
        let _busy = Busy(&UPDATES_BUSY);
        if !force {
            if let Some(saved) = load_saved() {
                if now_secs() - saved.checked_at < FRESH_SECS {
                    let _ = app.emit("apps-updates-done", report_of(Some(&saved)));
                    return;
                }
            }
        }
        match check(&app) {
            Ok(saved) => {
                let _ = cache::save(UPDATES_FILE, UPDATES_FORMAT, &saved);
                let _ = app.emit("apps-updates-done", report_of(Some(&saved)));
            }
            Err(error) => {
                crate::scanner::log(&format!("winget update check failed: {error}"));
                // Keep what the last good check found; say nothing newer than it.
                let _ = app.emit("apps-updates-done", report_of(load_saved().as_ref()));
            }
        }
    });
    Ok(())
}

#[derive(Serialize, Clone)]
struct UpdateJob {
    path: String,
    /// "running", "done" or "failed".
    state: &'static str,
    message: String,
}

fn emit_job(app: &AppHandle, path: &str, state: &'static str, message: String) {
    let _ = app.emit("apps-update-job", UpdateJob { path: path.to_string(), state, message });
}

/// The last line of winget's output that says something, for a failure message.
fn last_message(text: &str) -> String {
    text.split('\n')
        .map(clean)
        .filter(|line| !line.trim().is_empty() && !is_rule(line))
        .last()
        .unwrap_or_default()
}

/// Upgrades one app with winget in the background and reports as `apps-update-job`.
/// Returns "running".
pub(super) async fn update(app: AppHandle, path: String) -> Result<String, String> {
    let target = {
        let path = path.clone();
        blocking(move || {
            let saved = load_saved().ok_or("Check for updates first.")?;
            saved
                .targets
                .get(&path)
                .cloned()
                .ok_or_else(|| "winget lists no update for this app. Check for updates again.".to_string())
        })
        .await?
    };
    let (id, source) = target;
    if !safe_argument(&id) {
        return Err("winget gave an id that cannot be used.".into());
    }
    emit_job(&app, &path, "running", "Upgrading with winget…".into());
    std::thread::spawn(move || {
        let mut args: Vec<&str> = vec![
            "upgrade",
            "--id",
            &id,
            "--exact",
            "--silent",
            "--disable-interactivity",
            "--accept-source-agreements",
            "--accept-package-agreements",
        ];
        if !source.is_empty() && safe_argument(&source) {
            args.extend(["--source", &source]);
        }
        match run_winget(&args, UPGRADE_TIMEOUT) {
            Ok((0, _)) => {
                // The old answer is stale now; drop this app so the re-check does not show it again.
                if let Some(mut saved) = load_saved() {
                    saved.apps.retain(|row| row.path != path);
                    saved.targets.remove(&path);
                    let _ = cache::save(UPDATES_FILE, UPDATES_FORMAT, &saved);
                }
                emit_job(&app, &path, "done", "Updated with winget.".into());
            }
            Ok((code, text)) => {
                let detail = last_message(&text);
                let message = if detail.is_empty() { format!("winget exit code {code:#010X}.") } else { detail };
                emit_job(&app, &path, "failed", message);
            }
            Err(error) => emit_job(&app, &path, "failed", error),
        }
    });
    Ok("running".into())
}
