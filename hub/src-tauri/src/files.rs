//! Single-item actions from the Storage folder list, search results and treemap:
//! show in Finder, move to another folder, and move to the Trash. Every
//! mutating action checks the item again right before it acts: it must still
//! exist and still be the same item (device and inode) the person chose. A
//! move never overwrites, and across drives it copies first and trashes the
//! original only after the copy is complete.

#[cfg(target_os = "macos")]
use std::ffi::CString;
#[cfg(target_os = "macos")]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Serialize)]
pub struct Identity {
    pub dev: u64,
    pub ino: u64,
    pub is_dir: bool,
}

#[derive(Serialize)]
pub struct MovePlan {
    /// Where the item would land: the chosen folder plus its name.
    pub target: String,
    /// False when the destination is on another drive, so a move needs a copy.
    pub same_volume: bool,
}

#[derive(Serialize)]
pub struct MoveResult {
    pub target: String,
    /// True when the item was copied across drives and the original trashed.
    pub copied: bool,
}

fn absolute(path: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err("Paths must be absolute.".into());
    }
    Ok(path)
}

fn stat(path: &Path) -> Result<std::fs::Metadata, String> {
    // symlink_metadata: a link is judged as the link itself, never its target.
    std::fs::symlink_metadata(path).map_err(|_| "That item is no longer there.".to_string())
}

/// The item's (device, inode) pair. On Windows these are stable hashes of the
/// volume serial and the NTFS file id, kept under 2^53 so they survive the
/// trip through the page's numbers.
#[cfg(unix)]
pub(crate) fn ids(_path: &Path, metadata: &std::fs::Metadata) -> Result<(u64, u64), String> {
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
pub(crate) fn ids(path: &Path, metadata: &std::fs::Metadata) -> Result<(u64, u64), String> {
    fn fold(text: &str) -> u64 {
        // FNV-1a, masked to 53 bits.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in text.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash & ((1 << 53) - 1)
    }
    let info = pulse_core::platform::inspect(path, metadata);
    let id = info.file_id.ok_or_else(|| "This drive does not give files a stable identity.".to_string())?;
    Ok((fold(&id.volume.id), fold(&id.id)))
}

/// Whether two items are on the same drive.
#[cfg(unix)]
fn same_drive(_a: (&Path, &std::fs::Metadata), _b: (&Path, &std::fs::Metadata), dev_a: u64, dev_b: u64) -> bool {
    dev_a == dev_b
}

#[cfg(windows)]
fn same_drive(a: (&Path, &std::fs::Metadata), b: (&Path, &std::fs::Metadata), _dev_a: u64, _dev_b: u64) -> bool {
    let first = pulse_core::platform::inspect(a.0, a.1);
    let second = pulse_core::platform::inspect(b.0, b.1);
    first.volume == second.volume
}

/// The item at `path` is still the one the person chose: same device and inode.
fn same_item(path: &Path, dev: u64, ino: u64) -> Result<std::fs::Metadata, String> {
    let metadata = stat(path)?;
    if ids(path, &metadata)? != (dev, ino) {
        return Err("That item changed since you chose it. Nothing was changed.".into());
    }
    Ok(metadata)
}

/// The item's device and inode, read when the person asks for an action on it.
#[tauri::command]
pub fn file_identity(path: String) -> Result<Identity, String> {
    let path = absolute(&path)?;
    let metadata = stat(&path)?;
    let (dev, ino) = ids(&path, &metadata)?;
    Ok(Identity { dev, ino, is_dir: metadata.is_dir() })
}

/// Show an item in Finder: a folder opens, a file is selected in its folder.
/// Read-only: it only opens a window.
#[tauri::command]
pub fn finder_open(path: String) -> Result<(), String> {
    let path = absolute(&path)?;
    let metadata = stat(&path)?;
    #[cfg(target_os = "macos")]
    {
        let mut command = std::process::Command::new("/usr/bin/open");
        if !metadata.is_dir() {
            command.arg("-R");
        }
        command.arg(&path).spawn().map(|_| ()).map_err(|e| e.to_string())
    }
    #[cfg(windows)]
    {
        crate::explorer_show(&path, metadata.is_dir())
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = (path, metadata);
        Err("Not available on this system.".to_string())
    }
}

/// The native folder picker. None when the person cancels.
#[tauri::command]
pub async fn file_choose_folder() -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(choose_folder).await.map_err(|e| e.to_string())?
}

