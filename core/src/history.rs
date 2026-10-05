//! Read-only comparison between two stored scan snapshots.
//!
//! The result is bounded to observed scan bytes: growth figures describe the
//! difference between two complete reports over the same roots and volumes.
//! They are not claims about bytes that could be reclaimed, and volume
//! identities observed here never authorize mutations.
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
/// completeness, selected roots, or observed volume identities, or when
/// volume identity data is missing or empty.  A complete report in the same
/// scope yields signed growth figures; `i128` is used so shrinking usage
/// cannot underflow.
pub fn compare(previous: &Snapshot, current: &Snapshot) -> Comparison {
    let mut reasons = Vec::new();

    if previous.schema_version != current.schema_version {
        reasons.push(format!(
            "snapshot schemas differ ({} vs {})",
            previous.schema_version, current.schema_version
        ));
    }
    if previous.report.accounting.incomplete {
        reasons.push("previous snapshot accounting is incomplete".to_string());
    }
    if current.report.accounting.incomplete {
        reasons.push("current snapshot accounting is incomplete".to_string());
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
        if previous_volumes.iter().chain(&current_volumes).any(|id| id.trim().is_empty() || id.eq_ignore_ascii_case("unknown")) {
            reasons.push("empty or unknown volume identity".to_string());
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
