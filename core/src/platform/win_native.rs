//! Windows adapter. Opens the item with attribute-only access, backup
//! semantics and OPEN_REPARSE_POINT, so content is never read, reparse points
//! are never followed and cloud placeholders are never recalled.

use super::NativeInfo;
use crate::model::{FileIdentity, FsError, SnapshotState, VolumeIdentity, VolumeUsage};
use std::ffi::{OsString, c_void};
use std::fs;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, ERROR_MORE_DATA,
    ERROR_NO_MORE_FILES, ERROR_NOT_SUPPORTED, HANDLE,
};
use windows::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_NO_RECALL,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_BOTH_DIR_INFO, FILE_ID_EXTD_DIR_INFO, FILE_ID_INFO,
    FILE_INFO_BY_HANDLE_CLASS, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FILE_STANDARD_INFO, FileIdBothDirectoryInfo, FileIdBothDirectoryRestartInfo,
    FileIdExtdDirectoryInfo, FileIdExtdDirectoryRestartInfo, FileIdInfo, FileStandardInfo,
    FindFirstVolumeW, FindNextVolumeW, FindVolumeClose, GetDiskFreeSpaceExW,
    GetFileInformationByHandle, GetFileInformationByHandleEx, GetVolumePathNamesForVolumeNameW,
    OPEN_EXISTING,
};
use windows::core::PCWSTR;

/// FILE_READ_ATTRIBUTES: attributes only, no data access.
const ACCESS_READ_ATTRIBUTES: u32 = 0x0000_0080;
/// FILE_LIST_DIRECTORY: enumerate a directory; never FILE_READ_DATA on files.
const ACCESS_LIST_DIRECTORY: u32 = 0x0000_0001;
const ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
const ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
const ATTRIBUTE_OFFLINE: u32 = 0x0000_1000;
const ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x0004_0000;
const ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        // Closing an attribute-only handle has no content side effect.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

pub(super) fn inspect(path: &Path, metadata: &fs::Metadata) -> NativeInfo {
    let mut unavailable = Vec::new();
    let fallback_volume = || {
        VolumeIdentity::new(format!(
            "path-prefix-unstable:{}",
            path.components()
                .next()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .unwrap_or_else(|| "unknown".to_owned())
        ))
    };
    // Cheap attribute pre-check from the already-read metadata.
    let std_attributes = std::os::windows::fs::MetadataExt::file_attributes(metadata);
    let mut is_placeholder = placeholder_attributes(std_attributes);

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let opened = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            ACCESS_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_OPEN_NO_RECALL,
            None,
        )
    };
    let handle = match opened {
        Ok(handle) => Handle(handle),
        Err(error) => {
            unavailable.push(format!("attribute-only open failed: {error}"));
            unavailable.push("volume identity not stable (path prefix fallback)".into());
            unavailable.push("file id unavailable".into());
            unavailable.push("allocation size unavailable".into());
            return NativeInfo {
                volume: fallback_volume(),
                volume_stable: false,
                file_id: None,
                allocation_size: None,
                is_placeholder,
                unavailable,
            };
        }
    };

    let mut basic = BY_HANDLE_FILE_INFORMATION::default();
    let basic_ok = unsafe { GetFileInformationByHandle(handle.0, &mut basic) }.is_ok();
    if basic_ok {
        is_placeholder |= placeholder_attributes(basic.dwFileAttributes);
    } else {
        unavailable.push("GetFileInformationByHandle failed".into());
    }

    let mut id_info = FILE_ID_INFO::default();
    let id_ok = unsafe {
        GetFileInformationByHandleEx(
            handle.0,
            FileIdInfo,
            (&mut id_info as *mut FILE_ID_INFO).cast::<c_void>(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    }
    .is_ok();

    let (volume, volume_stable) = if id_ok {
        (
            VolumeIdentity::new(format!("serial:{:016x}", id_info.VolumeSerialNumber)),
            true,
        )
    } else if basic_ok {
        unavailable.push("volume identity not stable (32-bit serial fallback)".into());
        (
            VolumeIdentity::new(format!(
                "serial32-unstable:{:08x}",
                basic.dwVolumeSerialNumber
            )),
            false,
        )
    } else {
        unavailable.push("volume identity not stable (path prefix fallback)".into());
        (fallback_volume(), false)
    };

    let file_id = if id_ok {
        let hex: String = id_info
            .FileId
            .Identifier
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Some(FileIdentity {
            volume: volume.clone(),
            id: hex,
        })
    } else {
        unavailable.push("file id unavailable (FILE_ID_INFO unsupported or failed)".into());
        None
    };

    let allocation_size = if is_placeholder {
        unavailable.push(format!(
            "allocation unavailable: {} is a placeholder/offline/reparse item",
            path.display()
        ));
        None
    } else {
        let mut standard = FILE_STANDARD_INFO::default();
        let ok = unsafe {
            GetFileInformationByHandleEx(
                handle.0,
                FileStandardInfo,
                (&mut standard as *mut FILE_STANDARD_INFO).cast::<c_void>(),
                std::mem::size_of::<FILE_STANDARD_INFO>() as u32,
            )
        }
        .is_ok();
        match (ok, u64::try_from(standard.AllocationSize)) {
            (true, Ok(bytes)) => Some(bytes),
            _ => {
                unavailable.push("allocation size unavailable (FILE_STANDARD_INFO)".into());
                None
            }
        }
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

fn placeholder_attributes(attributes: u32) -> bool {
    // Any reparse point that std did not classify as a link is refused too:
    // its semantics (cloud, dedup, WIM, ...) are unknown.
    attributes
        & (ATTRIBUTE_REPARSE_POINT
            | ATTRIBUTE_OFFLINE
            | ATTRIBUTE_RECALL_ON_OPEN
            | ATTRIBUTE_RECALL_ON_DATA_ACCESS)
        != 0
}

struct VolumeFind(HANDLE);
impl Drop for VolumeFind {
    fn drop(&mut self) {
        unsafe {
            let _ = FindVolumeClose(self.0);
        }
    }
}

fn wide_nul(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn fs_error(context: &str, error: &windows::core::Error) -> FsError {
    let message = format!("{context}: {error}");
    if error.code() == ERROR_ACCESS_DENIED.to_hresult() {
        FsError::permission_denied(message)
    } else {
        FsError::new(message)
    }
}

/// Open with no-follow, no-recall, backup semantics and full sharing so the
/// open neither blocks nor is blocked by other users of the object.
fn open_no_follow(wide_nul: &[u16], access: u32) -> windows::core::Result<Handle> {
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide_nul.as_ptr()),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_OPEN_NO_RECALL,
            None,
        )
    }?;
    Ok(Handle(handle))
}

fn handle_attributes(handle: &Handle) -> windows::core::Result<u32> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(handle.0, &mut info) }?;
    Ok(info.dwFileAttributes)
}

