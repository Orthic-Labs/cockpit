#![cfg(unix)]

use cockpit_core::duplicates::{
    ContentHandle, ContentMetadata, ContentReader, DuplicateOptions, find_duplicates,
    find_duplicates_with_reader,
};
use cockpit_core::platform;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

fn fixture() -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    let process = std::process::id();
    let path = fs::canonicalize(std::env::temp_dir())
        .unwrap()
        .join(format!("cockpit-duplicates-{process}-{stamp}-{sequence}"));
    fs::create_dir(&path).unwrap();
    path
}

fn options() -> DuplicateOptions {
    DuplicateOptions {
        min_bytes: 1,
        max_files: 100,
        max_total_read_bytes: 10_000_000,
        deadline: Duration::from_secs(5),
    }
}

#[test]
fn exact_bytes_form_group_and_changed_content_does_not() {
    let root = fixture();
    let first = root.join("first");
    let second = root.join("second");
    let changed = root.join("changed");
    fs::write(&first, b"same bytes").unwrap();
    fs::write(&second, b"same bytes").unwrap();
    fs::write(&changed, b"other bytes").unwrap();

    let report = find_duplicates(std::slice::from_ref(&root), &options());
    assert_eq!(report.groups.len(), 1);
    assert_eq!(report.groups[0].extras.len(), 1);
    assert!(!report.groups[0].extras.contains(&changed));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn hardlinks_are_skipped_by_identity_and_symlinks_are_refused() {
    let root = fixture();
    let original = root.join("original");
    let hardlink = root.join("hardlink");
    let link = root.join("symlink");
    fs::write(&original, b"hardlink bytes").unwrap();
    fs::hard_link(&original, &hardlink).unwrap();
    std::os::unix::fs::symlink(&original, &link).unwrap();

    let report = find_duplicates(std::slice::from_ref(&root), &options());
    assert!(report.groups.is_empty());
    assert!(report.skipped.iter().any(|skip| {
        (skip.path == hardlink || skip.path == original) && skip.reason.contains("hard link")
    }));
    assert!(
        report
            .skipped
            .iter()
            .any(|skip| skip.path == link && skip.reason.contains("symlink"))
    );
    fs::remove_dir_all(root).unwrap();
}

struct FixtureReader {
    partial: bool,
}

struct FixtureHandle {
    bytes: Vec<u8>,
    metadata: ContentMetadata,
    cursor: usize,
    partial: bool,
}

impl ContentReader for FixtureReader {
    fn open(&self, path: &Path) -> Result<Box<dyn ContentHandle>, String> {
        let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        let native = platform::inspect(path, &metadata);
        let file_id = native
            .file_id
            .ok_or_else(|| "missing identity".to_owned())?;
        let bytes = fs::read(path).map_err(|e| e.to_string())?;
        Ok(Box::new(FixtureHandle {
            metadata: ContentMetadata {
                file_id,
                size: bytes.len() as u64,
            },
            bytes,
            cursor: 0,
            partial: self.partial,
        }))
    }
}

impl ContentHandle for FixtureHandle {
    fn metadata(&self) -> Result<ContentMetadata, String> {
        Ok(self.metadata.clone())
    }

    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<usize, String> {
        let offset = offset as usize;
        let available = self.bytes.len().saturating_sub(offset);
        let count = available.min(buffer.len());
        buffer[..count].copy_from_slice(&self.bytes[offset..offset + count]);
        if self.partial && count > 1 {
            return Ok(1);
        }
        Ok(count)
    }

    fn read_next(&mut self, buffer: &mut [u8]) -> Result<usize, String> {
        let count = self.read_at(self.cursor as u64, buffer)?;
        self.cursor += count;
        Ok(count)
    }
}

#[test]
fn injected_partial_reader_is_reported_without_group() {
    let root = fixture();
    fs::write(root.join("one"), b"same").unwrap();
    fs::write(root.join("two"), b"same").unwrap();
    let report = find_duplicates_with_reader(
        std::slice::from_ref(&root),
        &options(),
        &FixtureReader { partial: true },
    );
    assert!(report.groups.is_empty());
    assert!(
        report
            .skipped
            .iter()
            .any(|skip| skip.reason.contains("partial sample"))
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn read_budget_and_deadline_truncate_before_unbounded_content_reads() {
    let root = fixture();
    fs::write(root.join("one"), b"same").unwrap();
    fs::write(root.join("two"), b"same").unwrap();
    let mut bounded = options();
    bounded.max_total_read_bytes = 1;
    let budget =
        find_duplicates_with_reader(std::slice::from_ref(&root), &bounded, &FixtureReader { partial: false });
    assert!(budget.truncated);
    assert!(budget.bytes_read <= 1);

    let mut expired = options();
    expired.deadline = Duration::ZERO;
    let deadline =
        find_duplicates_with_reader(std::slice::from_ref(&root), &expired, &FixtureReader { partial: false });
    assert!(deadline.truncated);
    assert!(deadline.groups.is_empty());
    fs::remove_dir_all(root).unwrap();
}
