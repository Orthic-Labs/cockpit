//! Pure, evidence-based process grouping. No OS calls, no signals.
//!
//! A child joins its parent's group only when the parent PID resolves to
//! exactly one live incarnation in the snapshot and that incarnation started
//! no later than the child (otherwise the PID was reused). When the parent
//! PID matches multiple live incarnations the ancestry is ambiguous and the
//! child stays as its own group with an explicit note. Duplicate PIDs are
//! never deduplicated: every distinct `(pid, start_time)` is preserved.
//! Names are never used to group.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{Capability, Metric, ProcessIdentity, ProcessInfo};

const MAX_DEPTH: usize = 64;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessGroup {
    pub root: ProcessIdentity,
    pub name: String,
    pub members: Vec<ProcessIdentity>,
    pub cpu_usage_percent: f32,
    pub memory: Metric<u64>,
    pub evidence: GroupEvidence,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupEvidence {
    VerifiedParentChain,
    AmbiguousParentChain,
    SingleProcess,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessGroups {
    pub groups: Vec<ProcessGroup>,
    pub notes: Vec<String>,
}

fn id_key(id: &ProcessIdentity) -> (u32, u64) {
    (id.pid, id.start_time)
}

/// Group processes by verified parent chain. Input rows are not modified.
pub fn group(processes: &[ProcessInfo]) -> ProcessGroups {
    let mut notes: BTreeSet<String> = BTreeSet::new();

    // Every distinct identity is retained; index pid -> all incarnations.
    let mut by_pid: BTreeMap<u32, Vec<&ProcessInfo>> = BTreeMap::new();
    let mut by_id: BTreeMap<(u32, u64), &ProcessInfo> = BTreeMap::new();
    for p in processes {
        by_pid.entry(p.identity.pid).or_default().push(p);
        if by_id.insert(id_key(&p.identity), p).is_some() {
            notes.insert(format!(
                "identical identity pid {} start {} appears more than once in snapshot",
                p.identity.pid, p.identity.start_time
            ));
        }
    }
    for (pid, v) in &mut by_pid {
        v.sort_by_key(|p| p.identity.start_time);
        if v.len() > 1 {
            notes.insert(format!(
                "pid {pid} has {} live incarnations in snapshot; all retained",
                v.len()
            ));
        }
    }

    // Resolve a process's parent_pid to at most one identity.
    enum Parent {
        Verified(ProcessIdentity),
        Root,
    }

    let verified_parent = |p: &ProcessInfo, notes: &mut BTreeSet<String>| -> Parent {
        let Some(parent_pid) = p.parent_pid else {
            return Parent::Root;
        };
        if parent_pid == 0 || parent_pid == 1 || p.identity.pid <= 1 {
            return Parent::Root;
        }
        let Some(candidates) = by_pid.get(&parent_pid) else {
            notes.insert(format!(
                "pid {}: parent pid {} missing from snapshot; rooting own group",
                p.identity.pid, parent_pid
            ));
            return Parent::Root;
        };
        if candidates.len() != 1 {
            notes.insert(format!(
                "pid {}: parent pid {} is ambiguous ({} snapshot rows); rooting own group",
                p.identity.pid, parent_pid, candidates.len()
            ));
            return Parent::Root;
        }
        // Incarnations that could be the parent must not have started after
        // the child.
        let plausible: Vec<&&ProcessInfo> = candidates
            .iter()
            .filter(|c| c.identity.start_time <= p.identity.start_time)
            .collect();
        match plausible.len() {
            1 => Parent::Verified(plausible[0].identity.clone()),
            0 => {
                notes.insert(format!(
                    "pid {}: parent pid {} started later (reused); rooting own group",
                    p.identity.pid, parent_pid
                ));
                Parent::Root
            }
            n => {
                notes.insert(format!(
                    "pid {}: parent pid {} is ambiguous ({} live incarnations match); \
                     not attaching to a guessed incarnation",
                    p.identity.pid, parent_pid, n
                ));
                Parent::Root
            }
        }
    };

    let mut members_of: BTreeMap<(u32, u64), Vec<&ProcessInfo>> = BTreeMap::new();
    let mut ambiguous_roots = BTreeSet::new();
    for (&key, &p) in &by_id {
        let mut path: Vec<ProcessIdentity> = vec![p.identity.clone()];
        let mut current = p;
        let root = loop {
            match verified_parent(current, &mut notes) {
                Parent::Root => break id_key(&current.identity),
                Parent::Verified(next) => {
                    if let Some(pos) =
                        path.iter().position(|x| x.pid == next.pid && x.start_time == next.start_time)
                    {
                        // Deterministic break point inside the cycle.
                        let root_key =
                            path[pos..].iter().map(id_key).min().unwrap_or(id_key(&next));
                        ambiguous_roots.insert(root_key);
                        let involved: Vec<String> = path[pos..]
                            .iter()
                            .map(|i| format!("{}@{}", i.pid, i.start_time))
                            .collect();
                        notes.insert(format!(
                            "parent cycle detected involving [{}]; broken at pid {} start {}",
                            involved.join(", "),
                            root_key.0,
                            root_key.1
                        ));
                        break root_key;
                    }
                    if path.len() >= MAX_DEPTH {
                        notes.insert(format!(
                            "parent chain from pid {} exceeds depth {MAX_DEPTH}; rooted at pid {} start {}",
                            p.identity.pid,
                            current.identity.pid,
                            current.identity.start_time
                        ));
                        break id_key(&current.identity);
                    }
                    path.push(next.clone());
                    current = by_id[&id_key(&next)];
                }
            }
        };
        members_of.entry(root).or_default().push(p);
    }

    let mut groups: Vec<ProcessGroup> = members_of
        .into_iter()
        .map(|(root_key, mut members)| {
            members.sort_by_key(|m| (m.identity.pid, m.identity.start_time));
            let root_info = by_id[&root_key];
            let cpu: f32 = members.iter().map(|m| m.cpu_usage_percent).sum();
            let known: Vec<u64> = members.iter().filter_map(|m| m.memory.value).collect();
            let memory = if known.len() == members.len() {
                Metric {
                    value: Some(known.iter().fold(0u64, |a, b| a.saturating_add(*b))),
                    capability: Capability::Available,
                    label: if cfg!(windows) {
                        "working-set sum; private bytes unavailable"
                    } else {
                        "resident memory (RSS) sum; physical footprint unavailable"
                    }.into(),
                }
            } else {
                Metric {
                    value: None,
                    capability: Capability::Unavailable,
                    label: format!(
                        "memory sum unavailable: {} of {} members lack a resident memory reading",
                        members.len() - known.len(),
                        members.len()
                    ),
                }
            };
            ProcessGroup {
                root: root_info.identity.clone(),
                name: root_info.name.clone(),
                evidence: if ambiguous_roots.contains(&root_key) {
                    GroupEvidence::AmbiguousParentChain
                } else if members.len() > 1 {
                    GroupEvidence::VerifiedParentChain
                } else {
                    GroupEvidence::SingleProcess
                },
                members: members.iter().map(|m| m.identity.clone()).collect(),
                cpu_usage_percent: cpu,
                memory,
            }
        })
        .collect();
    groups.sort_by_key(|g| (g.root.pid, g.root.start_time));

    ProcessGroups {
        groups,
        notes: notes.into_iter().collect(),
    }
}
