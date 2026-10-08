use pulse_core::processes::{GroupEvidence, ProcessGroups, group};
use pulse_core::{Capability, Metric, ProcessIdentity, ProcessInfo};

fn mem(v: Option<u64>) -> Metric<u64> {
    Metric {
        value: v,
        capability: if v.is_some() {
            Capability::Available
        } else {
            Capability::Unavailable
        },
        label: "resident memory (RSS)".into(),
    }
}

fn p(pid: u32, start: u64, parent: Option<u32>, cpu: f32, m: Option<u64>) -> ProcessInfo {
    ProcessInfo {
        identity: ProcessIdentity {
            pid,
            start_time: start,
        },
        name: "same-name".into(),
        parent_pid: parent,
        cpu_usage_percent: cpu,
        memory: mem(m),
        gpu_usage_percent: Metric {
            value: None,
            capability: Capability::Unavailable,
            label: "n/a".into(),
        },
    }
}

fn shape(g: &ProcessGroups) -> Vec<(u32, Vec<u32>)> {
    g.groups
        .iter()
        .map(|x| (x.root.pid, x.members.iter().map(|m| m.pid).collect()))
        .collect()
}

#[test]
fn table_of_group_shapes() {
    type GroupCase = (&'static str, Vec<ProcessInfo>, Vec<(u32, Vec<u32>)>, bool);
    let cases: Vec<GroupCase> = vec![
        (
            "helper chain",
            vec![
                p(1, 1, Some(0), 0.0, Some(1)),
                p(10, 5, Some(1), 0.0, Some(1)),
                p(11, 6, Some(10), 0.0, Some(1)),
                p(12, 7, Some(11), 0.0, Some(1)),
            ],
            vec![(1, vec![1]), (10, vec![10, 11, 12])],
            false,
        ),
        (
            "reused parent",
            vec![
                p(20, 100, Some(1), 0.0, Some(1)),
                p(21, 50, Some(20), 0.0, Some(1)),
            ],
            vec![(20, vec![20]), (21, vec![21])],
            true,
        ),
        (
            "missing parent",
            vec![
                p(30, 5, Some(999), 0.0, Some(1)),
                p(31, 6, Some(30), 0.0, Some(1)),
            ],
            vec![(30, vec![30, 31])],
            true,
        ),
        (
            "cycle",
            vec![
                p(40, 5, Some(41), 0.0, Some(1)),
                p(41, 5, Some(40), 0.0, Some(1)),
            ],
            vec![(40, vec![40, 41])],
            true,
        ),
        (
            "pid 1 children root themselves",
            vec![
                p(0, 0, None, 0.0, Some(1)),
                p(1, 1, Some(0), 0.0, Some(1)),
                p(50, 5, Some(1), 0.0, Some(1)),
                p(51, 6, Some(1), 0.0, Some(1)),
            ],
            vec![(0, vec![0]), (1, vec![1]), (50, vec![50]), (51, vec![51])],
            false,
        ),
        (
            "unknown parent is root without note",
            vec![p(60, 5, None, 0.0, Some(1))],
            vec![(60, vec![60])],
            false,
        ),
    ];
    for (name, procs, expected, has_notes) in cases {
        let g = group(&procs);
        assert_eq!(shape(&g), expected, "{name}");
        assert_eq!(
            !g.notes.is_empty(),
            has_notes,
            "{name}: notes {:?}",
            g.notes
        );
    }
}

#[test]
fn duplicate_pid_both_incarnations_retained() {
    // Two live incarnations of pid 5 (different start_times) must both
    // appear as rows — nothing is silently dropped.
    let g = group(&[
        p(5, 100, Some(1), 0.0, Some(1)),
        p(5, 200, Some(1), 0.0, Some(1)),
        p(10, 300, Some(1), 0.0, Some(1)),
    ]);
    let total: usize = g.groups.iter().map(|x| x.members.len()).sum();
    assert_eq!(total, 3);
    let idents: Vec<(u32, u64)> = g
        .groups
        .iter()
        .flat_map(|x| x.members.iter().map(|m| (m.pid, m.start_time)))
        .collect();
    assert!(idents.contains(&(5, 100)));
    assert!(idents.contains(&(5, 200)));
    assert!(
        g.notes
            .iter()
            .any(|n| n.contains("pid 5") && n.contains("incarnations"))
    );
}

#[test]
fn ambiguous_parent_is_not_attached() {
    // pid 7 has two live incarnations; a child claiming parent pid 7 cannot
    // be verified to a single identity, so it roots itself with a note.
    let g = group(&[
        p(7, 100, Some(1), 0.0, Some(1)),
        p(7, 200, Some(1), 0.0, Some(1)),
        p(30, 300, Some(7), 0.0, Some(1)),
    ]);
    for grp in &g.groups {
        assert!(
            !grp.members
                .iter()
                .any(|m| m.pid == 30 && grp.root.pid != 30)
        );
    }
    assert_eq!(g.groups.len(), 3);
    assert!(g.notes.iter().any(|n| n.contains("ambiguous")));
}

#[test]
fn duplicate_parent_pid_remains_ambiguous_when_one_started_later() {
    // A reused PID in one snapshot cannot establish a live parent identity.
    let g = group(&[
        p(8, 100, Some(1), 0.0, Some(1)),
        p(8, 500, Some(1), 0.0, Some(1)),
        p(40, 300, Some(8), 0.0, Some(1)),
    ]);
    let grp = g
        .groups
        .iter()
        .find(|x| x.root.pid == 40)
        .expect("child stays standalone");
    assert_eq!(grp.members.len(), 1);
    assert!(matches!(grp.evidence, GroupEvidence::SingleProcess));
    // Later incarnation still exists as its own group.
    assert!(
        g.groups
            .iter()
            .any(|x| x.root.pid == 8 && x.root.start_time == 500)
    );
}

#[test]
fn cycle_reports_identities_as_evidence() {
    let g = group(&[
        p(60, 5, Some(61), 0.0, Some(1)),
        p(61, 5, Some(60), 0.0, Some(1)),
    ]);
    assert!(
        g.notes
            .iter()
            .any(|n| n.contains("cycle") && n.contains("60@5") && n.contains("61@5"))
    );
    assert!(
        g.groups
            .iter()
            .all(|row| matches!(row.evidence, GroupEvidence::AmbiguousParentChain))
    );
}

#[test]
fn cpu_sums_and_memory_sums_when_complete() {
    let g = group(&[
        p(10, 1, Some(1), 1.5, Some(100)),
        p(11, 2, Some(10), 2.5, Some(200)),
    ]);
    let grp = &g.groups[0];
    assert_eq!(grp.cpu_usage_percent, 4.0);
    assert_eq!(grp.memory.value, Some(300));
    assert!(matches!(grp.memory.capability, Capability::Available));
    assert!(grp.memory.label.contains("physical footprint unavailable"));
    assert!(matches!(grp.evidence, GroupEvidence::VerifiedParentChain));
}

#[test]
fn partial_memory_is_unavailable_not_fabricated() {
    let g = group(&[
        p(10, 1, Some(1), 1.0, Some(100)),
        p(11, 2, Some(10), 1.0, None),
    ]);
    let grp = &g.groups[0];
    assert!(grp.memory.value.is_none());
    assert!(matches!(grp.memory.capability, Capability::Unavailable));
    assert!(grp.memory.label.contains("unavailable"));
}

#[test]
fn single_process_evidence_and_name_never_groups() {
    let g = group(&[
        p(70, 1, Some(1), 0.0, Some(1)),
        p(71, 2, Some(1), 0.0, Some(1)),
    ]);
    assert_eq!(g.groups.len(), 2);
    assert!(
        g.groups
            .iter()
            .all(|x| matches!(x.evidence, GroupEvidence::SingleProcess))
    );
}

#[test]
fn deterministic_regardless_of_input_order() {
    let a = vec![
        p(12, 7, Some(11), 1.0, Some(1)),
        p(10, 5, Some(1), 1.0, Some(1)),
        p(80, 9, Some(1), 1.0, Some(1)),
        p(11, 6, Some(10), 1.0, Some(1)),
        p(40, 5, Some(41), 0.0, Some(1)),
        p(41, 5, Some(40), 0.0, Some(1)),
    ];
    let mut b = a.clone();
    b.reverse();
    let ja = serde_json::to_string(&group(&a)).unwrap();
    let jb = serde_json::to_string(&group(&b)).unwrap();
    assert_eq!(ja, jb);
}

#[test]
fn deep_chain_is_bounded() {
    let mut v = vec![p(100, 1, Some(1), 0.0, Some(1))];
    for i in 1..200u32 {
        v.push(p(100 + i, 1 + i as u64, Some(99 + i), 0.0, Some(1)));
    }
    let g = group(&v);
    assert!(!g.notes.is_empty());
    let total: usize = g.groups.iter().map(|x| x.members.len()).sum();
    assert_eq!(total, 200);
}
