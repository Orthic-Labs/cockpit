use cockpit_core::cleanup::*;
use cockpit_core::model::{EntryKind, FileIdentity, VolumeIdentity};
use std::path::PathBuf;

fn item(path: &str, id: &str) -> CleanupItem {
    let volume = VolumeIdentity::new("vol-a");
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

fn effect() -> EffectBinding {
    EffectBinding {
        action: CleanupAction::Trash,
        rule_id: "stale-temp".into(),
        rule_version: 1,
        reversible: true,
    }
}

fn plan(items: Vec<CleanupItem>) -> CleanupPlan {
    CleanupPlan::draft("plan-1", 10, 100, effect(), items)
        .unwrap()
        .seal()
        .unwrap()
}

struct Revalidate {
    result: Result<(), RevalidationError>,
}
impl Revalidator for Revalidate {
    fn revalidate(&mut self, _: &CleanupPlan, _: &CleanupItem) -> Result<(), RevalidationError> {
        self.result.clone()
    }
}

struct Execute {
    outcomes: Vec<ItemOutcome>,
    calls: usize,
}
impl CleanupExecutor for Execute {
    fn execute(&mut self, _: &EffectBinding, _: &CleanupItem) -> ItemOutcome {
        let outcome = self
            .outcomes
            .get(self.calls)
            .cloned()
            .unwrap_or(ItemOutcome::Failed {
                reason: "missing fixture outcome".into(),
            });
        self.calls += 1;
        outcome
    }
}

fn moved() -> ItemOutcome {
    let volume = VolumeIdentity::new("vol-a");
    ItemOutcome::MovedToTrash {
        trash: TrashIdentity {
            volume: volume.clone(),
            identity: FileIdentity {
                volume,
                id: "trash-1".into(),
            },
            path: "/.Trash/plan-1".into(),
        },
        moved_bytes: 8,
    }
}

#[test]
fn overlapping_targets_are_deduplicated_to_outer_target() {
    let p = plan(vec![item("/tmp/a", "a"), item("/tmp/a/child", "child")]);
    assert_eq!(p.items.len(), 1);
    assert_eq!(p.items[0].path, PathBuf::from("/tmp/a"));
}

#[test]
fn protected_descendant_refuses_plan_before_claim() {
    let mut protected = item("/tmp/a", "a");
    protected.protected_descendant = true;
    assert!(matches!(
        CleanupPlan::draft("p", 1, 2, effect(), vec![protected]),
        Err(PlanError::ProtectedDescendant(_))
    ));
}

#[test]
fn claim_is_one_time_and_expiry_is_enforced() {
    let p = plan(vec![item("/tmp/a", "a")]);
    let mut journal = Journal::default();
    assert_eq!(journal.claim(&p, 100), Err(ClaimError::Expired));
    assert_eq!(journal.claim(&p, 20), Ok(()));
    assert_eq!(journal.claim(&p, 20), Err(ClaimError::AlreadyClaimed));
}

#[test]
fn changed_effect_cannot_reuse_claim() {
    let mut p = plan(vec![item("/tmp/a", "a")]);
    let mut journal = Journal::default();
    journal.claim(&p, 20).unwrap();
    p.effect.rule_version = 2;
    assert_eq!(journal.claim(&p, 20), Err(ClaimError::EffectChanged));
}

#[test]
fn interrupted_item_is_indeterminate_and_never_replayed() {
    let p = plan(vec![item("/tmp/a", "a"), item("/tmp/b", "b")]);
    let mut journal = Journal::default();
    let mut revalidator = Revalidate { result: Ok(()) };
    let mut executor = Execute {
        outcomes: vec![moved(), ItemOutcome::Interrupted],
        calls: 0,
    };
    let report = apply(&p, 20, &mut journal, &mut revalidator, &mut executor).unwrap();
    assert_eq!(report.state, JournalState::Interrupted);
    assert_eq!(report.freed_bytes, None);
    assert!(matches!(
        journal.entry("plan-1").unwrap().items[1].state,
        ItemJournalState::Indeterminate
    ));
    let mut second = Execute {
        outcomes: vec![moved()],
        calls: 0,
    };
    assert_eq!(
        apply(&p, 20, &mut journal, &mut revalidator, &mut second),
        Err(ClaimError::Interrupted)
    );
    assert_eq!(second.calls, 0);
}

#[test]
fn changed_identity_is_recorded_as_item_failure_without_execution() {
    let p = plan(vec![item("/tmp/a", "a")]);
    let mut journal = Journal::default();
    let mut revalidator = Revalidate {
        result: Err(RevalidationError::IdentityChanged),
    };
    let mut executor = Execute {
        outcomes: vec![moved()],
        calls: 0,
    };
    let report = apply(&p, 20, &mut journal, &mut revalidator, &mut executor).unwrap();
    assert!(matches!(
        report.items[0].outcome,
        ItemOutcome::Failed { .. }
    ));
    assert_eq!(executor.calls, 0);
}

#[test]
fn undo_only_uses_confirmed_moves_and_reports_restore_conflict() {
    let p = plan(vec![item("/tmp/a", "a")]);
    let mut journal = Journal::default();
    let mut revalidator = Revalidate { result: Ok(()) };
    let mut executor = Execute {
        outcomes: vec![moved()],
        calls: 0,
    };
    let report = apply(&p, 20, &mut journal, &mut revalidator, &mut executor).unwrap();
    let mut undo = UndoPlan::from_apply("undo-1", 30, 100, &report)
        .unwrap()
        .seal();
    struct UndoCheck;
    impl UndoRevalidator for UndoCheck {
        fn revalidate(&mut self, _: &UndoItem) -> Result<(), UndoOutcome> {
            Err(UndoOutcome::ConflictOriginalOccupied)
        }
    }
    struct UndoExec;
    impl UndoExecutor for UndoExec {
        fn restore(&mut self, _: &UndoItem) -> UndoOutcome {
            UndoOutcome::Restored
        }
    }
    let out = apply_undo(&mut undo, 40, &mut UndoCheck, &mut UndoExec).unwrap();
    assert_eq!(out[0].1, UndoOutcome::ConflictOriginalOccupied);
}
