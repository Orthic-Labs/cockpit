//! Table-driven evidence tests for every rule family in rules/initial.json.
//! Pure data in, findings out: no filesystem, process or clock access.
//! `Option::None` always means "unknown"; nothing here invents a value the
//! scanner would not have supplied.

use pulse_core::rules::*;

type Mutator = fn(&mut ScanMetadata);

fn m(f: Mutator) -> Mutator {
    f
}

fn pack() -> RulePack {
    serde_json::from_str(include_str!("../../rules/initial.json")).expect("pack parses")
}

fn rule(id: &str) -> Rule {
    pack()
        .rules
        .into_iter()
        .find(|r| r.id == id)
        .unwrap_or_else(|| panic!("missing rule {id}"))
}

fn actionable_ids() -> Vec<String> {
    pack()
        .rules
        .iter()
        .filter(|r| r.is_actionable())
        .map(|r| r.id.clone())
        .collect()
}

fn key_name(key: EvidenceKey) -> String {
    serde_json::to_value(key)
        .unwrap()
        .as_str()
        .unwrap()
        .to_string()
}

/// A representative in-scope path for each rule family.
fn sample_path(id: &str) -> &'static str {
    match id {
        "chrome-signing-copies" => {
            "/private/var/folders/ab/T/com.google.Chrome.code_sign_clone/code_sign_clone.123"
        }
        "orphan-build-workspaces" => "/Volumes/Ext/.rightkit-managed/workspaces/job1",
        "stale-build-temp" => "/Volumes/Ext/.build-home/x/tmp123",
        "stale-graph-index" => "/Volumes/Ext/proj/.graph/a.bin",
        "agent-temp-model-cache" => "/Volumes/Ext/u/.cache/huggingface/model.bin",
        "workspace-trash" => "/Volumes/Ext/.workspace-trash/a",
        "obsolete-app-backups" => "/Applications/Tool.app.prev-1",
        "xcode-artifacts" => "/Users/u/Library/Developer/Xcode/iOS DeviceSupport/17.0",
        "app-leftovers" => "/Users/u/Library/Caches/com.example.gone",
        other => panic!("no sample for {other}"),
    }
}

/// Every signal the rule consults is set to its satisfied value; sizes and
/// age are known.  Signals the rule does not consult stay unknown (None), so
/// the baseline proves rules do not depend on unrelated signals.
fn clean(id: &str) -> ScanMetadata {
    let r = rule(id);
    let mut scan = ScanMetadata {
        path: sample_path(id).to_string(),
        volume_id: Some("vol-1".into()),
        volume_mounted: Some(true),
        path_state: PathState::Present,
        age_days: Some(400),
        logical_bytes: Some(10),
        attributed_bytes: Some(20),
        unique_bytes: Some(5),
        liveness: Liveness::NotInUse,
        evidence: ScanEvidence {
            inspection_complete: Some(true),
            ownership_confirmed: Some(true),
            protected_descendant: Some(false),
            source_repository: Some(false),
            user_data: Some(false),
            cloud_placeholder: Some(false),
            ..Default::default()
        },
        ..Default::default()
    };
    if r.volumes.contains(&VolumeScope::ExternalVolume) {
        scan.is_external_volume = Some(true);
        scan.is_startup_volume = Some(false);
    }
    if r.volumes.contains(&VolumeScope::StartupVolume) {
        scan.is_startup_volume = Some(true);
        scan.is_external_volume = Some(false);
    }
    for key in r.liveness.iter().chain(r.eligibility.iter()) {
        set_signal(&mut scan, &key_name(*key), Some(true));
    }
    scan
}

/// `satisfied`: Some(true) = requirement met, Some(false) = measured failure,
/// None = unknown.
fn set_signal(scan: &mut ScanMetadata, name: &str, satisfied: Option<bool>) {
    let inv = satisfied.map(|v| !v);
    let e = &mut scan.evidence;
    match name {
        "liveness_not_in_use" => {
            scan.liveness = match satisfied {
                Some(true) => Liveness::NotInUse,
                Some(false) => Liveness::InUse,
                None => Liveness::Unknown,
            }
        }
        "chrome_family_stopped" => e.chrome_family_running = inv,
        "active_residents_stopped" => e.active_residents = inv,
        "owner_state_complete" => e.owner_state_complete = satisfied,
        "source_root_confirmed_absent" => e.source_root_exists = inv,
        "owner_lease_inactive" => e.owner_lease_active = inv,
        "generator_stopped" => e.generating_tool_running = inv,
        "replacement_newer" => e.replacement_newer = satisfied,
        "model_cache_redownloadable" => e.model_cache_redownloadable = satisfied,
        "app_not_installed" => e.app_installed = inv,
        "app_process_stopped" => e.app_process_running = inv,
        "inventory_complete" => e.inventory_complete = satisfied,
        "simulator_not_in_use" => e.simulator_in_use = inv,
        other => panic!("unmapped signal {other}"),
    }
}

