//! Bounded, read-only comparison of folder totals from two scan snapshots.
//!
//! Folder totals are scanner-attributed observations.  They do not describe
//! reclaimable space, and this module never attempts to infer hard-link
//! ownership or filesystem state.

use crate::{
    VolumeIdentity, history,
    model::{EntryKind, FolderAccounting},
    store::Snapshot,
};
use serde::{Serialize, Serializer};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Maximum number of rows returned in any one result list.
pub const MAX_FOLDER_LIMIT: usize = 1_000;

/// Match CLI wide-integer JSON semantics: serde_json can safely carry signed
/// or unsigned 64-bit values, while wider negative values stay decimal text.
fn serialize_wide<S>(value: &i128, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if let Ok(value) = i64::try_from(*value) {
        serializer.serialize_i64(value)
    } else if let Ok(value) = u64::try_from(*value) {
        serializer.serialize_u64(value)
    } else {
        serializer.serialize_str(&value.to_string())
    }
}

/// One folder's signed change between two comparable snapshots.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FolderGrowth {
    pub volume: VolumeIdentity,
    pub path: PathBuf,
    pub previous_logical_bytes: u64,
    pub current_logical_bytes: u64,
    #[serde(serialize_with = "serialize_wide")]
    pub logical_growth_bytes: i128,
    pub previous_attributed_allocation_bytes: u64,
    pub current_attributed_allocation_bytes: u64,
    #[serde(serialize_with = "serialize_wide")]
    pub attributed_growth_bytes: i128,
}

/// Bounded folder-level history comparison.
///
/// `top_growth` and `top_shrink` contain changes for folders present in both
/// snapshots. `added_folders` and `removed_folders` contain one-sided rows,
/// represented as growth from or to zero. Every list has at most `limit`
/// entries. `truncated` is true when any eligible row was omitted. If caller
/// requests more than [`MAX_FOLDER_LIMIT`], `limit` is capped & `reasons`
/// reports that bound while comparison remains valid.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FolderGrowthComparison {
    pub comparable: bool,
    pub limit: usize,
    pub top_growth: Vec<FolderGrowth>,
    pub top_shrink: Vec<FolderGrowth>,
    pub added_folders: Vec<FolderGrowth>,
    pub removed_folders: Vec<FolderGrowth>,
    pub truncated: bool,
    pub reasons: Vec<String>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct FolderKey {
    volume: VolumeIdentity,
    path: PathBuf,
}

impl FolderKey {
    fn new(folder: &FolderAccounting) -> Self {
        Self {
            volume: folder.volume.clone(),
            path: folder.path.clone(),
        }
    }
}

fn unavailable(limit: usize, mut reasons: Vec<String>) -> FolderGrowthComparison {
    reasons.sort();
    reasons.dedup();
    FolderGrowthComparison {
        comparable: false,
        limit,
        top_growth: Vec::new(),
        top_shrink: Vec::new(),
        added_folders: Vec::new(),
        removed_folders: Vec::new(),
        truncated: false,
        reasons,
    }
}

fn valid_volume(volume: &VolumeIdentity) -> bool {
    let id = volume.id.trim();
    !id.is_empty() && !id.eq_ignore_ascii_case("unknown")
}

fn has_dot_component(path: &Path) -> bool {
    path.to_string_lossy()
        .split(['/', '\\'])
        .any(|component| component == "." || component == "..")
}

