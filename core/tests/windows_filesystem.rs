#![cfg(windows)]
//! Windows native listing and volume usage checks. Metadata only; no volume
//! modification and no cloud-provider access.

use cockpit_core::VolumeIdentity;
use cockpit_core::platform::{children_bounded, inspect, volume_usage};
use std::fs;
use std::path::PathBuf;

struct TempDir(PathBuf);
impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "cockpit-winfs-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn listing_is_bounded_and_reports_truncation() {
    let dir = TempDir::new("bounded");
    for i in 0..5 {
        fs::write(dir.0.join(format!("f{i}.txt")), b"x").unwrap();
    }
    let (some, truncated) = children_bounded(&dir.0, 3).expect("list");
    assert_eq!(some.len(), 3);
    assert!(truncated);
    let (all, truncated) = children_bounded(&dir.0, 5).expect("list");
    assert_eq!(all.len(), 5);
    assert!(!truncated);
    assert!(all.iter().all(|p| p.starts_with(&dir.0)));
    let (none, truncated) = children_bounded(&dir.0, 0).expect("list");
    assert!(none.is_empty());
    assert!(truncated);
}

#[test]
fn listing_refuses_junction_or_symlink() {
    let dir = TempDir::new("link");
    let target = dir.0.join("target");
    fs::create_dir(&target).unwrap();
    let link = dir.0.join("link");
    if let Err(error) = std::os::windows::fs::symlink_dir(&target, &link) {
        eprintln!("skipped: cannot create directory symlink here: {error}");
        return;
    }
    let error = children_bounded(&link, 10).expect_err("link must be refused");
    assert!(error.message.contains("refused"), "{}", error.message);
}

#[test]
fn listing_refuses_files() {
    let dir = TempDir::new("file");
    let file = dir.0.join("a.txt");
    fs::write(&file, b"x").unwrap();
    assert!(children_bounded(&file, 10).is_err());
}

#[test]
fn listing_identity_matches_inspection() {
    let dir = TempDir::new("identity");
    fs::write(dir.0.join("a.txt"), b"x").unwrap();
    let metadata = fs::symlink_metadata(&dir.0).unwrap();
    let info = inspect(&dir.0, &metadata);
    assert!(info.file_id.is_some(), "{:?}", info.unavailable);
    assert!(children_bounded(&dir.0, 10).is_ok());
    // The same object listed again after inspection is still accepted and
    // inspection identity is stable across the listing.
    let again = inspect(&dir.0, &fs::symlink_metadata(&dir.0).unwrap());
    assert_eq!(info.file_id, again.file_id);
}

#[test]
fn volume_usage_for_temp_volume_is_consistent() {
    let dir = TempDir::new("usage");
    let metadata = fs::symlink_metadata(&dir.0).unwrap();
    let info = inspect(&dir.0, &metadata);
    if !info.volume_stable {
        eprintln!("skipped: stable volume identity unavailable");
        return;
    }
    let usage = volume_usage(&info.volume).expect("usage for temp volume");
    let (total, used) = (usage.total_bytes.unwrap(), usage.used_bytes.unwrap());
    assert!(total >= used);
    assert!(usage.available_bytes.unwrap() <= total);
    assert!(usage.purgeable_bytes.is_none());
}

#[test]
fn volume_usage_rejects_unmatched_identity() {
    assert!(volume_usage(&VolumeIdentity::new("serial:ffffffffffffffff")).is_err());
    assert!(volume_usage(&VolumeIdentity::new("path-prefix-unstable:C:")).is_err());
}