fn handle_id(handle: &Handle) -> windows::core::Result<FILE_ID_INFO> {
    let mut info = FILE_ID_INFO::default();
    unsafe {
        GetFileInformationByHandleEx(
            handle.0,
            FileIdInfo,
            (&mut info as *mut FILE_ID_INFO).cast::<c_void>(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    }?;
    Ok(info)
}

/// Layout of one directory-information record class.
struct DirClass {
    restart: FILE_INFO_BY_HANDLE_CLASS,
    next: FILE_INFO_BY_HANDLE_CLASS,
    name_length_offset: usize,
    name_offset: usize,
}

const EXTD_CLASS: DirClass = DirClass {
    restart: FileIdExtdDirectoryRestartInfo,
    next: FileIdExtdDirectoryInfo,
    name_length_offset: std::mem::offset_of!(FILE_ID_EXTD_DIR_INFO, FileNameLength),
    name_offset: std::mem::offset_of!(FILE_ID_EXTD_DIR_INFO, FileName),
};
const BOTH_CLASS: DirClass = DirClass {
    restart: FileIdBothDirectoryRestartInfo,
    next: FileIdBothDirectoryInfo,
    name_length_offset: std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileNameLength),
    name_offset: std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileName),
};

const LIST_BUFFER_BYTES: usize = 64 * 1024;

enum ListFailure {
    /// The class is unsupported on this filesystem (first call only).
    Unsupported,
    Failed(FsError),
}

fn read_u32(buffer: &[u8], offset: usize) -> Option<u32> {
    let bytes = buffer.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

fn list_with_class(
    dir: &Handle,
    parent: &Path,
    class: &DirClass,
    wanted: usize,
) -> Result<Vec<PathBuf>, ListFailure> {
    // u64 backing storage gives the 8-byte alignment the records require.
    let mut storage = vec![0u64; LIST_BUFFER_BYTES / 8];
    let mut out = Vec::new();
    let mut first = true;
    while out.len() < wanted {
        let call = unsafe {
            GetFileInformationByHandleEx(
                dir.0,
                if first { class.restart } else { class.next },
                storage.as_mut_ptr().cast::<c_void>(),
                LIST_BUFFER_BYTES as u32,
            )
        };
        if let Err(error) = call {
            let code = error.code();
            if code == ERROR_NO_MORE_FILES.to_hresult() {
                break;
            }
            if first
                && (code == ERROR_INVALID_PARAMETER.to_hresult()
                    || code == ERROR_NOT_SUPPORTED.to_hresult())
            {
                return Err(ListFailure::Unsupported);
            }
            return Err(ListFailure::Failed(fs_error(
                "directory listing failed",
                &error,
            )));
        }
        first = false;
        // SAFETY: storage is a live u64 allocation of LIST_BUFFER_BYTES bytes.
        let buffer =
            unsafe { std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), LIST_BUFFER_BYTES) };
        let mut offset = 0usize;
        loop {
            let malformed =
                || ListFailure::Failed(FsError::new("malformed directory record from the OS"));
            let next = read_u32(buffer, offset).ok_or_else(malformed)? as usize;
            let name_bytes =
                read_u32(buffer, offset + class.name_length_offset).ok_or_else(malformed)? as usize;
            let start = offset + class.name_offset;
            let name = buffer
                .get(start..start.checked_add(name_bytes).ok_or_else(malformed)?)
                .ok_or_else(malformed)?;
            if !name_bytes.is_multiple_of(2) {
                return Err(malformed());
            }
            let units: Vec<u16> = name
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            let is_dot = units == [u16::from(b'.')] || units == [u16::from(b'.'); 2];
            if !is_dot {
                out.push(parent.join(OsString::from_wide(&units)));
                if out.len() >= wanted {
                    break;
                }
            }
            if next == 0 {
                break;
            }
            offset += next;
        }
    }
    Ok(out)
}

