//! Unix (stat-based) adapter. macOS adds a volume UUID lookup.

use super::NativeInfo;
use crate::model::{FileIdentity, FsError, SnapshotState, VolumeIdentity, VolumeUsage};
use std::ffi::{CStr, CString};
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
use std::os::macos::fs::MetadataExt as MacMetadataExt;

pub(super) fn inspect(path: &Path, metadata: &fs::Metadata) -> NativeInfo {
    let mut unavailable = Vec::new();
    let (volume, volume_stable, volume_reason) = volume_for(path, metadata);
    if let Some(reason) = volume_reason {
        unavailable.push(reason);
    }
    // File identity is st_dev + st_ino; directories get one too.
    let file_id = Some(FileIdentity {
        volume: volume.clone(),
        id: format!("{}:{}", metadata.dev(), metadata.ino()),
    });
    let is_placeholder = is_placeholder(metadata);
    let allocation_size = if is_placeholder {
        unavailable.push(format!(
            "allocation unavailable: {} is a dataless placeholder",
            path.display()
        ));
        None
    } else {
        // st_blocks is always in 512-byte units, independent of st_blksize.
        Some(metadata.blocks().saturating_mul(512))
    };
    NativeInfo {
        volume,
        volume_stable,
        file_id,
        allocation_size,
        is_placeholder,
        unavailable,
    }
}

/// Returns (identity, stable, reason-if-not-stable).
pub(super) fn volume_for(
    path: &Path,
    metadata: &fs::Metadata,
) -> (VolumeIdentity, bool, Option<String>) {
    #[cfg(target_os = "macos")]
    {
        super::mac_native::volume_for(path, metadata)
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Non-macOS Unix is not a shipping target: st_dev identity is
        // ephemeral and documented as such; it is not flagged per entry.
        let _ = path;
        (VolumeIdentity::new(metadata.dev().to_string()), true, None)
    }
}

#[cfg(target_os = "macos")]
fn is_placeholder(metadata: &fs::Metadata) -> bool {
    // SF_DATALESS. Reading stat flags does not hydrate an item.
    metadata.st_flags() & SF_DATALESS != 0
}

#[cfg(not(target_os = "macos"))]
fn is_placeholder(_metadata: &fs::Metadata) -> bool {
    false
}

const SF_DATALESS: u32 = 0x4000_0000;

fn os_error(error: std::io::Error) -> FsError {
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        FsError::permission_denied(error.to_string())
    } else {
        FsError::new(error.to_string())
    }
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
fn clear_errno() {
    unsafe { *libc::__error() = 0 };
}
#[cfg(any(target_os = "linux", target_os = "emscripten"))]
fn clear_errno() {
    unsafe { *libc::__errno_location() = 0 };
}
#[cfg(target_os = "android")]
fn clear_errno() {
    unsafe { *libc::__errno() = 0 };
}
#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "linux",
    target_os = "emscripten",
    target_os = "android"
)))]
fn clear_errno() {}

fn raw_placeholder(stat: &libc::stat) -> bool {
    #[cfg(target_os = "macos")]
    {
        stat.st_flags & SF_DATALESS != 0
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = stat;
        false
    }
}

/// Closes a `DIR*` (and with it the descriptor) on every exit path.
struct DirStream(*mut libc::DIR);

impl Drop for DirStream {
    fn drop(&mut self) {
        unsafe { libc::closedir(self.0) };
    }
}

/// Owns a file descriptor for the duration of the directory walk.
struct Fd(libc::c_int);

impl Drop for Fd {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

fn fstat_fd(fd: libc::c_int) -> Result<libc::stat, FsError> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
        return Err(os_error(std::io::Error::last_os_error()));
    }
    Ok(unsafe { stat.assume_init() })
}

