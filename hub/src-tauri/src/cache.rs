//! Versioned files the hub keeps between launches: the last Storage folder
//! index, view and name rows, and the last cleanup findings. A file is written
//! whole to a temporary name and renamed into place, readable only by the
//! user, and ignored when its version differs or it does not parse.

use std::io::Write;
use std::path::PathBuf;

use serde::{Serialize, de::DeserializeOwned};

#[derive(Serialize)]
struct Saved<'a, T: Serialize> {
    version: u32,
    data: &'a T,
}

#[derive(serde::Deserialize)]
struct Loaded<T> {
    version: u32,
    data: T,
}

/// The Pulse state directory shared with the core and the notch.
pub fn dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
        .join("Library/Application Support/Pulse")
}

/// `name` as saved at `version`, or `None` if it is missing, unreadable,
/// malformed or from another version.
pub fn load<T: DeserializeOwned>(name: &str, version: u32) -> Option<T> {
    let bytes = std::fs::read(dir().join(name)).ok()?;
    let loaded: Loaded<T> = serde_json::from_slice(&bytes).ok()?;
    (loaded.version == version).then_some(loaded.data)
}

/// Replace `name` with `data` at `version`. Written to a temporary file in the
/// same directory and renamed, so a reader never sees a partial file.
pub fn save<T: Serialize>(name: &str, version: u32, data: &T) -> std::io::Result<()> {
    let body = serde_json::to_vec(&Saved { version, data }).map_err(std::io::Error::other)?;
    write_bytes(name, &body)
}

/// The bytes of `name`, if it exists and can be read.
pub fn read_bytes(name: &str) -> Option<Vec<u8>> {
    std::fs::read(dir().join(name)).ok()
}

/// Replace `name` with `body` (the bytes a format writes itself). Atomic like
/// `save`: a reader never sees a partial file.
pub fn write_bytes(name: &str, body: &[u8]) -> std::io::Result<()> {
    let dir = dir();
    std::fs::create_dir_all(&dir)?;
    let temp = dir.join(format!("{name}.{}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    file.write_all(body)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&temp, dir.join(name))
}

/// Delete `name` if it exists.
pub fn remove(name: &str) -> std::io::Result<()> {
    match std::fs::remove_file(dir().join(name)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}