/// Identity of every ancestor of `path` (parent up to the root), innermost
/// first. Each ancestor is opened with backup semantics + no-reparse +
/// no-recall and must be a plain directory; its FILE_ID_INFO is recorded.
/// Comparing two snapshots detects an ancestor being renamed/swapped to a
/// different object. This is conservative re-verification, not a guarantee:
/// once the listing handle is open the enumeration is relative to that held
/// handle and cannot be redirected, but there is a residual window between
/// the second snapshot and the first enumeration call during which an
/// ancestor swap is undetectable with the pinned Win32 APIs available here.
fn ancestor_identity_snapshot(path: &Path) -> Result<Vec<FILE_ID_INFO>, FsError> {
    let mut ids = Vec::new();
    for ancestor in path.ancestors().skip(1) {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        let wide = wide_nul(ancestor);
        let handle = open_no_follow(&wide, ACCESS_READ_ATTRIBUTES).map_err(|error| {
            fs_error(
                &format!("ancestor identity unavailable: {}", ancestor.display()),
                &error,
            )
        })?;
        let attributes = handle_attributes(&handle)
            .map_err(|error| fs_error("ancestor attributes unavailable", &error))?;
        if attributes & ATTRIBUTE_DIRECTORY == 0 || placeholder_attributes(attributes) {
            return Err(FsError::new(format!(
                "listing refused: ancestor {} is a reparse/offline/non-directory object",
                ancestor.display()
            )));
        }
        ids.push(
            handle_id(&handle).map_err(|error| fs_error("ancestor file id unavailable", &error))?,
        );
    }
    Ok(ids)
}

/// Bounded, no-follow directory listing: at most `limit` children plus a
/// probe entry to report truncation. Refuses reparse/offline/recall objects
/// and refuses when the listing handle is not the object pre-inspected.
/// Ancestor directories are identity-verified before and after the listing
/// handle is opened; enumeration itself is relative to that held handle
/// (GetFileInformationByHandleEx), so a swapped ancestor cannot redirect the
/// listing once the handle is pinned.
pub(super) fn children_bounded(path: &Path, limit: usize) -> Result<(Vec<PathBuf>, bool), FsError> {
    let ancestors_before = ancestor_identity_snapshot(path)?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| FsError::new(format!("listing refused: metadata failed: {error}")))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(FsError::new("listing refused: not a plain directory"));
    }
    if placeholder_attributes(std::os::windows::fs::MetadataExt::file_attributes(
        &metadata,
    )) {
        return Err(FsError::new(
            "listing refused: reparse, offline or recall attributes set",
        ));
    }

    let wide = wide_nul(path);
    // Pre-open identity probe; held open through the listing to pin the object.
    let probe = open_no_follow(&wide, ACCESS_READ_ATTRIBUTES)
        .map_err(|error| fs_error("listing refused: identity probe open failed", &error))?;
    let probe_attributes = handle_attributes(&probe)
        .map_err(|error| fs_error("listing refused: attributes unavailable", &error))?;
    let probe_id = handle_id(&probe)
        .map_err(|error| fs_error("listing refused: file identity unavailable", &error))?;
    if probe_attributes & ATTRIBUTE_DIRECTORY == 0 || placeholder_attributes(probe_attributes) {
        return Err(FsError::new(
            "listing refused: reparse, offline, recall or non-directory object",
        ));
    }

    let dir = open_no_follow(&wide, ACCESS_LIST_DIRECTORY)
        .map_err(|error| fs_error("listing refused: directory open failed", &error))?;
    let dir_attributes = handle_attributes(&dir)
        .map_err(|error| fs_error("listing refused: attributes unavailable", &error))?;
    let dir_id = handle_id(&dir)
        .map_err(|error| fs_error("listing refused: file identity unavailable", &error))?;
    if dir_attributes & ATTRIBUTE_DIRECTORY == 0 || placeholder_attributes(dir_attributes) {
        return Err(FsError::new(
            "listing refused: reparse, offline, recall or non-directory object",
        ));
    }
    if dir_id != probe_id {
        return Err(FsError::new(
            "listing refused: directory changed identity between inspection and open",
        ));
    }
    if ancestor_identity_snapshot(path)? != ancestors_before {
        return Err(FsError::new(
            "listing refused: an ancestor directory changed identity during open",
        ));
    }

    let wanted = limit.saturating_add(1);
    let mut children = match list_with_class(&dir, path, &EXTD_CLASS, wanted) {
        Ok(children) => children,
        Err(ListFailure::Unsupported) => match list_with_class(&dir, path, &BOTH_CLASS, wanted) {
            Ok(children) => children,
            Err(ListFailure::Unsupported) => {
                return Err(FsError::new(
                    "directory listing classes unsupported on this filesystem",
                ));
            }
            Err(ListFailure::Failed(error)) => return Err(error),
        },
        Err(ListFailure::Failed(error)) => return Err(error),
    };
    let truncated = children.len() > limit;
    children.truncate(limit);
    Ok((children, truncated))
}