fn eval(id: &str, scan: &ScanMetadata) -> Option<Finding> {
    rule(id).evaluate(scan)
}

fn has(f: &Finding, needle: &str) -> bool {
    f.reasons
        .iter()
        .any(|r| r == needle || r.starts_with(needle))
}

#[test]
fn pack_is_structurally_safe_and_report_only() {
    let p = pack();
    assert_eq!(p.validate(), Vec::<String>::new());
    assert!(p.rules.iter().all(|r| r.report_only));
    assert_eq!(actionable_ids().len(), 9);
    assert_eq!(p.rules.len(), 12);
}

#[test]
fn explanation_rules_never_attach_or_become_eligible() {
    for r in pack().rules.iter().filter(|r| !r.is_actionable()) {
        assert_eq!(r.risk, Risk::Explanation, "{}", r.id);
        assert_eq!(r.route, CleanupRoute::None, "{}", r.id);
        assert!(r.explanation.as_deref().is_some_and(|t| !t.is_empty()));
        for path in ["/", "/Volumes/Ext/x", "/Users/u/Library/Caches/x"] {
            let scan = ScanMetadata {
                path: path.into(),
                ..Default::default()
            };
            assert!(r.evaluate(&scan).is_none(), "{} matched {path}", r.id);
        }
    }
}

#[test]
fn fully_evidenced_rows_are_the_only_eligible_rows() {
    for id in actionable_ids() {
        let f = eval(&id, &clean(&id)).unwrap_or_else(|| panic!("{id}: no finding"));
        assert!(f.eligible, "{id}: {:?}", f.reasons);
        assert!(f.reasons.is_empty(), "{id}: {:?}", f.reasons);
        assert!(f.unknown_signals.is_empty());
        assert_eq!(f.liveness, Liveness::NotInUse);
    }
}

#[test]
fn every_declared_requirement_unknown_failed_or_in_use_blocks() {
    for id in actionable_ids() {
        let r = rule(&id);
        let keys: Vec<String> = r
            .liveness
            .iter()
            .chain(r.eligibility.iter())
            .map(|k| key_name(*k))
            .collect();
        assert!(!keys.is_empty(), "{id}");
        for key in keys {
            // Unknown: names the missing signal.
            let mut scan = clean(&id);
            set_signal(&mut scan, &key, None);
            let f = eval(&id, &scan).unwrap();
            assert!(!f.eligible, "{id}/{key} unknown");
            assert!(
                has(&f, &format!("evidence_unknown:{key}")),
                "{id}/{key}: {:?}",
                f.reasons
            );
            assert!(
                f.unknown_signals.contains(&key),
                "{id}/{key}: {:?}",
                f.unknown_signals
            );
            if key == "liveness_not_in_use" {
                assert_eq!(f.liveness, Liveness::Unknown);
                assert!(
                    f.reasons
                        .iter()
                        .any(|r| r.contains("scanner_liveness_not_reported"))
                );
            }

            // Measured failure: ineligible, not reported as unknown.
            let mut scan = clean(&id);
            set_signal(&mut scan, &key, Some(false));
            let f = eval(&id, &scan).unwrap();
            assert!(!f.eligible, "{id}/{key} failed");
            assert!(
                has(&f, &format!("evidence_failed:{key}")),
                "{id}/{key}: {:?}",
                f.reasons
            );
            assert!(!f.unknown_signals.contains(&key));
        }
    }
}

