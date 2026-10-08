//! Preserve rename-era state without merging or replacing an existing destination.
use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// Move a legacy directory once. Both names existing means Pulse wins; the legacy
/// directory stays untouched. Failed moves are returned before callers create state.
pub fn migrate_directory(legacy: &Path, destination: &Path) -> io::Result<()> {
    match fs::symlink_metadata(destination) {
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    match fs::symlink_metadata(legacy) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "legacy state is not a directory",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }
    rename_exclusive(legacy, destination).or_else(|error| {
        // Another surface may have completed the same migration first.
        if error.kind() == io::ErrorKind::AlreadyExists
            || (error.kind() == io::ErrorKind::NotFound && destination.is_dir())
        {
            Ok(())
        } else {
            Err(error)
        }
    })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn rename_exclusive(legacy: &Path, destination: &Path) -> io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let legacy = CString::new(legacy.as_os_str().as_bytes())?;
    let destination = CString::new(destination.as_os_str().as_bytes())?;
    #[cfg(target_os = "macos")]
    let result =
        unsafe { libc::renamex_np(legacy.as_ptr(), destination.as_ptr(), libc::RENAME_EXCL) };
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            legacy.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "windows")]
fn rename_exclusive(legacy: &Path, destination: &Path) -> io::Result<()> {
    // MoveFileExW (std) cannot replace an existing directory on Windows.
    fs::rename(legacy, destination)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn rename_exclusive(_legacy: &Path, _destination: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "exclusive state migration unavailable",
    ))
}

/// Legacy metadata directory beside `root`; migration only.
pub fn legacy_metadata_dir(root: &Path) -> PathBuf {
    // legacy name, migration only
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let name = "Cockpit";
    // legacy name, migration only
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let name = "cockpit";
    root.with_file_name(name)
}

// legacy name, migration only
#[cfg(target_os = "macos")]
const LEGACY_MAC_SUPPORT_FOLDER: &str = "Cockpit";
// legacy name, migration only
#[cfg(target_os = "macos")]
const LEGACY_HUB_FOLDER: &str = "dev.orthic.cockpit.hub";

/// Shared Mac state plus native hub data, cache & WebKit storage.
#[cfg(target_os = "macos")]
pub fn migrate_mac_state(home: &Path) -> io::Result<()> {
    migrate_directory(
        &home
            .join("Library/Application Support")
            .join(LEGACY_MAC_SUPPORT_FOLDER),
        &home.join("Library/Application Support/Pulse"),
    )?;
    for parent in [
        "Library/Application Support",
        "Library/Caches",
        "Library/WebKit",
    ] {
        migrate_directory(
            &home.join(parent).join(LEGACY_HUB_FOLDER),
            &home.join(parent).join("dev.orthic.pulse.hub"),
        )?;
    }
    Ok(())
}
