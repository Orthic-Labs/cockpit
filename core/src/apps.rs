//! Read-only application inventory, startup, update, and process-history models.
//!
//! This module is deliberately a projector over caller-supplied evidence.  It
//! does not enumerate applications, read account state, contact update feeds,
//! or change startup state.  A missing source is represented as incomplete
//! coverage, rather than being turned into an empty installed set.

use crate::{FileIdentity, Metric, ProcessIdentity, ScanReport};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Inventory evidence and ownership
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct AppIdentity {
    pub bundle_id: Option<String>,
    pub registered_id: Option<String>,
    pub name: String,
    pub team_id: Option<String>,
}

impl AppIdentity {
    pub fn stable_key(&self) -> Option<String> {
        self.bundle_id
            .as_ref()
            .filter(|v| !v.trim().is_empty())
            .cloned()
            .or_else(|| {
                self.registered_id
                    .as_ref()
                    .filter(|v| !v.trim().is_empty())
                    .cloned()
            })
    }

    pub fn has_valid_bundle_id(&self) -> bool {
        self.bundle_id
            .as_deref()
            .map(valid_bundle_id)
            .unwrap_or(false)
    }
}

pub fn valid_bundle_id(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        })
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum InventorySource {
    Bundle,
    RegisteredUninstaller,
    StorePackage,
    Portable,
    ExternalVolume,
    Other(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum CoverageState {
    Complete,
    Partial,
    Unavailable,
    Unknown,
}

impl CoverageState {
    fn complete(self) -> bool {
        matches!(self, Self::Complete)
    }
}

/// Coverage is supplied by a platform adapter.  `Complete` means the adapter
/// checked that source; it does not mean that an application was found.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InventoryCoverage {
    pub bundle_source: CoverageState,
    pub registered_source: CoverageState,
    pub portable_apps: CoverageState,
    pub external_volumes: CoverageState,
    pub mounted_volume_ids: Vec<String>,
    pub indexing_enabled: Option<bool>,
}

impl Default for InventoryCoverage {
    fn default() -> Self {
        Self {
            bundle_source: CoverageState::Unknown,
            registered_source: CoverageState::Unknown,
            portable_apps: CoverageState::Unknown,
            external_volumes: CoverageState::Unknown,
            mounted_volume_ids: Vec::new(),
            indexing_enabled: None,
        }
    }
}

impl InventoryCoverage {
    /// Leftover claims require every relevant discovery boundary to have
    /// answered.  External-volume and portable-app gaps are safety blockers.
    pub fn complete_for_leftovers(&self) -> bool {
        self.bundle_source.complete()
            && self.registered_source.complete()
            && self.portable_apps.complete()
            && self.external_volumes.complete()
            && self.indexing_enabled != Some(false)
    }

    pub fn gap_reasons(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (label, state) in [
            ("bundle source", self.bundle_source),
            ("registered source", self.registered_source),
            ("portable app coverage", self.portable_apps),
            ("external-volume coverage", self.external_volumes),
        ] {
            if !state.complete() {
                out.push(format!("{label} is {:?}", state));
            }
        }
        if self.indexing_enabled == Some(false) {
            out.push("application indexing is disabled".into());
        }
        out
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RelatedDataKind {
    ApplicationBundle,
    ApplicationSupport,
    Caches,
    Preferences,
    Container,
    GroupContainer,
    Logs,
    LaunchItem,
    UserData,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum OwnershipConfidence {
    ExactBundleId,
    ExactRegisteredId,
    TeamIdVendor,
    NameMatch,
    Ambiguous,
    Unknown,
}

impl OwnershipConfidence {
    pub fn is_exact(self) -> bool {
        matches!(self, Self::ExactBundleId | Self::ExactRegisteredId)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum OwnershipDisposition {
    Preselected,
    Review,
    Excluded,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RelatedPathRecord {
    pub path: PathBuf,
    pub kind: RelatedDataKind,
    pub ownership: OwnershipConfidence,
    pub shared: bool,
    pub logical_bytes: Option<u64>,
    pub attributed_allocation_bytes: Option<u64>,
    pub file_id: Option<FileIdentity>,
}

impl RelatedPathRecord {
    pub fn disposition(&self) -> OwnershipDisposition {
        if self.shared || matches!(self.kind, RelatedDataKind::UserData) {
            return OwnershipDisposition::Excluded;
        }
        // Group Containers and vendor folders remain review items even when a
        // platform adapter has a plausible owner.
        if self.ownership.is_exact()
            && matches!(
                self.kind,
                RelatedDataKind::ApplicationBundle
                    | RelatedDataKind::Caches
                    | RelatedDataKind::Container
                    | RelatedDataKind::Preferences
                    | RelatedDataKind::ApplicationSupport
                    | RelatedDataKind::Logs
                    | RelatedDataKind::LaunchItem
            )
        {
            OwnershipDisposition::Preselected
        } else if matches!(
            self.ownership,
            OwnershipConfidence::Ambiguous | OwnershipConfidence::Unknown
        ) {
            OwnershipDisposition::Excluded
        } else {
            OwnershipDisposition::Review
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ByteTotals {
    pub logical_bytes: u64,
    pub attributed_allocation_bytes: u64,
    pub overflowed: bool,
    pub incomplete: bool,
}

impl ByteTotals {
    pub const UNIT: &'static str = "bytes";

    pub fn add_bytes(&mut self, logical: u64, allocation: u64) {
        let (logical, logical_overflow) = self.logical_bytes.overflowing_add(logical);
        let (allocation, allocation_overflow) =
            self.attributed_allocation_bytes.overflowing_add(allocation);
        self.logical_bytes = if logical_overflow { u64::MAX } else { logical };
        self.attributed_allocation_bytes = if allocation_overflow {
            u64::MAX
        } else {
            allocation
        };
        self.overflowed |= logical_overflow || allocation_overflow;
    }

    pub fn from_pairs<I>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (u64, u64)>,
    {
        let mut totals = Self::default();
        for (logical, allocation) in pairs {
            totals.add_bytes(logical, allocation);
        }
        totals
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppInventoryRecord {
    pub identity: AppIdentity,
    pub root: PathBuf,
    pub volume_id: Option<String>,
    pub source: InventorySource,
    pub version: Option<String>,
    pub related_paths: Vec<RelatedPathRecord>,
}

pub type InventoryRecord = AppInventoryRecord;

impl AppInventoryRecord {
    pub fn new(identity: AppIdentity, root: impl Into<PathBuf>, source: InventorySource) -> Self {
        Self {
            identity,
            root: root.into(),
            volume_id: None,
            source,
            version: None,
            related_paths: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectedApp {
    pub identity: AppIdentity,
    pub root: PathBuf,
    pub volume_id: Option<String>,
    pub source: InventorySource,
    pub version: Option<String>,
    pub install_state: InstallState,
    pub related_paths: Vec<RelatedPathRecord>,
    pub related_totals: ByteTotals,
    pub leftover_eligibility: LeftoverEligibility,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum InstallState {
    Installed,
    ConfirmedGone,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum LeftoverEligibility {
    NotApplicable,
    Eligible,
    Ineligible { reasons: Vec<String> },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppInventoryReport {
    pub apps: Vec<ProjectedApp>,
    pub coverage: InventoryCoverage,
    pub incomplete_reasons: Vec<String>,
}

pub type AppFootprint = AppInventoryReport;

fn path_is_within(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

fn report_entry_bytes(
    report: &ScanReport,
    path: &Path,
) -> Option<(u64, u64, Option<FileIdentity>)> {
    report
        .entries
        .iter()
        .find(|entry| entry.path == path)
        .map(|entry| {
            (
                entry.logical_bytes,
                entry.attributed_allocation_bytes,
                entry.metadata.file_id.clone(),
            )
        })
}

fn enrich(record: &mut RelatedPathRecord, report: &ScanReport) {
    if let Some((logical, allocation, file_id)) = report_entry_bytes(report, &record.path) {
        record.logical_bytes = record.logical_bytes.or(Some(logical));
        record.attributed_allocation_bytes =
            record.attributed_allocation_bytes.or(Some(allocation));
        record.file_id = record.file_id.clone().or(file_id);
    }
}

fn totals_without_overlap(paths: &[RelatedPathRecord]) -> ByteTotals {
    let mut out = ByteTotals::default();
    let mut seen_ids = BTreeSet::new();
    let mut seen_paths: Vec<(PathBuf, bool)> = Vec::new();
    let mut ordered: Vec<&RelatedPathRecord> = paths.iter().collect();
    ordered.sort_by_key(|path| path.path.components().count());
    for path in ordered {
        // An ancestor record already represents its descendants.  This also
        // protects callers that supplied both a directory and its files.
        if seen_paths
            .iter()
            .any(|(ancestor, covers)| *covers && path_is_within(&path.path, ancestor))
        {
            continue;
        }
        if let Some(id) = &path.file_id {
            if !seen_ids.insert(id.clone()) {
                continue;
            }
        } else if seen_paths.iter().any(|(known, _)| known == &path.path) {
            continue;
        }
        let covers_descendants = path.logical_bytes.unwrap_or(0) > 0
            || path.attributed_allocation_bytes.unwrap_or(0) > 0;
        seen_paths.push((path.path.clone(), covers_descendants));
        match (path.logical_bytes, path.attributed_allocation_bytes) {
            (Some(logical), Some(allocation)) => out.add_bytes(logical, allocation),
            _ => out.incomplete = true,
        }
    }
    out
}

/// Project supplied records and a supplied scan into serializable inventory.
/// No installed app is inferred from scan paths or from an absent record.
pub fn project_inventory(
    records: &[AppInventoryRecord],
    report: &ScanReport,
    coverage: InventoryCoverage,
) -> AppInventoryReport {
    let mut incomplete_reasons = coverage.gap_reasons();
    if report.accounting.incomplete {
        incomplete_reasons.push("scan accounting is incomplete".into());
    }
    let mut apps = Vec::with_capacity(records.len());
    for source in records {
        let mut related = source.related_paths.clone();
        for entry in &report.entries {
            if path_is_within(&entry.path, &source.root)
                && !related.iter().any(|path| path.path == entry.path)
            {
                related.push(RelatedPathRecord {
                    path: entry.path.clone(),
                    kind: RelatedDataKind::ApplicationBundle,
                    ownership: if source.identity.bundle_id.is_some() {
                        OwnershipConfidence::ExactBundleId
                    } else if source.identity.registered_id.is_some() {
                        OwnershipConfidence::ExactRegisteredId
                    } else {
                        OwnershipConfidence::Unknown
                    },
                    shared: false,
                    logical_bytes: Some(entry.logical_bytes),
                    attributed_allocation_bytes: Some(entry.attributed_allocation_bytes),
                    file_id: entry.metadata.file_id.clone(),
                });
            }
        }
        for item in &mut related {
            enrich(item, report);
        }
        let related_totals = totals_without_overlap(&related);
        if related_totals.incomplete {
            incomplete_reasons.push(format!(
                "related bytes unavailable for {}",
                source.root.display()
            ));
        }
        apps.push(ProjectedApp {
            identity: source.identity.clone(),
            root: source.root.clone(),
            volume_id: source.volume_id.clone(),
            source: source.source.clone(),
            version: source.version.clone(),
            install_state: InstallState::Installed,
            related_paths: related,
            related_totals,
            leftover_eligibility: LeftoverEligibility::NotApplicable,
        });
    }
    AppInventoryReport {
        apps,
        coverage,
        incomplete_reasons,
    }
}

pub fn project_app_footprint(
    records: &[AppInventoryRecord],
    report: &ScanReport,
    coverage: InventoryCoverage,
) -> AppFootprint {
    project_inventory(records, report, coverage)
}

// ---------------------------------------------------------------------------
// Positive install history and disappearance
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstalledHistoryRecord {
    pub identity: AppIdentity,
    pub root: PathBuf,
    pub first_seen_at: u64,
    pub last_seen_at: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct InstalledAppHistory {
    pub records: BTreeMap<String, InstalledHistoryRecord>,
}

impl InstalledAppHistory {
    pub fn observe(&mut self, records: &[AppInventoryRecord], now: u64) {
        for app in records {
            let Some(key) = app.identity.stable_key() else {
                continue;
            };
            match self.records.get_mut(&key) {
                Some(existing) => {
                    existing.last_seen_at = now;
                    existing.identity = app.identity.clone();
                    existing.root = app.root.clone();
                }
                None => {
                    self.records.insert(
                        key,
                        InstalledHistoryRecord {
                            identity: app.identity.clone(),
                            root: app.root.clone(),
                            first_seen_at: now,
                            last_seen_at: now,
                        },
                    );
                }
            }
        }
    }

    pub fn state(
        &self,
        identity: &AppIdentity,
        currently_present: bool,
        coverage: &InventoryCoverage,
    ) -> InstallState {
        if currently_present {
            InstallState::Installed
        } else if coverage.complete_for_leftovers() {
            if let Some(key) = identity.stable_key() {
                if self.records.contains_key(&key) {
                    return InstallState::ConfirmedGone;
                }
            }
            InstallState::Unknown
        } else {
            InstallState::Unknown
        }
    }
}

/// Apply historical disappearance states to a current report.  This function
/// still requires caller-supplied historical identity; it never invents one
/// from names, paths, or scan entries.
pub fn project_inventory_with_history(
    records: &[AppInventoryRecord],
    report: &ScanReport,
    coverage: InventoryCoverage,
    history: &InstalledAppHistory,
) -> AppInventoryReport {
    let mut projected = project_inventory(records, report, coverage.clone());
    for app in &mut projected.apps {
        app.install_state = history.state(&app.identity, true, &coverage);
    }
    projected
}

/// Project a disappeared app from positive history.  It is review-only until
/// a separate, complete process-liveness check says that no process remains.
pub fn project_disappeared(
    history: &InstalledAppHistory,
    identity_key: &str,
    coverage: InventoryCoverage,
) -> Option<ProjectedApp> {
    let record = history.records.get(identity_key)?;
    let mut reasons = coverage.gap_reasons();
    reasons.push("process liveness was not supplied".into());
    Some(ProjectedApp {
        identity: record.identity.clone(),
        root: record.root.clone(),
        volume_id: None,
        source: InventorySource::Other("history".into()),
        version: None,
        install_state: if coverage.complete_for_leftovers() {
            InstallState::ConfirmedGone
        } else {
            InstallState::Unknown
        },
        related_paths: Vec::new(),
        related_totals: ByteTotals::default(),
        leftover_eligibility: LeftoverEligibility::Ineligible { reasons },
    })
}

pub fn assess_leftover_eligibility(
    state: InstallState,
    coverage: &InventoryCoverage,
    process_running: Option<bool>,
) -> LeftoverEligibility {
    let mut reasons = coverage.gap_reasons();
    if state != InstallState::ConfirmedGone {
        reasons.push("app disappearance is not confirmed by positive install history".into());
    }
    match process_running {
        Some(true) => reasons.push("app process is still running".into()),
        None => reasons.push("process liveness is unknown".into()),
        Some(false) => {}
    }
    if coverage.complete_for_leftovers()
        && state == InstallState::ConfirmedGone
        && process_running == Some(false)
    {
        LeftoverEligibility::Eligible
    } else {
        LeftoverEligibility::Ineligible { reasons }
    }
}

pub fn project_disappeared_with_liveness(
    history: &InstalledAppHistory,
    identity_key: &str,
    coverage: InventoryCoverage,
    process_running: Option<bool>,
) -> Option<ProjectedApp> {
    let mut app = project_disappeared(history, identity_key, coverage.clone())?;
    app.leftover_eligibility =
        assess_leftover_eligibility(app.install_state, &coverage, process_running);
    Some(app)
}

// ---------------------------------------------------------------------------
// Typed startup entries
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum StartupKind {
    LoginItem,
    LaunchAgent,
    LaunchDaemon,
    ScheduledTask,
    Service,
    RegistryRun,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum StartupScope {
    User,
    Machine,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StartupEntry {
    pub id: String,
    pub kind: StartupKind,
    pub scope: StartupScope,
    pub path: Option<PathBuf>,
    pub app_key: Option<String>,
    pub enabled: Option<bool>,
    pub source: String,
}

pub type StartupRecord = StartupEntry;

/// Preserve supplied startup evidence in deterministic order.  No entry is
/// enabled, disabled, removed, or inferred by this helper.
pub fn normalize_startup_entries(mut entries: Vec<StartupEntry>) -> Vec<StartupEntry> {
    entries.sort_by(|a, b| a.id.cmp(&b.id).then_with(|| a.source.cmp(&b.source)));
    entries
}

// ---------------------------------------------------------------------------
// Update-feed normalization (network-free)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SuppliedUpdateRecord {
    pub app_key: String,
    pub current_version: String,
    pub candidate_version: Option<String>,
    pub feed_url: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum VersionComparison {
    Newer,
    NotNewer,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum UpdateFeedStatus {
    UpdateAvailable,
    UpToDate,
    UnknownVersion,
    InvalidUrl,
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NormalizedUpdate {
    pub app_key: String,
    pub feed_url: String,
    pub candidate_version: Option<String>,
    pub comparison: VersionComparison,
    pub status: UpdateFeedStatus,
    pub network_performed: bool,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct UpdateFeedReport {
    pub entries: Vec<NormalizedUpdate>,
    pub network_performed: bool,
}

fn valid_http_url(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https")
        || rest.is_empty()
        || rest
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
    {
        return false;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    !authority.is_empty() && !authority.starts_with(':') && !authority.contains('@')
}

fn parse_version(value: &str) -> Option<(Vec<u64>, Option<String>)> {
    let value = value.strip_prefix('v').unwrap_or(value);
    let value = value.split('+').next()?;
    let (core, pre) = value
        .split_once('-')
        .map_or((value, None), |(a, b)| (a, Some(b)));
    if core.is_empty() || core.split('.').count() > 8 {
        return None;
    }
    let numbers: Option<Vec<u64>> = core.split('.').map(|part| part.parse().ok()).collect();
    let numbers = numbers?;
    if numbers.iter().any(|_| false) || pre.is_some_and(|v| v.is_empty() || v.contains(' ')) {
        return None;
    }
    Some((numbers, pre.map(str::to_owned)))
}

pub fn compare_versions(current: &str, candidate: &str) -> VersionComparison {
    let Some((mut a, a_pre)) = parse_version(current) else {
        return VersionComparison::Unknown;
    };
    let Some((mut b, b_pre)) = parse_version(candidate) else {
        return VersionComparison::Unknown;
    };
    let width = a.len().max(b.len());
    a.resize(width, 0);
    b.resize(width, 0);
    match a.cmp(&b) {
        std::cmp::Ordering::Less => VersionComparison::Newer,
        std::cmp::Ordering::Greater => VersionComparison::NotNewer,
        std::cmp::Ordering::Equal => match (a_pre.as_deref(), b_pre.as_deref()) {
            (None, None) => VersionComparison::NotNewer,
            (Some(a), Some(b)) if a == b => VersionComparison::NotNewer,
            (Some(_), Some(_)) => VersionComparison::Unknown,
            (None, Some(_)) => VersionComparison::NotNewer,
            (Some(_), None) => VersionComparison::Newer,
        },
    }
}

/// Normalize already-supplied feed data.  This function never performs a
/// network request; absent candidate data remains `Unavailable`.
pub fn normalize_update_feed(records: &[SuppliedUpdateRecord]) -> UpdateFeedReport {
    let mut entries = Vec::with_capacity(records.len());
    for record in records {
        if !valid_http_url(&record.feed_url) {
            entries.push(NormalizedUpdate {
                app_key: record.app_key.clone(),
                feed_url: record.feed_url.clone(),
                candidate_version: record.candidate_version.clone(),
                comparison: VersionComparison::Unknown,
                status: UpdateFeedStatus::InvalidUrl,
                network_performed: false,
                reason: Some("feed URL must use http(s) with a host".into()),
            });
            continue;
        }
        let Some(candidate) = record.candidate_version.as_deref() else {
            entries.push(NormalizedUpdate {
                app_key: record.app_key.clone(),
                feed_url: record.feed_url.clone(),
                candidate_version: None,
                comparison: VersionComparison::Unknown,
                status: UpdateFeedStatus::Unavailable,
                network_performed: false,
                reason: Some("feed result was not supplied".into()),
            });
            continue;
        };
        let comparison = compare_versions(&record.current_version, candidate);
        let status = match comparison {
            VersionComparison::Newer => UpdateFeedStatus::UpdateAvailable,
            VersionComparison::NotNewer => UpdateFeedStatus::UpToDate,
            VersionComparison::Unknown => UpdateFeedStatus::UnknownVersion,
        };
        entries.push(NormalizedUpdate {
            app_key: record.app_key.clone(),
            feed_url: record.feed_url.clone(),
            candidate_version: Some(candidate.to_owned()),
            comparison,
            status,
            network_performed: false,
            reason: None,
        });
    }
    UpdateFeedReport {
        entries,
        network_performed: false,
    }
}

// ---------------------------------------------------------------------------
// Bounded process-incarnation history
// ---------------------------------------------------------------------------

pub trait Clock {
    fn now_seconds(&self) -> u64;
}

#[derive(Clone, Copy, Debug)]
pub struct FixedClock(pub u64);

impl Clock for FixedClock {
    fn now_seconds(&self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResourceSample {
    pub process: ProcessIdentity,
    pub observed_at: u64,
    pub sequence: u64,
    pub session_id: u64,
    pub cpu_usage_percent: Metric<f32>,
    pub memory: Metric<u64>,
    pub gpu_usage_percent: Metric<f32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessHistory {
    pub max_samples_per_process: usize,
    pub session_id: u64,
    next_sequence: u64,
    pub samples: Vec<ResourceSample>,
}

impl ProcessHistory {
    pub fn new(max_samples_per_process: usize) -> Self {
        Self {
            max_samples_per_process: max_samples_per_process.max(1),
            session_id: 0,
            next_sequence: 0,
            samples: Vec::new(),
        }
    }

    pub fn begin_session(&mut self) -> u64 {
        self.session_id = self.session_id.saturating_add(1);
        self.session_id
    }

    pub fn record_with_clock<C: Clock + ?Sized>(
        &mut self,
        process: ProcessIdentity,
        cpu_usage_percent: Metric<f32>,
        memory: Metric<u64>,
        gpu_usage_percent: Metric<f32>,
        clock: &C,
    ) -> u64 {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.samples.push(ResourceSample {
            process: process.clone(),
            observed_at: clock.now_seconds(),
            sequence,
            session_id: self.session_id,
            cpu_usage_percent,
            memory,
            gpu_usage_percent,
        });
        let count = self
            .samples
            .iter()
            .filter(|sample| {
                sample.process.pid == process.pid && sample.process.start_time == process.start_time
            })
            .count();
        if count > self.max_samples_per_process {
            if let Some(index) = self.samples.iter().position(|sample| {
                sample.process.pid == process.pid && sample.process.start_time == process.start_time
            }) {
                self.samples.remove(index);
            }
        }
        sequence
    }

    pub fn samples_for(&self, process: &ProcessIdentity) -> Vec<&ResourceSample> {
        self.samples
            .iter()
            .filter(|sample| {
                sample.process.pid == process.pid && sample.process.start_time == process.start_time
            })
            .collect()
    }

    pub fn samples_for_session(&self, session_id: u64) -> Vec<&ResourceSample> {
        self.samples
            .iter()
            .filter(|sample| sample.session_id == session_id)
            .collect()
    }
}
