//! Public data model for the read-only M1 core.
//!
//! The model deliberately keeps logical bytes, attributed allocation, reclaim
//! bounds, and observed volume changes as separate values.  A caller can show
//! an estimate without turning an unknown sharing relationship into a claim.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct VolumeIdentity {
    /// Provider identity (APFS UUID, NTFS serial, or fixture id).
    /// Standard provider uses ephemeral device/mount identity; never use it to bind mutations.
    pub id: String,
}

impl VolumeIdentity {
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into() }
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct FileIdentity {
    pub volume: VolumeIdentity,
    /// Provider-scoped inode/file id.  It is a string because Windows and
    /// platform fixtures do not share an integer identity shape.
    pub id: String,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct CloneIdentity {
    pub volume: VolumeIdentity,
    pub id: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileMetadata {
    pub kind: EntryKind,
    pub volume: VolumeIdentity,
    pub logical_size: Option<u64>,
    pub allocation_size: Option<u64>,
    pub file_id: Option<FileIdentity>,
    pub clone_id: Option<CloneIdentity>,
    /// True means the item is a cloud/dataless placeholder and must not be
    /// hydrated as part of inspection.
    pub is_placeholder: bool,
    /// False means one or more fields are unavailable; it is retained in the
    /// report rather than filled with a guessed size.
    pub metadata_complete: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VolumeUsage {
    pub volume: VolumeIdentity,
    pub total_bytes: Option<u64>,
    pub used_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
    pub purgeable_bytes: Option<u64>,
    pub snapshots: SnapshotState,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub enum SnapshotState {
    #[default]
    Unknown,
    NoneKnown,
    Present {
        count: u64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VolumeDelta {
    pub volume: VolumeIdentity,
    pub before_used_bytes: Option<u64>,
    pub after_used_bytes: Option<u64>,
    /// Positive means used space increased; negative means it decreased.
    pub delta_bytes: Option<i128>,
}

impl VolumeDelta {
    pub fn new(volume: VolumeIdentity, before: Option<u64>, after: Option<u64>) -> Self {
        let delta_bytes = before
            .zip(after)
            .map(|(b, a)| i128::from(a) - i128::from(b));
        Self {
            volume,
            before_used_bytes: before,
            after_used_bytes: after,
            delta_bytes,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ReclaimState {
    Bounded,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReclaimEstimate {
    pub lower_bytes: u64,
    pub upper_bytes: Option<u64>,
    pub state: ReclaimState,
    pub reasons: Vec<String>,
}

impl ReclaimEstimate {
    pub fn unknown(upper: Option<u64>, reason: impl Into<String>) -> Self {
        Self {
            lower_bytes: 0,
            upper_bytes: upper,
            state: ReclaimState::Unknown,
            reasons: vec![reason.into()],
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Accounting {
    pub logical_bytes: u64,
    pub attributed_allocation_bytes: u64,
    pub reclaim: ReclaimEstimateSummary,
    /// Scanned attributed allocation minus provider-reported volume usage.
    pub signed_discrepancy_bytes: Option<i128>,
    pub volume_discrepancies: Vec<VolumeDiscrepancy>,
    pub incomplete: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ReclaimEstimateSummary {
    pub lower_bytes: u64,
    pub upper_bytes: Option<u64>,
    pub state: Option<ReclaimState>,
    pub reasons: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VolumeDiscrepancy {
    pub volume: VolumeIdentity,
    pub scanned_minus_used_bytes: i128,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScannedEntry {
    pub path: PathBuf,
    pub metadata: FileMetadata,
    pub logical_bytes: u64,
    pub attributed_allocation_bytes: u64,
    pub accounting_owner: Option<PathBuf>,
    pub reclaim: Option<ReclaimEstimate>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InspectionError {
    pub path: PathBuf,
    pub operation: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkippedLink {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScanOptions {
    pub max_depth: usize,
    pub max_entries: usize,
    pub reject_placeholders: bool,
    /// Optional readings taken by the caller before/after this read-only scan.
    pub volume_deltas: Vec<VolumeDelta>,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            max_depth: 64,
            max_entries: 100_000,
            reject_placeholders: true,
            volume_deltas: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FolderAccounting {
    pub path: PathBuf,
    pub volume: VolumeIdentity,
    pub logical_bytes: u64,
    pub attributed_allocation_bytes: u64,
    pub incomplete: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScanReport {
    pub roots: Vec<PathBuf>,
    pub entries: Vec<ScannedEntry>,
    #[serde(default)]
    pub folders: Vec<FolderAccounting>,
    pub accounting: Accounting,
    pub volume_usage: Vec<VolumeUsage>,
    pub volume_deltas: Vec<VolumeDelta>,
    pub inspection_errors: Vec<InspectionError>,
    pub skipped_links: Vec<SkippedLink>,
    pub incomplete_reasons: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FsError {
    pub message: String,
    pub permission_denied: bool,
}

impl FsError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            permission_denied: false,
        }
    }
    pub fn permission_denied(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            permission_denied: true,
        }
    }
}

impl fmt::Display for FsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for FsError {}
