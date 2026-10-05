//! Windows adapter. Opens the item with attribute-only access, backup
//! semantics and OPEN_REPARSE_POINT, so content is never read, reparse points
//! are never followed and cloud placeholders are never recalled.

use super::NativeInfo;
use crate::model::{FileIdentity, VolumeIdentity};
use std::ffi::c_void;
use std::fs;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_NO_RECALL,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FILE_STANDARD_INFO, FileIdInfo, FileStandardInfo, GetFileInformationByHandle,
    GetFileInformationByHandleEx, OPEN_EXISTING,
};
use windows::core::PCWSTR;

/// FILE_READ_ATTRIBUTES: attributes only, no data access.
const ACCESS_READ_ATTRIBUTES: u32 = 0x0000_0080;
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
