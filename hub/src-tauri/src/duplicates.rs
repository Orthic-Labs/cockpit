//! Duplicate copies in a folder the user picks. Detection reads file contents
//! within core's bounds and only reports. The one effect is moving a verified
//! extra copy to the Trash through the cleanup trash path. The kept copy is
//! never touched, and nothing is deleted.

use std::path::{Path, PathBuf};

use pulse_core::duplicates::{self as dup, DuplicateOptions, DuplicateReport};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct TrashItem {
    /// The copy that stays.
    pub kept: String,
    /// The extra copy to move to the Trash.
    pub path: String,
}

#[derive(Serialize)]
pub struct Moved {
    pub path: String,
    pub bytes: u64,
}

#[derive(Serialize)]
pub struct Skipped {
    pub path: String,
    pub reason: String,
}

#[derive(Serialize, Default)]
pub struct TrashResult {
    pub moved: Vec<Moved>,
    pub skipped: Vec<Skipped>,
    pub moved_bytes: u64,
}

/// The folder the Duplicates tab starts in.
#[tauri::command]
pub fn home_path() -> String {
    crate::home().to_string_lossy().into_owned()
}

/// Exact-content duplicates under one folder. Bounded by core's file, read
/// and time limits; runs off the UI thread.
#[tauri::command]
pub async fn duplicates_scan(path: String) -> Result<DuplicateReport, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<DuplicateReport, String> {
        let root = PathBuf::from(&path);
        if !root.is_absolute() {
            return Err("Choose a folder by its full path.".into());
        }
        let metadata = std::fs::symlink_metadata(&root).map_err(|_| "That folder is not there.".to_string())?;
        if !metadata.is_dir() {
            return Err("Choose a folder, not a file or a link.".into());
        }
        Ok(dup::find_duplicates(&[root], &DuplicateOptions::default()))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Move each extra copy to the Trash after checking it again: it is still a
/// regular file, not the kept file itself or a hard link to it, and still
/// identical to the kept copy. Anything that fails a check is skipped.
#[tauri::command]
pub async fn duplicates_trash(items: Vec<TrashItem>) -> Result<TrashResult, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<TrashResult, String> {
        let mut result = TrashResult::default();
        for item in items {
            match verify(&item) {
                Ok(bytes) => match crate::cleanup::move_to_trash(Path::new(&item.path)) {
                    Ok(()) => {
                        result.moved_bytes += bytes;
                        result.moved.push(Moved { path: item.path.clone(), bytes });
                    }
                    Err(reason) => result.skipped.push(Skipped { path: item.path.clone(), reason }),
                },
                Err(reason) => result.skipped.push(Skipped { path: item.path.clone(), reason }),
            }
        }
        Ok(result)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn regular_file(path: &Path) -> Result<std::fs::Metadata, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| "Already gone.".to_string())?;
    if metadata.file_type().is_symlink() {
        return Err("Is a link now.".into());
    }
    if !metadata.is_file() {
        return Err("No longer a regular file.".into());
    }
    Ok(metadata)
}

#[cfg(unix)]
fn same_file(_: &Path, a: &std::fs::Metadata, _: &Path, b: &std::fs::Metadata) -> Result<bool, String> {
    use std::os::unix::fs::MetadataExt;
    Ok(a.dev() == b.dev() && a.ino() == b.ino())
}

/// Windows: the same NTFS file id on the same volume serial means one file with two
/// names (a hard link). A drive that gives no stable file id cannot prove two paths
/// are different files, so nothing is moved.
#[cfg(windows)]
fn same_file(a_path: &Path, a: &std::fs::Metadata, b_path: &Path, b: &std::fs::Metadata) -> Result<bool, String> {
    let first = crate::files::ids(a_path, a)?;
    let second = crate::files::ids(b_path, b)?;
    Ok(first == second)
}

#[cfg(not(any(unix, windows)))]
fn same_file(_: &Path, _: &std::fs::Metadata, _: &Path, _: &std::fs::Metadata) -> Result<bool, String> {
    Ok(false)
}

/// Bytes the extra copy occupies, when it is safe to move it now.
fn verify(item: &TrashItem) -> Result<u64, String> {
    let kept = Path::new(&item.kept);
    let extra = Path::new(&item.path);
    if !kept.is_absolute() || !extra.is_absolute() {
        return Err("Paths must be absolute.".into());
    }
    if kept == extra {
        return Err("That is the copy being kept.".into());
    }
    let extra_metadata = regular_file(extra)?;
    let kept_metadata = regular_file(kept)?;
    if same_file(extra, &extra_metadata, kept, &kept_metadata)? {
        return Err("A hard link to the kept copy; moving it frees no space.".into());
    }
    // Identical now, not just at scan time. Core only groups files it can read
    // completely (no placeholders), so an unreadable pair is never grouped.
    let pair = [kept.to_path_buf(), extra.to_path_buf()];
    let report = dup::find_duplicates(&pair, &DuplicateOptions { min_bytes: 0, ..Default::default() });
    let together = report.groups.iter().any(|group| {
        let mut members = vec![group.kept_path.as_path()];
        members.extend(group.extras.iter().map(PathBuf::as_path));
        members.contains(&kept) && members.contains(&extra)
    });
    if !together {
        return Err("No longer an identical copy of the kept file.".into());
    }
    Ok(extra_metadata.len())
}