#[test]
fn baseline_protections_apply_to_every_rule() {
    let table: Vec<(&str, Mutator, &str, bool)> = vec![
        (
            "volume unknown",
            m(|s| s.volume_mounted = None),
            "evidence_unknown:volume_mounted",
            true,
        ),
        (
            "volume unmounted",
            m(|s| s.volume_mounted = Some(false)),
            "evidence_failed:volume_mounted",
            true,
        ),
        (
            "path absent",
            m(|s| s.path_state = PathState::Absent),
            "evidence_failed:path_present",
            true,
        ),
        (
            "path inaccessible",
            m(|s| s.path_state = PathState::Inaccessible),
            "evidence_unknown:path_present:path_inaccessible",
            true,
        ),
        (
            "path unknown",
            m(|s| s.path_state = PathState::Unknown),
            "evidence_unknown:path_present:path_state_unknown",
            true,
        ),
        (
            "inspection unknown",
            m(|s| s.evidence.inspection_complete = None),
            "evidence_unknown:inspection_complete",
            true,
        ),
        (
            "inspection incomplete",
            m(|s| s.evidence.inspection_complete = Some(false)),
            "evidence_failed:inspection_complete",
            true,
        ),
        (
            "ownership unknown",
            m(|s| s.evidence.ownership_confirmed = None),
            "evidence_unknown:ownership_confirmed",
            true,
        ),
        (
            "ownership unconfirmed",
            m(|s| s.evidence.ownership_confirmed = Some(false)),
            "evidence_failed:ownership_confirmed",
            false,
        ),
        (
            "protected descendant",
            m(|s| s.evidence.protected_descendant = Some(true)),
            "protected:no_protected_descendant",
            false,
        ),
        (
            "protected unknown",
            m(|s| s.evidence.protected_descendant = None),
            "evidence_unknown:no_protected_descendant",
            true,
        ),
        (
            "source repo",
            m(|s| s.evidence.source_repository = Some(true)),
            "protected:no_source_repository",
            false,
        ),
        (
            "source repo unknown",
            m(|s| s.evidence.source_repository = None),
            "evidence_unknown:no_source_repository",
            true,
        ),
        (
            "user data",
            m(|s| s.evidence.user_data = Some(true)),
            "protected:no_user_data",
            false,
        ),
        (
            "user data unknown",
            m(|s| s.evidence.user_data = None),
            "evidence_unknown:no_user_data",
            true,
        ),
        (
            "placeholder",
            m(|s| s.evidence.cloud_placeholder = Some(true)),
            "protected:no_cloud_placeholder",
            false,
        ),
        (
            "placeholder unknown",
            m(|s| s.evidence.cloud_placeholder = None),
            "evidence_unknown:no_cloud_placeholder",
            true,
        ),
        (
            "age below",
            m(|s| s.age_days = Some(0)),
            "age_below_threshold:",
            false,
        ),
        (
            "age unknown",
            m(|s| s.age_days = None),
            "evidence_unknown:age_threshold_met",
            true,
        ),
        (
            "volume id unknown",
            m(|s| s.volume_id = None),
            "evidence_unknown:volume_id",
            true,
        ),
        (
            "volume id blank",
            m(|s| s.volume_id = Some("  ".into())),
            "evidence_unknown:volume_id",
            true,
        ),
    ];
    for id in actionable_ids() {
        for (label, mutate, reason, unknown) in &table {
            let mut scan = clean(&id);
            mutate(&mut scan);
            let f = eval(&id, &scan).unwrap_or_else(|| panic!("{id}/{label}: no finding"));
            assert!(!f.eligible, "{id}/{label}");
            assert!(has(&f, reason), "{id}/{label}: {:?}", f.reasons);
            assert_eq!(
                !f.unknown_signals.is_empty(),
                *unknown,
                "{id}/{label}: {:?}",
                f.unknown_signals
            );
        }
    }
}

#[test]
fn unknown_liveness_never_hides_in_use_and_has_a_cause() {
    for id in actionable_ids() {
        // In use is preserved even when other evidence is missing.
        let mut scan = clean(&id);
        scan.liveness = Liveness::InUse;
        scan.evidence.inspection_complete = None;
        let f = eval(&id, &scan).unwrap();
        assert_eq!(f.liveness, Liveness::InUse, "{id}");
        assert!(!f.eligible);

        for (label, mutate, cause) in [
            (
                "unmounted",
                m(|s| s.volume_mounted = Some(false)),
                "volume_not_mounted",
            ),
            (
                "incomplete",
                m(|s| s.evidence.inspection_complete = Some(false)),
                "inspection_incomplete_or_unknown",
            ),
            (
                "inaccessible",
                m(|s| s.path_state = PathState::Inaccessible),
                "path_inaccessible",
            ),
            (
                "absent",
                m(|s| s.path_state = PathState::Absent),
                "path_absent",
            ),
        ] {
            let mut scan = clean(&id);
            mutate(&mut scan);
            let f = eval(&id, &scan).unwrap();
            assert_eq!(f.liveness, Liveness::Unknown, "{id}/{label}");
            assert!(!f.eligible);
            assert!(
                f.reasons.iter().any(|r| r.contains(cause)),
                "{id}/{label}: {:?}",
                f.reasons
            );
        }
    }
}