/// Opens `name` relative to `base` with O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC.
/// Because the lookup starts from a held descriptor, renaming or swapping a
/// pathname component above `base` cannot redirect this step, and O_NOFOLLOW
/// refuses `name` itself if it is a link.
fn open_dir_at(base: libc::c_int, name: &CStr) -> Result<Fd, FsError> {
    let fd = unsafe {
        libc::openat(
            base,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(os_error(std::io::Error::last_os_error()));
    }
    Ok(Fd(fd))
}

/// Walks `path` one component at a time, each step relative to the descriptor
/// of the previously opened directory, so the whole ancestor chain is pinned
/// against concurrent renames/symlink swaps. Returns the descriptor of the
/// final directory after verifying its fstat (st_dev, st_ino) equals `before`,
/// the lstat identity the caller used to accept the path.
///
/// Remaining unpinned step: for a relative `path` the base descriptor is the
/// process working directory captured at call time (`open "."`); callers pass
/// absolute paths, where the chain starts at the pinned filesystem root.
fn pin_directory(path: &Path, before: &libc::stat) -> Result<Fd, FsError> {
    static ROOT: &CStr = c"/";
    static DOT: &CStr = c".";
    static DOTDOT: &CStr = c"..";
    let mut current = if path.is_absolute() {
        open_dir_at(libc::AT_FDCWD, ROOT)
            .map_err(|error| FsError::new(format!("cannot pin filesystem root: {error}")))?
    } else {
        open_dir_at(libc::AT_FDCWD, DOT)
            .map_err(|error| FsError::new(format!("cannot pin working directory: {error}")))?
    };
    for component in path.components() {
        let next = match component {
            std::path::Component::RootDir | std::path::Component::CurDir => continue,
            std::path::Component::ParentDir => open_dir_at(current.0, DOTDOT),
            std::path::Component::Normal(name) => {
                // A ".." that escaped normalization would still resolve
                // relative to the held descriptor, never a swapped ancestor.
                let bytes = name.as_bytes();
                let c_name = CString::new(bytes)
                    .map_err(|_| FsError::new("component contains NUL; directory not listed"))?;
                open_dir_at(current.0, &c_name)
            }
            std::path::Component::Prefix(_) => {
                return Err(FsError::new("path prefix unsupported on Unix"));
            }
        };
        current = next.map_err(|error| {
            FsError::new(format!(
                "cannot pin directory component of {}: {error}",
                path.display()
            ))
        })?;
    }
    let opened = fstat_fd(current.0)?;
    if opened.st_dev != before.st_dev || opened.st_ino != before.st_ino {
        return Err(FsError::new(format!(
            "directory identity changed before listing: {}",
            path.display()
        )));
    }
    if raw_placeholder(&opened) {
        return Err(FsError::new(format!(
            "placeholder directory not listed: {}",
            path.display()
        )));
    }
    Ok(current)
}

/// Descriptor-pinned, no-follow, bounded listing. The path is lstat-ed for
/// acceptance checks, then every ancestor component is opened with
/// openat(O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC) relative to the previous
/// descriptor, and the final descriptor's (st_dev, st_ino) must match the
/// lstat or the listing is refused. Enumeration (fdopendir+readdir) runs on
/// that held descriptor, so an ancestor swap after pinning cannot redirect
/// the listing. Placeholders are refused before any entry is read. At most
/// `limit` entries plus one probe are read.
pub(super) fn children_bounded(path: &Path, limit: usize) -> Result<(Vec<PathBuf>, bool), FsError> {
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| FsError::new("path contains NUL; directory not listed"))?;
    let mut before = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::lstat(c_path.as_ptr(), before.as_mut_ptr()) } != 0 {
        return Err(os_error(std::io::Error::last_os_error()));
    }
    let before = unsafe { before.assume_init() };
    if before.st_mode & libc::S_IFMT != libc::S_IFDIR {
        return Err(FsError::new(format!(
            "not a real directory (links are not followed): {}",
            path.display()
        )));
    }
    if raw_placeholder(&before) {
        return Err(FsError::new(format!(
            "placeholder directory not listed: {}",
            path.display()
        )));
    }
    let pinned = pin_directory(path, &before)?;
    // fdopendir takes ownership of the descriptor on success.
    let fd = pinned.0;
    std::mem::forget(pinned);
    let dir = unsafe { libc::fdopendir(fd) };
    if dir.is_null() {
        let error = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(os_error(error));
    }
    let stream = DirStream(dir);
    let mut children = Vec::new();
    let mut truncated = false;
    loop {
        clear_errno();
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error().is_some_and(|code| code != 0) {
                return Err(os_error(error));
            }
            break;
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        if children.len() >= limit {
            truncated = true;
            break;
        }
        children.push(path.join(std::ffi::OsString::from_vec(name.to_vec())));
    }
    drop(stream);
    children.sort();
    Ok((children, truncated))
}

