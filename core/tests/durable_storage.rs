use cockpit_core::{
    activity::{ActivityEvent, ActivityKind, DurableActivityLedger, RecordStatus},
    cleanup::{
        CleanupAction, CleanupExecutor, CleanupItem, CleanupPlan, DurableJournal, EffectBinding,
        ItemOutcome, JournalState, RevalidationError, Revalidator,
    },
    model::{EntryKind, FileIdentity, VolumeIdentity},
};
use std::fs;

fn item(path: &str, id: &str) -> CleanupItem {
    let volume = VolumeIdentity::new("volume-a");
    CleanupItem {
        path: path.into(),
        volume: volume.clone(),
        identity: FileIdentity {
            volume,
            id: id.into(),
        },
        kind: EntryKind::File,
        inspection_complete: true,
        protected_descendant: false,
        logical_bytes: Some(10),
        moved_bytes: Some(8),
    }
}

fn plan(id: &str, item: CleanupItem) -> CleanupPlan {
    CleanupPlan::draft(
        id,
        10,
        100,
        EffectBinding {
            action: CleanupAction::Trash,
            rule_id: "fixture-rule".into(),
            rule_version: 1,
            reversible: true,
        },
        vec![item],
    )
    .unwrap()
    .seal()
    .unwrap()
}

struct Revalidate(Result<(), RevalidationError>);
impl Revalidator for Revalidate {
    fn revalidate(&mut self, _: &CleanupPlan, _: &CleanupItem) -> Result<(), RevalidationError> {
        self.0.clone()
    }
}

struct Executor {
    calls: usize,
    outcome: ItemOutcome,
}
impl CleanupExecutor for Executor {
    fn execute(&mut self, _: &EffectBinding, _: &CleanupItem) -> ItemOutcome {
        self.calls += 1;
        self.outcome.clone()
    }
}

fn hex_id(id: &str) -> String {
    id.as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn durable_filesystem_journey_restarts_rejects_replay_holds_namespace_and_rejects_unknown_state() {
    let root = std::env::temp_dir().join(format!(
        "cockpit-durable-e2e-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();

    let event = ActivityEvent {
        id: "cleanup/job/1".into(),
        occurred_at: 1_704_067_200,
        kind: ActivityKind::CleanupMoved {
            moved_bytes: 8,
            logical_bytes: 10,
        },
    };
    {
        let mut ledger = DurableActivityLedger::open(&root).unwrap();
        assert_eq!(
            ledger.record(event.clone()).unwrap(),
            RecordStatus::Inserted
        );
    }
    {
        let mut ledger = DurableActivityLedger::open(&root).unwrap();
        assert_eq!(
            ledger.record(event.clone()).unwrap(),
            RecordStatus::DuplicateIgnored
        );
        assert_eq!(ledger.weekly(event.occurred_at).moved_bytes, 8);
    }

    {
        let mut journal = DurableJournal::open(&root).unwrap();
        let interrupted = plan("plan-1", item("/tmp/fixture-a", "file-a"));
        let mut revalidator = Revalidate(Ok(()));
        let mut executor = Executor {
            calls: 0,
            outcome: ItemOutcome::Interrupted,
        };
        assert_eq!(
            journal
                .apply(&interrupted, 20, &mut revalidator, &mut executor)
                .unwrap()
                .state,
            JournalState::Interrupted
        );
        assert_eq!(executor.calls, 1);
    }
    let mut journal = DurableJournal::open(&root).unwrap();
    assert_eq!(
        journal.entry("plan-1").unwrap().state,
        JournalState::Interrupted
    );
    let replay = plan("plan-1", item("/tmp/fixture-a", "file-a"));
    let mut revalidator = Revalidate(Ok(()));
    let mut executor = Executor {
        calls: 0,
        outcome: ItemOutcome::Failed {
            reason: "replay".into(),
        },
    };
    assert_eq!(
        journal.apply(&replay, 20, &mut revalidator, &mut executor),
        Err(cockpit_core::cleanup::ClaimError::Interrupted)
    );
    assert_eq!(executor.calls, 0);

    let cleanup = root.join("cleanup");
    let canonical = root.join("cleanup-canonical");
    fs::rename(&cleanup, &canonical).unwrap();
    fs::create_dir(&cleanup).unwrap();
    let held_plan = plan("plan-2", item("/tmp/fixture-b", "file-b"));
    let mut revalidator = Revalidate(Err(RevalidationError::IdentityChanged));
    let mut executor = Executor {
        calls: 0,
        outcome: ItemOutcome::Failed {
            reason: "must-not-execute".into(),
        },
    };
    assert_eq!(
        journal
            .apply(&held_plan, 20, &mut revalidator, &mut executor)
            .unwrap()
            .state,
        JournalState::Completed
    );
    assert_eq!(executor.calls, 0);
    assert!(fs::read_dir(&canonical).unwrap().next().is_some());
    assert!(fs::read_dir(&cleanup).unwrap().next().is_none());
    drop(journal);
    fs::remove_dir_all(&cleanup).unwrap();
    fs::rename(&canonical, &cleanup).unwrap();
    let restored = DurableJournal::open(&root).unwrap();
    assert!(restored.entry("plan-1").is_some());
    assert!(restored.entry("plan-2").is_some());

    let malformed = root
        .join("activity")
        .join(format!("record-{}-v2.json", hex_id(&event.id)));
    fs::write(malformed, b"{\"schema_version\":1").unwrap();
    assert!(DurableActivityLedger::open(&root).is_err());
    fs::remove_dir_all(root).unwrap();
}
