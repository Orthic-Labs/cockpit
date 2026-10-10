//! Read-only comparison between two stored scan snapshots.
//!
//! The result is bounded to observed scan bytes: growth figures describe the
//! difference between two reports without scan-wide gaps over the same roots
//! and volumes.
//! They are not claims about bytes that could be reclaimed, and volume
//! identities observed here never authorize mutations.
use crate::scan;
use crate::store::Snapshot;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize)]
pub struct Comparison {
    pub comparable: bool,
    /// Attributed allocation growth; positive means usage increased.
    /// Present only when `comparable` is true.
    pub attributed_growth_bytes: Option<i128>,
    /// Logical byte growth; positive means usage increased.
    /// Present only when `comparable` is true.
    pub logical_growth_bytes: Option<i128>,
    /// Deterministic reasons the snapshots could not be compared.
    /// Empty when `comparable` is true.
    pub reasons: Vec<String>,
}

fn root_set(snapshot: &Snapshot) -> BTreeSet<PathBuf> {
    snapshot.report.roots.iter().cloned().collect()
}

fn volume_ids(snapshot: &Snapshot) -> BTreeSet<String> {
    snapshot
        .report
        .volume_usage
        .iter()
        .map(|usage| usage.volume.id.clone())
        .collect()
}

/// Compares two snapshots without touching the filesystem or network.
///
/// Returns a non-comparable result when the snapshots differ in schema,
/// selected roots, or observed volume identities, when either has a scan-wide
/// gap, or when volume identity data is missing or empty.  Reports without scan-wide gaps in
/// the same scope yield signed growth figures; `i128` is used so shrinking
/// usage cannot underflow.
pub fn compare(previous: &Snapshot, current: &Snapshot) -> Comparison {
    let mut reasons = Vec::new();

    if previous.schema_version != current.schema_version {
        reasons.push(format!(
            "snapshot schemas differ ({} vs {})",
            previous.schema_version, current.schema_version
        ));
    }
    // Only a material gap that taints the whole scan (a limit hit, a cancelled
    // scan, an unreadable volume, an unexplained incomplete flag) makes a
    // snapshot incomplete for comparison; see `scan::scan_gaps`. Benign gaps
    // (an entry that vanished during the walk, links or placeholders not
    // followed, a file whose metadata could not be completed) do not, and a
    // gap confined to some folders (access denied, a depth limit) only makes
    // those folders non-comparable in `folder_growth`. Cleanup eligibility
    // keeps using the `accounting.incomplete` flag itself.
    for (label, snapshot) in [("previous", previous), ("current", current)] {
        let gaps = scan::scan_gaps(&snapshot.report);
        if gaps.whole_scan > 0 {
            reasons.push(format!(
                "{label} snapshot accounting is incomplete ({} scan-wide gap(s), first: {})",
                gaps.whole_scan,
                gaps.first_whole_scan.as_deref().unwrap_or("unspecified"),
            ));
        }
    }
    if root_set(previous) != root_set(current) {
        reasons.push("selected roots differ".to_string());
    }

    let previous_volumes = volume_ids(previous);
    let current_volumes = volume_ids(current);
    if previous_volumes.is_empty() && current_volumes.is_empty() {
        reasons.push("no observed volumes in either snapshot".to_string());
    } else {
        if previous_volumes != current_volumes {
            reasons.push("observed volume identities differ".to_string());
        }
        if previous_volumes
            .iter()
            .chain(&current_volumes)
            .any(|id| id.trim().is_empty() || id.eq_ignore_ascii_case("unknown"))
        {
            reasons.push("empty volume identity or unknown identity".to_string());
        }
    }

    if !reasons.is_empty() {
        return Comparison {
            comparable: false,
            attributed_growth_bytes: None,
            logical_growth_bytes: None,
            reasons,
        };
    }

    let attributed = i128::from(current.report.accounting.attributed_allocation_bytes)
        - i128::from(previous.report.accounting.attributed_allocation_bytes);
    let logical = i128::from(current.report.accounting.logical_bytes)
        - i128::from(previous.report.accounting.logical_bytes);

    Comparison {
        comparable: true,
        attributed_growth_bytes: Some(attributed),
        logical_growth_bytes: Some(logical),
        reasons: Vec::new(),
    }
}
