use pulse_core::activity::*;

fn event(id: &str, at: u64, kind: ActivityKind) -> ActivityEvent {
    ActivityEvent {
        id: id.into(),
        occurred_at: at,
        kind,
    }
}

#[test]
fn duplicate_event_is_idempotent_but_changed_duplicate_is_rejected() {
    let mut ledger = ActivityLedger::default();
    let e = event(
        "move-1",
        100,
        ActivityKind::CleanupMoved {
            moved_bytes: 8,
            logical_bytes: 10,
        },
    );
    assert_eq!(ledger.record(e.clone()), Ok(RecordStatus::Inserted));
    assert_eq!(ledger.record(e), Ok(RecordStatus::DuplicateIgnored));
    assert_eq!(
        ledger.record(event(
            "move-1",
            100,
            ActivityKind::CleanupMoved {
                moved_bytes: 9,
                logical_bytes: 10
            }
        )),
        Err(ActivityError::DuplicateConflict)
    );
}

#[test]
fn moved_logical_and_observed_delta_stay_separate() {
    let mut ledger = ActivityLedger::default();
    ledger
        .record(event(
            "move",
            1_704_067_200,
            ActivityKind::CleanupMoved {
                moved_bytes: 80,
                logical_bytes: 100,
            },
        ))
        .unwrap();
    ledger
        .record(event(
            "obs",
            1_704_067_201,
            ActivityKind::VolumeObservation {
                used_delta_bytes: -50,
            },
        ))
        .unwrap();
    ledger
        .record(event(
            "restore",
            1_704_067_202,
            ActivityKind::CleanupRestored { logical_bytes: 100 },
        ))
        .unwrap();
    let totals = ledger.weekly(1_704_067_203);
    assert_eq!(totals.moved_bytes, 80);
    assert_eq!(totals.logical_bytes, 100);
    assert_eq!(totals.observed_volume_delta_bytes, -50);
    assert_eq!(totals.observed_reclaimed_bytes, 50);
    assert_eq!(totals.restored_events, 1);
}

#[test]
fn weekly_and_monthly_windows_are_utc_and_have_explicit_bounds() {
    let timestamp = 1_704_067_200; // 2024-01-01 00:00:00 UTC, Monday.
    let week = week_window_utc(timestamp);
    let month = month_window_utc(timestamp);
    assert_eq!(week.start, timestamp);
    assert_eq!(week.end - week.start, 7 * 86_400);
    assert_eq!(month.start, timestamp);
    assert!(month.end > timestamp);
    assert!(month.contains(timestamp));
    assert!(!month.contains(month.end));
    assert!(UtcWindow::new(10, 10).is_none());
}