fn inside_roots(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

fn folder_map(snapshot: &Snapshot) -> Result<BTreeMap<FolderKey, FolderAccounting>, Vec<String>> {
    let mut reasons = Vec::new();
    let roots = &snapshot.report.roots;
    let observed_volumes: BTreeSet<_> = snapshot
        .report
        .volume_usage
        .iter()
        .map(|usage| usage.volume.id.as_str())
        .collect();
    let mut folders = BTreeMap::new();

    for root in roots {
        if has_dot_component(root) {
            reasons.push(format!(
                "selected scan root `{}` contains `.` or `..` path component",
                root.display()
            ));
        }
    }
    if snapshot
        .report
        .entries
        .iter()
        .any(|entry| entry.metadata.kind == EntryKind::Directory)
        && snapshot.report.folders.is_empty()
    {
        reasons.push("folder coverage is empty for a nonempty scan report".to_string());
    }

    if !snapshot.report.incomplete_reasons.is_empty()
        || !snapshot.report.inspection_errors.is_empty()
    {
        reasons.push("snapshot report contains incomplete inspection evidence".to_string());
    }

    for folder in &snapshot.report.folders {
        if folder.path.as_os_str().is_empty() {
            reasons.push("folder row has an empty path".to_string());
        }
        if has_dot_component(&folder.path) {
            reasons.push(format!(
                "folder `{}` contains `.` or `..` path component",
                folder.path.display()
            ));
        }
        if !valid_volume(&folder.volume) {
            reasons.push(format!(
                "folder `{}` has empty or unknown volume identity",
                folder.path.display()
            ));
        } else if !observed_volumes.contains(folder.volume.id.as_str()) {
            reasons.push(format!(
                "folder `{}` has unobserved volume identity `{}`",
                folder.path.display(),
                folder.volume.id
            ));
        }
        if !inside_roots(&folder.path, roots) {
            reasons.push(format!(
                "folder `{}` is outside selected scan roots",
                folder.path.display()
            ));
        }
        if folder.incomplete {
            reasons.push(format!(
                "folder `{}` accounting is incomplete",
                folder.path.display()
            ));
        }

        let key = FolderKey::new(folder);
        if folders.insert(key, folder.clone()).is_some() {
            reasons.push(format!(
                "duplicate folder row for volume `{}` and path `{}`",
                folder.volume.id,
                folder.path.display()
            ));
        }
    }

    if reasons.is_empty() {
        Ok(folders)
    } else {
        Err(reasons)
    }
}

fn growth(
    key: &FolderKey,
    previous: Option<&FolderAccounting>,
    current: Option<&FolderAccounting>,
) -> FolderGrowth {
    let previous_logical = previous.map_or(0, |folder| folder.logical_bytes);
    let current_logical = current.map_or(0, |folder| folder.logical_bytes);
    let previous_attributed = previous.map_or(0, |folder| folder.attributed_allocation_bytes);
    let current_attributed = current.map_or(0, |folder| folder.attributed_allocation_bytes);
    FolderGrowth {
        volume: key.volume.clone(),
        path: key.path.clone(),
        previous_logical_bytes: previous_logical,
        current_logical_bytes: current_logical,
        logical_growth_bytes: i128::from(current_logical) - i128::from(previous_logical),
        previous_attributed_allocation_bytes: previous_attributed,
        current_attributed_allocation_bytes: current_attributed,
        attributed_growth_bytes: i128::from(current_attributed) - i128::from(previous_attributed),
    }
}

fn growth_order(left: &FolderGrowth, right: &FolderGrowth) -> std::cmp::Ordering {
    right
        .attributed_growth_bytes
        .cmp(&left.attributed_growth_bytes)
        .then_with(|| right.logical_growth_bytes.cmp(&left.logical_growth_bytes))
        .then_with(|| left.volume.cmp(&right.volume))
        .then_with(|| left.path.cmp(&right.path))
}

fn shrink_order(left: &FolderGrowth, right: &FolderGrowth) -> std::cmp::Ordering {
    left.attributed_growth_bytes
        .cmp(&right.attributed_growth_bytes)
        .then_with(|| left.logical_growth_bytes.cmp(&right.logical_growth_bytes))
        .then_with(|| left.volume.cmp(&right.volume))
        .then_with(|| left.path.cmp(&right.path))
}

fn path_order(left: &FolderGrowth, right: &FolderGrowth) -> std::cmp::Ordering {
    left.volume
        .cmp(&right.volume)
        .then_with(|| left.path.cmp(&right.path))
}

fn bounded(mut rows: Vec<FolderGrowth>, limit: usize) -> (Vec<FolderGrowth>, bool) {
    let truncated = rows.len() > limit;
    rows.truncate(limit);
    (rows, truncated)
}

/// Compare folder totals from two stored snapshots.
///
/// The general snapshot compatibility guard from [`history::compare`] runs
/// first. Any incomplete report, changed roots, remount, missing identity,
/// duplicate row, or out-of-scope folder makes the result non-comparable and
/// leaves all totals empty.
pub fn compare_folders(
    previous: &Snapshot,
    current: &Snapshot,
    limit: usize,
) -> FolderGrowthComparison {
    let effective_limit = limit.min(MAX_FOLDER_LIMIT);
    let limit_reason = (limit > MAX_FOLDER_LIMIT).then(|| {
        format!(
            "requested folder limit {} exceeds maximum {}; capped",
            limit, MAX_FOLDER_LIMIT
        )
    });
    let compatibility = history::compare(previous, current);
    if !compatibility.comparable {
        let mut reasons = compatibility.reasons;
        if let Some(reason) = &limit_reason {
            reasons.push(reason.clone());
        }
        return unavailable(effective_limit, reasons);
    }

    let previous_folders = match folder_map(previous) {
        Ok(folders) => folders,
        Err(mut reasons) => {
            if let Some(reason) = &limit_reason {
                reasons.push(reason.clone());
            }
            return unavailable(effective_limit, reasons);
        }
    };
    let current_folders = match folder_map(current) {
        Ok(folders) => folders,
        Err(mut reasons) => {
            if let Some(reason) = &limit_reason {
                reasons.push(reason.clone());
            }
            return unavailable(effective_limit, reasons);
        }
    };

    let keys: BTreeSet<_> = previous_folders
        .keys()
        .chain(current_folders.keys())
        .cloned()
        .collect();
    let mut growth_rows = Vec::new();
    let mut shrink_rows = Vec::new();
    let mut added_rows = Vec::new();
    let mut removed_rows = Vec::new();

    for key in keys {
        let before = previous_folders.get(&key);
        let after = current_folders.get(&key);
        let row = growth(&key, before, after);
        match (before.is_some(), after.is_some()) {
            (false, true) => added_rows.push(row),
            (true, false) => removed_rows.push(row),
            // Attributed allocation is the accounting-primary direction. A
            // logical-only change decides direction only when allocation is
            // unchanged; both signed values remain in every returned row.
            (true, true)
                if row.attributed_growth_bytes > 0
                    || (row.attributed_growth_bytes == 0 && row.logical_growth_bytes > 0) =>
            {
                growth_rows.push(row)
            }
            (true, true)
                if row.attributed_growth_bytes < 0
                    || (row.attributed_growth_bytes == 0 && row.logical_growth_bytes < 0) =>
            {
                shrink_rows.push(row)
            }
            (true, true) => {}
            (false, false) => unreachable!("union of folder keys contains an absent row"),
        }
    }

    growth_rows.sort_by(growth_order);
    shrink_rows.sort_by(shrink_order);
    added_rows.sort_by(path_order);
    removed_rows.sort_by(path_order);

    let (top_growth, growth_truncated) = bounded(growth_rows, effective_limit);
    let (top_shrink, shrink_truncated) = bounded(shrink_rows, effective_limit);
    let (added_folders, added_truncated) = bounded(added_rows, effective_limit);
    let (removed_folders, removed_truncated) = bounded(removed_rows, effective_limit);

    let mut reasons = Vec::new();
    if let Some(reason) = limit_reason {
        reasons.push(reason);
    }

    FolderGrowthComparison {
        comparable: true,
        limit: effective_limit,
        top_growth,
        top_shrink,
        added_folders,
        removed_folders,
        truncated: growth_truncated || shrink_truncated || added_truncated || removed_truncated,
        reasons,
    }
}
