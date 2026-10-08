//! Folder growth between home scans. Each completed home scan is saved to the
//! core's snapshot store (the same state directory the CLI uses); `growth`
//! compares the newest snapshot with the latest earlier one that the core
//! accepts as comparable. Read-only apart from writing and pruning the hub's
//! own scan snapshots.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use pulse_core::folder_growth::compare_folders;
use pulse_core::store::{self, Snapshot};
use pulse_core::ScanReport;
use serde::Serialize;

/// Snapshots of the same roots kept after a save.
const KEEP: usize = 4;
/// Grown / shrunk folders returned.
const GROWN: usize = 5;
const SHRUNK: usize = 3;

/// Saves still running in the background.
static PENDING: AtomicUsize = AtomicUsize::new(0);

#[derive(Serialize)]
pub struct Change {
    path: PathBuf,
    /// Signed change in attributed bytes.
    bytes: i64,
}

#[derive(Serialize, Default)]
pub struct Growth {
    available: bool,
    /// Unix seconds of the scan being compared against.
    since: Option<u64>,
    grown: Vec<Change>,
    shrunk: Vec<Change>,
    reason: Option<String>,
}

/// Save a snapshot of `report` on a background thread and keep the newest
/// `KEEP` snapshots for the same roots. Failures are ignored: history is a
/// convenience and must never get in the way of the scan.
pub fn save_in_background(report: &ScanReport) {
    // Per-file entries are not needed for folder comparison and would push the
    // snapshot past the store's size cap on a home folder.
    let slim = ScanReport {
        roots: report.roots.clone(),
        entries: Vec::new(),
        folders: report.folders.clone(),
        accounting: report.accounting.clone(),
        volume_usage: report.volume_usage.clone(),
        volume_deltas: report.volume_deltas.clone(),
        inspection_errors: report.inspection_errors.clone(),
        skipped_links: Vec::new(),
        incomplete_reasons: report.incomplete_reasons.clone(),
    };
    PENDING.fetch_add(1, Ordering::SeqCst);
    std::thread::spawn(move || {
        let _ = save_and_prune(slim);
        PENDING.fetch_sub(1, Ordering::SeqCst);
    });
}

/// The newest saved scan of exactly `root`, with its unix time. Used to show
/// something on launch without scanning.
pub fn latest_scan(root: &Path) -> Option<(ScanReport, u64)> {
    // A save in progress finishes first.
    for _ in 0..300 {
        if PENDING.load(Ordering::SeqCst) == 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let dir = store::default_directory().ok()?;
    let all = store::history(&dir).ok()?;
    all.into_iter()
        .rev()
        .find(|s| s.report.roots.len() == 1 && s.report.roots[0] == root && !s.report.folders.is_empty())
        .map(|s| (s.report, s.created_at))
}

fn save_and_prune(report: ScanReport) -> Result<(), String> {
    let dir = store::default_directory().map_err(|e| e.to_string())?;
    let roots = report.roots.clone();
    store::save(&dir, &Snapshot::new(report, Vec::new())).map_err(|e| e.to_string())?;
    let mut mine: Vec<Snapshot> = store::history(&dir)
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|s| s.report.roots == roots)
        .collect();
    // History is ordered oldest first.
    while mine.len() > KEEP {
        let old = mine.remove(0);
        let _ = std::fs::remove_file(dir.join(format!("{}.json", old.id)));
    }
    Ok(())
}

fn compute() -> Result<Growth, String> {
    // A save that just started finishes first so the newest snapshot is this scan.
    for _ in 0..300 {
        if PENDING.load(Ordering::SeqCst) == 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let dir = store::default_directory().map_err(|e| e.to_string())?;
    let all = store::history(&dir).map_err(|e| e.to_string())?;
    let Some(current) = all.last() else {
        return Ok(unavailable("No saved scan yet."));
    };
    let state_dir = dir.as_path();
    let mut why = String::from("No earlier scan of this folder to compare with.");
    for previous in all[..all.len() - 1].iter().rev() {
        if previous.report.roots != current.report.roots {
            continue;
        }
        let comparison = compare_folders(previous, current, 1_000);
        if !comparison.comparable {
            why = comparison.reasons.first().cloned().unwrap_or(why);
            continue;
        }
        let mut grown = Vec::new();
        let mut shrunk = Vec::new();
        for row in comparison
            .top_growth
            .iter()
            .chain(&comparison.added_folders)
            .chain(&comparison.top_shrink)
            .chain(&comparison.removed_folders)
        {
            // The scan sees its own snapshots; they are not the user's data.
            if row.path.starts_with(state_dir) || current.report.roots.contains(&row.path) {
                continue;
            }
            let bytes = i64::try_from(row.attributed_growth_bytes).unwrap_or(0);
            if bytes > 0 {
                grown.push(Change { path: row.path.clone(), bytes });
            } else if bytes < 0 {
                shrunk.push(Change { path: row.path.clone(), bytes });
            }
        }
        return Ok(Growth {
            available: true,
            since: Some(previous.created_at),
            grown: pick(grown, true, GROWN),
            shrunk: pick(shrunk, false, SHRUNK),
            reason: None,
        });
    }
    Ok(unavailable(&why))
}

fn unavailable(reason: &str) -> Growth {
    Growth { reason: Some(reason.to_string()), ..Growth::default() }
}

/// Largest changes first, without listing a folder whose change is almost
/// entirely explained by one of its own subfolders (the subfolder is the news).
fn pick(mut rows: Vec<Change>, up: bool, take: usize) -> Vec<Change> {
    rows.sort_by(|a, b| if up { b.bytes.cmp(&a.bytes) } else { a.bytes.cmp(&b.bytes) });
    rows.dedup_by(|a, b| a.path == b.path);
    let explained = |outer: &Change| {
        rows.iter().any(|inner| {
            inner.path != outer.path
                && is_inside(&inner.path, &outer.path)
                && (inner.bytes as i128).abs() * 10 >= (outer.bytes as i128).abs() * 8
        })
    };
    let keep: Vec<bool> = rows.iter().map(|r| !explained(r)).collect();
    rows.into_iter()
        .zip(keep)
        .filter_map(|(r, k)| k.then_some(r))
        .take(take)
        .collect()
}

fn is_inside(path: &Path, ancestor: &Path) -> bool {
    path.starts_with(ancestor)
}

/// Top grown and shrunk folders since the previous comparable home scan.
#[tauri::command]
pub async fn growth() -> Result<Growth, String> {
    tauri::async_runtime::spawn_blocking(compute)
        .await
        .map_err(|e| e.to_string())?
}
