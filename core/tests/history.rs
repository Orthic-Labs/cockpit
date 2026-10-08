use pulse_core::history::compare;
use pulse_core::model::ScanReport;
use pulse_core::store::Snapshot;
use pulse_core::{Accounting, VolumeIdentity, VolumeUsage};
use std::path::PathBuf;

fn report(roots: &[&str], volumes: &[&str], logical: u64, attributed: u64) -> ScanReport {
    let mut report = ScanReport {
        roots: roots.iter().map(PathBuf::from).collect(),
        entries: Vec::new(),
        folders: Vec::new(),
        accounting: Accounting {
            logical_bytes: logical,
            attributed_allocation_bytes: attributed,
            ..Accounting::default()
        },
        volume_usage: volumes
            .iter()
            .map(|id| VolumeUsage {
                volume: VolumeIdentity::new(*id),
                total_bytes: None,
                used_bytes: None,
                available_bytes: None,
                purgeable_bytes: None,
                snapshots: Default::default(),
            })
            .collect(),
        volume_deltas: Vec::new(),
        inspection_errors: Vec::new(),
        skipped_links: Vec::new(),
        incomplete_reasons: Vec::new(),
    };
    report.accounting.incomplete = false;
    report
}

fn snapshot(report: &ScanReport) -> Snapshot {
    Snapshot::new(report.clone(), Vec::new())
}

#[test]
fn same_scope_growth_is_signed_and_positive() {
    let previous = snapshot(&report(&["/data"], &["vol-a"], 1_000, 1_200));
    let current = snapshot(&report(&["/data"], &["vol-a"], 1_500, 1_800));
    let comparison = compare(&previous, &current);
    assert!(comparison.comparable);
    assert_eq!(comparison.logical_growth_bytes, Some(500));
    assert_eq!(comparison.attributed_growth_bytes, Some(600));
    assert!(comparison.reasons.is_empty());
}

#[test]
fn same_scope_shrink_is_negative_without_underflow() {
    let previous = snapshot(&report(&["/data"], &["vol-a"], 2_000, 2_500));
    let current = snapshot(&report(&["/data"], &["vol-a"], 900, 1_100));
    let comparison = compare(&previous, &current);
    assert!(comparison.comparable);
    assert_eq!(comparison.logical_growth_bytes, Some(-1_100));
    assert_eq!(comparison.attributed_growth_bytes, Some(-1_400));
}

#[test]
fn changed_roots_are_not_comparable() {
    let previous = snapshot(&report(&["/data"], &["vol-a"], 100, 100));
    let current = snapshot(&report(&["/other"], &["vol-a"], 100, 100));
    let comparison = compare(&previous, &current);
    assert!(!comparison.comparable);
    assert_eq!(comparison.logical_growth_bytes, None);
    assert!(comparison.reasons.iter().any(|r| r.contains("roots")));
}

#[test]
fn reordered_and_duplicated_roots_still_compare() {
    let previous = snapshot(&report(&["/a", "/b"], &["vol-a"], 100, 200));
    let mut current_report = report(&["/b", "/a", "/b"], &["vol-a"], 100, 200);
    current_report.roots.push(PathBuf::from("/a"));
    let current = snapshot(&current_report);
    let comparison = compare(&previous, &current);
    assert!(comparison.comparable);
    assert_eq!(comparison.logical_growth_bytes, Some(0));
}

#[test]
fn changed_volume_set_is_not_comparable() {
    let previous = snapshot(&report(&["/data"], &["vol-a"], 100, 100));
    let current = snapshot(&report(&["/data"], &["vol-b"], 100, 100));
    let comparison = compare(&previous, &current);
    assert!(!comparison.comparable);
    assert!(
        comparison
            .reasons
            .iter()
            .any(|r| r.contains("volume identities differ"))
    );
}

#[test]
fn empty_volume_identity_is_not_comparable() {
    let previous = snapshot(&report(&["/data"], &[""], 100, 100));
    let current = snapshot(&report(&["/data"], &[""], 100, 100));
    let comparison = compare(&previous, &current);
    assert!(!comparison.comparable);
    assert!(
        comparison
            .reasons
            .iter()
            .any(|r| r.contains("empty volume identity"))
    );
}

#[test]
fn no_observed_volumes_is_not_comparable() {
    let previous = snapshot(&report(&["/data"], &[], 100, 100));
    let current = snapshot(&report(&["/data"], &[], 100, 100));
    let comparison = compare(&previous, &current);
    assert!(!comparison.comparable);
    assert!(
        comparison
            .reasons
            .iter()
            .any(|r| r.contains("no observed volumes"))
    );
}

#[test]
fn incomplete_accounting_is_not_comparable() {
    let mut current_report = report(&["/data"], &["vol-a"], 100, 100);
    current_report.accounting.incomplete = true;
    let previous = snapshot(&report(&["/data"], &["vol-a"], 100, 100));
    let current = snapshot(&current_report);
    let comparison = compare(&previous, &current);
    assert!(!comparison.comparable);
    assert!(comparison.reasons.iter().any(|r| r.contains("incomplete")));
}

#[test]
fn differing_schema_versions_are_not_comparable() {
    let previous = snapshot(&report(&["/data"], &["vol-a"], 100, 100));
    let mut current = snapshot(&report(&["/data"], &["vol-a"], 100, 100));
    current.schema_version = 2;
    let comparison = compare(&previous, &current);
    assert!(!comparison.comparable);
    assert!(comparison.reasons.iter().any(|r| r.contains("schema")));
}

#[test]
fn unknown_sharing_does_not_block_comparison() {
    // Reclaim estimates may be unknown; attributed accounting still compares.
    let mut report_a = report(&["/data"], &["vol-a"], 100, 100);
    report_a.accounting.reclaim.state = Some(pulse_core::ReclaimState::Unknown);
    let previous = snapshot(&report_a);
    let current = snapshot(&report(&["/data"], &["vol-a"], 150, 160));
    let comparison = compare(&previous, &current);
    assert!(comparison.comparable);
    assert_eq!(comparison.attributed_growth_bytes, Some(60));
}
