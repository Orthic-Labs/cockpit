use cockpit_core::apps::*;
use cockpit_core::{
    Accounting, Capability, EntryKind, FileIdentity, FileMetadata, Metric, ScanReport,
    ScannedEntry, VolumeIdentity,
};
use std::path::PathBuf;

fn identity(bundle_id: Option<&str>) -> AppIdentity {
    AppIdentity {
        bundle_id: bundle_id.map(str::to_owned),
        registered_id: None,
        name: "Example".into(),
        team_id: Some("TEAM".into()),
    }
}

fn coverage_complete() -> InventoryCoverage {
    InventoryCoverage {
        bundle_source: CoverageState::Complete,
        registered_source: CoverageState::Complete,
        portable_apps: CoverageState::Complete,
        external_volumes: CoverageState::Complete,
        mounted_volume_ids: vec!["startup".into()],
        indexing_enabled: Some(true),
    }
}

fn entry(path: &str, id: &str, logical: u64, allocation: u64) -> ScannedEntry {
    ScannedEntry {
        path: PathBuf::from(path),
        metadata: FileMetadata {
            kind: EntryKind::File,
            volume: VolumeIdentity::new("startup"),
            logical_size: Some(logical),
            allocation_size: Some(allocation),
            file_id: Some(FileIdentity {
                volume: VolumeIdentity::new("startup"),
                id: id.into(),
            }),
            clone_id: None,
            created_at: None,
            modified_at: None,
            is_placeholder: false,
            metadata_complete: true,
        },
        logical_bytes: logical,
        attributed_allocation_bytes: allocation,
        accounting_owner: None,
        reclaim: None,
    }
}

fn report(entries: Vec<ScannedEntry>) -> ScanReport {
    ScanReport {
        roots: vec![PathBuf::from("/Applications")],
        entries,
        folders: Vec::new(),
        accounting: Accounting::default(),
        volume_usage: Vec::new(),
        volume_deltas: Vec::new(),
        inspection_errors: Vec::new(),
        skipped_links: Vec::new(),
        incomplete_reasons: Vec::new(),
    }
}

#[test]
fn ownership_requires_exact_identity_and_excludes_shared_user_data() {
    assert!(valid_bundle_id("com.example.Tool_2"));
    assert!(!valid_bundle_id("com.example/Tool"));
    let exact = RelatedPathRecord {
        path: "/Library/Caches/com.example.Tool".into(),
        kind: RelatedDataKind::Caches,
        ownership: OwnershipConfidence::ExactBundleId,
        shared: false,
        logical_bytes: Some(1),
        attributed_allocation_bytes: Some(1),
        file_id: None,
    };
    assert_eq!(exact.disposition(), OwnershipDisposition::Preselected);
    let shared = RelatedPathRecord {
        shared: true,
        ..exact.clone()
    };
    assert_eq!(shared.disposition(), OwnershipDisposition::Excluded);
    let group = RelatedPathRecord {
        kind: RelatedDataKind::GroupContainer,
        ownership: OwnershipConfidence::TeamIdVendor,
        ..exact.clone()
    };
    assert_eq!(group.disposition(), OwnershipDisposition::Review);
    let user = RelatedPathRecord {
        kind: RelatedDataKind::UserData,
        ..exact
    };
    assert_eq!(user.disposition(), OwnershipDisposition::Excluded);
}

#[test]
fn projector_uses_supplied_records_and_deduplicates_overlap_and_hardlinks() {
    let mut app = AppInventoryRecord::new(
        identity(Some("com.example.Tool")),
        "/Applications/Tool.app",
        InventorySource::Bundle,
    );
    app.related_paths = vec![
        RelatedPathRecord {
            path: "/Library/Application Support/Tool".into(),
            kind: RelatedDataKind::ApplicationSupport,
            ownership: OwnershipConfidence::ExactBundleId,
            shared: false,
            logical_bytes: Some(100),
            attributed_allocation_bytes: Some(100),
            file_id: Some(FileIdentity {
                volume: VolumeIdentity::new("startup"),
                id: "same".into(),
            }),
        },
        RelatedPathRecord {
            path: "/Library/Application Support/Tool/cache".into(),
            kind: RelatedDataKind::Caches,
            ownership: OwnershipConfidence::ExactBundleId,
            shared: false,
            logical_bytes: Some(60),
            attributed_allocation_bytes: Some(60),
            file_id: None,
        },
        RelatedPathRecord {
            path: "/Library/Caches/Tool-copy".into(),
            kind: RelatedDataKind::Caches,
            ownership: OwnershipConfidence::ExactBundleId,
            shared: false,
            logical_bytes: Some(100),
            attributed_allocation_bytes: Some(100),
            file_id: Some(FileIdentity {
                volume: VolumeIdentity::new("startup"),
                id: "same".into(),
            }),
        },
    ];
    let projected = project_inventory(
        &[app],
        &report(vec![entry("/Applications/Tool.app/a", "bundle-a", 8, 8)]),
        coverage_complete(),
    );
    assert_eq!(projected.apps.len(), 1);
    // Parent directory owns its descendant, and hard-linked aliases count once.
    assert_eq!(projected.apps[0].related_totals.logical_bytes, 108);
    assert_eq!(
        projected.apps[0].related_totals.attributed_allocation_bytes,
        108
    );
}

