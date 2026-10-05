//! Opt-in local metadata history. Target files are never changed by a scan.
use crate::{rules::Finding, ScanReport};
use serde::{Deserialize, Serialize};
use std::{fs, io, path::{Path, PathBuf}, time::{SystemTime, UNIX_EPOCH}};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema_version: u32,
    pub id: String,
    pub created_at: u64,
    pub report: ScanReport,
    pub findings: Vec<Finding>,
}
impl Snapshot {
    pub fn new(report: ScanReport, findings: Vec<Finding>) -> Self {
        let time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        Self { schema_version: 1, id: format!("scan-{}", time.as_nanos()), created_at: time.as_secs(), report, findings }
    }
}

pub fn default_directory() -> io::Result<PathBuf> {
    #[cfg(target_os = "windows")]
    let root = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).map(|p| p.join("Cockpit"));
    #[cfg(target_os = "macos")]
    let root = std::env::var_os("HOME").map(PathBuf::from).map(|p| p.join("Library/Application Support/Cockpit"));
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let root = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from).map(|p| p.join("cockpit")).or_else(|| std::env::var_os("HOME").map(PathBuf::from).map(|p| p.join(".local/state/cockpit")));
    root.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "local metadata directory unavailable"))
}
fn reject_links(path: &Path) -> io::Result<()> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(io::Error::new(io::ErrorKind::InvalidInput, "metadata directory contains symlink")),
            Ok(meta) => {
                #[cfg(windows)]
                { use std::os::windows::fs::MetadataExt; if meta.file_attributes() & 0x400 != 0 { return Err(io::Error::new(io::ErrorKind::InvalidInput, "metadata directory contains reparse point")); } }
                let _ = meta;
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
pub fn save(directory: &Path, snapshot: &Snapshot) -> io::Result<PathBuf> {
    reject_links(directory)?;
    let directory_existed = directory.exists();
    fs::create_dir_all(directory)?;
    #[cfg(unix)]
    { use std::os::unix::fs::PermissionsExt; if !directory_existed { fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?; } }
    #[cfg(not(unix))]
    let _ = directory_existed;
    let destination = directory.join(format!("{}.json", snapshot.id));
    let temporary = directory.join(format!(".{}.tmp", snapshot.id));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
    let mut file = options.open(&temporary)?;
    let result = (|| {
        serde_json::to_writer(&mut file, snapshot).map_err(io::Error::other)?;
        file.sync_all()?;
        fs::rename(&temporary, &destination)?;
        Ok(destination.clone())
    })();
    if result.is_err() { let _ = fs::remove_file(&temporary); }
    result
}
pub fn history(directory: &Path) -> io::Result<Vec<Snapshot>> {
    reject_links(directory)?;
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut snapshots = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_file() || !entry.file_name().to_string_lossy().starts_with("scan-") || entry.path().extension().and_then(|s| s.to_str()) != Some("json") { continue; }
        if entry.metadata()?.len() > 64 * 1024 * 1024 { return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot exceeds 64 MiB")); }
        if snapshots.len() >= 1000 { return Err(io::Error::new(io::ErrorKind::InvalidData, "history exceeds 1000 snapshots")); }
        let snapshot: Snapshot = serde_json::from_reader(fs::File::open(entry.path())?).map_err(io::Error::other)?;
        if snapshot.schema_version != 1 { return Err(io::Error::new(io::ErrorKind::InvalidData, "unsupported snapshot schema")); }
        snapshots.push(snapshot);
    }
    snapshots.sort_by_key(|s| (s.created_at, s.id.clone()));
    Ok(snapshots)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn refuses_linked_state_directory() {
        use std::os::unix::fs::symlink;
        let path = fs::canonicalize(std::env::temp_dir()).unwrap().join(format!("cockpit-store-link-{}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        let link = path.join("link");
        symlink(&path, &link).unwrap();
        assert!(history(&link).is_err());
        fs::remove_file(link).unwrap();
        fs::remove_dir(path).unwrap();
    }
}
