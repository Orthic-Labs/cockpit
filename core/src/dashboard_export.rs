//! Versioned, typed read-only payload consumed by Pulse dashboard.
//!
//! The CLI keeps the persisted [`Snapshot`] intact, then adds bounded module
//! projections.  Module fields are concrete Rust types so an export cannot
//! silently turn an unavailable reading into an opaque JSON placeholder.

use crate::{
    activity::{ActivityEvent, ActivityTotals, DurableActivityLedger},
    duplicates::DuplicateReport,
    folder_growth::FolderGrowthComparison,
    monitor::ExtendedStatus,
    storage_browser::{DateAvailability, StorageFolder, StorageItem},
    store::Snapshot,
};
use serde::Serialize;
use std::path::PathBuf;

pub const DASHBOARD_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize)]
pub struct DashboardExport {
    pub schema_version: u32,
    pub snapshot: Snapshot,
    pub modules: DashboardModules,
}

#[derive(Clone, Debug, Serialize)]
pub struct DashboardModules {
    pub storage: StorageProjection,
    pub monitor: ExtendedStatus,
    pub history: HistoryProjection,
    pub activity: ActivityProjection,
    pub folder_growth: Option<FolderGrowthComparison>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplicates: Option<DuplicateReport>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ActivityProjection {
    pub events: Vec<ActivityEvent>,
    pub weekly: ActivityTotals,
    pub monthly: ActivityTotals,
}

impl ActivityProjection {
    pub fn empty(timestamp: u64) -> Self {
        Self {
            events: Vec::new(),
            weekly: ActivityTotals {
                window: crate::activity::week_window_utc(timestamp),
                ..ActivityTotals::default()
            },
            monthly: ActivityTotals {
                window: crate::activity::month_window_utc(timestamp),
                ..ActivityTotals::default()
            },
        }
    }

    pub fn from_ledger(ledger: &DurableActivityLedger, timestamp: u64) -> Self {
        Self {
            events: ledger.events().cloned().collect(),
            weekly: ledger.weekly(timestamp),
            monthly: ledger.monthly(timestamp),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct HistoryProjection {
    pub snapshots: Vec<SnapshotSummary>,
    pub skipped: Vec<HistorySkip>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SnapshotSummary {
    pub id: String,
    pub created_at: u64,
    pub roots: Vec<PathBuf>,
    pub logical_bytes: u64,
    pub attributed_allocation_bytes: u64,
    pub incomplete: bool,
    pub findings_count: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct HistorySkip {
    pub file: String,
    pub reason: String,
}

impl SnapshotSummary {
    pub fn from_snapshot(snapshot: &Snapshot) -> Self {
        Self {
            id: snapshot.id.clone(),
            created_at: snapshot.created_at,
            roots: snapshot.report.roots.clone(),
            logical_bytes: snapshot.report.accounting.logical_bytes,
            attributed_allocation_bytes: snapshot.report.accounting.attributed_allocation_bytes,
            incomplete: snapshot.report.accounting.incomplete,
            findings_count: snapshot.findings.len(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct StorageProjection {
    pub largest_files: Vec<StorageItem>,
    pub largest_folders: Vec<StorageFolder>,
    pub incomplete: bool,
    pub dates: DateAvailability,
}

impl DashboardExport {
    pub fn from_snapshot(
        snapshot: Snapshot,
        history: Vec<SnapshotSummary>,
        skipped: Vec<HistorySkip>,
        activity: ActivityProjection,
        folder_growth: Option<FolderGrowthComparison>,
        monitor: ExtendedStatus,
    ) -> Self {
        let incomplete = snapshot.report.accounting.incomplete
            || !snapshot.report.incomplete_reasons.is_empty()
            || !snapshot.report.inspection_errors.is_empty()
            || !snapshot.report.skipped_links.is_empty();
        let largest_files =
            crate::storage_browser::largest_files(&snapshot.report, 100).unwrap_or_default();
        let largest_folders =
            crate::storage_browser::largest_folders(&snapshot.report, 100).unwrap_or_default();
        Self {
            schema_version: DASHBOARD_SCHEMA_VERSION,
            snapshot,
            modules: DashboardModules {
                storage: StorageProjection {
                    largest_files,
                    largest_folders,
                    incomplete,
                    dates: DateAvailability::default(),
                },
                monitor,
                history: HistoryProjection {
                    snapshots: history,
                    skipped,
                },
                activity,
                folder_growth,
                duplicates: None,
            },
        }
    }
}