fn first_mount_path(volume_name: &[u16]) -> Option<Vec<u16>> {
    let mut buffer = vec![0u16; 1024];
    let mut needed = 0u32;
    for _ in 0..2 {
        let result = unsafe {
            GetVolumePathNamesForVolumeNameW(
                PCWSTR(volume_name.as_ptr()),
                Some(buffer.as_mut_slice()),
                &mut needed,
            )
        };
        match result {
            Ok(()) => {
                let end = buffer.iter().position(|unit| *unit == 0).unwrap_or(0);
                if end == 0 {
                    return None;
                }
                let mut first = buffer[..end].to_vec();
                first.push(0);
                return Some(first);
            }
            Err(error) if error.code() == ERROR_MORE_DATA.to_hresult() => {
                buffer = vec![0u16; (needed as usize).max(buffer.len() * 2)];
            }
            Err(_) => return None,
        }
    }
    None
}

/// Native total/used/available for the volume whose stable identity is
/// `serial:<16hex>`. Purgeable and snapshot state are not reported.
pub(super) fn volume_usage(volume: &VolumeIdentity) -> Result<VolumeUsage, FsError> {
    let Some(wanted) = volume.id.strip_prefix("serial:") else {
        return Err(FsError::new(format!(
            "volume usage unavailable: {} is not a stable NTFS/ReFS serial identity",
            volume.id
        )));
    };
    let wanted = wanted.to_ascii_lowercase();

    let mut name = vec![0u16; 260];
    let find = unsafe { FindFirstVolumeW(&mut name) }
        .map_err(|error| fs_error("volume enumeration failed", &error))?;
    let _find = VolumeFind(find);
    let mut skipped = 0usize;
    loop {
        let end = name
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(name.len() - 1);
        let volume_name: Vec<u16> = name[..end].iter().copied().chain(Some(0)).collect();
        let serial = open_no_follow(&volume_name, ACCESS_READ_ATTRIBUTES)
            .and_then(|root| handle_id(&root))
            .map(|info| format!("{:016x}", info.VolumeSerialNumber));
        match serial {
            Ok(serial) if serial == wanted => {
                let target = first_mount_path(&volume_name).unwrap_or(volume_name);
                let (mut available, mut total, mut free) = (0u64, 0u64, 0u64);
                unsafe {
                    GetDiskFreeSpaceExW(
                        PCWSTR(target.as_ptr()),
                        Some(&mut available),
                        Some(&mut total),
                        Some(&mut free),
                    )
                }
                .map_err(|error| fs_error("GetDiskFreeSpaceExW failed", &error))?;
                return Ok(VolumeUsage {
                    volume: volume.clone(),
                    total_bytes: Some(total),
                    used_bytes: Some(total.saturating_sub(free)),
                    available_bytes: Some(available),
                    purgeable_bytes: None,
                    snapshots: SnapshotState::Unknown,
                });
            }
            Ok(_) => {}
            Err(_) => skipped += 1,
        }
        match unsafe { FindNextVolumeW(find, &mut name) } {
            Ok(()) => {}
            Err(error) if error.code() == ERROR_NO_MORE_FILES.to_hresult() => break,
            Err(error) => return Err(fs_error("volume enumeration failed", &error)),
        }
    }
    Err(FsError::new(format!(
        "no mounted volume matches {} ({skipped} volume(s) could not be opened)",
        volume.id
    )))
}
