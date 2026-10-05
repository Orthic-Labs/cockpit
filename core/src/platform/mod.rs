//! Native, metadata-only filesystem adapters.
//!
//! Every adapter obeys the same contract: no file content is ever read, links
//! are never traversed, placeholders are never hydrated, and a field that
//! cannot be produced safely is reported as unavailable with a reason instead
//! of being guessed.

use crate::model::{FileIdentity, FsError, VolumeIdentity, VolumeUsage};
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
mod mac_native;
#[cfg(unix)]
mod unix_native;
#[cfg(windows)]
mod win_native;

/// Per-entry facts produced by the native adapter for one path.
#[derive(Clone, Debug)]
pub struct NativeInfo {
    /// Volume identity. Stable (macOS volume UUID, NTFS volume serial) only
    /// when `volume_stable` is true.
    pub volume: VolumeIdentity,
    /// False when the volume identity is a fallback that may change across
    /// mounts or reboots (statfs fsid, st_dev, drive prefix).
    pub volume_stable: bool,
    /// File identity; present for files and directories when the platform
    /// supplies it.
    pub file_id: Option<FileIdentity>,
    /// Allocated bytes straight from the platform (st_blocks*512 or
    /// AllocationSize). `None` when unavailable or the item is a placeholder.
    pub allocation_size: Option<u64>,
    /// True for dataless / offline / recall-on-access items and unknown
    /// reparse points.
    pub is_placeholder: bool,
    /// Human-readable reasons for each unavailable field.
    pub unavailable: Vec<String>,
}

/// Inspect `path` using lstat-style semantics (`metadata` must come from
/// `symlink_metadata`). Never reads content and never follows links.
pub fn inspect(path: &Path, metadata: &fs::Metadata) -> NativeInfo {
    #[cfg(windows)]
    {
        win_native::inspect(path, metadata)
    }
    #[cfg(unix)]
    {
        unix_native::inspect(path, metadata)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = metadata;
        NativeInfo {
            volume: VolumeIdentity::new("unsupported-platform"),
            volume_stable: false,
            file_id: None,
            allocation_size: None,
            is_placeholder: false,
            unavailable: vec![format!(
                "native metadata unsupported on this platform: {}",
                path.display()
            )],
        }
    }
}

/// Volume identity for a mount point or path, used to map scanned volumes to
/// mount accounting. Returns `(identity, stable)`; `None` when unsupported.
pub fn volume_identity_for_path(path: &Path) -> Option<(VolumeIdentity, bool)> {
    #[cfg(unix)]
    {
        let metadata = fs::symlink_metadata(path).ok()?;
        let (volume, stable, _) = unix_native::volume_for(path, &metadata);
        Some((volume, stable))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Bounded, no-follow directory listing. Reads at most `limit` entries plus
/// one probe entry; returns `(children, truncated)`. Implementations must
/// verify the opened directory is the same object (identity) that was
/// inspected and refuse placeholders/reparse points before listing.
/// Settled seam: unix -> `unix_native::children_bounded`,
/// windows -> `win_native::children_bounded`.
pub fn children_bounded(path: &Path, limit: usize) -> Result<(Vec<PathBuf>, bool), FsError> {
    #[cfg(unix)]
    {
        unix_native::children_bounded(path, limit)
    }
    #[cfg(windows)]
    {
        win_native::children_bounded(path, limit)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, limit);
        Err(FsError::new("directory listing unsupported on this platform"))
    }
}

/// Native mount accounting for a scanned volume identity. Total/used/
/// available only; purgeable and snapshot state stay unknown unless a real
/// provider reports them. Settled seam: unix -> `unix_native::volume_usage`,
/// windows -> `win_native::volume_usage`.
pub fn volume_usage(volume: &VolumeIdentity) -> Result<VolumeUsage, FsError> {
    #[cfg(unix)]
    {
        unix_native::volume_usage(volume)
    }
    #[cfg(windows)]
    {
        win_native::volume_usage(volume)
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(FsError::new(format!("volume usage unsupported: {}", volume.id)))
    }
}

/// Lower-case canonical 8-4-4-4-12 rendering of a 16-byte UUID.
pub fn format_uuid(bytes: &[u8; 16]) -> String {
    let mut out = String::with_capacity(36);
    for (i, byte) in bytes.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            out.push('-');
        }
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::format_uuid;

    #[test]
    fn uuid_format_is_canonical() {
        let bytes = [
            0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
            0xcd, 0xef,
        ];
        assert_eq!(format_uuid(&bytes), "01234567-89ab-cdef-0123-456789abcdef");
    }
}