#[test]
fn contradictory_evidence_is_never_eligible() {
    // Scanner says not in use but a process signal says otherwise.
    for (id, signal, reason) in [
        (
            "chrome-signing-copies",
            "chrome_family_stopped",
            "not_in_use_but_chrome_family_running",
        ),
        (
            "orphan-build-workspaces",
            "owner_lease_inactive",
            "not_in_use_but_owner_lease_active",
        ),
        (
            "stale-build-temp",
            "active_residents_stopped",
            "not_in_use_but_active_residents",
        ),
        (
            "stale-graph-index",
            "generator_stopped",
            "not_in_use_but_generating_tool_running",
        ),
        (
            "obsolete-app-backups",
            "app_process_stopped",
            "not_in_use_but_app_process_running",
        ),
        (
            "xcode-artifacts",
            "simulator_not_in_use",
            "not_in_use_but_simulator_in_use",
        ),
    ] {
        let mut scan = clean(id);
        assert_eq!(scan.liveness, Liveness::NotInUse);
        set_signal(&mut scan, signal, Some(false));
        let f = eval(id, &scan).unwrap();
        assert!(!f.eligible, "{id}");
        assert!(
            has(&f, &format!("evidence_conflict:{reason}")),
            "{id}: {:?}",
            f.reasons
        );
    }

    // Replacement newer but the app is recorded as not installed.
    let mut scan = clean("obsolete-app-backups");
    scan.evidence.app_installed = Some(false);
    let f = eval("obsolete-app-backups", &scan).unwrap();
    assert!(!f.eligible);
    assert!(has(
        &f,
        "evidence_conflict:replacement_newer_but_app_not_installed"
    ));

    // Startup and external at once.
    let mut scan = clean("stale-graph-index");
    scan.is_startup_volume = Some(true);
    scan.is_external_volume = Some(true);
    let f = eval("stale-graph-index", &scan).unwrap();
    assert!(!f.eligible);
    assert!(has(&f, "evidence_conflict:volume_startup_and_external"));
}

#[test]
fn volume_scope_must_be_confirmed_not_assumed() {
    for id in actionable_ids() {
        let r = rule(&id);
        for scope in &r.volumes {
            let (flag_unknown, flag_false): (Mutator, Mutator) = match scope {
                VolumeScope::ExternalVolume => (
                    m(|s| s.is_external_volume = None),
                    m(|s| s.is_external_volume = Some(false)),
                ),
                VolumeScope::StartupVolume => (
                    m(|s| s.is_startup_volume = None),
                    m(|s| s.is_startup_volume = Some(false)),
                ),
                VolumeScope::AnyMountedLocal => continue,
            };
            let mut scan = clean(&id);
            flag_unknown(&mut scan);
            let f = eval(&id, &scan).unwrap();
            assert!(!f.eligible, "{id}");
            assert!(
                has(&f, "evidence_unknown:volume_scope"),
                "{id}: {:?}",
                f.reasons
            );
            assert!(f.unknown_signals.contains(&"volume_scope".to_string()));

            // Known to be on another kind of volume: out of scope entirely.
            let mut scan = clean(&id);
            flag_false(&mut scan);
            assert!(eval(&id, &scan).is_none(), "{id}");
        }
    }
}