/// macOS: the same descriptor-pinned, no-follow listing as `children_bounded`,
/// read with one bulk call that also returns each regular file's facts (see
/// `mac_bulk`). Same refusals, same limit; the pinned descriptor is closed on
/// every exit path.
#[cfg(target_os = "macos")]
pub(super) fn bulk_children_bounded(
    path: &Path,
    limit: usize,
) -> Result<super::mac_bulk::BulkChildren, FsError> {
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| FsError::new("path contains NUL; directory not listed"))?;
    let mut before = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::lstat(c_path.as_ptr(), before.as_mut_ptr()) } != 0 {
        return Err(os_error(std::io::Error::last_os_error()));
    }
    let before = unsafe { before.assume_init() };
    if before.st_mode & libc::S_IFMT != libc::S_IFDIR {
        return Err(FsError::new(format!(
            "not a real directory (links are not followed): {}",
            path.display()
        )));
    }
    if raw_placeholder(&before) {
        return Err(FsError::new(format!(
            "placeholder directory not listed: {}",
            path.display()
        )));
    }
    let pinned = pin_directory(path, &before)?;
    super::mac_bulk::read_entries(pinned.0, path, limit).map_err(os_error)
}

// statvfs field widths vary across Unix ABIs.
#[allow(clippy::unnecessary_cast)]
pub(super) fn volume_usage(volume: &VolumeIdentity) -> Result<VolumeUsage, FsError> {
    for disk in sysinfo::Disks::new_with_refreshed_list().list() {
        let mount = disk.mount_point();
        // Same identity function as inspection, so UUID ids match.
        let Ok(mount_metadata) = fs::symlink_metadata(mount) else {
            continue;
        };
        if volume_for(mount, &mount_metadata).0.id != volume.id {
            continue;
        }
        let Ok(path) = CString::new(mount.as_os_str().as_bytes()) else {
            continue;
        };
        let mut statistics = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // statvfs reads mount accounting only, never file contents.
        if unsafe { libc::statvfs(path.as_ptr(), statistics.as_mut_ptr()) } != 0 {
            continue;
        }
        let statistics = unsafe { statistics.assume_init() };
        let unit = statistics.f_frsize as u64;
        let total = (statistics.f_blocks as u64).saturating_mul(unit);
        let free = (statistics.f_bfree as u64).saturating_mul(unit);
        let available = (statistics.f_bavail as u64).saturating_mul(unit);
        return Ok(VolumeUsage {
            volume: volume.clone(),
            total_bytes: Some(total),
            used_bytes: Some(total.saturating_sub(free)),
            available_bytes: Some(available),
            purgeable_bytes: None,
            snapshots: SnapshotState::Unknown,
        });
    }
    // Unsupported or inaccessible mount accounting stays explicitly unknown.
    Ok(VolumeUsage {
        volume: volume.clone(),
        total_bytes: None,
        used_bytes: None,
        available_bytes: None,
        purgeable_bytes: None,
        snapshots: SnapshotState::Unknown,
    })
}