/// The Windows folder picker (a PowerShell FolderBrowserDialog). Empty output means cancelled.
#[cfg(windows)]
fn choose_folder() -> Result<Option<String>, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let script = "Add-Type -AssemblyName System.Windows.Forms; \
        $d = New-Object System.Windows.Forms.FolderBrowserDialog; \
        $d.Description = 'Move to'; \
        if ($d.ShowDialog() -eq 'OK') { [Console]::Out.Write($d.SelectedPath) }";
    let output = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-STA", "-Command", script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("Could not open the folder picker: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "Could not open the folder picker: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok((!text.is_empty()).then_some(text))
}

#[cfg(not(windows))]
fn choose_folder() -> Result<Option<String>, String> {
    {
        let output = std::process::Command::new("/usr/bin/osascript")
            .args(["-e", "POSIX path of (choose folder with prompt \"Move to\")"])
            .output()
            .map_err(|e| e.to_string())?;
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let trimmed = text.trim_end_matches('/');
            return Ok(Some(if trimmed.is_empty() { "/".into() } else { trimmed.to_string() }));
        }
        let error = String::from_utf8_lossy(&output.stderr);
        // -128 is the picker's "User canceled" error.
        if error.contains("-128") {
            Ok(None)
        } else {
            Err(format!("Could not open the folder picker: {}", error.trim()))
        }
    }
}

/// What a move would do, checked now: the item is still the same, the
/// destination is a folder, the name is free there, and a folder is not moved
/// into itself. Changes nothing.
fn plan_move(path: &Path, dev: u64, ino: u64, destination: &Path) -> Result<(PathBuf, bool), String> {
    let metadata = same_item(path, dev, ino)?;
    let folder = std::fs::metadata(destination).map_err(|_| "That folder is not there.".to_string())?;
    if !folder.is_dir() {
        return Err("Choose a folder to move into.".into());
    }
    if path.parent() == Some(destination) {
        return Err("It is already in that folder.".into());
    }
    if metadata.is_dir() && destination.starts_with(path) {
        return Err("A folder cannot be moved into itself.".into());
    }
    let name = path.file_name().ok_or("That item has no name.")?;
    let target = destination.join(name);
    if std::fs::symlink_metadata(&target).is_ok() {
        return Err(format!("{} already exists there. Nothing was moved.", name.to_string_lossy()));
    }
    let (folder_dev, _) = ids(destination, &folder).unwrap_or((0, 0));
    let (item_dev, _) = ids(path, &metadata)?;
    Ok((target, same_drive((destination, &folder), (path, &metadata), folder_dev, item_dev)))
}

