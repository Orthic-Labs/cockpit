#![cfg(unix)]

use cockpit_core::{ScanOptions, scan};
use std::fs;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::PathBuf;

struct Fixture(PathBuf);
impl Fixture {
    fn new(name: &str) -> Self {
        let root = fs::canonicalize(std::env::temp_dir()).unwrap()
            .join(format!("cockpit-native-{name}-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) { fs::remove_dir_all(&self.0).unwrap(); }
}

#[test]
fn native_sparse_file_and_hardlink_use_unique_stat_allocation() {
    let fixture = Fixture::new("allocation");
    let first = fixture.0.join("a");
    let file = fs::File::create(&first).unwrap();
    file.set_len(8 * 1024 * 1024).unwrap();
    fs::hard_link(&first, fixture.0.join("b")).unwrap();
    let metadata = fs::symlink_metadata(&first).unwrap();
    let report = scan(&[fixture.0.clone()], &ScanOptions::default());
    assert_eq!(report.accounting.logical_bytes, metadata.len());
    assert_eq!(report.accounting.attributed_allocation_bytes, metadata.blocks() * 512);
    assert_eq!(report.folders[0].attributed_allocation_bytes, metadata.blocks() * 512);
    assert_eq!(fs::symlink_metadata(&first).unwrap().len(), metadata.len());
}

#[test]
fn native_symlink_ancestor_blocks_requested_descendant() {
    let fixture = Fixture::new("ancestor");
    let target = fixture.0.join("target");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("secret"), b"never inspected through link").unwrap();
    symlink(&target, fixture.0.join("link")).unwrap();
    let report = scan(&[fixture.0.join("link/secret")], &ScanOptions::default());
    assert!(report.entries.is_empty());
    assert!(report.accounting.incomplete);
    assert!(report.incomplete_reasons.iter().any(|reason| reason.contains("symlink")));
}