#[test]
fn missing_external_or_portable_coverage_keeps_disappearance_unknown() {
    let mut history = InstalledAppHistory::default();
    let record = AppInventoryRecord::new(
        identity(Some("com.example.Tool")),
        "/Applications/Tool.app",
        InventorySource::Bundle,
    );
    history.observe(std::slice::from_ref(&record), 10);
    let mut partial = coverage_complete();
    partial.external_volumes = CoverageState::Unavailable;
    assert_eq!(
        history.state(&record.identity, false, &partial),
        InstallState::Unknown
    );
    partial.external_volumes = CoverageState::Complete;
    partial.portable_apps = CoverageState::Partial;
    assert_eq!(
        history.state(&record.identity, false, &partial),
        InstallState::Unknown
    );
    partial.portable_apps = CoverageState::Complete;
    assert_eq!(
        history.state(&record.identity, false, &partial),
        InstallState::ConfirmedGone
    );
    assert!(matches!(
        assess_leftover_eligibility(InstallState::ConfirmedGone, &partial, Some(false)),
        LeftoverEligibility::Eligible
    ));
    assert!(matches!(
        assess_leftover_eligibility(InstallState::ConfirmedGone, &partial, None),
        LeftoverEligibility::Ineligible { .. }
    ));
}

#[test]
fn malformed_and_unavailable_updates_are_distinct_and_network_free() {
    let result = normalize_update_feed(&[
        SuppliedUpdateRecord {
            app_key: "tool".into(),
            current_version: "1.2.0".into(),
            candidate_version: Some("1.3.0".into()),
            feed_url: "file:///tmp/feed.json".into(),
        },
        SuppliedUpdateRecord {
            app_key: "other".into(),
            current_version: "1.0.0".into(),
            candidate_version: None,
            feed_url: "https://updates.example.test/feed.json".into(),
        },
    ]);
    assert!(!result.network_performed);
    assert_eq!(result.entries[0].status, UpdateFeedStatus::InvalidUrl);
    assert_eq!(result.entries[1].status, UpdateFeedStatus::Unavailable);
    assert_eq!(
        compare_versions("1.2", "1.2.0"),
        VersionComparison::NotNewer
    );
    assert_eq!(
        compare_versions("made-up", "1.0"),
        VersionComparison::Unknown
    );
}

#[test]
fn bounded_history_keys_samples_by_incarnation_and_session() {
    let mut history = ProcessHistory::new(2);
    let process = cockpit_core::ProcessIdentity {
        pid: 7,
        start_time: 42,
    };
    fn metric<T>(value: Option<T>) -> Metric<T> {
        Metric {
            value,
            capability: Capability::Available,
            label: "bytes".into(),
        }
    }
    let clock = FixedClock(100);
    history.begin_session();
    history.record_with_clock(
        process.clone(),
        metric(Some(1.0)),
        metric(Some(1)),
        metric(None::<f32>),
        &clock,
    );
    history.record_with_clock(
        process.clone(),
        metric(Some(2.0)),
        metric(Some(2)),
        metric(None::<f32>),
        &clock,
    );
    history.record_with_clock(
        process.clone(),
        metric(Some(3.0)),
        metric(Some(3)),
        metric(None::<f32>),
        &clock,
    );
    assert_eq!(history.samples_for(&process).len(), 2);
    assert_eq!(
        history.samples_for(&process)[0].sequence + 1,
        history.samples_for(&process)[1].sequence
    );
    assert_eq!(history.samples_for_session(history.session_id).len(), 2);
    // A reused PID is a different process incarnation and gets its own bound.
    let reused = cockpit_core::ProcessIdentity {
        pid: 7,
        start_time: 43,
    };
    history.record_with_clock(
        reused.clone(),
        metric(Some(1.0)),
        metric(Some(1)),
        metric(None::<f32>),
        &clock,
    );
    assert_eq!(history.samples_for(&reused).len(), 1);
}

#[test]
fn byte_totals_report_units_and_saturating_overflow() {
    let totals = ByteTotals::from_pairs([(u64::MAX, u64::MAX), (1, 1)]);
    assert_eq!(ByteTotals::UNIT, "bytes");
    assert_eq!(totals.logical_bytes, u64::MAX);
    assert_eq!(totals.attributed_allocation_bytes, u64::MAX);
    assert!(totals.overflowed);
}