#[tauri::command]
pub async fn file_move_plan(path: String, dev: u64, ino: u64, destination: String) -> Result<MovePlan, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<MovePlan, String> {
        let path = absolute(&path)?;
        let destination = absolute(&destination)?;
        let (target, same_volume) = plan_move(&path, dev, ino, &destination)?;
        Ok(MovePlan { target: target.to_string_lossy().into_owned(), same_volume })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Move an item into another folder. Within one drive it is a rename, which
/// refuses to replace an existing name. Across drives it needs
/// `copy_across_volumes`: the copy is made first and the original goes to the
/// Trash only after that.
#[tauri::command]
pub async fn file_move(
    path: String,
    dev: u64,
    ino: u64,
    destination: String,
    copy_across_volumes: bool,
) -> Result<MoveResult, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<MoveResult, String> {
        let path = absolute(&path)?;
        let destination = absolute(&destination)?;
        let (target, same_volume) = plan_move(&path, dev, ino, &destination)?;
        if same_volume {
            rename_no_replace(&path, &target)?;
            return Ok(MoveResult { target: target.to_string_lossy().into_owned(), copied: false });
        }
        if !copy_across_volumes {
            return Err("That folder is on another drive. Confirm to copy it there and move the original to the Trash.".into());
        }
        copy_then_trash(&path, dev, ino, &target)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn renamex_np(from: *const std::ffi::c_char, to: *const std::ffi::c_char, flags: u32) -> std::ffi::c_int;
}

/// RENAME_EXCL: fail with EEXIST instead of replacing, checked by the kernel.
#[cfg(target_os = "macos")]
const RENAME_EXCL: u32 = 0x4;

/// rename(2) that never replaces an existing name.
#[cfg(target_os = "macos")]
fn rename_no_replace(from: &Path, to: &Path) -> Result<(), String> {
    let (Ok(from_c), Ok(to_c)) = (CString::new(from.as_os_str().as_bytes()), CString::new(to.as_os_str().as_bytes())) else {
        return Err("Paths cannot contain a NUL character.".into());
    };
    if unsafe { renamex_np(from_c.as_ptr(), to_c.as_ptr(), RENAME_EXCL) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    Err(match error.raw_os_error() {
        Some(17) => "That name already exists there. Nothing was moved.".into(),
        Some(18) => "That folder is on another drive. Confirm to copy it there instead.".into(),
        _ => format!("Could not move it: {error}"),
    })
}

/// Elsewhere (the app is macOS-only; this keeps the crate building): a check
/// then rename. Not atomic, but the same refusal to replace.
#[cfg(not(target_os = "macos"))]
fn rename_no_replace(from: &Path, to: &Path) -> Result<(), String> {
    if std::fs::symlink_metadata(to).is_ok() {
        return Err("That name already exists there. Nothing was moved.".into());
    }
    std::fs::rename(from, to).map_err(|e| format!("Could not move it: {e}"))
}

/// Copy the item to `target` (which must not exist), then move the original to
/// the Trash. A failed copy is taken back out to the Trash and the original is
/// left as it was. If the original cannot be trashed, both copies stay and the
/// message says where the copy is.
fn copy_then_trash(path: &Path, dev: u64, ino: u64, target: &Path) -> Result<MoveResult, String> {
    if let Err(error) = copy_item(path, target) {
        if std::fs::symlink_metadata(target).is_ok() {
            let _ = crate::cleanup::move_to_trash(target);
        }
        return Err(format!("Copy failed; the original is unchanged: {error}"));
    }
    if let Err(reason) = same_item(path, dev, ino) {
        return Err(format!("{reason} The copy is at {}; the original was left in place.", target.display()));
    }
    crate::cleanup::move_to_trash(path).map_err(|reason| {
        format!("Copied to {}, but the original could not go to the Trash: {reason}", target.display())
    })?;
    Ok(MoveResult { target: target.to_string_lossy().into_owned(), copied: true })
}

/// Recursive copy that never overwrites: every created name is new, links are
/// recreated as links, and special files are refused rather than read.
fn copy_item(source: &Path, target: &Path) -> std::io::Result<()> {
    let metadata = std::fs::symlink_metadata(source)?;
    let kind = metadata.file_type();
    if kind.is_symlink() {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(std::fs::read_link(source)?, target)
        }
        #[cfg(not(unix))]
        {
            Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "Links cannot be copied."))
        }
    } else if kind.is_dir() {
        std::fs::create_dir(target)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            copy_item(&entry.path(), &target.join(entry.file_name()))?;
        }
        std::fs::set_permissions(target, metadata.permissions())
    } else if kind.is_file() {
        let mut input = std::fs::File::open(source)?;
        let mut output = std::fs::OpenOptions::new().write(true).create_new(true).open(target)?;
        std::io::copy(&mut input, &mut output)?;
        output.set_permissions(metadata.permissions())
    } else {
        Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "Special files cannot be copied."))
    }
}

/// Move one item to the Trash after checking it is still the one chosen. The
/// home folder and the startup disk root are never trashed from here.
#[tauri::command]
pub async fn file_trash(path: String, dev: u64, ino: u64) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let path = absolute(&path)?;
        if path.parent().is_none() || path == crate::home() {
            return Err("The home folder and the startup disk cannot be moved to the Trash from here.".into());
        }
        same_item(&path, dev, ino)?;
        crate::cleanup::move_to_trash(&path)
    })
    .await
    .map_err(|e| e.to_string())?
}