#[test]
fn missing_measured_size_is_reported_not_invented() {
    for id in actionable_ids() {
        let r = rule(&id);
        let (name, clear): (&str, Mutator) = match r.measurement {
            Measurement::CloneAwareUniqueBytes => ("unique_bytes", m(|s| s.unique_bytes = None)),
            Measurement::AttributedAllocation => {
                ("attributed_bytes", m(|s| s.attributed_bytes = None))
            }
            Measurement::LogicalBytes => ("logical_bytes", m(|s| s.logical_bytes = None)),
            Measurement::ExplanationOnly => panic!("{id}"),
        };
        let mut scan = clean(&id);
        clear(&mut scan);
        let f = eval(&id, &scan).unwrap();
        assert!(!f.eligible, "{id}");
        assert!(
            has(&f, &format!("size_unknown:{name}")),
            "{id}: {:?}",
            f.reasons
        );
        assert!(f.unknown_signals.contains(&name.to_string()));

        // Sizes pass through untouched; estimates are never derived.
        let mut scan = clean(&id);
        scan.logical_bytes = None;
        scan.attributed_bytes = None;
        scan.unique_bytes = None;
        let f = eval(&id, &scan).unwrap();
        assert_eq!(f.logical_bytes, None);
        assert_eq!(f.attributed_bytes, None);
        assert_eq!(f.unique_bytes, None);
        assert_eq!(f.deletion_estimate_lower, None);
        assert_eq!(f.deletion_estimate_upper, None);
    }

    // A zero size is a known size.
    let mut scan = clean("workspace-trash");
    scan.attributed_bytes = Some(0);
    assert!(eval("workspace-trash", &scan).unwrap().eligible);

    // Inverted bounds are withheld and flagged.
    let mut scan = clean("workspace-trash");
    scan.deletion_estimate_lower = Some(10);
    scan.deletion_estimate_upper = Some(1);
    let f = eval("workspace-trash", &scan).unwrap();
    assert!(!f.eligible);
    assert_eq!(
        (f.deletion_estimate_lower, f.deletion_estimate_upper),
        (None, None)
    );
    assert!(has(&f, "estimate_inconsistent"));

    // Valid bounds are passed through as supplied.
    let mut scan = clean("workspace-trash");
    scan.deletion_estimate_lower = Some(1);
    scan.deletion_estimate_upper = Some(10);
    let f = eval("workspace-trash", &scan).unwrap();
    assert!(f.eligible);
    assert_eq!(
        (f.deletion_estimate_lower, f.deletion_estimate_upper),
        (Some(1), Some(10))
    );
}

#[test]
fn report_only_findings_name_every_unknown_signal() {
    let mut scan = clean("orphan-build-workspaces");
    scan.evidence.owner_state_complete = None;
    scan.evidence.owner_lease_active = None;
    scan.evidence.user_data = None;
    scan.age_days = None;
    scan.attributed_bytes = None;
    let f = eval("orphan-build-workspaces", &scan).unwrap();
    assert!(!f.eligible);
    assert_eq!(
        f.unknown_signals,
        vec![
            "age_threshold_met",
            "attributed_bytes",
            "no_user_data",
            "owner_lease_inactive",
            "owner_state_complete",
        ]
    );
    for name in &f.unknown_signals {
        assert!(
            f.reasons.iter().any(|r| r.contains(name.as_str())),
            "{name} not explained in {:?}",
            f.reasons
        );
    }
    // Unknown evidence is listed even when an earlier failure already
    // makes the row ineligible.
    let mut scan = clean("app-leftovers");
    scan.evidence.app_installed = Some(true);
    scan.evidence.inventory_complete = None;
    let f = eval("app-leftovers", &scan).unwrap();
    assert!(has(&f, "evidence_failed:app_not_installed"));
    assert!(has(&f, "evidence_unknown:inventory_complete"));
}

