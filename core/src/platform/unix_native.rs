//! Unix (stat-based) adapter. macOS adds a volume UUID lookup.

use super::NativeInfo;
use crate::model::{FileIdentity, VolumeIdentity};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

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
pub(super) fn volume_for(path: &Path, metadata: &fs::Metadata) -> (VolumeIdentity, bool, Option<String>) {
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
    metadata.st_flags() & 0x4000_0000 != 0
}

#[cfg(not(target_os = "macos"))]
fn is_placeholder(_metadata: &fs::Metadata) -> bool {
    false
}