#[test]
fn path_matching_is_segment_exact_and_case_insensitive() {
    // (rule, path, should produce a finding)
    let table = [
        ("workspace-trash", "/Volumes/Ext/.workspace-trash/a", true),
        ("workspace-trash", "/Volumes/Ext/.WORKSPACE-TRASH/A", true),
        (
            "workspace-trash",
            "\\Volumes\\Ext\\.workspace-trash\\a",
            true,
        ),
        (
            "workspace-trash",
            "/Volumes/Ext//.workspace-trash/./a/",
            true,
        ),
        ("workspace-trash", "/Volumes/Ext/.workspace-trash", false),
        ("workspace-trash", "/Volumes/Ext/.workspace-trash/", false),
        (
            "workspace-trash",
            "/Volumes/Ext/.workspace-trash-old/a",
            false,
        ),
        ("workspace-trash", "/Volumes/Ext/x.workspace-trash/a", false),
        ("workspace-trash", "/Volumes/.workspace-trash/a", false),
        (
            "workspace-trash",
            "/Volumes/Ext/Sub/.workspace-trash/a",
            false,
        ),
        (
            "obsolete-app-backups",
            "/Applications/Tool.app.prev-1",
            true,
        ),
        ("obsolete-app-backups", "/Applications/Tool.app.pre-2", true),
        (
            "obsolete-app-backups",
            "/applications/tool.app.PREV-1",
            true,
        ),
        ("obsolete-app-backups", "/Applications/Tool.app", false),
        (
            "obsolete-app-backups",
            "/Applications/Tool.app.prev-1/Contents",
            false,
        ),
        (
            "obsolete-app-backups",
            "/Applications/Sub/Tool.app.prev-1",
            false,
        ),
        (
            "obsolete-app-backups",
            "/Applications2/Tool.app.prev-1",
            false,
        ),
        ("app-leftovers", "/Users/u/Library/Caches/com.example", true),
        ("app-leftovers", "/Users/u/Library/Caches", false),
        ("app-leftovers", "/Users/u/Library/Caches-old/x", false),
        (
            "app-leftovers",
            "/Users/u/Library/Application Support/Foo",
            true,
        ),
        (
            "app-leftovers",
            "/Users/u/Library/Application Support",
            false,
        ),
        ("app-leftovers", "/Library/Preferences/x.plist", true),
        ("app-leftovers", "/Library/Preferences", false),
        ("stale-graph-index", "/Volumes/Ext/p/q/graph-1.json", true),
        (
            "stale-graph-index",
            "/Volumes/Ext/p/q/graph.json.bak",
            false,
        ),
        ("stale-graph-index", "/Volumes/Ext/p/q/index9.db", true),
        ("stale-graph-index", "/Volumes/Ext/p/.index", false),
        ("stale-build-temp", "/Volumes/Ext/.build-home/tmpabc", true),
        (
            "stale-build-temp",
            "/Volumes/Ext/.build-home/a/b/runs/1",
            true,
        ),
        ("stale-build-temp", "/Volumes/Ext/.build-home/runs", false),
        (
            "stale-build-temp",
            "/Volumes/Ext/.build-home/tmpabc/deeper",
            false,
        ),
        (
            "chrome-signing-copies",
            "/var/folders/a/b/com.google.Chrome.code_sign_clone/code_sign_clone.9",
            true,
        ),
        (
            "chrome-signing-copies",
            "/var/folders/a/b/com.google.Chrome.code_sign_clone",
            false,
        ),
        (
            "chrome-signing-copies",
            "/var/folders/a/b/com.example.code_sign_clone/code_sign_clone.9",
            false,
        ),
        (
            "xcode-artifacts",
            "/Users/u/Library/Developer/CoreSimulator/Devices/ABC",
            true,
        ),
        (
            "xcode-artifacts",
            "/Users/u/Library/Developer/CoreSimulator/Devices",
            false,
        ),
        (
            "xcode-artifacts",
            "/Library/Developer/CoreSimulator/Profiles/Runtimes/iOS.simruntime",
            true,
        ),
        (
            "agent-temp-model-cache",
            "/Volumes/Ext/u/tmp/local-assistants-1/x",
            true,
        ),
        (
            "agent-temp-model-cache",
            "/Volumes/Ext/u/tmp/local-assistants-1",
            false,
        ),
        (
            "orphan-build-workspaces",
            "/Volumes/Ext/.rightkit-managed/ws/j",
            true,
        ),
        (
            "orphan-build-workspaces",
            "/Volumes/Ext/.rightkit-managed",
            false,
        ),
    ];
    for (id, path, expect) in table {
        let mut scan = clean(id);
        scan.path = path.to_string();
        assert_eq!(eval(id, &scan).is_some(), expect, "{id} {path}");
    }
}

#[test]
fn relative_and_traversing_paths_match_but_never_become_eligible() {
    for (path, problem) in [
        ("Volumes/Ext/.workspace-trash/a", "relative"),
        (
            "/Volumes/Ext/.workspace-trash/../Documents/x",
            "parent_traversal",
        ),
        (
            "\\Volumes\\Ext\\.workspace-trash\\..\\x",
            "parent_traversal",
        ),
    ] {
        let mut scan = clean("workspace-trash");
        scan.path = path.into();
        let f = eval("workspace-trash", &scan).unwrap_or_else(|| panic!("{path}"));
        assert!(!f.eligible, "{path}");
        assert!(
            has(&f, &format!("path_not_canonical:{problem}")),
            "{path}: {:?}",
            f.reasons
        );
    }
}

#[test]
fn finding_ids_are_stable_and_order_independent() {
    let id = "workspace-trash";
    let a = stable_finding_id(id, "/Volumes/Ext/.workspace-trash/a");
    assert_eq!(a, stable_finding_id(id, "/Volumes/Ext/.workspace-trash/a"));
    assert_eq!(
        a,
        stable_finding_id(id, "\\Volumes\\Ext\\.workspace-trash\\a")
    );
    assert_eq!(
        a,
        stable_finding_id(id, "/Volumes//Ext/./.workspace-trash/a/")
    );
    assert_ne!(a, stable_finding_id(id, "/Volumes/Ext/.workspace-trash/b"));
    assert_ne!(a, stable_finding_id(id, "/Volumes/Ext/.Workspace-Trash/a"));
    assert_ne!(a, stable_finding_id(id, "Volumes/Ext/.workspace-trash/a"));
    assert_ne!(
        a,
        stable_finding_id("other", "/Volumes/Ext/.workspace-trash/a")
    );
    assert!(a.starts_with("finding:workspace-trash:"));

    let rules = pack().rules;
    let mut rows: Vec<ScanMetadata> = actionable_ids().iter().map(|i| clean(i)).collect();
    let forward = evaluate_all(&rules, &rows);
    rows.reverse();
    let reversed = evaluate_all(&rules, &rows);
    let ids = |v: &[Finding]| v.iter().map(|f| f.id.clone()).collect::<Vec<_>>();
    assert_eq!(ids(&forward), ids(&reversed));
    let mut sorted = ids(&forward);
    sorted.sort();
    assert_eq!(ids(&forward), sorted);
    assert_eq!(
        serde_json::to_string(&forward).unwrap(),
        serde_json::to_string(&reversed).unwrap()
    );
    // Rule order does not change the output either.
    let mut shuffled = rules.clone();
    shuffled.reverse();
    assert_eq!(ids(&forward), ids(&evaluate_all(&shuffled, &rows)));
}

#[test]
fn duplicate_rows_collapse_and_conflicts_are_ineligible() {
    let rules = pack().rules;
    let good = clean("workspace-trash");
    let same = evaluate_all(&rules, &[good.clone(), good.clone()]);
    assert_eq!(same.len(), 1);
    assert!(same[0].eligible);

    // Same path, one row eligible and one with unknown liveness.
    let mut bad = good.clone();
    bad.evidence.inspection_complete = None;
    for rows in [[good.clone(), bad.clone()], [bad.clone(), good.clone()]] {
        let out = evaluate_all(&rules, &rows);
        assert_eq!(out.len(), 1);
        assert!(!out[0].eligible);
        assert!(has(&out[0], "evidence_conflict:duplicate_scan_rows"));
    }

    // Same path, both eligible but with different sizes: still withheld.
    let mut other = good.clone();
    other.attributed_bytes = Some(999);
    let out = evaluate_all(&rules, &[good, other]);
    assert_eq!(out.len(), 1);
    assert!(!out[0].eligible);
}

#[test]
fn explicit_unknown_defaults_are_never_eligible() {
    // A bare row (everything unknown) must not become eligible for any rule.
    let rules = pack().rules;
    for id in actionable_ids() {
        let scan = ScanMetadata {
            path: sample_path(&id).into(),
            ..Default::default()
        };
        let f = rules
            .iter()
            .find(|r| r.id == id)
            .unwrap()
            .evaluate(&scan)
            .unwrap();
        assert!(!f.eligible, "{id}");
        assert_eq!(f.liveness, Liveness::Unknown);
        assert!(!f.unknown_signals.is_empty());
        assert!(!f.reasons.is_empty());
    }
}

#[test]
fn findings_carry_rule_text_for_the_report() {
    let f = eval("workspace-trash", &clean("workspace-trash")).unwrap();
    assert_eq!(f.risk, Risk::Review);
    assert_eq!(f.route, CleanupRoute::Trash);
    assert!(
        f.route_detail
            .as_deref()
            .is_some_and(|t| t.contains("explicit review"))
    );
    assert_eq!(f.rule_version, rule("workspace-trash").rule_version);
}
